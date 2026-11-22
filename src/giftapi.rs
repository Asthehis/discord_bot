//! Client du site de réclamation des codes cadeaux Kingshot (Century Games).
//!
//! `POST /api/gift_code` avec `fid`, `cdk`, `kid`, `time` et une signature
//! `md5("cdk=…&fid=…&kid=…&time=…" + clé)`. Pas de captcha côté Kingshot.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

const BASE_URL: &str = "https://kingshot-giftcode.centurygame.com";
const REDEEM_URL: &str = "https://kingshot-giftcode.centurygame.com/api/gift_code";

/// Délai minimal entre deux requêtes (toutes requêtes confondues).
const MIN_INTERVAL: Duration = Duration::from_millis(1100);
/// Pause quand le quota est épuisé et que le serveur ne dit pas combien attendre.
const QUOTA_FALLBACK: Duration = Duration::from_secs(61);
/// Pause après un `TOO FREQUENT` (limite par joueur).
const TOO_FREQUENT_BACKOFF: Duration = Duration::from_secs(5);
const MAX_ATTEMPTS: u32 = 5;

/// Pourquoi un code est inutilisable pour tout le monde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidReason {
    NotFound,
    Expired,
    UsageLimit,
}

impl InvalidReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::Expired => "expired",
            Self::UsageLimit => "usage limit reached",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    AlreadyReceived,
    SameType,
    CodeInvalid(InvalidReason),
    LevelTooLow,
    VipTooLow,
    /// `fid` + `kid` ne correspondent à aucun joueur (mauvais kingdom, migration…).
    KingdomMismatch,
    PlayerNotFound,
    /// Échec après les retries, ou réponse inconnue : à retenter plus tard.
    Error(String),
}

impl Outcome {
    /// Valeur stockée dans `redemptions.status`.
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::AlreadyReceived => "RECEIVED",
            Self::SameType => "SAME_TYPE",
            Self::CodeInvalid(_) => "CODE_INVALID",
            Self::LevelTooLow => "LEVEL_TOO_LOW",
            Self::VipTooLow => "VIP_TOO_LOW",
            Self::KingdomMismatch => "KINGDOM_MISMATCH",
            Self::PlayerNotFound => "PLAYER_NOT_FOUND",
            Self::Error(_) => "ERROR",
        }
    }

    /// Le serveur a reconnu le code pour ce joueur : le code existe.
    pub fn proves_code_valid(&self) -> bool {
        matches!(
            self,
            Self::Success | Self::AlreadyReceived | Self::SameType | Self::LevelTooLow | Self::VipTooLow
        )
    }
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    err_code: Option<i64>,
}

enum Attempt {
    Done(Outcome),
    Retry(Duration),
}

pub struct GiftApi {
    http: reqwest::Client,
    key: String,
    /// Prochain instant où une requête peut partir (pacing global).
    next_slot: Mutex<Instant>,
}

impl GiftApi {
    pub fn new(key: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { http, key, next_slot: Mutex::new(Instant::now()) })
    }

    /// Tente de réclamer `code` pour le joueur `fid` du kingdom `kid`.
    /// Gère lui-même le pacing, les quotas et les erreurs transitoires.
    pub async fn redeem(&self, fid: i64, kid: i64, code: &str) -> Outcome {
        let mut last_error = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            self.wait_slot().await;
            match self.try_once(fid, kid, code).await {
                Ok(Attempt::Done(outcome)) => return outcome,
                Ok(Attempt::Retry(wait)) => {
                    debug!(fid, code, attempt, ?wait, "gift api : retry");
                    self.push_slot(wait).await;
                    last_error = "throttled".into();
                }
                Err(e) => {
                    warn!(fid, code, attempt, "gift api : {e}");
                    self.push_slot(Duration::from_secs(5 * attempt as u64)).await;
                    last_error = e.to_string();
                }
            }
        }
        Outcome::Error(last_error)
    }

    /// Vérifie qu'un `fid` existe dans le kingdom `kid`, avec un code qui ne peut pas exister.
    pub async fn probe(&self, fid: i64, kid: i64) -> Outcome {
        let fake = format!("ZZ{:014X}", now_nanos() & 0xFF_FFFF_FFFF_FFFF);
        self.redeem(fid, kid, &fake).await
    }

    async fn try_once(&self, fid: i64, kid: i64, code: &str) -> reqwest::Result<Attempt> {
        let form = self.signed_form(fid, kid, code);
        let resp = self
            .http
            .post(REDEEM_URL)
            .header("Origin", BASE_URL)
            .header("Referer", format!("{BASE_URL}/"))
            .form(&form)
            .send()
            .await?;

        let status = resp.status();
        let quota_wait = quota_wait(resp.headers());
        if status.as_u16() == 429 {
            return Ok(Attempt::Retry(quota_wait.unwrap_or(QUOTA_FALLBACK)));
        }
        if status.is_server_error() {
            return Ok(Attempt::Retry(Duration::from_secs(10)));
        }
        // Quota épuisé par cette requête : on la garde, mais les suivantes attendront.
        if let Some(wait) = quota_wait {
            self.push_slot(wait).await;
        }

        let body: Response = resp.json().await?;
        Ok(classify(&body))
    }

    fn signed_form(&self, fid: i64, kid: i64, code: &str) -> Vec<(&'static str, String)> {
        let time = (now_nanos() / 1_000_000_000).to_string();
        // Clés triées par ordre alphabétique, comme le site.
        let payload = format!("cdk={code}&fid={fid}&kid={kid}&time={time}");
        let sign = md5_hex(&format!("{payload}{}", self.key));
        vec![
            ("sign", sign),
            ("cdk", code.to_string()),
            ("fid", fid.to_string()),
            ("kid", kid.to_string()),
            ("time", time),
        ]
    }

    async fn wait_slot(&self) {
        let mut next = self.next_slot.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now() + MIN_INTERVAL;
    }

    async fn push_slot(&self, wait: Duration) {
        let mut next = self.next_slot.lock().await;
        let target = Instant::now() + wait;
        if target > *next {
            *next = target;
        }
    }
}

fn classify(body: &Response) -> Attempt {
    let msg = body.msg.as_deref().unwrap_or("").trim().trim_end_matches('.');
    let outcome = match (msg, body.err_code) {
        ("SUCCESS", _) => Outcome::Success,
        ("RECEIVED", Some(40008)) => Outcome::AlreadyReceived,
        ("SAME TYPE EXCHANGE", Some(40011)) => Outcome::SameType,
        ("TIME ERROR", Some(40007)) => Outcome::CodeInvalid(InvalidReason::Expired),
        ("CDK NOT FOUND", Some(40014)) => Outcome::CodeInvalid(InvalidReason::NotFound),
        ("USED", Some(40005)) => Outcome::CodeInvalid(InvalidReason::UsageLimit),
        ("TIMEOUT RETRY", Some(40004)) => return Attempt::Retry(Duration::from_secs(5)),
        ("TOO FREQUENT", Some(40019)) => return Attempt::Retry(TOO_FREQUENT_BACKOFF),
        ("USER INFO ERROR", Some(40020)) => Outcome::KingdomMismatch,
        (m, Some(40001)) if m.to_lowercase().contains("not exist") => Outcome::PlayerNotFound,
        ("STOVE_LV ERROR", Some(40006)) => Outcome::LevelTooLow,
        ("RECHARGE_MONEY ERROR", Some(40017)) | ("RECHARGE_MONEY_VIP ERROR", Some(40018)) => {
            Outcome::VipTooLow
        }
        (m, code) => Outcome::Error(format!("réponse inconnue : msg={m:?} err_code={code:?}")),
    };
    Attempt::Done(outcome)
}

/// Attente demandée par le serveur si le quota est épuisé.
fn quota_wait(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let get = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<f64>().ok();
    let exhausted = get("X-RateLimit-Remaining") == Some(0.0);
    let explicit = get("Retry-After").or_else(|| get("X-RateLimit-Reset"));
    match (exhausted, explicit) {
        // Certains serveurs mettent un timestamp dans Reset : on plafonne.
        (_, Some(s)) if s > 0.0 && s < 600.0 => Some(Duration::from_secs_f64(s)),
        (true, _) => Some(QUOTA_FALLBACK),
        _ => None,
    }
}

fn md5_hex(input: &str) -> String {
    Md5::digest(input.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn now_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(msg: &str, err_code: Option<i64>) -> Response {
        Response { msg: Some(msg.into()), err_code }
    }

    fn outcome(r: Response) -> Option<Outcome> {
        match classify(&r) {
            Attempt::Done(o) => Some(o),
            Attempt::Retry(_) => None,
        }
    }

    #[test]
    fn md5_matches_reference() {
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn form_is_signed_over_sorted_keys() {
        let api = GiftApi::new("KEY".into()).unwrap();
        let form = api.signed_form(42, 7, "CODE");
        let get = |k: &str| form.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        let expected = md5_hex(&format!("cdk=CODE&fid=42&kid=7&time={}KEY", get("time")));
        assert_eq!(get("sign"), expected);
    }

    #[test]
    fn classification() {
        assert_eq!(outcome(resp("SUCCESS", Some(20000))), Some(Outcome::Success));
        assert_eq!(outcome(resp("RECEIVED.", Some(40008))), Some(Outcome::AlreadyReceived));
        assert_eq!(
            outcome(resp("CDK NOT FOUND.", Some(40014))),
            Some(Outcome::CodeInvalid(InvalidReason::NotFound))
        );
        assert_eq!(
            outcome(resp("TIME ERROR.", Some(40007))),
            Some(Outcome::CodeInvalid(InvalidReason::Expired))
        );
        assert_eq!(outcome(resp("USER INFO ERROR.", Some(40020))), Some(Outcome::KingdomMismatch));
        assert_eq!(outcome(resp("role not exist.", Some(40001))), Some(Outcome::PlayerNotFound));
        assert_eq!(outcome(resp("TOO FREQUENT.", Some(40019))), None);
        assert!(matches!(outcome(resp("WAT", None)), Some(Outcome::Error(_))));
    }

    /// Une vraie requête (faux ID, faux code) pour vérifier que la signature passe.
    /// `cargo test live_probe -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live_probe() {
        let api = GiftApi::new("mN4!pQs6JrYwV9".into()).unwrap();
        let outcome = api.probe(1, 1).await;
        println!("outcome = {outcome:?}");
        assert!(
            matches!(outcome, Outcome::PlayerNotFound | Outcome::KingdomMismatch),
            "signature refusée ou réponse inattendue : {outcome:?}"
        );
    }

    #[test]
    fn code_validity() {
        assert!(Outcome::LevelTooLow.proves_code_valid());
        assert!(!Outcome::KingdomMismatch.proves_code_valid());
        assert!(!Outcome::CodeInvalid(InvalidReason::Expired).proves_code_valid());
    }
}
