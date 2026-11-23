-- Panneaux de rôles en libre-service (/roles) : un message avec un bouton par rôle.
CREATE TABLE role_panels (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    channel_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL,
    title       TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX role_panels_guild ON role_panels (guild_id);
CREATE UNIQUE INDEX role_panels_message ON role_panels (message_id);

-- Rôles d'un panneau, dans l'ordre d'ajout (rowid).
CREATE TABLE role_panel_entries (
    panel_id INTEGER NOT NULL REFERENCES role_panels (id) ON DELETE CASCADE,
    role_id  INTEGER NOT NULL,
    emoji    TEXT,
    label    TEXT    NOT NULL,
    PRIMARY KEY (panel_id, role_id)
);
