use crate::{b, id, n, now, protocol::*, s, token, valid_id, valid_target};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Column, ConnectOptions, MySql, MySqlConnection, MySqlPool, Row, Transaction, TypeInfo,
    ValueRef,
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode},
};
use std::{collections::HashSet, fmt, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
mod billing;
mod lifecycle;
mod migration;
mod resources;
mod traffic;
#[derive(Debug)]
pub struct Fault {
    pub code: u16,
    pub message: &'static str,
}
impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for Fault {}
pub fn invalid() -> anyhow::Error {
    Fault {
        code: 400,
        message: "invalid input",
    }
    .into()
}
pub fn forbidden() -> anyhow::Error {
    Fault {
        code: 403,
        message: "not permitted",
    }
    .into()
}
pub fn missing() -> anyhow::Error {
    Fault {
        code: 404,
        message: "not found",
    }
    .into()
}
pub fn conflict() -> anyhow::Error {
    Fault {
        code: 409,
        message: "resource conflict",
    }
    .into()
}
pub fn quota() -> anyhow::Error {
    Fault {
        code: 409,
        message: "package quota exhausted",
    }
    .into()
}
pub struct Store {
    pub pool: MySqlPool,
    pub writer: Mutex<()>,
}
pub type Tx<'a> = Transaction<'a, MySql>;
fn query<'a>(
    sql: &'a str,
    args: &[Value],
) -> sqlx::query::Query<'a, MySql, sqlx::mysql::MySqlArguments> {
    let mut q = sqlx::query(sql);
    for v in args {
        q = match v {
            Value::Null => q.bind(None::<String>),
            Value::Bool(v) => q.bind(*v),
            Value::Number(v) => q.bind(v.as_i64().unwrap_or(0)),
            Value::String(v) => q.bind(v.clone()),
            _ => q.bind(v.to_string()),
        };
    }
    q
}
pub async fn exec(db: &mut MySqlConnection, sql: &str, args: &[Value]) -> Result<u64> {
    Ok(query(sql, args).execute(db).await?.rows_affected())
}
pub async fn rows(db: &mut MySqlConnection, sql: &str, args: &[Value]) -> Result<Vec<Value>> {
    let rr = query(sql, args).fetch_all(db).await?;
    let mut out = Vec::with_capacity(rr.len());
    for row in rr {
        let mut o = serde_json::Map::new();
        for (i, col) in row.columns().iter().enumerate() {
            let k = col.name();
            let typ = col.type_info().name();
            let v = if row.try_get_raw(i)?.is_null() {
                Value::Null
            } else if [
                "disabled",
                "supports_limits",
                "enabled",
                "manual_paused",
                "ended",
                "released",
                "deleted",
                "current",
                "used",
                "email_verified",
                "two_factor",
            ]
            .contains(&k)
            {
                json!(row.try_get_unchecked::<i64, _>(i)? != 0)
            } else if typ.contains("INT") || typ == "DECIMAL" {
                json!(row.try_get_unchecked::<i64, _>(i)?)
            } else if typ == "FLOAT" || typ == "DOUBLE" {
                json!(row.try_get::<f64, _>(i)?)
            } else {
                let text = row.try_get_unchecked::<String, _>(i)?;
                if [
                    "node_ids",
                    "targets",
                    "probe",
                    "errors",
                    "snapshot",
                    "active_rules",
                    "applied_rules",
                ]
                .contains(&k)
                {
                    serde_json::from_str(&text)?
                } else {
                    json!(text)
                }
            };
            o.insert(k.to_owned(), v);
        }
        let mut v = Value::Object(o);
        if v.get("targets").is_some() {
            v["load_balance"] = json!(v["targets"].as_array().is_some_and(|a| a.len() > 1))
        }
        out.push(v);
    }
    Ok(out)
}
pub async fn one(db: &mut MySqlConnection, sql: &str, args: &[Value]) -> Result<Value> {
    rows(db, sql, args)
        .await?
        .into_iter()
        .next()
        .ok_or_else(missing)
}
pub async fn count(db: &mut MySqlConnection, sql: &str, args: &[Value]) -> Result<i64> {
    Ok(n(&one(db, sql, args).await?, "n"))
}
async fn bump(db: &mut MySqlConnection) -> Result<()> {
    exec(
        db,
        "UPDATE vp_meta SET value=value+1 WHERE meta_key='revision'",
        &[],
    )
    .await?;
    Ok(())
}
async fn audit(db: &mut MySqlConnection, event: &str, subject: &str, detail: &str) -> Result<()> {
    exec(
        db,
        "INSERT INTO vp_audit(at,event,subject,detail) VALUES(?,?,?,?)",
        &[json!(now()), json!(event), json!(subject), json!(detail)],
    )
    .await?;
    Ok(())
}
const PLAN: &str = "SELECT id,name,port_limit,traffic_limit AS traffic_limit_bytes,period_days,price_cents,reset_price_cents,node_ids,enabled,deleted,bandwidth_mbps,tcp_limit,udp_limit FROM vp_plans";
const LEASE: &str = "SELECT l.id,l.user_id,l.plan_id,l.plan_name,l.port_limit,l.traffic_limit AS traffic_limit_bytes,l.period_days,l.node_ids,l.expires_at,l.next_reset_at,l.current_cycle AS cycle_id,l.manual_paused,l.ended,l.deleted,l.bandwidth_mbps,l.tcp_limit,l.udp_limit,c.up AS used_up,c.down AS used_down,(SELECT COUNT(*) FROM vp_allocations a WHERE a.lease_id=l.id AND a.released=0) AS used_ports FROM vp_leases l JOIN vp_cycles c ON c.id=l.current_cycle";
const ALLOCATION: &str = "SELECT a.id,a.lease_id,a.node_id,a.public_ip,a.bind_ip,a.port,a.target_host,a.target_port,a.released,COALESCE(t.targets,'[]') AS targets FROM vp_allocations a LEFT JOIN vp_allocation_targets t ON t.rule_id=a.id";
const ORDER: &str = "SELECT id,user_id,plan_id,lease_id,kind,amount_cents,status,created_at,paid_at,snapshot FROM vp_orders";
const NODES: &str = "SELECT n.id,n.name,n.enabled,n.last_seen,n.applied_revision,n.probe,n.errors,n.active_rules,n.applied_rules,n.agent_version,n.kernel_version,n.supports_limits,COALESCE(d.status,'') AS removal_status FROM vp_nodes n LEFT JOIN vp_node_removals d ON d.node_id=n.id WHERE n.deleted=0";
async fn lease(db: &mut MySqlConnection, id: &str) -> Result<Value> {
    one(db, &format!("{LEASE} WHERE l.id=?"), &[json!(id)]).await
}
async fn plan(db: &mut MySqlConnection, id: &str) -> Result<Value> {
    one(db, &format!("{PLAN} WHERE id=?"), &[json!(id)]).await
}
async fn allocation(db: &mut MySqlConnection, id: &str) -> Result<Value> {
    one(db, &format!("{ALLOCATION} WHERE a.id=?"), &[json!(id)]).await
}
async fn order(db: &mut MySqlConnection, id: &str) -> Result<Value> {
    one(db, &format!("{ORDER} WHERE id=?"), &[json!(id)]).await
}
async fn usable(db: &mut MySqlConnection, l: &Value) -> Result<bool> {
    Ok(!b(l, "deleted")
        && !b(l, "ended")
        && !b(l, "manual_paused")
        && n(l, "expires_at") > now()
        && (n(l, "traffic_limit_bytes") == 0
            || n(l, "used_up") + n(l, "used_down") < n(l, "traffic_limit_bytes"))
        && count(
            db,
            "SELECT COUNT(*) AS n FROM vp_users WHERE id=? AND disabled=0",
            &[l["user_id"].clone()],
        )
        .await?
            == 1)
}
fn in_ids(v: &Value, id: &str) -> bool {
    v.as_array()
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id)))
}
fn name_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= 160 && !v.chars().any(char::is_control)
}
impl Store {
    pub async fn open(c: &Value) -> Result<Arc<Self>> {
        let opt = MySqlConnectOptions::new()
            .host(s(c, "host"))
            .port(n(c, "port") as u16)
            .database(s(c, "name"))
            .username(s(c, "user"))
            .password(s(c, "password"))
            .charset("utf8mb4")
            .collation("utf8mb4_bin")
            .ssl_mode(if b(c, "tls") {
                MySqlSslMode::VerifyIdentity
            } else {
                MySqlSslMode::Disabled
            })
            .disable_statement_logging();
        let pool = MySqlPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with(opt)
            .await?;
        let mut db = pool.acquire().await?;
        for statement in concat!(include_str!("mysql.sql"), "\n", include_str!("web.sql"))
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            exec(&mut db, statement, &[]).await?;
        }
        if count(&mut db,"SELECT COUNT(*) AS n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name='vp_nodes' AND column_name='deleted'",&[]).await?==0{exec(&mut db,"ALTER TABLE vp_nodes ADD COLUMN deleted BOOLEAN NOT NULL DEFAULT 0",&[]).await?;}
        exec(&mut db,"CREATE TABLE IF NOT EXISTS vp_node_removals(node_id VARCHAR(80) COLLATE utf8mb4_bin PRIMARY KEY,nonce VARCHAR(80) NOT NULL,status VARCHAR(30) NOT NULL,requested_at BIGINT NOT NULL,FOREIGN KEY(node_id) REFERENCES vp_nodes(id)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",&[]).await?;
        // Probe reports can contain up to 8,192 checks, which exceed a TEXT column.
        if count(&mut db,"SELECT COUNT(*) AS n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name='vp_nodes' AND column_name='probe' AND data_type='text'",&[]).await?==1{exec(&mut db,"ALTER TABLE vp_nodes MODIFY probe MEDIUMTEXT NOT NULL",&[]).await?;}
        for table in ["vp_plans", "vp_leases"] {
            if count(&mut db, "SELECT COUNT(*) n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name=? AND column_name='deleted'", &[json!(table)]).await? == 0 {
                exec(&mut db, &format!("ALTER TABLE {table} ADD COLUMN deleted BOOLEAN NOT NULL DEFAULT 0"), &[]).await?;
            }
        }
        if count(&mut db, "SELECT COUNT(*) n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name='vp_nodes' AND column_name='active_rules'", &[]).await? == 0 {
            exec(&mut db, "ALTER TABLE vp_nodes ADD COLUMN active_rules MEDIUMTEXT NULL", &[]).await?;
        }
        if count(&mut db, "SELECT COUNT(*) n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name='vp_nodes' AND column_name='applied_rules'", &[]).await? == 0 {
            exec(&mut db, "ALTER TABLE vp_nodes ADD COLUMN applied_rules MEDIUMTEXT NULL", &[]).await?;
        }
        for table in ["vp_plans", "vp_leases"] {
            for field in ["bandwidth_mbps", "tcp_limit", "udp_limit"] {
                if count(&mut db, "SELECT COUNT(*) n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name=? AND column_name=?", &[json!(table),json!(field)]).await? == 0 {
                    exec(&mut db, &format!("ALTER TABLE {table} ADD COLUMN {field} INT NOT NULL DEFAULT 0"), &[]).await?;
                }
            }
        }
        if count(&mut db, "SELECT COUNT(*) n FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name='vp_nodes' AND column_name='supports_limits'", &[]).await? == 0 {
            exec(&mut db, "ALTER TABLE vp_nodes ADD COLUMN supports_limits BOOLEAN NOT NULL DEFAULT 0", &[]).await?;
        }
        exec(&mut db, "CREATE TABLE IF NOT EXISTS vp_resource_limits(lease_id VARCHAR(80) COLLATE utf8mb4_bin PRIMARY KEY,state MEDIUMTEXT NOT NULL,FOREIGN KEY(lease_id) REFERENCES vp_leases(id)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4", &[]).await?;
        drop(db);
        Ok(Arc::new(Self {
            pool,
            writer: Mutex::new(()),
        }))
    }
    pub async fn authenticate(&self, id: &str, token: &str) -> bool {
        if !valid_id(id) || !(40..=200).contains(&token.len()) {
            return false;
        }
        let r =
            sqlx::query("SELECT token_hash FROM vp_nodes WHERE id=? AND enabled=1 AND deleted=0")
                .bind(id)
                .fetch_optional(&self.pool)
                .await;
        if let Ok(Some(r)) = r
            && let Ok(h) = r.try_get::<Vec<u8>, _>(0)
        {
            return bool::from(
                h.as_slice()
                    .ct_eq(Sha256::digest(token.as_bytes()).as_slice()),
            );
        }
        false
    }
    pub async fn snapshot(&self) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        let users = rows(&mut tx, "SELECT id,name,disabled FROM vp_users", &[]).await?;
        let nodes = rows(&mut tx, NODES, &[]).await?;
        let pools = rows(
            &mut tx,
            "SELECT node_id,public_ip,bind_ip,start_port AS start,end_port AS end FROM vp_pools",
            &[],
        )
        .await?;
        let plans = rows(&mut tx, &format!("{PLAN} WHERE deleted=0"), &[]).await?;
        let leases = rows(&mut tx, &format!("{LEASE} WHERE l.deleted=0"), &[]).await?;
        let allocations = rows(&mut tx, &format!("{ALLOCATION} WHERE a.released=0"), &[]).await?;
        let orders = rows(&mut tx, &format!("{ORDER} ORDER BY created_at DESC"), &[]).await?;
        let revision = one(
            &mut tx,
            "SELECT value FROM vp_meta WHERE meta_key='revision'",
            &[],
        )
        .await?["value"]
            .clone();
        tx.commit().await?;
        Ok(
            json!({"users":users,"nodes":nodes,"pools":pools,"plans":plans,"leases":leases,"allocations":allocations,"orders":orders,"revision":revision}),
        )
    }
    pub async fn put_node(&self, id: &str, name: &str, token: &str) -> Result<()> {
        self.command(
            json!({"action":"node","node":{"id":id,"name":name,"enabled":true},"token":token}),
        )
        .await?;
        Ok(())
    }
    pub async fn command(&self, c: Value) -> Result<Value> {
        let _guard = self.writer.lock().await;
        let mut tx = self.pool.begin().await?;
        let out = self.command_tx(&mut tx, &c).await?;
        tx.commit().await?;
        Ok(out)
    }
    async fn command_tx(&self, db: &mut MySqlConnection, c: &Value) -> Result<Value> {
        let uid = s(c, "user_id");
        let cid = s(c, "id");
        let lid = s(c, "lease_id");
        match s(c, "action") {
            "export-node-rules" => return migration::export(db, c).await,
            "preview-node-migration" | "import-node-rules" => {
                return migration::migrate(db, c).await;
            }
            "delete-plan" => return lifecycle::delete_plan(db, c).await,
            "delete-lease" | "admin-delete-lease" => return lifecycle::delete_lease(db, c).await,
            "edit-node" => return lifecycle::edit_node(db, c).await,
            "user" => {
                let v = &c["user"];
                if !valid_id(s(v, "id")) || !name_ok(s(v, "name")) {
                    return Err(invalid());
                }
                exec(db,"INSERT INTO vp_users VALUES(?,?,?) ON DUPLICATE KEY UPDATE name=VALUES(name),disabled=VALUES(disabled)",&[v["id"].clone(),v["name"].clone(),json!(b(v,"disabled"))]).await?;
                bump(db).await?;
            }
            "node" => {
                let v = &c["node"];
                let tok = s(c, "token");
                if !valid_id(s(v, "id"))
                    || !name_ok(s(v, "name"))
                    || !(40..=200).contains(&tok.len())
                {
                    return Err(invalid());
                }
                sqlx::query("INSERT INTO vp_nodes(id,name,token_hash,enabled,probe,errors) VALUES(?,?,?,?,'{}','[]') ON DUPLICATE KEY UPDATE name=VALUES(name),token_hash=VALUES(token_hash),enabled=VALUES(enabled),deleted=0").bind(s(v,"id")).bind(s(v,"name")).bind(Sha256::digest(tok).to_vec()).bind(b(v,"enabled")).execute(&mut *db).await?;
                exec(
                    db,
                    "DELETE FROM vp_node_removals WHERE node_id=?",
                    &[v["id"].clone()],
                )
                .await?;
                bump(db).await?;
            }
            "node-status" => {
                let v = &c["node"];
                if !valid_id(s(v, "id")) || !name_ok(s(v, "name")) {
                    return Err(invalid());
                }
                one(
                    db,
                    "SELECT id FROM vp_nodes WHERE id=? AND deleted=0",
                    &[v["id"].clone()],
                )
                .await?;
                if count(
                    db,
                    "SELECT COUNT(*) AS n FROM vp_node_removals WHERE node_id=?",
                    &[v["id"].clone()],
                )
                .await?
                    > 0
                {
                    return Err(conflict());
                }
                exec(
                    db,
                    "UPDATE vp_nodes SET name=?,enabled=? WHERE id=?",
                    &[v["name"].clone(), json!(b(v, "enabled")), v["id"].clone()],
                )
                .await?;
                bump(db).await?;
            }
            "pool" | "remove-pool" => {
                let p = &c["pool"];
                let start = n(p, "start");
                let end = n(p, "end");
                for k in ["public_ip", "bind_ip"] {
                    if !s(p, k)
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| !ip.is_unspecified() && !ip.is_multicast())
                    {
                        return Err(invalid());
                    }
                }
                if start < 1024 || end > 65535 || end < start || end - start >= 10000 {
                    return Err(invalid());
                }
                if s(c, "action") == "pool" {
                    one(
                        db,
                        "SELECT id FROM vp_nodes WHERE id=? AND deleted=0",
                        &[p["node_id"].clone()],
                    )
                    .await?;
                    if count(db,"SELECT COUNT(*) AS n FROM vp_pools WHERE node_id=? AND (public_ip=? OR bind_ip=?) AND start_port<=? AND end_port>=?",&[p["node_id"].clone(),p["public_ip"].clone(),p["bind_ip"].clone(),json!(end),json!(start)]).await?>0{return Err(conflict())}
                    exec(
                        db,
                        "INSERT INTO vp_pools VALUES(?,?,?,?,?)",
                        &[
                            p["node_id"].clone(),
                            p["public_ip"].clone(),
                            p["bind_ip"].clone(),
                            json!(start),
                            json!(end),
                        ],
                    )
                    .await?;
                } else {
                    if count(db,"SELECT COUNT(*) AS n FROM vp_allocations WHERE node_id=? AND public_ip=? AND port>=? AND port<=? AND released=0",&[p["node_id"].clone(),p["public_ip"].clone(),json!(start),json!(end)]).await?>0{return Err(conflict())}
                    exec(db,"DELETE FROM vp_pools WHERE node_id=? AND public_ip=? AND start_port=? AND end_port=?",&[p["node_id"].clone(),p["public_ip"].clone(),json!(start),json!(end)]).await?;
                }
                bump(db).await?;
            }
            "preview-plan" => {
                lifecycle::administrator(db, c).await?;
                return lifecycle::preview_plan(db, &c["plan"]).await;
            }
            "plan" => {
                let mut normalized = c["plan"].clone();
                if ["bandwidth_mbps", "tcp_limit", "udp_limit"]
                    .iter()
                    .any(|k| normalized.get(*k).is_none())
                {
                    let previous = rows(
                        db,
                        "SELECT bandwidth_mbps,tcp_limit,udp_limit FROM vp_plans WHERE id=?",
                        &[normalized["id"].clone()],
                    )
                    .await?;
                    if let Some(old) = previous.first() {
                        for key in ["bandwidth_mbps", "tcp_limit", "udp_limit"] {
                            if normalized.get(key).is_none() {
                                normalized[key] = old[key].clone();
                            }
                        }
                    }
                }
                let limits: Limits =
                    serde_json::from_value(normalized.clone()).map_err(|_| invalid())?;
                if !limits.valid() {
                    return Err(invalid());
                }
                for (key, value) in [
                    ("bandwidth_mbps", limits.bandwidth_mbps),
                    ("tcp_limit", limits.tcp_limit),
                    ("udp_limit", limits.udp_limit),
                ] {
                    normalized[key] = json!(value);
                }
                let p = &normalized;
                let ids = p["node_ids"].as_array().ok_or_else(invalid)?;
                if !valid_id(s(p, "id"))
                    || !name_ok(s(p, "name"))
                    || !(1..=500).contains(&n(p, "port_limit"))
                    || !(0..=1_000_000_000_000_000).contains(&n(p, "traffic_limit_bytes"))
                    || !(1..=366).contains(&n(p, "period_days"))
                    || !(0..=9999999).contains(&n(p, "price_cents"))
                    || !(0..=9999999).contains(&n(p, "reset_price_cents"))
                    || ids.is_empty()
                    || ids.len() > 50
                {
                    return Err(invalid());
                }
                let mut seen = HashSet::new();
                for node in ids {
                    if !seen.insert(node.as_str())
                        || count(
                            db,
                            "SELECT COUNT(*) AS n FROM vp_nodes WHERE id=? AND deleted=0",
                            std::slice::from_ref(node),
                        )
                        .await?
                            != 1
                    {
                        return Err(invalid());
                    }
                }
                if count(
                    db,
                    "SELECT COUNT(*) n FROM vp_plans WHERE id=? AND deleted=1",
                    &[p["id"].clone()],
                )
                .await?
                    > 0
                {
                    return Err(missing());
                }
                let impact = if b(c, "update_existing") {
                    lifecycle::administrator(db, c).await?;
                    let preview = lifecycle::preview_plan(db, p).await?;
                    if s(c, "preview_digest").is_empty()
                        || s(c, "preview_digest") != s(&preview, "digest")
                    {
                        return Err(Fault {
                            code: 409,
                            message: "套餐或规则已变化，请重新预览后确认",
                        }
                        .into());
                    }
                    Some(preview)
                } else {
                    None
                };
                exec(db,"INSERT INTO vp_plans(id,name,port_limit,traffic_limit,period_days,price_cents,reset_price_cents,node_ids,enabled,bandwidth_mbps,tcp_limit,udp_limit) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON DUPLICATE KEY UPDATE name=VALUES(name),port_limit=VALUES(port_limit),traffic_limit=VALUES(traffic_limit),period_days=VALUES(period_days),price_cents=VALUES(price_cents),reset_price_cents=VALUES(reset_price_cents),node_ids=VALUES(node_ids),enabled=VALUES(enabled),bandwidth_mbps=VALUES(bandwidth_mbps),tcp_limit=VALUES(tcp_limit),udp_limit=VALUES(udp_limit)",&["id","name","port_limit","traffic_limit_bytes","period_days","price_cents","reset_price_cents","node_ids","enabled","bandwidth_mbps","tcp_limit","udp_limit"].iter().map(|k|p[*k].clone()).collect::<Vec<_>>()).await?;
                if let Some(preview) = impact {
                    lifecycle::sync_leases(db, p, &preview).await?;
                }
                bump(db).await?;
            }
            "grant" => {
                let p = plan(db, s(c, "plan_id")).await?;
                if !b(&p, "enabled") || b(&p, "deleted") {
                    return Err(forbidden());
                }
                let l = billing::grant(db, uid, &p).await?;
                bump(db).await?;
                return Ok(l);
            }
            "lease" => return lease(db, lid).await,
            "pause" | "end-lease" | "expiry" => {
                let l = lease(db, lid).await?;
                if b(&l, "deleted") {
                    return Err(missing());
                }
                match s(c, "action") {
                    "pause" => {
                        exec(
                            db,
                            "UPDATE vp_leases SET manual_paused=? WHERE id=?",
                            &[json!(b(c, "paused")), json!(lid)],
                        )
                        .await?;
                    }
                    "end-lease" => {
                        exec(
                            db,
                            "UPDATE vp_allocations SET released=1 WHERE lease_id=?",
                            &[json!(lid)],
                        )
                        .await?;
                        exec(db, "UPDATE vp_leases SET ended=1 WHERE id=?", &[json!(lid)]).await?;
                    }
                    _ => {
                        let at = n(c, "expires");
                        if at <= now() || at > now() + 86400 * 3660 {
                            return Err(invalid());
                        }
                        if b(&l, "ended") {
                            return Err(forbidden());
                        }
                        exec(
                            db,
                            "UPDATE vp_leases SET expires_at=? WHERE id=?",
                            &[json!(at), json!(lid)],
                        )
                        .await?;
                    }
                }
                bump(db).await?;
            }
            "claim" => {
                let v = &c["claim"];
                let port = n(v, "port");
                if port != 0 && !(1024..=65535).contains(&port) {
                    return Err(invalid());
                }
                one(
                    db,
                    "SELECT id FROM vp_leases WHERE id=? FOR UPDATE",
                    &[v["lease_id"].clone()],
                )
                .await?;
                let l = lease(db, s(v, "lease_id")).await?;
                if s(&l, "user_id") != s(v, "user_id") {
                    return Err(forbidden());
                }
                if !usable(db, &l).await? || n(&l, "used_ports") >= n(&l, "port_limit") {
                    return Err(quota());
                }
                if !in_ids(&l["node_ids"], s(v, "node_id")) {
                    return Err(forbidden());
                }
                if count(db,"SELECT COUNT(*) AS n FROM vp_nodes n WHERE n.id=? AND n.enabled=1 AND n.deleted=0 AND NOT EXISTS(SELECT 1 FROM vp_node_removals d WHERE d.node_id=n.id)",&[v["node_id"].clone()]).await?!=1{return Err(forbidden())}
                let used = rows(
                    db,
                    "SELECT bind_ip,port FROM vp_allocations WHERE node_id=? AND released=0",
                    &[v["node_id"].clone()],
                )
                .await?;
                if used.len() >= 512 {
                    return Err(quota());
                }
                let used: HashSet<_> = used
                    .iter()
                    .map(|v| (s(v, "bind_ip").to_owned(), n(v, "port")))
                    .collect();
                let pools=rows(db,"SELECT bind_ip,start_port,end_port FROM vp_pools WHERE node_id=? AND public_ip=?",&[v["node_id"].clone(),v["public_ip"].clone()]).await?;
                use rand::seq::IteratorRandom;
                let free = pools
                    .iter()
                    .flat_map(|p| {
                        (n(p, "start_port")..=n(p, "end_port"))
                            .map(move |port| (s(p, "bind_ip").to_owned(), port))
                    })
                    .filter(|(ip, p)| {
                        (port == 0 || port == *p) && !used.contains(&(ip.clone(), *p))
                    })
                    .choose(&mut rand::rngs::OsRng)
                    .ok_or_else(conflict)?;
                let rid = id();
                exec(db,"INSERT INTO vp_allocations(id,lease_id,node_id,public_ip,bind_ip,port) VALUES(?,?,?,?,?,?)",&[json!(rid),l["id"].clone(),v["node_id"].clone(),v["public_ip"].clone(),json!(free.0),json!(free.1)]).await?;
                bump(db).await?;
                return allocation(db, &rid).await;
            }
            "target" => {
                let a = allocation(db, cid).await?;
                let l = lease(db, s(&a, "lease_id")).await?;
                if s(&l, "user_id") != uid || b(&a, "released") {
                    return Err(forbidden());
                }
                if !usable(db, &l).await? {
                    return Err(quota());
                }
                let balance = b(c, "load_balance");
                let targets: Vec<Target> = if balance {
                    serde_json::from_value(c["targets"].clone()).map_err(|_| invalid())?
                } else {
                    if !(1..=65535).contains(&n(c, "port")) {
                        return Err(invalid());
                    }
                    vec![Target {
                        host: s(c, "host").into(),
                        port: n(c, "port") as u16,
                    }]
                };
                let mut seen = HashSet::new();
                if (balance && !(2..=16).contains(&targets.len()))
                    || targets
                        .iter()
                        .any(|t| !valid_target(&t.host, t.port) || !seen.insert(t.clone()))
                {
                    return Err(invalid());
                }
                if balance {
                    let node = one(
                        db,
                        "SELECT agent_version FROM vp_nodes WHERE id=?",
                        &[a["node_id"].clone()],
                    )
                    .await?;
                    let parts: Vec<_> = s(&node, "agent_version").split('.').collect();
                    if parts
                        .first()
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0)
                        == 0
                        && parts
                            .get(1)
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(0)
                            < 2
                    {
                        return Err(forbidden());
                    }
                }
                exec(
                    db,
                    "UPDATE vp_allocations SET target_host=?,target_port=? WHERE id=?",
                    &[json!(targets[0].host), json!(targets[0].port), json!(cid)],
                )
                .await?;
                if balance {
                    exec(db,"INSERT INTO vp_allocation_targets VALUES(?,?) ON DUPLICATE KEY UPDATE targets=VALUES(targets)",&[json!(cid),json!(targets)]).await?;
                } else {
                    exec(
                        db,
                        "DELETE FROM vp_allocation_targets WHERE rule_id=?",
                        &[json!(cid)],
                    )
                    .await?;
                }
                bump(db).await?;
            }
            "release" => {
                let a = allocation(db, cid).await?;
                let l = lease(db, s(&a, "lease_id")).await?;
                if s(&l, "user_id") != uid {
                    return Err(forbidden());
                }
                exec(
                    db,
                    "UPDATE vp_allocations SET released=1,target_host='',target_port=0 WHERE id=?",
                    &[json!(cid)],
                )
                .await?;
                bump(db).await?;
            }
            "order" => {
                billing::tick(db).await?;
                return billing::new_order(db, c).await;
            }
            "cancel-order" => {
                let o = order(db, cid).await?;
                if s(&o, "user_id") != uid {
                    return Err(forbidden());
                }
                if s(&o, "status") != "pending" {
                    return Err(conflict());
                }
                exec(
                    db,
                    "UPDATE vp_orders SET status='cancelled' WHERE id=?",
                    &[json!(cid)],
                )
                .await?;
            }
            "admin-order-status" => {
                billing::tick(db).await?;
                return billing::manage(db, c).await;
            }
            "remove-node" => return self.request_removal(db, cid, b(c, "force")).await,
            _ => return Err(invalid()),
        }
        Ok(Value::Null)
    }
    pub async fn get_order(&self, id: &str) -> Result<Value> {
        order(&mut *self.pool.acquire().await?, id).await
    }
    pub async fn tick(&self) -> Result<()> {
        let _g = self.writer.lock().await;
        let mut tx = self.pool.begin().await?;
        billing::tick(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn rules(&self, node: &str) -> Result<Config> {
        let mut db = self.pool.acquire().await?;
        let revision = n(
            &one(
                &mut db,
                "SELECT value FROM vp_meta WHERE meta_key='revision'",
                &[],
            )
            .await?,
            "value",
        );
        let removed = rows(
            &mut db,
            "SELECT nonce FROM vp_node_removals WHERE node_id=?",
            &[json!(node)],
        )
        .await?;
        if let Some(r) = removed.first() {
            return Ok(Config {
                version: 1,
                revision,
                valid_for_seconds: 10,
                rules: vec![],
                decommission: s(r, "nonce").into(),
                ..Default::default()
            });
        }
        let sql = "SELECT a.id,l.user_id,l.id AS lease_id,l.current_cycle AS cycle_id,a.bind_ip AS listen_ip,a.port AS listen_port,a.target_host,a.target_port,l.expires_at,l.bandwidth_mbps,l.tcp_limit,l.udp_limit,COALESCE(at.targets,'[]') AS targets FROM vp_allocations a LEFT JOIN vp_allocation_targets at ON at.rule_id=a.id JOIN vp_nodes n ON n.id=a.node_id JOIN vp_leases l ON l.id=a.lease_id JOIN vp_cycles c ON c.id=l.current_cycle JOIN vp_users u ON u.id=l.user_id WHERE a.node_id=? AND n.enabled=1 AND n.deleted=0 AND a.released=0 AND a.target_host<>'' AND l.deleted=0 AND l.ended=0 AND l.manual_paused=0 AND l.expires_at>? AND u.disabled=0 AND (l.traffic_limit=0 OR c.up+c.down<l.traffic_limit)";
        let rules = rows(&mut db, sql, &[json!(node), json!(now())])
            .await?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<Rule>, _>>()?;
        Ok(Config {
            version: 1,
            revision,
            valid_for_seconds: 10,
            rules,
            ..Default::default()
        })
    }
    async fn request_removal(
        &self,
        db: &mut MySqlConnection,
        node: &str,
        force: bool,
    ) -> Result<Value> {
        if !valid_id(node) {
            return Err(invalid());
        }
        let v = one(
            db,
            "SELECT id,last_seen,enabled,agent_version FROM vp_nodes WHERE id=? AND deleted=0",
            &[json!(node)],
        )
        .await?;
        let online = b(&v, "enabled") && n(&v, "last_seen") > now() - 15;
        if online && !force {
            if !s(&v, "agent_version")
                .split('.')
                .take(2)
                .map(|s| s.parse::<u32>().unwrap_or(0))
                .collect::<Vec<_>>()
                .as_slice()
                .ge(&[0, 3])
            {
                return Err(Fault {
                    code: 409,
                    message: "upgrade agent before removal",
                }
                .into());
            }
            exec(db,"INSERT INTO vp_node_removals VALUES(?,?,'waiting',?) ON DUPLICATE KEY UPDATE requested_at=VALUES(requested_at)",&[json!(node),json!(id()),json!(now())]).await?;
            bump(db).await?;
            audit(
                db,
                "node_remove_requested",
                node,
                "waiting for agent acknowledgement",
            )
            .await?;
            Ok(json!({"pending":true,"online":true}))
        } else {
            self.finalize_removal(db, node).await?;
            Ok(json!({"pending":false,"online":false}))
        }
    }
    async fn finalize_removal(&self, db: &mut MySqlConnection, node: &str) -> Result<()> {
        exec(
            db,
            "UPDATE vp_allocations SET released=1 WHERE node_id=?",
            &[json!(node)],
        )
        .await?;
        exec(db, "DELETE FROM vp_pools WHERE node_id=?", &[json!(node)]).await?;
        for table in ["vp_plans", "vp_leases"] {
            for mut row in rows(db, &format!("SELECT id,node_ids FROM {table}"), &[]).await? {
                if !in_ids(&row["node_ids"], node) {
                    continue;
                }
                let list: Vec<_> = row["node_ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|v| v.as_str() != Some(node))
                    .cloned()
                    .collect();
                row["node_ids"] = json!(list);
                exec(
                    db,
                    &format!("UPDATE {table} SET node_ids=? WHERE id=?"),
                    &[row["node_ids"].clone(), row["id"].clone()],
                )
                .await?;
                if list.is_empty() {
                    let set = if table == "vp_plans" {
                        "enabled=0"
                    } else {
                        "manual_paused=1"
                    };
                    exec(
                        db,
                        &format!("UPDATE {table} SET {set} WHERE id=?"),
                        &[row["id"].clone()],
                    )
                    .await?;
                }
            }
        }
        sqlx::query("UPDATE vp_nodes SET enabled=0,deleted=1,token_hash=?,probe='{}',errors='[]' WHERE id=?").bind(Sha256::digest(token()).to_vec()).bind(node).execute(&mut *db).await?;
        exec(
            db,
            "UPDATE vp_node_removals SET status='complete' WHERE node_id=?",
            &[json!(node)],
        )
        .await?;
        exec(db, "DELETE FROM vp_metadata WHERE id=?", &[json!(node)]).await?;
        audit(
            db,
            "node_removed",
            node,
            "allocations released; accounting history retained",
        )
        .await?;
        bump(db).await
    }
    pub async fn finish_removal(&self, node: &str, nonce: &str) -> Result<()> {
        let _g = self.writer.lock().await;
        let mut tx = self.pool.begin().await?;
        if count(
            &mut tx,
            "SELECT COUNT(*) AS n FROM vp_node_removals WHERE node_id=? AND nonce=?",
            &[json!(node), json!(nonce)],
        )
        .await?
            != 1
        {
            return Err(forbidden());
        }
        self.finalize_removal(&mut tx, node).await?;
        tx.commit().await?;
        Ok(())
    }
}
