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

pub async fn member_kingdom(pool: &Pool, discord_id: u64) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT kingdom FROM members WHERE discord_id = ?")
        .bind(discord_id as i64)
        .fetch_optional(pool)
        .await
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
pub async fn upsert_player(
    pool: &Pool,
    fid: i64,
    discord_id: u64,
    kingdom: i64,
    alliance: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO players (fid, discord_id, kingdom, alliance) VALUES (?, ?, ?, ?)
         ON CONFLICT (fid) DO UPDATE SET
           discord_id = excluded.discord_id, kingdom = excluded.kingdom, alliance = excluded.alliance",
    )
    .bind(fid)
    .bind(discord_id as i64)
    .bind(kingdom)
    .bind(alliance)
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

/// Joueurs pour qui `code` reste à tenter, éventuellement limité à un `fid`.
pub async fn players_to_redeem(
    pool: &Pool,
    code: &str,
    only_fid: Option<i64>,
) -> sqlx::Result<Vec<(i64, i64)>> {
    // Liste à garder alignée avec FINAL_STATUSES (vérifié par un test).
    sqlx::query_as(
        "SELECT p.fid, p.kingdom FROM players p
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
        upsert_player(&pool, 1, 100, 42, "ABC").await.unwrap();
        upsert_player(&pool, 2, 100, 42, "ABC").await.unwrap();
        assert!(insert_code(&pool, "CODE", "channel").await.unwrap());
        assert!(!insert_code(&pool, "CODE", "share_api").await.unwrap());

        assert_eq!(players_to_redeem(&pool, "CODE", None).await.unwrap().len(), 2);
        record_redemption(&pool, 1, "CODE", "SUCCESS").await.unwrap();
        record_redemption(&pool, 2, "CODE", "ERROR").await.unwrap();
        // ERROR est retenté, SUCCESS non.
        assert_eq!(players_to_redeem(&pool, "CODE", None).await.unwrap(), vec![(2, 42)]);
        assert_eq!(players_to_redeem(&pool, "CODE", Some(1)).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn final_statuses_are_not_retried() {
        use crate::giftapi::{InvalidReason, Outcome};
        let pool = mem().await;
        upsert_player(&pool, 1, 100, 42, "ABC").await.unwrap();
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
        upsert_player(&pool, 1, 100, 42, "ABC").await.unwrap();
        insert_code(&pool, "CODE", "channel").await.unwrap();
        record_redemption(&pool, 1, "CODE", "KINGDOM_MISMATCH").await.unwrap();
        assert!(players_to_redeem(&pool, "CODE", None).await.unwrap().is_empty());

        upsert_player(&pool, 1, 100, 43, "ABC").await.unwrap();
        assert_eq!(players_to_redeem(&pool, "CODE", None).await.unwrap(), vec![(1, 43)]);
    }

    #[tokio::test]
    async fn ownership() {
        let pool = mem().await;
        upsert_player(&pool, 1, 100, 42, "ABC").await.unwrap();
        assert_eq!(player_owner(&pool, 1).await.unwrap(), Some(100));
        assert!(!delete_player(&pool, 1, 999).await.unwrap());
        assert!(delete_player(&pool, 1, 100).await.unwrap());
        assert!(players_of(&pool, 100).await.unwrap().is_empty());
    }
}
