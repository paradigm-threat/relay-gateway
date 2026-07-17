//! Local park-tls state under `${RELAY_INSTALL_ROOT}/state/park-tls/`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn park_tls_root() -> PathBuf {
    if let Ok(p) = std::env::var("RELAY_PARK_TLS_PATH") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let root = std::env::var("RELAY_INSTALL_ROOT").unwrap_or_else(|_| "/opt/relay".into());
    Path::new(&root).join("state/park-tls")
}

fn challenges_dir() -> PathBuf {
    park_tls_root().join("challenges")
}

fn certs_dir() -> PathBuf {
    park_tls_root().join("certs")
}

fn ensure_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)
}

pub fn persist_challenge(token: &str, key_authorization: &str) -> std::io::Result<()> {
    let dir = challenges_dir();
    ensure_dir(&dir)?;
    let path = dir.join(token);
    std::fs::write(path, key_authorization.as_bytes())
}

pub fn remove_challenge(token: &str) {
    let path = challenges_dir().join(token);
    let _ = std::fs::remove_file(path);
}

pub fn challenge_body(token: &str) -> Option<Vec<u8>> {
    let path = challenges_dir().join(token);
    std::fs::read(path).ok()
}

pub fn cert_paths(domain: &str) -> (PathBuf, PathBuf) {
    let dir = certs_dir().join(domain);
    (dir.join("fullchain.pem"), dir.join("privkey.pem"))
}

pub fn persist_cert_pair(domain: &str, fullchain_pem: &str, privkey_pem: &str) -> std::io::Result<()> {
    let (chain, key) = cert_paths(domain);
    if let Some(parent) = chain.parent() {
        ensure_dir(parent)?;
    }
    std::fs::write(&chain, fullchain_pem.as_bytes())?;
    std::fs::write(&key, privkey_pem.as_bytes())?;
    // Caddy (uid caddy) must read PEMs; keep keys non-executable but world-readable on
    // the hardened host (state dir is not web-exposed). Operators may tighten with ACLs.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&chain, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

/// Rough days until leaf expiry; `None` if missing/unreadable.
pub fn read_cert_expiry_days(domain: &str) -> Option<f64> {
    let (chain, _) = cert_paths(domain);
    let pem = std::fs::read_to_string(chain).ok()?;
    let der = pem_to_first_cert_der(&pem)?;
    let not_after = x509_not_after_unix(&der)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((not_after - now) as f64 / 86400.0)
}

fn pem_to_first_cert_der(pem: &str) -> Option<Vec<u8>> {
    let mut lines = pem.lines();
    while let Some(line) = lines.next() {
        if line.contains("BEGIN CERTIFICATE") {
            let mut b64 = String::new();
            for l in lines.by_ref() {
                if l.contains("END CERTIFICATE") {
                    break;
                }
                b64.push_str(l.trim());
            }
            return base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                b64.as_bytes(),
            )
            .ok();
        }
    }
    None
}

/// Minimal DER UTCTime/GeneralizedTime parse for notAfter (ASN.1 Certificate).
fn x509_not_after_unix(der: &[u8]) -> Option<i64> {
    // Prefer openssl via std isn't available; use a tiny recursive walk for UTCTime/GeneralizedTime
    // inside TBSCertificate validity.notAfter. Fallback: treat as needing renewal if unparsable.
    walk_find_time(der, 0).map(|t| t as i64)
}

fn walk_find_time(der: &[u8], depth: usize) -> Option<u64> {
    if depth > 12 || der.is_empty() {
        return None;
    }
    let mut i = 0usize;
    let mut times = Vec::new();
    while i + 2 < der.len() {
        let tag = der[i];
        let (len, hdr) = read_len(&der[i + 1..])?;
        let start = i + 1 + hdr;
        let end = start.checked_add(len)?;
        if end > der.len() {
            break;
        }
        let body = &der[start..end];
        if tag == 0x17 || tag == 0x18 {
            if let Some(ts) = parse_asn1_time(tag, body) {
                times.push(ts);
            }
        } else if tag & 0x20 != 0 {
            if let Some(ts) = walk_find_time(body, depth + 1) {
                times.push(ts);
            }
        }
        i = end;
    }
    // Validity has notBefore then notAfter; take the second time found at shallowest useful depth.
    if times.len() >= 2 {
        Some(times[1])
    } else {
        times.first().copied()
    }
}

fn read_len(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.is_empty() {
        return None;
    }
    let b0 = bytes[0];
    if b0 & 0x80 == 0 {
        return Some((b0 as usize, 1));
    }
    let n = (b0 & 0x7f) as usize;
    if n == 0 || n > 4 || bytes.len() < 1 + n {
        return None;
    }
    let mut len = 0usize;
    for j in 0..n {
        len = (len << 8) | bytes[1 + j] as usize;
    }
    Some((len, 1 + n))
}

fn parse_asn1_time(tag: u8, body: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(body).ok()?;
    // UTCTime YYMMDDHHMMSSZ or GeneralizedTime YYYYMMDDHHMMSSZ
    let (year, rest) = if tag == 0x17 {
        let yy: i32 = s.get(0..2)?.parse().ok()?;
        let year = if yy >= 50 { 1900 + yy } else { 2000 + yy };
        (year, &s[2..])
    } else {
        let yyyy: i32 = s.get(0..4)?.parse().ok()?;
        (yyyy, &s[4..])
    };
    let month: u32 = rest.get(0..2)?.parse().ok()?;
    let day: u32 = rest.get(2..4)?.parse().ok()?;
    let hour: u32 = rest.get(4..6)?.parse().ok()?;
    let min: u32 = rest.get(6..8)?.parse().ok()?;
    let sec: u32 = rest.get(8..10)?.parse().ok()?;
    // days from civil date (Howard Hinnant algorithm) → unix
    let y = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era * 146097 + doe as i64 - 719468) as i64;
    Some((days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RELAY_PARK_TLS_PATH", root.path());
        persist_challenge("tok123", "tok123.abc").unwrap();
        assert_eq!(
            String::from_utf8(challenge_body("tok123").unwrap()).unwrap(),
            "tok123.abc"
        );
        remove_challenge("tok123");
        assert!(challenge_body("tok123").is_none());
        std::env::remove_var("RELAY_PARK_TLS_PATH");
    }
}
