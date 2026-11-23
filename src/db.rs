use std::path::Path;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

pub type Pool = SqlitePool;

/// Statuts de `redemptions` qui ne seront pas retentés (repris en dur dans
/// `players_to_redeem`). `ERROR` (et l'absence de ligne) = à retenter.
#[cfg(test)]
const FINAL_STATUSES: [&str; 8] = [
    "SUCCESS",
    "RECEIVED",
    "SAME_TYPE",
    "CODE_INVALID",
    "LEVEL_TOO_LOW",
    "VIP_TOO_LOW",
    "KINGDOM_MISMATCH",
    "PLAYER_NOT_FOUND",
];

pub async fn connect(path: &str) -> anyhow::Result<Pool> {
    if let Some(dir) = Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

// --- members ---------------------------------------------------------------

pub async fn upsert_member(
    pool: &Pool,
    discord_id: u64,
    name: &str,
    alliance: &str,
    kingdom: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO members (discord_id, name, alliance, kingdom) VALUES (?, ?, ?, ?)
         ON CONFLICT (discord_id) DO UPDATE SET
           name = excluded.name, alliance = excluded.alliance,
           kingdom = excluded.kingdom, verified_at = datetime('now')",
    )
    .bind(discord_id as i64)
    .bind(name)
    .bind(alliance)
    .bind(kingdom)
    .execute(pool)
    .await?;
    Ok(())
}

// --- players ---------------------------------------------------------------

pub struct Player {
    pub fid: i64,
    pub kingdom: i64,
    pub alliance: String,
}

pub async fn player_owner(pool: &Pool, fid: i64) -> sqlx::Result<Option<u64>> {
    let owner: Option<i64> = sqlx::query_scalar("SELECT discord_id FROM players WHERE fid = ?")
        .bind(fid)
        .fetch_optional(pool)
        .await?;
    Ok(owner.map(|id| id as u64))
}

/// Crée ou met à jour le lien, et oublie les échecs liés à l'ancien kingdom.
/// `guild` : serveur d'inscription (`None` = principal).
pub async fn upsert_player(
    pool: &Pool,
    fid: i64,
    discord_id: u64,
    kingdom: i64,
    alliance: &str,
    guild: Option<u64>,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO players (fid, discord_id, kingdom, alliance, guild_id) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (fid) DO UPDATE SET
           discord_id = excluded.discord_id, kingdom = excluded.kingdom,
           alliance = excluded.alliance, guild_id = excluded.guild_id",
    )
    .bind(fid)
    .bind(discord_id as i64)
    .bind(kingdom)
    .bind(alliance)
    .bind(guild.map(|g| g as i64))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM redemptions WHERE fid = ? AND status IN ('KINGDOM_MISMATCH','PLAYER_NOT_FOUND')",
    )
    .bind(fid)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

pub async fn players_of(pool: &Pool, discord_id: u64) -> sqlx::Result<Vec<Player>> {
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT fid, kingdom, alliance FROM players WHERE discord_id = ? ORDER BY created_at",
    )
    .bind(discord_id as i64)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(fid, kingdom, alliance)| Player { fid, kingdom, alliance }).collect())
}

/// Supprime le lien si `fid` appartient bien à `discord_id`.
pub async fn delete_player(pool: &Pool, fid: i64, discord_id: u64) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM players WHERE fid = ? AND discord_id = ?")
        .bind(fid)
        .bind(discord_id as i64)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

// --- gift codes ------------------------------------------------------------

/// `true` si le code est nouveau.
pub async fn insert_code(pool: &Pool, code: &str, source: &str) -> sqlx::Result<bool> {
    let res = sqlx::query("INSERT OR IGNORE INTO gift_codes (code, source) VALUES (?, ?)")
        .bind(code)
        .bind(source)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn set_code_status(
    pool: &Pool,
    code: &str,
    status: &str,
    reason: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE gift_codes SET status = ?, reason = ? WHERE code = ?")
        .bind(status)
        .bind(reason)
        .bind(code)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn code_status(pool: &Pool, code: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT status FROM gift_codes WHERE code = ?")
        .bind(code)
        .fetch_optional(pool)
        .await
}

/// Codes encore utiles (`pending` d'abord, puis `valid`), du plus récent au plus ancien.
pub async fn active_codes(pool: &Pool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT code FROM gift_codes WHERE status IN ('pending','valid')
         ORDER BY status = 'valid', added_at DESC",
    )
    .fetch_all(pool)
    .await
}

/// Joueur à qui réclamer un code.
#[derive(Debug, PartialEq, sqlx::FromRow)]
pub struct Redeemable {
    pub fid: i64,
    pub kingdom: i64,
    /// Serveur d'inscription (`None` = inconnu : import).
    pub guild_id: Option<i64>,
    pub discord_id: i64,
    pub alliance: String,
}

/// Joueurs pour qui `code` reste à tenter, éventuellement limité à un `fid`.
pub async fn players_to_redeem(
    pool: &Pool,
    code: &str,
    only_fid: Option<i64>,
) -> sqlx::Result<Vec<Redeemable>> {
    // Liste à garder alignée avec FINAL_STATUSES (vérifié par un test).
    sqlx::query_as(
        "SELECT p.fid, p.kingdom, p.guild_id, p.discord_id, p.alliance FROM players p
         LEFT JOIN redemptions r ON r.fid = p.fid AND r.code = ?
         WHERE (r.status IS NULL OR r.status NOT IN (
             'SUCCESS','RECEIVED','SAME_TYPE','CODE_INVALID','LEVEL_TOO_LOW',
             'VIP_TOO_LOW','KINGDOM_MISMATCH','PLAYER_NOT_FOUND'))
           AND (? IS NULL OR p.fid = ?)
         ORDER BY p.created_at",
    )
        .bind(code)
        .bind(only_fid)
        .bind(only_fid)
        .fetch_all(pool)
        .await
}

pub async fn record_redemption(pool: &Pool, fid: i64, code: &str, status: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO redemptions (fid, code, status) VALUES (?, ?, ?)
         ON CONFLICT (fid, code) DO UPDATE SET
           status = excluded.status, attempted_at = datetime('now')",
    )
    .bind(fid)
    .bind(code)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> Pool {
        let opts = SqliteConnectOptions::new().in_memory(true).foreign_keys(true);
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn redemption_queue() {
        let pool = mem().await;
        upsert_player(&pool, 1, 100, 42, "ABC", None).await.unwrap();
        upsert_player(&pool, 2, 100, 42, "ABC", Some(7)).await.unwrap();
        assert!(insert_code(&pool, "CODE", "channel").await.unwrap());
        assert!(!insert_code(&pool, "CODE", "share_api").await.unwrap());

        assert_eq!(players_to_redeem(&pool, "CODE", None).await.unwrap().len(), 2);
        record_redemption(&pool, 1, "CODE", "SUCCESS").await.unwrap();
        record_redemption(&pool, 2, "CODE", "ERROR").await.unwrap();
        // ERROR est retenté, SUCCESS non.
        assert_eq!(
            players_to_redeem(&pool, "CODE", None).await.unwrap(),
            vec![Redeemable {
                fid: 2,
                kingdom: 42,
                guild_id: Some(7),
                discord_id: 100,
                alliance: "ABC".into()
            }]
        );
        assert_eq!(players_to_redeem(&pool, "CODE", Some(1)).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn final_statuses_are_not_retried() {
        use crate::giftapi::{InvalidReason, Outcome};
        let pool = mem().await;
        upsert_player(&pool, 1, 100, 42, "ABC", None).await.unwrap();
        insert_code(&pool, "CODE", "channel").await.unwrap();
        for status in FINAL_STATUSES {
            record_redemption(&pool, 1, "CODE", status).await.unwrap();
            assert!(players_to_redeem(&pool, "CODE", None).await.unwrap().is_empty(), "{status}");
        }
        // Tous les statuts définitifs d'Outcome sont bien dans la liste.
        for o in [
            Outcome::Success,
            Outcome::AlreadyReceived,
            Outcome::SameType,
            Outcome::CodeInvalid(InvalidReason::Expired),
            Outcome::LevelTooLow,
            Outcome::VipTooLow,
            Outcome::KingdomMismatch,
            Outcome::PlayerNotFound,
        ] {
            assert!(FINAL_STATUSES.contains(&o.as_db()), "{o:?}");
        }
        assert!(!FINAL_STATUSES.contains(&Outcome::Error(String::new()).as_db()));
    }

    #[tokio::test]
    async fn reregister_clears_kingdom_mismatch() {
        let pool = mem().await;
        upsert_player(&pool, 1, 100, 42, "ABC", None).await.unwrap();
        insert_code(&pool, "CODE", "channel").await.unwrap();
        record_redemption(&pool, 1, "CODE", "KINGDOM_MISMATCH").await.unwrap();
        assert!(players_to_redeem(&pool, "CODE", None).await.unwrap().is_empty());

        upsert_player(&pool, 1, 100, 43, "ABC", None).await.unwrap();
        assert_eq!(
            players_to_redeem(&pool, "CODE", None).await.unwrap(),
            vec![Redeemable {
                fid: 1,
                kingdom: 43,
                guild_id: None,
                discord_id: 100,
                alliance: "ABC".into()
            }]
        );
    }

    #[tokio::test]
    async fn ownership() {
        let pool = mem().await;
        upsert_player(&pool, 1, 100, 42, "ABC", None).await.unwrap();
        assert_eq!(player_owner(&pool, 1).await.unwrap(), Some(100));
        assert!(!delete_player(&pool, 1, 999).await.unwrap());
        assert!(delete_player(&pool, 1, 100).await.unwrap());
        assert!(players_of(&pool, 100).await.unwrap().is_empty());
    }
}

// --- events ----------------------------------------------------------------

/// IDs Discord stockés en i64 (SQLite), convertis à l'usage.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Event {
    pub id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub role_id: Option<i64>,
    pub name: String,
    pub emoji: String,
    pub message: String,
    pub first_start: i64,
    pub interval_secs: i64,
    pub lead_secs: i64,
    pub last_notified: i64,
}

pub struct NewEvent<'a> {
    pub guild_id: u64,
    pub channel_id: u64,
    pub role_id: Option<u64>,
    pub name: &'a str,
    pub emoji: &'a str,
    pub message: &'a str,
    pub first_start: i64,
    pub interval_secs: i64,
    pub lead_secs: i64,
    pub created_by: u64,
}

pub async fn insert_event(pool: &Pool, e: &NewEvent<'_>) -> sqlx::Result<i64> {
    let res = sqlx::query(
        "INSERT INTO events (guild_id, channel_id, role_id, name, emoji, message,
                             first_start, interval_secs, lead_secs, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(e.guild_id as i64)
    .bind(e.channel_id as i64)
    .bind(e.role_id.map(|r| r as i64))
    .bind(e.name)
    .bind(e.emoji)
    .bind(e.message)
    .bind(e.first_start)
    .bind(e.interval_secs)
    .bind(e.lead_secs)
    .bind(e.created_by as i64)
    .execute(pool)
    .await?;
    Ok(res.last_insert_rowid())
}

pub async fn all_events(pool: &Pool) -> sqlx::Result<Vec<Event>> {
    sqlx::query_as(
        "SELECT id, guild_id, channel_id, role_id, name, emoji, message,
                first_start, interval_secs, lead_secs, last_notified
         FROM events",
    )
    .fetch_all(pool)
    .await
}

pub async fn events_of(pool: &Pool, guild_id: u64) -> sqlx::Result<Vec<Event>> {
    sqlx::query_as(
        "SELECT id, guild_id, channel_id, role_id, name, emoji, message,
                first_start, interval_secs, lead_secs, last_notified
         FROM events WHERE guild_id = ? ORDER BY id",
    )
    .bind(guild_id as i64)
    .fetch_all(pool)
    .await
}

pub async fn event(pool: &Pool, guild_id: u64, id: i64) -> sqlx::Result<Option<Event>> {
    sqlx::query_as(
        "SELECT id, guild_id, channel_id, role_id, name, emoji, message,
                first_start, interval_secs, lead_secs, last_notified
         FROM events WHERE guild_id = ? AND id = ?",
    )
    .bind(guild_id as i64)
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn set_last_notified(pool: &Pool, id: i64, start: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE events SET last_notified = ? WHERE id = ?")
        .bind(start)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_event(pool: &Pool, guild_id: u64, id: i64) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM events WHERE guild_id = ? AND id = ?")
        .bind(guild_id as i64)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

// --- guild settings -------------------------------------------------------

/// Ligne de `guild_settings` (IDs en i64, convertis dans `settings`).
#[derive(Debug, Default, Clone, sqlx::FromRow)]
pub struct GuildRow {
    pub guild_id: i64,
    pub verif_channel: Option<i64>,
    pub verif_category: Option<i64>,
    pub role_verified: Option<i64>,
    pub role_guest: Option<i64>,
    pub role_moderator: Option<i64>,
    pub welcome_channel: Option<i64>,
    pub rules_channel: Option<i64>,
    pub roles_channel: Option<i64>,
    pub gift_channel: Option<i64>,
    pub reminders: bool,
    pub board_channel: Option<i64>,
    pub board_message: Option<i64>,
}

pub async fn guild_rows(pool: &Pool) -> sqlx::Result<Vec<GuildRow>> {
    sqlx::query_as(
        "SELECT guild_id, verif_channel, verif_category, role_verified, role_guest,
                role_moderator, welcome_channel, rules_channel, roles_channel,
                gift_channel, reminders, board_channel, board_message
         FROM guild_settings",
    )
    .fetch_all(pool)
    .await
}

/// (guild_id, lang_key, role_id), dans l'ordre d'ajout.
pub async fn guild_languages(pool: &Pool) -> sqlx::Result<Vec<(i64, String, i64)>> {
    sqlx::query_as("SELECT guild_id, lang_key, role_id FROM guild_languages ORDER BY rowid")
        .fetch_all(pool)
        .await
}

/// Remplace tous les réglages d'un serveur (ligne + langues).
pub async fn save_guild(pool: &Pool, row: &GuildRow, languages: &[(String, i64)]) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT OR REPLACE INTO guild_settings
           (guild_id, verif_channel, verif_category, role_verified, role_guest, role_moderator,
            welcome_channel, rules_channel, roles_channel, gift_channel, reminders,
            board_channel, board_message)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.guild_id)
    .bind(row.verif_channel)
    .bind(row.verif_category)
    .bind(row.role_verified)
    .bind(row.role_guest)
    .bind(row.role_moderator)
    .bind(row.welcome_channel)
    .bind(row.rules_channel)
    .bind(row.roles_channel)
    .bind(row.gift_channel)
    .bind(row.reminders)
    .bind(row.board_channel)
    .bind(row.board_message)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM guild_languages WHERE guild_id = ?")
        .bind(row.guild_id)
        .execute(&mut *tx)
        .await?;
    for (key, role) in languages {
        sqlx::query("INSERT INTO guild_languages (guild_id, lang_key, role_id) VALUES (?, ?, ?)")
            .bind(row.guild_id)
            .bind(key)
            .bind(role)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Joueurs sans serveur d'inscription (avant le multi-serveur, import) → `guild`.
pub async fn assign_unowned_players(pool: &Pool, guild: u64) -> sqlx::Result<u64> {
    let res = sqlx::query("UPDATE players SET guild_id = ? WHERE guild_id IS NULL")
        .bind(guild as i64)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

// --- role panels ----------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Panel {
    pub id: i64,
    pub channel_id: i64,
    pub message_id: i64,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct PanelEntry {
    pub role_id: i64,
    pub emoji: Option<String>,
    pub label: String,
}

pub async fn insert_panel(
    pool: &Pool,
    guild: u64,
    channel: u64,
    message: u64,
    title: &str,
    description: &str,
) -> sqlx::Result<i64> {
    let res = sqlx::query(
        "INSERT INTO role_panels (guild_id, channel_id, message_id, title, description)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(guild as i64)
    .bind(channel as i64)
    .bind(message as i64)
    .bind(title)
    .bind(description)
    .execute(pool)
    .await?;
    Ok(res.last_insert_rowid())
}

pub async fn panels_of(pool: &Pool, guild: u64) -> sqlx::Result<Vec<Panel>> {
    sqlx::query_as(
        "SELECT id, channel_id, message_id, title, description
         FROM role_panels WHERE guild_id = ? ORDER BY id",
    )
    .bind(guild as i64)
    .fetch_all(pool)
    .await
}

pub async fn panel(pool: &Pool, guild: u64, id: i64) -> sqlx::Result<Option<Panel>> {
    sqlx::query_as(
        "SELECT id, channel_id, message_id, title, description
         FROM role_panels WHERE guild_id = ? AND id = ?",
    )
    .bind(guild as i64)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Rôles du panneau, dans l'ordre d'ajout.
pub async fn panel_entries(pool: &Pool, panel: i64) -> sqlx::Result<Vec<PanelEntry>> {
    sqlx::query_as(
        "SELECT role_id, emoji, label FROM role_panel_entries WHERE panel_id = ? ORDER BY rowid",
    )
    .bind(panel)
    .fetch_all(pool)
    .await
}

/// Ajoute le rôle, ou met à jour son emoji/libellé en gardant sa place.
pub async fn upsert_panel_entry(pool: &Pool, panel: i64, e: &PanelEntry) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO role_panel_entries (panel_id, role_id, emoji, label) VALUES (?, ?, ?, ?)
         ON CONFLICT (panel_id, role_id) DO UPDATE SET emoji = excluded.emoji, label = excluded.label",
    )
    .bind(panel)
    .bind(e.role_id)
    .bind(&e.emoji)
    .bind(&e.label)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_panel_entry(pool: &Pool, panel: i64, role: u64) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM role_panel_entries WHERE panel_id = ? AND role_id = ?")
        .bind(panel)
        .bind(role as i64)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Supprime le panneau et ses rôles.
pub async fn delete_panel(pool: &Pool, guild: u64, id: i64) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM role_panels WHERE guild_id = ? AND id = ?")
        .bind(guild as i64)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Le bouton `role` du message `message` fait-il bien partie d'un panneau du serveur ?
pub async fn panel_has_role(
    pool: &Pool,
    guild: u64,
    message: u64,
    role: u64,
) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM role_panel_entries e JOIN role_panels p ON p.id = e.panel_id
         WHERE p.guild_id = ? AND p.message_id = ? AND e.role_id = ?",
    )
    .bind(guild as i64)
    .bind(message as i64)
    .bind(role as i64)
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Rôle supprimé du serveur : retiré de tous les panneaux. Renvoie les panneaux touchés.
pub async fn forget_panel_role(pool: &Pool, guild: u64, role: u64) -> sqlx::Result<Vec<i64>> {
    let mut tx = pool.begin().await?;
    let panels: Vec<i64> = sqlx::query_scalar(
        "SELECT e.panel_id FROM role_panel_entries e JOIN role_panels p ON p.id = e.panel_id
         WHERE p.guild_id = ? AND e.role_id = ?",
    )
    .bind(guild as i64)
    .bind(role as i64)
    .fetch_all(&mut *tx)
    .await?;
    for panel in &panels {
        sqlx::query("DELETE FROM role_panel_entries WHERE panel_id = ? AND role_id = ?")
            .bind(panel)
            .bind(role as i64)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(panels)
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    async fn mem() -> Pool {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn entry(role: i64, label: &str) -> PanelEntry {
        PanelEntry {
            role_id: role,
            emoji: None,
            label: label.into(),
        }
    }

    #[tokio::test]
    async fn panel_lifecycle() {
        let pool = mem().await;
        let id = insert_panel(&pool, 1, 2, 3, "Events", "").await.unwrap();
        upsert_panel_entry(&pool, id, &entry(10, "Bear"))
            .await
            .unwrap();
        upsert_panel_entry(&pool, id, &entry(11, "Viking"))
            .await
            .unwrap();
        // Mise à jour : garde sa place.
        upsert_panel_entry(&pool, id, &entry(10, "Bear Trap"))
            .await
            .unwrap();
        let labels: Vec<_> = panel_entries(&pool, id)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.label)
            .collect();
        assert_eq!(labels, ["Bear Trap", "Viking"]);

        assert!(panel_has_role(&pool, 1, 3, 10).await.unwrap());
        assert!(
            !panel_has_role(&pool, 9, 3, 10).await.unwrap(),
            "other guild"
        );
        assert!(
            !panel_has_role(&pool, 1, 3, 12).await.unwrap(),
            "role not on panel"
        );

        assert_eq!(forget_panel_role(&pool, 1, 11).await.unwrap(), [id]);
        assert_eq!(panel_entries(&pool, id).await.unwrap().len(), 1);

        assert!(!delete_panel(&pool, 9, id).await.unwrap());
        assert!(delete_panel(&pool, 1, id).await.unwrap());
        assert!(
            panel_entries(&pool, id).await.unwrap().is_empty(),
            "cascade"
        );
    }
}
