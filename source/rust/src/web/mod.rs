mod account;
mod admin;
pub mod auth;
pub mod install;
mod mail;
pub mod server;
mod state;
use crate::{
    atomic_write, b,
    control::App,
    id, n, now, s,
    store::{Fault, count, exec, one, rows},
};
use anyhow::Result;
use auth::{Session, rate, safe_account};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use subtle::ConstantTimeEq;
#[derive(Debug)]
pub struct ApiError {
    status: u16,
    message: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ApiError {}
pub fn fail(status: u16, message: impl Into<String>) -> anyhow::Error {
    ApiError {
        status,
        message: message.into(),
    }
    .into()
}
pub struct Context<'a> {
    app: &'a App,
    session: Session,
    ip: String,
}
impl Context<'_> {
    fn user(&self) -> Result<&Value> {
        self.session
            .user
            .as_ref()
            .ok_or_else(|| fail(401, "请先登录"))
    }
    fn admin(&self) -> Result<&Value> {
        let u = self.user()?;
        if s(u, "role") != "admin" {
            return Err(fail(403, "需要管理员权限"));
        }
        Ok(u)
    }
    async fn audit(&self, action: &str, subject: &str, detail: &str) -> Result<()> {
        let mut db = self.app.store.pool.acquire().await?;
        exec(
            &mut db,
            "INSERT INTO vp_web_audit(at,actor,subject,action,detail) VALUES(?,?,?,?,?)",
            &[
                json!(now()),
                json!(self.session.user.as_ref().map(|u| s(u, "id")).unwrap_or("")),
                json!(subject),
                json!(action),
                json!(detail.chars().take(300).collect::<String>()),
            ],
        )
        .await?;
        Ok(())
    }
}
fn text(v: &Value, key: &str, max: usize, required: bool) -> Result<String> {
    let value = match v.get(key) {
        None => "",
        Some(Value::String(v)) => v.as_str(),
        _ => return Err(fail(400, format!("字段格式不正确：{key}"))),
    };
    let value = value.trim();
    if required && value.is_empty()
        || value.chars().count() > max
        || value.chars().any(|c| c.is_control())
    {
        return Err(fail(400, format!("请填写有效的 {key}")));
    }
    Ok(value.into())
}
fn ident(v: &Value, key: &str) -> Result<String> {
    let id = text(v, key, 80, true)?;
    if !crate::valid_id(&id) {
        return Err(fail(400, "记录编号不正确"));
    }
    Ok(id)
}
fn integer(v: &Value, key: &str, min: i64, max: i64) -> Result<i64> {
    let value = v
        .get(key)
        .and_then(|v| {
            v.as_i64().or_else(|| {
                v.as_str()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse().ok())
            })
        })
        .ok_or_else(|| fail(400, format!("请填写有效的 {key}")))?;
    if value < min || value > max {
        return Err(fail(400, format!("{key} 超出允许范围")));
    }
    Ok(value)
}
pub async fn setting(app: &App, key: &str, default: Value) -> Result<Value> {
    let mut db = app.store.pool.acquire().await?;
    let list = rows(
        &mut db,
        "SELECT value FROM vp_settings WHERE name=?",
        &[json!(key)],
    )
    .await?;
    list.first()
        .map(|v| serde_json::from_str(s(v, "value")).map_err(Into::into))
        .unwrap_or(Ok(default))
}
async fn set_setting(app: &App, key: &str, value: Value) -> Result<()> {
    let mut db = app.store.pool.acquire().await?;
    exec(&mut db,"INSERT INTO vp_settings(name,value) VALUES(?,?) ON DUPLICATE KEY UPDATE value=VALUES(value)",&[json!(key),json!(value.to_string())]).await?;
    Ok(())
}
async fn metadata(app: &App, key: &str) -> Result<Value> {
    let mut db = app.store.pool.acquire().await?;
    let list = rows(
        &mut db,
        "SELECT value FROM vp_metadata WHERE id=?",
        &[json!(key)],
    )
    .await?;
    Ok(list
        .first()
        .and_then(|v| serde_json::from_str(s(v, "value")).ok())
        .unwrap_or(json!({})))
}
async fn meta_save(app: &App, key: &str, value: &Value) -> Result<()> {
    let mut db = app.store.pool.acquire().await?;
    exec(
        &mut db,
        "INSERT INTO vp_metadata(id,value) VALUES(?,?) ON DUPLICATE KEY UPDATE value=VALUES(value)",
        &[json!(key), json!(value.to_string())],
    )
    .await?;
    Ok(())
}
pub fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/api/{*route}", get(api).post(api))
        .fallback(get(page))
}
fn security(mut response: Response) -> Response {
    let h = response.headers_mut();
    for (k, v) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "same-origin"),
        ("cache-control", "no-store, private"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; form-action https:; frame-ancestors 'none'; base-uri 'none'; object-src 'none'",
        ),
    ] {
        h.insert(
            header::HeaderName::from_static(k),
            header::HeaderValue::from_static(v),
        );
    }
    response
}
fn error(e: anyhow::Error) -> Response {
    let (status, message) = if let Some(e) = e.downcast_ref::<ApiError>() {
        (e.status, e.message.clone())
    } else if let Some(e) = e.downcast_ref::<Fault>() {
        let message = match e.message {
            "invalid input" => "输入内容不符合要求",
            "not permitted" => "无权操作或资源已停用",
            "not found" => "记录不存在",
            "resource conflict" => "资源已被占用或有任务正在运行",
            "too many pending orders" => "待付款订单最多 20 笔，请先处理现有订单",
            "package quota exhausted" => "套餐已到期、暂停或额度不足",
            "invalid payment data" => "支付配置或所选付款方式不可用",
            "upgrade agent before removal" => "请先更新在线节点的 Agent，再执行删除",
            _ => "操作未完成",
        };
        (e.code, message.into())
    } else if e
        .downcast_ref::<sqlx::Error>()
        .is_some_and(|e| matches!(e,sqlx::Error::Database(d) if d.is_unique_violation()))
    {
        (409, "记录已存在，请刷新后重试".into())
    } else {
        eprintln!("web operation failed: {}", e.root_cause());
        (500, "操作未完成，请检查服务状态后重试".into())
    };
    security(
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"error":message})),
        )
            .into_response(),
    )
}
async fn api(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(route): Path<String>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let Ok(_slot) = app.request_slots.try_acquire() else {
        return error(fail(429, "请求较多，请稍后重试"));
    };
    let trusted = peer.ip().is_loopback();
    let ip = if trusted {
        headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .unwrap_or(peer.ip())
    } else {
        peer.ip()
    };
    let secure = trusted
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            == Some("https");
    let result = async {
        let body = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            axum::body::to_bytes(body, 65536),
        )
        .await
        .map_err(|_| fail(408, "请求超时"))?
        .map_err(|_| fail(413, "请求过大"))?;
        let config = app.config.read().await;
        if !host_allowed(&headers, s(&config, "origin")) {
            return Err(fail(421, "请使用配置的面板地址"));
        }
        drop(config);
        if body.len() > 65536 {
            return Err(fail(413, "请求过大"));
        }
        let session = Session::load(&app, &headers).await?;
        if session.original.is_empty() {
            rate(&app, &format!("new-session:{ip}"), 120, 600).await?;
        }
        let mut ctx = Context {
            app: &app,
            session,
            ip: ip.to_string(),
        };
        let value = if method == Method::POST {
            if !bool::from(
                ctx.session.csrf.as_bytes().ct_eq(
                    headers
                        .get("x-csrf-token")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .as_bytes(),
                ),
            ) {
                return Err(fail(419, "页面已过期，请刷新重试"));
            }
            let config = app.config.read().await;
            let origin = headers
                .get("origin")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !origin.is_empty() && origin != s(&config, "origin") {
                return Err(fail(403, "请求来源不正确"));
            }
            if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
                return Err(fail(403, "请求来源不正确"));
            }
            drop(config);
            let v: Value =
                serde_json::from_slice(&body).map_err(|_| fail(400, "请求格式不正确"))?;
            if !v.is_object() {
                return Err(fail(400, "请求格式不正确"));
            }
            v
        } else {
            Value::Null
        };
        let response = dispatch(
            &mut ctx,
            &route,
            query.as_deref().unwrap_or(""),
            &method,
            &value,
        )
        .await;
        // A created anonymous session must also survive a normal validation failure.
        ctx.session.save(&app).await?;
        let mut result = match response {
            Ok(v) => security(Json(v).into_response()),
            Err(e) => error(e),
        };
        if ctx.session.cookie {
            let cookie = format!(
                "vistart_session={}; Path=/; HttpOnly; SameSite=Lax{}",
                ctx.session.token,
                if secure { "; Secure" } else { "" }
            );
            result
                .headers_mut()
                .insert(header::SET_COOKIE, header::HeaderValue::from_str(&cookie)?);
        }
        Ok::<_, anyhow::Error>(result)
    }
    .await;
    match result {
        Ok(r) => r,
        Err(e) => error(e),
    }
}
async fn dispatch(
    ctx: &mut Context<'_>,
    route: &str,
    query: &str,
    method: &Method,
    v: &Value,
) -> Result<Value> {
    if route == "bootstrap" && method == Method::GET {
        return Ok(
            json!({"ready":true,"csrf":ctx.session.csrf,"user":ctx.session.user.clone().map(safe_account),"site_name":setting(ctx.app,"site_name",json!("Vistart Ports")).await?,"registration":setting(ctx.app,"registration",json!(false)).await?,"email_verification":setting(ctx.app,"registration_email_verification",json!(false)).await?,"mail_enabled":b(&mail::config(ctx.app)?,"enabled")}),
        );
    }
    if method == Method::POST
        && let Some(out) = account::public(ctx, route, v).await?
    {
        return Ok(out);
    }
    let user = ctx.user()?.clone();
    if route == "state" && method == Method::GET {
        let admin = url::form_urlencoded::parse(query.as_bytes())
            .any(|(k, v)| k == "scope" && v == "admin");
        if admin {
            ctx.admin()?;
        }
        return state::snapshot(ctx, admin).await;
    }
    if method != Method::POST {
        return Err(fail(404, "接口不存在"));
    }
    rate(ctx.app, &format!("action:{}", s(&user, "id")), 150, 60).await?;
    if let Some(out) = account::private(ctx, route, v).await? {
        return Ok(out);
    }
    match route {
        "order" => {
            let r=ctx.app.store.command(json!({"action":"order","user_id":user["id"],"plan_id":ident(v,"plan_id")?,"lease_id":text(v,"lease_id",80,false)?,"kind":if s(v,"kind")=="reset"{"reset"}else{"purchase"}})).await?;
            ctx.audit("创建订单", s(&user, "id"), s(&r, "id")).await?;
            return Ok(r);
        }
        "checkout" => {
            let out=crate::control::dispatch(ctx.app,json!({"action":"checkout","user_id":user["id"],"id":ident(v,"id")?,"method":text(v,"method",80,false)?})).await?;
            ctx.audit("发起支付", s(&user, "id"), s(v, "id")).await?;
            return Ok(out);
        }
        "cancel-order" | "release" => {
            let id = ident(v, "id")?;
            ctx.app
                .store
                .command(json!({"action":route,"id":id,"user_id":user["id"]}))
                .await?;
            ctx.audit(
                if route == "release" {
                    "释放端口"
                } else {
                    "取消订单"
                },
                s(&user, "id"),
                &id,
            )
            .await?;
            return Ok(json!({"ok":true}));
        }
        "claim" => {
            let target = targets(v)?;
            let remark = text(v, "remark", 120, false)?;
            let a=ctx.app.store.command(json!({"action":"claim","claim":{"user_id":user["id"],"lease_id":ident(v,"lease_id")?,"node_id":ident(v,"node_id")?,"public_ip":text(v,"public_ip",64,true)?,"port":integer(v,"port",0,65535)?}})).await?;
            let mut c = target;
            c["action"] = json!("target");
            c["user_id"] = user["id"].clone();
            c["id"] = a["id"].clone();
            if let Err(e) = ctx.app.store.command(c).await {
                ctx.app
                    .store
                    .command(json!({"action":"release","user_id":user["id"],"id":a["id"]}))
                    .await?;
                return Err(e);
            }
            meta_save(
                ctx.app,
                &format!("allocation_{}", s(&a, "id")),
                &json!({"remark":remark}),
            )
            .await?;
            ctx.audit(
                "添加转发",
                s(&user, "id"),
                &format!("{}:{}", s(&a, "public_ip"), n(&a, "port")),
            )
            .await?;
            return Ok(a);
        }
        "target" => {
            let mut c = targets(v)?;
            let id = ident(v, "id")?;
            c["action"] = json!("target");
            c["id"] = json!(id);
            c["user_id"] = user["id"].clone();
            ctx.app.store.command(c).await?;
            if v.get("remark").is_some() {
                meta_save(
                    ctx.app,
                    &format!("allocation_{id}"),
                    &json!({"remark":text(v,"remark",120,false)?}),
                )
                .await?;
            }
            ctx.audit("修改转发目标", s(&user, "id"), &id).await?;
            return Ok(json!({"ok":true}));
        }
        _ => (),
    }
    ctx.admin()?;
    admin::dispatch(ctx, route, v).await
}
fn targets(v: &Value) -> Result<Value> {
    if b(v, "load_balance") {
        let list = v["targets"]
            .as_array()
            .ok_or_else(|| fail(400, "目标格式不正确"))?;
        if !(2..=16).contains(&list.len()) {
            return Err(fail(400, "负载均衡需要 2–16 个目标"));
        }
        let ts = list
            .iter()
            .map(|t| Ok(json!({"host":text(t,"host",253,true)?,"port":integer(t,"port",1,65535)?})))
            .collect::<Result<Vec<Value>>>()?;
        Ok(json!({"host":ts[0]["host"],"port":ts[0]["port"],"load_balance":true,"targets":ts}))
    } else {
        Ok(
            json!({"host":text(v,"target_host",253,true)?,"port":integer(v,"target_port",1,65535)?,"load_balance":false,"targets":[]}),
        )
    }
}
include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
fn host_allowed(headers: &HeaderMap, origin: &str) -> bool {
    let Some(expected) = origin.split_once("://").map(|v| v.1) else {
        return false;
    };
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.eq_ignore_ascii_case(expected))
}
async fn page(State(app): State<Arc<App>>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    if !host_allowed(&headers, s(&*app.config.read().await, "origin")) {
        return error(fail(421, "请使用配置的面板地址"));
    }

    let path = uri.path();
    if path.starts_with("/assets/") {
        if let Some((_, mime, data)) = WEB_ASSETS.iter().find(|(name, _, _)| *name == path) {
            let mut r = security(([(header::CONTENT_TYPE, *mime)], *data).into_response());
            r.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
            return r;
        }
        return StatusCode::NOT_FOUND.into_response();
    }
    if ![
        "/",
        "/login",
        "/register",
        "/forgot-password",
        "/account",
        "/install",
    ]
    .contains(&path)
        && !path.starts_with("/app")
        && !path.starts_with("/admin")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    security(
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            WEB_HTML,
        )
            .into_response(),
    )
}
