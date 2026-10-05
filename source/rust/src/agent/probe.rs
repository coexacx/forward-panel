use crate::protocol::Probe;
use std::{fs, time::Instant};
#[derive(Default)]
pub struct Sampler {
    total: u64,
    idle: u64,
    tx: u64,
    rx: u64,
    at: Option<Instant>,
    pub interface: String,
}
fn num(s: Option<&&str>) -> u64 {
    s.and_then(|s| s.parse().ok()).unwrap_or(0)
}
impl Sampler {
    pub fn sample(&mut self) -> Probe {
        let mut p = Probe {
            sampled_at: crate::now(),
            ..Default::default()
        };
        let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
        let v: Vec<_> = stat
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let total = (1..=8).map(|i| num(v.get(i))).sum::<u64>();
        let idle = num(v.get(4)) + num(v.get(5));
        let dt = total.saturating_sub(self.total);
        let di = idle.saturating_sub(self.idle);
        if dt > 0 && self.total > 0 {
            p.cpu_percent = (100.0 * (dt.saturating_sub(di)) as f64 / dt as f64).clamp(0.0, 100.0)
        }
        self.total = total;
        self.idle = idle;
        let mem = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let mut avail = 0;
        for line in mem.lines() {
            let v: Vec<_> = line.split_whitespace().collect();
            match v.first().copied().unwrap_or("") {
                "MemTotal:" => p.memory_total = num(v.get(1)) * 1024,
                "MemAvailable:" => avail = num(v.get(1)) * 1024,
                _ => (),
            }
        }
        p.memory_used = p.memory_total.saturating_sub(avail);
        let net = fs::read_to_string("/proc/net/dev").unwrap_or_default();
        let (mut tx, mut rx) = (0, 0);
        for line in net.lines() {
            if let Some((name, v)) = line.split_once(':')
                && name.trim() == self.interface
            {
                let v: Vec<_> = v.split_whitespace().collect();
                rx = num(v.first());
                tx = num(v.get(8));
            }
        }
        let at = Instant::now();
        if let Some(old) = self.at {
            let sec = at.duration_since(old).as_secs_f64();
            if sec > 0.0 {
                p.up_bytes_per_second = tx.saturating_sub(self.tx) as f64 / sec;
                p.down_bytes_per_second = rx.saturating_sub(self.rx) as f64 / sec
            }
        }
        self.tx = tx;
        self.rx = rx;
        self.at = Some(at);
        p
    }
}
