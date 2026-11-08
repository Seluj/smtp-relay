//! `--screenshot` : photographie l'interface web avec un Chrome/Chromium sans fenêtre.

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CHROME") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let fixed = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    ];
    for f in fixed {
        if Path::new(f).exists() {
            return Some(PathBuf::from(f));
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome", "msedge"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub fn capture(url: &str, out: &Path) -> Result<(), String> {
    let browser = find_browser().ok_or("aucun navigateur Chrome/Chromium trouvé (variable CHROME pour l'indiquer)")?;
    let out = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(out)
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&out);
    let profile = std::env::temp_dir().join(format!("relais-smtp-chrome-{}", std::process::id()));
    let status = Command::new(&browser)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-first-run",
            "--no-default-browser-check",
            "--force-device-scale-factor=1",
            "--window-size=1280,820",
            "--virtual-time-budget=4000",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--screenshot={}", out.display()))
        .arg(url)
        .output()
        .map_err(|e| format!("lancement du navigateur impossible : {e}"))?;
    let _ = std::fs::remove_dir_all(&profile);
    if !out.exists() {
        return Err(format!(
            "le navigateur n'a pas produit d'image ({}) : {}",
            status.status,
            String::from_utf8_lossy(&status.stderr).lines().last().unwrap_or("")
        ));
    }
    Ok(())
}
