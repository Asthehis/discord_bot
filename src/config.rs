use std::collections::HashSet;

use anyhow::{Context, bail};
use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use poise::serenity_prelude::{
    ChannelId, CreateSelectMenuOption, GuildId, ReactionType, RoleId,
};
use serde::Deserialize;

/// Limite Discord du nombre d'options dans un select menu.
const MAX_SELECT_OPTIONS: usize = 25;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub guild_id: GuildId,
    pub roles: Roles,
    pub channels: Channels,
    pub verif_category: ChannelId,
    pub languages: Vec<Language>,
    /// Fichier SQLite (sur le PVC en prod).
    #[serde(default = "default_database_path")]
    pub database_path: String,
    pub gift: Gift,
    pub alliances: Vec<Alliance>,
    /// Serveurs annexes (rappels, codes cadeaux). Le principal peut y figurer pour `/event`.
    #[serde(default)]
    pub guilds: Vec<ExtraGuild>,
}

#[derive(Debug, Deserialize)]
pub struct ExtraGuild {
    pub id: GuildId,
    /// Active `/event`.
    #[serde(default)]
    pub reminders: bool,
    /// Salon des codes de ce serveur (détection + résumés) ; active `/register`.
    pub gift_codes: Option<ChannelId>,
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

impl Config {
    /// Alliance par tag ou par nom, sans tenir compte de la casse.
    pub fn alliance(&self, input: &str) -> Option<&Alliance> {
        let input = input.trim();
        self.alliances
            .iter()
            .find(|a| a.tag.eq_ignore_ascii_case(input) || a.name.eq_ignore_ascii_case(input))
    }

    fn extra(&self, guild: GuildId) -> Option<&ExtraGuild> {
        self.guilds.iter().find(|g| g.id == guild)
    }

    pub fn reminders_enabled(&self, guild: GuildId) -> bool {
        self.extra(guild).is_some_and(|g| g.reminders)
    }

    /// Salon des codes du serveur (le principal en a toujours un).
    pub fn gift_channel(&self, guild: GuildId) -> Option<ChannelId> {
        if guild == self.guild_id {
            Some(self.channels.gift_codes)
        } else {
            self.extra(guild).and_then(|g| g.gift_codes)
        }
    }

    /// Tous les salons des codes, avec leur serveur.
    pub fn gift_channels(&self) -> impl Iterator<Item = (GuildId, ChannelId)> + '_ {
        std::iter::once((self.guild_id, self.channels.gift_codes)).chain(
            self.guilds
                .iter()
                .filter(|g| g.id != self.guild_id)
                .filter_map(|g| Some((g.id, g.gift_codes?))),
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct Roles {
    pub verified: RoleId,
    pub guest: RoleId,
    pub moderator: RoleId,
}

#[derive(Debug, Deserialize)]
pub struct Channels {
    pub verification: ChannelId,
    pub general_chat: ChannelId,
    pub rules: ChannelId,
    pub roles: ChannelId,
    /// Salon lu pour détecter les codes cadeaux (+ résumés de réclamation).
    pub gift_codes: ChannelId,
    /// Copie des logs du bot. Absent = désactivé.
    #[serde(default)]
    pub logs: Option<ChannelId>,
}

#[derive(Debug, Deserialize)]
pub struct Language {
    /// Identifiant stable, utilisé comme valeur dans le select menu.
    pub key: String,
    pub label: String,
    pub role_id: RoleId,
    pub emoji: Option<String>,
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

impl Config {
    /// Charge `CONFIG_PATH` (défaut `config.toml`), surchargé par les variables `BOT_*`
    /// (`__` pour les niveaux : `BOT_ROLES__VERIFIED=123`).
    pub fn load() -> anyhow::Result<Self> {
        let path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".into());

        let config: Config = Figment::new()
            .merge(Toml::file(&path))
            .merge(Env::prefixed("BOT_").split("__"))
            .extract()
            .with_context(|| format!("config invalide (fichier : {path})"))?;

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.languages.is_empty() {
            bail!("`languages` est vide");
        }
        if self.languages.len() > MAX_SELECT_OPTIONS {
            bail!(
                "{} langues configurées, maximum {MAX_SELECT_OPTIONS}",
                self.languages.len()
            );
        }

        let mut keys = HashSet::new();
        for lang in &self.languages {
            if !keys.insert(lang.key.as_str()) {
                bail!("clé de langue en double : `{}`", lang.key);
            }
        }

        if self.alliances.is_empty() {
            bail!("`alliances` est vide");
        }
        let mut tags = HashSet::new();
        for a in &self.alliances {
            if !tags.insert(a.tag.to_lowercase()) {
                bail!("tag d'alliance en double : `{}`", a.tag);
            }
            if a.kingdom <= 0 {
                bail!("alliance `{}` : `kingdom` invalide", a.tag);
            }
        }
        let mut ids = HashSet::new();
        for g in &self.guilds {
            if !ids.insert(g.id) {
                bail!("serveur en double dans `guilds` : {}", g.id);
            }
            if g.id == self.guild_id && g.gift_codes.is_some() {
                bail!("serveur principal : son salon des codes est `channels.gift_codes`");
            }
        }
        if self.gift.encrypt_key.is_empty() {
            bail!("`gift.encrypt_key` est vide");
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../config.example.toml");

    fn parse(toml: &str) -> anyhow::Result<Config> {
        // Les IDs à 0 de l'exemple sont refusés par serenity : on met des IDs factices.
        let toml = toml.replace("= 0\n", "= 123456789012345678\n");
        let config: Config = Figment::from(Toml::string(&toml)).extract()?;
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
        assert_eq!(c.gift_channels().count(), 2);
        let extra = c.guilds[0].id;
        assert!(c.reminders_enabled(extra) && !c.reminders_enabled(c.guild_id));
        assert!(c.gift_channel(extra).is_some());
    }

    #[test]
    fn duplicate_language_key_is_rejected() {
        let dup = format!("{EXAMPLE}\n[[languages]]\nkey = \"fr\"\nlabel = \"Bis\"\nrole_id = 0\n");
        let err = parse(&dup).unwrap_err().to_string();
        assert!(err.contains("double"), "{err}");
    }

    #[test]
    fn too_many_languages_is_rejected() {
        let mut many = EXAMPLE.to_string();
        for n in 0..25 {
            many.push_str(&format!("\n[[languages]]\nkey = \"l{n}\"\nlabel = \"L{n}\"\nrole_id = 0\n"));
        }
        let err = parse(&many).unwrap_err().to_string();
        assert!(err.contains("maximum"), "{err}");
    }
}
