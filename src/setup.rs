//! `/setup` : configuration du bot par serveur (Administrateur).

use poise::serenity_prelude::{
    self as serenity, AutocompleteChoice, ChannelId, CreateAutocompleteResponse, CreateEmbed,
    GuildId, Mentionable,
};
use tracing::info;

use crate::{
    Context, Error, roles,
    settings::{GuildSettings, Verification},
    verification,
};

const EMBED_COLOR: u32 = 0x5865F2;

#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum Feature {
    #[name = "Verification"]
    Verification,
    #[name = "Language roles"]
    Languages,
    #[name = "Gift codes"]
    GiftCodes,
    #[name = "Event reminders"]
    Reminders,
}

#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum Post {
    #[name = "Verification message"]
    Verification,
    #[name = "Language roles menu"]
    Languages,
}

/// Configure the bot on this server.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR",
    subcommands(
        "verification",
        "languages",
        "language",
        "gift_codes",
        "reminders",
        "disable",
        "post",
        "show"
    ),
    subcommand_required
)]
pub async fn setup(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Set up member verification (first time: fill in every option).
#[poise::command(slash_command, guild_only)]
#[allow(clippy::too_many_arguments)] // une option slash = un argument
async fn verification(
    ctx: Context<'_>,
    #[description = "Channel with the verification button"]
    #[channel_types("Text")]
    channel: Option<serenity::GuildChannel>,
    #[description = "Category where private verification channels are created"]
    #[channel_types("Category")]
    category: Option<serenity::GuildChannel>,
    #[description = "Role given to verified members"] verified: Option<serenity::Role>,
    #[description = "Role given to guests"] guest: Option<serenity::Role>,
    #[description = "Role allowed to validate requests"] moderator: Option<serenity::Role>,
    #[description = "Channel where new members are welcomed"]
    #[channel_types("Text")]
    welcome: Option<serenity::GuildChannel>,
    #[description = "Rules channel (linked in the welcome message)"]
    #[channel_types("Text")]
    rules: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.setup;
    let guild = guild(ctx);
    let current = data.guilds.get(guild).verification;
    let cur = current.as_ref();

    let mut missing = Vec::new();
    let mut pick = |name: &'static str, new: Option<u64>, old: Option<u64>| {
        let v = new.or(old);
        if v.is_none() {
            missing.push(format!("`{name}`"));
        }
        v.unwrap_or(1)
    };
    let new = Verification {
        channel: ChannelId::new(pick("channel", channel.map(|c| c.id.get()), cur.map(|v| v.channel.get()))),
        category: ChannelId::new(pick("category", category.map(|c| c.id.get()), cur.map(|v| v.category.get()))),
        verified: pick("verified", verified.map(|r| r.id.get()), cur.map(|v| v.verified.get())).into(),
        guest: pick("guest", guest.map(|r| r.id.get()), cur.map(|v| v.guest.get())).into(),
        moderator: pick("moderator", moderator.map(|r| r.id.get()), cur.map(|v| v.moderator.get())).into(),
        welcome: ChannelId::new(pick("welcome", welcome.map(|c| c.id.get()), cur.map(|v| v.welcome.get()))),
        rules: ChannelId::new(pick("rules", rules.map(|c| c.id.get()), cur.map(|v| v.rules.get()))),
    };
    if !missing.is_empty() {
        return reply(ctx, t.verification_incomplete.replace("{missing}", &missing.join(", "))).await;
    }

    // Nouveau salon (ou première config) : on y poste le message.
    let post_in = (cur.map(|v| v.channel) != Some(new.channel)).then_some(new.channel);
    data.guilds.update(guild, |s| s.verification = Some(new)).await?;
    info!(%guild, user = %ctx.author().id, "verification configured");

    let mut msg = t.saved.clone();
    if let Some(channel) = post_in {
        channel.send_message(ctx.serenity_context(), verification::verification_message(data)).await?;
        msg = t.posted.replace("{channel}", &channel.mention().to_string());
    }
    refresh_commands(ctx, guild).await;
    reply(ctx, msg).await
}

/// Set the channel of the language roles menu.
#[poise::command(slash_command, guild_only)]
async fn languages(
    ctx: Context<'_>,
    #[description = "Channel where the language menu is posted"]
    #[channel_types("Text")]
    channel: serenity::GuildChannel,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.setup;
    let guild = guild(ctx);
    if data.guilds.get(guild).languages.is_empty() {
        return reply(ctx, &t.languages_empty).await;
    }
    data.guilds.update(guild, |s| s.roles_channel = Some(channel.id)).await?;
    channel.id.send_message(ctx.serenity_context(), roles::roles_message(data)).await?;
    info!(%guild, user = %ctx.author().id, "language menu configured");
    reply(ctx, t.posted.replace("{channel}", &channel.mention().to_string())).await
}

/// Link a language to a role (leave `role` empty to remove it).
#[poise::command(slash_command, guild_only)]
async fn language(
    ctx: Context<'_>,
    #[description = "Language"]
    #[autocomplete = "autocomplete_language"]
    key: String,
    #[description = "Role given to members who pick it (empty = remove)"] role: Option<serenity::Role>,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.setup;
    let guild = guild(ctx);
    let Some(lang) = data.config.language(&key) else {
        return reply(ctx, &t.language_unknown).await;
    };
    let key = lang.key.clone();
    let settings = data
        .guilds
        .update(guild, |s| match role.as_ref() {
            Some(r) => match s.languages.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = r.id,
                None => s.languages.push((key.clone(), r.id)),
            },
            None => s.languages.retain(|(k, _)| *k != key),
        })
        .await?;
    info!(%guild, user = %ctx.author().id, lang = key, "language role updated");
    let list = format_languages(ctx, &settings);
    reply(ctx, format!("{}\n{list}", t.saved)).await
}

/// Set the gift codes channel (enables /register on this server).
#[poise::command(slash_command, guild_only, rename = "gift-codes")]
async fn gift_codes(
    ctx: Context<'_>,
    #[description = "Channel where codes are detected and summaries posted"]
    #[channel_types("Text", "News")]
    channel: serenity::GuildChannel,
) -> Result<(), Error> {
    let data = ctx.data();
    let guild = guild(ctx);
    data.guilds.update(guild, |s| s.gift_channel = Some(channel.id)).await?;
    info!(%guild, user = %ctx.author().id, channel = %channel.id, "gift codes configured");
    refresh_commands(ctx, guild).await;
    reply(ctx, &data.texts.setup.saved).await
}

/// Enable or disable event reminders (/event).
#[poise::command(slash_command, guild_only)]
async fn reminders(
    ctx: Context<'_>,
    #[description = "Enable /event on this server"] enabled: bool,
) -> Result<(), Error> {
    let data = ctx.data();
    let guild = guild(ctx);
    data.guilds.update(guild, |s| s.reminders = enabled).await?;
    info!(%guild, user = %ctx.author().id, enabled, "reminders configured");
    refresh_commands(ctx, guild).await;
    reply(ctx, &data.texts.setup.saved).await
}

/// Turn a feature off on this server.
#[poise::command(slash_command, guild_only)]
async fn disable(
    ctx: Context<'_>,
    #[description = "Feature to disable"] feature: Feature,
) -> Result<(), Error> {
    let data = ctx.data();
    let guild = guild(ctx);
    data.guilds
        .update(guild, |s| match feature {
            Feature::Verification => s.verification = None,
            Feature::Languages => {
                s.roles_channel = None;
                s.languages.clear();
            }
            Feature::GiftCodes => s.gift_channel = None,
            Feature::Reminders => s.reminders = false,
        })
        .await?;
    info!(%guild, user = %ctx.author().id, ?feature, "feature disabled");
    refresh_commands(ctx, guild).await;
    reply(ctx, &data.texts.setup.saved).await
}

/// Post the verification message or the language menu again.
#[poise::command(slash_command, guild_only)]
async fn post(
    ctx: Context<'_>,
    #[description = "Message to post"] what: Post,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.setup;
    let settings = data.guilds.get(guild(ctx));
    let (channel, msg) = match what {
        Post::Verification => match &settings.verification {
            Some(v) => (v.channel, verification::verification_message(data)),
            None => return reply(ctx, &t.not_configured).await,
        },
        Post::Languages => match settings.roles_channel {
            Some(c) if !settings.languages.is_empty() => (c, roles::roles_message(data)),
            _ => return reply(ctx, &t.not_configured).await,
        },
    };
    channel.send_message(ctx.serenity_context(), msg).await?;
    reply(ctx, t.posted.replace("{channel}", &channel.mention().to_string())).await
}

/// Show this server's settings.
#[poise::command(slash_command, guild_only)]
async fn show(ctx: Context<'_>) -> Result<(), Error> {
    let s = ctx.data().guilds.get(guild(ctx));
    let off = "❌ off".to_string();
    let verification = s.verification.as_ref().map_or(off.clone(), |v| {
        format!(
            "✅ {} · category {}\nVerified {} · Guest {} · Mod {}\nWelcome {} · Rules {}",
            v.channel.mention(),
            v.category.mention(),
            v.verified.mention(),
            v.guest.mention(),
            v.moderator.mention(),
            v.welcome.mention(),
            v.rules.mention(),
        )
    });
    let languages = match s.roles_channel {
        Some(c) => format!("✅ {}\n{}", c.mention(), format_languages(ctx, &s)),
        None if !s.languages.is_empty() => format!("⚠️ no channel\n{}", format_languages(ctx, &s)),
        None => off.clone(),
    };
    let gift = s.gift_channel.map_or(off.clone(), |c| format!("✅ {}", c.mention()));
    let reminders = if s.reminders { "✅ on".to_string() } else { off };

    let embed = CreateEmbed::new()
        .title(&ctx.data().texts.setup.show_title)
        .color(EMBED_COLOR)
        .field("Verification", verification, false)
        .field("Language roles", languages, false)
        .field("Gift codes", gift, false)
        .field("Event reminders", reminders, false);
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true)).await?;
    Ok(())
}

async fn autocomplete_language(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let partial = partial.to_lowercase();
    let choices = ctx
        .data()
        .config
        .languages
        .iter()
        .filter(|l| l.key.contains(&partial) || l.label.to_lowercase().contains(&partial))
        .take(25)
        .map(|l| AutocompleteChoice::new(l.label.clone(), l.key.clone()))
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}

fn format_languages(ctx: Context<'_>, s: &GuildSettings) -> String {
    if s.languages.is_empty() {
        return "—".into();
    }
    s.languages
        .iter()
        .map(|(key, role)| {
            let label = ctx.data().config.language(key).map_or(key.as_str(), |l| l.label.as_str());
            format!("• {label} → {}", role.mention())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Met à jour les commandes visibles sur le serveur (selon les fonctionnalités actives).
async fn refresh_commands(ctx: Context<'_>, guild: GuildId) {
    crate::register_commands(
        ctx.serenity_context(),
        ctx.data(),
        &ctx.framework().options().commands,
        guild,
    )
    .await;
}

fn guild(ctx: Context<'_>) -> GuildId {
    // Toutes les sous-commandes sont `guild_only`.
    ctx.guild_id().expect("guild_only")
}

async fn reply(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().content(content).ephemeral(true)).await?;
    Ok(())
}
