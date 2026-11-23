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

    let welcome = data
        .texts
        .common
        .welcome
        .replace("{user}", &event.user.id.mention().to_string())
        .replace("{rules}", &v.rules.mention().to_string())
        .replace(
            "{roles}",
            &settings.roles_channel.map_or_else(String::new, |c| c.mention().to_string()),
        );

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
