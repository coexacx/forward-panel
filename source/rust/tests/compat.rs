use serde_json::{Value, json};
use std::sync::Arc;
use vistart_forward::{
    id, n, now,
    protocol::{Counter, Probe, Report},
    s,
    store::{Store, exec, rows},
};
async fn command(db: &Arc<Store>, c: Value) -> Value {
    db.command(c).await.unwrap()
}
fn report(epoch: &str, seq: u64, rule: &str, cycle: &str, up: u64, down: u64) -> Report {
    Report {
        version: 1,
        epoch: epoch.into(),
        sequence: seq,
        agent_version: "0.3.0".into(),
        kernel_version: "0.3.0".into(),
        counters: vec![Counter {
            rule_id: rule.into(),
            cycle_id: cycle.into(),
            up,
            down,
        }],
        probe: Probe::default(),
        ..Default::default()
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_business_and_protocol_parity() {
    let Some(path) = std::env::var_os("VISTART_TEST_CONFIG") else {
        eprintln!(
            "MySQL integration skipped; set VISTART_TEST_CONFIG to an isolated *_qa database"
        );
        return;
    };
    let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(s(&config["mysql"], "name").ends_with("_qa"));
    let db = Store::open(&config["mysql"]).await.unwrap();
    {
        let mut conn = db.pool.acquire().await.unwrap();
        for stmt in include_str!("../src/store/web.sql")
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            exec(&mut conn, stmt, &[]).await.unwrap();
        }
    }
    let u = id();
    let other = id();
    let node1 = id();
    let node2 = id();
    let plan = id();
    command(
        &db,
        json!({"action":"user","user":{"id":u,"name":"QA admin","disabled":false}}),
    )
    .await;
    command(
        &db,
        json!({"action":"user","user":{"id":other,"name":"QA tenant","disabled":false}}),
    )
    .await;
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(&mut c,"INSERT INTO vp_accounts(id,username,name,password_hash,role,created_at) VALUES(?,?,?,'not-a-login-hash','admin',?)",&[json!(u),json!(u),json!("QA"),json!(now())]).await.unwrap();
    }
    for node in [&node1, &node2] {
        command(&db,json!({"action":"node","node":{"id":node,"name":"QA node","enabled":true},"token":"0123456789012345678901234567890123456789012345678901234567890123"})).await;
        command(&db,json!({"action":"pool","pool":{"node_id":node,"public_ip":"1.1.1.1","bind_ip":"1.1.1.1","start":32000,"end":32004}})).await;
    }
    assert!(
        db.authenticate(
            &node1,
            "0123456789012345678901234567890123456789012345678901234567890123"
        )
        .await
    );
    assert!(
        !db.authenticate(
            &node1,
            "wrong-012345678901234567890123456789012345678901234567890"
        )
        .await
    );
    command(&db,json!({"action":"plan","plan":{"id":plan,"name":"QA plan","port_limit":2,"traffic_limit_bytes":1000,"period_days":30,"price_cents":100,"reset_price_cents":20,"node_ids":[node1,node2],"enabled":true}})).await;
    let l = command(&db, json!({"action":"grant","user_id":u,"plan_id":plan})).await;
    let lid = s(&l, "id");
    let cycle = s(&l, "cycle_id");
    let mut tasks = tokio::task::JoinSet::new();
    for (node, port) in [(&node1, 32000), (&node2, 32000), (&node1, 32001)] {
        let db = db.clone();
        let c = json!({"action":"claim","claim":{"user_id":u,"lease_id":lid,"node_id":node,"public_ip":"1.1.1.1","port":port}});
        tasks.spawn(async move { db.command(c).await });
    }
    let mut allocations = vec![];
    let mut rejected = 0;
    while let Some(r) = tasks.join_next().await {
        match r.unwrap() {
            Ok(a) => allocations.push(a),
            Err(_) => rejected += 1,
        }
    }
    assert_eq!(allocations.len(), 2);
    assert_eq!(rejected, 1);
    let a = &allocations[0];
    let aid = s(a, "id");
    let anode = s(a, "node_id");
    let second = &allocations[1];
    let bid = s(second, "id");
    let bnode = s(second, "node_id");
    assert!(
        db.command(json!({"action":"release","user_id":other,"id":aid}))
            .await
            .is_err()
    );
    assert!(
        db.command(
            json!({"action":"target","user_id":other,"id":aid,"host":"example.com","port":443})
        )
        .await
        .is_err()
    );
    for a in &allocations {
        let mut r = report(&id(), 1, s(a, "id"), cycle, 0, 0);
        r.counters.clear();
        db.report(s(a, "node_id"), &r).await.unwrap();
        command(&db,json!({"action":"target","user_id":u,"id":a["id"],"host":"example.com","port":443,"load_balance":true,"targets":[{"host":"example.com","port":443},{"host":"example.net","port":443}]})).await;
    }
    assert!(
        db.rules(anode)
            .await
            .unwrap()
            .rules
            .iter()
            .any(|r| r.id == aid && r.load_balance && r.targets.len() == 2)
    );
    let epoch = id();
    let r = report(&epoch, 1, aid, cycle, 300, 100);
    db.report(anode, &r).await.unwrap();
    db.report(anode, &r).await.unwrap();
    let l = command(&db, json!({"action":"lease","lease_id":lid})).await;
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 400);
    let rollback = report(&epoch, 2, aid, cycle, 299, 100);
    assert!(db.report(anode, &rollback).await.is_err());
    let wrong_node = if anode == node1 { &node2 } else { &node1 };
    assert!(
        db.report(wrong_node, &report(&id(), 1, aid, cycle, 1, 0))
            .await
            .is_err()
    );

    // Connection telemetry must be bounded, replay-safe and scoped to this node's rules.
    let stats_epoch = id();
    let mut stats = Report {
        version: 1,
        epoch: stats_epoch,
        sequence: 1,
        probe: Probe {
            rule_connections: Some(
                [
                    (
                        aid.to_owned(),
                        vistart_forward::protocol::RuleConnections { tcp: 7, udp: 4 },
                    ),
                    (
                        id(),
                        vistart_forward::protocol::RuleConnections { tcp: 99, udp: 99 },
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    db.report(anode, &stats).await.unwrap();
    let snap = db.snapshot().await.unwrap();
    let node = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| s(n, "id") == anode)
        .unwrap();
    assert_eq!(
        node["probe"]["rule_connections"].as_object().unwrap().len(),
        1
    );
    assert_eq!(node["probe"]["rule_connections"][aid]["tcp"], 7);
    stats
        .probe
        .rule_connections
        .as_mut()
        .unwrap()
        .get_mut(aid)
        .unwrap()
        .tcp = 8;
    db.report(anode, &stats).await.unwrap(); // duplicate sequence cannot change live stats
    let snap = db.snapshot().await.unwrap();
    let node = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| s(n, "id") == anode)
        .unwrap();
    assert_eq!(node["probe"]["rule_connections"][aid]["tcp"], 7);
    stats.sequence = 2;
    stats
        .probe
        .rule_connections
        .as_mut()
        .unwrap()
        .get_mut(aid)
        .unwrap()
        .tcp = 65_537;
    assert!(db.report(anode, &stats).await.is_err());
    stats
        .probe
        .rule_connections
        .as_mut()
        .unwrap()
        .get_mut(aid)
        .unwrap()
        .tcp = 1;
    stats
        .probe
        .rule_connections
        .as_mut()
        .unwrap()
        .get_mut(aid)
        .unwrap()
        .udp = 16_385;
    assert!(db.report(anode, &stats).await.is_err());
    stats
        .probe
        .rule_connections
        .as_mut()
        .unwrap()
        .get_mut(aid)
        .unwrap()
        .udp = 1;
    db.report(wrong_node, &stats).await.unwrap();
    let snap = db.snapshot().await.unwrap();
    let node = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| s(n, "id") == *wrong_node)
        .unwrap();
    assert!(
        node["probe"]["rule_connections"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    stats.probe.rule_connections = Some((0..513).map(|_| (id(), Default::default())).collect());
    assert!(db.report(anode, &stats).await.is_err());
    stats.probe.rule_connections = Some(
        [("../invalid".into(), Default::default())]
            .into_iter()
            .collect(),
    );
    assert!(db.report(anode, &stats).await.is_err());
    assert!(
        serde_json::from_value::<Report>(
            json!({"probe":{"rule_connections":{"a":{"tcp":-1,"udp":0}}}})
        )
        .is_err()
    );
    stats.probe.rule_connections = None;
    db.report(anode, &stats).await.unwrap();
    let snap = db.snapshot().await.unwrap();
    let node = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| s(n, "id") == anode)
        .unwrap();
    assert!(node["probe"]["rule_connections"].is_null());
    eprintln!(
        "PASS: per-rule metrics ownership, bounds, duplicate report and old-agent compatibility"
    );
    let epoch2 = id();
    db.report(bnode, &report(&epoch2, 1, bid, cycle, 400, 250))
        .await
        .unwrap();
    let l = command(&db, json!({"action":"lease","lease_id":lid})).await;
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 1050);
    for node in [&node1, &node2] {
        assert!(db.rules(node).await.unwrap().rules.is_empty());
    }
    assert!(db.command(json!({"action":"claim","claim":{"user_id":u,"lease_id":lid,"node_id":node1,"public_ip":"1.1.1.1","port":32004}})).await.is_err());
    eprintln!(
        "PASS: atomic cross-node port quota, ownership, WSS identity, load balancing, duplicate/rollback reports, traffic suspension"
    );
    let reset = command(
        &db,
        json!({"action":"order","user_id":u,"plan_id":plan,"lease_id":lid,"kind":"reset"}),
    )
    .await;
    let exp = n(&l, "expires_at");
    let natural = n(&l, "next_reset_at");
    assert!(
        db.payment("epay", &id(), s(&reset, "id"), 21)
            .await
            .is_err()
    );
    let tid = id();
    db.payment("epay", &tid, s(&reset, "id"), 20).await.unwrap();
    db.payment("epay", &tid, s(&reset, "id"), 20).await.unwrap();
    let l = command(&db, json!({"action":"lease","lease_id":lid})).await;
    let new_cycle = s(&l, "cycle_id").to_owned();
    assert_ne!(new_cycle, cycle);
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 0);
    assert_eq!(n(&l, "expires_at"), exp);
    assert_eq!(n(&l, "next_reset_at"), natural);
    db.report(anode, &report(&epoch, 3, aid, cycle, 350, 100))
        .await
        .unwrap(); // Late old-cycle traffic must not spend the new allowance.
    db.report(anode, &report(&epoch, 4, aid, &new_cycle, 100, 100))
        .await
        .unwrap();
    let renew = command(
        &db,
        json!({"action":"order","user_id":u,"plan_id":plan,"lease_id":lid,"kind":"purchase"}),
    )
    .await;
    db.payment("epay", &id(), s(&renew, "id"), 100)
        .await
        .unwrap();
    let l = command(&db, json!({"action":"lease","lease_id":lid})).await;
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 200);
    assert_eq!(n(&l, "expires_at"), exp + 30 * 86400);
    assert_eq!(n(&l, "next_reset_at"), natural);
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(
            &mut c,
            "UPDATE vp_leases SET next_reset_at=? WHERE id=?",
            &[json!(now() - 1), json!(lid)],
        )
        .await
        .unwrap();
    }
    db.tick().await.unwrap();
    let l = command(&db, json!({"action":"lease","lease_id":lid})).await;
    assert_eq!(n(&l, "used_up") + n(&l, "used_down"), 0);
    assert_ne!(s(&l, "cycle_id"), new_cycle);
    let manual = command(
        &db,
        json!({"action":"order","user_id":u,"plan_id":plan,"lease_id":lid,"kind":"purchase"}),
    )
    .await;
    assert!(db.command(json!({"action":"admin-order-status","actor_id":other,"id":manual["id"],"status":"paid","note":"no privilege"})).await.is_err());
    command(&db,json!({"action":"admin-order-status","actor_id":u,"id":manual["id"],"status":"cancelled","note":"QA cancel"})).await;
    command(&db,json!({"action":"admin-order-status","actor_id":u,"id":manual["id"],"status":"paid","note":"QA receipt"})).await;
    let before = command(&db, json!({"action":"lease","lease_id":lid})).await;
    db.payment("epay", &id(), s(&manual, "id"), 100)
        .await
        .unwrap();
    let after = command(&db, json!({"action":"lease","lease_id":lid})).await;
    assert_eq!(before["expires_at"], after["expires_at"]);
    let waiting = command(
        &db,
        json!({"action":"order","user_id":u,"plan_id":plan,"lease_id":lid,"kind":"reset"}),
    )
    .await;
    command(&db, json!({"action":"end-lease","lease_id":lid})).await;
    db.payment("epay", &id(), s(&waiting, "id"), 20)
        .await
        .unwrap();
    assert_eq!(
        s(&db.get_order(s(&waiting, "id")).await.unwrap(), "status"),
        "paid_review"
    );
    eprintln!(
        "PASS: paid reset, ordinary renewal, natural reset, late counters, callback idempotency, manual settlement, paid-review preservation"
    );
    let report = Report {
        version: 1,
        epoch: id(),
        sequence: 1,
        agent_version: "0.3.0".into(),
        kernel_version: "0.3.0".into(),
        ..Default::default()
    };
    db.report(&node1, &report).await.unwrap();
    let removal = command(&db, json!({"action":"remove-node","id":node1})).await;
    assert_eq!(removal["pending"], true);
    let cfg = db.rules(&node1).await.unwrap();
    assert!(!cfg.decommission.is_empty());
    assert!(cfg.rules.is_empty());
    db.finish_removal(&node1, &cfg.decommission).await.unwrap();
    assert!(
        !db.authenticate(
            &node1,
            "0123456789012345678901234567890123456789012345678901234567890123"
        )
        .await
    );
    let snapshot = db.snapshot().await.unwrap();
    assert!(
        !snapshot["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| s(n, "id") == node1)
    );
    assert_eq!(
        snapshot["plans"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| s(p, "id") == plan)
            .unwrap()["node_ids"],
        json!([node2])
    );
    assert_eq!(
        snapshot["orders"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| s(o, "user_id") == u)
            .count(),
        4
    );
    let mut c = db.pool.acquire().await.unwrap();
    assert!(
        !rows(
            &mut c,
            "SELECT id FROM vp_cycles WHERE lease_id=?",
            &[json!(lid)]
        )
        .await
        .unwrap()
        .is_empty()
    );
    eprintln!(
        "PASS: online decommission, credential revocation, node association cleanup, history retention"
    );
}
