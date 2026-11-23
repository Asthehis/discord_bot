use std::collections::HashMap;

use anyhow::{Context, bail};
use figment::{
    Figment,
    providers::{Format, Toml},
};
use serde::Deserialize;

use crate::config::Config;

pub const DEFAULT_LANG: &str = "en";

/// Textes du bot, chargés depuis `MESSAGES_PATH` (défaut `messages.toml`).
#[derive(Debug, Deserialize)]
pub struct Texts {
    /// Textes côté modos et salon public : anglais uniquement.
    pub common: Common,
    /// `/register`, `/unregister` et codes cadeaux : anglais uniquement.
    pub gift: GiftTexts,
    /// `/event` et rappels : anglais uniquement.
    pub events: EventTexts,
    /// `/setup` (admins) : anglais uniquement.
    pub setup: SetupTexts,
    /// `/roles` et panneaux de rôles : anglais uniquement.
    pub panels: PanelTexts,
    /// Textes côté membre, une entrée par clé de langue (`en` obligatoire).
    lang: HashMap<String, Lang>,
}

#[derive(Debug, Deserialize)]
pub struct Common {
    pub recap_title: String,
    pub field_name: String,
    pub field_alliance: String,
    pub field_kingdom: String,
    pub field_language: String,
    pub button_verified: String,
    pub button_guest: String,
    pub button_refuse: String,
    pub mod_only: String,
    pub member_left: String,
    pub action_failed: String,
    pub roles_title: String,
    pub roles_description: String,
    pub roles_button: String,
    pub roles_placeholder: String,
    /// Placeholder : `{languages}`.
    pub roles_updated: String,
    pub roles_none: String,
    /// Placeholders : `{user}`, `{rules}`, `{roles}`.
    pub welcome: String,
}

/// Placeholders : `{id}`, `{kingdom}`, `{alliance}`, `{code}`, `{reason}`,
/// `{success}`, `{already}`, `{failed}`, `{list}`.
#[derive(Debug, Deserialize)]
pub struct GiftTexts {
    pub register_ok: String,
    pub register_list: String,
    pub register_taken: String,
    pub register_bad_alliance: String,
    pub register_not_found: String,
    pub register_api_down: String,
    pub unregister_ok: String,
    pub unregister_not_yours: String,
    pub code_invalid: String,
    pub code_summary_title: String,
    pub code_summary: String,
}

/// Placeholders : `{missing}`, `{channel}`.
#[derive(Debug, Deserialize)]
pub struct SetupTexts {
    pub saved: String,
    pub posted: String,
    pub verification_incomplete: String,
    pub languages_empty: String,
    pub language_unknown: String,
    pub not_configured: String,
    pub show_title: String,
}

/// Placeholders : `{id}`, `{emoji}`, `{name}`, `{next}`, `{every}`, `{before}`,
/// `{channel}`, `{ping}`, `{skipped}` ; rappel : `{relative}`, `{time}`, `{message}`.
#[derive(Debug, Deserialize)]
pub struct EventTexts {
    pub reminder: String,
    pub board_title: String,
    pub board_line: String,
    pub board_empty: String,
    pub board_footer: String,
    pub board_set: String,
    pub board_removed: String,
    pub board_pin_failed: String,
    pub added: String,
    pub list_title: String,
    pub list_line: String,
    pub list_empty: String,
    pub skipped: String,
    pub removed: String,
    pub not_found: String,
    pub not_enabled: String,
    pub need_name: String,
    pub bad_start: String,
    pub bad_every: String,
    pub start_past: String,
}

/// Placeholders : `{id}`, `{title}`, `{channel}`, `{role}`, `{emoji}`, `{roles}`.
#[derive(Debug, Deserialize)]
pub struct PanelTexts {
    pub line: String,
    pub empty: String,
    pub created: String,
    pub added: String,
    pub updated: String,
    pub removed: String,
    pub deleted: String,
    pub list_title: String,
    pub list_line: String,
    pub list_empty: String,
    pub not_found: String,
    pub not_in_panel: String,
    pub full: String,
    pub role_managed: String,
    pub role_dangerous: String,
    pub role_above_bot: String,
    pub role_above_user: String,
    pub bad_emoji: String,
    pub message_gone: String,
    pub given: String,
    pub taken: String,
    pub failed: String,
}

#[derive(Debug, Deserialize)]
pub struct Lang {
    pub verif_title: String,
    pub verif_description: String,
    pub start_button: String,
    pub lang_placeholder: String,
    pub modal_title: String,
    pub modal_name: String,
    pub modal_alliance: String,
    pub modal_kingdom: String,
    pub invalid_name: String,
    pub invalid_kingdom: String,
    /// Placeholder : `{channel}`.
    pub pending_exists: String,
    /// Placeholder : `{channel}`.
    pub request_created: String,
    pub ticket_intro: String,
}

impl Texts {
    pub fn load(config: &Config) -> anyhow::Result<Self> {
        let path = std::env::var("MESSAGES_PATH").unwrap_or_else(|_| "messages.toml".into());
        Self::from_figment(Figment::new().merge(Toml::file(&path)), config)
            .with_context(|| format!("invalid texts (file: {path})"))
    }

    fn from_figment(figment: Figment, config: &Config) -> anyhow::Result<Self> {
        let texts: Texts = figment.extract()?;
        texts.validate(config)?;
        Ok(texts)
    }

    fn validate(&self, config: &Config) -> anyhow::Result<()> {
        if !self.lang.contains_key(DEFAULT_LANG) {
            bail!("missing section `[lang.{DEFAULT_LANG}]`");
        }
        for l in &config.languages {
            if !self.lang.contains_key(&l.key) {
                bail!("missing section `[lang.{}]`", l.key);
            }
        }

        // Limites Discord : un texte trop long fait échouer l'interaction au runtime.
        let c = &self.common;
        let mut checks: Vec<(String, &str, usize)> = vec![
            ("common.button_verified".into(), &c.button_verified, 80),
            ("common.button_guest".into(), &c.button_guest, 80),
            ("common.button_refuse".into(), &c.button_refuse, 80),
            ("common.recap_title".into(), &c.recap_title, 256),
            ("common.roles_title".into(), &c.roles_title, 256),
            ("common.roles_description".into(), &c.roles_description, 4096),
            ("common.roles_button".into(), &c.roles_button, 80),
            ("common.roles_placeholder".into(), &c.roles_placeholder, 150),
            // Marge pour les mentions qui remplacent les placeholders.
            ("common.welcome".into(), &c.welcome, 1900),
        ];
        for (key, l) in &self.lang {
            let p = |f: &str| format!("lang.{key}.{f}");
            checks.extend([
                (p("verif_title"), l.verif_title.as_str(), 256),
                (p("verif_description"), &l.verif_description, 4096),
                (p("start_button"), &l.start_button, 80),
                (p("lang_placeholder"), &l.lang_placeholder, 150),
                (p("modal_title"), &l.modal_title, 45),
                (p("modal_name"), &l.modal_name, 45),
                (p("modal_alliance"), &l.modal_alliance, 45),
                (p("modal_kingdom"), &l.modal_kingdom, 45),
                // Traduction + version anglaise + mentions doivent tenir dans 2000.
                (p("ticket_intro"), &l.ticket_intro, 900),
            ]);
        }
        for (name, text, max) in checks {
            let len = text.chars().count();
            if len == 0 || len > max {
                bail!("`{name}`: {len} characters (expected 1 to {max})");
            }
        }
        Ok(())
    }

    /// Textes de la langue `key`, anglais si inconnue.
    pub fn lang(&self, key: &str) -> &Lang {
        self.lang.get(key).unwrap_or_else(|| &self.lang[DEFAULT_LANG])
    }

    pub fn has_lang(&self, key: &str) -> bool {
        self.lang.contains_key(key)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../messages.example.toml");

    fn config() -> Config {
        crate::config::tests::example_config()
    }

    fn texts(toml: &str) -> anyhow::Result<Texts> {
        Texts::from_figment(Figment::from(Toml::string(toml)), &config())
    }

    pub(crate) fn example_texts() -> Texts {
        texts(EXAMPLE).unwrap()
    }

    #[test]
    fn example_is_valid() {
        let t = texts(EXAMPLE).unwrap();
        for l in &config().languages {
            assert!(t.has_lang(&l.key), "missing language {}", l.key);
        }
    }

    #[test]
    fn unknown_lang_falls_back_to_english() {
        let t = texts(EXAMPLE).unwrap();
        assert_eq!(t.lang("xx").start_button, t.lang(DEFAULT_LANG).start_button);
    }

    #[test]
    fn missing_configured_lang_is_rejected() {
        let without_tr = EXAMPLE.split("[lang.tr]").next().unwrap();
        let err = texts(without_tr).unwrap_err().to_string();
        assert!(err.contains("lang.tr"), "{err}");
    }

    #[test]
    fn too_long_modal_label_is_rejected() {
        let long = EXAMPLE.replace(
            "modal_title = \"Vérification\"",
            &format!("modal_title = \"{}\"", "x".repeat(46)),
        );
        let err = texts(&long).unwrap_err().to_string();
        assert!(err.contains("lang.fr.modal_title"), "{err}");
    }
}
