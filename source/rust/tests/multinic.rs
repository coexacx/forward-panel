use serde_json::{Value, json};
use vistart_forward::{
    id, s,
    store::{Store, exec},
};
#[tokio::test]
async fn wildcard_pools_and_allocation_reject_cross_interface_port_conflicts() {
    let Some(path) = std::env::var_os("VISTART_TEST_CONFIG") else {
        return;
    };
    let cfg: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(s(&cfg["mysql"], "name").ends_with("_qa"));
    let db = Store::open(&cfg["mysql"]).await.unwrap();
    let node = id();
    let user = id();
    let plan = id();
    db.command(json!({"action":"user","user":{"id":user,"name":"Multi NIC QA"}}))
        .await
        .unwrap();
    db.command(json!({"action":"node","node":{"id":node,"name":"Multi NIC QA","enabled":true},"token":"qa-only-multinic-secret-never-used-on-a-real-node"})).await.unwrap();
    let mut pool = json!({"node_id":node,"public_ip":"198.51.100.10","bind_ip":"127.0.0.1","start":35550,"end":35550});
    db.command(json!({"action":"pool","pool":pool}))
        .await
        .unwrap();
    let mut overlap = pool.clone();
    overlap["public_ip"] = json!("198.51.100.11");
    overlap["bind_ip"] = json!("0.0.0.0");
    assert!(
        db.command(json!({"action":"pool","pool":overlap}))
            .await
            .is_err()
    );
    db.command(json!({"action":"remove-pool","pool":pool}))
        .await
        .unwrap();
    pool["bind_ip"] = json!("0.0.0.0");
    db.command(json!({"action":"pool","pool":pool}))
        .await
        .unwrap();
    overlap["bind_ip"] = json!("127.0.0.2");
    assert!(
        db.command(json!({"action":"pool","pool":overlap}))
            .await
            .is_err()
    );
    db.command(json!({"action":"plan","plan":{"id":plan,"name":"Multi NIC QA","price_cents":0,"reset_price_cents":0,"port_limit":3,"traffic_limit_bytes":1000000,"period_days":30,"node_ids":[node],"enabled":true}})).await.unwrap();
    let lease = db
        .command(json!({"action":"grant","user_id":user,"plan_id":plan}))
        .await
        .unwrap();
    let claim = json!({"user_id":user,"lease_id":lease["id"],"node_id":node,"public_ip":"198.51.100.10","port":35550});
    db.command(json!({"action":"claim","claim":claim}))
        .await
        .unwrap();
    // Legacy databases could contain distinct-IP overlapping pools. A wildcard
    // allocation must still prevent their explicit and random claims colliding.
    {
        let mut c = db.pool.acquire().await.unwrap();
        exec(
            &mut c,
            "INSERT INTO vp_pools(node_id,public_ip,bind_ip,start_port,end_port) VALUES(?,?,?,?,?)",
            &[
                json!(node),
                json!("198.51.100.11"),
                json!("127.0.0.2"),
                json!(35550),
                json!(35551),
            ],
        )
        .await
        .unwrap();
    }
    let mut other = claim.clone();
    other["public_ip"] = json!("198.51.100.11");
    assert!(
        db.command(json!({"action":"claim","claim":other}))
            .await
            .is_err()
    );
    other["port"] = json!(0);
    let assigned = db
        .command(json!({"action":"claim","claim":other}))
        .await
        .unwrap();
    assert_eq!(assigned["port"], 35551);
}
