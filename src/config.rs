//! Config globale du bot (TOML + env). Les réglages par serveur sont en base
//! (`settings`, via `/setup`).

use std::collections::HashSet;

use anyhow::{Context, bail};
use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use poise::serenity_prelude::{ChannelId, CreateSelectMenuOption, GuildId, ReactionType, RoleId};
use serde::Deserialize;

/// Limite Discord du nombre d'options dans un select menu.
pub const MAX_SELECT_OPTIONS: usize = 25;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Catalogue des langues (textes dans `messages.toml`) ; rôles choisis par serveur.
    pub languages: Vec<Language>,
    /// Fichier SQLite.
    #[serde(default = "default_database_path")]
    pub database_path: String,
    /// Salon recevant une copie des logs du bot (tous serveurs confondus).
    pub logs_channel: Option<ChannelId>,
    pub gift: Gift,
    pub alliances: Vec<Alliance>,
    /// Anciennes clés par serveur : reprises en base au démarrage, puis à supprimer.
    #[serde(flatten)]
    pub legacy: Legacy,
}

fn default_database_path() -> String {
    "data/bot.db".into()
}

#[derive(Debug, Deserialize)]
pub struct Gift {
    /// Clé de signature du site de réclamation.
    pub encrypt_key: String,
    /// API communautaire de partage de codes (lecture seule). Absente = désactivée.
    pub share_api: Option<ShareApi>,
}

#[derive(Debug, Deserialize)]
pub struct ShareApi {
    pub url: String,
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct Alliance {
    pub tag: String,
    pub name: String,
    /// Royaume de l'alliance : utilisé par `/register`.
    pub kingdom: i64,
}

#[derive(Debug, Deserialize)]
pub struct Language {
    /// Identifiant stable, utilisé comme valeur dans les select menus.
    pub key: String,
    pub label: String,
    pub emoji: Option<String>,
    /// Ancien format (rôle du serveur principal) : repris en base au démarrage.
    pub role_id: Option<RoleId>,
}

impl Language {
    /// Option de select menu (valeur = `key`).
    pub fn select_option(&self, selected: bool) -> CreateSelectMenuOption {
        let opt = CreateSelectMenuOption::new(&self.label, &self.key).default_selection(selected);
        match &self.emoji {
            Some(e) => opt.emoji(ReactionType::Unicode(e.clone())),
            None => opt,
        }
    }
}

// --- ancien format (un serveur principal décrit dans le TOML) ----------------

#[derive(Debug, Default, Deserialize)]
pub struct Legacy {
    pub guild_id: Option<GuildId>,
    pub verif_category: Option<ChannelId>,
    pub roles: Option<LegacyRoles>,
    pub channels: Option<LegacyChannels>,
    #[serde(default)]
    pub guilds: Vec<LegacyGuild>,
}

#[derive(Debug, Deserialize)]
pub struct LegacyRoles {
    pub verified: RoleId,
    pub guest: RoleId,
    pub moderator: RoleId,
}

#[derive(Debug, Deserialize)]
pub struct LegacyChannels {
    pub verification: ChannelId,
    pub general_chat: ChannelId,
    pub rules: ChannelId,
    pub roles: Option<ChannelId>,
    pub gift_codes: Option<ChannelId>,
    pub logs: Option<ChannelId>,
}

#[derive(Debug, Deserialize)]
pub struct LegacyGuild {
    pub id: GuildId,
    #[serde(default)]
    pub reminders: bool,
    pub gift_codes: Option<ChannelId>,
}

impl Legacy {
    pub fn is_empty(&self) -> bool {
        self.guild_id.is_none() && self.guilds.is_empty()
    }
}

impl Config {
    /// Charge `CONFIG_PATH` (défaut `config.toml`), surchargé par les variables `BOT_*`
    /// (`__` pour les niveaux : `BOT_GIFT__ENCRYPT_KEY=…`).
    pub fn load() -> anyhow::Result<Self> {
        let path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".into());

        let config: Config = Figment::new()
            .merge(Toml::file(&path))
            .merge(Env::prefixed("BOT_").split("__"))
            .extract()
            .with_context(|| format!("invalid config (file: {path})"))?;

        config.validate()?;
        Ok(config)
    }

    /// Salon des logs (nouvelle clé, sinon ancienne `channels.logs`).
    pub fn logs_channel(&self) -> Option<ChannelId> {
        self.logs_channel
            .or_else(|| self.legacy.channels.as_ref().and_then(|c| c.logs))
    }

    /// Alliance par tag ou par nom, sans tenir compte de la casse.
    pub fn alliance(&self, input: &str) -> Option<&Alliance> {
        let input = input.trim();
        self.alliances
            .iter()
            .find(|a| a.tag.eq_ignore_ascii_case(input) || a.name.eq_ignore_ascii_case(input))
    }

    pub fn language(&self, key: &str) -> Option<&Language> {
        self.languages.iter().find(|l| l.key == key)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.languages.len() > MAX_SELECT_OPTIONS {
            bail!(
                "{} languages configured, maximum {MAX_SELECT_OPTIONS}",
                self.languages.len()
            );
        }
        let mut keys = HashSet::new();
        for lang in &self.languages {
            if !keys.insert(lang.key.as_str()) {
                bail!("duplicate language key: `{}`", lang.key);
            }
        }

        if self.alliances.is_empty() {
            bail!("`alliances` is empty");
        }
        let mut tags = HashSet::new();
        for a in &self.alliances {
            if !tags.insert(a.tag.to_lowercase()) {
                bail!("duplicate alliance tag: `{}`", a.tag);
            }
            if a.kingdom <= 0 {
                bail!("alliance `{}`: invalid `kingdom`", a.tag);
            }
        }
        if self.gift.encrypt_key.is_empty() {
            bail!("`gift.encrypt_key` is empty");
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../config.example.toml");

    fn parse(toml: &str) -> anyhow::Result<Config> {
        let config: Config = Figment::from(Toml::string(toml)).extract()?;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn example_config() -> Config {
        parse(EXAMPLE).unwrap()
    }

    #[test]
    fn example_is_valid() {
        let c = example_config();
        assert_eq!(c.languages.len(), 3);
        assert!(c.legacy.is_empty());
    }

    #[test]
    fn legacy_keys_are_read() {
        let legacy = format!(
            "guild_id = 10\nverif_category = 11\n{EXAMPLE}\n\
             [roles]\nverified = 1\nguest = 2\nmoderator = 3\n\
             [channels]\nverification = 4\ngeneral_chat = 5\nrules = 6\ngift_codes = 7\nlogs = 8\n\
             [[guilds]]\nid = 20\nreminders = true\n"
        );
        let c = parse(&legacy).unwrap();
        assert_eq!(c.legacy.guild_id, Some(GuildId::new(10)));
        assert_eq!(c.legacy.guilds.len(), 1);
        assert_eq!(c.logs_channel(), Some(ChannelId::new(8)));
    }

    #[test]
    fn duplicate_language_key_is_rejected() {
        let dup = format!("{EXAMPLE}\n[[languages]]\nkey = \"fr\"\nlabel = \"Bis\"\n");
        let err = parse(&dup).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn too_many_languages_is_rejected() {
        let mut many = EXAMPLE.to_string();
        for n in 0..25 {
            many.push_str(&format!("\n[[languages]]\nkey = \"l{n}\"\nlabel = \"L{n}\"\n"));
        }
        let err = parse(&many).unwrap_err().to_string();
        assert!(err.contains("maximum"), "{err}");
    }
}
