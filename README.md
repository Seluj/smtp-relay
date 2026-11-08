# Relais SMTP

Un petit serveur d'e-mails, avec une interface web, qui sert d'intermédiaire entre vos applications
auto-hébergées (Dolibarr, Immich, Nextcloud…) et votre fournisseur de messagerie (OVH, Gmail, Orange,
Free, Outlook, Brevo…).

## À quoi ça sert

Chaque application qui envoie des e-mails (factures Dolibarr, invitations et alertes Immich,
réinitialisations de mot de passe…) demande un serveur SMTP. Sans relais, il faut renseigner dans chacune
le serveur du fournisseur, son mot de passe, son chiffrement et ses règles d'expéditeur. Il faut ensuite
tout refaire dans chaque application au moindre changement.

Avec le relais :

```
Dolibarr  ──┐
Immich    ──┼──► Relais SMTP ──────────────► Fournisseur ──► destinataires
Nextcloud ──┘    (file d'envoi,             (OVH, Gmail,
                  historique)                Brevo…)
   SMTP port 2525, réseau local      SMTP chiffré, port 587 ou 465
```

- **Un seul endroit pour les réglages du fournisseur.** Les applications ne connaissent que le relais.
  Pour changer de fournisseur ou de mot de passe, on ne modifie que le relais.
- **Un compte par application.** Chaque application a son identifiant et son mot de passe. On sait qui
  envoie quoi, et on peut couper un compte sans toucher aux autres.
- **Expéditeur maîtrisé.** Beaucoup de fournisseurs n'acceptent que vos propres adresses en expéditeur.
  Le relais peut imposer le champ « De », pour toutes les applications ou compte par compte. Il garde le
  nom affiché de l'application et met l'adresse d'origine en « Répondre à ».
- **Aucun e-mail perdu.** Si le fournisseur est indisponible, les messages restent dans une file et
  sont renvoyés plus tard. Le nombre d'essais et l'intervalle sont réglables.
- **Historique et journal.** On voit chaque message reçu, son statut (envoyé, en attente, échec) et la
  raison d'un échec.
- **Mode test.** Les e-mails sont gardés sans être envoyés, pour vérifier qu'une application se connecte
  bien.

## Ce qu'il faut

- Un serveur (ou un NAS, un Raspberry Pi…) avec **Docker**, sur le même réseau que vos applications.
- Les **identifiants SMTP de votre fournisseur** : serveur, port, identifiant, mot de passe. Pour Gmail,
  il faut un « mot de passe d'application ».

## Mise en place

### 1. Installer le relais

**Avec l'image publiée sur registry.selutech.fr** (recommandé sur un serveur) :

```sh
docker login registry.selutech.fr
```

Créez un fichier `docker-compose.yml` (ou partez de celui de ce dépôt) :

```yaml
services:
  relais-smtp:
    image: registry.selutech.fr/relais-smtp:latest
    container_name: relais-smtp
    restart: unless-stopped
    ports:
      - "2525:2525"   # SMTP : à indiquer dans Dolibarr / Immich
      - "8080:8080"   # interface web
    environment:
      TZ: Europe/Paris
      ADMIN_PASSWORD: "choisissez-un-mot-de-passe"   # facultatif
    volumes:
      - relais-smtp-data:/data
    networks:
      - relais

networks:
  relais:
    name: relais

volumes:
  relais-smtp-data:
```

Puis démarrez le relais :

```sh
docker compose up -d
```

**En construisant l'image à partir de ce dossier :**

```sh
docker compose up -d --build
```

Le `docker-compose.yml` du dépôt utilise `build: .`.

### 2. Ouvrir l'interface web

Ouvrez `http://<adresse-du-serveur>:8080`.

Le mot de passe est celui de `ADMIN_PASSWORD`. Sans cette variable, un mot de passe est généré au
premier démarrage et s'affiche dans les journaux :

```sh
docker compose logs relais-smtp
```

Vous pourrez le changer dans l'onglet « Réception ».

### 3. Configurer l'envoi (onglet « Envoi »)

1. Choisissez votre **fournisseur** dans la liste. Le serveur, le port et la sécurité se remplissent
   seuls. Prenez « Personnalisé » sinon.
2. Saisissez l'**identifiant** et le **mot de passe** fournis par le fournisseur.
3. Facultatif : remplissez **Expéditeur imposé** avec votre adresse, si le fournisseur refuse les
   autres expéditeurs (OVH et Gmail, notamment).
4. **Enregistrez**, cliquez sur **« Tester la connexion au fournisseur »**, puis sur **« Envoyer un
   e-mail de test »** à votre adresse.

### 4. Créer un compte par application (onglet « Comptes »)

| Identifiant | Mot de passe       | Expéditeur (facultatif)              |
|-------------|--------------------|--------------------------------------|
| `dolibarr`  | bouton « Générer » | `Factures <factures@mon-domaine.fr>` |
| `immich`    | bouton « Générer » | `Photos <photos@mon-domaine.fr>`     |

Notez les mots de passe : ce sont eux qu'on saisit dans les applications.

### 5. Relier les applications au relais

**Applications dans Docker sur le même serveur.** Ajoutez-les au réseau `relais` et utilisez l'hôte
`relais-smtp`. Dans le `docker-compose.yml` de l'application (pour Immich, le service `immich-server`) :

```yaml
services:
  immich-server:
    networks: [default, relais]

networks:
  relais:
    external: true
    name: relais
```

Redémarrez ensuite l'application (`docker compose up -d`).

**Applications installées ailleurs.** Utilisez l'adresse IP du serveur du relais et le port `2525`.

### 6. Régler Dolibarr

*Accueil > Configuration > E-mails > Modifier*

| Réglage                                  | Valeur                                  |
|------------------------------------------|-----------------------------------------|
| Méthode d'envoi des e-mails              | SMTP/SMTPS socket library               |
| Nom d'hôte du serveur SMTP               | `relais-smtp` (ou l'IP du serveur)      |
| Port du serveur SMTP                     | `2525`                                  |
| Identifiant SMTP                         | `dolibarr`                              |
| Mot de passe SMTP                        | celui du compte `dolibarr`              |
| Utiliser le chiffrement TLS (SSL)        | **Non**                                 |
| Utiliser le chiffrement TLS (STARTTLS)   | **Non**                                 |
| E-mail de l'expéditeur par défaut        | votre adresse, ex. `factures@mon-domaine.fr` |

Enregistrez, puis cliquez sur « Envoyer un e-mail de test » en haut de la page.

### 7. Régler Immich

*Administration > Paramètres > Paramètres de notification > E-mail*

| Réglage                         | Valeur                                  |
|---------------------------------|-----------------------------------------|
| Activer                         | oui                                     |
| Hôte                            | `relais-smtp` (ou l'IP du serveur)      |
| Port                            | `2525`                                  |
| Nom d'utilisateur               | `immich`                                |
| Mot de passe                    | celui du compte `immich`                |
| SSL/TLS (si l'option existe)    | **désactivé**                           |
| Adresse d'expédition            | `Immich <photos@mon-domaine.fr>`        |

Cliquez sur « Envoyer un e-mail de test ». Chaque utilisateur doit avoir une adresse e-mail dans son profil
pour recevoir les notifications.

Équivalent dans un fichier de configuration Immich (`IMMICH_CONFIG_FILE`). Le champ `secure` n'existe que
dans les versions récentes :

```json
"notifications": {
  "smtp": {
    "enabled": true,
    "from": "Immich <photos@mon-domaine.fr>",
    "replyTo": "",
    "transport": {
      "host": "relais-smtp",
      "port": 2525,
      "username": "immich",
      "password": "<mot de passe du compte immich>",
      "ignoreCert": false,
      "secure": false
    }
  }
}
```

### 8. Vérifier

Les e-mails de test apparaissent dans l'onglet **« Messages »** avec leur statut. L'onglet **« Journal »**
montre les connexions, les identifications et les envois.

Autres applications (Nextcloud, Gitea, Uptime Kuma…) : même principe. Hôte du relais, port `2525`, sans
chiffrement, identifiant et mot de passe d'un compte. L'onglet « Aide » de l'interface rappelle ces étapes
avec vos propres valeurs.

## Choisir l'expéditeur (« De »)

| Réglage                                            | Portée                     |
|----------------------------------------------------|----------------------------|
| Onglet « Comptes », colonne *Expéditeur*           | un seul compte (prioritaire) |
| Onglet « Envoi », *Expéditeur imposé*              | tous les autres comptes    |
| Rien de renseigné                                  | l'adresse de l'application est gardée |

Formats acceptés : `adresse@exemple.fr` ou `Nom affiché <adresse@exemple.fr>` (accents permis). Sans nom
affiché, celui de l'application est conservé (« Dolibarr », « Immich Photo Server »…). L'adresse d'origine
devient le « Répondre à », sauf si l'application en indique déjà un.

## Dépannage

| Symptôme                                                  | Cause probable / solution |
|-----------------------------------------------------------|---------------------------|
| « 454 TLS non disponible » ou erreur de chiffrement       | SSL/TLS ou STARTTLS activé dans l'application : désactivez-les |
| « 530 Identification requise »                            | l'application n'envoie pas d'identifiant : renseignez-en un, ou ajoutez son IP aux « Adresses de confiance » (onglet « Réception ») |
| « 535 Identifiant ou mot de passe incorrect »             | mauvais mot de passe, ou compte décoché dans « Comptes » |
| Connexion refusée ou délai dépassé                        | application hors du réseau `relais`, mauvais hôte ou port, pare-feu |
| Message en « Échec » dans « Messages »                    | refus du fournisseur, lisez l'erreur. Expéditeur refusé : renseignez l'*Expéditeur* du compte ou l'*Expéditeur imposé* |
| Message qui reste « En attente »                          | serveur d'envoi non configuré (onglet « Envoi ») ou fournisseur injoignable : nouvel essai automatique |
| Mot de passe de l'interface perdu                         | définissez `ADMIN_PASSWORD` et redémarrez le conteneur |

## Sécurité

- **N'exposez pas le port SMTP (2525) sur Internet.** Le relais est fait pour un réseau local ou un
  réseau Docker. Les échanges entre applications et relais ne sont pas chiffrés.
- Si l'interface web doit être joignable de l'extérieur, placez-la derrière un proxy HTTPS (Caddy,
  Traefik, Nginx Proxy Manager…) et choisissez un mot de passe solide.
- Les réglages, y compris les mots de passe, sont stockés dans le volume `/data`. Sauvegardez-le et
  protégez-le.

## Référence

### Ports et données

| Élément         | Valeur                                 |
|-----------------|----------------------------------------|
| SMTP            | `2525`                                 |
| Interface web   | `8080`                                 |
| Données         | `/data` dans le conteneur (volume `relais-smtp-data`) : `config.json` et dossier `messages/` |

### Variables d'environnement

| Variable          | Rôle                                                                    | Défaut         |
|-------------------|-------------------------------------------------------------------------|----------------|
| `ADMIN_PASSWORD`  | mot de passe de l'interface web (sinon généré et affiché dans les logs) | —              |
| `SMTP_PORT`       | port SMTP à l'intérieur du conteneur                                    | `2525`         |
| `WEB_PORT`        | port de l'interface web à l'intérieur du conteneur                      | `8080`         |
| `TZ`              | fuseau horaire                                                          | `Europe/Paris` |
| `RELAIS_SMTP_DIR` | dossier des réglages et messages                                        | `/data`        |

### Mettre à jour

Avec l'image du registre :

```sh
docker compose pull && docker compose up -d
```

En construisant depuis ce dossier (`build: .`) :

```sh
docker compose up -d --build
```

Les réglages et l'historique sont gardés dans le volume.

## Publier l'image sur registry.selutech.fr

### Depuis votre poste

```sh
docker login registry.selutech.fr
docker buildx build --platform linux/amd64 -t registry.selutech.fr/relais-smtp:latest --push .
```

`--platform linux/amd64` est indispensable : sans lui, une image construite sur un Mac Apple Silicon ne
tourne pas sur un serveur classique. Ajoutez `,linux/arm64` pour un Raspberry Pi ou un NAS ARM.

### Avec GitHub Actions

Le workflow `.github/workflows/docker.yml` :

1. construit l'image à chaque envoi sur `main`/`master` ou à chaque étiquette `v1.2.3` ;
2. la publie sur `registry.selutech.fr/relais-smtp` (étiquettes `latest`, `sha-xxxxxxx`, version) ;
3. si la variable `DEPLOY_HOST` existe, se connecte en SSH au serveur et y remplace le conteneur
   `relais-smtp` par la nouvelle image (les données du volume `relais-smtp-data` sont gardées).

À régler dans GitHub, *Settings > Secrets and variables > Actions* :

- Secrets obligatoires : `REGISTRY_USERNAME` et `REGISTRY_PASSWORD` (identifiants de registry.selutech.fr).
- Autres secrets : `DEPLOY_SSH_KEY` (clé privée SSH autorisée sur le serveur), `ADMIN_PASSWORD` (facultatif).
- Variables : `DEPLOY_HOST`, `DEPLOY_USER` (doit pouvoir lancer `docker`), et au besoin `IMAGE_NAME`
  (`relais-smtp` par défaut), `DEPLOY_PORT`, `SMTP_PUBLISH`, `WEB_PUBLISH`,
  `PLATFORMS` (`linux/amd64,linux/arm64` pour un Raspberry Pi ou un NAS ARM).

## Développement (sans Docker)

```sh
cargo build --release
./target/release/relais-smtp          # interface sur http://localhost:8080
cargo test                            # tests unitaires
```

`relais-smtp --help` liste les options, dont `--screenshot` pour photographier une page de l'interface.
Hors conteneur, les données sont rangées dans le dossier de configuration de l'utilisateur, sous
`RelaisSMTP`. Sur macOS : `~/Library/Application Support/RelaisSMTP`.
