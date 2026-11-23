//! Rappels d'events du jeu : `/event add|list|skip|remove` et tâche d'envoi.
//!
//! Un event = une première occurrence (UTC) + une période. Le rappel part `lead_secs`
//! avant chaque occurrence ; `last_notified` évite les doublons (y compris après un
//! redémarrage) et sert aussi à sauter une occurrence.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use poise::serenity_prelude::{
    self as serenity, AutocompleteChoice, ChannelId, CreateAllowedMentions,
    CreateAutocompleteResponse, CreateEmbed, CreateEmbedFooter, CreateMessage, EditMessage,
    GuildId, Mentionable, MessageId, RoleId,
};
use tracing::{error, info, warn};

use crate::{Context, Data, Error, db, texts::EventTexts};

/// Nom de la commande, enregistrée seulement là où `reminders = true`.
pub const COMMAND: &str = "event";

const TICK: Duration = Duration::from_secs(30);
/// Un rappel reste envoyable jusqu'à 2 min après le début de l'event (bot redémarré…).
const GRACE: i64 = 120;
const MIN_INTERVAL: i64 = 3600;
const DEFAULT_LEAD_MINUTES: u32 = 30;

const EMBED_COLOR: u32 = 0x5865F2;

const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;

// --- modèles -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum Template {
    #[name = "Bear Trap"]
    BearTrap,
    #[name = "Viking Vengeance"]
    VikingVengeance,
    #[name = "Swordland Showdown"]
    SwordlandShowdown,
    #[name = "Tri-Alliance Clash"]
    TriAllianceClash,
    #[name = "Fortress Battle"]
    FortressBattle,
    #[name = "Daily Reset"]
    DailyReset,
    #[name = "Custom"]
    Custom,
}

struct Preset {
    name: &'static str,
    emoji: &'static str,
    interval: i64,
    message: &'static str,
}

impl Template {
    fn preset(self) -> Preset {
        let p = |name, emoji, interval, message| Preset { name, emoji, interval, message };
        match self {
            Self::BearTrap => p("Bear Trap", "🐻", 2 * DAY,
                "Get your buffs on and prepare your marches for the hunt!"),
            Self::VikingVengeance => p("Viking Vengeance", "🔥", 4 * WEEK,
                "Come online and join the defense!"),
            Self::SwordlandShowdown => p("Swordland Showdown", "⚔️", 2 * WEEK,
                "Buff up, heal up, recall your marches and get ready to fight!"),
            Self::TriAllianceClash => p("Tri-Alliance Clash", "🔱", 4 * WEEK,
                "Buff up and get ready to fight!"),
            Self::FortressBattle => p("Fortress Battle", "🏯", WEEK,
                "Prepare to rally and fight for the registered Strongholds and Forts."),
            Self::DailyReset => p("Daily Reset", "🔄", DAY,
                "Make sure you have done your dailies and arena battles!"),
            Self::Custom => p("", "📅", 0, ""),
        }
    }
}

// --- calcul des occurrences --------------------------------------------------

/// Dernière occurrence qui commence au plus tard à `t`.
fn occurrence_at_or_before(first: i64, interval: i64, t: i64) -> Option<i64> {
    if t < first {
        None
    } else if interval == 0 {
        Some(first)
    } else {
        Some(first + (t - first) / interval * interval)
    }
}

/// Première occurrence qui commence strictement après `t`.
fn occurrence_after(first: i64, interval: i64, t: i64) -> Option<i64> {
    if t < first {
        Some(first)
    } else if interval == 0 {
        None
    } else {
        Some(first + ((t - first) / interval + 1) * interval)
    }
}

impl db::Event {
    /// Occurrence dont le rappel est dû à `now` et pas encore traité.
    fn due(&self, now: i64) -> Option<i64> {
        occurrence_at_or_before(self.first_start, self.interval_secs, now + self.lead_secs)
            .filter(|&s| s > self.last_notified)
    }

    /// Prochaine occurrence pas encore rappelée ni sautée.
    fn next(&self, now: i64) -> Option<i64> {
        occurrence_after(self.first_start, self.interval_secs, now.max(self.last_notified))
    }

    /// Prochain début affiché dans le tableau : l'occurrence à venir, même déjà rappelée,
    /// sauf si elle a été sautée (marquée alors que son rappel n'était pas encore dû).
    fn board_next(&self, now: i64) -> Option<i64> {
        let s = occurrence_after(self.first_start, self.interval_secs, now)?;
        if s <= self.last_notified && now < s - self.lead_secs {
            occurrence_after(self.first_start, self.interval_secs, s)
        } else {
            Some(s)
        }
    }

    /// Events quotidiens (ou plus fréquents) : pas dans le tableau.
    fn on_board(&self) -> bool {
        self.interval_secs == 0 || self.interval_secs > DAY
    }

    fn channel(&self) -> ChannelId {
        ChannelId::new(self.channel_id as u64)
    }

    fn role(&self) -> Option<RoleId> {
        self.role_id.map(|r| RoleId::new(r as u64))
    }
}

// --- dates (UTC, sans dépendance) ---------------------------------------------

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Jours depuis 1970-01-01 (algorithme de H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parse_hm(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * HOUR + m * MINUTE)
}

/// `YYYY-MM-DD HH:MM` ou `HH:MM` (prochaine fois que cette heure arrive), en UTC.
fn parse_start(input: &str, now: i64) -> Option<i64> {
    let input = input.trim();
    if let Some((date, time)) = input.split_once([' ', 'T']) {
        let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
        let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
        if parts.next().is_some() || !(1..=12).contains(&m) || d < 1 {
            return None;
        }
        let days = days_from_civil(y, m, d);
        // Rejette le 31 février & co (la date ne fait pas l'aller-retour).
        (civil_from_days(days) == (y, m, d)).then_some(())?;
        Some(days * DAY + parse_hm(time.trim())?)
    } else {
        let today = now.div_euclid(DAY) * DAY + parse_hm(input)?;
        Some(if today <= now { today + DAY } else { today })
    }
}

/// `2d`, `1w`, `12h`, `90m` ; `once` = une seule fois (0).
fn parse_every(input: &str) -> Option<i64> {
    let input = input.trim().to_lowercase();
    if matches!(input.as_str(), "once" | "none" | "0") {
        return Some(0);
    }
    let unit = match input.chars().last()? {
        'm' => MINUTE,
        'h' => HOUR,
        'd' => DAY,
        'w' => WEEK,
        _ => return None,
    };
    let n: i64 = input[..input.len() - 1].parse().ok()?;
    n.checked_mul(unit).filter(|&s| s >= MIN_INTERVAL)
}

fn format_every(secs: i64) -> String {
    match secs {
        0 => "once".into(),
        s if s % WEEK == 0 => format!("every {}w", s / WEEK),
        s if s % DAY == 0 => format!("every {}d", s / DAY),
        s if s % HOUR == 0 => format!("every {}h", s / HOUR),
        s => format!("every {}m", s / MINUTE),
    }
}

/// `2026-11-25 19:00 UTC` (autocomplete : pas de balise `<t:…>`).
fn format_utc(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(DAY));
    let secs = ts.rem_euclid(DAY);
    format!("{y}-{m:02}-{d:02} {:02}:{:02} UTC", secs / HOUR, secs % HOUR / MINUTE)
}

// --- commandes ----------------------------------------------------------------

/// Serveur courant, s'il a les rappels activés.
fn reminder_guild(ctx: Context<'_>) -> Option<GuildId> {
    ctx.guild_id().filter(|&g| ctx.data().guilds.get(g).reminders)
}

async fn reply(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().content(content).ephemeral(true)).await?;
    Ok(())
}

/// Rappels des events du jeu.
#[poise::command(
    slash_command,
    rename = "event",
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("add", "list", "skip", "remove", "board"),
    subcommand_required
)]
pub async fn event(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create an event reminder (times in UTC, game time).
#[poise::command(slash_command, guild_only)]
#[allow(clippy::too_many_arguments)] // une option slash = un argument
async fn add(
    ctx: Context<'_>,
    #[rename = "type"]
    #[description = "Game event, or Custom"]
    template: Template,
    #[description = "Next start, UTC: YYYY-MM-DD HH:MM or HH:MM"] start: String,
    #[description = "Repeat: 2d, 1w, 12h… or once (default: the event's usual cycle)"]
    every: Option<String>,
    #[description = "Minutes before the start (default 30)"]
    #[max = 1440]
    before: Option<u32>,
    #[description = "Role to ping"] role: Option<serenity::Role>,
    #[description = "Channel for the reminder (default: this one)"]
    #[channel_types("Text", "News")]
    channel: Option<serenity::GuildChannel>,
    #[description = "Name (required for Custom)"]
    #[max_length = 80]
    name: Option<String>,
    #[description = "Text under the reminder"]
    #[max_length = 500]
    message: Option<String>,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.events;
    let Some(guild) = reminder_guild(ctx) else {
        return reply(ctx, &t.not_enabled).await;
    };
    let preset = template.preset();
    let now = now_secs();

    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let Some(name) = name.or_else(|| (!preset.name.is_empty()).then(|| preset.name.into())) else {
        return reply(ctx, &t.need_name).await;
    };
    let Some(first_start) = parse_start(&start, now) else {
        return reply(ctx, &t.bad_start).await;
    };
    let interval = match every.as_deref().map(parse_every) {
        None => preset.interval,
        Some(Some(i)) => i,
        Some(None) => return reply(ctx, &t.bad_every).await,
    };
    if interval == 0 && first_start <= now {
        return reply(ctx, &t.start_past).await;
    }
    let lead = i64::from(before.unwrap_or(DEFAULT_LEAD_MINUTES)) * MINUTE;
    let channel_id = channel.map_or(ctx.channel_id(), |c| c.id);
    let message = message.unwrap_or_else(|| preset.message.into());

    let id = db::insert_event(
        &data.db,
        &db::NewEvent {
            guild_id: guild.get(),
            channel_id: channel_id.get(),
            role_id: role.as_ref().map(|r| r.id.get()),
            name: &name,
            emoji: preset.emoji,
            message: &message,
            first_start,
            interval_secs: interval,
            lead_secs: lead,
            created_by: ctx.author().id.get(),
        },
    )
    .await?;
    info!(%guild, id, name, user = %ctx.author().id, "reminder created");

    let next = occurrence_after(first_start, interval, now)
        .map_or_else(|| "—".into(), |s| format!("<t:{s}:F> (<t:{s}:R>)"));
    let text = t
        .added
        .replace("{id}", &id.to_string())
        .replace("{emoji}", preset.emoji)
        .replace("{name}", &name)
        .replace("{next}", &next)
        .replace("{every}", &format_every(interval))
        .replace("{before}", &(lead / MINUTE).to_string())
        .replace("{channel}", &format!("<#{channel_id}>"))
        .replace("{ping}", &role.map_or_else(String::new, |r| format!("<@&{}>", r.id)));
    reply(ctx, text).await?;
    refresh_board(ctx.http(), data, guild).await;
    Ok(())
}

/// List this server's event reminders.
#[poise::command(slash_command, guild_only)]
async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.events;
    let Some(guild) = reminder_guild(ctx) else {
        return reply(ctx, &t.not_enabled).await;
    };
    let now = now_secs();
    let mut events = db::events_of(&data.db, guild.get()).await?;
    if events.is_empty() {
        return reply(ctx, &t.list_empty).await;
    }
    events.sort_by_key(|e| e.next(now).unwrap_or(i64::MAX));

    let mut description = String::new();
    for e in &events {
        let next = e.next(now).map_or_else(|| "—".into(), |s| format!("<t:{s}:F> (<t:{s}:R>)"));
        let line = t
            .list_line
            .replace("{id}", &e.id.to_string())
            .replace("{emoji}", &e.emoji)
            .replace("{name}", &e.name)
            .replace("{next}", &next)
            .replace("{every}", &format_every(e.interval_secs))
            .replace("{before}", &(e.lead_secs / MINUTE).to_string())
            .replace("{channel}", &format!("<#{}>", e.channel_id))
            .replace("{ping}", &e.role().map_or_else(String::new, |r| format!("<@&{r}>")));
        // Limite Discord d'une description d'embed.
        if description.len() + line.len() > 4000 {
            description.push('…');
            break;
        }
        description.push_str(line.trim_end());
        description.push('\n');
    }
    let embed = CreateEmbed::new().title(&t.list_title).description(description);
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true)).await?;
    Ok(())
}

/// Skip the next occurrence of a reminder (event cancelled, moved…).
#[poise::command(slash_command, guild_only)]
async fn skip(
    ctx: Context<'_>,
    #[description = "The reminder"]
    #[autocomplete = "autocomplete_event"]
    id: i64,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.events;
    let Some(guild) = reminder_guild(ctx) else {
        return reply(ctx, &t.not_enabled).await;
    };
    let now = now_secs();
    let Some(e) = db::event(&data.db, guild.get(), id).await? else {
        return reply(ctx, &t.not_found).await;
    };
    let Some(skipped) = e.next(now) else {
        return reply(ctx, &t.not_found).await;
    };
    if e.interval_secs == 0 {
        db::delete_event(&data.db, guild.get(), id).await?;
    } else {
        db::set_last_notified(&data.db, id, skipped).await?;
    }
    info!(%guild, id, skipped, user = %ctx.author().id, "occurrence skipped");

    let following = occurrence_after(e.first_start, e.interval_secs, skipped)
        .map_or_else(|| "—".into(), |s| format!("<t:{s}:F> (<t:{s}:R>)"));
    let text = t
        .skipped
        .replace("{name}", &e.name)
        .replace("{skipped}", &format!("<t:{skipped}:F>"))
        .replace("{next}", &following);
    reply(ctx, text).await?;
    refresh_board(ctx.http(), data, guild).await;
    Ok(())
}

/// Delete a reminder.
#[poise::command(slash_command, guild_only)]
async fn remove(
    ctx: Context<'_>,
    #[description = "The reminder"]
    #[autocomplete = "autocomplete_event"]
    id: i64,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.events;
    let Some(guild) = reminder_guild(ctx) else {
        return reply(ctx, &t.not_enabled).await;
    };
    let Some(e) = db::event(&data.db, guild.get(), id).await? else {
        return reply(ctx, &t.not_found).await;
    };
    db::delete_event(&data.db, guild.get(), id).await?;
    info!(%guild, id, name = e.name, user = %ctx.author().id, "reminder deleted");
    let text = t.removed.replace("{id}", &id.to_string()).replace("{name}", &e.name);
    reply(ctx, text).await?;
    refresh_board(ctx.http(), data, guild).await;
    Ok(())
}

/// Post a pinned board of upcoming events, kept up to date (no channel = remove it).
#[poise::command(slash_command, guild_only)]
async fn board(
    ctx: Context<'_>,
    #[description = "Channel for the board (leave empty to remove it)"]
    #[channel_types("Text", "News")]
    channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.events;
    let Some(guild) = reminder_guild(ctx) else {
        return reply(ctx, &t.not_enabled).await;
    };
    ctx.defer_ephemeral().await?;
    let mut boards = BOARDS.lock().await;

    // Un seul tableau par serveur : l'ancien message disparaît.
    if let Some((old_channel, old_message)) = data.guilds.get(guild).board {
        let _ = old_channel.delete_message(ctx.http(), old_message).await;
    }
    let Some(channel) = channel else {
        data.guilds.update(guild, |s| s.board = None).await?;
        boards.remove(&guild);
        info!(%guild, user = %ctx.author().id, "event board removed");
        return reply(ctx, &t.board_removed).await;
    };

    let events = db::events_of(&data.db, guild.get()).await?;
    let (embed, content) = render_board(t, &events, now_secs());
    let (message, pinned) = post_board(ctx.http(), channel.id, embed).await?;
    data.guilds.update(guild, |s| s.board = Some((channel.id, message))).await?;
    boards.insert(guild, content);
    info!(%guild, user = %ctx.author().id, channel = %channel.id, "event board posted");

    let mut text = t.board_set.replace("{channel}", &channel.mention().to_string());
    if !pinned {
        text.push('\n');
        text.push_str(&t.board_pin_failed);
    }
    reply(ctx, text).await
}

async fn autocomplete_event(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let Some(guild) = reminder_guild(ctx) else {
        return CreateAutocompleteResponse::new();
    };
    let now = now_secs();
    let partial = partial.to_lowercase();
    let events = db::events_of(&ctx.data().db, guild.get()).await.unwrap_or_default();
    let choices = events
        .into_iter()
        .filter(|e| e.name.to_lowercase().contains(&partial) || e.id.to_string() == partial)
        .take(25)
        .map(|e| {
            let name: String = e.name.chars().take(50).collect();
            let next = e.next(now).map_or_else(|| "—".into(), format_utc);
            AutocompleteChoice::new(format!("#{} {name} — next {next}", e.id), e.id)
        })
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}

// --- envoi --------------------------------------------------------------------

pub fn spawn_scheduler(http: Arc<serenity::Http>, data: Data) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = run_due(&http, &data).await {
                error!("reminders: {e:#}");
            }
            // Après chaque event, le tableau passe à l'occurrence suivante.
            for guild in data.guilds.guilds_with_board() {
                refresh_board(&http, &data, guild).await;
            }
        }
    });
}

async fn run_due(http: &serenity::Http, data: &Data) -> anyhow::Result<()> {
    let now = now_secs();
    for e in db::all_events(&data.db).await? {
        let Some(start) = e.due(now) else { continue };
        // Marqué avant l'envoi : un salon supprimé ne doit pas spammer les logs.
        if e.interval_secs == 0 {
            db::delete_event(&data.db, e.guild_id as u64, e.id).await?;
        } else {
            db::set_last_notified(&data.db, e.id, start).await?;
        }
        if now > start + GRACE {
            info!(id = e.id, name = e.name, start, "reminder missed (bot offline), skipped");
            continue;
        }
        let msg = CreateMessage::new()
            .content(render_reminder(&data.texts.events, &e, start))
            .allowed_mentions(CreateAllowedMentions::new().roles(e.role()));
        match e.channel().send_message(http, msg).await {
            Ok(_) => info!(id = e.id, name = e.name, "reminder sent"),
            Err(err) => warn!(id = e.id, name = e.name, channel = e.channel_id, "reminder not sent: {err}"),
        }
    }
    Ok(())
}

// --- tableau des events --------------------------------------------------------

/// Dernière description postée par serveur : le message n'est édité que si elle change.
/// Le verrou évite aussi deux reposts simultanés.
static BOARDS: LazyLock<tokio::sync::Mutex<HashMap<GuildId, String>>> =
    LazyLock::new(Default::default);

/// Met à jour le tableau du serveur s'il en a un (reposté s'il a été supprimé).
pub async fn refresh_board(http: &serenity::Http, data: &Data, guild: GuildId) {
    let Some((channel, message)) = data.guilds.get(guild).board else {
        return;
    };
    let mut boards = BOARDS.lock().await;
    let events = match db::events_of(&data.db, guild.get()).await {
        Ok(events) => events,
        Err(e) => return error!(%guild, "event board: {e}"),
    };
    let (embed, content) = render_board(&data.texts.events, &events, now_secs());
    if boards.get(&guild) == Some(&content) {
        return;
    }
    boards.insert(guild, content);

    match channel.edit_message(http, message, EditMessage::new().embed(embed.clone())).await {
        Ok(_) => {}
        Err(e) if is_unknown_message(&e) => match post_board(http, channel, embed).await {
            Ok((message, _)) => {
                if let Err(e) = data.guilds.update(guild, |s| s.board = Some((channel, message))).await {
                    error!(%guild, "saving the event board: {e}");
                }
                info!(%guild, "event board was deleted, posted again");
            }
            Err(e) => warn!(%guild, "posting the event board again: {e}"),
        },
        Err(e) => warn!(%guild, "updating the event board: {e}"),
    }
}

async fn post_board(
    http: &serenity::Http,
    channel: ChannelId,
    embed: CreateEmbed,
) -> Result<(MessageId, bool), Error> {
    let msg = channel.send_message(http, CreateMessage::new().embed(embed)).await?;
    let pinned = match msg.pin(http).await {
        Ok(()) => true,
        Err(e) => {
            warn!(channel = %channel, "pinning the event board: {e}");
            false
        }
    };
    Ok((msg.id, pinned))
}

pub(crate) fn is_unknown_message(e: &serenity::Error) -> bool {
    matches!(
        e,
        serenity::Error::Http(serenity::HttpError::UnsuccessfulRequest(r)) if r.error.code == 10008
    )
}

/// Embed du tableau + sa description (pour savoir s'il a changé).
fn render_board(t: &EventTexts, events: &[db::Event], now: i64) -> (CreateEmbed, String) {
    let mut rows: Vec<(i64, &db::Event)> = events
        .iter()
        .filter(|e| e.on_board())
        .filter_map(|e| Some((e.board_next(now)?, e)))
        .collect();
    rows.sort_by_key(|(start, _)| *start);

    let mut description = String::new();
    for (start, e) in rows {
        let line = t
            .board_line
            .replace("{emoji}", &e.emoji)
            .replace("{name}", &e.name)
            .replace("{time}", &format!("<t:{start}:F>"))
            .replace("{relative}", &format!("<t:{start}:R>"));
        // Limite Discord d'une description d'embed.
        if description.len() + line.len() > 4000 {
            description.push('…');
            break;
        }
        description.push_str(&line);
        description.push('\n');
    }
    if description.is_empty() {
        description = t.board_empty.clone();
    }
    let embed = CreateEmbed::new()
        .title(&t.board_title)
        .description(&description)
        .footer(CreateEmbedFooter::new(&t.board_footer))
        .color(EMBED_COLOR);
    (embed, description)
}

fn render_reminder(t: &EventTexts, e: &db::Event, start: i64) -> String {
    t.reminder
        .replace("{emoji}", &e.emoji)
        .replace("{name}", &e.name)
        .replace("{relative}", &format!("<t:{start}:R>"))
        .replace("{time}", &format!("<t:{start}:t>"))
        .replace("{message}", &e.message)
        .replace("{ping}", &e.role().map_or_else(String::new, |r| format!("<@&{r}>")))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(first: i64, interval: i64, lead: i64, last: i64) -> db::Event {
        db::Event {
            id: 1,
            guild_id: 1,
            channel_id: 1,
            role_id: None,
            name: "Bear Trap".into(),
            emoji: "🐻".into(),
            message: String::new(),
            first_start: first,
            interval_secs: interval,
            lead_secs: lead,
            last_notified: last,
        }
    }

    #[test]
    fn civil_roundtrip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 22), 20_779);
        for z in [-1000, 0, 20_779, 60_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn parses_start() {
        let now = days_from_civil(2026, 11, 22) * DAY + 18 * HOUR; // 18:00
        let today_19 = now + HOUR;
        assert_eq!(parse_start("2026-11-22 19:00", now), Some(today_19));
        assert_eq!(parse_start("2026-11-22T19:00", now), Some(today_19));
        assert_eq!(parse_start("19:00", now), Some(today_19));
        assert_eq!(parse_start("17:00", now), Some(today_19 - 2 * HOUR + DAY));
        assert_eq!(parse_start("2026-02-31 19:00", now), None);
        assert_eq!(parse_start("24:00", now), None);
        assert_eq!(parse_start("tomorrow", now), None);
        assert_eq!(format_utc(today_19), "2026-11-22 19:00 UTC");
    }

    #[test]
    fn parses_every() {
        assert_eq!(parse_every("2d"), Some(2 * DAY));
        assert_eq!(parse_every("1W"), Some(WEEK));
        assert_eq!(parse_every("once"), Some(0));
        assert_eq!(parse_every("30m"), None); // < 1 h
        assert_eq!(parse_every("2x"), None);
        assert_eq!(format_every(2 * DAY), "every 2d");
        assert_eq!(format_every(2 * WEEK), "every 2w");
    }

    #[test]
    fn due_once_per_occurrence() {
        let lead = 30 * MINUTE;
        let mut e = ev(10_000, 2 * DAY, lead, 0);
        assert_eq!(e.due(10_000 - lead - 1), None);
        assert_eq!(e.due(10_000 - lead), Some(10_000));
        e.last_notified = 10_000;
        assert_eq!(e.due(10_000), None);
        assert_eq!(e.next(10_000 - lead), Some(10_000 + 2 * DAY));
        assert_eq!(e.due(10_000 + 2 * DAY - lead), Some(10_000 + 2 * DAY));
    }

    #[test]
    fn skip_moves_next() {
        let mut e = ev(10_000, DAY, 0, 0);
        let skipped = e.next(0).unwrap();
        assert_eq!(skipped, 10_000);
        e.last_notified = skipped;
        assert_eq!(e.due(10_000), None);
        assert_eq!(e.next(0), Some(10_000 + DAY));
    }

    #[test]
    fn board_skips_daily_and_skipped() {
        let t = crate::texts::tests::example_texts().events;
        let lead = 30 * MINUTE;
        let mut bear = ev(10_000, 2 * DAY, lead, 0);
        let mut daily = ev(5_000, DAY, 0, 0);
        daily.name = "Daily Reset".into();
        let (_, text) = render_board(&t, &[bear.clone(), daily], 0);
        assert!(text.contains("Bear Trap") && !text.contains("Daily Reset"), "{text}");
        assert!(text.contains("<t:10000:R>"), "{text}");

        // Déjà rappelée (fenêtre du rappel) : toujours affichée.
        bear.last_notified = 10_000;
        assert_eq!(bear.board_next(10_000 - 10 * MINUTE), Some(10_000));
        // Sautée avant son rappel : on affiche la suivante.
        assert_eq!(bear.board_next(0), Some(10_000 + 2 * DAY));
    }

    #[test]
    fn once_event() {
        let e = ev(10_000, 0, 0, 0);
        assert_eq!(e.due(10_000), Some(10_000));
        assert_eq!(e.next(20_000), None);
    }

    #[test]
    fn renders_example_reminder() {
        let texts = crate::texts::tests::example_texts();
        let mut e = ev(10_000, DAY, 0, 0);
        e.role_id = Some(42);
        let msg = render_reminder(&texts.events, &e, 10_000);
        assert!(msg.contains("<t:10000:R>") && msg.contains("<@&42>"), "{msg}");
    }
}
