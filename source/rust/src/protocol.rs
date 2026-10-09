use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct Target {
    pub host: String,
    pub port: u16,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Rule {
    pub load_balance: bool,
    pub targets: Vec<Target>,
    pub id: String,
    pub user_id: String,
    pub lease_id: String,
    pub cycle_id: String,
    pub listen_ip: String,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub expires_at: i64,
}
impl Rule {
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            serde_json::to_vec(self).expect("rule serialization"),
        ))
    }
    pub fn targets(&self) -> Vec<Target> {
        if self.load_balance {
            self.targets.clone()
        } else {
            vec![Target {
                host: self.target_host.clone(),
                port: self.target_port,
            }]
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Counter {
    pub rule_id: String,
    pub cycle_id: String,
    pub up: u64,
    pub down: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetCheck {
    pub rule_id: String,
    pub host: String,
    pub port: u16,
    pub status: String,
    pub latency_ms: f64,
    pub checked_at: i64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Probe {
    pub target_checks: Option<Vec<TargetCheck>>,
    pub cpu_percent: f64,
    pub memory_used: u64,
    pub memory_total: u64,
    pub up_bytes_per_second: f64,
    pub down_bytes_per_second: f64,
    pub sampled_at: i64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleError {
    pub rule_id: String,
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub agent_version: String,
    pub kernel_version: String,
    pub version: u32,
    pub epoch: String,
    pub sequence: u64,
    pub counters: Vec<Counter>,
    pub probe: Probe,
    pub applied_revision: i64,
    pub active_rules: Option<Vec<String>>,
    pub applied_rules: Option<std::collections::BTreeMap<String, String>>,
    pub supports_delta: bool,
    pub errors: Vec<RuleError>,
    pub decommission_ack: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub delta: bool,
    pub base_revision: i64,
    pub removed_rules: Vec<String>,
    pub controller_urls: Option<Vec<String>>,
    pub version: u32,
    pub revision: i64,
    pub rules: Vec<Rule>,
    pub valid_for_seconds: u32,
    pub ack_sequence: u64,
    pub ack_epoch: String,
    pub decommission: String,
}
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

impl Config {
    /// Only used after the peer confirms the preceding configuration on this WSS session.
    pub fn delta_from(&self, previous: &Self) -> Self {
        let old: std::collections::HashMap<_, _> =
            previous.rules.iter().map(|r| (r.id.as_str(), r)).collect();
        let wanted: std::collections::HashSet<_> =
            self.rules.iter().map(|r| r.id.as_str()).collect();
        let mut next = self.clone();
        next.delta = true;
        next.base_revision = previous.revision;
        next.rules
            .retain(|r| old.get(r.id.as_str()).is_none_or(|old| **old != *r));
        next.removed_rules = previous
            .rules
            .iter()
            .filter(|r| !wanted.contains(r.id.as_str()))
            .map(|r| r.id.clone())
            .collect();
        next
    }
    /// Reconstruct a bounded desired set before passing it to the local engine.
    pub fn expand(&self, previous: Option<&Self>) -> anyhow::Result<Self> {
        use anyhow::{bail, ensure};
        use std::collections::{BTreeMap, HashSet};
        ensure!(
            self.version == 1 && (1..=30).contains(&self.valid_for_seconds),
            "invalid configuration"
        );
        ensure!(
            self.rules.len() <= 512 && self.removed_rules.len() <= 512,
            "too many rules"
        );
        let mut seen = HashSet::new();
        for r in &self.rules {
            ensure!(
                crate::valid_id(&r.id) && seen.insert(r.id.as_str()),
                "duplicate or invalid rule"
            );
        }
        for id in &self.removed_rules {
            ensure!(
                crate::valid_id(id) && seen.insert(id.as_str()),
                "conflicting rule removal"
            );
        }
        let mut rules: BTreeMap<String, Rule> = if self.delta {
            let Some(old) = previous else {
                bail!("full configuration required")
            };
            ensure!(
                old.revision == self.base_revision && self.revision >= self.base_revision,
                "configuration base mismatch"
            );
            old.rules
                .iter()
                .map(|r| (r.id.clone(), r.clone()))
                .collect()
        } else {
            ensure!(self.removed_rules.is_empty(), "removals require delta");
            BTreeMap::new()
        };
        for id in &self.removed_rules {
            rules.remove(id);
        }
        for r in &self.rules {
            rules.insert(r.id.clone(), r.clone());
        }
        ensure!(rules.len() <= 512, "too many rules");
        let mut full = self.clone();
        full.delta = false;
        full.base_revision = 0;
        full.removed_rules.clear();
        full.rules = rules.into_values().collect();
        Ok(full)
    }
    pub fn fingerprints(&self) -> std::collections::BTreeMap<String, String> {
        self.rules
            .iter()
            .map(|r| (r.id.clone(), r.fingerprint()))
            .collect()
    }
}
