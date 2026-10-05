use super::*;
use std::collections::{HashMap, HashSet};
pub async fn snapshot(ctx: &Context<'_>, admin: bool) -> Result<Value> {
    let mut state = ctx.app.store.snapshot().await?;
    let jobs = if admin {
        ctx.app.jobs.list().await
    } else {
        Vec::new()
    };
    let live: HashSet<String> = state["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| s(n, "id").to_owned())
        .collect();
    for job in &jobs {
        let job = serde_json::to_value(job)?;
        if live.contains(s(&job, "node_id")) && job["result"].is_object() {
            let mut m = metadata(ctx.app, s(&job, "node_id")).await?;
            m.as_object_mut()
                .unwrap()
                .extend(job["result"].as_object().unwrap().clone());
            meta_save(ctx.app, s(&job, "node_id"), &m).await?;
        }
    }
    let mut db = ctx.app.store.pool.acquire().await?;
    let meta: HashMap<String, Value> = rows(&mut db, "SELECT id,value FROM vp_metadata", &[])
        .await?
        .iter()
        .map(|m| {
            (
                s(m, "id").to_owned(),
                serde_json::from_str(s(m, "value")).unwrap_or(json!({})),
            )
        })
        .collect();
    let revision = n(&state, "revision");
    let mut samples = HashMap::<(String, String, String, i64), Value>::new();
    for node in state["nodes"].as_array_mut().unwrap() {
        node["meta"] = meta.get(s(node, "id")).cloned().unwrap_or(json!({}));
        node["online"] = json!(b(node, "enabled") && n(node, "last_seen") > now() - 15);
        node["syncing"] = json!(n(node, "applied_revision") < revision);
        node["tcping_supported"] = json!(node["probe"]["target_checks"].is_array());
        if let Some(checks) = node["probe"]["target_checks"].as_array() {
            for check in checks {
                samples.insert(
                    (
                        s(node, "id").into(),
                        s(check, "rule_id").into(),
                        s(check, "host").into(),
                        n(check, "port"),
                    ),
                    check.clone(),
                );
            }
        }
        if let Some(p) = node["probe"].as_object_mut() {
            p.remove("target_checks");
        }
    }
    let nodes: HashMap<String, Value> = state["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (s(v, "id").into(), v.clone()))
        .collect();
    let leases: HashMap<String, Value> = state["leases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (s(v, "id").into(), v.clone()))
        .collect();
    let disabled: HashSet<String> = state["users"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| b(v, "disabled"))
        .map(|v| s(v, "id").into())
        .collect();
    for a in state["allocations"].as_array_mut().unwrap() {
        let empty = json!({});
        let node = nodes.get(s(a, "node_id")).unwrap_or(&empty);
        let lease = leases.get(s(a, "lease_id")).unwrap_or(&empty);
        let status = if b(a, "released")
            || lease == &empty
            || b(lease, "ended")
            || b(lease, "manual_paused")
            || n(lease, "expires_at") <= now()
            || disabled.contains(s(lease, "user_id"))
            || (n(lease, "traffic_limit_bytes") > 0
                && n(lease, "used_up") + n(lease, "used_down") >= n(lease, "traffic_limit_bytes"))
        {
            "paused"
        } else if !b(node, "online") {
            "offline"
        } else if b(node, "syncing") {
            "pending"
        } else if node["errors"].as_array().is_some_and(|errors| {
            errors
                .iter()
                .any(|e| s(e, "rule_id").is_empty() || s(e, "rule_id") == s(a, "id"))
        }) {
            "apply_failed"
        } else if !b(node, "tcping_supported") {
            "unsupported"
        } else {
            "checking"
        };
        let targets = if b(a, "load_balance") {
            a["targets"].clone()
        } else if !s(a, "target_host").is_empty() {
            json!([{"host":a["target_host"],"port":a["target_port"]}])
        } else {
            json!([])
        };
        let mut checks = Vec::new();
        for t in targets.as_array().unwrap() {
            let mut check = json!({"host":t["host"],"port":t["port"],"status":status,"latency_ms":null,"checked_at":0});
            if status == "checking"
                && let Some(sample) = samples.get(&(
                    s(a, "node_id").into(),
                    s(a, "id").into(),
                    s(t, "host").into(),
                    n(t, "port"),
                ))
            {
                let at = n(sample, "checked_at");
                check["checked_at"] = json!(at);
                check["status"] = json!(if at < now() - 35 || at > now() + 10 {
                    "stale"
                } else {
                    s(sample, "status")
                });
                if s(&check, "status") == "ok" {
                    check["latency_ms"] = sample["latency_ms"].clone();
                }
            }
            checks.push(check);
        }
        a["target_checks"] = json!(checks);
        a["remark"] = meta
            .get(&format!("allocation_{}", s(a, "id")))
            .and_then(|v| v.get("remark"))
            .cloned()
            .unwrap_or(json!(""));
    }
    let user = ctx.user()?;
    state["users"] = if admin {
        json!(rows(&mut db,"SELECT id,username,name,role,disabled,created_at,email,email_verified,(totp_secret IS NOT NULL) AS two_factor FROM vp_accounts ORDER BY created_at DESC",&[]).await?.into_iter().map(safe_account).collect::<Vec<_>>())
    } else {
        json!([safe_account(user.clone())])
    };
    if !admin {
        state["leases"]
            .as_array_mut()
            .unwrap()
            .retain(|l| s(l, "user_id") == s(user, "id"));
        let lids: HashSet<String> = state["leases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| s(l, "id").into())
            .collect();
        let planids: HashSet<String> = state["leases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| s(l, "plan_id").into())
            .collect();
        let mut allowed = HashSet::<String>::new();
        for l in state["leases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| !b(l, "ended") && n(l, "expires_at") > now())
        {
            for n in l["node_ids"].as_array().unwrap() {
                if let Some(id) = n.as_str() {
                    allowed.insert(id.into());
                }
            }
        }
        state["allocations"]
            .as_array_mut()
            .unwrap()
            .retain(|a| !b(a, "released") && lids.contains(s(a, "lease_id")));
        state["orders"]
            .as_array_mut()
            .unwrap()
            .retain(|o| s(o, "user_id") == s(user, "id"));
        state["pools"]
            .as_array_mut()
            .unwrap()
            .retain(|p| allowed.contains(s(p, "node_id")));
        state["plans"]
            .as_array_mut()
            .unwrap()
            .retain(|p| b(p, "enabled") || planids.contains(s(p, "id")));
        for n in state["nodes"].as_array_mut().unwrap() {
            n["meta"] =
                json!({"region":s(&n["meta"],"region"),"public_ip":s(&n["meta"],"public_ip")});
            n.as_object_mut().unwrap().remove("applied_revision");
            if !allowed.contains(s(n, "id")) {
                for k in [
                    "probe",
                    "errors",
                    "agent_version",
                    "kernel_version",
                    "last_seen",
                    "online",
                ] {
                    n.as_object_mut().unwrap().remove(k);
                }
            } else {
                n["errors"] = json!([]);
            }
        }
    }
    let sql = "SELECT w.id,w.at,w.action,w.detail,w.subject,COALESCE(a.name,'系统') actor FROM vp_web_audit w LEFT JOIN vp_accounts a ON a.id=w.actor";
    state["audit"] = json!(if admin {
        rows(&mut db, &format!("{sql} ORDER BY w.id DESC LIMIT 300"), &[]).await?
    } else {
        rows(
            &mut db,
            &format!("{sql} WHERE w.subject=? ORDER BY w.id DESC LIMIT 300"),
            &[user["id"].clone()],
        )
        .await?
    });
    drop(db);
    let config = ctx.app.config.read().await.clone();
    let mut payment = config["payment"].clone();
    if admin {
        payment["has_secret"] = json!(!s(&payment, "secret").is_empty());
        if let Some(o) = payment.as_object_mut() {
            for k in ["secret", "private_key", "platform_key"] {
                o.remove(k);
            }
        }
    } else {
        payment = json!({"enabled":b(&payment,"enabled"),"methods":payment["methods"].as_array().map(|a|a.iter().filter(|v|b(v,"enabled")).cloned().collect::<Vec<_>>()).unwrap_or_default()});
    }
    state["payment"] = payment;
    state["mail"] = if admin {
        mail::safe(ctx.app)?
    } else {
        Value::Null
    };
    state["settings"] = json!({"site_name":setting(ctx.app,"site_name",json!("Vistart Ports")).await?,"registration":setting(ctx.app,"registration",json!(false)).await?,"email_verification":setting(ctx.app,"registration_email_verification",json!(false)).await?,"controller_url":config["controller_urls"].as_array().and_then(|a|a.first()).cloned().unwrap_or(json!("")),"public_origin":config["origin"],"version":crate::VERSION});
    state["jobs"] = json!(jobs);
    state["user"] = safe_account(user.clone());
    state["now"] = json!(now());
    Ok(state)
}
