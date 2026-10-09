use serde_json::{Value, json};
use vistart_forward::{
    b, id, n, now,
    protocol::{Counter, Report},
    s,
    store::{Store, exec, rows},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_edits_deletion_late_counters_and_payment() {
    let Some(path) = std::env::var_os("VISTART_TEST_CONFIG") else {
        return;
    };
    let cfg: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(s(&cfg["mysql"], "name").ends_with("_qa"));
    let db = Store::open(&cfg["mysql"]).await.unwrap();
    let owner = id();
    let stranger = id();
    let node = id();
    let second_node = id();
    let pid = id();
    for uid in [&owner, &stranger] {
        db.command(json!({"action":"user","user":{"id":uid,"name":"Lifecycle test"}}))
            .await
            .unwrap();
    }
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(&mut c,"INSERT INTO vp_accounts(id,username,name,password_hash,role,created_at) VALUES(?,?,?,'not-a-login-hash','admin',?)",&[json!(owner),json!(owner),json!("QA"),json!(now())]).await.unwrap();
    }
    for nid in [&node, &second_node] {
        db.command(json!({"action":"node","node":{"id":nid,"name":"QA","enabled":true},"token":"test-token-with-at-least-forty-characters-not-used-for-login"})).await.unwrap();
        db.command(json!({"action":"pool","pool":{"node_id":nid,"public_ip":"1.1.1.1","bind_ip":"1.1.1.1","start":35000,"end":35010}})).await.unwrap();
    }
    let mut p = json!({"id":pid,"name":"Package A","port_limit":3,"traffic_limit_bytes":1000,"period_days":30,"price_cents":100,"reset_price_cents":20,"node_ids":[node,second_node],"enabled":true});
    db.command(json!({"action":"plan","plan":p})).await.unwrap();
    let lease = db
        .command(json!({"action":"grant","user_id":owner,"plan_id":pid}))
        .await
        .unwrap();
    let lid = s(&lease, "id");
    let cycle = s(&lease, "cycle_id");
    let mut rs = Vec::new();
    for (nid, port) in [(&node, 35000), (&node, 35001), (&second_node, 35000)] {
        let r=db.command(json!({"action":"claim","claim":{"user_id":owner,"lease_id":lid,"node_id":nid,"public_ip":"1.1.1.1","port":port}})).await.unwrap();
        db.command(
            json!({"action":"target","user_id":owner,"id":r["id"],"host":"example.com","port":443}),
        )
        .await
        .unwrap();
        rs.push(r);
    }
    let epoch = id();
    let mut report = Report {
        version: 1,
        epoch: epoch.clone(),
        sequence: 1,
        active_rules: Some(vec![s(&rs[0], "id").into()]),
        counters: vec![Counter {
            rule_id: s(&rs[0], "id").into(),
            cycle_id: cycle.into(),
            up: 100,
            down: 50,
        }],
        ..Default::default()
    };
    db.report(&node, &report).await.unwrap();
    let snap = db.snapshot().await.unwrap();
    assert_eq!(
        snap["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == node)
            .unwrap()["active_rules"],
        json!([rs[0]["id"]])
    );

    let edit = json!({"action":"edit-node","actor_id":owner,"node":{"id":node,"name":"QA renamed","region":"互联","public_ip":"172.20.0.5","enabled":true}});
    let mut denied = edit.clone();
    denied["actor_id"] = json!(stranger);
    assert!(db.command(denied).await.is_err());
    db.command(edit).await.unwrap();
    let snap = db.snapshot().await.unwrap();
    assert!(
        snap["pools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["node_id"] == node)
            .all(|x| x["public_ip"] == "172.20.0.5" && x["bind_ip"] == "1.1.1.1")
    );
    assert!(
        snap["allocations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["node_id"] == node)
            .all(|x| x["public_ip"] == "172.20.0.5" && x["bind_ip"] == "1.1.1.1")
    );

    p["bandwidth_mbps"] = json!(8);
    p["tcp_limit"] = json!(2);
    p["udp_limit"] = json!(1);
    p["name"] = json!("Package B");
    p["port_limit"] = json!(1);
    p["traffic_limit_bytes"] = json!(100);
    p["period_days"] = json!(45);
    p["node_ids"] = json!([node]);
    db.command(json!({"action":"plan","plan":p})).await.unwrap();
    let unchanged = db
        .command(json!({"action":"lease","lease_id":lid}))
        .await
        .unwrap();
    assert_eq!(unchanged["port_limit"], 3);
    assert_eq!(unchanged["used_ports"], 3);
    assert_eq!(unchanged["bandwidth_mbps"], 0);
    assert!(
        db.command(json!({"action":"plan","plan":p,"actor_id":owner,"update_existing":true}))
            .await
            .is_err()
    );
    let preview = db
        .command(json!({"action":"preview-plan","actor_id":owner,"plan":p}))
        .await
        .unwrap();
    db.command(json!({"action":"plan","plan":p,"actor_id":owner,"update_existing":true,"preview_digest":preview["digest"]}))
        .await
        .unwrap();
    let synced = db
        .command(json!({"action":"lease","lease_id":lid}))
        .await
        .unwrap();
    assert_eq!(synced["port_limit"], 1);
    assert_eq!(synced["bandwidth_mbps"], 8);
    assert_eq!(synced["tcp_limit"], 2);
    assert_eq!(synced["udp_limit"], 1);
    assert_eq!(synced["used_ports"], 1);
    assert_eq!(synced["plan_name"], "Package B");
    assert_eq!(synced["period_days"], 45);
    assert_eq!(synced["expires_at"], lease["expires_at"]);
    assert_eq!(synced["next_reset_at"], lease["next_reset_at"]);
    assert_eq!(synced["cycle_id"], lease["cycle_id"]);
    assert_eq!(n(&synced, "used_up") + n(&synced, "used_down"), 150);
    assert!(db.rules(&node).await.unwrap().rules.is_empty());
    assert!(db.rules(&second_node).await.unwrap().rules.is_empty());
    p["traffic_limit_bytes"] = json!(1000);
    assert!(
        db.command(json!({"action":"plan","plan":p,"actor_id":owner,"update_existing":true}))
            .await
            .is_err()
    );
    let preview = db
        .command(json!({"action":"preview-plan","actor_id":owner,"plan":p}))
        .await
        .unwrap();
    db.command(json!({"action":"plan","plan":p,"actor_id":owner,"update_existing":true,"preview_digest":preview["digest"]}))
        .await
        .unwrap();
    assert_eq!(db.rules(&node).await.unwrap().rules.len(), 1);
    assert!(
        db.command(json!({"action":"delete-lease","user_id":owner,"id":lid}))
            .await
            .is_err()
    );
    assert!(
        db.command(json!({"action":"delete-lease","user_id":stranger,"id":lid}))
            .await
            .is_err()
    );
    assert!(
        db.command(json!({"action":"admin-delete-lease","actor_id":stranger,"id":lid}))
            .await
            .is_err()
    );
    let pending=db.command(json!({"action":"order","user_id":owner,"plan_id":pid,"lease_id":lid,"kind":"purchase"})).await.unwrap();
    assert!(
        db.command(json!({"action":"delete-plan","actor_id":stranger,"id":pid}))
            .await
            .is_err()
    );
    db.command(json!({"action":"delete-plan","actor_id":owner,"id":pid}))
        .await
        .unwrap();
    assert_eq!(
        db.rules(&node).await.unwrap().rules.len(),
        1,
        "removing product must preserve paid lease"
    );
    assert!(
        db.command(json!({"action":"grant","user_id":owner,"plan_id":pid}))
            .await
            .is_err()
    );
    assert!(
        db.command(json!({"action":"order","user_id":owner,"plan_id":pid,"kind":"purchase"}))
            .await
            .is_err()
    );
    assert!(
        db.command(json!({"action":"plan","plan":p})).await.is_err(),
        "deleted ID cannot be silently resurrected"
    );
    let txid = id();
    db.payment("epay", &txid, s(&pending, "id"), 100)
        .await
        .unwrap();
    db.payment("epay", &txid, s(&pending, "id"), 100)
        .await
        .unwrap();
    assert_eq!(
        db.get_order(s(&pending, "id")).await.unwrap()["status"],
        "paid_review"
    );
    assert_eq!(
        db.command(json!({"action":"lease","lease_id":lid}))
            .await
            .unwrap()["expires_at"],
        lease["expires_at"]
    );
    db.command(json!({"action":"admin-delete-lease","actor_id":owner,"id":lid}))
        .await
        .unwrap();
    assert!(db.rules(&node).await.unwrap().rules.is_empty());
    report.sequence = 2;
    report.counters[0].up = 120;
    report.active_rules = Some(vec![]);
    db.report(&node, &report).await.unwrap();
    db.report(&node, &report).await.unwrap();
    let deleted = db
        .command(json!({"action":"lease","lease_id":lid}))
        .await
        .unwrap();
    assert!(b(&deleted, "deleted"));
    assert_eq!(n(&deleted, "used_up"), 120);
    let snap = db.snapshot().await.unwrap();
    assert!(
        !snap["leases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == lid)
    );
    assert!(
        !snap["plans"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == pid)
    );
    assert!(
        !snap["allocations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["lease_id"] == lid)
    );
    assert!(
        snap["orders"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == pending["id"])
    );

    p["id"] = json!(id());
    p["node_ids"] = json!([node]);
    db.command(json!({"action":"plan","plan":p})).await.unwrap();
    let expired = db
        .command(json!({"action":"grant","user_id":owner,"plan_id":p["id"]}))
        .await
        .unwrap();
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(
            &mut c,
            "UPDATE vp_leases SET expires_at=? WHERE id=?",
            &[json!(now() - 1), expired["id"].clone()],
        )
        .await
        .unwrap();
    }
    db.command(json!({"action":"delete-lease","user_id":owner,"id":expired["id"]}))
        .await
        .unwrap();
    db.command(json!({"action":"delete-lease","user_id":owner,"id":expired["id"]}))
        .await
        .unwrap();
    assert!(
        db.command(json!({"action":"delete-lease","user_id":stranger,"id":expired["id"]}))
            .await
            .is_err()
    );
    let mut c = db.pool.acquire().await.unwrap();
    assert!(
        rows(
            &mut c,
            "SELECT id FROM vp_leases WHERE id=? AND deleted=1",
            &[expired["id"].clone()]
        )
        .await
        .unwrap()
        .len()
            == 1
    );
}
