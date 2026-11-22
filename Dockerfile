# --- Build ------------------------------------------------------------------
FROM rust:1.96-slim-trixie AS build
WORKDIR /src

# Dépendances d'abord, avec un main factice : cette couche reste en cache
# tant que Cargo.toml / Cargo.lock ne changent pas.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/deps/discord_verif_bot* target/release/discord-verif-bot*

# Code du bot (les migrations sont embarquées dans le binaire par sqlx::migrate!).
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked

# --- Runtime ----------------------------------------------------------------
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /app bot \
    && mkdir -p /app/data && chown bot /app/data

WORKDIR /app
COPY --from=build /src/target/release/discord-verif-bot /usr/local/bin/discord-verif-bot
# Config et textes versionnés avec le code : un déploiement = une image.
COPY config.toml messages.toml ./

USER bot
# Base SQLite (database_path = "data/bot.db" relatif à /app).
VOLUME /app/data
ENTRYPOINT ["discord-verif-bot"]
