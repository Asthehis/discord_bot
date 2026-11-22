use crate::{Context, Error, roles, verification};

/// Poste le message avec le bouton de vérification.
#[poise::command(
    slash_command,
    rename = "setup-verification",
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn setup_verification(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    data.config
        .channels
        .verification
        .send_message(ctx.serenity_context(), verification::verification_message(data))
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .content(&data.texts.common.setup_done)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Poste le menu de choix des langues.
#[poise::command(
    slash_command,
    rename = "setup-roles",
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn setup_roles(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    data.config
        .channels
        .roles
        .send_message(ctx.serenity_context(), roles::roles_message(data))
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .content(&data.texts.common.setup_done)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}
