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

pub(super) async fn sync_leases(db: &mut MySqlConnection, p: &Value) -> Result<()> {
    let ls = rows(
        db,
        &format!("{LEASE} WHERE l.plan_id=? AND l.ended=0 AND l.deleted=0"),
        &[p["id"].clone()],
    )
    .await?;
    for l in ls {
        let allocations=rows(db,"SELECT id,node_id,port FROM vp_allocations WHERE lease_id=? AND released=0 ORDER BY port,id",&[l["id"].clone()]).await?;
        let mut kept = 0;
        for a in allocations {
            if in_ids(&p["node_ids"], s(&a, "node_id")) && kept < n(p, "port_limit") {
                kept += 1;
            } else {
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
                    &[json!(format!("allocation_{}", s(&a, "id")))],
                )
                .await?;
            }
        }
        exec(db,"UPDATE vp_leases SET plan_name=?,port_limit=?,traffic_limit=?,period_days=?,node_ids=? WHERE id=?",&[p["name"].clone(),p["port_limit"].clone(),p["traffic_limit_bytes"].clone(),p["period_days"].clone(),p["node_ids"].clone(),l["id"].clone()]).await?;
        audit(
            db,
            "lease_plan_synced",
            s(&l, "id"),
            "configuration updated; expiry, next reset and consumed traffic retained",
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
