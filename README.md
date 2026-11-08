# Relais SMTP

Un serveur SMTP qui reçoit les e-mails de vos applications (Dolibarr, Immich, Nextcloud…) et les transmet
à votre fournisseur de messagerie (Gmail, OVH, Orange, Free, Outlook, Brevo…). Tout se règle depuis
une interface web.

- **SMTP** : port `2525` (avec identifiant et mot de passe par application)
- **Interface web** : port `8080`
- **Données** (réglages + messages) : dossier `/data` dans le conteneur

## Démarrer avec Docker

```sh
docker compose up -d
docker compose logs relais-smtp     # affiche le mot de passe de l'interface au premier démarrage
```

Puis ouvrez `http://<adresse-du-serveur>:8080`.

Sans compose :

```sh
docker build -t relais-smtp .
docker run -d --name relais-smtp --restart unless-stopped \
  -p 2525:2525 -p 8080:8080 -v relais-smtp-data:/data \
  -e ADMIN_PASSWORD="choisissez-un-mot-de-passe" relais-smtp
```

### Variables d'environnement

| Variable          | Rôle                                                                   | Défaut           |
|-------------------|------------------------------------------------------------------------|------------------|
| `ADMIN_PASSWORD`  | mot de passe de l'interface web (sinon généré et affiché dans les logs) | —                |
| `SMTP_PORT`       | port SMTP à l'intérieur du conteneur                                    | `2525`           |
| `WEB_PORT`        | port de l'interface web à l'intérieur du conteneur                      | `8080`           |
| `TZ`              | fuseau horaire                                                          | `Europe/Paris`   |

## Publier l'image sur registry.selutech.fr

### Depuis votre poste

```sh
docker login registry.selutech.fr
docker buildx build --platform linux/amd64 -t registry.selutech.fr/relais-smtp:latest --push .
```

(`--platform linux/amd64` : sans lui, une image construite sur un Mac Apple Silicon ne tourne pas sur un
serveur classique. Ajoutez `,linux/arm64` pour un Raspberry Pi / NAS ARM.)

### Avec GitHub Actions

Le workflow `.github/workflows/docker.yml` (le dossier `.github` doit être à la racine du dépôt) :

1. construit l'image à chaque envoi sur `main`/`master` ou à chaque étiquette `v1.2.3` ;
2. la publie sur `registry.selutech.fr/relais-smtp` (étiquettes `latest`, `sha-xxxxxxx`, version) ;
3. si la variable `DEPLOY_HOST` existe, se connecte en SSH au serveur et y remplace le conteneur
   `relais-smtp` par la nouvelle image (les données du volume `relais-smtp-data` sont gardées).

À régler dans GitHub, *Settings > Secrets and variables > Actions* :

- Secrets obligatoires : `REGISTRY_USERNAME` et `REGISTRY_PASSWORD` (identifiants de registry.selutech.fr).
- Autres secrets : `DEPLOY_SSH_KEY` (clé privée SSH autorisée sur le serveur), `ADMIN_PASSWORD` (facultatif).
- Variables : `DEPLOY_HOST`, `DEPLOY_USER` (doit pouvoir lancer `docker`), et au besoin `IMAGE_NAME`
  (`relais-smtp` par défaut), `DEPLOY_PORT`, `SMTP_PUBLISH`, `WEB_PUBLISH`,
  `PLATFORMS` (`linux/amd64,linux/arm64` pour un Raspberry Pi / NAS ARM).

Sur le serveur, il faut seulement Docker.

## Brancher vos applications

Dans Dolibarr / Immich : hôte = adresse du serveur (ou `relais-smtp` si l'application est dans le réseau
Docker `relais`), port `2525`, **sans** SSL/TLS, identifiant et mot de passe d'un compte de l'onglet
« Comptes ». L'onglet « Aide » de l'interface donne les étapes exactes.

### Choisir l'expéditeur (« De »)

- Onglet « Envoi », *Expéditeur imposé* : adresse utilisée pour toutes les applications.
- Onglet « Comptes », colonne *Expéditeur* : adresse propre à un compte (ex. `factures@exemple.fr` pour
  Dolibarr, `photos@exemple.fr` pour Immich). Elle passe avant celle de l'onglet « Envoi ».

Les deux acceptent `adresse@exemple.fr` ou `Nom affiché <adresse@exemple.fr>`. Sans nom affiché, celui de
l'application est gardé. L'adresse d'origine de l'application est mise en « Répondre à ».

## Sécurité

- N'exposez pas le port SMTP sur Internet : il est fait pour votre réseau local ou votre serveur.
- Si l'interface web doit être accessible de l'extérieur, placez-la derrière un proxy HTTPS
  (Caddy, Traefik, Nginx Proxy Manager…).

## Sans Docker

```sh
cargo build --release
./target/release/relais-smtp          # interface sur http://localhost:8080
```

`relais-smtp --help` liste les options.
