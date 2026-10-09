use crate::{
    deploy::Jobs,
    n, now,
    payment::{self, Provider},
    protocol::*,
    s,
    store::{Fault, Store},
};
use anyhow::{Result, bail};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, RawQuery, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration,
};
use tokio::sync::{Mutex, RwLock, Semaphore};
pub struct App {
    pub root: std::path::PathBuf,
    pub config: RwLock<Value>,
    pub password_slots: Arc<Semaphore>,
    pub request_slots: Semaphore,
    pub store: Arc<Store>,
    pub payment: RwLock<Option<Provider>>,
    pub jobs: Arc<Jobs>,
    connected: Mutex<HashSet<String>>,
    auth: Arc<Semaphore>,
}
fn reply(r: Result<Value>) -> Response {
    match r {
        Ok(v) => (
            [("Cache-Control", "no-store")],
            Json(json!({"ok":true,"data":v})),
        )
            .into_response(),
        Err(e) => {
            let (code, msg) = if let Some(e) = e.downcast_ref::<Fault>() {
                (e.code, e.message)
            } else if e
                .downcast_ref::<sqlx::Error>()
                .is_some_and(|x| matches!(x,sqlx::Error::Database(d) if d.is_unique_violation()))
            {
                (409, "resource conflict")
            } else {
                eprintln!("control operation failed: {}", e.root_cause());
                (500, "operation failed")
            };
            (
                StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                [("Cache-Control", "no-store")],
                Json(json!({"error":msg})),
            )
                .into_response()
        }
    }
}
async fn state(State(a): State<Arc<App>>) -> Response {
    reply(a.store.snapshot().await)
}
async fn snapshot(State(a): State<Arc<App>>) -> Response {
    reply(
        async {
            let state = a.store.snapshot().await?;
            let mut counts = json!({});
            for k in ["users", "nodes", "plans", "leases", "allocations", "orders"] {
                counts[k] = json!(state[k].as_array().map(Vec::len).unwrap_or(0));
            }
            Ok(json!({"nodes":state["nodes"],"counts":counts}))
        }
        .await,
    )
}
async fn jobs(State(a): State<Arc<App>>) -> Response {
    reply(Ok(json!(a.jobs.list().await)))
}
async fn command(State(a): State<Arc<App>>, Json(c): Json<Value>) -> Response {
    reply(dispatch(&a, c).await)
}
pub async fn dispatch(a: &App, c: Value) -> Result<Value> {
    match s(&c, "action") {
        "deploy" => a.jobs.start(c["deployment"].clone(), a.store.clone()).await,
        "payment-config" => {
            let p = Provider::new(c["payment"].clone())?;
            *a.payment.write().await = p;
            Ok(Value::Null)
        }
        "checkout" => {
            let o = a.store.get_order(s(&c, "id")).await?;
            if s(&o, "user_id") != s(&c, "user_id") || s(&o, "status") != "pending" {
                return Err(crate::store::forbidden());
            }
            {
                let mut db = a.store.pool.acquire().await?;
                if crate::store::count(
                    &mut db,
                    "SELECT COUNT(*) n FROM vp_plans WHERE id=? AND deleted=0",
                    &[o["plan_id"].clone()],
                )
                .await?
                    != 1
                {
                    return Err(crate::store::Fault {
                        code: 409,
                        message: "套餐已删除，无法继续支付",
                    }
                    .into());
                }
                if !s(&o, "lease_id").is_empty()
                    && crate::store::count(
                        &mut db,
                        "SELECT COUNT(*) n FROM vp_leases WHERE id=? AND deleted=0",
                        &[o["lease_id"].clone()],
                    )
                    .await?
                        != 1
                {
                    return Err(crate::store::Fault {
                        code: 409,
                        message: "用户套餐已删除，无法继续支付",
                    }
                    .into());
                }
            }
            if n(&o, "amount_cents") == 0 {
                a.store
                    .payment("free", &format!("free_{}", s(&o, "id")), s(&o, "id"), 0)
                    .await?;
                Ok(json!({"free":true}))
            } else {
                let guard = a.payment.read().await;
                let p = guard.as_ref().ok_or_else(crate::store::invalid)?;
                p.checkout(&o, s(&c, "method"))
            }
        }
        "probe" => {
            let snapshot = a.store.snapshot().await?;
            let mut allowed = HashSet::new();
            for l in snapshot["leases"].as_array().unwrap() {
                if s(l, "user_id") == s(&c, "user_id")
                    && !crate::b(l, "ended")
                    && n(l, "expires_at") > now()
                {
                    for id in l["node_ids"].as_array().unwrap() {
                        allowed.insert(id.clone());
                    }
                }
            }
            let nodes: Vec<_> = snapshot["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|n| allowed.contains(&n["id"]))
                .map(|v| {
                    let mut v = v.clone();
                    v["errors"] = Value::Null;
                    if let Some(p) = v["probe"].as_object_mut() {
                        p.remove("target_checks");
                    }
                    v
                })
                .collect();
            Ok(json!(nodes))
        }
        _ => a.store.command(c).await,
    }
}

async fn notify(State(a): State<Arc<App>>, RawQuery(raw): RawQuery) -> Response {
    let result = async {
        let v = payment::parse(raw.as_deref().unwrap_or(""))?;
        let (tid, oid, cents) = {
            let p = a.payment.read().await;
            p.as_ref().ok_or_else(crate::store::invalid)?.verify(&v)?
        };
        a.store.payment("epay", &tid, &oid, cents).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let (status, body) = match result {
        Ok(()) => (StatusCode::OK, "success"),
        Err(e) => {
            if e.downcast_ref::<Fault>().is_some() {
                (StatusCode::BAD_REQUEST, "invalid")
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "retry")
            }
        }
    };
    (
        status,
        [
            ("Cache-Control", "no-store"),
            ("Content-Type", "text/plain"),
        ],
        body,
    )
        .into_response()
}
async fn agent(
    State(a): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> Response {
    if !peer.ip().is_loopback() || !s(&*a.config.read().await, "origin").starts_with("https://") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let id = headers
        .get("X-Node-ID")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let token = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    let Ok(_permit) = a.auth.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    if headers.contains_key("Origin") || query.is_some() || !a.store.authenticate(&id, &token).await
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut map = a.connected.lock().await;
        if !map.insert(id.clone()) {
            return StatusCode::CONFLICT.into_response();
        }
    }
    let failure = a.clone();
    let failure_id = id.clone();
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_failed_upgrade(move |_| {
            tokio::spawn(async move {
                failure.connected.lock().await.remove(&failure_id);
            });
        })
        .on_upgrade(move |ws| async move {
            let _ = session(&a, &id, &token, ws).await;
            a.connected.lock().await.remove(&id);
        })
}
async fn session(a: &App, id: &str, token: &str, mut ws: WebSocket) -> Result<()> {
    let mut next = tokio::time::Instant::now();
    let mut previous: Option<Config> = None;
    let limit_session = crate::id();
    loop {
        tokio::time::sleep_until(next).await;
        next = tokio::time::Instant::now() + Duration::from_millis(500);
        let frame = tokio::time::timeout(Duration::from_secs(12), ws.recv())
            .await?
            .ok_or_else(crate::store::invalid)??;
        let Message::Text(raw) = frame else {
            bail!("unexpected agent frame")
        };
        if !a.store.authenticate(id, token).await {
            bail!("agent authentication revoked")
        }
        let r: Report = serde_json::from_str(&raw)?;
        a.store.report(id, &r).await?;
        let mut cfg = a.store.rules(id).await?;
        a.store
            .limit_config(id, &limit_session, &r, &mut cfg, previous.as_ref())
            .await?;
        let urls: Vec<String> =
            serde_json::from_value(a.config.read().await["controller_urls"].clone())?;
        if !urls.is_empty() {
            cfg.controller_urls = Some(urls);
        }

        cfg.ack_epoch = r.epoch;
        cfg.ack_sequence = r.sequence;
        let finish = !cfg.decommission.is_empty() && r.decommission_ack == cfg.decommission;
        let wire = if r.supports_delta
            && cfg.decommission.is_empty()
            && let Some(old) = &previous
            && old.revision == r.applied_revision
        {
            cfg.delta_from(old)
        } else {
            cfg.clone()
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            ws.send(Message::Text(serde_json::to_string(&wire)?.into())),
        )
        .await??;
        previous = Some(cfg.clone());
        if finish {
            a.store.finish_removal(id, &cfg.decommission).await?;
            return Ok(());
        }
    }
}
pub async fn run(mut config: Value) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listen: std::net::SocketAddr = s(&config, "listen").parse()?;
    let socket_path = std::path::PathBuf::from(s(&config, "socket"));
    let socket = socket_path.as_path();
    let root = socket
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("invalid socket path"))?;
    let urls: Vec<String> = serde_json::from_value(config["controller_urls"].clone())?;
    if !urls.is_empty() {
        crate::agent::client::validate_urls(&urls)?;
    }
    if s(&config, "origin").is_empty() {
        let path = root.join("state/panel.json");
        if path.exists() {
            let panel: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            config["origin"] = panel["origin"].clone();
        }
    }
    if s(&config, "origin").is_empty() {
        bail!("panel origin is required");
    }

    let origin = url::Url::parse(s(&config, "origin"))?;
    if !["http", "https"].contains(&origin.scheme())
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
        || origin.path() != "/"
    {
        bail!("canonical HTTP(S) origin required")
    }
    if origin.scheme() == "https" && !listen.ip().is_loopback() {
        bail!("HTTPS origin requires loopback listen behind a local reverse proxy")
    }
    config["origin"] = json!(origin.as_str().trim_end_matches('/'));
    let store = Store::open(&config["mysql"]).await?;
    let app = Arc::new(App {
        root: root.into(),
        config: RwLock::new(config.clone()),
        password_slots: Arc::new(Semaphore::new(2)),
        request_slots: Semaphore::new(128),
        store: store.clone(),
        payment: RwLock::new(Provider::new(config["payment"].clone())?),
        jobs: Jobs::new(root, config.clone())?,
        connected: Mutex::new(HashSet::new()),
        auth: Arc::new(Semaphore::new(64)),
    });
    let public = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok":true,"version":crate::VERSION,"runtime":"rust"})) }),
        )
        .merge(crate::web::router())
        .route("/control/agent", get(agent))
        .route("/control/payment/notify", get(notify))
        .layer(DefaultBodyLimit::max(65536))
        .with_state(app.clone());
    let private = Router::new()
        .route("/state", get(state))
        .route("/snapshot", get(snapshot))
        .route("/jobs", get(jobs))
        .route("/command", post(command))
        .layer(DefaultBodyLimit::max(65536))
        .with_state(app);
    std::fs::create_dir_all(socket.parent().unwrap())?;
    if let Ok(meta) = std::fs::symlink_metadata(socket) {
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            bail!("socket path occupied")
        }
        std::fs::remove_file(socket)?;
    }
    let uds = tokio::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let tcp = tokio::net::TcpListener::bind(listen).await?;
    let stop = tokio_util::sync::CancellationToken::new();
    let sig = stop.clone();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM");
        tokio::select! {_=tokio::signal::ctrl_c()=>(),_=term.recv()=>()};
        sig.cancel();
    });
    let maintenance = store.pool.clone();
    let c = stop.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(1));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_=c.cancelled()=>break,_=t.tick()=>{if store.tick().await.is_err(){eprintln!("billing cycle maintenance failed")}}}
        }
    });
    let c = stop.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            tokio::select! {_=c.cancelled()=>break,_=interval.tick()=>{
             for table in ["vp_sessions","vp_limits","vp_email_codes"]{
              let _=sqlx::query(&format!("DELETE FROM {table} WHERE expires_at<? LIMIT 5000")).bind(crate::now()-3600).execute(&maintenance).await;
             }
            }}
        }
    });
    eprintln!("Rust controller ready");
    let public = crate::web::server::serve(tcp, public, stop.clone());
    let private = axum::serve(uds, private).with_graceful_shutdown(stop.clone().cancelled_owned());
    tokio::select! {r=public=>r?,r=private=>r?,_=stop.cancelled()=>()}
    Ok(())
}
