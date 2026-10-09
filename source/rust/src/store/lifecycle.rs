use super::*;

pub(super) async fn administrator(db: &mut MySqlConnection, c: &Value) -> Result<()> {
    if count(
        db,
        "SELECT COUNT(*) n FROM vp_accounts WHERE id=? AND role='admin' AND disabled=0",
        &[c["actor_id"].clone()],
    )
    .await?
        != 1
    {
        return Err(forbidden());
    }
    Ok(())
}

pub(super) async fn delete_plan(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    administrator(db, c).await?;
    let p = plan(db, s(c, "id")).await?;
    if b(&p, "deleted") {
        return Ok(json!({"ok":true}));
    }
    // Preserve order snapshots and paid leases. No future checkout/new grants.
    exec(
        db,
        "UPDATE vp_plans SET deleted=1,enabled=0 WHERE id=?",
        &[p["id"].clone()],
    )
    .await?;
    bump(db).await?;
    audit(
        db,
        "plan_deleted",
        s(&p, "id"),
        "catalogue removed; paid leases and orders retained",
    )
    .await?;
    Ok(json!({"ok":true}))
}

pub(super) async fn delete_lease(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    let l = lease(db, s(c, "id")).await?;
    if s(c, "action") == "admin-delete-lease" {
        administrator(db, c).await?;
    } else {
        if s(&l, "user_id") != s(c, "user_id") {
            return Err(forbidden());
        }
        if !b(&l, "ended") && n(&l, "expires_at") > now() {
            return Err(Fault {
                code: 409,
                message: "只能删除已结束或已到期的套餐",
            }
            .into());
        }
    }
    if b(&l, "deleted") {
        return Ok(json!({"ok":true}));
    }
    let rules = rows(
        db,
        "SELECT id FROM vp_allocations WHERE lease_id=?",
        &[l["id"].clone()],
    )
    .await?;
    exec(
        db,
        "UPDATE vp_allocations SET released=1,target_host='',target_port=0 WHERE lease_id=?",
        &[l["id"].clone()],
    )
    .await?;
    for r in rules {
        exec(
            db,
            "DELETE FROM vp_allocation_targets WHERE rule_id=?",
            &[r["id"].clone()],
        )
        .await?;
        exec(
            db,
            "DELETE FROM vp_metadata WHERE id=?",
            &[json!(format!("allocation_{}", s(&r, "id")))],
        )
        .await?;
    }
    exec(
        db,
        "UPDATE vp_leases SET deleted=1,ended=1,manual_paused=1 WHERE id=?",
        &[l["id"].clone()],
    )
    .await?;
    exec(
        db,
        "DELETE FROM vp_metadata WHERE id=?",
        &[json!(format!("lease_apply_{}", s(&l, "id")))],
    )
    .await?;
    // Keep small accounting records so in-flight/replayed counters remain idempotent.
    // A delayed paid renewal/reset is recorded as paid_review, never reactivated.
    bump(db).await?;
    audit(
        db,
        "lease_deleted",
        s(&l, "id"),
        "ports released; orders and accounting retained",
    )
    .await?;
    Ok(json!({"ok":true}))
}

pub(super) async fn preview_plan(db: &mut MySqlConnection, p: &Value) -> Result<Value> {
    let original = plan(db, s(p, "id")).await?;
    if b(&original, "deleted") || !(1..=500).contains(&n(p, "port_limit")) {
        return Err(invalid());
    }
    let policy: Limits = serde_json::from_value(p.clone()).map_err(|_| invalid())?;
    if !policy.valid() {
        return Err(invalid());
    }
    let ls = rows(
        db,
        &format!(
            "{LEASE} WHERE l.plan_id=? AND l.ended=0 AND l.deleted=0 ORDER BY l.id LIMIT 2001"
        ),
        &[p["id"].clone()],
    )
    .await?;
    if ls.len() > 2000 {
        return Err(Fault {
            code: 409,
            message: "受影响套餐超过 2000 份，请先整理后再强制更新",
        }
        .into());
    }
    let fields = [
        "plan_name",
        "port_limit",
        "traffic_limit_bytes",
        "period_days",
        "node_ids",
        "bandwidth_mbps",
        "tcp_limit",
        "udp_limit",
    ];
    let mut details = Vec::new();
    let mut basis = Vec::new();
    let mut users = HashSet::new();
    let mut released = 0usize;
    let mut total_rules = 0usize;
    let mut pausing = 0usize;
    let mut resuming = 0usize;
    let mut upgrades = Vec::new();
    if policy.enabled() {
        for nid in p["node_ids"].as_array().ok_or_else(invalid)? {
            let node = one(
                db,
                "SELECT id,name,supports_limits FROM vp_nodes WHERE id=? AND deleted=0",
                std::slice::from_ref(nid),
            )
            .await?;
            if !b(&node, "supports_limits") {
                upgrades.push(node);
            }
        }
    }
    for l in ls {
        let owner=one(db,"SELECT u.id,u.name,u.disabled,COALESCE(a.username,'') AS username FROM vp_users u LEFT JOIN vp_accounts a ON a.id=u.id WHERE u.id=?",&[l["user_id"].clone()]).await?;
        let allocations = rows(
            db,
            &format!("{ALLOCATION} WHERE a.lease_id=? AND a.released=0 ORDER BY a.port,a.id"),
            &[l["id"].clone()],
        )
        .await?;
        total_rules += allocations.len();
        if total_rules > 10_000 {
            return Err(Fault {
                code: 409,
                message: "受影响规则超过 10000 条，请先整理后再强制更新",
            }
            .into());
        }
        let mut kept = 0;
        let mut removing = Vec::new();
        for a in &allocations {
            let reason = if !in_ids(&p["node_ids"], s(a, "node_id")) {
                "node_removed"
            } else if kept >= n(p, "port_limit") {
                "port_quota"
            } else {
                kept += 1;
                continue;
            };
            let mut entry = a.clone();
            entry["reason"] = json!(reason);
            removing.push(entry);
        }
        let mut before = json!({});
        let mut after = json!({});
        for field in fields {
            before[field] = l[field].clone();
            after[field] = if field == "plan_name" {
                p["name"].clone()
            } else if ["bandwidth_mbps", "tcp_limit", "udp_limit"].contains(&field) {
                json!(n(p, field))
            } else {
                p[field].clone()
            };
        }
        let eligible =
            !b(&owner, "disabled") && !b(&l, "manual_paused") && n(&l, "expires_at") > now();
        let used = n(&l, "used_up") + n(&l, "used_down");
        let was_limited = n(&l, "traffic_limit_bytes") > 0 && used >= n(&l, "traffic_limit_bytes");
        let becomes_limited =
            n(p, "traffic_limit_bytes") > 0 && used >= n(p, "traffic_limit_bytes");
        let will_pause = eligible && !was_limited && becomes_limited;
        let will_resume = eligible && was_limited && !becomes_limited;
        released += removing.len();
        pausing += usize::from(will_pause);
        resuming += usize::from(will_resume);
        users.insert(s(&l, "user_id").to_owned());
        // Do not bind to constantly changing byte counters. Bind to their actual
        // pause/resume consequences, and to every structural billing/rule field.
        basis.push(json!({"id":l["id"],"owner":owner,"before":before,"allocations":allocations,
            "cycle_id":l["cycle_id"],"expires_at":l["expires_at"],"next_reset_at":l["next_reset_at"],
            "manual_paused":l["manual_paused"],"will_pause":will_pause,"will_resume":will_resume}));
        details.push(json!({"id":l["id"],"user_id":l["user_id"],"user_name":owner["name"],"username":owner["username"],
            "before":before,"after":after,"release_rules":removing,"kept_rules":kept,
            "will_pause":will_pause,"will_resume":will_resume,"used_bytes":used,
            "expires_at":l["expires_at"],"next_reset_at":l["next_reset_at"]}));
    }
    let mut requested = p.clone();
    for key in ["bandwidth_mbps", "tcp_limit", "udp_limit"] {
        requested[key] = json!(n(p, key));
    }
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(
        &json!({"plan":original,"requested":requested,"leases":basis,"upgrades":upgrades}),
    )?));
    Ok(
        json!({"digest":digest,"plan":original,"requested":requested,"leases":details,"upgrade_nodes":upgrades,
        "users_count":users.len(),"leases_count":details.len(),"release_count":released,
        "pause_count":pausing,"resume_count":resuming}),
    )
}

pub(super) async fn sync_leases(
    db: &mut MySqlConnection,
    p: &Value,
    preview: &Value,
) -> Result<()> {
    for entry in preview["leases"].as_array().ok_or_else(invalid)? {
        for a in entry["release_rules"].as_array().ok_or_else(invalid)? {
            exec(
                db,
                "UPDATE vp_allocations SET released=1,target_host='',target_port=0 WHERE id=?",
                &[a["id"].clone()],
            )
            .await?;
            exec(
                db,
                "DELETE FROM vp_allocation_targets WHERE rule_id=?",
                &[a["id"].clone()],
            )
            .await?;
            exec(
                db,
                "DELETE FROM vp_metadata WHERE id=?",
                &[json!(format!("allocation_{}", s(a, "id")))],
            )
            .await?;
        }
        exec(db,"UPDATE vp_leases SET plan_name=?,port_limit=?,traffic_limit=?,period_days=?,node_ids=?,bandwidth_mbps=?,tcp_limit=?,udp_limit=? WHERE id=?",
            &[p["name"].clone(),p["port_limit"].clone(),p["traffic_limit_bytes"].clone(),p["period_days"].clone(),p["node_ids"].clone(),
            json!(n(p,"bandwidth_mbps")),json!(n(p,"tcp_limit")),json!(n(p,"udp_limit")),entry["id"].clone()]).await?;
        audit(
            db,
            "lease_plan_synced",
            s(entry, "id"),
            "reviewed update; expiry, natural reset and consumed traffic retained",
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn edit_node(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    administrator(db, c).await?;
    let v = &c["node"];
    let nid = s(v, "id");
    if !valid_id(nid) || !name_ok(s(v, "name")) || !name_ok(s(v, "region")) {
        return Err(invalid());
    }
    let public = s(v, "public_ip")
        .parse::<std::net::IpAddr>()
        .map_err(|_| invalid())?;
    // Private/interconnect addresses are valid. Never infer reachability from public Internet.
    if public.is_unspecified() || public.is_multicast() || public.is_loopback() {
        return Err(invalid());
    }
    one(
        db,
        "SELECT id FROM vp_nodes WHERE id=? AND deleted=0",
        &[json!(nid)],
    )
    .await?;
    if count(
        db,
        "SELECT COUNT(*) n FROM vp_node_removals WHERE node_id=?",
        &[json!(nid)],
    )
    .await?
        > 0
    {
        return Err(conflict());
    }
    let duplicate=rows(db,"SELECT start_port,COUNT(*) n FROM vp_pools WHERE node_id=? GROUP BY start_port HAVING COUNT(*)>1",&[json!(nid)]).await?;
    if !duplicate.is_empty() {
        return Err(Fault {
            code: 409,
            message: "此节点有多个地址的同起始端口池，请先整理端口池",
        }
        .into());
    }
    let saved = rows(
        db,
        "SELECT value FROM vp_metadata WHERE id=? FOR UPDATE",
        &[json!(nid)],
    )
    .await?;
    let mut meta: Value = saved
        .first()
        .map(|v| serde_json::from_str(s(v, "value")))
        .transpose()?
        .unwrap_or(json!({}));
    meta["public_ip"] = json!(public.to_string());
    meta["region"] = v["region"].clone();
    meta["name"] = v["name"].clone();
    exec(
        db,
        "UPDATE vp_nodes SET name=?,enabled=? WHERE id=?",
        &[v["name"].clone(), json!(b(v, "enabled")), json!(nid)],
    )
    .await?;
    exec(
        db,
        "UPDATE vp_pools SET public_ip=? WHERE node_id=?",
        &[json!(public.to_string()), json!(nid)],
    )
    .await?;
    exec(
        db,
        "UPDATE vp_allocations SET public_ip=? WHERE node_id=? AND released=0",
        &[json!(public.to_string()), json!(nid)],
    )
    .await?;
    exec(
        db,
        "INSERT INTO vp_metadata(id,value) VALUES(?,?) ON DUPLICATE KEY UPDATE value=VALUES(value)",
        &[json!(nid), meta],
    )
    .await?;
    bump(db).await?;
    Ok(json!({"ok":true}))
}

/// Adjust this billing cycle's aggregate; cumulative Agent cursors stay intact.
pub(super) async fn adjust_usage(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    administrator(db, c).await?;
    let total = c["used_bytes"]
        .as_i64()
        .filter(|n| (0..=MAX_USAGE_BYTES).contains(n))
        .ok_or_else(invalid)?;
    if !valid_id(s(c, "lease_id")) || !valid_id(s(c, "expected_cycle")) {
        return Err(invalid());
    }
    // Store::command serializes this transaction with reports and natural resets.
    // Row locks also protect against a second process writing the same cycle.
    let l = one(
        db,
        &format!("{LEASE} WHERE l.id=? FOR UPDATE"),
        &[c["lease_id"].clone()],
    )
    .await?;
    if b(&l, "deleted") {
        return Err(missing());
    }
    if b(&l, "ended") || n(&l, "expires_at") <= now() {
        return Err(Fault {
            code: 409,
            message: "套餐已结束或到期，不能调整已用流量",
        }
        .into());
    }
    if l["cycle_id"] != c["expected_cycle"] || n(&l, "next_reset_at") <= now() {
        return Err(Fault {
            code: 409,
            message: "流量周期已变化，请刷新套餐后重试",
        }
        .into());
    }
    let old_up = n(&l, "used_up");
    let old_down = n(&l, "used_down");
    let old_total = old_up + old_down;
    // Preserve the existing accounting split without floating point rounding.
    let up = if old_total > 0 {
        ((total as i128 * old_up as i128) / old_total as i128) as i64
    } else {
        0
    };
    let down = total - up;
    exec(
        db,
        "UPDATE vp_cycles SET up=?,down=? WHERE id=?",
        &[json!(up), json!(down), l["cycle_id"].clone()],
    )
    .await?;
    bump(db).await?;
    let limit = n(&l, "traffic_limit_bytes");
    if limit > 0 && (old_total >= limit) != (total >= limit) {
        lease_dispatch_barrier(db, s(&l, "id")).await?;
    }
    audit(
        db,
        "usage_adjusted",
        s(&l, "id"),
        &json!({
            "actor_id":c["actor_id"],"cycle_id":l["cycle_id"],
            "before_up":old_up,"before_down":old_down,"after_up":up,"after_down":down
        })
        .to_string(),
    )
    .await?;
    // The visible audit event commits atomically with the adjustment.
    exec(
        db,
        "INSERT INTO vp_web_audit(at,actor,subject,action,detail) VALUES(?,?,?,?,?)",
        &[
            json!(now()),
            c["actor_id"].clone(),
            l["user_id"].clone(),
            json!("修改已用流量"),
            json!(format!(
                "套餐 {}：{} B → {} B；周期 {}",
                s(&l, "id"),
                old_total,
                total,
                s(&l, "cycle_id")
            )),
        ],
    )
    .await?;
    Ok(json!({"ok":true,"before_used_bytes":old_total,"used_bytes":total,"cycle_id":l["cycle_id"]}))
}
