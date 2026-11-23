use poise::serenity_prelude::{AutocompleteChoice, CreateAutocompleteResponse};

use crate::{Context, Error, db, gift::Job, giftapi::Outcome};

/// Commandes enregistrées sur les serveurs qui ont un salon des codes.
pub const COMMANDS: [&str; 2] = ["register", "unregister"];

/// Link your in-game Governor ID to unlock auto-claimed gift codes.
#[poise::command(slash_command, guild_only)]
pub async fn register(
    ctx: Context<'_>,
    #[description = "Your Governor ID"]
    #[min = 1]
    id: u64,
    #[description = "Your alliance"]
    #[autocomplete = "autocomplete_alliance"]
    alliance: String,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.gift;
    let user_id = ctx.author().id.get();
    let fid = id as i64;
    let fill = |s: &str| s.replace("{id}", &id.to_string());

    // Commande enregistrée seulement là où il y a un salon des codes.
    let Some(guild) = ctx.guild_id().filter(|&g| data.guilds.get(g).gift_channel.is_some()) else {
        return Ok(());
    };
    let Some(alliance) = data.config.alliance(&alliance) else {
        return reply(ctx, &t.register_bad_alliance).await;
    };
    let kingdom = alliance.kingdom;
    if db::player_owner(&data.db, fid).await?.is_some_and(|owner| owner != user_id) {
        return reply(ctx, &fill(&t.register_taken)).await;
    }

    // La vérification auprès du jeu prend quelques secondes.
    ctx.defer_ephemeral().await?;
    match data.gift_api.probe(fid, kingdom).await {
        Outcome::KingdomMismatch | Outcome::PlayerNotFound => {
            let msg = fill(&t.register_not_found).replace("{kingdom}", &kingdom.to_string());
            return reply(ctx, &msg).await;
        }
        Outcome::Error(e) => {
            tracing::warn!(fid, "/register probe: {e}");
            return reply(ctx, &t.register_api_down).await;
        }
        // Toute autre réponse (en pratique « CDK NOT FOUND ») : le joueur existe.
        _ => {}
    }

    db::upsert_player(&data.db, fid, user_id, kingdom, &alliance.tag, Some(guild.get())).await?;
    let _ = data.jobs.send(Job::Player { fid });
    tracing::info!(fid, user = user_id, kingdom, alliance = alliance.tag, %guild, "player registered");

    let mut msg = fill(&t.register_ok)
        .replace("{kingdom}", &kingdom.to_string())
        .replace("{alliance}", &alliance.name);
    let linked = db::players_of(&data.db, user_id).await?;
    if linked.len() > 1 {
        msg.push_str("\n\n");
        msg.push_str(&t.register_list.replace("{list}", &format_players(&linked)));
    }
    reply(ctx, &msg).await
}

/// Unlink one of your Governor IDs.
#[poise::command(slash_command, guild_only)]
pub async fn unregister(
    ctx: Context<'_>,
    #[description = "The Governor ID to unlink"]
    #[autocomplete = "autocomplete_my_ids"]
    id: u64,
) -> Result<(), Error> {
    let data = ctx.data();
    let t = &data.texts.gift;
    let msg = if db::delete_player(&data.db, id as i64, ctx.author().id.get()).await? {
        tracing::info!(fid = id, user = %ctx.author().id, "player unregistered");
        &t.unregister_ok
    } else {
        &t.unregister_not_yours
    };
    reply(ctx, &msg.replace("{id}", &id.to_string())).await
}

async fn autocomplete_alliance(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let partial = partial.to_lowercase();
    let choices = ctx
        .data()
        .config
        .alliances
        .iter()
        .filter(|a| {
            a.tag.to_lowercase().contains(&partial) || a.name.to_lowercase().contains(&partial)
        })
        .take(25)
        .map(|a| AutocompleteChoice::new(format!("[{}] {}", a.tag, a.name), a.tag.clone()))
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}

async fn autocomplete_my_ids(ctx: Context<'_>, _partial: &str) -> CreateAutocompleteResponse {
    let players = db::players_of(&ctx.data().db, ctx.author().id.get()).await.unwrap_or_default();
    let choices = players
        .into_iter()
        .take(25)
        .map(|p| {
            let label = format!("{} (kingdom {}, {})", p.fid, p.kingdom, p.alliance);
            AutocompleteChoice::new(label, p.fid)
        })
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}

fn format_players(players: &[db::Player]) -> String {
    players
        .iter()
        .map(|p| format!("• `{}` — kingdom {}, {}", p.fid, p.kingdom, p.alliance))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn reply(ctx: Context<'_>, content: &str) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().content(content).ephemeral(true)).await?;
    Ok(())
}
