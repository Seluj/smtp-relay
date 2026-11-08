mod config;
mod delivery;
mod mime;
mod server;
mod shot;
mod store;
mod web;

use std::path::PathBuf;

const HELP: &str = "Relais SMTP — serveur SMTP avec interface web

Utilisation : relais-smtp [options]
  (sans option)            démarre le serveur SMTP et l'interface web
  --screenshot <f.png>     photographie l'interface web puis quitte
  --page <nom>             page à photographier (tableau, messages, reception, comptes, envoi, journal, aide)
  --sante                  vérifie que l'interface web répond (code de sortie 0 = OK)

Variables d'environnement :
  RELAIS_SMTP_DIR   dossier des réglages et messages (conteneur : /data)
  SMTP_PORT         port SMTP (remplace le réglage enregistré)
  WEB_PORT          port de l'interface web (par défaut 8080)
  ADMIN_PASSWORD    mot de passe de l'interface web (sinon généré et affiché au premier démarrage)";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(18080)
}

/// Vérifie que l'interface web répond (pour le HEALTHCHECK de Docker).
fn health_check() -> i32 {
    use std::io::{Read, Write};
    let port = env_port("WEB_PORT").unwrap_or(8080);
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), std::time::Duration::from_secs(3)) else { return 1 };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    if s.write_all(b"GET /sante HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return 1;
    }
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    if String::from_utf8_lossy(&buf[..n]).contains(" 200 ") { 0 } else { 1 }
}

fn env_port(name: &str) -> Option<u16> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).filter(|p| *p > 0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut screenshot: Option<PathBuf> = None;
    let mut page = String::from("tableau");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--screenshot" if i + 1 < args.len() => {
                screenshot = Some(PathBuf::from(&args[i + 1]));
                i += 1;
            }
            "--page" if i + 1 < args.len() => {
                page = args[i + 1].clone();
                i += 1;
            }
            "--sante" | "--health" => std::process::exit(health_check()),
            "--help" | "-h" => {
                println!("{HELP}");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    if screenshot.is_some() {
        // réglages isolés pour l'image de contrôle
        let dir = std::env::temp_dir().join(format!("relais-smtp-apercu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe {
            std::env::set_var("RELAIS_SMTP_DIR", &dir);
            std::env::set_var("SMTP_PORT", free_port().to_string());
        }
    }

    let container = std::env::var("RELAIS_CONTAINER").is_ok_and(|v| v == "1") || std::path::Path::new("/.dockerenv").exists();
    let mut cfg = config::Config::load();
    let mut changed = false;
    if let Some(p) = env_port("SMTP_PORT") {
        if cfg.port != p {
            cfg.port = p;
            changed = true;
        }
    }
    if let Some(p) = env_port("WEB_PORT") {
        cfg.web_port = p;
    }
    if container && !cfg.listen_all {
        cfg.listen_all = true;
        changed = true;
    }
    let env_admin = std::env::var("ADMIN_PASSWORD").ok().filter(|p| !p.trim().is_empty());
    let mut generated = None;
    if let Some(p) = &env_admin {
        cfg.admin_password = p.clone();
    } else if cfg.admin_password.is_empty() {
        let p = config::random_password(14);
        cfg.admin_password = p.clone();
        generated = Some(p);
        changed = true;
    }
    if changed && env_admin.is_none() {
        let _ = cfg.save();
    } else if changed {
        // ne pas écrire dans le fichier un mot de passe venant de l'environnement
        let mut on_disk = cfg.clone();
        on_disk.admin_password = config::disk_admin_password();
        let _ = on_disk.save();
    }

    let core = store::Core::new(cfg.clone());
    delivery::spawn_worker(core.clone());
    let ctl = web::Ctl::new(core.clone(), screenshot.is_some(), container, env_admin.is_some());
    if cfg.autostart {
        let _ = ctl.start_smtp();
    }

    if let Some(out) = screenshot {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let ctl2 = ctl.clone();
        let addr2 = addr.clone();
        std::thread::spawn(move || {
            if let Err(e) = web::serve(ctl2, &addr2) {
                eprintln!("{e}");
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        let result = shot::capture(&format!("http://{addr}/#{page}"), &out);
        ctl.stop_smtp();
        let _ = std::fs::remove_dir_all(config::app_dir());
        match result {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("capture : {e}");
                std::process::exit(1);
            }
        }
    }

    let web_addr = format!("0.0.0.0:{}", cfg.web_port);
    println!("──────────────────────────────────────────────");
    println!(" Relais SMTP {}", env!("CARGO_PKG_VERSION"));
    println!(" Interface web : http://{}:{}/", if container { "<adresse-du-serveur>".to_string() } else { web::local_ip() }, cfg.web_port);
    println!(" Serveur SMTP  : port {}", cfg.port);
    println!(" Données       : {}", config::app_dir().display());
    if let Some(p) = generated {
        println!(" Mot de passe de l'interface (à noter, modifiable ensuite) : {p}");
    } else if env_admin.is_some() {
        println!(" Mot de passe de l'interface : celui de ADMIN_PASSWORD");
    }
    println!("──────────────────────────────────────────────");
    if let Err(e) = web::serve(ctl, &web_addr) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
