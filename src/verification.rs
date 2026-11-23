use poise::serenity_prelude::{
    self as serenity, ActionRowComponent, ButtonStyle, ChannelId, ChannelType,
    ComponentInteraction, ComponentInteractionDataKind, CreateActionRow, CreateAllowedMentions,
    CreateButton, CreateChannel, CreateEmbed, CreateInputText, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateModal, CreateSelectMenu,
    CreateSelectMenuKind, EditInteractionResponse, EditMember,
    GuildId, InputTextStyle, Member, Mentionable, Message, ModalInteraction, PermissionOverwrite,
    PermissionOverwriteType, Permissions, ReactionType, RoleId, UserId,
};
use tracing::{info, warn};

use crate::{
    Data, Error,
    settings::Verification,
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

/// Message du salon de vérification, posté par `/setup verification`.
pub fn verification_message(data: &Data) -> CreateMessage {
    let en = data.texts.lang(DEFAULT_LANG);
    let mut rows = vec![CreateActionRow::Buttons(vec![start_button(DEFAULT_LANG, en)])];
    // Menu de traduction, s'il y a d'autres langues que l'anglais.
    if !data.config.languages.is_empty() {
        let options = data.config.languages.iter().map(|l| l.select_option(false)).collect();
        rows.push(CreateActionRow::SelectMenu(
            CreateSelectMenu::new(ID_LANG, CreateSelectMenuKind::String { options })
                .placeholder(&en.lang_placeholder),
        ));
    }
    CreateMessage::new().embed(verif_embed(en)).components(rows)
}

/// Réglages de vérification du serveur de l'interaction.
fn settings(data: &Data, guild: Option<GuildId>) -> Option<(GuildId, Verification)> {
    let guild = guild?;
    Some((guild, data.guilds.get(guild).verification?))
}

pub async fn on_component(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
) -> Result<(), Error> {
    if i.data.custom_id == ID_LANG {
        return on_lang_select(ctx, data, i).await;
    }
    // Vérification désactivée sur ce serveur : boutons inertes.
    let Some((guild, v)) = settings(data, i.guild_id) else {
        i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
        return Ok(());
    };
    match i.data.custom_id.as_str() {
        ID_VERIFIED => on_decision(ctx, data, guild, &v, i, Decision::Verified).await,
        ID_GUEST => on_decision(ctx, data, guild, &v, i, Decision::Guest).await,
        ID_REFUSE => on_decision(ctx, data, guild, &v, i, Decision::Refuse).await,
        id => match id.strip_prefix(ID_START) {
            Some(rest) => on_start(ctx, data, guild, &v, i, lang_key(rest)).await,
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
    let (Some(member), Some((guild, v))) = (i.member.as_ref(), settings(data, i.guild_id)) else {
        return Ok(());
    };

    if is_validated(&v, &member.roles) {
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
    if let Some(ch) = find_pending_channel(ctx, guild, &v, member.user.id) {
        let msg = lang.pending_exists.replace("{channel}", &ch.mention().to_string());
        return Ok(i.create_response(ctx, ephemeral(&msg)).await?);
    }

    i.defer_ephemeral(ctx).await?;
    let ticket = Ticket { name, alliance, kingdom, lang_key: key };
    match create_ticket(ctx, data, guild, &v, member, &ticket, lang).await {
        Ok(ch) => {
            info!(%guild, user = %member.user.id, lang = key, channel = %ch, "verification request created");
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
pub fn is_validated(v: &Verification, roles: &[RoleId]) -> bool {
    roles.contains(&v.verified) || roles.contains(&v.guest)
}

/// Salon de vérif ouvert pour ce membre (lu dans le cache, via le topic).
pub fn find_pending_channel(
    ctx: &serenity::Context,
    guild: GuildId,
    v: &Verification,
    user_id: UserId,
) -> Option<ChannelId> {
    let topic = topic_for(user_id);
    let guild = ctx.cache.guild(guild)?;
    guild
        .channels
        .values()
        .find(|c| c.parent_id == Some(v.category) && c.topic.as_deref() == Some(topic.as_str()))
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
    guild: GuildId,
    v: &Verification,
    i: &ComponentInteraction,
    key: &str,
) -> Result<(), Error> {
    let Some(member) = i.member.as_ref() else {
        return Ok(());
    };
    // Déjà vérifié : on acquitte sans rien afficher.
    if is_validated(v, &member.roles) {
        i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
        return Ok(());
    }

    let lang = data.texts.lang(key);
    if let Some(ch) = find_pending_channel(ctx, guild, v, member.user.id) {
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
    guild: GuildId,
    v: &Verification,
    member: &Member,
    t: &Ticket<'_>,
    lang: &Lang,
) -> Result<ChannelId, Error> {
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
            kind: PermissionOverwriteType::Role(guild.everyone_role()),
        },
        allow(access, PermissionOverwriteType::Member(user_id)),
        allow(access, PermissionOverwriteType::Role(v.moderator)),
        allow(access | Permissions::EMBED_LINKS, PermissionOverwriteType::Member(bot_id)),
    ];

    let channel = guild
        .create_channel(
            ctx,
            CreateChannel::new(channel_name(t.name, user_id))
                .kind(ChannelType::Text)
                .category(v.category)
                .topic(topic_for(user_id))
                .permissions(overwrites),
        )
        .await?;

    let lang_label = data.config.language(t.lang_key).map_or("English", |l| l.label.as_str());

    // Champs 0 à 3 relus par `recap_info` au moment de la validation : ne pas réordonner.
    let recap = CreateEmbed::new()
        .title(&c.recap_title)
        .color(EMBED_COLOR)
        .thumbnail(member.user.face())
        .field(&c.field_name, t.name, true)
        .field(&c.field_alliance, t.alliance, true)
        .field(&c.field_kingdom, t.kingdom, true)
        .field(&c.field_language, lang_label, true);
    let recap = if data.config.alliance(t.alliance).is_none() {
        recap.description(&c.unknown_alliance)
    } else {
        recap
    };

    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(ID_VERIFIED).label(&c.button_verified).style(ButtonStyle::Success),
        CreateButton::new(ID_GUEST).label(&c.button_guest).style(ButtonStyle::Primary),
        CreateButton::new(ID_REFUSE).label(&c.button_refuse).style(ButtonStyle::Danger),
    ]);

    let mut content = format!(
        "{} {}\n{}",
        user_id.mention(),
        v.moderator.mention(),
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
                .roles([v.moderator]),
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
    guild: GuildId,
    v: &Verification,
    i: &ComponentInteraction,
    decision: Decision,
) -> Result<(), Error> {
    let c = &data.texts.common;

    let is_mod = i.member.as_ref().is_some_and(|m| m.roles.contains(&v.moderator));
    if !is_mod {
        return Ok(i.create_response(ctx, ephemeral(&c.mod_only)).await?);
    }
    let Some(user_id) = ticket_owner(ctx, guild, v, i.channel_id) else {
        i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
        return Ok(());
    };

    let role = match decision {
        Decision::Refuse => {
            i.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
            i.channel_id.delete(ctx).await?;
            return Ok(());
        }
        Decision::Verified => v.verified,
        Decision::Guest => v.guest,
    };

    i.defer_ephemeral(ctx).await?;

    let member = match guild.member(ctx, user_id).await {
        Ok(m) => m,
        Err(_) => {
            i.edit_response(ctx, EditInteractionResponse::new().content(&c.member_left)).await?;
            return Ok(());
        }
    };

    let info = recap_info(&i.message);
    let lang = info.as_ref().map_or(DEFAULT_LANG, |r| lang_from_label(data, r.lang_label));

    // Enregistré avant l'édition : `members::on_update` y lit la langue du welcome.
    // Non bloquant : sert aussi au pré-remplissage de /register.
    if let Some(r) = &info
        && let Err(e) =
            crate::db::upsert_member(&data.db, user_id.get(), r.name, r.alliance, r.kingdom, lang).await
    {
        warn!(user = %user_id, "saving verification info: {e}");
    }

    let mut base = member.roles.clone();
    if !base.contains(&role) {
        base.push(role);
    }
    // Rôle de langue (Verified et Guest) + rôle d'alliance (Verified uniquement).
    let mut roles = base.clone();
    let lang_role = data.guilds.get(guild).languages.iter().find(|(k, _)| k == lang).map(|(_, r)| *r);
    let alliance_role = match (decision, &info) {
        (Decision::Verified, Some(r)) => alliance_role(ctx, data, guild, r.alliance),
        _ => None,
    };
    for r in lang_role.into_iter().chain(alliance_role) {
        if !roles.contains(&r) {
            roles.push(r);
        }
    }

    // Rôles + pseudo en un seul appel => un seul guild_member_update.
    let mut edit = EditMember::new().roles(roles.clone());
    if let Some(r) = info.as_ref().filter(|r| !r.name.is_empty()) {
        edit = edit.nickname(r.name);
    }
    if let Err(e) = guild.edit_member(ctx, user_id, edit).await {
        // Renommage ou rôle impossible (owner, rôle au-dessus du bot…) : on essaie avec les
        // rôles seuls, puis avec le seul rôle de validation.
        warn!(%guild, user = %user_id, "cannot set roles + nickname ({e}), retrying with roles only");
        if let Err(e) = guild.edit_member(ctx, user_id, EditMember::new().roles(roles)).await {
            warn!(%guild, user = %user_id, "cannot set language/alliance roles ({e}), verification role only");
            if let Err(e) = guild.edit_member(ctx, user_id, EditMember::new().roles(base)).await {
                i.edit_response(ctx, EditInteractionResponse::new().content(&c.action_failed)).await?;
                return Err(e.into());
            }
        }
    }
    info!(%guild, user = %user_id, lang, alliance_role = ?alliance_role, language_role = ?lang_role, "member validated");
    // La suite (welcome + suppression du salon) est faite par `members::on_update`.
    Ok(())
}

/// Infos relues dans l'embed récap (champs 0 à 3).
struct RecapInfo<'a> {
    name: &'a str,
    alliance: &'a str,
    kingdom: i64,
    lang_label: &'a str,
}

fn recap_info(message: &Message) -> Option<RecapInfo<'_>> {
    let fields = &message.embeds.first()?.fields;
    Some(RecapInfo {
        name: fields.first()?.value.trim(),
        alliance: fields.get(1)?.value.trim(),
        kingdom: fields.get(2)?.value.trim().parse().ok()?,
        lang_label: fields.get(3).map_or("", |f| f.value.trim()),
    })
}

/// Clé de langue depuis le libellé affiché dans le récap (anglais si inconnu).
fn lang_from_label<'a>(data: &'a Data, label: &str) -> &'a str {
    data.config.languages.iter().find(|l| l.label == label).map_or(DEFAULT_LANG, |l| l.key.as_str())
}

/// Rôle du serveur nommé comme le tag de l'alliance saisie (sans tenir compte de la casse).
fn alliance_role(ctx: &serenity::Context, data: &Data, guild: GuildId, input: &str) -> Option<RoleId> {
    let Some(alliance) = data.config.alliance(input) else {
        warn!(%guild, alliance = input, "unknown alliance, no alliance role given");
        return None;
    };
    let role = ctx.cache.guild(guild).and_then(|g| {
        g.roles.values().find(|r| r.name.eq_ignore_ascii_case(&alliance.tag)).map(|r| r.id)
    });
    if role.is_none() {
        warn!(%guild, tag = alliance.tag, "no role named like the alliance tag");
    }
    role
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
fn ticket_owner(
    ctx: &serenity::Context,
    guild: GuildId,
    v: &Verification,
    channel_id: ChannelId,
) -> Option<UserId> {
    let guild = ctx.cache.guild(guild)?;
    let channel = guild.channels.get(&channel_id)?;
    if channel.parent_id != Some(v.category) {
        return None;
    }
    let id = channel.topic.as_deref()?.strip_prefix("uid:")?.parse::<u64>().ok()?;
    Some(UserId::new(id))
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
