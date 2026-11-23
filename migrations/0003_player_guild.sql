-- Serveur Discord depuis lequel le joueur s'est inscrit (résumés par serveur).
-- NULL = serveur principal (inscriptions antérieures, import).
ALTER TABLE players ADD COLUMN guild_id INTEGER;
