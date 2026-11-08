//! Interface web : sert la page de réglages et une petite API JSON.

use crate::config::{Config, Mode};
use crate::delivery;
use crate::server::{self, ServerHandle};
use crate::store::{Level, Shared, Status, lock, now};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX: &str = include_str!("../web/index.html");
const SESSION_HOURS: i64 = 12;

pub struct Ctl {
    pub core: Shared,
    smtp: Mutex<Option<ServerHandle>>,
    sessions: Mutex<HashMap<String, i64>>,
    pub no_auth: bool,
    pub container: bool,
    pub admin_from_env: bool,
}

pub type CtlRef = Arc<Ctl>;

impl Ctl {
    pub fn new(core: Shared, no_auth: bool, container: bool, admin_from_env: bool) -> CtlRef {
        Arc::new(Ctl {
            core,
            smtp: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            no_auth,
            container,
            admin_from_env,
        })
    }

    pub fn start_smtp(&self) -> Result<(), String> {
        let mut g = lock(&self.smtp);
        if g.is_some() {
            return Ok(());
        }
        *g = Some(server::start(self.core.clone())?);
        Ok(())
    }

    pub fn stop_smtp(&self) {
        if let Some(h) = lock(&self.smtp).take() {
            h.stop();
        }
    }

    fn running(&self) -> bool {
        lock(&self.smtp).is_some()
    }
}

pub fn serve(ctl: CtlRef, addr: &str) -> Result<(), String> {
    let server = Server::http(addr).map_err(|e| format!("Interface web : impossible d'écouter sur {addr} : {e}"))?;
    for req in server.incoming_requests() {
        let ctl = ctl.clone();
        std::thread::spawn(move || handle(ctl, req));
    }
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("en-tête valide")
}

fn json_resp(code: u16, v: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(v.to_string())
        .with_status_code(code)
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
        .with_header(header("Cache-Control", "no-store"))
}

fn err(code: u16, msg: impl Into<String>) -> Response<std::io::Cursor<Vec<u8>>> {
    json_resp(code, json!({ "error": msg.into() }))
}

fn cookie_token(req: &Request) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("Cookie"))
        .and_then(|h| {
            h.value
                .as_str()
                .split(';')
                .filter_map(|p| p.trim().strip_prefix("relais_session="))
                .next()
                .map(|s| s.to_string())
        })
}

fn handle(ctl: CtlRef, mut req: Request) {
    let method = req.method().clone();
    let url = req.url().split('?').next().unwrap_or("/").to_string();

    // la page elle-même
    if method == Method::Get && (url == "/" || url == "/index.html") {
        let r = Response::from_string(INDEX)
            .with_header(header("Content-Type", "text/html; charset=utf-8"))
            .with_header(header("Cache-Control", "no-store"))
            .with_header(header("X-Frame-Options", "DENY"))
            .with_header(header("Referrer-Policy", "no-referrer"));
        let _ = req.respond(r);
        return;
    }
    if method == Method::Get && url == "/sante" {
        let _ = req.respond(json_resp(200, json!({ "ok": true, "smtp": ctl.running() })));
        return;
    }
    if !url.starts_with("/api/") {
        let _ = req.respond(err(404, "page introuvable"));
        return;
    }

    let mut body = String::new();
    let _ = req.as_reader().take(4 * 1024 * 1024).read_to_string(&mut body);
    let body_json: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    // Les requêtes qui modifient doivent être du JSON (protection contre les envois depuis d'autres sites).
    if method != Method::Get {
        let is_json = req.headers().iter().any(|h| {
            h.field.equiv("Content-Type") && h.value.as_str().starts_with("application/json")
        });
        if !is_json {
            let _ = req.respond(err(415, "requête JSON attendue"));
            return;
        }
    }

    if url == "/api/login" && method == Method::Post {
        let given = body_json["password"].as_str().unwrap_or("");
        let expected = ctl.core.cfg().admin_password;
        if !expected.is_empty() && constant_eq(given, &expected) {
            let token = crate::config::random_password(40);
            {
                let mut s = lock(&ctl.sessions);
                let t = now();
                s.retain(|_, exp| *exp > t);
                s.insert(token.clone(), t + SESSION_HOURS * 3600);
            }
            let cookie = format!("relais_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}", SESSION_HOURS * 3600);
            let _ = req.respond(json_resp(200, json!({ "ok": true })).with_header(header("Set-Cookie", &cookie)));
        } else {
            std::thread::sleep(std::time::Duration::from_millis(1200));
            ctl.core.log(Level::Warn, "Mot de passe de l'interface web incorrect");
            let _ = req.respond(err(401, "Mot de passe incorrect"));
        }
        return;
    }

    let authed = ctl.no_auth
        || cookie_token(&req)
            .map(|t| lock(&ctl.sessions).get(&t).is_some_and(|exp| *exp > now()))
            .unwrap_or(false);
    if !authed {
        let _ = req.respond(err(401, "identification requise"));
        return;
    }

    let resp = route(&ctl, &method, &url, &body_json, cookie_token(&req));
    let _ = req.respond(resp);
}

fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn route(ctl: &CtlRef, method: &Method, url: &str, body: &Value, token: Option<String>) -> Response<std::io::Cursor<Vec<u8>>> {
    let core = &ctl.core;
    let parts: Vec<&str> = url.trim_start_matches("/api/").split('/').collect();
    match (method, parts.as_slice()) {
        (Method::Get, ["state"]) => json_resp(200, state_json(ctl)),
        (Method::Post, ["logout"]) => {
            if let Some(t) = token {
                lock(&ctl.sessions).remove(&t);
            }
            json_resp(200, json!({ "ok": true }))
                .with_header(header("Set-Cookie", "relais_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))
        }
        (Method::Put, ["config"]) => save_config(ctl, body),
        (Method::Post, ["server", "start"]) => match ctl.start_smtp() {
            Ok(()) => json_resp(200, json!({ "ok": true })),
            Err(e) => err(400, e),
        },
        (Method::Post, ["server", "stop"]) => {
            ctl.stop_smtp();
            json_resp(200, json!({ "ok": true }))
        }
        (Method::Post, ["relay", "test"]) => {
            let mut cfg = core.cfg();
            if let Ok(draft) = serde_json::from_value::<Config>(body.clone()) {
                cfg.relay = draft.relay;
                cfg.hostname = draft.hostname;
            }
            match delivery::test_connection(&cfg) {
                Ok(t) => json_resp(200, json!({ "ok": true, "message": t })),
                Err(e) => err(400, e),
            }
        }
        (Method::Post, ["test-mail"]) => {
            let to = body["to"].as_str().unwrap_or("");
            match delivery::queue_test_mail(core, to) {
                Ok(id) => json_resp(200, json!({ "ok": true, "id": id })),
                Err(e) => err(400, e),
            }
        }
        (Method::Get, ["messages", id, "raw"]) => {
            let id: u64 = id.parse().unwrap_or(0);
            let raw = std::fs::read(core.body_path(id)).unwrap_or_default();
            let mut text = String::from_utf8_lossy(&raw[..raw.len().min(100_000)]).into_owned();
            if raw.len() > 100_000 {
                text.push_str("\n\n[… suite non affichée …]");
            }
            json_resp(200, json!({ "raw": text }))
        }
        (Method::Post, ["messages", id, "retry"]) => {
            core.retry(id.parse().unwrap_or(0));
            json_resp(200, json!({ "ok": true }))
        }
        (Method::Delete, ["messages", id]) => {
            core.delete(id.parse().unwrap_or(0));
            json_resp(200, json!({ "ok": true }))
        }
        (Method::Post, ["messages", "retry-failed"]) => {
            let ids: Vec<u64> = core.st().messages.iter().filter(|m| m.status == Status::Failed).map(|m| m.id).collect();
            for id in &ids {
                core.retry(*id);
            }
            json_resp(200, json!({ "ok": true, "count": ids.len() }))
        }
        (Method::Post, ["messages", "clear"]) => {
            core.clear_finished();
            json_resp(200, json!({ "ok": true }))
        }
        (Method::Post, ["events", "clear"]) => {
            core.st().events.clear();
            json_resp(200, json!({ "ok": true }))
        }
        (Method::Post, ["password"]) => {
            if ctl.admin_from_env {
                return err(400, "Le mot de passe est fixé par la variable ADMIN_PASSWORD du conteneur.");
            }
            let current = body["current"].as_str().unwrap_or("");
            let new = body["new"].as_str().unwrap_or("");
            let mut cfg = core.cfg();
            if !constant_eq(current, &cfg.admin_password) {
                return err(400, "Mot de passe actuel incorrect");
            }
            if new.chars().count() < 8 {
                return err(400, "Le nouveau mot de passe doit faire au moins 8 caractères");
            }
            cfg.admin_password = new.to_string();
            if let Err(e) = cfg.save() {
                return err(500, e);
            }
            *lock(&core.config) = cfg;
            // les autres sessions sont fermées
            if let Some(t) = token {
                lock(&ctl.sessions).retain(|k, _| *k == t);
            }
            core.log(Level::Info, "Mot de passe de l'interface web modifié");
            json_resp(200, json!({ "ok": true }))
        }
        _ => err(404, "action inconnue"),
    }
}

fn public_config(cfg: &Config) -> Value {
    let mut v = serde_json::to_value(cfg).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.remove("admin_password");
    }
    v
}

fn state_json(ctl: &CtlRef) -> Value {
    let cfg = ctl.core.cfg();
    let st = ctl.core.st();
    let start_of_day = chrono::Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|d| d.and_local_timezone(chrono::Local).single())
        .map(|t| t.timestamp())
        .unwrap_or(0);
    let count = |f: &dyn Fn(Status) -> bool| st.messages.iter().filter(|m| f(m.status)).count();
    let messages: Vec<&crate::store::MessageMeta> = st.messages.iter().rev().take(1000).collect();
    let events: Vec<Value> = st
        .events
        .iter()
        .rev()
        .take(500)
        .map(|e| {
            json!({
                "time": e.time,
                "level": match e.level { Level::Info => "info", Level::Ok => "ok", Level::Warn => "warn", Level::Error => "error" },
                "text": e.text,
            })
        })
        .collect();
    json!({
        "server": {
            "running": st.server.running,
            "error": st.server.error,
            "started_at": st.server.started_at,
            "connections": st.server.connections,
        },
        "stats": {
            "today": st.messages.iter().filter(|m| m.received >= start_of_day).count(),
            "sent": count(&|s| s == Status::Sent),
            "waiting": count(&|s| matches!(s, Status::Queued | Status::Sending)),
            "failed": count(&|s| s == Status::Failed),
        },
        "config": public_config(&cfg),
        "messages": messages,
        "events": events,
        "env": {
            "container": ctl.container,
            "admin_from_env": ctl.admin_from_env,
            "local_ip": local_ip(),
            "data_dir": crate::config::app_dir().display().to_string(),
            "version": env!("CARGO_PKG_VERSION"),
        },
        "presets": crate::config::PRESETS.iter().map(|(n, h, p, s, hint)| json!({"name": n, "host": h, "port": p, "security": s, "hint": hint})).collect::<Vec<_>>(),
        "now": now(),
    })
}

fn save_config(ctl: &CtlRef, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut d: Config = match serde_json::from_value(body.clone()) {
        Ok(c) => c,
        Err(e) => return err(400, format!("réglages illisibles : {e}")),
    };
    let old = ctl.core.cfg();
    d.admin_password = old.admin_password.clone();
    d.web_port = old.web_port;
    if ctl.container {
        d.listen_all = true;
    }
    if d.port == 0 {
        return err(400, "Le port doit être supérieur à 0.");
    }
    d.trusted = d.trusted.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    for a in d.accounts.iter_mut() {
        a.username = a.username.trim().to_string();
    }
    let mut names: Vec<&str> = d.accounts.iter().map(|a| a.username.as_str()).collect();
    if names.iter().any(|n| n.is_empty()) {
        return err(400, "Chaque compte doit avoir un identifiant.");
    }
    names.sort();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return err(400, "Deux comptes ont le même identifiant.");
    }
    d.relay.host = d.relay.host.trim().to_string();
    d.relay.force_from = d.relay.force_from.trim().to_string();
    if let Err(e) = crate::config::parse_sender(&d.relay.force_from) {
        return err(400, format!("Onglet « Envoi », expéditeur imposé : {e}."));
    }
    for a in d.accounts.iter_mut() {
        a.from = a.from.trim().to_string();
        if let Err(e) = crate::config::parse_sender(&a.from) {
            return err(400, format!("Compte « {} », expéditeur : {e}.", a.username));
        }
    }
    let mut on_disk = d.clone();
    if ctl.admin_from_env {
        // le mot de passe venant de ADMIN_PASSWORD n'est jamais écrit dans le fichier
        on_disk.admin_password = crate::config::disk_admin_password();
    }
    if let Err(e) = on_disk.save() {
        return err(500, format!("Enregistrement impossible : {e}"));
    }
    *lock(&ctl.core.config) = d.clone();
    let mut restarted = false;
    if ctl.running() && d.listen_changed(&old) {
        ctl.stop_smtp();
        restarted = true;
        if let Err(e) = ctl.start_smtp() {
            return err(400, format!("Réglages enregistrés, mais le serveur n'a pas redémarré : {e}"));
        }
    }
    if d.mode != old.mode && d.mode == Mode::Relay {
        ctl.core.log(Level::Info, "Mode normal : les e-mails reçus seront envoyés");
    }
    ctl.core.log(Level::Info, "Réglages enregistrés");
    json_resp(200, json!({ "ok": true, "restarted": restarted }))
}

/// L'adresse IP de cette machine sur le réseau (aucun paquet n'est envoyé).
pub fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("192.0.2.1:9")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}
