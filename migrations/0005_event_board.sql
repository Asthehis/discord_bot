-- Tableau des events (/event board) : message épinglé, mis à jour par le bot.
ALTER TABLE guild_settings ADD COLUMN board_channel INTEGER;
ALTER TABLE guild_settings ADD COLUMN board_message INTEGER;
