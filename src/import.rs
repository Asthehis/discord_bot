//! Import unique depuis la base de l'ancien bot (kingshot-discord-bot) :
//! `discord-verif-bot import <users.sqlite> <alliance.sqlite> [--map ANCIEN=NOUVEAU]...`
//!
//! Reprend fid, compte Discord, kingdom et alliance. Rejouable : un joueur déjà
//! présent est mis à jour, un fid lié à un autre compte Discord n'est pas touché.

use std::collections::HashMap;

use anyhow::{Context, bail};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::{config::Config, db};

struct Args {
    users: String,
    alliances: String,
    /// Ancien nom d'alliance (minuscules) → alliance de la config.
    map: HashMap<String, String>,
}

fn parse_args(args: &[String]) -> anyhow::Result<Args> {
    let mut positional = Vec::new();
    let mut map = HashMap::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--map" {
            let pair = it.next().context("--map expects OLD=NEW")?;
            let (old, new) = pair.split_once('=').context("--map expects OLD=NEW")?;
            map.insert(old.trim().to_lowercase(), new.trim().to_string());
        } else {
            positional.push(arg.clone());
        }
    }
    let [users, alliances] = <[String; 2]>::try_from(positional).map_err(|_| {
        anyhow::anyhow!("usage: import <users.sqlite> <alliance.sqlite> [--map OLD=NEW]...")
    })?;
    Ok(Args { users, alliances, map })
}

async fn open_readonly(path: &str) -> anyhow::Result<sqlx::SqlitePool> {
    if !std::path::Path::new(path).exists() {
        bail!("file not found: {path}");
    }
    let opts = SqliteConnectOptions::new().filename(path).read_only(true);
    Ok(SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?)
}

pub async fn run(config: &Config, args: &[String]) -> anyhow::Result<()> {
    let args = parse_args(args)?;
    let old_users = open_readonly(&args.users).await?;
    let old_alliances = open_readonly(&args.alliances).await?;
    let pool = db::connect(&config.database_path).await?;

    let names: HashMap<i64, String> =
        sqlx::query_as::<_, (i64, String)>("SELECT alliance_id, name FROM alliance_list")
            .fetch_all(&old_alliances)
            .await?
            .into_iter()
            .collect();

    // (fid, kid, alliance, discord_id) ; `alliance` est stocké en TEXT dans l'ancienne base.
    type OldUser = (i64, Option<i64>, Option<String>, Option<i64>);
    let rows: Vec<OldUser> =
        sqlx::query_as("SELECT fid, kid, alliance, discord_id FROM users ORDER BY fid")
            .fetch_all(&old_users)
            .await?;

    let (mut imported, mut updated) = (0, 0);
    let mut skipped: Vec<String> = Vec::new();
    let mut per_alliance: HashMap<String, u32> = HashMap::new();

    for (fid, kid, alliance, discord_id) in rows {
        let Some(discord_id) = discord_id.map(|d| d as u64) else {
            skipped.push(format!("{fid} : pas de compte Discord"));
            continue;
        };
        let Some(kid) = kid else {
            skipped.push(format!("{fid} : pas de kingdom"));
            continue;
        };
        let old_name = alliance
            .as_deref()
            .and_then(|a| a.trim().parse::<i64>().ok())
            .and_then(|id| names.get(&id))
            .cloned()
            .unwrap_or_default();
        let target = args.map.get(&old_name.to_lowercase()).map_or(old_name.as_str(), String::as_str);
        let Some(new_alliance) = config.alliance(target) else {
            skipped.push(format!("{fid} : alliance « {old_name} » absente de la config (--map ?)"));
            continue;
        };

        match db::player_owner(&pool, fid).await? {
            Some(owner) if owner != discord_id => {
                skipped.push(format!("{fid} : déjà lié à un autre compte Discord"));
                continue;
            }
            Some(_) => updated += 1,
            None => imported += 1,
        }
        db::upsert_player(&pool, fid, discord_id, kid, &new_alliance.tag, None).await?;
        let label = if old_name == new_alliance.tag {
            new_alliance.tag.clone()
        } else {
            format!("{old_name} → {}", new_alliance.tag)
        };
        *per_alliance.entry(label).or_default() += 1;
    }

    println!("Import terminé : {imported} nouveaux, {updated} mis à jour, {} ignorés.", skipped.len());
    let mut per_alliance: Vec<_> = per_alliance.into_iter().collect();
    per_alliance.sort();
    for (alliance, n) in per_alliance {
        println!("  {alliance} : {n}");
    }
    for s in &skipped {
        println!("  ignoré : {s}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args() {
        let a = parse_args(&s(&["u.sqlite", "--map", "PHI=CLT", "a.sqlite"])).unwrap();
        assert_eq!((a.users.as_str(), a.alliances.as_str()), ("u.sqlite", "a.sqlite"));
        assert_eq!(a.map.get("phi").map(String::as_str), Some("CLT"));
        assert!(parse_args(&s(&["u.sqlite"])).is_err());
        assert!(parse_args(&s(&["u", "a", "--map", "PHI"])).is_err());
    }
}
