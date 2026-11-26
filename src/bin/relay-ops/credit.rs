//! Credit burn calendar (`ops/credit_burn.json`) for Fleet UI runway / scale-down warnings.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditBurnFile {
    pub schema: String,
    pub promo: String,
    #[serde(rename = "creditUsd")]
    pub credit_usd: f64,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
    #[serde(rename = "provisionedAt")]
    pub provisioned_at: String,
    #[serde(rename = "hourlyUsd")]
    pub hourly_usd: f64,
    #[serde(rename = "fullFleetDaysApprox")]
    pub full_fleet_days_approx: f64,
    #[serde(rename = "scaleDownTargetDay")]
    pub scale_down_target_day: f64,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreditSnapshot {
    pub promo: String,
    pub credit_usd: f64,
    pub expires_at: String,
    pub provisioned_at: String,
    pub hourly_usd: f64,
    pub days_until_expiry: f64,
    pub days_since_provision: f64,
    pub days_to_scale_down_target: f64,
    pub scale_down_due: bool,
    pub notes: String,
}

pub fn load_path(path: &Path) -> Option<CreditBurnFile> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn load_default() -> Option<CreditBurnFile> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("ops/credit_burn.json");
    if let Some(c) = load_path(&manifest) {
        return Some(c);
    }
    load_path(Path::new("ops/credit_burn.json"))
}

fn unix_now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Parse `YYYY-MM-DD` as UTC midnight (approx via civil days since 1970-01-01).
fn ymd_utc_midnight_secs(ymd: &str) -> Option<f64> {
    let mut parts = ymd.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant civil_from_days / days_from_civil (public domain).
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = m.wrapping_add(9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era * 146097 + doe as i32 - 719468) as f64;
    Some(days * 86400.0)
}

/// Parse RFC3339-ish `2027-02-01T04:59:59Z` (Z or +00:00 only).
fn rfc3339_utc_secs(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z').trim_end_matches("+00:00");
    let (date, time) = s.split_once('T')?;
    let date_secs = ymd_utc_midnight_secs(date)?;
    let mut t = time.split(':');
    let h: f64 = t.next()?.parse().ok()?;
    let mi: f64 = t.next()?.parse().ok()?;
    let sec: f64 = t.next()?.parse().ok()?;
    Some(date_secs + h * 3600.0 + mi * 60.0 + sec)
}

pub fn snapshot(file: &CreditBurnFile, now_unix: f64) -> Option<CreditSnapshot> {
    let expires = rfc3339_utc_secs(&file.expires_at)?;
    let provisioned = ymd_utc_midnight_secs(&file.provisioned_at)?;
    let days_until_expiry = (expires - now_unix) / 86400.0;
    let days_since_provision = (now_unix - provisioned) / 86400.0;
    let days_to_scale_down_target = file.full_fleet_days_approx - days_since_provision;
    Some(CreditSnapshot {
        promo: file.promo.clone(),
        credit_usd: file.credit_usd,
        expires_at: file.expires_at.clone(),
        provisioned_at: file.provisioned_at.clone(),
        hourly_usd: file.hourly_usd,
        days_until_expiry,
        days_since_provision,
        days_to_scale_down_target,
        scale_down_due: days_to_scale_down_target <= 3.0,
        notes: file.notes.clone(),
    })
}

pub fn snapshot_now(file: &CreditBurnFile) -> Option<CreditSnapshot> {
    snapshot(file, unix_now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_math_from_provision() {
        let f = CreditBurnFile {
            schema: "relay.ops.credit_burn.v1".into(),
            promo: "SUPPORT10060".into(),
            credit_usd: 100.0,
            expires_at: "2027-02-01T04:59:59Z".into(),
            provisioned_at: "2026-11-26".into(),
            hourly_usd: 0.144,
            full_fleet_days_approx: 29.0,
            scale_down_target_day: 29.0,
            notes: "test".into(),
        };
        let noon = ymd_utc_midnight_secs("2026-11-26").unwrap() + 12.0 * 3600.0;
        let s = snapshot(&f, noon).unwrap();
        assert!((s.days_since_provision - 0.5).abs() < 0.05);
        assert!((s.days_to_scale_down_target - 28.5).abs() < 0.05);
        assert!(!s.scale_down_due);
    }

    #[test]
    fn load_committed_credit_burn_json() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("ops/credit_burn.json");
        let f = load_path(&p).expect("ops/credit_burn.json");
        assert_eq!(f.promo, "SUPPORT10060");
        assert!(snapshot_now(&f).is_some());
    }
}
