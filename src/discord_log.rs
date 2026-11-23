//! Copie d'une partie des logs `tracing` vers un salon Discord.
//!
//! Le layer ne fait que pousser des lignes dans une file bornée (jamais bloquant) ;
//! une tâche les regroupe toutes les quelques secondes en messages de ≤ 2000 caractères.

use std::{
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use poise::serenity_prelude::{ChannelId, CreateMessage, Http};
use tokio::sync::mpsc;
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    layer::Context,
};

const QUEUE: usize = 500;
const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// Contenu d'un message, hors délimiteurs du bloc de code.
const MESSAGE_BUDGET: usize = 1900;
const LINE_MAX: usize = 600;

static DROPPED: AtomicU64 = AtomicU64::new(0);

pub type LogReceiver = mpsc::Receiver<String>;

/// Ce qui part sur Discord : INFO+ pour le bot, ERROR pour les dépendances,
/// rien pour ce module (une erreur d'envoi ne doit pas reboucler).
pub fn filter() -> Targets {
    let me = env!("CARGO_CRATE_NAME");
    Targets::new()
        .with_default(LevelFilter::ERROR)
        .with_target(me, LevelFilter::INFO)
        .with_target(module_path!(), LevelFilter::OFF)
}

pub struct DiscordLayer {
    tx: mpsc::Sender<String>,
}

pub fn layer() -> (DiscordLayer, LogReceiver) {
    let (tx, rx) = mpsc::channel(QUEUE);
    (DiscordLayer { tx }, rx)
}

impl<S: Subscriber> tracing_subscriber::Layer<S> for DiscordLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = LineVisitor::default();
        event.record(&mut v);
        let line = format_line(*event.metadata().level(), &v.message, &v.fields);
        // File pleine : on compte, sans jamais bloquer. Récepteur absent (salon
        // non configuré) : on ignore.
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(line) {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: String,
}

impl Visit for LineVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

fn format_line(level: Level, message: &str, fields: &str) -> String {
    let icon = match level {
        Level::ERROR => "❌",
        Level::WARN => "⚠️",
        _ => "ℹ️",
    };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() % 86_400;
    let mut line = format!(
        "{icon} {:02}:{:02}:{:02} {message}{fields}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    // Pas de fin de bloc de code prématurée, pas de ligne géante.
    line = line.replace("```", "'''");
    if line.chars().count() > LINE_MAX {
        line = line.chars().take(LINE_MAX).collect::<String>() + "…";
    }
    line
}

/// Découpe les lignes en messages tenant dans `MESSAGE_BUDGET`.
fn pack(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in lines {
        if !cur.is_empty() && cur.len() + line.len() + 1 > MESSAGE_BUDGET {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn spawn_sender(http: Arc<Http>, channel: ChannelId, mut rx: LogReceiver) {
    tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Some(first) = rx.recv().await {
            buf.push(first);
            tokio::time::sleep(FLUSH_EVERY).await;
            while let Ok(line) = rx.try_recv() {
                buf.push(line);
            }
            let dropped = DROPPED.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                buf.push(format!("⚠️ {dropped} ligne(s) de log perdue(s) (file pleine)"));
            }
            for chunk in pack(&buf) {
                let msg = CreateMessage::new().content(format!("```\n{chunk}\n```"));
                if let Err(e) = channel.send_message(&*http, msg).await {
                    // Filtré par `filter()` : ne repart pas vers Discord.
                    tracing::warn!("sending logs to Discord: {e}");
                }
            }
            buf.clear();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format() {
        let l = format_line(Level::WARN, "gift api : timeout", " fid=42");
        assert!(l.starts_with("⚠️ "), "{l}");
        assert!(l.ends_with("gift api : timeout fid=42"), "{l}");
        assert!(!format_line(Level::INFO, "a ``` b", "").contains("```"));
        assert!(format_line(Level::INFO, &"x".repeat(5000), "").chars().count() <= LINE_MAX + 1);
    }

    #[test]
    fn packing() {
        let lines: Vec<String> = (0..100).map(|i| format!("{i:03} {}", "y".repeat(90))).collect();
        let packed = pack(&lines);
        assert!(packed.len() > 1);
        assert!(packed.iter().all(|m| m.len() <= MESSAGE_BUDGET));
        assert_eq!(packed.join("\n").lines().count(), 100);
    }

    #[test]
    fn filter_levels() {
        let me = env!("CARGO_CRATE_NAME");
        let f = filter();
        assert!(f.would_enable(&format!("{me}::gift"), &Level::INFO));
        assert!(!f.would_enable(&format!("{me}::gift"), &Level::DEBUG));
        assert!(!f.would_enable("serenity::gateway", &Level::WARN));
        assert!(f.would_enable("serenity::gateway", &Level::ERROR));
        assert!(!f.would_enable(module_path!().trim_end_matches("::tests"), &Level::ERROR));
    }
}
