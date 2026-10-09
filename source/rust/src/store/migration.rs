use super::*;
fn problem(message: &'static str) -> anyhow::Error {
    Fault { code: 409, message }.into()
}
pub(super) async fn export(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    lifecycle::administrator(db, c).await?;
    let nid = s(c, "node_id");
    let node = one(
        db,
        "SELECT id,name FROM vp_nodes WHERE id=? AND deleted=0",
        &[json!(nid)],
    )
    .await?;
    let mut allocations = rows(
        db,
        &format!("{ALLOCATION} WHERE a.node_id=? AND a.released=0 ORDER BY a.id"),
        &[json!(nid)],
    )
    .await?;
    if allocations.len() > 512 {
        return Err(invalid());
    }
    for a in &mut allocations {
        let meta = rows(
            db,
            "SELECT value FROM vp_metadata WHERE id=?",
            &[json!(format!("allocation_{}", s(a, "id")))],
        )
        .await?;
        a["remark"] = meta
            .first()
            .and_then(|v| serde_json::from_str::<Value>(s(v, "value")).ok())
            .map(|v| v["remark"].clone())
            .unwrap_or(json!(""));
    }
    Ok(
        json!({"format":"vistart-forward-rules","schema":1,"source_node":node,"exported_at":now(),"rules":allocations}),
    )
}
fn replace(ids: &Value, source: &str, target: &str) -> Value {
    let mut out: Vec<Value> = ids
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|v| v.as_str() != Some(source))
        .collect();
    if !out.iter().any(|v| v.as_str() == Some(target)) {
        out.push(json!(target))
    }
    json!(out)
}
pub(super) async fn migrate(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    lifecycle::administrator(db, c).await?;
    let source = s(&c["file"]["source_node"], "id");
    let target = s(c, "node_id");
    if !valid_id(source) || !valid_id(target) {
        return Err(invalid());
    }
    let source_node = one(
        db,
        "SELECT id,deleted FROM vp_nodes WHERE id=?",
        &[json!(source)],
    )
    .await?;
    let restoring = b(&source_node, "deleted");
    let source_rules = c["file"]["rules"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or_else(invalid)?;
    let file_key = format!("migration_{}", s(c, "file_hash"));
    if source != target
        && count(
            db,
            "SELECT COUNT(*) n FROM vp_metadata WHERE id=?",
            &[json!(file_key)],
        )
        .await?
            > 0
    {
        return Err(problem("此文件已迁移，请从当前服务器重新导出规则"));
    }
    if !restoring {
        let live = export(db, &json!({"node_id":source,"actor_id":c["actor_id"]})).await?;
        let normalize = |rules: &Value| {
            let mut rules = rules.clone();
            for r in rules.as_array_mut().unwrap() {
                r.as_object_mut().unwrap().remove("public_ip");
                r.as_object_mut().unwrap().remove("bind_ip");
            }
            rules
        };
        if normalize(&live["rules"]) != normalize(&c["file"]["rules"]) {
            return Err(problem("原服务器规则已改变，请重新导出后再导入"));
        }
    }
    let mut source_ids = HashSet::new();
    let mut lease_ids = HashSet::new();
    for r in source_rules {
        if !valid_id(s(r, "id")) || !source_ids.insert(s(r, "id").to_owned()) {
            return Err(invalid());
        }
        if source != target
            && count(
                db,
                "SELECT COUNT(*) n FROM vp_metadata WHERE id=?",
                &[json!(format!("moved_{}", s(r, "id")))],
            )
            .await?
                > 0
        {
            return Err(problem("规则已经迁移，请从当前服务器重新导出"));
        }
        let old = allocation(db, s(r, "id")).await?;
        if s(&old, "node_id") != source || old["lease_id"] != r["lease_id"] {
            return Err(forbidden());
        }
        let l = lease(db, s(r, "lease_id")).await?;
        if b(&l, "deleted") || b(&l, "ended") {
            return Err(problem("关联用户套餐已删除或结束，不能恢复"));
        }
        lease_ids.insert(s(&l, "id").to_owned());
    }
    if count(db,"SELECT COUNT(*) n FROM vp_nodes n WHERE id=? AND deleted=0 AND enabled=1 AND last_seen>? AND NOT EXISTS(SELECT 1 FROM vp_node_removals d WHERE d.node_id=n.id)",&[json!(target),json!(now()-15)]).await?!=1 {
        return Err(problem("新服务器必须已部署、启用且在线"))
    }
    let used = rows(
        db,
        "SELECT id,bind_ip,port FROM vp_allocations WHERE node_id=? AND released=0",
        &[json!(target)],
    )
    .await?;
    let used: Vec<Value> = used
        .into_iter()
        .filter(|v| source != target || !source_ids.contains(s(v, "id")))
        .collect();
    if used.len() + source_rules.len() > 512 {
        return Err(problem("目标节点规则数量超出上限"));
    }
    let mut taken: HashSet<(String, i64)> = used
        .iter()
        .map(|v| (s(v, "bind_ip").into(), n(v, "port")))
        .collect();
    let mut occupied: HashSet<i64> = c["occupied_ports"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|v| v.as_i64().unwrap_or(0))
        .collect();
    if source == target {
        let node = one(
            db,
            "SELECT active_rules FROM vp_nodes WHERE id=?",
            &[json!(target)],
        )
        .await?;
        for r in source_rules {
            if node["active_rules"]
                .as_array()
                .is_some_and(|v| v.contains(&r["id"]))
            {
                occupied.remove(&n(r, "port"));
            }
        }
    }
    let pools=rows(db,"SELECT public_ip,bind_ip,start_port,end_port FROM vp_pools WHERE node_id=? ORDER BY public_ip,start_port",&[json!(target)]).await?;
    let mut restoring_counts = std::collections::HashMap::<String, i64>::new();
    for r in source_rules {
        if b(&allocation(db, s(r, "id")).await?, "released") {
            *restoring_counts.entry(s(r, "lease_id").into()).or_default() += 1;
        }
    }
    for (lid, extra) in restoring_counts {
        let l = lease(db, &lid).await?;
        if n(&l, "used_ports") + extra > n(&l, "port_limit") {
            return Err(problem("用户套餐剩余端口额度不足，请先调整额度再恢复"));
        }
    }
    let mut entries = Vec::new();
    for a in source_rules {
        let mut selected = None;
        for same in [true, false] {
            for p in &pools {
                for port in n(p, "start_port")..=n(p, "end_port") {
                    if (same && port != n(a, "port"))
                        || occupied.contains(&port)
                        || taken.iter().any(|(ip, used_port)| {
                            *used_port == port && crate::listen_overlap(ip, s(p, "bind_ip"))
                        })
                    {
                        continue;
                    }
                    selected = Some((
                        s(p, "public_ip").to_owned(),
                        s(p, "bind_ip").to_owned(),
                        port,
                    ));
                    break;
                }
                if selected.is_some() {
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let (public, bind, port) =
            selected.ok_or_else(|| problem("目标端口池没有足够可用端口，原规则尚未改变"))?;
        taken.insert((bind.clone(), port));
        entries.push(json!({"old_id":a["id"],"lease_id":a["lease_id"],"old_ip":a["public_ip"],"old_port":a["port"],"public_ip":public,"bind_ip":bind,"port":port,"target_host":a["target_host"],"target_port":a["target_port"],"remark":a["remark"]}));
    }
    let leases = rows(db, &format!("{LEASE} WHERE l.deleted=0"), &[])
        .await?
        .into_iter()
        .filter(|l| in_ids(&l["node_ids"], source) || lease_ids.contains(s(l, "id")))
        .collect::<Vec<_>>();
    let plan_ids: HashSet<String> = leases.iter().map(|l| s(l, "plan_id").to_owned()).collect();
    let plans = rows(db, &format!("{PLAN} WHERE deleted=0"), &[])
        .await?
        .into_iter()
        .filter(|p| in_ids(&p["node_ids"], source) || (restoring && plan_ids.contains(s(p, "id"))))
        .collect::<Vec<_>>();
    let preview = json!({"source_id":source,"node_id":target,"entries":entries,
        "leases":leases.iter().map(|v|json!({"id":v["id"],"node_ids":v["node_ids"]})).collect::<Vec<_>>(),
        "plans":plans.iter().map(|v|json!({"id":v["id"],"node_ids":v["node_ids"]})).collect::<Vec<_>>()});
    if s(c, "action") == "preview-node-migration" {
        return Ok(preview);
    }
    if c["preview"] != preview {
        return Err(problem("端口占用或套餐绑定已改变，请重新预览后确认"));
    }
    for l in &leases {
        exec(
            db,
            "UPDATE vp_leases SET node_ids=? WHERE id=?",
            &[replace(&l["node_ids"], source, target), l["id"].clone()],
        )
        .await?;
    }
    for p in &plans {
        exec(
            db,
            "UPDATE vp_plans SET node_ids=? WHERE id=?",
            &[replace(&p["node_ids"], source, target), p["id"].clone()],
        )
        .await?;
    }
    let mut moved = Vec::new();
    for (old, entry) in source_rules.iter().zip(&entries) {
        let rid = if source == target {
            s(old, "id").to_owned()
        } else {
            id()
        };
        if source == target {
            exec(
                db,
                "UPDATE vp_allocations SET public_ip=?,bind_ip=?,port=? WHERE id=?",
                &[
                    entry["public_ip"].clone(),
                    entry["bind_ip"].clone(),
                    entry["port"].clone(),
                    json!(rid),
                ],
            )
            .await?;
        } else {
            exec(
                db,
                "UPDATE vp_allocations SET released=1,target_host='',target_port=0 WHERE id=?",
                &[old["id"].clone()],
            )
            .await?;
            exec(db,"INSERT INTO vp_allocations(id,lease_id,node_id,public_ip,bind_ip,port,target_host,target_port) VALUES(?,?,?,?,?,?,?,?)",&[
                json!(rid),old["lease_id"].clone(),json!(target),entry["public_ip"].clone(),entry["bind_ip"].clone(),entry["port"].clone(),old["target_host"].clone(),old["target_port"].clone()]).await?;
            if b(old, "load_balance") {
                exec(
                    db,
                    "INSERT INTO vp_allocation_targets(rule_id,targets) VALUES(?,?)",
                    &[json!(rid), old["targets"].clone()],
                )
                .await?;
            }
            exec(
                db,
                "INSERT INTO vp_metadata(id,value) VALUES(?,?)",
                &[
                    json!(format!("allocation_{rid}")),
                    json!({"remark":old["remark"]}),
                ],
            )
            .await?;
            exec(
                db,
                "DELETE FROM vp_allocation_targets WHERE rule_id=?",
                &[old["id"].clone()],
            )
            .await?;
            exec(
                db,
                "DELETE FROM vp_metadata WHERE id=?",
                &[json!(format!("allocation_{}", s(old, "id")))],
            )
            .await?;
        }
        if source != target {
            exec(
                db,
                "INSERT INTO vp_metadata(id,value) VALUES(?,?)",
                &[
                    json!(format!("moved_{}", s(old, "id"))),
                    json!({"id":rid,"node_id":target,"at":now()}),
                ],
            )
            .await?;
        }
        let mut e = entry.clone();
        e["id"] = json!(rid);
        moved.push(e);
    }
    if source != target {
        exec(
            db,
            "INSERT INTO vp_metadata(id,value) VALUES(?,?)",
            &[json!(file_key), json!({"node_id":target,"at":now()})],
        )
        .await?;
    }
    bump(db).await?;
    audit(
        db,
        "node_rules_migrated",
        source,
        &format!("target={target}; rules={}", moved.len()),
    )
    .await?;
    Ok(json!({"ok":true,"entries":moved,"source_id":source,"node_id":target}))
}
