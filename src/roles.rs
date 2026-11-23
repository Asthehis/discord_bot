use poise::serenity_prelude::{
    self as serenity, ButtonStyle, ComponentInteraction, ComponentInteractionDataKind,
    CreateActionRow, CreateButton, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
    ReactionType, RoleId,
};

use crate::{Data, Error, settings::GuildSettings};

const PREFIX: &str = "roles:";
const ID_OPEN: &str = "roles:open";
const ID_SELECT: &str = "roles:select";

const EMBED_COLOR: u32 = 0x5865F2;

pub fn handles(custom_id: &str) -> bool {
    custom_id.starts_with(PREFIX)
}

/// Menu des langues, posté par `/setup languages`.
pub fn roles_message(data: &Data) -> CreateMessage {
    let c = &data.texts.common;
    CreateMessage::new()
        .embed(
            CreateEmbed::new()
                .title(&c.roles_title)
                .description(&c.roles_description)
                .color(EMBED_COLOR),
        )
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(ID_OPEN)
                .label(&c.roles_button)
                .emoji(ReactionType::Unicode("🌐".into()))
                .style(ButtonStyle::Primary),
        ])])
}

pub async fn on_component(
    ctx: &serenity::Context,
    data: &Data,
    i: &ComponentInteraction,
) -> Result<(), Error> {
    let (Some(member), Some(guild)) = (i.member.as_ref(), i.guild_id) else {
        return Ok(());
    };
    let settings = data.guilds.get(guild);

    match i.data.custom_id.as_str() {
        // Menu éphémère pré-coché avec les langues actuelles du membre.
        ID_OPEN => {
            let msg = CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .components(vec![language_select(data, &settings, &member.roles)]);
            i.create_response(ctx, CreateInteractionResponse::Message(msg)).await?;
        }
        // La sélection remplace les rôles de langue ; les autres rôles ne bougent pas.
        ID_SELECT => {
            let ComponentInteractionDataKind::StringSelect { values } = &i.data.kind else {
                return Ok(());
            };
            let langs = &settings.languages;
            let (to_add, to_remove): (Vec<RoleId>, Vec<RoleId>) = {
                let wanted = |key: &str| values.iter().any(|v| v == key);
                let has = |r: &RoleId| member.roles.contains(r);
                (
                    langs.iter().filter(|(k, r)| wanted(k) && !has(r)).map(|(_, r)| *r).collect(),
                    langs.iter().filter(|(k, r)| !wanted(k) && has(r)).map(|(_, r)| *r).collect(),
                )
            };

            let c = &data.texts.common;
            let result = async {
                if !to_add.is_empty() {
                    member.add_roles(ctx, &to_add).await?;
                }
                if !to_remove.is_empty() {
                    member.remove_roles(ctx, &to_remove).await?;
                }
                Ok::<_, serenity::Error>(())
            }
            .await;

            let response = match &result {
                Ok(()) => {
                    let selected: Vec<&str> = langs
                        .iter()
                        .filter(|(k, _)| values.contains(k))
                        .filter_map(|(k, _)| data.config.language(k).map(|l| l.label.as_str()))
                        .collect();
                    let content = if selected.is_empty() {
                        c.roles_none.clone()
                    } else {
                        c.roles_updated.replace("{languages}", &selected.join(", "))
                    };
                    let now: Vec<RoleId> = member
                        .roles
                        .iter()
                        .filter(|r| !to_remove.contains(r))
                        .chain(&to_add)
                        .copied()
                        .collect();
                    CreateInteractionResponseMessage::new()
                        .content(content)
                        .components(vec![language_select(data, &settings, &now)])
                }
                Err(_) => CreateInteractionResponseMessage::new().content(&c.action_failed),
            };
            i.create_response(ctx, CreateInteractionResponse::UpdateMessage(response)).await?;
            result?;
        }
        _ => {}
    }
    Ok(())
}

/// Langues du serveur (rôle défini et langue toujours au catalogue), pré-cochées.
fn language_select(data: &Data, settings: &GuildSettings, member_roles: &[RoleId]) -> CreateActionRow {
    let options: Vec<_> = settings
        .languages
        .iter()
        .filter_map(|(key, role)| {
            Some(data.config.language(key)?.select_option(member_roles.contains(role)))
        })
        .collect();
    // <= 25 : une langue par entrée du catalogue, limité par la validation de la config.
    let count = options.len() as u8;
    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(ID_SELECT, CreateSelectMenuKind::String { options })
            .placeholder(&data.texts.common.roles_placeholder)
            .min_values(0)
            .max_values(count.max(1)),
    )
}
