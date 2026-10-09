use super::*;
pub(super) async fn grant(db: &mut MySqlConnection, user: &str, p: &Value) -> Result<Value> {
    one(db, "SELECT id FROM vp_users WHERE id=?", &[json!(user)]).await?;
    let mut p = p.clone();
    let mut valid_nodes = Vec::new();
    for node in p["node_ids"].as_array().ok_or_else(invalid)? {
        if count(
            db,
            "SELECT COUNT(*) n FROM vp_nodes WHERE id=? AND deleted=0",
            std::slice::from_ref(node),
        )
        .await?
            == 1
        {
            valid_nodes.push(node.clone());
        }
    }
    if valid_nodes.is_empty() {
        return Err(quota());
    }
    p["node_ids"] = json!(valid_nodes);
    let lid = id();
    let cycle = id();
    let at = now() + n(&p, "period_days") * 86400;
    exec(
        db,
        "INSERT INTO vp_leases(id,user_id,plan_id,plan_name,port_limit,traffic_limit,period_days,node_ids,expires_at,next_reset_at,current_cycle,manual_paused,ended) VALUES(?,?,?,?,?,?,?,?,?,?,?,0,0)",
        &[
            json!(lid),
            json!(user),
            p["id"].clone(),
            p["name"].clone(),
            p["port_limit"].clone(),
            p["traffic_limit_bytes"].clone(),
            p["period_days"].clone(),
            p["node_ids"].clone(),
            json!(at),
            json!(at),
            json!(cycle),
        ],
    )
    .await?;
    exec(
        db,
        "INSERT INTO vp_cycles(id,lease_id,started_at,reason) VALUES(?,?,?,'activation')",
        &[json!(cycle), json!(lid), json!(now())],
    )
    .await?;
    lease(db, &lid).await
}
async fn rotate(db: &mut MySqlConnection, l: &Value, reason: &str) -> Result<()> {
    let cycle = id();
    exec(
        db,
        "UPDATE vp_cycles SET ended_at=? WHERE id=?",
        &[json!(now()), l["cycle_id"].clone()],
    )
    .await?;
    exec(
        db,
        "INSERT INTO vp_cycles(id,lease_id,started_at,reason) VALUES(?,?,?,?)",
        &[json!(cycle), l["id"].clone(), json!(now()), json!(reason)],
    )
    .await?;
    exec(
        db,
        "UPDATE vp_leases SET current_cycle=? WHERE id=?",
        &[json!(cycle), l["id"].clone()],
    )
    .await?;
    Ok(())
}
pub(super) async fn tick(db: &mut MySqlConnection) -> Result<()> {
    let due = rows(
        db,
        &format!(
            "{LEASE} WHERE l.deleted=0 AND l.ended=0 AND l.next_reset_at<=? AND l.next_reset_at<=l.expires_at"
        ),
        &[json!(now())],
    )
    .await?;
    if due.is_empty() {
        return Ok(());
    }
    for l in due {
        let end = now().min(n(&l, "expires_at"));
        let period = n(&l, "period_days") * 86400;
        if period <= 0 {
            bail!("invalid stored billing period")
        }
        let next = n(&l, "next_reset_at") + ((end - n(&l, "next_reset_at")) / period + 1) * period;
        rotate(db, &l, "natural").await?;
        exec(
            db,
            "UPDATE vp_leases SET next_reset_at=? WHERE id=?",
            &[json!(next), l["id"].clone()],
        )
        .await?;
    }
    bump(db).await
}
pub(super) async fn new_order(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    let user = s(c, "user_id");
    let pid = s(c, "plan_id");
    let kind = s(c, "kind");
    if !["purchase", "reset"].contains(&kind) {
        return Err(invalid());
    }
    if count(
        db,
        "SELECT COUNT(*) AS n FROM vp_users WHERE id=? AND disabled=0",
        &[json!(user)],
    )
    .await?
        != 1
    {
        return Err(forbidden());
    }
    if count(
        db,
        "SELECT COUNT(*) AS n FROM vp_orders WHERE user_id=? AND status='pending'",
        &[json!(user)],
    )
    .await?
        >= 20
    {
        return Err(Fault {
            code: 409,
            message: "too many pending orders",
        }
        .into());
    }
    let p = plan(db, pid).await?;
    if !b(&p, "enabled") || b(&p, "deleted") {
        return Err(forbidden());
    }
    let mut lid = s(c, "lease_id").to_owned();
    if lid.is_empty() {
        let ls = rows(
            db,
            "SELECT id FROM vp_leases WHERE user_id=? AND plan_id=? AND ended=0",
            &[json!(user), json!(pid)],
        )
        .await?;
        if ls.len() > 1 {
            return Err(conflict());
        }
        if let Some(l) = ls.first() {
            lid = s(l, "id").into()
        }
    }
    let (mut op, mut amount) = ("new", n(&p, "price_cents"));
    if !lid.is_empty() {
        let l = lease(db, &lid).await?;
        if s(&l, "user_id") != user || s(&l, "plan_id") != pid || b(&l, "ended") {
            return Err(forbidden());
        }
        op = "renew";
        if kind == "reset" {
            if n(&l, "expires_at") <= now() {
                return Err(forbidden());
            }
            op = "reset";
            amount = n(&p, "reset_price_cents");
        }
    } else if kind == "reset" {
        return Err(invalid());
    }
    let oid = id();
    exec(
        db,
        "INSERT INTO vp_orders VALUES(?,?,?,?,?,?,'pending',?,0,?)",
        &[
            json!(oid),
            json!(user),
            json!(pid),
            json!(lid),
            json!(op),
            json!(amount),
            json!(now()),
            p,
        ],
    )
    .await?;
    order(db, &oid).await
}
async fn settle(db: &mut MySqlConnection, mut o: Value) -> Result<Value> {
    if !["new", "renew", "reset"].contains(&s(&o, "kind")) {
        return Err(invalid());
    }
    if s(&o, "kind") == "new" {
        let existing=rows(db,"SELECT id FROM vp_leases WHERE user_id=? AND plan_id=? AND ended=0 ORDER BY expires_at DESC LIMIT 1",&[o["user_id"].clone(),o["plan_id"].clone()]).await?;
        if let Some(l) = existing.first() {
            o["kind"] = json!("renew");
            o["lease_id"] = l["id"].clone();
        }
    }
    o["status"] = json!("paid");
    let current_plan = plan(db, s(&o, "plan_id")).await?;
    if b(&current_plan, "deleted") {
        // A valid delayed payment still needs a durable receipt, never a revived package.
        o["status"] = json!("paid_review");
    } else if s(&o, "kind") == "new" {
        match grant(db, s(&o, "user_id"), &o["snapshot"]).await {
            Ok(l) => o["lease_id"] = l["id"].clone(),
            Err(e)
                if e.downcast_ref::<Fault>()
                    .is_some_and(|f| f.message == "package quota exhausted") =>
            {
                o["status"] = json!("paid_review")
            }
            Err(e) => return Err(e),
        }
    } else {
        let l = lease(db, s(&o, "lease_id")).await?;
        if b(&l, "ended") || (s(&o, "kind") == "reset" && n(&l, "expires_at") <= now()) {
            o["status"] = json!("paid_review");
        } else if s(&o, "kind") == "renew" {
            exec(
                db,
                "UPDATE vp_leases SET expires_at=? WHERE id=?",
                &[
                    json!(
                        now().max(n(&l, "expires_at")) + n(&o["snapshot"], "period_days") * 86400
                    ),
                    l["id"].clone(),
                ],
            )
            .await?;
        } else {
            rotate(db, &l, "paid_reset").await?;
        }
    }
    o["paid_at"] = json!(now());
    exec(
        db,
        "UPDATE vp_orders SET status=?,paid_at=?,lease_id=?,kind=? WHERE id=?",
        &[
            o["status"].clone(),
            o["paid_at"].clone(),
            o["lease_id"].clone(),
            o["kind"].clone(),
            o["id"].clone(),
        ],
    )
    .await?;
    audit(
        db,
        &format!("payment_{}", s(&o, "kind")),
        s(&o, "id"),
        s(&o, "status"),
    )
    .await?;
    bump(db).await?;
    Ok(o)
}
pub(super) async fn manage(db: &mut MySqlConnection, c: &Value) -> Result<Value> {
    let actor = s(c, "actor_id");
    let cid = s(c, "id");
    let status = s(c, "status");
    let note = s(c, "note").trim();
    if !valid_id(actor)
        || !valid_id(cid)
        || !["paid", "cancelled"].contains(&status)
        || note.is_empty()
        || note.chars().count() > 200
    {
        return Err(invalid());
    }
    // Defense in depth: the Unix API also verifies the administrator role.
    if count(
        db,
        "SELECT COUNT(*) AS n FROM vp_accounts WHERE id=? AND role='admin' AND disabled=0",
        &[json!(actor)],
    )
    .await?
        != 1
    {
        return Err(forbidden());
    }
    let mut o = order(db, cid).await?;
    let before = s(&o, "status").to_owned();
    if before == status || (status == "paid" && before == "paid_review") {
        return Ok(o);
    }
    if status == "cancelled" {
        if before != "pending" {
            return Err(conflict());
        }
        exec(
            db,
            "UPDATE vp_orders SET status='cancelled' WHERE id=?",
            &[json!(cid)],
        )
        .await?;
        o["status"] = json!("cancelled");
        bump(db).await?;
    } else {
        if !["pending", "cancelled"].contains(&before.as_str()) {
            return Err(conflict());
        }
        o = settle(db, o).await?;
        exec(
            db,
            "INSERT INTO vp_payment_events VALUES('admin',?,?,?,?)",
            &[
                json!(format!("admin_{cid}")),
                json!(cid),
                o["amount_cents"].clone(),
                json!(now()),
            ],
        )
        .await?;
    }
    audit(
        db,
        "order_admin",
        cid,
        &json!({"actor":actor,"from":before,"to":o["status"],"note":note}).to_string(),
    )
    .await?;
    Ok(o)
}
impl Store {
    pub async fn payment(
        &self,
        provider: &str,
        transaction: &str,
        oid: &str,
        cents: i64,
    ) -> Result<()> {
        if !(provider == "epay" || provider == "free" && cents == 0)
            || !valid_id(transaction)
            || !valid_id(oid)
            || cents < 0
        {
            return Err(invalid());
        }
        let _g = self.writer.lock().await;
        let mut tx = self.pool.begin().await?;
        tick(&mut tx).await?;
        let o = order(&mut tx, oid).await?;
        if n(&o, "amount_cents") != cents {
            return Err(invalid());
        }
        let prior=rows(&mut tx,"SELECT order_id,amount_cents FROM vp_payment_events WHERE provider=? AND transaction_id=?",&[json!(provider),json!(transaction)]).await?;
        if let Some(p) = prior.first() {
            if s(p, "order_id") == oid && n(p, "amount_cents") == cents {
                tx.commit().await?;
                return Ok(());
            }
            return Err(conflict());
        }
        if s(&o, "status") != "pending" {
            if provider!="epay"||!["paid","paid_review"].contains(&s(&o,"status"))||count(&mut tx,"SELECT COUNT(*) AS n FROM vp_payment_events WHERE provider='admin' AND order_id=?",&[json!(oid)]).await?!=1{return Err(conflict())}
            exec(&mut tx,"UPDATE vp_payment_events SET provider=?,transaction_id=?,received_at=? WHERE order_id=? AND provider='admin'",&[json!(provider),json!(transaction),json!(now()),json!(oid)]).await?;
            audit(&mut tx, "payment_verified", oid, provider).await?;
        } else {
            settle(&mut tx, o).await?;
            exec(
                &mut tx,
                "INSERT INTO vp_payment_events VALUES(?,?,?,?,?)",
                &[
                    json!(provider),
                    json!(transaction),
                    json!(oid),
                    json!(cents),
                    json!(now()),
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
