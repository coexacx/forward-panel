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
    pub errors: Vec<RuleError>,
    pub decommission_ack: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
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
