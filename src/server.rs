//! Le serveur SMTP qui reçoit les e-mails des applications (Dolibarr, Immich, …).

use crate::config::Config;
use crate::store::{Level, Shared, lock, now};
use base64::Engine;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct ServerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(core: Shared) -> Result<ServerHandle, String> {
    let cfg = core.cfg();
    let addr = cfg.bind_address();
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("Le port {} est déjà utilisé par un autre programme. Choisissez un autre port dans « Réception ».", cfg.port)
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!("Le port {} demande des droits administrateur. Utilisez un port au-dessus de 1024 (ex. 2525).", cfg.port)
            } else {
                format!("Impossible d'écouter sur {addr} : {e}")
            };
            {
                let mut st = core.st();
                st.server.running = false;
                st.server.error = Some(msg.clone());
            }
            core.log(Level::Error, msg.clone());
            return Err(msg);
        }
    };
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    {
        let mut st = core.st();
        st.server.running = true;
        st.server.error = None;
        st.server.address = addr.clone();
        st.server.started_at = now();
    }
    core.log(Level::Ok, format!("Serveur démarré, à l'écoute sur {addr}"));

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let core2 = core.clone();
    let thread = std::thread::Builder::new()
        .name("smtp-accept".into())
        .spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        let core3 = core2.clone();
                        let stop3 = stop2.clone();
                        let _ = std::thread::Builder::new()
                            .name("smtp-session".into())
                            .spawn(move || {
                                core3.st().server.connections += 1;
                                if let Err(e) = Session::run(core3.clone(), stream, peer, stop3) {
                                    core3.log(Level::Warn, format!("Connexion {} interrompue : {e}", peer.ip()));
                                }
                            });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        core2.log(Level::Warn, format!("Erreur d'acceptation : {e}"));
                        std::thread::sleep(Duration::from_millis(300));
                    }
                }
            }
            {
                let mut st = core2.st();
                st.server.running = false;
            }
            core2.log(Level::Info, "Serveur arrêté");
        })
        .map_err(|e| e.to_string())?;

    Ok(ServerHandle { stop, thread: Some(thread) })
}

struct Session {
    core: Shared,
    cfg: Config,
    peer: SocketAddr,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    helo: Option<String>,
    user: Option<String>,
    from: Option<String>,
    to: Vec<String>,
    auth_failures: u32,
}

const MAX_LINE: usize = 4096;

impl Session {
    fn run(core: Shared, stream: TcpStream, peer: SocketAddr, stop: Arc<AtomicBool>) -> std::io::Result<()> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(300)))?;
        stream.set_write_timeout(Some(Duration::from_secs(60)))?;
        let writer = stream.try_clone()?;
        let cfg = lock(&core.config).clone();
        let mut s = Session {
            core,
            cfg,
            peer,
            reader: BufReader::new(stream),
            writer,
            helo: None,
            user: None,
            from: None,
            to: Vec::new(),
            auth_failures: 0,
        };
        s.core.log(Level::Info, format!("Connexion de {}", peer.ip()));
        s.reply(&format!("220 {} ESMTP Relais SMTP pret", s.cfg.hostname))?;
        loop {
            if stop.load(Ordering::SeqCst) {
                s.reply("421 Service en cours d'arret")?;
                return Ok(());
            }
            let Some(line) = s.read_line()? else { return Ok(()) };
            let (verb, arg) = match line.split_once(' ') {
                Some((v, a)) => (v.to_ascii_uppercase(), a.trim().to_string()),
                None => (line.trim().to_ascii_uppercase(), String::new()),
            };
            match verb.as_str() {
                "EHLO" => {
                    s.helo = Some(arg.clone());
                    s.reset();
                    let max = s.max_bytes();
                    let host = s.cfg.hostname.clone();
                    s.reply(&format!(
                        "250-{host} bonjour {arg}\r\n250-SIZE {max}\r\n250-8BITMIME\r\n250-SMTPUTF8\r\n250-PIPELINING\r\n250-ENHANCEDSTATUSCODES\r\n250-AUTH PLAIN LOGIN\r\n250-AUTH=PLAIN LOGIN\r\n250 HELP"
                    ))?;
                }
                "HELO" => {
                    s.helo = Some(arg.clone());
                    s.reset();
                    let host = s.cfg.hostname.clone();
                    s.reply(&format!("250 {host} bonjour {arg}"))?;
                }
                "AUTH" => s.auth(&arg)?,
                "MAIL" => s.mail(&arg)?,
                "RCPT" => s.rcpt(&arg)?,
                "DATA" => s.data()?,
                "RSET" => {
                    s.reset();
                    s.reply("250 2.0.0 OK")?;
                }
                "NOOP" => s.reply("250 2.0.0 OK")?,
                "HELP" => s.reply("214 2.0.0 Commandes : EHLO HELO AUTH MAIL RCPT DATA RSET NOOP QUIT")?,
                "VRFY" => s.reply("252 2.5.2 Impossible de verifier, mais le message sera tente")?,
                "STARTTLS" => s.reply("454 4.7.0 TLS non disponible sur ce relais, continuez sans chiffrement")?,
                "QUIT" => {
                    s.reply("221 2.0.0 Au revoir")?;
                    return Ok(());
                }
                "" => s.reply("500 5.5.2 Commande vide")?,
                _ => s.reply("502 5.5.2 Commande non reconnue")?,
            }
        }
    }

    fn max_bytes(&self) -> usize {
        (self.cfg.max_size_mb.max(1) as usize) * 1024 * 1024
    }

    fn reset(&mut self) {
        self.from = None;
        self.to.clear();
    }

    fn reply(&mut self, text: &str) -> std::io::Result<()> {
        self.writer.write_all(text.as_bytes())?;
        self.writer.write_all(b"\r\n")?;
        self.writer.flush()
    }

    fn read_line(&mut self) -> std::io::Result<Option<String>> {
        let mut buf = Vec::new();
        let n = (&mut self.reader).take(MAX_LINE as u64).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        while buf.last().is_some_and(|c| *c == b'\n' || *c == b'\r') {
            buf.pop();
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }

    fn ip(&self) -> IpAddr {
        self.peer.ip()
    }

    fn trusted(&self) -> bool {
        let ip = self.ip().to_string();
        let ip = ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string();
        self.cfg.trusted.iter().any(|t| {
            let t = t.trim();
            !t.is_empty() && (ip == t || (t.ends_with('.') && ip.starts_with(t)) || (t.ends_with('*') && ip.starts_with(t.trim_end_matches('*'))))
        })
    }

    fn check_credentials(&self, user: &str, pass: &str) -> bool {
        self.cfg
            .accounts
            .iter()
            .any(|a| a.enabled && a.username == user && a.password == pass)
    }

    fn auth(&mut self, arg: &str) -> std::io::Result<()> {
        if self.user.is_some() {
            return self.reply("503 5.5.1 Deja identifie");
        }
        let mut parts = arg.split_whitespace();
        let mech = parts.next().unwrap_or("").to_ascii_uppercase();
        let initial = parts.next().map(|s| s.to_string());
        let b64 = base64::engine::general_purpose::STANDARD;
        let creds: Option<(String, String)> = match mech.as_str() {
            "PLAIN" => {
                let data = match initial {
                    Some(d) if d != "=" => d,
                    _ => {
                        self.reply("334 ")?;
                        self.read_line()?.unwrap_or_default()
                    }
                };
                if data == "*" {
                    return self.reply("501 5.0.0 Identification annulee");
                }
                b64.decode(data.trim()).ok().and_then(|raw| {
                    let mut it = raw.split(|b| *b == 0);
                    let _authz = it.next()?;
                    let user = String::from_utf8_lossy(it.next()?).into_owned();
                    let pass = String::from_utf8_lossy(it.next()?).into_owned();
                    Some((user, pass))
                })
            }
            "LOGIN" => {
                let user_b64 = match initial {
                    Some(d) => d,
                    None => {
                        self.reply("334 VXNlcm5hbWU6")?;
                        self.read_line()?.unwrap_or_default()
                    }
                };
                self.reply("334 UGFzc3dvcmQ6")?;
                let pass_b64 = self.read_line()?.unwrap_or_default();
                if user_b64 == "*" || pass_b64 == "*" {
                    return self.reply("501 5.0.0 Identification annulee");
                }
                match (b64.decode(user_b64.trim()), b64.decode(pass_b64.trim())) {
                    (Ok(u), Ok(p)) => Some((
                        String::from_utf8_lossy(&u).into_owned(),
                        String::from_utf8_lossy(&p).into_owned(),
                    )),
                    _ => None,
                }
            }
            _ => return self.reply("504 5.5.4 Methode non prise en charge (PLAIN ou LOGIN)"),
        };
        match creds {
            Some((user, pass)) if self.check_credentials(&user, &pass) => {
                self.core.log(Level::Ok, format!("{} identifié en tant que « {user} »", self.ip()));
                self.user = Some(user);
                self.reply("235 2.7.0 Identification reussie")
            }
            Some((user, _)) => {
                self.auth_failures += 1;
                self.core.log(
                    Level::Warn,
                    format!("Identifiant ou mot de passe refusé pour « {user} » depuis {}", self.ip()),
                );
                std::thread::sleep(Duration::from_millis(800 * self.auth_failures as u64));
                self.reply("535 5.7.8 Identifiant ou mot de passe incorrect")
            }
            None => self.reply("501 5.5.2 Donnees d'identification illisibles"),
        }
    }

    fn mail(&mut self, arg: &str) -> std::io::Result<()> {
        if self.helo.is_none() {
            return self.reply("503 5.5.1 Envoyez EHLO d'abord");
        }
        if self.from.is_some() {
            return self.reply("503 5.5.1 Expediteur deja indique");
        }
        if self.cfg.require_auth && self.user.is_none() && !self.trusted() {
            self.core.log(
                Level::Warn,
                format!("{} a essayé d'envoyer sans s'identifier : refusé", self.ip()),
            );
            return self.reply("530 5.7.0 Identification requise");
        }
        let Some(rest) = strip_prefix_ci(arg, "FROM:") else {
            return self.reply("501 5.5.4 Syntaxe : MAIL FROM:<adresse>");
        };
        let (addr, params) = parse_path(rest);
        for p in params.split_whitespace() {
            if let Some(v) = strip_prefix_ci(p, "SIZE=") {
                if v.parse::<usize>().unwrap_or(0) > self.max_bytes() {
                    return self.reply("552 5.3.4 Message trop volumineux");
                }
            }
        }
        if !addr.is_empty() && !addr.contains('@') {
            return self.reply("553 5.1.7 Adresse d'expediteur invalide");
        }
        self.from = Some(addr);
        self.reply("250 2.1.0 Expediteur OK")
    }

    fn rcpt(&mut self, arg: &str) -> std::io::Result<()> {
        if self.from.is_none() {
            return self.reply("503 5.5.1 Envoyez MAIL FROM d'abord");
        }
        let Some(rest) = strip_prefix_ci(arg, "TO:") else {
            return self.reply("501 5.5.4 Syntaxe : RCPT TO:<adresse>");
        };
        let (addr, _) = parse_path(rest);
        if !addr.contains('@') || addr.starts_with('@') || addr.ends_with('@') {
            return self.reply("553 5.1.3 Adresse de destinataire invalide");
        }
        if self.to.len() >= 200 {
            return self.reply("452 4.5.3 Trop de destinataires");
        }
        self.to.push(addr);
        self.reply("250 2.1.5 Destinataire OK")
    }

    fn data(&mut self) -> std::io::Result<()> {
        if self.from.is_none() || self.to.is_empty() {
            return self.reply("503 5.5.1 Indiquez expediteur et destinataires d'abord");
        }
        self.reply("354 Envoyez le message, terminez par une ligne contenant un seul point")?;
        let max = self.max_bytes();
        let mut data: Vec<u8> = Vec::new();
        let mut too_big = false;
        loop {
            let mut line = Vec::new();
            let n = (&mut self.reader).take(1024 * 1024).read_until(b'\n', &mut line)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "fin de connexion pendant DATA"));
            }
            let trimmed: &[u8] = {
                let mut t = &line[..];
                while t.last().is_some_and(|c| *c == b'\n' || *c == b'\r') {
                    t = &t[..t.len() - 1];
                }
                t
            };
            if trimmed == b"." {
                break;
            }
            if too_big {
                continue;
            }
            let content = if trimmed.starts_with(b".") { &trimmed[1..] } else { trimmed };
            data.extend_from_slice(content);
            data.extend_from_slice(b"\r\n");
            if data.len() > max {
                too_big = true;
                data.clear();
            }
        }
        if too_big {
            self.reset();
            self.core.log(Level::Warn, format!("Message de {} refusé : trop volumineux", self.ip()));
            return self.reply("552 5.3.4 Message trop volumineux");
        }
        let from = self.from.clone().unwrap_or_default();
        let to = std::mem::take(&mut self.to);
        let client = match &self.user {
            Some(u) => format!("{} ({u})", self.ip()),
            None => self.ip().to_string(),
        };
        let n_to = to.len();
        let account = self.user.clone().unwrap_or_default();
        let res = self.core.enqueue(from.clone(), to, data, client, account);
        self.reset();
        match res {
            Ok(id) => {
                self.core.log(
                    Level::Info,
                    format!("Message n°{id} reçu de {} : {from} vers {n_to} destinataire(s)", self.ip()),
                );
                self.reply(&format!("250 2.0.0 OK, message {id} en file d'envoi"))
            }
            Err(e) => {
                self.core.log(Level::Error, format!("Message non enregistré : {e}"));
                self.reply("451 4.3.0 Erreur locale, reessayez plus tard")
            }
        }
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(s[prefix.len()..].trim_start())
    } else {
        None
    }
}

/// « <a@b.fr> SIZE=123 » → (« a@b.fr », « SIZE=123 »)
fn parse_path(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            return (rest[..end].trim().to_string(), rest[end + 1..].trim().to_string());
        }
    }
    match s.split_once(' ') {
        Some((a, p)) => (a.to_string(), p.to_string()),
        None => (s.to_string(), String::new()),
    }
}
