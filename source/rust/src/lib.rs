pub mod agent;
#[cfg(feature = "controller")]
pub mod control;
#[cfg(feature = "controller")]
pub mod deploy;
#[cfg(feature = "controller")]
pub mod payment;
pub mod protocol;
#[cfg(feature = "controller")]
pub mod store;
#[cfg(feature = "controller")]
pub mod web;
use rand::RngCore;
use serde_json::Value;
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path};
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn id() -> String {
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}
pub fn token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}
pub fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
pub fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}
pub fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}
pub fn valid_id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 80
        && v.bytes()
            .all(|x| x.is_ascii_alphanumeric() || x == b'_' || x == b'-')
}
pub fn valid_target(host: &str, port: u16) -> bool {
    if port == 0 || host.is_empty() || host.len() > 253 || host.trim() != host {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return !ip.is_unspecified() && !ip.is_multicast();
    }
    if host.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return false;
    }
    host.trim_end_matches('.').split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.as_bytes()[0].is_ascii_alphanumeric()
            && l.as_bytes()[l.len() - 1].is_ascii_alphanumeric()
            && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    })
}
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid state path"))?;
    let tmp = parent.join(format!(
        ".{}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        id()
    ));
    let res = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    res
}

/// IPv4 wildcard covers all local NICs. IPv6 keeps its existing explicit-IP mode.
pub fn valid_listen_ip(value: &str) -> bool {
    value.parse::<std::net::IpAddr>().is_ok_and(|ip| {
        !ip.is_multicast()
            && (!ip.is_unspecified() || ip.is_ipv4())
            && !matches!(ip, std::net::IpAddr::V4(v) if v.is_broadcast())
            && !matches!(ip, std::net::IpAddr::V6(v) if v.to_ipv4_mapped().is_some())
    })
}
pub fn listen_overlap(a: &str, b: &str) -> bool {
    match (a.parse::<std::net::IpAddr>(), b.parse::<std::net::IpAddr>()) {
        (Ok(a), Ok(b)) => {
            let a = a.to_canonical();
            let b = b.to_canonical();
            a == b || (a.is_ipv4() == b.is_ipv4() && (a.is_unspecified() || b.is_unspecified()))
        }
        _ => true,
    }
}
#[cfg(test)]
mod listen_tests {
    use super::*;
    #[test]
    fn wildcard_overlaps_all_ipv4_interfaces_but_not_ipv6() {
        assert!(valid_listen_ip("0.0.0.0"));
        assert!(!valid_listen_ip("::"));
        assert!(!valid_listen_ip("224.0.0.1"));
        assert!(!valid_listen_ip("255.255.255.255"));
        assert!(listen_overlap("0.0.0.0", "172.10.1.72"));
        assert!(listen_overlap("103.167.134.7", "0.0.0.0"));
        assert!(!listen_overlap("103.167.134.7", "172.10.1.72"));
        assert!(!listen_overlap("0.0.0.0", "::1"));
    }
}
