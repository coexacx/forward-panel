use serde_json::{Value, json};
use vistart_forward::{
    id, n, now,
    protocol::{Counter, Report},
    s,
    store::{Store, exec},
};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_restore_conflicts_quota_and_late_accounting() {
    let Some(path) = std::env::var_os("VISTART_TEST_CONFIG") else {
        return;
    };
    let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(s(&config["mysql"], "name").ends_with("_qa"));
    let db = Store::open(&config["mysql"]).await.unwrap();
    let uid = id();
    let old = id();
    let target = id();
    let pid = id();
    db.command(json!({"action":"user","user":{"id":uid,"name":"Migration QA"}}))
        .await
        .unwrap();
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(&mut c,"INSERT INTO vp_accounts(id,username,name,password_hash,role,created_at) VALUES(?,?,?,'not-a-login-hash','admin',?)",&[json!(uid),json!(uid),json!("Migration QA"),json!(now())]).await.unwrap();
    }
    for node in [&old, &target] {
        db.command(json!({"action":"node","node":{"id":node,"name":"Migration QA","enabled":true},"token":"not-a-real-token-at-least-forty-characters-123456789"})).await.unwrap();
        db.command(json!({"action":"pool","pool":{"node_id":node,"public_ip":"1.1.1.1","bind_ip":"1.1.1.1","start":36000,"end":36005}})).await.unwrap();
        let mut c = db.pool.acquire().await.unwrap();
        exec(
            &mut c,
            "UPDATE vp_nodes SET last_seen=?,agent_version='0.3.1' WHERE id=?",
            &[json!(now()), json!(node)],
        )
        .await
        .unwrap();
    }
    let plan = json!({"id":pid,"name":"Migration plan","port_limit":2,"traffic_limit_bytes":1000000,
        "period_days":30,"price_cents":0,"reset_price_cents":0,"node_ids":[old],"enabled":true});
    db.command(json!({"action":"plan","plan":plan}))
        .await
        .unwrap();
    let lease = db
        .command(json!({"action":"grant","user_id":uid,"plan_id":pid}))
        .await
        .unwrap();
    let rule=db.command(json!({"action":"claim","claim":{"user_id":uid,"lease_id":lease["id"],"node_id":old,"public_ip":"1.1.1.1","port":36000}})).await.unwrap();
    db.command(
        json!({"action":"target","user_id":uid,"id":rule["id"],"host":"example.com","port":443}),
    )
    .await
    .unwrap();
    let mut r = Report {
        version: 1,
        epoch: id(),
        sequence: 1,
        counters: vec![Counter {
            rule_id: s(&rule, "id").into(),
            cycle_id: s(&lease, "cycle_id").into(),
            up: 10,
            down: 5,
        }],
        ..Default::default()
    };
    db.report(&old, &r).await.unwrap();
    let file = db
        .command(json!({"action":"export-node-rules","actor_id":uid,"node_id":old}))
        .await
        .unwrap();
    let mut cmd = json!({"action":"preview-node-migration","actor_id":uid,"node_id":old,"file":file,"file_hash":id(),"occupied_ports":[]});
    let preview = db.command(cmd.clone()).await.unwrap();
    cmd["action"] = json!("import-node-rules");
    cmd["preview"] = preview;
    let restored = db.command(cmd.clone()).await.unwrap();
    assert_eq!(
        restored["entries"][0]["id"], rule["id"],
        "same-node recovery keeps IDs"
    );
    assert_eq!(
        db.command(json!({"action":"lease","lease_id":lease["id"]}))
            .await
            .unwrap()["used_ports"],
        1
    );
    // A different node preserves the old port if possible, otherwise presents a replacement.
    cmd["action"] = json!("preview-node-migration");
    cmd["node_id"] = json!(target);
    cmd["occupied_ports"] = json!([36000]);
    let preview = db.command(cmd.clone()).await.unwrap();
    assert_eq!(preview["entries"][0]["port"], 36001);
    cmd["action"] = json!("import-node-rules");
    cmd["preview"] = preview.clone();
    cmd["occupied_ports"] = json!([36000, 36001]);
    assert!(
        db.command(cmd.clone()).await.is_err(),
        "changed occupancy must require new approval"
    );
    assert_eq!(db.rules(&old).await.unwrap().rules.len(), 1);
    cmd["occupied_ports"] = json!([36000]);
    let result = db.command(cmd.clone()).await.unwrap();
    let new_id = s(&result["entries"][0], "id").to_owned();
    assert_ne!(new_id, s(&rule, "id"));
    assert!(db.rules(&old).await.unwrap().rules.is_empty());
    assert_eq!(db.rules(&target).await.unwrap().rules[0].listen_port, 36001);
    assert!(
        db.command(cmd).await.is_err(),
        "export cannot migrate twice"
    );
    let moved = db
        .command(json!({"action":"lease","lease_id":lease["id"]}))
        .await
        .unwrap();
    for key in [
        "expires_at",
        "next_reset_at",
        "cycle_id",
        "port_limit",
        "traffic_limit_bytes",
    ] {
        assert_eq!(moved[key], lease[key]);
    }
    assert_eq!(moved["used_up"], 10);
    assert_eq!(moved["used_down"], 5);
    assert_eq!(moved["used_ports"], 1);
    assert_eq!(moved["node_ids"], json!([target]));
    r.sequence = 2;
    r.counters[0].up = 20;
    db.report(&old, &r).await.unwrap();
    db.report(&old, &r).await.unwrap();
    let new_report = Report {
        version: 1,
        epoch: id(),
        sequence: 1,
        counters: vec![Counter {
            rule_id: new_id,
            cycle_id: s(&lease, "cycle_id").into(),
            up: 7,
            down: 3,
        }],
        ..Default::default()
    };
    db.report(&target, &new_report).await.unwrap();
    let final_lease = db
        .command(json!({"action":"lease","lease_id":lease["id"]}))
        .await
        .unwrap();
    assert_eq!(
        n(&final_lease, "used_up") + n(&final_lease, "used_down"),
        35
    );
    // Deleted source records can be restored once without losing late counter ownership.
    let file = db
        .command(json!({"action":"export-node-rules","actor_id":uid,"node_id":target}))
        .await
        .unwrap();
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(
            &mut c,
            "UPDATE vp_nodes SET last_seen=0 WHERE id=?",
            &[json!(target)],
        )
        .await
        .unwrap();
    }
    db.command(json!({"action":"remove-node","id":target}))
        .await
        .unwrap();
    let mut restore = json!({"action":"preview-node-migration","actor_id":uid,"node_id":old,"file":file,"file_hash":id(),"occupied_ports":[]});
    let preview = db.command(restore.clone()).await.unwrap();
    restore["action"] = json!("import-node-rules");
    restore["preview"] = preview;
    db.command(restore.clone()).await.unwrap();
    restore["action"] = json!("preview-node-migration");
    restore["file_hash"] = json!(id());
    assert!(
        db.command(restore).await.is_err(),
        "another export of already moved IDs cannot duplicate rules"
    );
    let after = db
        .command(json!({"action":"lease","lease_id":lease["id"]}))
        .await
        .unwrap();
    assert_eq!(after["used_ports"], 1);
    assert_eq!(after["node_ids"], json!([old]));
    assert_eq!(after["used_up"], final_lease["used_up"]);
}
