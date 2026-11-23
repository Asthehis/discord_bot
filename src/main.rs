mod commands;
mod config;
mod db;
mod discord_log;
mod events;
mod gift;
mod giftapi;
mod import;
mod members;
mod register;
mod roles;
mod texts;
mod verification;

use std::sync::Arc;

use poise::serenity_prelude::{self as serenity, FullEvent, GuildId, Interaction};
use tracing::{error, info};
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::{config::Config, giftapi::GiftApi, texts::Texts};

pub struct State {
    pub config: Config,
    pub texts: Texts,
    pub db: db::Pool,
    pub gift_api: GiftApi,
    pub jobs: gift::JobSender,
}

pub type Data = Arc<State>;
pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Terminal : RUST_LOG. Discord : niveaux fixes (voir discord_log::filter).
    let (discord_layer, log_rx) = discord_log::layer();
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,serenity=warn".into());
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
        .with(discord_layer.with_filter(discord_log::filter()))
        .init();

    let config = Config::load()?;
    let logs_channel = config.channels.logs;

    // `discord-verif-bot import …` : import de l'ancienne base, sans Discord.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("import") {
        return import::run(&config, &args[1..]).await;
    }

    let token = std::env::var("DISCORD_TOKEN").map_err(|_| anyhow::anyhow!("DISCORD_TOKEN absent"))?;
    let texts = Texts::load(&config)?;
    let db = db::connect(&config.database_path).await?;
    let gift_api = GiftApi::new(config.gift.encrypt_key.clone())?;
    let (jobs, jobs_rx) = tokio::sync::mpsc::unbounded_channel();
    let data: Data = Arc::new(State { config, texts, db, gift_api, jobs });

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::setup_verification(),
                commands::setup_roles(),
                register::register(),
                register::unregister(),
                events::event(),
            ],
            event_handler: |fw, event| Box::pin(event_handler(fw, event)),
            on_error: |err| Box::pin(on_error(err)),
            ..Default::default()
        })
        .setup(move |ctx, ready, _framework| {
            Box::pin(async move {
                // Sans salon configuré, `log_rx` est abandonné et le layer ne fait plus rien.
                if let Some(channel) = logs_channel {
                    discord_log::spawn_sender(ctx.http.clone(), channel, log_rx);
                }
                info!("connecté en tant que {}", ready.user.name);
                // Appelé une seule fois, au premier Ready.
                gift::spawn_worker(ctx.http.clone(), data.clone(), jobs_rx);
                gift::spawn_share_api_poller(data.clone());
                events::spawn_scheduler(ctx.http.clone(), data.clone());
                Ok(data)
            })
        })
        .build();

    // MESSAGE_CONTENT (privilégié) : lecture des codes dans le salon gift_codes.
    let intents = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_MEMBERS
        | serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;

    // Arrêt propre sur SIGTERM (pod K8s) ou Ctrl-C.
    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        wait_for_shutdown().await;
        info!("signal d'arrêt reçu, déconnexion");
        shard_manager.shutdown_all().await;
    });

    client.start().await?;
    Ok(())
}

async fn event_handler(
    fw: poise::FrameworkContext<'_, Data, Error>,
    event: &FullEvent,
) -> Result<(), Error> {
    let (ctx, data) = (fw.serenity_context, fw.user_data);
    match event {
        // Reçu pour chaque serveur au démarrage et à chaque nouvelle invitation.
        FullEvent::GuildCreate { guild, .. } => {
            register_commands(ctx, data, &fw.options().commands, guild.id).await;
        }
        FullEvent::InteractionCreate { interaction } => match interaction {
            Interaction::Component(i) => {
                if verification::handles(&i.data.custom_id) {
                    verification::on_component(ctx, data, i).await?;
                } else if roles::handles(&i.data.custom_id) {
                    roles::on_component(ctx, data, i).await?;
                }
            }
            Interaction::Modal(i) if verification::handles(&i.data.custom_id) => {
                verification::on_modal(ctx, data, i).await?;
            }
            _ => {}
        },
        // `event` plutôt que `new` : toujours présent, même si le membre n'est pas en cache.
        FullEvent::GuildMemberUpdate { event, .. } => {
            members::on_update(ctx, data, event).await?;
        }
        FullEvent::Message { new_message } => {
            gift::on_message(ctx, data, new_message).await?;
        }
        FullEvent::GuildMemberRemoval { guild_id, user, .. } => {
            members::on_removal(ctx, data, *guild_id, user).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Principal : vérif, rôles, codes. Selon `guilds` : `/event` et/ou `/register`.
async fn register_commands(
    ctx: &serenity::Context,
    data: &Data,
    commands: &[poise::Command<Data, Error>],
    guild: GuildId,
) {
    let cfg = &data.config;
    let main = guild == cfg.guild_id;
    let gift = cfg.gift_channel(guild).is_some();
    let reminders = cfg.reminders_enabled(guild);
    if !main && !gift && !reminders {
        return;
    }
    let list: Vec<_> = commands
        .iter()
        .filter(|c| match c.name.as_ref() {
            events::COMMAND => reminders,
            name if register::COMMANDS.contains(&name) => gift,
            _ => main,
        })
        .filter_map(|c| c.create_as_slash_command())
        .collect();
    match guild.set_commands(&ctx.http, list).await {
        Ok(cmds) => info!(%guild, count = cmds.len(), "commandes enregistrées"),
        Err(e) => error!(%guild, "enregistrement des commandes : {e}"),
    }
}

async fn on_error(err: poise::FrameworkError<'_, Data, Error>) {
    match err {
        poise::FrameworkError::EventHandler { error, event, .. } => {
            error!(event = event.snake_case_name(), "erreur event handler : {error:#}");
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                error!("erreur dans on_error : {e}");
            }
        }
    }
}

async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installation du handler SIGTERM");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
