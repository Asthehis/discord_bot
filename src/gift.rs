//! Codes cadeaux : détection (salon + API de partage), file de réclamation, résumés.
//!
//! Un seul worker traite les jobs l'un après l'autre : le site limite les requêtes
//! par IP, inutile de paralléliser. Tout est en base, le worker reprend au démarrage.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use poise::serenity_prelude::{
    self as serenity, ChannelId, CreateEmbed, CreateMessage, GuildId, Http, Message,
    MessageFlags, MessageId, ReactionType,
};
use regex::Regex;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::{
    Data, Error, db,
    giftapi::{InvalidReason, Outcome},
};

const SHARE_API_MIN_INTERVAL: u64 = 300;
const SHARE_API_JITTER: u64 = 300;

const EMBED_COLOR: u32 = 0x57F287;

pub enum Job {
    /// Nouveau code : le tester puis le réclamer pour tout le monde.
    Code { code: String, origin: Option<(ChannelId, MessageId)> },
    /// Nouveau joueur : lui réclamer tous les codes encore valides.
    Player { fid: i64 },
}

pub type JobSender = mpsc::UnboundedSender<Job>;
pub type JobReceiver = mpsc::UnboundedReceiver<Job>;

// --- Détection dans le salon ----------------------------------------------

static LONE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{4,32}$").unwrap());
static LABELED_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)code\s*:\s*([A-Za-z0-9]{4,32})\b").unwrap());

/// Codes d'un texte : un mot seul alphanumérique, ou `Code: XXXX`.
fn extract_codes(text: &str) -> Vec<String> {
    let clean: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '_' | '|' | '~') && !c.is_control() || *c == '\n')
        .collect();
    let clean = clean.trim();
    if LONE_CODE.is_match(clean) {
        return vec![clean.to_string()];
    }
    LABELED_CODE.captures_iter(clean).map(|c| c[1].to_string()).collect()
}

fn message_codes(msg: &Message) -> Vec<String> {
    let mut codes = extract_codes(&msg.content);
    for embed in &msg.embeds {
        let texts = [embed.title.as_deref(), embed.description.as_deref()]
            .into_iter()
            .flatten()
            .chain(embed.fields.iter().flat_map(|f| [f.name.as_str(), f.value.as_str()]));
        for t in texts {
            // Dans un embed, seul le format `Code: XXXX` compte (un titre seul serait ambigu).
            codes.extend(LABELED_CODE.captures_iter(t).map(|c| c[1].to_string()));
        }
    }
    codes.sort();
    codes.dedup();
    codes
}

pub async fn on_message(ctx: &serenity::Context, data: &Data, msg: &Message) -> Result<(), Error> {
    let is_gift_channel = msg
        .guild_id
        .and_then(|g| data.guilds.get(g).gift_channel)
        .is_some_and(|c| c == msg.channel_id);
    if !is_gift_channel {
        return Ok(());
    }
    // Les annonces d'un salon suivi arrivent comme des messages de bot : on les garde.
    let crosspost = msg.flags.is_some_and(|f| f.contains(MessageFlags::IS_CROSSPOST));
    if msg.author.bot && !crosspost {
        return Ok(());
    }

    for code in message_codes(msg) {
        if db::insert_code(&data.db, &code, "channel").await? {
            info!(code, "new code found in the channel");
            let _ = data.jobs.send(Job::Code { code, origin: Some((msg.channel_id, msg.id)) });
        } else {
            react(&ctx.http, (msg.channel_id, msg.id), "🔁").await;
        }
    }
    Ok(())
}

// --- API de partage --------------------------------------------------------

#[derive(Deserialize)]
struct ShareResponse {
    #[serde(default)]
    codes: Vec<String>,
}

/// Interroge l'API communautaire toutes les 5 à 10 min (lecture seule).
pub fn spawn_share_api_poller(data: Data) {
    let Some(api) = &data.config.gift.share_api else {
        info!("share API disabled");
        return;
    };
    let (url, key) = (api.url.clone(), api.key.clone());
    tokio::spawn(async move {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        loop {
            match fetch_share_api(&http, &url, &key).await {
                Ok(codes) => {
                    for code in codes {
                        match db::insert_code(&data.db, &code, "share_api").await {
                            Ok(true) => {
                                info!(code, "new code from the share API");
                                let _ = data.jobs.send(Job::Code { code, origin: None });
                            }
                            Ok(false) => {}
                            Err(e) => error!("inserting code {code}: {e}"),
                        }
                    }
                }
                Err(e) => warn!("share API: {e:#}"),
            }
            let jitter = (now_secs() * 7919) % SHARE_API_JITTER;
            tokio::time::sleep(Duration::from_secs(SHARE_API_MIN_INTERVAL + jitter)).await;
        }
    });
}

async fn fetch_share_api(http: &reqwest::Client, url: &str, key: &str) -> anyhow::Result<Vec<String>> {
    let resp = http.get(url).header("X-API-Key", key).send().await?.error_for_status()?;
    let body: ShareResponse = resp.json().await?;
    // Lignes « CODE jj.mm.aaaa ».
    Ok(body
        .codes
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|c| LONE_CODE.is_match(c))
        .map(str::to_string)
        .collect())
}

// --- Worker ----------------------------------------------------------------

pub fn spawn_worker(http: Arc<Http>, data: Data, mut rx: JobReceiver) {
    tokio::spawn(async move {
        // Reprise : codes pas encore testés ou réclamés pour tout le monde.
        match db::active_codes(&data.db).await {
            Ok(codes) => {
                for code in codes {
                    run_code(&http, &data, &code, None, None).await;
                }
            }
            Err(e) => error!("resuming codes: {e}"),
        }

        while let Some(job) = rx.recv().await {
            match job {
                Job::Code { code, origin } => run_code(&http, &data, &code, origin, None).await,
                Job::Player { fid } => match db::active_codes(&data.db).await {
                    Ok(codes) => {
                        for code in codes {
                            run_code(&http, &data, &code, None, Some(fid)).await;
                        }
                    }
                    Err(e) => error!("codes for {fid}: {e}"),
                },
            }
        }
    });
}

#[derive(Default)]
struct Stats {
    success: u32,
    already: u32,
    failed: u32,
}

/// Réclame `code` pour les joueurs restants (ou seulement `only_fid`).
async fn run_code(
    http: &Http,
    data: &Data,
    code: &str,
    origin: Option<(ChannelId, MessageId)>,
    only_fid: Option<i64>,
) {
    if let Err(e) = try_run_code(http, data, code, origin, only_fid).await {
        error!(code, "redeeming: {e:#}");
    }
}

async fn try_run_code(
    http: &Http,
    data: &Data,
    code: &str,
    origin: Option<(ChannelId, MessageId)>,
    only_fid: Option<i64>,
) -> anyhow::Result<()> {
    let pool = &data.db;
    let players = db::players_to_redeem(pool, code, only_fid).await?;
    if players.is_empty() {
        return Ok(());
    }

    let mut known_valid = db::code_status(pool, code).await?.as_deref() == Some("valid");
    // Par serveur d'inscription : chaque salon des codes a son propre résumé.
    let mut stats: HashMap<Option<GuildId>, Stats> = HashMap::new();

    for db::Redeemable { fid, kingdom: kid, guild_id, discord_id, alliance } in players {
        // Joueur sans serveur (import) : compté dans aucun résumé.
        let guild = guild_id.map(|g| GuildId::new(g as u64));
        let outcome = data.gift_api.redeem(fid, kid, code).await;

        if let Outcome::CodeInvalid(reason) = outcome {
            // Un code « valide » qui devient invalide (expiré, épuisé) : on arrête là.
            db::set_code_status(pool, code, "invalid", Some(reason.as_str())).await?;
            info!(code, reason = reason.as_str(), "code not usable");
            if !known_valid {
                announce_invalid(http, data, code, reason, origin).await;
            }
            break;
        }

        if outcome.proves_code_valid() && !known_valid {
            known_valid = true;
            db::set_code_status(pool, code, "valid", None).await?;
            info!(code, "valid code, redeeming for everyone");
            if let Some(o) = origin {
                react(http, o, "✅").await;
            }
        }

        let stats = stats.entry(guild).or_default();
        match &outcome {
            Outcome::Success => stats.success += 1,
            Outcome::AlreadyReceived | Outcome::SameType => stats.already += 1,
            Outcome::Error(e) if e.contains("sign") => {
                // Clé de signature invalide : inutile de continuer.
                error!("signature rejected, check gift.encrypt_key");
                db::record_redemption(pool, fid, code, outcome.as_db()).await?;
                return Ok(());
            }
            _ => {
                stats.failed += 1;
                warn!(
                    code,
                    fid,
                    kingdom = kid,
                    alliance,
                    user = discord_id,
                    status = outcome.as_db(),
                    "redeem failed"
                );
            }
        }
        db::record_redemption(pool, fid, code, outcome.as_db()).await?;
    }

    let total = stats.values().fold(Stats::default(), |t, s| Stats {
        success: t.success + s.success,
        already: t.already + s.already,
        failed: t.failed + s.failed,
    });
    if total.success + total.already + total.failed > 0 {
        info!(
            code,
            redeemed = total.success,
            already = total.already,
            failed = total.failed,
            player = only_fid,
            "code claimed"
        );
    }

    // Résumé uniquement pour une vraie passe sur tout le monde.
    if only_fid.is_none() && known_valid {
        for (guild, channel) in data.guilds.gift_channels() {
            if let Some(s) = stats.get(&Some(guild)).filter(|s| s.success + s.already + s.failed > 0) {
                post_summary(http, data, code, s, channel).await;
            }
        }
    }
    Ok(())
}

async fn announce_invalid(
    http: &Http,
    data: &Data,
    code: &str,
    reason: InvalidReason,
    origin: Option<(ChannelId, MessageId)>,
) {
    // Codes de l'API de partage : souvent anciens, inutile d'en parler.
    let Some(o) = origin else { return };
    react(http, o, "❌").await;
    let text = data
        .texts
        .gift
        .code_invalid
        .replace("{code}", code)
        .replace("{reason}", reason.as_str());
    if let Err(e) = o.0.send_message(http, CreateMessage::new().content(text)).await {
        warn!("announcing invalid code: {e}");
    }
}

async fn post_summary(http: &Http, data: &Data, code: &str, s: &Stats, channel: ChannelId) {
    let t = &data.texts.gift;
    let embed = CreateEmbed::new()
        .title(t.code_summary_title.replace("{code}", code))
        .description(
            t.code_summary
                .replace("{success}", &s.success.to_string())
                .replace("{already}", &s.already.to_string())
                .replace("{failed}", &s.failed.to_string()),
        )
        .color(EMBED_COLOR);
    if let Err(e) = channel.send_message(http, CreateMessage::new().embed(embed)).await {
        warn!("summary for code {code}: {e}");
    }
}

async fn react(http: &Http, (channel, message): (ChannelId, MessageId), emoji: &str) {
    let reaction = ReactionType::Unicode(emoji.to_string());
    if let Err(e) = channel.create_reaction(http, message, reaction).await {
        warn!("reaction {emoji}: {e}");
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_code() {
        assert_eq!(extract_codes("KS2026GIFT"), vec!["KS2026GIFT"]);
        assert_eq!(extract_codes("  **KS2026GIFT**  "), vec!["KS2026GIFT"]);
        assert_eq!(extract_codes("`abc123`"), vec!["abc123"]);
    }

    #[test]
    fn labeled_code() {
        assert_eq!(
            extract_codes("New gift code!\nCode: VIP777 valid until Friday"),
            vec!["VIP777"]
        );
        assert_eq!(extract_codes("code:   **XMAS2026**"), vec!["XMAS2026"]);
    }

    #[test]
    fn chatter_is_ignored() {
        assert!(extract_codes("merci pour le code les gars").is_empty());
        assert!(extract_codes("ok").is_empty());
        assert!(extract_codes("").is_empty());
    }
}
