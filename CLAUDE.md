# discord-verif-bot

Bot Discord en Rust (poise + serenity) pour l'onboarding d'un serveur de jeu.

## Fonctionnement
Multi-serveur : chaque fonctionnalité s'active par serveur avec `/setup` (Administrateur),
réglages en base (`guild_settings`, `guild_languages`, cache mémoire dans `settings::Store`).
Commandes enregistrées par serveur (au `GuildCreate` et après chaque `/setup`) : `/setup`
partout, `/register` + `/unregister` si salon des codes, `/event` si rappels actifs.

- `/setup verification` (salon, catégorie, rôles Verified/Guest/Mod, salons welcome/règles) :
  poste le message avec le bouton de vérification
- Clic → modal (pseudo, alliance, royaume) → salon privé `#verif-<pseudo>` (membre + Mod + bot),
  topic `uid:<id>`, embed récap + boutons Verified / Guest / Refuser (Mod only)
- Validation : rôle donné + renommage en `Pseudo` (sans préfixe)
- `guild_member_update` : passage à Verified ou Guest → welcome dans le salon welcome
  (le même pour les deux), puis suppression du salon (sans transcript)
- Refus : suppression du salon uniquement, pas de kick
- `guild_member_removal` : supprime le salon de vérif s'il existe
- `/setup language` (langue du catalogue → rôle) + `/setup languages` (salon) : bouton → menu
  éphémère pré-coché des langues du serveur
- `/setup gift-codes` : salon des codes du serveur ; `/register id alliance` lie un ou plusieurs
  Governor ID à un compte Discord (kingdom = celui de l'alliance dans la config), vérifié auprès
  du jeu ; `/unregister`
- Codes cadeaux : détectés dans les salons des codes (y compris annonces suivies) et via
  l'API communautaire de partage (lecture seule) → réclamés pour tous les joueurs (pool commun)
  par un worker unique, résumé posté dans chaque salon des codes avec les chiffres des joueurs
  inscrits depuis ce serveur (`players.guild_id`)
- `/setup reminders` puis `/event add|list|skip|remove` (Gérer le serveur) : rappels d'events
  du jeu (modèles Bear Trap, Viking…, ou Custom), heures UTC, récurrence + délai avant ; tâche
  toutes les 30 s, anti-doublon via `last_notified`
- `/setup show|disable|post` : voir, désactiver, reposter les messages

## Contraintes
- Vérification sans état : custom_ids fixes, lien membre ↔ salon dans le topic du salon
- SQLite (sqlx, migrations dans `migrations/`) pour réglages des serveurs, joueurs, codes,
  réclamations, rappels et infos de vérif ; 1 seule instance
- Intents : GUILDS + GUILD_MEMBERS + GUILD_MESSAGES + MESSAGE_CONTENT (uniquement pour lire
  les salons des codes). Cache serenity activé
- `config.toml` = global uniquement (catalogue des langues, alliances, clé de signature, API de
  partage, salon des logs). Les anciennes clés par serveur (`guild_id`, `roles`, `channels`,
  `guilds`, `languages.role_id`) sont reprises en base au démarrage si le serveur n'y est pas
  encore (`settings::import_legacy`), puis à supprimer
- Textes dans `messages.toml` ; logs en anglais
- Déployé sur la VM Oracle (x86_64) en Docker Compose, 1 seule instance, dans
  `/opt/docker/discord-verif-bot` (token dans `bot.env`, base dans `data/`, uid 10001)
- CI GitHub Actions sur tag `v*` : clippy + tests → image `ghcr.io/<repo>:<version>`
  (config.toml + messages.toml embarqués) → déploiement SSH (`deploy/docker-compose.yml`)

## Façon de travailler
Discuter de l'architecture avant de coder, puis ne donner que les ajouts ou diffs, pas de fichiers complets.
