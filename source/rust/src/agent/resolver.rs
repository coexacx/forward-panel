use anyhow::{Context, Result, bail};
use ipnet::IpNet;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};
struct Entry {
    ip: IpAddr,
    at: Instant,
}
pub struct Resolver {
    cache: RwLock<HashMap<String, Entry>>,
    gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    pub allowed: Vec<IpNet>,
    pub denied: Vec<IpAddr>,
    pub ttl: Duration,
}
impl Resolver {
    pub fn new(allowed: Vec<IpNet>, denied: Vec<IpAddr>) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            allowed,
            denied,
            ttl: Duration::from_secs(15),
        }
    }
    pub fn permitted(&self, ip: IpAddr) -> bool {
        let ip = unmap(ip);
        if ip.is_unspecified() || ip.is_multicast() || self.denied.contains(&ip) {
            return false;
        }
        if self.allowed.iter().any(|net| net.contains(&ip)) {
            return true;
        }
        const DENY: &[&str] = &[
            "0.0.0.0/8",
            "10.0.0.0/8",
            "100.64.0.0/10",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "192.0.0.0/24",
            "192.0.2.0/24",
            "198.18.0.0/15",
            "198.51.100.0/24",
            "203.0.113.0/24",
            "224.0.0.0/4",
            "240.0.0.0/4",
            "::/128",
            "::1/128",
            "fc00::/7",
            "fe80::/10",
            "ff00::/8",
            "2001:db8::/32",
            "64:ff9b::/96",
            "64:ff9b:1::/48",
            "2002::/16",
        ];
        static NETS: std::sync::OnceLock<Vec<IpNet>> = std::sync::OnceLock::new();
        !NETS
            .get_or_init(|| DENY.iter().map(|s| s.parse().unwrap()).collect())
            .iter()
            .any(|n| n.contains(&ip))
    }
    pub async fn resolve(&self, host: &str) -> Result<IpAddr> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            let ip = unmap(ip);
            if !self.permitted(ip) {
                bail!("blocked")
            }
            return Ok(ip);
        }
        if !crate::valid_target(host, 1) {
            bail!("blocked")
        }
        {
            let c = self.cache.read().await;
            if let Some(e) = c.get(host)
                && e.at.elapsed() < self.ttl
            {
                return Ok(e.ip);
            }
        }
        let gate = {
            let mut gates = self.gates.lock().await;
            gates.retain(|_, v| Arc::strong_count(v) > 1);
            gates.entry(host.into()).or_default().clone()
        };
        let _guard = gate.lock().await;
        {
            let c = self.cache.read().await;
            if let Some(e) = c.get(host)
                && e.at.elapsed() < self.ttl
            {
                return Ok(e.ip);
            }
        }
        let answer =
            tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host((host, 0))).await;
        if let Ok(Ok(ips)) = answer {
            let ips: Vec<_> = ips.map(|x| unmap(x.ip())).collect();
            if ips.is_empty() {
                bail!("dns_error")
            }
            if ips.iter().any(|ip| !self.permitted(*ip)) {
                self.cache.write().await.remove(host);
                bail!("blocked")
            }
            let ip = ips[0];
            let mut c = self.cache.write().await;
            if c.len() >= 1024
                && !c.contains_key(host)
                && let Some(key) = c.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone())
            {
                c.remove(&key);
            }
            c.insert(
                host.into(),
                Entry {
                    ip,
                    at: Instant::now(),
                },
            );
            return Ok(ip);
        }
        let c = self.cache.read().await;
        c.get(host)
            .filter(|e| e.at.elapsed() < Duration::from_secs(60))
            .map(|e| e.ip)
            .context("dns_error")
    }
}
pub fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rebinding_ranges_blocked() {
        let r = Resolver::new(vec![], vec!["8.8.8.8".parse().unwrap()]);
        for ip in [
            "127.0.0.1",
            "10.1.1.1",
            "::ffff:127.0.0.1",
            "169.254.169.254",
            "fc00::1",
            "64:ff9b::a00:1",
            "8.8.8.8",
        ] {
            assert!(!r.permitted(ip.parse().unwrap()), "{ip}")
        }
        assert!(r.permitted("1.1.1.1".parse().unwrap()));
    }
}
