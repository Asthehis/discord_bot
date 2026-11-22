-- Rappels d'events du jeu, par serveur Discord. Heures en secondes Unix (UTC).
CREATE TABLE events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id      INTEGER NOT NULL,
    channel_id    INTEGER NOT NULL,
    role_id       INTEGER,
    name          TEXT    NOT NULL,
    emoji         TEXT    NOT NULL,
    message       TEXT    NOT NULL,
    -- Première occurrence (début de l'event) et période ; 0 = une seule fois.
    first_start   INTEGER NOT NULL,
    interval_secs INTEGER NOT NULL,
    lead_secs     INTEGER NOT NULL,
    -- Début de la dernière occurrence rappelée ou sautée (anti-doublon).
    last_notified INTEGER NOT NULL DEFAULT 0,
    created_by    INTEGER NOT NULL
);
CREATE INDEX events_guild ON events (guild_id);
