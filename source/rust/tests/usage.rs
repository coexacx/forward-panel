use serde_json::{Value, json};
use vistart_forward::{
    b, id, n, now,
    protocol::{Counter, Report},
    s,
    store::{Store, exec, rows},
};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_adjustment_preserves_cursors_cycles_and_permissions() {
    let Some(path) = std::env::var_os("VISTART_TEST_CONFIG") else {
        return;
    };
    let cfg: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(s(&cfg["mysql"], "name").ends_with("_qa"));
    let db = Store::open(&cfg["mysql"]).await.unwrap();
    let admin = id();
    let owner = id();
    let plan = id();
    let nodes = [id(), id()];
    for uid in [&admin, &owner] {
        db.command(json!({"action":"user","user":{"id":uid,"name":"Usage QA"}}))
            .await
            .unwrap();
        let mut c = db.pool.acquire().await.unwrap();
        exec(&mut c,"INSERT INTO vp_accounts(id,username,name,password_hash,role,created_at) VALUES(?,?,?,'not-a-login-hash',?,?)",
          &[json!(uid),json!(uid),json!("Usage QA"),json!(if uid==&admin {"admin"} else {"user"}),json!(now())]).await.unwrap();
    }
    for node in &nodes {
        db.command(json!({"action":"node","node":{"id":node,"name":"Usage QA","enabled":true},"token":"qa-unique-node-secret-used-only-in-isolated-tests"})).await.unwrap();
        db.command(json!({"action":"pool","pool":{"node_id":node,"public_ip":"1.1.1.1","bind_ip":"1.1.1.1","start":35000,"end":35010}})).await.unwrap();
    }
    db.command(json!({"action":"plan","plan":{"id":plan,"name":"Usage QA","port_limit":3,"traffic_limit_bytes":1000,"period_days":30,"price_cents":0,"reset_price_cents":0,"node_ids":nodes,"enabled":true}})).await.unwrap();
    let original = db
        .command(json!({"action":"grant","user_id":owner,"plan_id":plan}))
        .await
        .unwrap();
    let lid = s(&original, "id");
    let cycle = s(&original, "cycle_id");
    let mut rules = vec![];
    for node in &nodes {
        let rule=db.command(json!({"action":"claim","claim":{"user_id":owner,"lease_id":lid,"node_id":node,"public_ip":"1.1.1.1","port":35000}})).await.unwrap();
        db.command(json!({"action":"target","user_id":owner,"id":rule["id"],"host":"example.com","port":443})).await.unwrap();
        rules.push(rule);
    }
    let mut reports: Vec<Report> = rules
        .iter()
        .enumerate()
        .map(|(i, r)| Report {
            version: 1,
            epoch: id(),
            sequence: 1,
            counters: vec![Counter {
                rule_id: s(r, "id").into(),
                cycle_id: cycle.into(),
                up: if i == 0 { 100 } else { 25 },
                down: if i == 0 { 50 } else { 25 },
            }],
            ..Default::default()
        })
        .collect();
    for (i, node) in nodes.iter().enumerate() {
        db.report(node, &reports[i]).await.unwrap();
    }
    let edit = json!({"action":"adjust-lease-usage","actor_id":admin,"lease_id":lid,"expected_cycle":cycle,"used_bytes":500});
    let mut bad = edit.clone();
    bad["actor_id"] = json!(owner);
    assert!(db.command(bad).await.is_err());
    for value in [
        json!(-1),
        json!(1.5),
        json!("500"),
        Value::Null,
        json!(9_007_199_254_740_992u64),
    ] {
        let mut bad = edit.clone();
        bad["used_bytes"] = value;
        assert!(db.command(bad).await.is_err());
    }
    let mut bad = edit.clone();
    bad["expected_cycle"] = json!(id());
    assert!(db.command(bad).await.is_err());
    let out = db.command(edit.clone()).await.unwrap();
    assert_eq!(out["before_used_bytes"], 200);
    for (i, node) in nodes.iter().enumerate() {
        // A new sequence carrying the same old cumulative total adds nothing.
        reports[i].sequence += 1;
        db.report(node, &reports[i]).await.unwrap();
    }
    let get = || db.command(json!({"action":"lease","lease_id":lid}));
    let l = get().await.unwrap();
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 500);
    {
        let mut c = db.pool.acquire().await.unwrap();
        assert!(
            rows(
                &mut c,
                "SELECT id FROM vp_metadata WHERE id=?",
                &[json!(format!("lease_apply_{lid}"))]
            )
            .await
            .unwrap()
            .is_empty()
        );
    }
    assert_eq!((n(&l, "used_up"), n(&l, "used_down")), (312, 188));
    reports[0].sequence += 1;
    reports[0].counters[0].up += 30;
    reports[0].counters[0].down += 10;
    reports[1].sequence += 1;
    reports[1].counters[0].up += 5;
    reports[1].counters[0].down += 20;
    for (i, node) in nodes.iter().enumerate() {
        db.report(node, &reports[i]).await.unwrap();
        db.report(node, &reports[i]).await.unwrap();
    }
    let l = get().await.unwrap();
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 565);
    for field in [
        "cycle_id",
        "expires_at",
        "next_reset_at",
        "port_limit",
        "traffic_limit_bytes",
    ] {
        assert_eq!(l[field], original[field]);
    }
    for total in [1000, 9_007_199_254_740_991i64, 100] {
        let mut c = edit.clone();
        c["used_bytes"] = json!(total);
        db.command(c).await.unwrap();
        for node in &nodes {
            assert_eq!(
                db.rules(node).await.unwrap().rules.len(),
                if total >= 1000 { 0 } else { 1 }
            );
        }
    }
    {
        let mut c = db.pool.acquire().await.unwrap();
        let meta = rows(
            &mut c,
            "SELECT value FROM vp_metadata WHERE id=?",
            &[json!(format!("lease_apply_{lid}"))],
        )
        .await
        .unwrap();
        let value: Value = serde_json::from_str(s(&meta[0], "value")).unwrap();
        assert!(n(&value, "revision") > 0);
    }
    db.command(json!({"action":"pause","lease_id":lid,"paused":true}))
        .await
        .unwrap();
    let mut zero = edit.clone();
    zero["used_bytes"] = json!(0);
    db.command(zero).await.unwrap();
    let l = get().await.unwrap();
    assert!(b(&l, "manual_paused"));
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 0);
    assert!(db.rules(&nodes[0]).await.unwrap().rules.is_empty());
    {
        let mut c = db.pool.acquire().await.unwrap();
        assert!(
            !rows(
                &mut c,
                "SELECT id FROM vp_web_audit WHERE actor=? AND subject=? AND action='修改已用流量'",
                &[json!(admin), json!(owner)]
            )
            .await
            .unwrap()
            .is_empty()
        );
        exec(
            &mut c,
            "UPDATE vp_leases SET next_reset_at=? WHERE id=?",
            &[json!(now() - 1), json!(lid)],
        )
        .await
        .unwrap();
    }
    assert!(db.command(edit.clone()).await.is_err());
    db.tick().await.unwrap();
    assert_ne!(get().await.unwrap()["cycle_id"], original["cycle_id"]);
    assert!(db.command(edit).await.is_err());
    let fresh = get().await.unwrap();
    db.command(json!({"action":"end-lease","lease_id":lid}))
        .await
        .unwrap();
    assert!(db.command(json!({"action":"adjust-lease-usage","actor_id":admin,"lease_id":lid,"expected_cycle":fresh["cycle_id"],"used_bytes":0})).await.is_err());
}
