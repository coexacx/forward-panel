use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// A reservation outlives the 10 s Agent grant plus bounded frame transit.
// Persisted reservations also survive a controller restart.
const RESERVATION_SECONDS: i64 = 30;
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    entries: BTreeMap<String, Entry>,
}
#[derive(Default, Serialize, Deserialize)]
struct Entry {
    grant: LeaseGrant,
    reserved: Budget,
    usage: LeaseUsage,
    seen: i64,
    until: i64,
}
fn get(b: &Budget, dimension: usize) -> u64 {
    match dimension {
        0 => b.bandwidth,
        1 => u64::from(b.tcp),
        _ => u64::from(b.udp),
    }
}
fn set(b: &mut Budget, dimension: usize, v: u64) {
    match dimension {
        0 => b.bandwidth = v,
        1 => b.tcp = v as u32,
        _ => b.udp = v as u32,
    }
}
fn distribute(cap: u64, base: &[u64], demand: &[u64]) -> Vec<u64> {
    let total: u64 = base.iter().sum();
    if total > cap {
        return vec![0; base.len()];
    }
    let mut out = base.to_vec();
    let mut free = cap - total;
    loop {
        let eligible: Vec<_> = (0..out.len()).filter(|i| out[*i] < demand[*i]).collect();
        if eligible.is_empty() || free == 0 {
            break;
        }
        let share = (free / eligible.len() as u64).max(1);
        for i in eligible {
            let add = share.min(demand[i].saturating_sub(out[i])).min(free);
            out[i] += add;
            free -= add;
        }
    }
    out
}
impl Ledger {
    fn allocate(
        &mut self,
        key: &str,
        lease: &str,
        policy: &Limits,
        usage: &LeaseUsage,
        at: i64,
    ) -> Option<LeaseGrant> {
        self.entries.retain(|_, e| e.until > at);
        if !self.entries.contains_key(key) && self.entries.len() >= 256 {
            return None;
        }
        let current = self.entries.entry(key.into()).or_insert_with(|| Entry {
            grant: LeaseGrant {
                lease_id: lease.into(),
                id: id(),
                policy: policy.clone(),
                ..Default::default()
            },
            ..Default::default()
        });
        if !usage.grant_id.is_empty() && usage.grant_id == current.grant.id {
            current.reserved = current.grant.budget.clone();
            current.reserved.tcp = current.reserved.tcp.max(usage.tcp_active);
            current.reserved.udp = current.reserved.udp.max(usage.udp_active);
        }
        current.usage = usage.clone();
        current.seen = at;
        current.until = at + RESERVATION_SECONDS;
        // Previously unlimited established connections are counted during a
        // policy change. They drain normally; no new capacity is invented.
        current.reserved.tcp = current.reserved.tcp.max(usage.tcp_active);
        current.reserved.udp = current.reserved.udp.max(usage.udp_active);
        let keys: Vec<_> = self.entries.keys().cloned().collect();
        let live: Vec<_> = keys.iter().map(|k| self.entries[k].seen > at - 5).collect();
        let live_count = live.iter().filter(|x| **x).count();
        let mut target = vec![Budget::default(); keys.len()];
        for (dim, cap) in [
            (0, policy.bytes_per_second()),
            (1, u64::from(policy.tcp_limit)),
            (2, u64::from(policy.udp_limit)),
        ] {
            if cap == 0 {
                continue;
            }
            let base: Vec<_> = keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    if !live[i] {
                        return 0;
                    }
                    match dim {
                        1 => u64::from(self.entries[k].usage.tcp_active),
                        2 => u64::from(self.entries[k].usage.udp_active),
                        _ => 0,
                    }
                })
                .collect();
            let demand: Vec<_> = keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    if !live[i] {
                        return 0;
                    }
                    let u = &self.entries[k].usage;
                    if live_count == 1 {
                        return cap.max(base[i]);
                    }
                    match dim {
                        0 => {
                            if u.bandwidth_needed {
                                cap
                            } else {
                                0
                            }
                        }
                        1 => base[i]
                            .saturating_add(u64::from(u.tcp_waiting))
                            .min(cap)
                            .max(base[i]),
                        _ => base[i]
                            .saturating_add(u64::from(u.udp_waiting))
                            .min(cap)
                            .max(base[i]),
                    }
                })
                .collect();
            let mut amounts = distribute(cap, &base, &demand);
            // Keep spare connection slots warm while no node is asking for
            // them. Pending connections take priority over spare capacity.
            if dim > 0 && base.iter().sum::<u64>() <= cap {
                let warm: Vec<_> = live
                    .iter()
                    .enumerate()
                    .map(|(i, live)| if *live { cap } else { amounts[i] })
                    .collect();
                amounts = distribute(cap, &amounts, &warm);
            }
            if dim == 0 && demand.iter().all(|d| *d == 0) {
                let warm: Vec<_> = live.iter().map(|yes| if *yes { cap } else { 0 }).collect();
                amounts = distribute(cap, &base, &warm);
            }
            for (i, v) in amounts.into_iter().enumerate() {
                set(&mut target[i], dim, v);
            }
        }
        // Send reductions first; their old reservations remain held until the
        // receiving Agent acknowledges the exact new grant identifier.
        for (i, k) in keys.iter().enumerate() {
            let e = self.entries.get_mut(k).unwrap();
            let mut changed = e.grant.policy != *policy;
            for dim in 0..3 {
                let desired = get(&target[i], dim);
                if desired < get(&e.grant.budget, dim) {
                    set(&mut e.grant.budget, dim, desired);
                    changed = true;
                }
            }
            if changed {
                e.grant.id = id();
                e.grant.policy = policy.clone();
            }
        }
        for dim in 0..3 {
            let cap = match dim {
                0 => policy.bytes_per_second(),
                1 => u64::from(policy.tcp_limit),
                _ => u64::from(policy.udp_limit),
            };
            let occupied: u64 = self.entries.values().map(|e| get(&e.reserved, dim)).sum();
            let mut available = cap.saturating_sub(occupied);
            // A current request is handled first; distribution above still
            // determines fair target shares and bounds the sum.
            let mut indices: Vec<_> = (0..keys.len()).collect();
            indices.sort_by_key(|i| keys[*i] != key);
            for i in indices {
                let e = self.entries.get_mut(&keys[i]).unwrap();
                let offered = get(&e.grant.budget, dim);
                let reserved = get(&e.reserved, dim);
                let reusable = reserved.saturating_sub(offered);
                let extra = get(&target[i], dim).saturating_sub(offered);
                let add = extra.min(reusable.saturating_add(available));
                if add == 0 {
                    continue;
                }
                let newly_reserved = add.saturating_sub(reusable);
                available = available.saturating_sub(newly_reserved);
                set(&mut e.grant.budget, dim, offered + add);
                set(&mut e.reserved, dim, reserved + newly_reserved);
                e.grant.id = id();
                e.grant.policy = policy.clone();
            }
        }
        Some(self.entries[key].grant.clone())
    }
}
impl Store {
    pub async fn limit_config(
        &self,
        node: &str,
        session: &str,
        r: &Report,
        cfg: &mut Config,
        previous: Option<&Config>,
    ) -> Result<()> {
        if !r.supports_limits {
            cfg.rules.retain(|r| !r.limits.enabled());
            return Ok(());
        }
        if r.lease_usage.len() > 512 {
            return Err(invalid());
        }
        let mut seen = HashSet::new();
        for u in &r.lease_usage {
            if !valid_id(&u.lease_id)
                || !seen.insert(&u.lease_id)
                || (!u.grant_id.is_empty() && !valid_id(&u.grant_id))
                || u.tcp_active > 65_536
                || u.udp_active > 16_384
                || u.tcp_waiting > 65_536
                || u.udp_waiting > 16_384
            {
                return Err(invalid());
            }
        }
        let policies: BTreeMap<_, _> = cfg
            .rules
            .iter()
            .filter(|r| r.limits.enabled())
            .map(|r| (r.lease_id.clone(), r.limits.clone()))
            .collect();
        let retiring: Vec<_> = previous
            .map(|p| {
                p.lease_limits
                    .iter()
                    .filter(|g| !policies.contains_key(&g.lease_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if policies.is_empty() && retiring.is_empty() {
            return Ok(());
        }
        let _guard = self.writer.lock().await;
        let mut db = self.pool.begin().await?;
        let key = format!("{node}/{session}");
        for (lease, policy) in policies {
            let existing = rows(
                &mut db,
                "SELECT state FROM vp_resource_limits WHERE lease_id=?",
                &[json!(lease)],
            )
            .await?;
            let mut ledger: Ledger = existing
                .first()
                .map(|r| serde_json::from_str(s(r, "state")))
                .transpose()?
                .unwrap_or_default();
            let blank = LeaseUsage {
                lease_id: lease.clone(),
                ..Default::default()
            };
            let usage = r
                .lease_usage
                .iter()
                .find(|u| u.lease_id == lease)
                .unwrap_or(&blank);
            if let Some(grant) = ledger.allocate(&key, &lease, &policy, usage, now()) {
                cfg.lease_limits.push(grant);
            }
            exec(&mut db,"INSERT INTO vp_resource_limits(lease_id,state) VALUES(?,?) ON DUPLICATE KEY UPDATE state=VALUES(state)",&[json!(lease),json!(serde_json::to_string(&ledger)?)]).await?;
        }
        for prior in retiring {
            let lease = &prior.lease_id;
            let existing = rows(
                &mut db,
                "SELECT state FROM vp_resource_limits WHERE lease_id=?",
                &[json!(lease)],
            )
            .await?;
            let Some(raw) = existing.first() else {
                continue;
            };
            let mut ledger: Ledger = serde_json::from_str(s(raw, "state"))?;
            // Once the Agent acknowledges the exact remaining desired set for
            // this lease, removed listeners cannot consume its old capacity.
            let rule_ids: HashSet<String> = rows(
                &mut db,
                "SELECT id FROM vp_allocations WHERE lease_id=? AND node_id=?",
                &[json!(lease), json!(node)],
            )
            .await?
            .into_iter()
            .map(|v| s(&v, "id").to_owned())
            .collect();
            let expected: std::collections::BTreeMap<_, _> = cfg
                .rules
                .iter()
                .filter(|r| r.lease_id == *lease)
                .map(|r| (r.id.clone(), r.fingerprint()))
                .collect();
            let acknowledged = r.applied_rules.as_ref().is_some_and(|map| {
                map.iter()
                    .filter(|(id, _)| rule_ids.contains(*id))
                    .all(|(id, hash)| expected.get(id) == Some(hash))
                    && expected.iter().all(|(id, hash)| map.get(id) == Some(hash))
            });
            if acknowledged {
                ledger.entries.remove(&key);
            } else if let Some(entry) = ledger.entries.get_mut(&key) {
                entry.until = now() + RESERVATION_SECONDS;
                // Carry the retirement marker until a later report confirms
                // removal, even though there is no limited rule left.
                cfg.lease_limits.push(prior.clone());
            }
            exec(
                &mut db,
                "UPDATE vp_resource_limits SET state=? WHERE lease_id=?",
                &[json!(serde_json::to_string(&ledger)?), json!(lease)],
            )
            .await?;
        }
        db.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_ack_restart_and_expiry_do_not_double_grant() {
        let policy = Limits {
            bandwidth_mbps: 8,
            tcp_limit: 2,
            udp_limit: 2,
        };
        let mut l = Ledger::default();
        let ua = LeaseUsage {
            lease_id: "lease".into(),
            bandwidth_needed: true,
            ..Default::default()
        };
        let ga = l.allocate("a/session", "lease", &policy, &ua, 100).unwrap();
        assert_eq!(ga.budget.tcp, 2);
        let ub = LeaseUsage {
            lease_id: "lease".into(),
            tcp_waiting: 2,
            udp_waiting: 2,
            bandwidth_needed: true,
            ..Default::default()
        };
        let gb = l.allocate("b/session", "lease", &policy, &ub, 101).unwrap();
        assert_eq!(gb.budget.bandwidth, 0);
        assert_eq!(gb.budget.tcp, 0);
        let mut ua = ua;
        ua.grant_id = ga.id.clone();
        let reduction = l.allocate("a/session", "lease", &policy, &ua, 102).unwrap();
        assert!(reduction.budget.tcp < 2);
        let still_held = l.allocate("b/session", "lease", &policy, &ub, 103).unwrap();
        assert_eq!(still_held.budget.tcp, 0);
        ua.grant_id = reduction.id;
        l.allocate("a/session", "lease", &policy, &ua, 104).unwrap();
        let gb = l.allocate("b/session", "lease", &policy, &ub, 105).unwrap();
        assert!(gb.budget.tcp > 0);
        assert!(l.entries.values().map(|e| e.reserved.tcp).sum::<u32>() <= 2);
        assert!(
            l.entries
                .values()
                .map(|e| e.reserved.bandwidth)
                .sum::<u64>()
                <= 1_000_000
        );
        let raw = serde_json::to_string(&l).unwrap();
        let mut restored: Ledger = serde_json::from_str(&raw).unwrap();
        let next = restored
            .allocate("c/new-session", "lease", &policy, &ub, 106)
            .unwrap();
        assert_eq!(next.budget.tcp, 0);
        let free = restored
            .allocate("c/new-session", "lease", &policy, &ub, 140)
            .unwrap();
        assert_eq!(free.budget.tcp, 2);
    }
    #[test]
    fn lowering_concurrency_drains_existing_connections() {
        let mut l = Ledger::default();
        let p = Limits {
            tcp_limit: 10,
            ..Default::default()
        };
        let u = LeaseUsage {
            tcp_active: 8,
            ..Default::default()
        };
        let first = l.allocate("a", "lease", &p, &u, 100).unwrap();
        let p = Limits {
            tcp_limit: 2,
            ..Default::default()
        };
        let u = LeaseUsage {
            grant_id: first.id,
            tcp_active: 8,
            ..Default::default()
        };
        let next = l.allocate("a", "lease", &p, &u, 101).unwrap();
        assert_eq!(next.budget.tcp, 0);
        assert!(l.entries["a"].reserved.tcp >= 8);
    }

    #[test]
    fn delayed_acknowledgements_keep_aggregate_reservations_bounded() {
        let policy = Limits {
            bandwidth_mbps: 100,
            tcp_limit: 79,
            udp_limit: 37,
        };
        let mut ledger = Ledger::default();
        let mut returned: std::collections::BTreeMap<String, LeaseGrant> = Default::default();
        let mut random = 19u64;
        for step in 0..2500 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = format!("node-{}", (random >> 32) % 7);
            let old = returned.get(&key);
            let usage = LeaseUsage {
                lease_id: "lease".into(),
                grant_id: old.map(|g| g.id.clone()).unwrap_or_default(),
                tcp_active: old
                    .map(|g| ((random >> 20) % (u64::from(g.budget.tcp) + 1)) as u32)
                    .unwrap_or(0),
                udp_active: old
                    .map(|g| ((random >> 40) % (u64::from(g.budget.udp) + 1)) as u32)
                    .unwrap_or(0),
                tcp_waiting: ((random >> 5) % 16) as u32,
                udp_waiting: ((random >> 9) % 8) as u32,
                bandwidth_needed: random & 1 == 1,
            };
            let grant = ledger
                .allocate(&key, "lease", &policy, &usage, 100 + step / 100)
                .unwrap();
            returned.insert(key, grant);
            assert!(
                ledger.entries.values().map(|e| e.reserved.tcp).sum::<u32>() <= policy.tcp_limit
            );
            assert!(
                ledger.entries.values().map(|e| e.reserved.udp).sum::<u32>() <= policy.udp_limit
            );
            assert!(
                ledger
                    .entries
                    .values()
                    .map(|e| e.reserved.bandwidth)
                    .sum::<u64>()
                    <= policy.bytes_per_second()
            );
            if step % 100 == 0 {
                ledger = serde_json::from_str(&serde_json::to_string(&ledger).unwrap()).unwrap();
            }
        }
    }
}
