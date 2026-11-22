use poise::serenity_prelude::{
    self as serenity, CreateAllowedMentions, CreateMessage, GuildId, GuildMemberUpdateEvent,
    Mentionable, User,
};
use tracing::debug;

use crate::{Data, Error, verification};

/// Passage à Verified/Guest avec un salon de vérif ouvert → welcome + suppression du salon.
/// Sans salon ouvert (membre déjà validé, rôle donné hors process), rien ne se passe.
pub async fn on_update(
    ctx: &serenity::Context,
    data: &Data,
    event: &GuildMemberUpdateEvent,
) -> Result<(), Error> {
    let cfg = &data.config;
    if event.guild_id != cfg.guild_id || !verification::is_validated(data, &event.roles) {
        return Ok(());
    }
    let Some(channel) = verification::find_pending_channel(ctx, data, event.user.id) else {
        return Ok(());
    };

    // La suppression sert de verrou : si deux updates arrivent avant que le cache ne
    // soit à jour, seule la première réussit et poste le welcome.
    if let Err(e) = channel.delete(ctx).await {
        debug!(user = %event.user.id, "salon de vérif déjà supprimé ({e})");
        return Ok(());
    }

    let welcome = data
        .texts
        .common
        .welcome
        .replace("{user}", &event.user.id.mention().to_string())
        .replace("{rules}", &cfg.channels.rules.mention().to_string())
        .replace("{roles}", &cfg.channels.roles.mention().to_string());

    cfg.channels
        .general_chat
        .send_message(
            ctx,
            CreateMessage::new()
                .content(welcome)
                .allowed_mentions(CreateAllowedMentions::new().users([event.user.id])),
        )
        .await?;
    Ok(())
}

pub async fn on_removal(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: &User,
) -> Result<(), Error> {
    if guild_id != data.config.guild_id {
        return Ok(());
    }
    if let Some(channel) = verification::find_pending_channel(ctx, data, user.id)
        && let Err(e) = channel.delete(ctx).await
    {
        debug!(user = %user.id, "salon de vérif déjà supprimé ({e})");
    }
    Ok(())
}
