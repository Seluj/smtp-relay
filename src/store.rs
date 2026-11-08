//! État partagé entre l'interface, le serveur de réception et l'envoi :
//! réglages, messages (métadonnées en mémoire, contenu sur disque) et journal d'activité.

use crate::config::{Config, spool_dir};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Queued,
    Sending,
    Sent,
    Failed,
    Captured,
}

impl Status {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "En attente",
            Status::Sending => "Envoi…",
            Status::Sent => "Envoyé",
            Status::Failed => "Échec",
            Status::Captured => "Gardé (test)",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MessageMeta {
    pub id: u64,
    pub received: i64,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub size: usize,
    pub client: String,
    /// Compte SMTP utilisé par l'application (vide = adresse de confiance ou test).
    #[serde(default)]
    pub account: String,
    pub status: Status,
    pub attempts: u32,
    pub last_error: String,
    pub next_try: i64,
    pub sent_at: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

pub struct Event {
    pub time: i64,
    pub level: Level,
    pub text: String,
}

#[derive(Default)]
pub struct ServerState {
    pub running: bool,
    pub address: String,
    pub error: Option<String>,
    pub started_at: i64,
    pub connections: u64,
}

pub struct State {
    pub messages: Vec<MessageMeta>,
    pub events: VecDeque<Event>,
    pub server: ServerState,
    pub next_id: u64,
}

pub struct Core {
    pub config: Mutex<Config>,
    pub state: Mutex<State>,
    pub spool: PathBuf,
}

pub type Shared = Arc<Core>;

pub fn now() -> i64 {
    chrono::Local::now().timestamp()
}

#[allow(dead_code)]
pub fn fmt_time(ts: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts, 0).single() {
        Some(t) => {
            if t.date_naive() == chrono::Local::now().date_naive() {
                t.format("%H:%M:%S").to_string()
            } else {
                t.format("%d/%m %H:%M").to_string()
            }
        }
        None => "-".into(),
    }
}

impl Core {
    pub fn new(config: Config) -> Shared {
        let spool = spool_dir();
        let _ = std::fs::create_dir_all(&spool);
        let mut messages = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&spool) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("json") {
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        if let Ok(mut m) = serde_json::from_str::<MessageMeta>(&text) {
                            if m.status == Status::Sending {
                                m.status = Status::Queued;
                            }
                            messages.push(m);
                        }
                    }
                }
            }
        }
        messages.sort_by_key(|m| m.id);
        let next_id = messages.last().map(|m| m.id + 1).unwrap_or(1);
        Arc::new(Core {
            config: Mutex::new(config),
            state: Mutex::new(State {
                messages,
                events: VecDeque::new(),
                server: ServerState::default(),
                next_id,
            }),
            spool,
        })
    }

    pub fn cfg(&self) -> Config {
        lock(&self.config).clone()
    }

    pub fn st(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub fn repaint(&self) {}

    pub fn log(&self, level: Level, text: impl Into<String>) {
        let text: String = text.into();
        let tag = match level { Level::Info => "info", Level::Ok => "ok", Level::Warn => "attention", Level::Error => "erreur" };
        println!("{} [{tag}] {text}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        {
            let mut st = self.st();
            st.events.push_back(Event { time: now(), level, text });
            while st.events.len() > 1000 {
                st.events.pop_front();
            }
        }
        self.repaint();
    }

    pub fn body_path(&self, id: u64) -> PathBuf {
        self.spool.join(format!("{id:08}.eml"))
    }

    fn meta_path(&self, id: u64) -> PathBuf {
        self.spool.join(format!("{id:08}.json"))
    }

    pub fn persist(&self, m: &MessageMeta) {
        if let Ok(text) = serde_json::to_string_pretty(m) {
            let _ = std::fs::write(self.meta_path(m.id), text);
        }
    }

    /// Enregistre un nouveau message reçu et le met dans la file d'envoi.
    pub fn enqueue(&self, from: String, to: Vec<String>, data: Vec<u8>, client: String, account: String) -> Result<u64, String> {
        let capture = self.cfg().mode == crate::config::Mode::Capture;
        let subject = crate::mime::subject_of(&data);
        let id = {
            let mut st = self.st();
            let id = st.next_id;
            st.next_id += 1;
            id
        };
        std::fs::write(self.body_path(id), &data).map_err(|e| format!("écriture impossible : {e}"))?;
        let m = MessageMeta {
            id,
            received: now(),
            from,
            to,
            subject,
            size: data.len(),
            client,
            account,
            status: if capture { Status::Captured } else { Status::Queued },
            attempts: 0,
            last_error: String::new(),
            next_try: 0,
            sent_at: 0,
        };
        self.persist(&m);
        {
            let mut st = self.st();
            st.messages.push(m);
        }
        self.trim_history();
        self.repaint();
        Ok(id)
    }

    pub fn update(&self, id: u64, f: impl FnOnce(&mut MessageMeta)) {
        let copy = {
            let mut st = self.st();
            match st.messages.iter_mut().find(|m| m.id == id) {
                Some(m) => {
                    f(m);
                    m.clone()
                }
                None => return,
            }
        };
        self.persist(&copy);
        self.repaint();
    }

    pub fn delete(&self, id: u64) {
        self.st().messages.retain(|m| m.id != id);
        let _ = std::fs::remove_file(self.body_path(id));
        let _ = std::fs::remove_file(self.meta_path(id));
        self.repaint();
    }

    pub fn retry(&self, id: u64) {
        self.update(id, |m| {
            m.status = Status::Queued;
            m.attempts = 0;
            m.next_try = 0;
            m.last_error.clear();
        });
    }

    /// Supprime les plus anciens messages terminés au-delà de la limite d'historique.
    pub fn trim_history(&self) {
        let limit = self.cfg().history_limit.max(10);
        let remove: Vec<u64> = {
            let st = self.st();
            let done: Vec<u64> = st
                .messages
                .iter()
                .filter(|m| matches!(m.status, Status::Sent | Status::Failed | Status::Captured))
                .map(|m| m.id)
                .collect();
            if st.messages.len() > limit {
                let excess = st.messages.len() - limit;
                done.into_iter().take(excess).collect()
            } else {
                Vec::new()
            }
        };
        for id in remove {
            self.delete(id);
        }
    }

    pub fn clear_finished(&self) {
        let ids: Vec<u64> = self
            .st()
            .messages
            .iter()
            .filter(|m| matches!(m.status, Status::Sent | Status::Captured))
            .map(|m| m.id)
            .collect();
        for id in ids {
            self.delete(id);
        }
    }
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}
