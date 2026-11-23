use poise::serenity_prelude::{
    self as serenity, CreateAllowedMentions, CreateMessage, GuildId, GuildMemberUpdateEvent,
    Mentionable, User,
};
use tracing::{debug, warn};

use crate::{Data, Error, db, texts::DEFAULT_LANG, verification};

/// Passage à Verified/Guest avec un salon de vérif ouvert → welcome + suppression du salon.
/// Sans salon ouvert (membre déjà validé, rôle donné hors process), rien ne se passe.
pub async fn on_update(
    ctx: &serenity::Context,
    data: &Data,
    event: &GuildMemberUpdateEvent,
) -> Result<(), Error> {
    let settings = data.guilds.get(event.guild_id);
    let Some(v) = &settings.verification else {
        return Ok(());
    };
    if !verification::is_validated(v, &event.roles) {
        return Ok(());
    }
    let Some(channel) = verification::find_pending_channel(ctx, event.guild_id, v, event.user.id)
    else {
        return Ok(());
    };

    // La suppression sert de verrou : si deux updates arrivent avant que le cache ne
    // soit à jour, seule la première réussit et poste le welcome.
    if let Err(e) = channel.delete(ctx).await {
        debug!(user = %event.user.id, "verification channel already deleted ({e})");
        return Ok(());
    }

    // Langue choisie à la vérification (enregistrée par `verification::on_decision`).
    let lang = match db::member_lang(&data.db, event.user.id.get()).await {
        Ok(lang) => lang.unwrap_or_else(|| DEFAULT_LANG.into()),
        Err(e) => {
            warn!(user = %event.user.id, "reading the member language: {e}");
            DEFAULT_LANG.into()
        }
    };
    let fill = |text: &str| {
        text.replace("{user}", &event.user.id.mention().to_string())
            .replace("{rules}", &v.rules.mention().to_string())
            .replace(
                "{roles}",
                &settings.roles_channel.map_or_else(String::new, |c| c.mention().to_string()),
            )
    };
    let mut welcome = fill(&data.texts.lang(&lang).welcome);
    // Version anglaise en petit dessous, pour le reste du serveur.
    if lang != DEFAULT_LANG && data.texts.has_lang(&lang) {
        welcome = format!("{}\n\n{}", welcome.trim_end(), subtext(&fill(&data.texts.lang(DEFAULT_LANG).welcome)));
    }

    v.welcome
        .send_message(
            ctx,
            CreateMessage::new()
                .content(welcome)
                .allowed_mentions(CreateAllowedMentions::new().users([event.user.id])),
        )
        .await?;
    Ok(())
}

/// Chaque ligne non vide en petit (`-# `) ; les lignes vides sont retirées.
fn subtext(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| format!("-# {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn on_removal(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: &User,
) -> Result<(), Error> {
    let Some(v) = data.guilds.get(guild_id).verification else {
        return Ok(());
    };
    if let Some(channel) = verification::find_pending_channel(ctx, guild_id, &v, user.id)
        && let Err(e) = channel.delete(ctx).await
    {
        debug!(user = %user.id, "verification channel already deleted ({e})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtext_lines() {
        assert_eq!(subtext("Hello\n\n**Bold**\n"), "-# Hello\n-# **Bold**");
    }

    #[test]
    fn welcome_fits_with_english() {
        let t = crate::texts::tests::example_texts();
        let en = t.lang(DEFAULT_LANG).welcome.len();
        for key in ["fr", "ar", "tr"] {
            let total = t.lang(key).welcome.len() + en + 200;
            assert!(total < 2000, "{key}: {total}");
        }
    }
}
