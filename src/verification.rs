use poise::serenity_prelude::{
    self as serenity, ActionRowComponent, ButtonStyle, ChannelId, ChannelType,
    ComponentInteraction, ComponentInteractionDataKind, CreateActionRow, CreateAllowedMentions,
    CreateButton, CreateChannel, CreateEmbed, CreateInputText, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateModal, CreateSelectMenu,
    CreateSelectMenuKind, EditInteractionResponse, EditMember,
    InputTextStyle, Member, Mentionable, Message, ModalInteraction, PermissionOverwrite,
    PermissionOverwriteType, Permissions, ReactionType, RoleId, UserId,
};
use tracing::{info, warn};

use crate::{
    Data, Error,
    texts::{DEFAULT_LANG, Lang},
};

// custom_ids fixes. `start` et `modal` portent la langue en suffixe (`verif:start:fr`).
const PREFIX: &str = "verif:";
const ID_START: &str = "verif:start";
const ID_LANG: &str = "verif:lang";
const ID_MODAL: &str = "verif:modal";
const ID_VERIFIED: &str = "verif:verified";
const ID_GUEST: &str = "verif:guest";
const ID_REFUSE: &str = "verif:refuse";

const FIELD_NAME: &str = "name";
const FIELD_ALLIANCE: &str = "alliance";
const FIELD_KINGDOM: &str = "kingdom";

/// Limite Discord d'un pseudo.
const NAME_MAX: u16 = 32;
const ALLIANCE_MAX: u16 = 32;
const KINGDOM_MAX: u16 = 6;

const EMBED_COLOR: u32 = 0x5865F2;

#[derive(Clone, Copy)]
enum Decision {
    Verified,
    Guest,
    Refuse,
}

pub fn handles(custom_id: &str) -> bool {
    custom_id.starts_with(PREFIX)
}

/// Message de #verification, posté par `/setup-verification`.
pub fn verification_message(data: &Data) -> CreateMessage {
    let en = data.texts.lang(DEFAULT_LANG);
    let options = data.config.languages.iter().map(|l| l.select_option(false)).collect();

    CreateMessage::new()
        .embed(verif_embed(en))
        .components(vec![
            CreateActionRow::Buttons(vec![start_button(DEFAULT_LANG, en)]),
            CreateActionRow::SelectMenu(
                CreateSelectMenu::new(ID_LANG, CreateSelectMenuKind::String { options })
                    .placeholder(&en.lang_placeholder),
            ),
        ])
}

pub async fn on_component(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
) -> Result<(), Error> {
    match i.data.custom_id.as_str() {
        ID_LANG => on_lang_select(ctx, data, i).await,
        ID_VERIFIED => on_decision(ctx, data, i, Decision::Verified).await,
        ID_GUEST => on_decision(ctx, data, i, Decision::Guest).await,
        ID_REFUSE => on_decision(ctx, data, i, Decision::Refuse).await,
        id => match id.strip_prefix(ID_START) {
            Some(rest) => on_start(ctx, data, i, lang_key(rest)).await,
            None => Ok(()),
        },
    }
}

pub async fn on_modal(
    ctx: &serenity::Context,
    data: &Data,
    i: &ModalInteraction,
) -> Result<(), Error> {
    let Some(rest) = i.data.custom_id.strip_prefix(ID_MODAL) else {
        return Ok(());
    };
    let key = lang_key(rest);
    let lang = data.texts.lang(key);
    let Some(member) = i.member.as_ref() else {
        return Ok(());
    };

    if is_validated(data, &member.roles) {
        i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
        return Ok(());
    }

    let name = modal_value(i, FIELD_NAME).trim();
    let alliance = modal_value(i, FIELD_ALLIANCE).trim();
    let kingdom = modal_value(i, FIELD_KINGDOM).trim();

    if name.is_empty() || name.chars().count() > NAME_MAX as usize {
        return Ok(i.create_response(ctx, ephemeral(&lang.invalid_name)).await?);
    }
    if kingdom.is_empty() || !kingdom.chars().all(|c| c.is_ascii_digit()) {
        return Ok(i.create_response(ctx, ephemeral(&lang.invalid_kingdom)).await?);
    }
    // Double soumission (deux modals ouverts en parallèle).
    if let Some(ch) = find_pending_channel(ctx, data, member.user.id) {
        let msg = lang.pending_exists.replace("{channel}", &ch.mention().to_string());
        return Ok(i.create_response(ctx, ephemeral(&msg)).await?);
    }

    i.defer_ephemeral(ctx).await?;
    let ticket = Ticket { name, alliance, kingdom, lang_key: key };
    match create_ticket(ctx, data, member, &ticket, lang).await {
        Ok(ch) => {
            info!(user = %member.user.id, lang = key, channel = %ch, "demande de vérif créée");
            let msg = lang.request_created.replace("{channel}", &ch.mention().to_string());
            i.edit_response(ctx, EditInteractionResponse::new().content(msg)).await?;
            Ok(())
        }
        Err(e) => {
            let msg = &data.texts.common.action_failed;
            i.edit_response(ctx, EditInteractionResponse::new().content(msg)).await?;
            Err(e)
        }
    }
}

/// Le membre a déjà Verified ou Guest.
pub fn is_validated(data: &Data, roles: &[RoleId]) -> bool {
    let r = &data.config.roles;
    roles.contains(&r.verified) || roles.contains(&r.guest)
}

/// Salon de vérif ouvert pour ce membre (lu dans le cache, via le topic).
pub fn find_pending_channel(
    ctx: &serenity::Context,
    data: &Data,
    user_id: UserId,
) -> Option<ChannelId> {
    let topic = topic_for(user_id);
    let guild = ctx.cache.guild(data.config.guild_id)?;
    guild
        .channels
        .values()
        .find(|c| {
            c.parent_id == Some(data.config.verif_category)
                && c.topic.as_deref() == Some(topic.as_str())
        })
        .map(|c| c.id)
}

// --- Membre ---------------------------------------------------------------

async fn on_lang_select(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
) -> Result<(), Error> {
    let key = match &i.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => {
            values.first().map(String::as_str).unwrap_or(DEFAULT_LANG)
        }
        _ => DEFAULT_LANG,
    };
    let lang = data.texts.lang(key);

    let msg = CreateInteractionResponseMessage::new()
        .ephemeral(true)
        .embed(verif_embed(lang))
        .components(vec![CreateActionRow::Buttons(vec![start_button(key, lang)])]);
    i.create_response(ctx, CreateInteractionResponse::Message(msg)).await?;
    Ok(())
}

async fn on_start(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
    key: &str,
) -> Result<(), Error> {
    let Some(member) = i.member.as_ref() else {
        return Ok(());
    };
    // Déjà vérifié : on acquitte sans rien afficher.
    if is_validated(data, &member.roles) {
        i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
        return Ok(());
    }

    let lang = data.texts.lang(key);
    if let Some(ch) = find_pending_channel(ctx, data, member.user.id) {
        let msg = lang.pending_exists.replace("{channel}", &ch.mention().to_string());
        return Ok(i.create_response(ctx, ephemeral(&msg)).await?);
    }

    let modal = CreateModal::new(format!("{ID_MODAL}:{key}"), &lang.modal_title).components(vec![
        text_input(&lang.modal_name, FIELD_NAME, NAME_MAX),
        text_input(&lang.modal_alliance, FIELD_ALLIANCE, ALLIANCE_MAX),
        text_input(&lang.modal_kingdom, FIELD_KINGDOM, KINGDOM_MAX),
    ]);
    i.create_response(ctx, CreateInteractionResponse::Modal(modal)).await?;
    Ok(())
}

struct Ticket<'a> {
    name: &'a str,
    alliance: &'a str,
    kingdom: &'a str,
    lang_key: &'a str,
}

async fn create_ticket(
    ctx: &serenity::Context,
    data: &Data,
    member: &Member,
    t: &Ticket<'_>,
    lang: &Lang,
) -> Result<ChannelId, Error> {
    let cfg = &data.config;
    let c = &data.texts.common;
    let user_id = member.user.id;
    let bot_id = ctx.cache.current_user().id;

    let access = Permissions::VIEW_CHANNEL
        | Permissions::SEND_MESSAGES
        | Permissions::READ_MESSAGE_HISTORY
        | Permissions::ATTACH_FILES;
    let allow = |allow, kind| PermissionOverwrite { allow, deny: Permissions::empty(), kind };
    let overwrites = [
        PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            kind: PermissionOverwriteType::Role(cfg.guild_id.everyone_role()),
        },
        allow(access, PermissionOverwriteType::Member(user_id)),
        allow(access, PermissionOverwriteType::Role(cfg.roles.moderator)),
        allow(access | Permissions::EMBED_LINKS, PermissionOverwriteType::Member(bot_id)),
    ];

    let channel = cfg
        .guild_id
        .create_channel(
            ctx,
            CreateChannel::new(channel_name(t.name, user_id))
                .kind(ChannelType::Text)
                .category(cfg.verif_category)
                .topic(topic_for(user_id))
                .permissions(overwrites),
        )
        .await?;

    let lang_label = cfg
        .languages
        .iter()
        .find(|l| l.key == t.lang_key)
        .map_or("English", |l| l.label.as_str());

    // Le champ 0 (pseudo) est relu par `recap_name` au moment de la validation.
    let recap = CreateEmbed::new()
        .title(&c.recap_title)
        .color(EMBED_COLOR)
        .thumbnail(member.user.face())
        .field(&c.field_name, t.name, true)
        .field(&c.field_alliance, t.alliance, true)
        .field(&c.field_kingdom, t.kingdom, true)
        .field(&c.field_language, lang_label, true);

    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(ID_VERIFIED).label(&c.button_verified).style(ButtonStyle::Success),
        CreateButton::new(ID_GUEST).label(&c.button_guest).style(ButtonStyle::Primary),
        CreateButton::new(ID_REFUSE).label(&c.button_refuse).style(ButtonStyle::Danger),
    ]);

    let mut content = format!(
        "{} {}\n{}",
        user_id.mention(),
        cfg.roles.moderator.mention(),
        lang.ticket_intro
    );
    // Version anglaise en petit sous la traduction, pour les modos.
    if t.lang_key != DEFAULT_LANG {
        content.push_str("\n-# ");
        content.push_str(&data.texts.lang(DEFAULT_LANG).ticket_intro);
    }

    let msg = CreateMessage::new()
        .content(content)
        .embed(recap)
        .components(vec![buttons])
        .allowed_mentions(
            CreateAllowedMentions::new()
                .users([user_id])
                .roles([cfg.roles.moderator]),
        );

    if let Err(e) = channel.id.send_message(ctx, msg).await {
        // Pas de salon orphelin sans boutons.
        let _ = channel.delete(ctx).await;
        return Err(e.into());
    }
    Ok(channel.id)
}

// --- Modo -----------------------------------------------------------------

async fn on_decision(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
    decision: Decision,
) -> Result<(), Error> {
    let cfg = &data.config;
    let c = &data.texts.common;

    let is_mod = i.member.as_ref().is_some_and(|m| m.roles.contains(&cfg.roles.moderator));
    if !is_mod {
        return Ok(i.create_response(ctx, ephemeral(&c.mod_only)).await?);
    }
    let Some(user_id) = ticket_owner(ctx, data, i.channel_id) else {
        i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
        return Ok(());
    };

    let role = match decision {
        Decision::Refuse => {
            i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
            i.channel_id.delete(ctx).await?;
            return Ok(());
        }
        Decision::Verified => cfg.roles.verified,
        Decision::Guest => cfg.roles.guest,
    };

    i.defer_ephemeral(ctx).await?;

    let member = match cfg.guild_id.member(ctx, user_id).await {
        Ok(m) => m,
        Err(_) => {
            i.edit_response(ctx, EditInteractionResponse::new().content(&c.member_left)).await?;
            return Ok(());
        }
    };

    let mut roles = member.roles.clone();
    if !roles.contains(&role) {
        roles.push(role);
    }

    // Rôle + pseudo en un seul appel => un seul guild_member_update.
    let mut edit = EditMember::new().roles(roles.clone());
    if let Some(name) = recap_name(&i.message) {
        edit = edit.nickname(name);
    }
    if let Err(e) = cfg.guild_id.edit_member(ctx, user_id, edit).await {
        // Renommage impossible (owner, rôle au-dessus du bot…) : on donne au moins le rôle.
        warn!(user = %user_id, "rôle + renommage impossible ({e}), rôle seul");
        if let Err(e) = cfg.guild_id.edit_member(ctx, user_id, EditMember::new().roles(roles)).await {
            i.edit_response(ctx, EditInteractionResponse::new().content(&c.action_failed)).await?;
            return Err(e.into());
        }
    }
    // Pré-remplissage de /register. Non bloquant : la validation est déjà faite.
    if let Some((name, alliance, kingdom)) = recap_info(&i.message)
        && let Err(e) = crate::db::upsert_member(&data.db, user_id.get(), name, alliance, kingdom).await
    {
        warn!(user = %user_id, "enregistrement des infos de vérif : {e}");
    }
    // La suite (welcome + suppression du salon) est faite par `members::on_update`.
    Ok(())
}

/// (pseudo, alliance, kingdom) relus dans l'embed récap (champs 0, 1, 2).
fn recap_info(message: &Message) -> Option<(&str, &str, i64)> {
    let fields = &message.embeds.first()?.fields;
    let kingdom = fields.get(2)?.value.trim().parse().ok()?;
    Some((fields.first()?.value.trim(), fields.get(1)?.value.trim(), kingdom))
}

// --- Helpers --------------------------------------------------------------

/// `":fr"` → `"fr"`, vide → langue par défaut.
fn lang_key(suffix: &str) -> &str {
    suffix.strip_prefix(':').filter(|k| !k.is_empty()).unwrap_or(DEFAULT_LANG)
}

fn topic_for(user_id: UserId) -> String {
    format!("uid:{user_id}")
}

/// Propriétaire d'un salon de vérif, lu dans son topic.
fn ticket_owner(ctx: &serenity::Context, data: &Data, channel_id: ChannelId) -> Option<UserId> {
    let guild = ctx.cache.guild(data.config.guild_id)?;
    let channel = guild.channels.get(&channel_id)?;
    if channel.parent_id != Some(data.config.verif_category) {
        return None;
    }
    let id = channel.topic.as_deref()?.strip_prefix("uid:")?.parse::<u64>().ok()?;
    Some(UserId::new(id))
}

fn recap_name(message: &Message) -> Option<&str> {
    let value = message.embeds.first()?.fields.first()?.value.trim();
    (!value.is_empty()).then_some(value)
}

fn channel_name(name: &str, user_id: UserId) -> String {
    let slug = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { format!("verif-{user_id}") } else { format!("verif-{slug}") }
}

fn verif_embed(lang: &Lang) -> CreateEmbed {
    CreateEmbed::new()
        .title(&lang.verif_title)
        .description(&lang.verif_description)
        .color(EMBED_COLOR)
}

fn start_button(key: &str, lang: &Lang) -> CreateButton {
    CreateButton::new(format!("{ID_START}:{key}"))
        .label(&lang.start_button)
        .emoji(ReactionType::Unicode("✅".into()))
        .style(ButtonStyle::Success)
}

fn text_input(label: &str, id: &str, max: u16) -> CreateActionRow {
    CreateActionRow::InputText(
        CreateInputText::new(InputTextStyle::Short, label, id)
            .required(true)
            .min_length(1)
            .max_length(max),
    )
}

fn modal_value<'a>(i: &'a ModalInteraction, id: &str) -> &'a str {
    i.data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            ActionRowComponent::InputText(t) if t.custom_id == id => t.value.as_deref(),
            _ => None,
        })
        .unwrap_or("")
}

fn ephemeral(content: &str) -> CreateInteractionResponse {
    CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().ephemeral(true).content(content),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_key_parsing() {
        assert_eq!(lang_key(":fr"), "fr");
        assert_eq!(lang_key(""), DEFAULT_LANG);
        assert_eq!(lang_key(":"), DEFAULT_LANG);
    }

    #[test]
    fn custom_ids_are_routed() {
        for id in [ID_START, ID_LANG, ID_MODAL, ID_VERIFIED, ID_GUEST, ID_REFUSE] {
            assert!(handles(id), "{id}");
        }
        assert!(!handles("roles:open"));
    }

    #[test]
    fn channel_names() {
        let uid = UserId::new(42);
        assert_eq!(channel_name("Dark Knight", uid), "verif-dark-knight");
        assert_eq!(channel_name("  [ABC] Joe!! ", uid), "verif-abc-joe");
        assert_eq!(channel_name("Çağrı", uid), "verif-çağrı");
        assert_eq!(channel_name("!!!", uid), "verif-42");
    }

    #[test]
    fn topic_roundtrip() {
        assert_eq!(topic_for(UserId::new(42)), "uid:42");
    }
}
