-- Infos saisies pendant la vérification (pré-remplissage de /register).
CREATE TABLE members (
    discord_id  INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    alliance    TEXT    NOT NULL,
    kingdom     INTEGER NOT NULL,
    verified_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Comptes en jeu liés via /register (plusieurs par compte Discord).
CREATE TABLE players (
    fid        INTEGER PRIMARY KEY,
    discord_id INTEGER NOT NULL,
    kingdom    INTEGER NOT NULL,
    alliance   TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX players_discord_id ON players (discord_id);

-- status : pending (pas encore testé) | valid | invalid
CREATE TABLE gift_codes (
    code     TEXT PRIMARY KEY,
    source   TEXT NOT NULL,
    status   TEXT NOT NULL DEFAULT 'pending',
    reason   TEXT,
    added_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Résultat par joueur et par code. status = Outcome::as_db().
CREATE TABLE redemptions (
    fid          INTEGER NOT NULL REFERENCES players (fid) ON DELETE CASCADE,
    code         TEXT    NOT NULL REFERENCES gift_codes (code) ON DELETE CASCADE,
    status       TEXT    NOT NULL,
    attempted_at TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (fid, code)
);
