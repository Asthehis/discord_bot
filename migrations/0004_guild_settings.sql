-- Réglages par serveur Discord (/setup). NULL = non configuré.
CREATE TABLE guild_settings (
    guild_id        INTEGER PRIMARY KEY,
    -- Vérification : active si tous les champs sont renseignés.
    verif_channel   INTEGER,
    verif_category  INTEGER,
    role_verified   INTEGER,
    role_guest      INTEGER,
    role_moderator  INTEGER,
    welcome_channel INTEGER,
    rules_channel   INTEGER,
    -- Menu des rôles de langue.
    roles_channel   INTEGER,
    -- Codes cadeaux : salon de détection + résumés ; active /register.
    gift_channel    INTEGER,
    -- /event.
    reminders       INTEGER NOT NULL DEFAULT 0
);

-- Rôle Discord de chaque langue du catalogue (config.toml), par serveur.
CREATE TABLE guild_languages (
    guild_id INTEGER NOT NULL,
    lang_key TEXT    NOT NULL,
    role_id  INTEGER NOT NULL,
    PRIMARY KEY (guild_id, lang_key)
);
