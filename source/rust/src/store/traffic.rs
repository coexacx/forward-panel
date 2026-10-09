use super::*;
impl Store {
    pub async fn report(&self, node: &str, r: &Report) -> Result<()> {
        if r.agent_version.len() > 80
            || r.kernel_version.len() > 80
            || r.version != 1
            || !valid_id(&r.epoch)
            || r.sequence == 0
            || r.sequence > i64::MAX as u64
            || r.counters.len() > 1000
            || r.lease_usage.len() > 512
            || r.lease_usage.iter().any(|u| {
                !valid_id(&u.lease_id)
                    || (!u.grant_id.is_empty() && !valid_id(&u.grant_id))
                    || u.tcp_active > 65_536
                    || u.tcp_waiting > 65_536
                    || u.udp_active > 16_384
                    || u.udp_waiting > 16_384
            })
            || r.errors.len() > 512
            || r.active_rules.as_ref().is_some_and(|ids| {
                ids.len() > 512
                    || ids.iter().any(|id| !valid_id(id))
                    || ids.iter().collect::<HashSet<_>>().len() != ids.len()
            })
            || r.applied_rules.as_ref().is_some_and(|rules| {
                rules.len() > 512
                    || rules.iter().any(|(id, hash)| {
                        !valid_id(id)
                            || hash.len() != 64
                            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                    })
            })
            || r.probe
                .target_checks
                .as_ref()
                .is_some_and(|v| v.len() > 8192)
            || r.probe.rule_connections.as_ref().is_some_and(|counts| {
                counts.len() > 512
                    || counts
                        .iter()
                        .any(|(id, c)| !valid_id(id) || c.tcp > 65_536 || c.udp > 16_384)
            })
            || !r.probe.cpu_percent.is_finite()
            || !(0.0..=100.0).contains(&r.probe.cpu_percent)
            || r.probe.memory_used > r.probe.memory_total
            || !r.probe.up_bytes_per_second.is_finite()
            || r.probe.up_bytes_per_second < 0.0
            || !r.probe.down_bytes_per_second.is_finite()
            || r.probe.down_bytes_per_second < 0.0
        {
            return Err(invalid());
        }
        for e in &r.errors {
            if e.rule_id.len() > 80 || e.code.len() > 80 || e.message.len() > 256 {
                return Err(invalid());
            }
        }
        let _g = self.writer.lock().await;
        let mut tx = self.pool.begin().await?;
        if count(
            &mut tx,
            "SELECT COUNT(*) AS n FROM vp_nodes WHERE id=? AND enabled=1 AND deleted=0",
            &[json!(node)],
        )
        .await?
            != 1
        {
            return Err(forbidden());
        }
        let last = rows(
            &mut tx,
            "SELECT sequence FROM vp_epochs WHERE node_id=? AND epoch=?",
            &[json!(node), json!(r.epoch)],
        )
        .await?;
        if last
            .first()
            .is_some_and(|v| n(v, "sequence") >= r.sequence as i64)
        {
            tx.commit().await?;
            return Ok(());
        }
        let mut seen = HashSet::new();
        let mut changed = false;
        for c in &r.counters {
            if !valid_id(&c.rule_id)
                || !valid_id(&c.cycle_id)
                || c.up > 1 << 60
                || c.down > 1 << 60
                || !seen.insert((&c.rule_id, &c.cycle_id))
            {
                return Err(invalid());
            }
            let record=one(&mut tx,"SELECT a.lease_id,c.id=l.current_cycle AS current FROM vp_allocations a JOIN vp_leases l ON l.id=a.lease_id JOIN vp_cycles c ON c.lease_id=l.id WHERE a.id=? AND a.node_id=? AND c.id=?",&[json!(c.rule_id),json!(node),json!(c.cycle_id)]).await.map_err(|_|forbidden())?;
            let cursors=rows(&mut tx,"SELECT up,down FROM vp_cursors WHERE node_id=? AND epoch=? AND rule_id=? AND cycle_id=?",&[json!(node),json!(r.epoch),json!(c.rule_id),json!(c.cycle_id)]).await?;
            let (up, down) = cursors
                .first()
                .map(|v| (n(v, "up") as u64, n(v, "down") as u64))
                .unwrap_or_default();
            if c.up < up || c.down < down {
                return Err(invalid());
            }
            let (du, dd) = (c.up - up, c.down - down);
            if du > 0 || dd > 0 {
                if exec(
                    &mut tx,
                    "UPDATE vp_cycles SET up=up+?,down=down+? WHERE id=? AND up<=? AND down<=?",
                    &[
                        json!(du),
                        json!(dd),
                        json!(c.cycle_id),
                        json!((1u64 << 61) - du),
                        json!((1u64 << 61) - dd),
                    ],
                )
                .await?
                    != 1
                {
                    return Err(invalid());
                }
                if b(&record, "current") {
                    let l = lease(&mut tx, s(&record, "lease_id")).await?;
                    let used = n(&l, "used_up") + n(&l, "used_down");
                    let limit = n(&l, "traffic_limit_bytes");
                    if limit > 0 && used >= limit && used - (du as i64) - (dd as i64) < limit {
                        changed = true;
                        audit(&mut tx, "traffic_limit", s(&l, "id"), "suspended").await?;
                    }
                }
            }
            exec(&mut tx,"INSERT INTO vp_cursors VALUES(?,?,?,?,?,?) ON DUPLICATE KEY UPDATE up=VALUES(up),down=VALUES(down)",&[json!(node),json!(r.epoch),json!(c.rule_id),json!(c.cycle_id),json!(c.up),json!(c.down)]).await?;
        }
        let mut probe = r.probe.clone();
        if r.probe.target_checks.is_some() || r.probe.rule_connections.is_some() {
            let targets = rows(
                &mut tx,
                &format!("{ALLOCATION} WHERE a.node_id=? AND a.released=0 AND a.target_host<>''"),
                &[json!(node)],
            )
            .await?;
            if let Some(counts) = &mut probe.rule_connections {
                let owned: HashSet<_> = targets.iter().map(|a| s(a, "id")).collect();
                counts.retain(|id, _| owned.contains(id.as_str()));
            }
            if let Some(checks) = &r.probe.target_checks {
                let mut wanted = HashSet::new();
                for a in targets {
                    let ts: Vec<Target> = if b(&a, "load_balance") {
                        serde_json::from_value(a["targets"].clone())?
                    } else {
                        vec![Target {
                            host: s(&a, "target_host").into(),
                            port: n(&a, "target_port") as u16,
                        }]
                    };
                    for t in ts {
                        wanted.insert((s(&a, "id").to_owned(), t.host, t.port));
                    }
                }
                let mut clean = Vec::new();
                for c in checks {
                    if !valid_id(&c.rule_id)
                        || !valid_target(&c.host, c.port)
                        || c.checked_at < 1
                        || c.checked_at > 253402300799
                        || !c.latency_ms.is_finite()
                        || !(0.0..=10000.0).contains(&c.latency_ms)
                        || ![
                            "ok",
                            "timeout",
                            "refused",
                            "unreachable",
                            "dns_error",
                            "blocked",
                        ]
                        .contains(&c.status.as_str())
                        || c.status != "ok" && c.latency_ms != 0.0
                    {
                        return Err(invalid());
                    }
                    if wanted.remove(&(c.rule_id.clone(), c.host.clone(), c.port)) {
                        clean.push(c.clone())
                    }
                }
                probe.target_checks = Some(clean);
            }
        }
        exec(
            &mut tx,
            "INSERT INTO vp_epochs VALUES(?,?,?) ON DUPLICATE KEY UPDATE sequence=VALUES(sequence)",
            &[json!(node), json!(r.epoch), json!(r.sequence)],
        )
        .await?;
        exec(&mut tx,"UPDATE vp_nodes SET last_seen=?,applied_revision=?,probe=?,errors=?,agent_version=?,kernel_version=?,active_rules=?,applied_rules=?,supports_limits=? WHERE id=?",&[json!(now()),json!(r.applied_revision),json!(probe),json!(r.errors),json!(r.agent_version),json!(r.kernel_version),r.active_rules.as_ref().map(|v|json!(v)).unwrap_or(Value::Null),r.applied_rules.as_ref().map(|v|json!(v)).unwrap_or(Value::Null),json!(r.supports_limits),json!(node)]).await?;
        if changed {
            bump(&mut tx).await?
        }
        tx.commit().await?;
        Ok(())
    }
}
