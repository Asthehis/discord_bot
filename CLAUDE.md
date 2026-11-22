# discord-verif-bot

Bot Discord en Rust (poise + serenity) pour l'onboarding d'un serveur de jeu.

## Fonctionnement
- `#verification` : message avec un bouton "Commencer la vérification" (posté via `/setup-verification`)
- Clic → modal (pseudo, alliance, royaume) → salon privé `#verif-<pseudo>` (membre + Mod + bot),
  topic `uid:<id>`, embed récap + boutons Verified / Guest / Refuser (Mod only)
- Validation : rôle donné + renommage en `Pseudo` (sans préfixe)
- `guild_member_update` : passage à Verified ou Guest → welcome dans le salon welcome
  (le même pour les deux), puis suppression du salon (sans transcript)
- Refus : suppression du salon uniquement, pas de kick
- `guild_member_removal` : supprime le salon de vérif s'il existe
- `/setup-roles` : bouton → menu éphémère pré-coché des langues (extensible via la config)
- `/register id alliance [kingdom]` : lie un ou plusieurs Governor ID à un compte Discord
  (kingdom par défaut = celui saisi à la vérif), vérifié auprès du jeu ; `/unregister`
- Codes cadeaux : détectés dans `channels.gift_codes` (y compris annonces suivies) et via
  l'API communautaire de partage (lecture seule) → réclamés pour tous les joueurs par un
  worker unique, résumé posté dans le salon
- `/event add|list|skip|remove` (Gérer le serveur) : rappels d'events du jeu (modèles Bear Trap,
  Viking…, ou Custom), heures UTC, récurrence + délai avant ; tâche toutes les 30 s, anti-doublon
  via `last_notified`. Disponible sur `reminder_guilds` (le bot peut être sur plusieurs serveurs ;
  vérif/codes restent sur `guild_id`). Commandes enregistrées par serveur au `GuildCreate`

## Contraintes
- Vérification sans état : custom_ids fixes, lien membre ↔ salon dans le topic du salon
- SQLite (sqlx, migrations dans `migrations/`) pour joueurs, codes, réclamations et infos
  de vérif ; fichier sur un PVC → 1 seul replica
- Intents : GUILDS + GUILD_MEMBERS + GUILD_MESSAGES + MESSAGE_CONTENT (uniquement pour lire
  le salon des codes). Cache serenity activé
- Config en TOML/env (IDs, langues, alliances, clé de signature) ; textes dans `messages.toml`
- Déployé sur la VM Oracle (x86_64) en Docker Compose, 1 seule instance, dans
  `/opt/docker/discord-verif-bot` (token dans `bot.env`, base dans `data/`, uid 10001)
- CI GitHub Actions sur tag `v*` : clippy + tests → image `ghcr.io/<repo>:<version>`
  (config.toml + messages.toml embarqués) → déploiement SSH (`deploy/docker-compose.yml`)

## Façon de travailler
Discuter de l'architecture avant de coder, puis ne donner que les ajouts ou diffs, pas de fichiers complets.
