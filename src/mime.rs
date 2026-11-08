//! Petites aides pour lire les en-têtes d'un e-mail brut.

use base64::Engine;

/// Sépare l'en-tête et le corps (à la première ligne vide).
pub fn split(data: &[u8]) -> (&[u8], &[u8]) {
    for i in 0..data.len() {
        if data[i..].starts_with(b"\r\n\r\n") {
            return (&data[..i + 2], &data[i + 4..]);
        }
        if data[i..].starts_with(b"\n\n") {
            return (&data[..i + 1], &data[i + 2..]);
        }
    }
    (data, &[])
}

/// En-têtes dépliés (nom, valeur brute).
pub fn headers(data: &[u8]) -> Vec<(String, String)> {
    let (head, _) = split(data);
    let text = String::from_utf8_lossy(head);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

pub fn header(data: &[u8], name: &str) -> Option<String> {
    headers(data)
        .into_iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| decode_words(&v))
}

pub fn subject_of(data: &[u8]) -> String {
    header(data, "Subject").unwrap_or_else(|| "(sans objet)".into())
}

/// Décode les « mots encodés » RFC 2047 (=?utf-8?B?...?= et =?utf-8?Q?...?=).
pub fn decode_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let before = &rest[..start];
        let after = &rest[start + 2..];
        let parsed = (|| {
            let (charset, r) = after.split_once('?')?;
            let (enc, r) = r.split_once('?')?;
            let end = r.find("?=")?;
            let text = &r[..end];
            let bytes = match enc {
                "B" | "b" => base64::engine::general_purpose::STANDARD
                    .decode(text.trim())
                    .ok()?,
                "Q" | "q" => decode_q(text),
                _ => return None,
            };
            let consumed = start + 2 + charset.len() + 1 + enc.len() + 1 + end + 2;
            Some((to_string(&bytes, charset), consumed))
        })();
        match parsed {
            Some((decoded, consumed)) => {
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&decoded);
                rest = &rest[consumed..];
                last_was_word = true;
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_q(text: &str) -> Vec<u8> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'_' => out.push(b' '),
            b'=' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'='),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn to_string(bytes: &[u8], charset: &str) -> String {
    let cs = charset.to_ascii_lowercase();
    if cs.contains("8859") || cs.contains("latin") || cs.contains("1252") {
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Nom affiché prêt pour un en-tête : tel quel s'il est simple, entre guillemets s'il contient des signes
/// spéciaux, encodé (RFC 2047) s'il contient des accents.
pub fn encode_display_name(name: &str) -> String {
    let name = name.trim();
    if !name.is_ascii() {
        return format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(name.as_bytes()));
    }
    let simple = name.chars().all(|c| c.is_ascii_alphanumeric() || " !#$%&'*+-/=?^_`{|}~".contains(c));
    if simple && !name.contains("=?") {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Le nom affiché d'un en-tête « From » brut, sans le modifier (« "Dolibarr" <a@b.fr> » → « "Dolibarr" »).
fn raw_display_name(from: &str) -> Option<String> {
    let (name, _) = from.rsplit_once('<')?;
    let name = name.trim();
    (!name.is_empty() && name != "\"\"").then(|| name.to_string())
}

/// Remplace l'expéditeur (« From: ») et garde l'original en « Reply-To: » s'il n'y en a pas.
/// `name` : nom affiché imposé ; sans lui, celui de l'application est gardé tel quel.
pub fn rewrite_from(data: &[u8], name: Option<&str>, new_from: &str) -> Vec<u8> {
    let (head, body) = split(data);
    let text = String::from_utf8_lossy(head);
    let mut lines: Vec<String> = Vec::new();
    let mut original_from: Option<String> = None;
    let mut has_reply_to = false;
    // en-tête en cours de suppression : None, Some(true) = From (on le garde de côté), Some(false) = Sender
    let mut skipping: Option<bool> = None;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let continuation = line.starts_with(' ') || line.starts_with('\t');
        if continuation {
            if let Some(is_from) = skipping {
                if is_from {
                    if let Some(f) = original_from.as_mut() {
                        f.push(' ');
                        f.push_str(line.trim());
                    }
                }
                continue;
            }
            lines.push(line.to_string());
            continue;
        }
        skipping = None;
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("from:") {
            original_from = Some(line[5..].trim().to_string());
            skipping = Some(true);
            continue;
        }
        if lower.starts_with("reply-to:") {
            has_reply_to = true;
        }
        if lower.starts_with("sender:") {
            skipping = Some(false);
            continue;
        }
        lines.push(line.to_string());
    }
    // Nom affiché : celui imposé, sinon celui de l'application recopié sans retouche
    // (il était déjà correctement écrit, guillemets et encodage compris).
    let display = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => Some(encode_display_name(n)),
        None => original_from.as_deref().and_then(raw_display_name),
    };
    let from_line = match display {
        Some(n) => format!("From: {n} <{new_from}>"),
        None => format!("From: {new_from}"),
    };
    let mut out = String::new();
    out.push_str(&from_line);
    out.push_str("\r\n");
    if let (Some(orig), false) = (&original_from, has_reply_to) {
        if !orig.to_ascii_lowercase().contains(&new_from.to_ascii_lowercase()) {
            out.push_str(&format!("Reply-To: {orig}\r\n"));
        }
    }
    for l in lines {
        out.push_str(&l);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_subjects() {
        assert_eq!(decode_words("=?UTF-8?B?RmFjdHVyZSBuwrAxMg==?="), "Facture n°12");
        assert_eq!(decode_words("=?utf-8?Q?R=C3=A9initialiser_le_mot?= =?utf-8?Q?_de_passe?="), "Réinitialiser le mot de passe");
        assert_eq!(decode_words("Bonjour"), "Bonjour");
    }

    #[test]
    fn rewrites_from() {
        let mail = b"From: Dolibarr <noreply@erp.local>\r\nTo: a@b.fr\r\nSubject: x\r\n\r\nCorps";
        let out = String::from_utf8(rewrite_from(mail, None, "moi@ovh.fr")).unwrap();
        assert!(out.starts_with("From: Dolibarr <moi@ovh.fr>\r\nReply-To: Dolibarr <noreply@erp.local>\r\n"));
        assert!(out.ends_with("\r\n\r\nCorps"));
    }

    fn from_line(out: &[u8]) -> String {
        header(out, "From").unwrap_or_default()
    }

    #[test]
    fn keeps_encoded_display_name_decodable() {
        // Dolibarr encode « Ma Société » : il ne faut surtout pas le mettre entre guillemets
        let mail = b"From: =?UTF-8?B?TWEgU29jacOpdMOp?= <noreply@erp.local>\r\nTo: a@b.fr\r\n\r\nx";
        let out = rewrite_from(mail, None, "moi@ovh.fr");
        assert!(String::from_utf8_lossy(&out).starts_with("From: =?UTF-8?B?TWEgU29jacOpdMOp?= <moi@ovh.fr>\r\n"));
        assert_eq!(from_line(&out), "Ma Société <moi@ovh.fr>");
        // nom entre guillemets (Immich) : recopié tel quel
        let mail = b"From: \"Immich Photo Server\" <noreply@immich.app>\r\n\r\nx";
        assert!(String::from_utf8_lossy(&rewrite_from(mail, None, "p@x.fr")).starts_with("From: \"Immich Photo Server\" <p@x.fr>\r\n"));
    }

    #[test]
    fn imposed_display_name() {
        let mail = b"From: Dolibarr <noreply@erp.local>\r\nSender: Robot\r\n <noreply@erp.local>\r\n\r\nx";
        let out = rewrite_from(mail, Some("Ma Société"), "moi@ovh.fr");
        assert_eq!(from_line(&out), "Ma Société <moi@ovh.fr>");
        assert!(header(&out, "Sender").is_none());
        assert_eq!(header(&out, "Reply-To").as_deref(), Some("Dolibarr <noreply@erp.local>"));
        let out = rewrite_from(b"From: a@b.fr\r\n\r\nx", Some("Factures, Compta"), "moi@ovh.fr");
        assert!(String::from_utf8_lossy(&out).starts_with("From: \"Factures, Compta\" <moi@ovh.fr>\r\nReply-To: a@b.fr\r\n"));
        // sans nom nulle part
        let out = rewrite_from(b"Subject: x\r\n\r\nx", None, "moi@ovh.fr");
        assert!(String::from_utf8_lossy(&out).starts_with("From: moi@ovh.fr\r\nSubject: x\r\n"));
    }

    #[test]
    fn parses_senders() {
        use crate::config::parse_sender;
        assert_eq!(parse_sender("").unwrap(), None);
        let s = parse_sender(" moi@ovh.fr ").unwrap().unwrap();
        assert_eq!((s.name, s.address.as_str()), (None, "moi@ovh.fr"));
        let s = parse_sender("Ma Société <moi@ovh.fr>").unwrap().unwrap();
        assert_eq!((s.name.as_deref(), s.address.as_str()), (Some("Ma Société"), "moi@ovh.fr"));
        assert!(parse_sender("moi").is_err());
        assert!(parse_sender("Ma Société moi@ovh.fr").is_err());
    }
}
