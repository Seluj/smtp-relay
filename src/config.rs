//! Réglages du relais SMTP, enregistrés en JSON dans le dossier de configuration de l'utilisateur.

use serde::{Deserialize, Serialize};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    /// Aucun chiffrement (port 25 / réseau local).
    None,
    /// STARTTLS (souvent port 587).
    StartTls,
    /// TLS direct (souvent port 465).
    Tls,
}

impl Security {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Security::None => "Aucune",
            Security::StartTls => "STARTTLS",
            Security::Tls => "SSL/TLS",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Les e-mails reçus sont transmis au serveur d'envoi du fournisseur.
    Relay,
    /// Les e-mails sont gardés dans l'historique sans être envoyés (pour tester).
    Capture,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Account {
    pub username: String,
    pub password: String,
    pub enabled: bool,
    /// Expéditeur propre à ce compte (vide = celui de l'onglet « Envoi », ou celui de l'application).
    #[serde(default)]
    pub from: String,
}

/// Un expéditeur imposé : nom affiché facultatif et adresse.
#[derive(Clone, PartialEq, Debug)]
pub struct Sender {
    pub name: Option<String>,
    pub address: String,
}

/// Lit « moi@exemple.fr » ou « Mon nom <moi@exemple.fr> ». Vide = aucun expéditeur imposé.
pub fn parse_sender(text: &str) -> Result<Option<Sender>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let invalid = || format!("« {text} » n'est pas un expéditeur valide : indiquez une adresse (moi@exemple.fr) ou « Nom <moi@exemple.fr> »");
    let mb: lettre::message::Mailbox = text.parse().map_err(|_| invalid())?;
    let address = mb.email.to_string();
    if !address.contains('@') || address.starts_with('@') || address.ends_with('@') {
        return Err(invalid());
    }
    let name = mb.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    Ok(Some(Sender { name, address }))
}

impl Config {
    /// L'expéditeur imposé pour un message reçu via ce compte : celui du compte, sinon celui de l'onglet « Envoi ».
    pub fn forced_sender_text(&self, account: &str) -> &str {
        self.accounts
            .iter()
            .find(|a| !account.is_empty() && a.username == account && !a.from.trim().is_empty())
            .map(|a| a.from.trim())
            .unwrap_or_else(|| self.relay.force_from.trim())
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct RelayConfig {
    pub preset: String,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub password: String,
    /// Adresse d'expéditeur imposée (vide = garder celle de l'application).
    pub force_from: String,
    pub accept_invalid_certs: bool,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            preset: "Personnalisé".into(),
            host: String::new(),
            port: 587,
            security: Security::StartTls,
            username: String::new(),
            password: String::new(),
            force_from: String::new(),
            accept_invalid_certs: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    /// true = accessible depuis les autres machines du réseau (0.0.0.0), false = cet ordinateur seulement.
    pub listen_all: bool,
    pub port: u16,
    /// Nom annoncé par le serveur (EHLO).
    pub hostname: String,
    pub max_size_mb: u32,
    pub require_auth: bool,
    /// Adresses IP (ou débuts d'adresse, ex. « 192.168.1. ») autorisées sans identifiant.
    pub trusted: Vec<String>,
    pub accounts: Vec<Account>,
    pub mode: Mode,
    pub relay: RelayConfig,
    pub max_attempts: u32,
    pub retry_minutes: u32,
    pub autostart: bool,
    pub history_limit: usize,
    /// Mot de passe de l'interface web (vide = généré au premier démarrage).
    pub admin_password: String,
    /// Port de l'interface web.
    pub web_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_all: true,
            port: 2525,
            hostname: "relais.local".into(),
            max_size_mb: 25,
            require_auth: true,
            trusted: vec!["127.0.0.1".into()],
            accounts: vec![Account {
                username: "applications".into(),
                password: random_password(16),
                enabled: true,
                from: String::new(),
            }],
            mode: Mode::Relay,
            relay: RelayConfig::default(),
            max_attempts: 5,
            retry_minutes: 5,
            autostart: true,
            history_limit: 500,
            admin_password: String::new(),
            web_port: 8080,
        }
    }
}

pub fn app_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RELAIS_SMTP_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("RelaisSMTP")
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

pub fn spool_dir() -> PathBuf {
    app_dir().join("messages")
}

impl Config {
    pub fn load() -> Config {
        let path = config_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
                return cfg;
            }
        }
        let cfg = Config::default();
        let _ = cfg.save();
        cfg
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn bind_address(&self) -> String {
        let ip = if self.listen_all { "0.0.0.0" } else { "127.0.0.1" };
        format!("{ip}:{}", self.port)
    }

    /// Les réglages qui demandent de redémarrer l'écoute.
    pub fn listen_changed(&self, other: &Config) -> bool {
        self.listen_all != other.listen_all || self.port != other.port
    }
}

/// Fournisseurs connus : nom, serveur, port, sécurité, conseil.
pub const PRESETS: &[(&str, &str, u16, Security, &str)] = &[
    ("Personnalisé", "", 587, Security::StartTls, "Renseignez les informations données par votre fournisseur."),
    ("Gmail", "smtp.gmail.com", 587, Security::StartTls, "Utilisez un « mot de passe d'application » (Compte Google > Sécurité > Validation en deux étapes)."),
    ("Outlook / Hotmail", "smtp-mail.outlook.com", 587, Security::StartTls, "Votre adresse Outlook et son mot de passe (ou un mot de passe d'application)."),
    ("Microsoft 365", "smtp.office365.com", 587, Security::StartTls, "L'envoi SMTP authentifié doit être activé pour la boîte dans l'administration Microsoft 365."),
    ("OVH (MX Plan / Email Pro)", "ssl0.ovh.net", 465, Security::Tls, "Identifiant = adresse e-mail complète. L'expéditeur doit être cette même adresse."),
    ("Orange", "smtp.orange.fr", 465, Security::Tls, "Identifiant = adresse Orange complète."),
    ("Free", "smtp.free.fr", 465, Security::Tls, "Identifiant = adresse Free complète."),
    ("SFR", "smtp.sfr.fr", 465, Security::Tls, "Identifiant = adresse SFR complète."),
    ("Infomaniak", "mail.infomaniak.com", 465, Security::Tls, "Identifiant = adresse e-mail complète."),
    ("Brevo", "smtp-relay.brevo.com", 587, Security::StartTls, "Identifiant et clé SMTP affichés dans Brevo > SMTP & API."),
    ("Mailjet", "in-v3.mailjet.com", 587, Security::StartTls, "Identifiant = clé API, mot de passe = clé secrète."),
    ("SendGrid", "smtp.sendgrid.net", 587, Security::StartTls, "Identifiant = « apikey », mot de passe = votre clé API."),
    ("Amazon SES (Paris)", "email-smtp.eu-west-3.amazonaws.com", 587, Security::StartTls, "Identifiants SMTP créés dans la console Amazon SES."),
];

pub fn random_password(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut out = String::with_capacity(len);
    let state = std::collections::hash_map::RandomState::new();
    let mut seed = {
        let mut h = state.build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.finish()
    };
    for i in 0..len {
        let mut h = state.build_hasher();
        h.write_u64(seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        seed = h.finish();
        out.push(CHARS[(seed % CHARS.len() as u64) as usize] as char);
    }
    out
}

/// Le mot de passe de l'interface tel qu'il est enregistré dans le fichier (sans les variables d'environnement).
pub fn disk_admin_password() -> String {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .map(|c| c.admin_password)
        .unwrap_or_default()
}
