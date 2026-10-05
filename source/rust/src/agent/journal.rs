use crate::{
    atomic_write, id,
    protocol::{Counter, Report, Rule},
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
pub struct Meter {
    pub rule: String,
    pub cycle: String,
    pub up: AtomicU64,
    pub down: AtomicU64,
    baseline_up: AtomicU64,
    baseline_down: AtomicU64,
}
impl Meter {
    pub fn add(&self, n: usize, up: bool) {
        let v = if up { &self.up } else { &self.down };
        let _ = v.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            old.checked_add(n as u64).filter(|x| *x <= 1 << 60)
        });
    }
    pub fn counter(&self) -> Counter {
        Counter {
            rule_id: self.rule.clone(),
            cycle_id: self.cycle.clone(),
            up: self
                .up
                .load(Ordering::Relaxed)
                .saturating_sub(self.baseline_up.load(Ordering::Relaxed)),
            down: self
                .down
                .load(Ordering::Relaxed)
                .saturating_sub(self.baseline_down.load(Ordering::Relaxed)),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Disk {
    epoch: String,
    sequence: u64,
    counters: BTreeMap<String, Counter>,
}
struct Inner {
    epoch: String,
    sequence: u64,
    meters: BTreeMap<String, Arc<Meter>>,
    acked: HashMap<String, Counter>,
    cursor: String,
}
pub struct Journal {
    path: PathBuf,
    inner: Mutex<Inner>,
    _lock: std::fs::File,
}
impl Journal {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        if !path.is_absolute() {
            bail!("journal path must be absolute")
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .context("another agent is using this journal")?;
        let d = if path.exists() {
            if std::fs::symlink_metadata(path)?.len() > 16 << 20 {
                bail!("journal too large")
            }
            serde_json::from_slice::<Disk>(&std::fs::read(path)?)?
        } else {
            Disk {
                epoch: id(),
                sequence: 0,
                counters: BTreeMap::new(),
            }
        };
        if !crate::valid_id(&d.epoch) || d.sequence >= i64::MAX as u64 {
            bail!("invalid journal")
        }
        let mut meters = BTreeMap::new();
        for (k, c) in d.counters {
            if k != format!("{}:{}", c.rule_id, c.cycle_id)
                || !crate::valid_id(&c.rule_id)
                || !crate::valid_id(&c.cycle_id)
                || c.up > 1 << 60
                || c.down > 1 << 60
            {
                bail!("invalid journal counter")
            }
            meters.insert(
                k,
                Arc::new(Meter {
                    rule: c.rule_id,
                    cycle: c.cycle_id,
                    up: AtomicU64::new(c.up),
                    down: AtomicU64::new(c.down),
                    baseline_up: AtomicU64::new(0),
                    baseline_down: AtomicU64::new(0),
                }),
            );
        }
        let j = Arc::new(Self {
            path: path.to_path_buf(),
            inner: Mutex::new(Inner {
                epoch: d.epoch,
                sequence: d.sequence,
                meters,
                acked: HashMap::new(),
                cursor: String::new(),
            }),
            _lock: lock,
        });
        j.flush()?;
        Ok(j)
    }
    pub fn meter(&self, rule: &str, cycle: &str) -> Arc<Meter> {
        let mut x = self.inner.lock().unwrap();
        x.meters
            .entry(format!("{rule}:{cycle}"))
            .or_insert_with(|| {
                Arc::new(Meter {
                    rule: rule.into(),
                    cycle: cycle.into(),
                    up: AtomicU64::new(0),
                    down: AtomicU64::new(0),
                    baseline_up: AtomicU64::new(0),
                    baseline_down: AtomicU64::new(0),
                })
            })
            .clone()
    }
    fn save(&self, x: &Inner) -> Result<()> {
        let d = Disk {
            epoch: x.epoch.clone(),
            sequence: x.sequence,
            counters: x
                .meters
                .iter()
                .map(|(k, m)| (k.clone(), m.counter()))
                .collect(),
        };
        atomic_write(&self.path, &serde_json::to_vec(&d)?, 0o600)
    }
    pub fn flush(&self) -> Result<()> {
        self.save(&self.inner.lock().unwrap())
    }
    pub fn report(&self) -> Result<Report> {
        let mut x = self.inner.lock().unwrap();
        x.sequence = x
            .sequence
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .context("journal sequence exhausted")?;
        let all: BTreeMap<_, _> = x
            .meters
            .iter()
            .map(|(k, m)| (k.clone(), m.counter()))
            .collect();
        let mut counters = Vec::new();
        let mut last = String::new();
        for (k, c) in all
            .iter()
            .filter(|(k, _)| **k > x.cursor)
            .chain(all.iter().filter(|(k, _)| **k <= x.cursor))
        {
            if x.acked.get(k) != Some(c) {
                counters.push(c.clone());
                last = k.clone();
                if counters.len() == 1000 {
                    break;
                }
            }
        }
        if !last.is_empty() {
            x.cursor = last;
        }
        // Persist the very same snapshot sent to the controller. A crash can replay it,
        // but must never move an acknowledged counter backwards.
        let d = Disk {
            epoch: x.epoch.clone(),
            sequence: x.sequence,
            counters: all,
        };
        atomic_write(&self.path, &serde_json::to_vec(&d)?, 0o600)?;
        Ok(Report {
            version: 1,
            epoch: x.epoch.clone(),
            sequence: x.sequence,
            counters,
            ..Default::default()
        })
    }
    pub fn ack(&self, r: &Report) {
        let mut x = self.inner.lock().unwrap();
        if x.epoch == r.epoch {
            for c in &r.counters {
                x.acked
                    .insert(format!("{}:{}", c.rule_id, c.cycle_id), c.clone());
            }
        }
    }
    pub fn prune(&self, rules: &[Rule]) -> Result<()> {
        let mut x = self.inner.lock().unwrap();
        let current: HashMap<_, _> = rules
            .iter()
            .map(|r| (r.id.as_str(), r.cycle_id.as_str()))
            .collect();
        let remove: Vec<_> = x
            .meters
            .iter()
            .filter(|(k, m)| {
                current.get(m.rule.as_str()).is_some_and(|c| *c != m.cycle)
                    && x.acked.get(*k) == Some(&m.counter())
                    && Arc::strong_count(m) == 1
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in remove {
            x.meters.remove(&k);
            x.acked.remove(&k);
        }
        if x.meters.len() > 4096 {
            // Advance the reporting epoch without resetting live atomic counters.
            // Subtract only controller-acknowledged bytes. In-flight increments
            // therefore remain chargeable in the new epoch, with no packet lock.
            let removable: Vec<_> = x
                .meters
                .iter()
                .filter(|(k, m)| {
                    !current.contains_key(m.rule.as_str())
                        && Arc::strong_count(m) == 1
                        && x.acked.get(*k) == Some(&m.counter())
                })
                .map(|(k, _)| k.clone())
                .collect();
            if !removable.is_empty() {
                for k in removable {
                    x.meters.remove(&k);
                    x.acked.remove(&k);
                }
                for (k, m) in &x.meters {
                    if let Some(ack) = x.acked.get(k) {
                        m.baseline_up.fetch_add(ack.up, Ordering::Relaxed);
                        m.baseline_down.fetch_add(ack.down, Ordering::Relaxed);
                    }
                }
                x.epoch = id();
                x.sequence = 0;
                x.cursor.clear();
                x.acked.clear();
                self.save(&x)?;
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_rules_compact_without_losing_live_or_paused_traffic() {
        let d = std::env::temp_dir().join(format!("forward-compact-{}", id()));
        std::fs::create_dir(&d).unwrap();
        let path = d.join("traffic.json");
        let j = Journal::open(&path).unwrap();
        for index in 0..4100 {
            j.meter(&format!("old{index}"), "cycle").add(20, true);
        }
        let live = j.meter("live", "cycle");
        live.add(100, true);
        for _ in 0..6 {
            let r = j.report().unwrap();
            j.ack(&r);
        }
        let old = j.report().unwrap();
        j.ack(&old);
        live.add(7, true);
        let in_flight = j.meter("pending", "cycle");
        in_flight.add(3, true);
        drop(in_flight);
        let writer = live.clone();
        let thread = std::thread::spawn(move || {
            for _ in 0..100_000 {
                writer.add(1, true);
            }
        });
        j.prune(&[Rule {
            id: "live".into(),
            cycle_id: "cycle".into(),
            ..Default::default()
        }])
        .unwrap();
        thread.join().unwrap();
        let r = j.report().unwrap();
        assert_ne!(r.epoch, old.epoch);
        assert_eq!(
            r.counters.iter().find(|c| c.rule_id == "live").unwrap().up,
            100_007
        );
        assert_eq!(
            r.counters
                .iter()
                .find(|c| c.rule_id == "pending")
                .unwrap()
                .up,
            3
        );
        assert_eq!(j.inner.lock().unwrap().meters.len(), 2);
        // A paused/retired rule starts at zero only after changing the epoch.
        j.meter("old0", "cycle").add(5, true);
        let r = j.report().unwrap();
        assert_eq!(
            r.counters.iter().find(|c| c.rule_id == "old0").unwrap().up,
            5
        );
        drop(live);
        drop(j);
        let restarted = Journal::open(&path).unwrap();
        let replay = restarted.report().unwrap();
        assert_eq!(replay.epoch, r.epoch);
        assert_eq!(replay.counters, r.counters);
        drop(restarted);
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn replay_does_not_double_or_lose() {
        let d = std::env::temp_dir().join(format!("forward-journal-{}", id()));
        std::fs::create_dir(&d).unwrap();
        let p = d.join("traffic.json");
        {
            let j = Journal::open(&p).unwrap();
            let m = j.meter("r", "c");
            m.add(20, true);
            let r = j.report().unwrap();
            m.add(7, true);
            j.ack(&r);
            assert_eq!(j.report().unwrap().counters[0].up, 27);
            assert!(Journal::open(&p).is_err());
        }
        let j = Journal::open(&p).unwrap();
        assert_eq!(j.report().unwrap().counters[0].up, 27);
        drop(j);
        std::fs::remove_dir_all(d).unwrap();
    }
}
