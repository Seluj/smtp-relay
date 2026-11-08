//! Envoi des messages en file vers le serveur d'envoi du fournisseur (relais).

use crate::config::{Config, Mode, Security};
use crate::store::{Level, Shared, Status, now};
use lettre::Transport;
use lettre::address::{Address, Envelope};
use lettre::transport::smtp::SmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use std::time::Duration;

pub fn build_transport(cfg: &Config) -> Result<SmtpTransport, String> {
    let r = &cfg.relay;
    let host = r.host.trim().to_string();
    if host.is_empty() {
        return Err("aucun serveur d'envoi n'est configuré (onglet « Envoi »)".into());
    }
    let mut b = SmtpTransport::builder_dangerous(&host).port(r.port);
    if r.security != Security::None {
        let params = TlsParameters::builder(host.clone())
            .dangerous_accept_invalid_certs(r.accept_invalid_certs)
            .dangerous_accept_invalid_hostnames(r.accept_invalid_certs)
            .build()
            .map_err(|e| format!("paramètres TLS : {e}"))?;
        b = b.tls(match r.security {
            Security::StartTls => Tls::Required(params),
            _ => Tls::Wrapper(params),
        });
    } else {
        b = b.tls(Tls::None);
    }
    if !r.username.trim().is_empty() {
        b = b.credentials(Credentials::new(r.username.trim().to_string(), r.password.clone()));
    }
    let hello = if cfg.hostname.trim().is_empty() { "relais.local".to_string() } else { cfg.hostname.trim().to_string() };
    Ok(b.timeout(Some(Duration::from_secs(30))).hello_name(ClientId::Domain(hello)).build())
}

pub fn explain(err: &lettre::transport::smtp::Error) -> String {
    let raw = err.to_string();
    let low = raw.to_ascii_lowercase();
    let hint = if low.contains("535") || low.contains("authentication") || low.contains("credentials") {
        " — identifiant ou mot de passe du fournisseur refusé"
    } else if low.contains("connection refused") {
        " — le serveur refuse la connexion (vérifiez le port)"
    } else if low.contains("timed out") || low.contains("timeout") {
        " — pas de réponse (port bloqué ou mauvais serveur ?)"
    } else if low.contains("certificate") {
        " — certificat refusé"
    } else if low.contains("lookup") || low.contains("resolve") || low.contains("dns") || low.contains("nodename") {
        " — nom de serveur introuvable"
    } else if low.contains("554") || low.contains("553") || low.contains("550") || low.contains("sender") {
        " — expéditeur ou destinataire refusé par le fournisseur (essayez « Expéditeur imposé »)"
    } else {
        ""
    };
    format!("{raw}{hint}")
}

pub fn test_connection(cfg: &Config) -> Result<String, String> {
    let t = build_transport(cfg)?;
    match t.test_connection() {
        Ok(true) => Ok(format!("Connexion réussie à {}:{}", cfg.relay.host.trim(), cfg.relay.port)),
        Ok(false) => Err("le serveur n'a pas répondu correctement".into()),
        Err(e) => Err(explain(&e)),
    }
}

/// Fabrique un e-mail de test et le met dans la file, comme s'il venait d'une application.
pub fn queue_test_mail(core: &Shared, to: &str) -> Result<u64, String> {
    use lettre::Message;
    use lettre::message::header::ContentType;
    let cfg = core.cfg();
    let forced = crate::config::parse_sender(&cfg.relay.force_from).map_err(|e| format!("expéditeur imposé : {e}"))?;
    let (name, from) = match forced {
        Some(s) => (s.name.unwrap_or_else(|| "Relais SMTP".into()), s.address),
        None if cfg.relay.username.contains('@') => ("Relais SMTP".into(), cfg.relay.username.trim().to_string()),
        None => ("Relais SMTP".into(), format!("relais@{}", cfg.hostname.trim())),
    };
    let to_addr: Address = to.trim().parse().map_err(|_| format!("adresse invalide : {to}"))?;
    let from_addr: Address = from.parse().map_err(|_| format!("adresse d'expéditeur invalide : {from}"))?;
    let msg = Message::builder()
        .from(lettre::message::Mailbox::new(Some(name), from_addr))
        .to(lettre::message::Mailbox::new(None, to_addr))
        .subject("Test du relais SMTP")
        .header(ContentType::TEXT_PLAIN)
        .body(format!(
            "Bonjour,\n\nCe message de test a été envoyé par votre relais SMTP le {}.\nSi vous le lisez, l'envoi fonctionne.\n",
            chrono::Local::now().format("%d/%m/%Y à %H:%M")
        ))
        .map_err(|e| e.to_string())?;
    core.enqueue(from, vec![to.trim().to_string()], msg.formatted(), "Test depuis l'interface".into(), String::new())
}

/// Boucle d'envoi : tourne en tâche de fond pendant toute la vie du programme.
pub fn spawn_worker(core: Shared) {
    let _ = std::thread::Builder::new().name("smtp-delivery".into()).spawn(move || {
        let mut warned_unconfigured = false;
        loop {
            std::thread::sleep(Duration::from_millis(700));
            let cfg = core.cfg();
            if cfg.mode == Mode::Capture {
                // En mode test, les messages en attente sont simplement gardés.
                let ids: Vec<u64> = core.st().messages.iter().filter(|m| m.status == Status::Queued).map(|m| m.id).collect();
                for id in ids {
                    core.update(id, |m| m.status = Status::Captured);
                }
                continue;
            }
            let next = {
                let t = now();
                core.st()
                    .messages
                    .iter()
                    .find(|m| m.status == Status::Queued && m.next_try <= t)
                    .map(|m| m.clone())
            };
            let Some(msg) = next else { continue };
            if cfg.relay.host.trim().is_empty() {
                if !warned_unconfigured {
                    core.log(Level::Warn, "Des messages attendent : configurez le serveur d'envoi dans l'onglet « Envoi ».");
                    warned_unconfigured = true;
                }
                continue;
            }
            warned_unconfigured = false;
            core.update(msg.id, |m| m.status = Status::Sending);
            let result = send_one(&core, &cfg, &msg);
            match result {
                Ok(()) => {
                    core.update(msg.id, |m| {
                        m.status = Status::Sent;
                        m.attempts += 1;
                        m.sent_at = now();
                        m.last_error.clear();
                    });
                    core.log(Level::Ok, format!("Message n°{} envoyé à {}", msg.id, msg.to.join(", ")));
                }
                Err((text, permanent)) => {
                    let max = cfg.max_attempts.max(1);
                    let retry = cfg.retry_minutes.max(1) as i64 * 60;
                    let mut final_fail = false;
                    core.update(msg.id, |m| {
                        m.attempts += 1;
                        m.last_error = text.clone();
                        if permanent || m.attempts >= max {
                            m.status = Status::Failed;
                            final_fail = true;
                        } else {
                            m.status = Status::Queued;
                            m.next_try = now() + retry;
                        }
                    });
                    if final_fail {
                        core.log(Level::Error, format!("Message n°{} non envoyé : {text}", msg.id));
                    } else {
                        core.log(
                            Level::Warn,
                            format!("Message n°{} : échec temporaire, nouvel essai dans {} min ({text})", msg.id, cfg.retry_minutes.max(1)),
                        );
                    }
                }
            }
        }
    });
}

fn send_one(core: &Shared, cfg: &Config, msg: &crate::store::MessageMeta) -> Result<(), (String, bool)> {
    let (from, to) = (msg.from.as_str(), &msg.to);
    let mut data = std::fs::read(core.body_path(msg.id)).map_err(|e| (format!("contenu introuvable : {e}"), true))?;
    let forced = crate::config::parse_sender(cfg.forced_sender_text(&msg.account)).map_err(|e| (format!("expéditeur imposé : {e}"), true))?;
    let env_from: Option<Address> = if let Some(s) = forced {
        data = crate::mime::rewrite_from(&data, s.name.as_deref(), &s.address);
        Some(s.address.parse().map_err(|_| (format!("expéditeur imposé invalide : {}", s.address), true))?)
    } else if from.is_empty() {
        None
    } else {
        Some(from.parse().map_err(|_| (format!("adresse d'expéditeur invalide : {from}"), true))?)
    };
    let mut rcpts = Vec::new();
    for t in to {
        rcpts.push(t.parse::<Address>().map_err(|_| (format!("destinataire invalide : {t}"), true))?);
    }
    let envelope = Envelope::new(env_from, rcpts).map_err(|e| (e.to_string(), true))?;
    let transport = build_transport(cfg).map_err(|e| (e, false))?;
    transport.send_raw(&envelope, &data).map(|_| ()).map_err(|e| (explain(&e), e.is_permanent()))
}
