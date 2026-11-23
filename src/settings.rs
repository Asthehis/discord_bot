//! Réglages par serveur (`/setup`) : en base, avec une copie en mémoire lue sans `await`.

use std::{collections::HashMap, sync::RwLock};

use poise::serenity_prelude::{ChannelId, GuildId, RoleId};
use tracing::{info, warn};

use crate::{config::Config, db};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GuildSettings {
    pub verification: Option<Verification>,
    /// Salon du menu des rôles de langue.
    pub roles_channel: Option<ChannelId>,
    /// Rôle de chaque langue du catalogue, dans l'ordre d'ajout.
    pub languages: Vec<(String, RoleId)>,
    /// Salon des codes cadeaux ; active `/register`.
    pub gift_channel: Option<ChannelId>,
    /// Active `/event`.
    pub reminders: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verification {
    pub channel: ChannelId,
    pub category: ChannelId,
    pub verified: RoleId,
    pub guest: RoleId,
    pub moderator: RoleId,
    pub welcome: ChannelId,
    pub rules: ChannelId,
}

impl GuildSettings {
    fn from_row(row: &db::GuildRow, languages: Vec<(String, RoleId)>) -> Self {
        let ch = |v: Option<i64>| v.map(|id| ChannelId::new(id as u64));
        let role = |v: Option<i64>| v.map(|id| RoleId::new(id as u64));
        let verification = (|| {
            Some(Verification {
                channel: ch(row.verif_channel)?,
                category: ch(row.verif_category)?,
                verified: role(row.role_verified)?,
                guest: role(row.role_guest)?,
                moderator: role(row.role_moderator)?,
                welcome: ch(row.welcome_channel)?,
                rules: ch(row.rules_channel)?,
            })
        })();
        Self {
            verification,
            roles_channel: ch(row.roles_channel),
            languages,
            gift_channel: ch(row.gift_channel),
            reminders: row.reminders,
        }
    }

    fn to_row(&self, guild: GuildId) -> (db::GuildRow, Vec<(String, i64)>) {
        let v = self.verification.as_ref();
        let ch = |c: Option<ChannelId>| c.map(|c| c.get() as i64);
        let role = |r: Option<RoleId>| r.map(|r| r.get() as i64);
        let row = db::GuildRow {
            guild_id: guild.get() as i64,
            verif_channel: ch(v.map(|v| v.channel)),
            verif_category: ch(v.map(|v| v.category)),
            role_verified: role(v.map(|v| v.verified)),
            role_guest: role(v.map(|v| v.guest)),
            role_moderator: role(v.map(|v| v.moderator)),
            welcome_channel: ch(v.map(|v| v.welcome)),
            rules_channel: ch(v.map(|v| v.rules)),
            roles_channel: ch(self.roles_channel),
            gift_channel: ch(self.gift_channel),
            reminders: self.reminders,
        };
        let languages = self.languages.iter().map(|(k, r)| (k.clone(), r.get() as i64)).collect();
        (row, languages)
    }
}

pub struct Store {
    db: db::Pool,
    cache: RwLock<HashMap<GuildId, GuildSettings>>,
}

impl Store {
    pub async fn load(db: db::Pool) -> sqlx::Result<Self> {
        let mut langs: HashMap<i64, Vec<(String, RoleId)>> = HashMap::new();
        for (guild, key, role) in db::guild_languages(&db).await? {
            langs.entry(guild).or_default().push((key, RoleId::new(role as u64)));
        }
        let cache = db::guild_rows(&db)
            .await?
            .iter()
            .map(|row| {
                let languages = langs.remove(&row.guild_id).unwrap_or_default();
                (GuildId::new(row.guild_id as u64), GuildSettings::from_row(row, languages))
            })
            .collect();
        Ok(Self { db, cache: RwLock::new(cache) })
    }

    /// Réglages du serveur (vides s'il n'a jamais été configuré).
    pub fn get(&self, guild: GuildId) -> GuildSettings {
        self.cache.read().unwrap().get(&guild).cloned().unwrap_or_default()
    }

    fn is_known(&self, guild: GuildId) -> bool {
        self.cache.read().unwrap().contains_key(&guild)
    }

    /// Salons des codes de tous les serveurs.
    pub fn gift_channels(&self) -> Vec<(GuildId, ChannelId)> {
        self.cache
            .read()
            .unwrap()
            .iter()
            .filter_map(|(g, s)| Some((*g, s.gift_channel?)))
            .collect()
    }

    /// Applique `change` aux réglages du serveur et les enregistre.
    pub async fn update(
        &self,
        guild: GuildId,
        change: impl FnOnce(&mut GuildSettings),
    ) -> sqlx::Result<GuildSettings> {
        let mut settings = self.get(guild);
        change(&mut settings);
        let (row, languages) = settings.to_row(guild);
        db::save_guild(&self.db, &row, &languages).await?;
        self.cache.write().unwrap().insert(guild, settings.clone());
        Ok(settings)
    }
}

/// Reprend les anciennes clés de `config.toml` pour les serveurs pas encore en base.
pub async fn import_legacy(config: &Config, store: &Store) -> anyhow::Result<()> {
    let legacy = &config.legacy;
    if legacy.is_empty() {
        return Ok(());
    }

    if let Some(guild) = legacy.guild_id {
        if !store.is_known(guild) {
            let verification = match (&legacy.roles, &legacy.channels, legacy.verif_category) {
                (Some(r), Some(c), Some(category)) => Some(Verification {
                    channel: c.verification,
                    category,
                    verified: r.verified,
                    guest: r.guest,
                    moderator: r.moderator,
                    welcome: c.general_chat,
                    rules: c.rules,
                }),
                _ => None,
            };
            let channels = legacy.channels.as_ref();
            let languages = config
                .languages
                .iter()
                .filter_map(|l| Some((l.key.clone(), l.role_id?)))
                .collect();
            let reminders = legacy.guilds.iter().any(|g| g.id == guild && g.reminders);
            store
                .update(guild, |s| {
                    s.verification = verification;
                    s.roles_channel = channels.and_then(|c| c.roles);
                    s.languages = languages;
                    s.gift_channel = channels.and_then(|c| c.gift_codes);
                    s.reminders = reminders;
                })
                .await?;
            info!(%guild, "settings imported from config.toml");
        }
        // Inscriptions d'avant le multi-serveur : rattachées au serveur principal.
        let moved = db::assign_unowned_players(&store.db, guild.get()).await?;
        if moved > 0 {
            info!(%guild, count = moved, "players attached to the main server");
        }
    }

    for g in &legacy.guilds {
        if Some(g.id) == legacy.guild_id || store.is_known(g.id) {
            continue;
        }
        store
            .update(g.id, |s| {
                s.reminders = g.reminders;
                s.gift_channel = g.gift_codes;
            })
            .await?;
        info!(guild = %g.id, "settings imported from config.toml");
    }

    warn!("config.toml still has per-server keys (guild_id, roles, channels, guilds…): they are now in the database, remove them");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_roundtrip() {
        let guild = GuildId::new(1);
        let s = GuildSettings {
            verification: Some(Verification {
                channel: ChannelId::new(2),
                category: ChannelId::new(3),
                verified: RoleId::new(4),
                guest: RoleId::new(5),
                moderator: RoleId::new(6),
                welcome: ChannelId::new(7),
                rules: ChannelId::new(8),
            }),
            roles_channel: Some(ChannelId::new(9)),
            languages: vec![("fr".into(), RoleId::new(10))],
            gift_channel: None,
            reminders: true,
        };
        let (row, langs) = s.to_row(guild);
        let langs = langs.into_iter().map(|(k, r)| (k, RoleId::new(r as u64))).collect();
        assert_eq!(GuildSettings::from_row(&row, langs), s);
    }

    #[test]
    fn partial_verification_is_disabled() {
        let row = db::GuildRow { guild_id: 1, verif_channel: Some(2), ..Default::default() };
        assert!(GuildSettings::from_row(&row, vec![]).verification.is_none());
    }
}
