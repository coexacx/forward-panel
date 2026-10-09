use crate::protocol::{Budget, LeaseGrant, LeaseUsage, Limits};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

struct State {
    policy: Limits,
    budget: Budget,
    id: String,
    deadline: Instant,
    last: Instant,
    tokens: f64,
}
pub struct Gate {
    id: String,
    state: Mutex<State>,
    tcp: AtomicU32,
    udp: AtomicU32,
    waiting: AtomicU32,
    udp_waiting: AtomicU32,
    bandwidth_needed: AtomicBool,
    bandwidth_waiting: AtomicU32,
    bandwidth_limited: AtomicBool,
    notify: Notify,
}
pub struct Permit {
    gate: Arc<Gate>,
    udp: bool,
}
impl Drop for Permit {
    fn drop(&mut self) {
        if self.udp {
            self.gate.udp.fetch_sub(1, Ordering::Relaxed);
        } else {
            self.gate.tcp.fetch_sub(1, Ordering::Relaxed);
        }
        self.gate.notify.notify_waiters();
    }
}
struct Waiting<'a>(&'a AtomicU32);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
impl Gate {
    pub fn new(id: String) -> Arc<Self> {
        let at = Instant::now();
        Arc::new(Self {
            id,
            state: Mutex::new(State {
                policy: Limits::default(),
                budget: Budget::default(),
                id: String::new(),
                deadline: at,
                last: at,
                tokens: 0.,
            }),
            tcp: AtomicU32::new(0),
            udp: AtomicU32::new(0),
            waiting: AtomicU32::new(0),
            udp_waiting: AtomicU32::new(0),
            bandwidth_needed: AtomicBool::new(false),
            bandwidth_waiting: AtomicU32::new(0),
            bandwidth_limited: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }
    pub fn configure(&self, policy: &Limits, grant: Option<&LeaseGrant>, seconds: u32) {
        let mut s = self.state.lock().unwrap();
        let at = Instant::now();
        refill(&mut s, at);
        s.policy = policy.clone();
        s.deadline = at + Duration::from_secs(u64::from(seconds));
        if let Some(g) = grant.filter(|g| g.policy == *policy) {
            s.budget = g.budget.clone();
            s.id = g.id.clone();
        } else {
            s.budget = Budget::default();
            s.id.clear();
        }
        s.tokens = s.tokens.min(burst(s.budget.bandwidth));
        if s.budget.bandwidth == 0 {
            s.tokens = 0.;
        }
        self.bandwidth_limited
            .store(policy.bandwidth_mbps > 0, Ordering::Release);
        self.notify.notify_waiters();
    }
    pub fn usage(&self) -> LeaseUsage {
        let s = self.state.lock().unwrap();
        LeaseUsage {
            lease_id: self.id.clone(),
            grant_id: s.id.clone(),
            tcp_active: self.tcp.load(Ordering::Relaxed),
            udp_active: self.udp.load(Ordering::Relaxed),
            tcp_waiting: self.waiting.load(Ordering::Relaxed),
            udp_waiting: self.udp_waiting.swap(0, Ordering::Relaxed),
            bandwidth_needed: self.bandwidth_needed.swap(false, Ordering::Relaxed)
                || self.bandwidth_waiting.load(Ordering::Relaxed) > 0,
        }
    }
    pub fn idle(&self) -> bool {
        self.tcp.load(Ordering::Relaxed) == 0 && self.udp.load(Ordering::Relaxed) == 0
    }
    fn try_enter(self: &Arc<Self>, udp: bool) -> Option<Permit> {
        let s = self.state.lock().unwrap();
        let (active, limit, slots) = if udp {
            (&self.udp, s.policy.udp_limit, s.budget.udp)
        } else {
            (&self.tcp, s.policy.tcp_limit, s.budget.tcp)
        };
        if limit > 0 && (Instant::now() >= s.deadline || active.load(Ordering::Relaxed) >= slots) {
            return None;
        }
        active.fetch_add(1, Ordering::Relaxed);
        Some(Permit {
            gate: self.clone(),
            udp,
        })
    }
    pub async fn tcp(self: &Arc<Self>) -> Option<Permit> {
        if let Some(p) = self.try_enter(false) {
            return Some(p);
        }
        let wait_cap = self.state.lock().unwrap().policy.tcp_limit.clamp(1, 16);
        if self
            .waiting
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < wait_cap).then_some(n + 1)
            })
            .is_err()
        {
            return None;
        }
        let _waiting = Waiting(&self.waiting);
        let wait = async {
            loop {
                let notified = self.notify.notified();
                if let Some(p) = self.try_enter(false) {
                    return p;
                }
                // Bound missed notifications and remain cancellation safe.
                tokio::select! {_=notified=>(),_=tokio::time::sleep(Duration::from_millis(200))=>()}
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .ok()
    }
    pub fn udp(self: &Arc<Self>) -> Option<Permit> {
        let result = self.try_enter(true);
        if result.is_none() {
            let _ = self
                .udp_waiting
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some((n + 1).min(16_384))
                });
        }
        result
    }
    pub fn bandwidth_limited(&self) -> bool {
        self.bandwidth_limited.load(Ordering::Acquire)
    }
    pub async fn take(&self, wanted: usize) -> usize {
        if wanted == 0 || !self.bandwidth_limited.load(Ordering::Acquire) {
            return wanted;
        }
        self.bandwidth_waiting.fetch_add(1, Ordering::Relaxed);
        let _waiting = Waiting(&self.bandwidth_waiting);
        loop {
            let notified = self.notify.notified();
            let delay = {
                let mut s = self.state.lock().unwrap();
                if s.policy.bandwidth_mbps == 0 {
                    return wanted;
                }
                let now = Instant::now();
                refill(&mut s, now);
                if now >= s.deadline || s.budget.bandwidth == 0 {
                    Duration::from_secs(1)
                } else {
                    // A useful chunk per wakeup avoids byte-by-byte CPU spinning.
                    let minimum =
                        wanted.min((s.budget.bandwidth / 100).clamp(1024, 32768) as usize);
                    if s.tokens >= minimum as f64 {
                        let n = wanted.min(s.tokens as usize);
                        s.tokens -= n as f64;
                        self.bandwidth_needed.store(true, Ordering::Relaxed);
                        return n;
                    }
                    Duration::from_secs_f64(
                        ((minimum as f64 - s.tokens) / s.budget.bandwidth as f64).clamp(0.001, 0.2),
                    )
                }
            };
            tokio::select! {_=notified=>(),_=tokio::time::sleep(delay)=>()}
        }
    }
    pub fn refund(&self, n: usize) {
        if n == 0 || !self.bandwidth_limited.load(Ordering::Acquire) {
            return;
        }
        let mut s = self.state.lock().unwrap();
        s.tokens = (s.tokens + n as f64).min(burst(s.budget.bandwidth));
    }
    pub fn packet(&self, n: usize) -> bool {
        if n == 0 || !self.bandwidth_limited.load(Ordering::Acquire) {
            return true;
        }
        self.bandwidth_needed.store(true, Ordering::Relaxed);
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        refill(&mut s, now);
        if now >= s.deadline || s.budget.bandwidth == 0 || s.tokens < n as f64 {
            return false;
        }
        s.tokens -= n as f64;
        true
    }
}
fn burst(rate: u64) -> f64 {
    if rate == 0 {
        0.
    } else {
        (rate / 10).clamp(65_535, 1_048_576) as f64
    }
}
fn refill(s: &mut State, at: Instant) {
    let until = at.min(s.deadline);
    let seconds = until.saturating_duration_since(s.last).as_secs_f64();
    s.tokens = (s.tokens + seconds * s.budget.bandwidth as f64).min(burst(s.budget.bandwidth));
    s.last = at;
}
