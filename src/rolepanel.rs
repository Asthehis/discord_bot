//! Panneaux de rôles en libre-service : `/roles create|add|remove|delete|list`.
//!
//! Un panneau = un message (embed + un bouton par rôle, 25 max). Un clic donne le rôle,
//! un second le retire. Sans état : `custom_id` = `rp:<role_id>`, vérifié en base au clic.

use std::sync::LazyLock;

use poise::serenity_prelude::{
    self as serenity, AutocompleteChoice, ButtonStyle, ComponentInteraction, CreateActionRow,
    CreateAutocompleteResponse, CreateButton, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage, GuildId, Mentionable,
    Permissions, ReactionType, Role, RoleId,
};
use tracing::{info, warn};

use crate::{Context, Data, Error, db, events::is_unknown_message, texts::PanelTexts};

const PREFIX: &str = "rp:";
const MAX_ROLES: usize = 25;
const PER_ROW: usize = 5;
const MAX_LABEL: usize = 80;
const EMBED_COLOR: u32 = 0x5865F2;

/// Permissions qu'un rôle en libre-service ne doit jamais donner.
const DANGEROUS: Permissions = Permissions::ADMINISTRATOR
    .union(Permissions::MANAGE_GUILD)
    .union(Permissions::MANAGE_ROLES)
    .union(Permissions::MANAGE_CHANNELS)
    .union(Permissions::MANAGE_MESSAGES)
    .union(Permissions::MANAGE_WEBHOOKS)
    .union(Permissions::MANAGE_GUILD_EXPRESSIONS)
    .union(Permissions::MANAGE_EVENTS)
    .union(Permissions::MANAGE_THREADS)
    .union(Permissions::MANAGE_NICKNAMES)
    .union(Permissions::KICK_MEMBERS)
    .union(Permissions::BAN_MEMBERS)
    .union(Permissions::MODERATE_MEMBERS)
    .union(Permissions::MENTION_EVERYONE)
    .union(Permissions::VIEW_AUDIT_LOG);

/// Une modification de panneau à la fois (lecture base → édition du message → écriture).
static LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

pub fn handles(custom_id: &str) -> bool {
    custom_id.starts_with(PREFIX)
}

fn parse_custom_id(custom_id: &str) -> Option<RoleId> {
    custom_id
        .strip_prefix(PREFIX)?
        .parse()
        .ok()
        .filter(|&id| id != 0)
        .map(RoleId::new)
}

// --- rendu -------------------------------------------------------------------

fn emoji(e: &db::PanelEntry) -> Option<ReactionType> {
    e.emoji
        .as_deref()
        .and_then(|s| ReactionType::try_from(s).ok())
}

fn embed(t: &PanelTexts, panel: &db::Panel, entries: &[db::PanelEntry]) -> CreateEmbed {
    let lines: Vec<String> = entries
        .iter()
        .map(|e| {
            t.line
                .replace("{emoji}", e.emoji.as_deref().unwrap_or("•"))
                .replace(
                    "{role}",
                    &RoleId::new(e.role_id as u64).mention().to_string(),
                )
        })
        .collect();
    let body = if lines.is_empty() {
        t.empty.clone()
    } else {
        lines.join("\n")
    };
    let description = match panel.description.trim() {
        "" => body,
        d => format!("{d}\n\n{body}"),
    };
    CreateEmbed::new()
        .title(&panel.title)
        .description(description)
        .color(EMBED_COLOR)
}

fn buttons(entries: &[db::PanelEntry]) -> Vec<CreateActionRow> {
    entries
        .chunks(PER_ROW)
        .map(|row| {
            CreateActionRow::Buttons(
                row.iter()
                    .map(|e| {
                        let b = CreateButton::new(format!("{PREFIX}{}", e.role_id))
                            .label(&e.label)
                            .style(ButtonStyle::Secondary);
                        match emoji(e) {
                            Some(r) => b.emoji(r),
                            None => b,
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Réécrit le message du panneau avec `entries`.
async fn apply(
    http: &serenity::Http,
    t: &PanelTexts,
    panel: &db::Panel,
    entries: &[db::PanelEntry],
) -> Result<(), Box<serenity::Error>> {
    let channel = serenity::ChannelId::new(panel.channel_id as u64);
    let message = serenity::MessageId::new(panel.message_id as u64);
    channel
        .edit_message(
            http,
            message,
            EditMessage::new()
                .embed(embed(t, panel, entries))
                .components(buttons(entries)),
        )
        .await
        .map(|_| ())
        .map_err(Box::new)
}

// --- commandes ---------------------------------------------------------------

async fn reply(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

fn guild(ctx: Context<'_>) -> GuildId {
    ctx.guild_id().expect("guild_only")
}

/// Self-assignable role panels.
#[poise::command(
    slash_command,
    rename = "roles",
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    subcommands("create", "add", "remove", "delete", "list"),
    subcommand_required
)]
pub async fn roles(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Post a new role panel.
#[poise::command(slash_command, guild_only)]
async fn create(
    ctx: Context<'_>,
    #[description = "Channel for the panel"]
    #[channel_types("Text", "News")]
    channel: serenity::GuildChannel,
    #[description = "Panel title"]
    #[max_length = 256]
    title: String,
    #[description = "Text shown above the roles"]
    #[max_length = 2000]
    description: Option<String>,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.panels;
    let guild = guild(ctx);
    let description = description.unwrap_or_default().replace("\\n", "\n");
    ctx.defer_ephemeral().await?;

    let draft = db::Panel {
        id: 0,
        channel_id: 0,
        message_id: 0,
        title,
        description,
    };
    let msg = channel
        .id
        .send_message(ctx, CreateMessage::new().embed(embed(t, &draft, &[])))
        .await?;
    let id = db::insert_panel(
        &data.db,
        guild.get(),
        channel.id.get(),
        msg.id.get(),
        &draft.title,
        &draft.description,
    )
    .await?;
    info!(%guild, id, user = %ctx.author().id, channel = %channel.id, "role panel created");
    let text = t
        .created
        .replace("{id}", &id.to_string())
        .replace("{channel}", &channel.mention().to_string());
    reply(ctx, text).await
}

/// Add a role to a panel (or change its emoji/label).
#[poise::command(slash_command, guild_only)]
async fn add(
    ctx: Context<'_>,
    #[description = "Panel"]
    #[autocomplete = "autocomplete_panel"]
    panel: i64,
    #[description = "Role members can pick"] role: Role,
    #[description = "Button emoji (standard or from this server)"] emoji: Option<String>,
    #[description = "Button text (default: role name)"]
    #[max_length = 80]
    label: Option<String>,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.panels;
    let guild = guild(ctx);
    let mention = role.mention().to_string();
    let fill = |s: &str| {
        s.replace("{role}", &mention)
            .replace("{id}", &panel.to_string())
    };

    if let Some(problem) = check_role(ctx, &role).await? {
        return reply(ctx, fill(problem)).await;
    }
    let emoji = emoji
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty());
    if emoji
        .as_deref()
        .is_some_and(|e| ReactionType::try_from(e).is_err())
    {
        return reply(ctx, &t.bad_emoji).await;
    }

    ctx.defer_ephemeral().await?;
    let _lock = LOCK.lock().await;
    let Some(p) = db::panel(&data.db, guild.get(), panel).await? else {
        return reply(ctx, &t.not_found).await;
    };
    let mut entries = db::panel_entries(&data.db, p.id).await?;
    let label: String = label
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| role.name.clone())
        .chars()
        .take(MAX_LABEL)
        .collect();
    let entry = db::PanelEntry {
        role_id: role.id.get() as i64,
        emoji,
        label,
    };
    let existing = entries.iter().position(|e| e.role_id == entry.role_id);
    match existing {
        Some(i) => entries[i] = entry.clone(),
        None if entries.len() >= MAX_ROLES => return reply(ctx, &t.full).await,
        None => entries.push(entry.clone()),
    }

    if let Err(e) = apply(ctx.http(), t, &p, &entries).await {
        if is_unknown_message(&e) {
            return reply(ctx, &t.message_gone).await;
        }
        if entry.emoji.is_some() {
            warn!(%guild, panel, "editing the role panel: {e}");
            return reply(ctx, &t.bad_emoji).await;
        }
        return Err(e.into());
    }
    db::upsert_panel_entry(&data.db, p.id, &entry).await?;
    info!(%guild, panel, role = %role.id, user = %ctx.author().id, "role added to panel");
    reply(
        ctx,
        fill(if existing.is_some() {
            &t.updated
        } else {
            &t.added
        }),
    )
    .await
}

/// Remove a role from a panel.
#[poise::command(slash_command, guild_only)]
async fn remove(
    ctx: Context<'_>,
    #[description = "Panel"]
    #[autocomplete = "autocomplete_panel"]
    panel: i64,
    #[description = "Role to remove"] role: Role,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.panels;
    let guild = guild(ctx);
    let fill = |s: &str| {
        s.replace("{role}", &role.mention().to_string())
            .replace("{id}", &panel.to_string())
    };
    ctx.defer_ephemeral().await?;
    let _lock = LOCK.lock().await;
    let Some(p) = db::panel(&data.db, guild.get(), panel).await? else {
        return reply(ctx, &t.not_found).await;
    };
    if !db::delete_panel_entry(&data.db, p.id, role.id.get()).await? {
        return reply(ctx, fill(&t.not_in_panel)).await;
    }
    let entries = db::panel_entries(&data.db, p.id).await?;
    if let Err(e) = apply(ctx.http(), t, &p, &entries).await {
        warn!(%guild, panel, "editing the role panel: {e}");
    }
    info!(%guild, panel, role = %role.id, user = %ctx.author().id, "role removed from panel");
    reply(ctx, fill(&t.removed)).await
}

/// Delete a panel and its message.
#[poise::command(slash_command, guild_only)]
async fn delete(
    ctx: Context<'_>,
    #[description = "Panel"]
    #[autocomplete = "autocomplete_panel"]
    panel: i64,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.panels;
    let guild = guild(ctx);
    let _lock = LOCK.lock().await;
    let Some(p) = db::panel(&data.db, guild.get(), panel).await? else {
        return reply(ctx, &t.not_found).await;
    };
    let channel = serenity::ChannelId::new(p.channel_id as u64);
    let _ = channel
        .delete_message(ctx, serenity::MessageId::new(p.message_id as u64))
        .await;
    db::delete_panel(&data.db, guild.get(), p.id).await?;
    info!(%guild, panel, user = %ctx.author().id, "role panel deleted");
    reply(ctx, t.deleted.replace("{id}", &panel.to_string())).await
}

/// List this server's role panels.
#[poise::command(slash_command, guild_only)]
async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.panels;
    let panels = db::panels_of(&data.db, guild(ctx).get()).await?;
    if panels.is_empty() {
        return reply(ctx, &t.list_empty).await;
    }
    let mut lines = Vec::with_capacity(panels.len());
    for p in &panels {
        let roles: Vec<String> = db::panel_entries(&data.db, p.id)
            .await?
            .iter()
            .map(|e| RoleId::new(e.role_id as u64).mention().to_string())
            .collect();
        let roles = if roles.is_empty() {
            "—".into()
        } else {
            roles.join(" ")
        };
        lines.push(
            t.list_line
                .replace("{id}", &p.id.to_string())
                .replace("{title}", &p.title)
                .replace(
                    "{channel}",
                    &serenity::ChannelId::new(p.channel_id as u64)
                        .mention()
                        .to_string(),
                )
                .replace("{roles}", &roles),
        );
    }
    let embed = CreateEmbed::new()
        .title(&t.list_title)
        .description(lines.join("\n"))
        .color(EMBED_COLOR);
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

async fn autocomplete_panel(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let Some(guild) = ctx.guild_id() else {
        return CreateAutocompleteResponse::new();
    };
    let partial = partial.to_lowercase();
    let panels = db::panels_of(&ctx.data().db, guild.get())
        .await
        .unwrap_or_default();
    let choices = panels
        .into_iter()
        .filter(|p| p.title.to_lowercase().contains(&partial) || p.id.to_string() == partial)
        .take(25)
        .map(|p| {
            let title: String = p.title.chars().take(90).collect();
            AutocompleteChoice::new(format!("#{} {title}", p.id), p.id)
        })
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}

/// Raison de refuser `role` (texte à renvoyer), sinon `None`.
async fn check_role<'a>(ctx: Context<'a>, role: &Role) -> Result<Option<&'a str>, Error> {
    let t = &ctx.data().texts.panels;
    let guild = guild(ctx);
    if role.managed || role.id.get() == guild.get() {
        return Ok(Some(&t.role_managed));
    }
    if role.permissions.intersects(DANGEROUS) {
        return Ok(Some(&t.role_dangerous));
    }
    let bot_id = ctx.cache().current_user().id;
    let bot = guild.member(ctx, bot_id).await?;
    let author = ctx
        .author_member()
        .await
        .ok_or_else(|| anyhow::anyhow!("author not found"))?;

    // Positions lues dans le cache, sans `await` pendant qu'il est emprunté.
    let (bot_pos, author_pos, is_owner) = {
        let g = ctx
            .guild()
            .ok_or_else(|| anyhow::anyhow!("guild not in cache"))?;
        let pos = |m: &serenity::Member| g.member_highest_role(m).map_or(0, |r| r.position);
        (pos(&bot), pos(&author), g.owner_id == ctx.author().id)
    };
    if bot_pos <= role.position {
        return Ok(Some(&t.role_above_bot));
    }
    if !is_owner && author_pos <= role.position {
        return Ok(Some(&t.role_above_user));
    }
    Ok(None)
}

// --- clics -------------------------------------------------------------------

pub async fn on_component(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
) -> Result<(), Error> {
    let (Some(member), Some(guild), Some(role)) = (
        i.member.as_ref(),
        i.guild_id,
        parse_custom_id(&i.data.custom_id),
    ) else {
        return Ok(());
    };
    let t = &data.texts.panels;
    let mention = role.mention().to_string();

    // Rôle bien présent sur ce panneau, et toujours sans permission sensible.
    let allowed = db::panel_has_role(&data.db, guild.get(), i.message.id.get(), role.get()).await?
        && ctx.cache.guild(guild).is_some_and(|g| {
            g.roles
                .get(&role)
                .is_some_and(|r| !r.managed && !r.permissions.intersects(DANGEROUS))
        });

    let has = member.roles.contains(&role);
    let result = if !allowed {
        Err(None)
    } else if has {
        member.remove_role(ctx, role).await.map_err(Some)
    } else {
        member.add_role(ctx, role).await.map_err(Some)
    };
    let content = match &result {
        Ok(()) if has => t.taken.replace("{role}", &mention),
        Ok(()) => t.given.replace("{role}", &mention),
        Err(e) => {
            warn!(%guild, role = %role, user = %member.user.id, "self-assign failed: {}",
                e.as_ref().map_or_else(|| "role not allowed".into(), ToString::to_string));
            t.failed.clone()
        }
    };
    let msg = CreateInteractionResponseMessage::new()
        .content(content)
        .ephemeral(true);
    i.create_response(ctx, CreateInteractionResponse::Message(msg))
        .await?;
    Ok(())
}

/// Rôle supprimé du serveur : retiré des panneaux qui l'avaient.
pub async fn on_role_delete(
    http: &serenity::Http,
    data: &Data,
    guild: GuildId,
    role: RoleId,
) -> Result<(), Error> {
    let t = &data.texts.panels;
    let _lock = LOCK.lock().await;
    for id in db::forget_panel_role(&data.db, guild.get(), role.get()).await? {
        let Some(p) = db::panel(&data.db, guild.get(), id).await? else {
            continue;
        };
        let entries = db::panel_entries(&data.db, id).await?;
        if let Err(e) = apply(http, t, &p, &entries).await {
            warn!(%guild, panel = id, "editing the role panel: {e}");
        }
        info!(%guild, panel = id, role = %role, "deleted role removed from panel");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_id_roundtrip() {
        assert!(handles("rp:123"));
        assert!(!handles("roles:open"));
        assert_eq!(parse_custom_id("rp:123"), Some(RoleId::new(123)));
        assert_eq!(parse_custom_id("rp:0"), None);
        assert_eq!(parse_custom_id("rp:abc"), None);
    }

    #[test]
    fn buttons_wrap_by_five() {
        let entries: Vec<_> = (1..=12)
            .map(|n| db::PanelEntry {
                role_id: n,
                emoji: None,
                label: format!("R{n}"),
            })
            .collect();
        assert_eq!(buttons(&entries).len(), 3);
        assert!(buttons(&[]).is_empty());
    }

    #[test]
    fn dangerous_permissions() {
        assert!(Permissions::ADMINISTRATOR.intersects(DANGEROUS));
        assert!(!(Permissions::SEND_MESSAGES | Permissions::VIEW_CHANNEL).intersects(DANGEROUS));
    }
}
