use super::*;
use auth::{seal, unseal};
use hmac::{Hmac, Mac};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use rand::Rng;
use sha2::{Digest, Sha256};
pub fn email(v: &Value) -> Result<String> {
    let e = text(v, "email", 150, true)?.to_lowercase();
    e.parse::<lettre::Address>()
        .map_err(|_| fail(400, "请填写有效的邮箱地址"))?;
    Ok(e)
}
pub fn config(app: &App) -> Result<Value> {
    let p = app.root.join("state/mail.json");
    if p.exists() {
        Ok(serde_json::from_slice(&std::fs::read(p)?)?)
    } else {
        Ok(
            json!({"enabled":false,"host":"","port":465,"security":"ssl","username":"","password":"","from_email":"","from_name":""}),
        )
    }
}
pub fn safe(app: &App) -> Result<Value> {
    let mut c = config(app)?;
    c["has_password"] = json!(!s(&c, "password").is_empty());
    c.as_object_mut().unwrap().remove("password");
    Ok(c)
}
pub async fn send(app: &App, to: &str, subject: &str, body: &str) -> Result<()> {
    let c = config(app)?;
    if !b(&c, "enabled") {
        return Err(fail(503, "邮件服务尚未启用"));
    }
    let result = async {
        let tls = match s(&c, "security") {
            "ssl" => Tls::Wrapper(TlsParameters::new(s(&c, "host").to_owned())?),
            "tls" => Tls::Required(TlsParameters::new(s(&c, "host").to_owned())?),
            "none" if ["127.0.0.1", "::1"].contains(&s(&c, "host")) => Tls::None,
            _ => return Err(fail(400, "请选择 SSL 或 STARTTLS 加密")),
        };
        let mut transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(s(&c, "host"))
            .port(n(&c, "port") as u16)
            .tls(tls)
            .timeout(Some(std::time::Duration::from_secs(12)));
        if !s(&c, "username").is_empty() {
            transport = transport.credentials(Credentials::new(
                s(&c, "username").into(),
                if s(&c, "password").is_empty() {
                    String::new()
                } else {
                    unseal(app, s(&c, "password"))?
                },
            ));
        }
        let from = lettre::message::Mailbox::new(
            Some(s(&c, "from_name").into()),
            s(&c, "from_email").parse()?,
        );
        let message = Message::builder()
            .from(from)
            .to(to.parse()?)
            .subject(subject)
            .body(body.to_string())?;
        transport.build().send(message).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    result.map_err(|_| {
        eprintln!("SMTP delivery failed");
        fail(503, "邮件发送失败，请稍后重试或联系管理员")
    })
}
fn code_hash(app: &App, email: &str, purpose: &str, code: &str) -> Result<String> {
    let key = std::fs::read(app.root.join("state/app.key"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key)?;
    mac.update(format!("{purpose}|{email}|{code}").as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}
pub async fn send_code(ctx: &Context<'_>, v: &Value) -> Result<Value> {
    let email = email(v)?;
    let purpose = if s(v, "purpose").is_empty() {
        "register"
    } else {
        s(v, "purpose")
    };
    if !["register", "reset"].contains(&purpose) {
        return Err(fail(400, "验证码用途不正确"));
    }
    if purpose == "register"
        && (!setting(ctx.app, "registration", json!(false))
            .await?
            .as_bool()
            .unwrap_or(false)
            || !setting(ctx.app, "registration_email_verification", json!(false))
                .await?
                .as_bool()
                .unwrap_or(false))
    {
        return Err(fail(403, "当前不需要注册验证码"));
    }
    if !b(&config(ctx.app)?, "enabled") {
        return Err(fail(503, "邮件服务尚未启用"));
    }
    rate(ctx.app, &format!("email-hour:{}", ctx.ip), 12, 3600).await?;
    let id = hex::encode(Sha256::digest(format!("{purpose}|{email}")));
    let ip = hex::encode(Sha256::digest(format!("ip|{}", ctx.ip)));
    let at = now();
    let code = format!("{:06}", rand::rngs::OsRng.gen_range(0..1_000_000u32));
    let hash = code_hash(ctx.app, &email, purpose, &code)?;
    let mut tx = ctx.app.store.pool.begin().await?;
    // Every sender locks IP then destination, avoiding inverse lock ordering.
    exec(&mut tx,"INSERT IGNORE INTO vp_email_codes(id,email,purpose,code_hash,expires_at,last_sent,attempts,used) VALUES(?,'','ip','',0,0,0,1)",&[json!(ip)]).await?;
    exec(&mut tx,"INSERT IGNORE INTO vp_email_codes(id,email,purpose,code_hash,expires_at,last_sent,attempts,used) VALUES(?,?,?,'',0,0,0,0)",&[json!(id),json!(email),json!(purpose)]).await?;
    let a = one(
        &mut tx,
        "SELECT last_sent FROM vp_email_codes WHERE id=? FOR UPDATE",
        &[json!(ip)],
    )
    .await?;
    let e = one(
        &mut tx,
        "SELECT last_sent FROM vp_email_codes WHERE id=? FOR UPDATE",
        &[json!(id)],
    )
    .await?;
    let last = n(&a, "last_sent").max(n(&e, "last_sent"));
    if last > at - 60 {
        return Err(fail(
            429,
            format!("请在 {} 秒后重新获取验证码", last + 60 - at),
        ));
    }
    exec(
        &mut tx,
        "UPDATE vp_email_codes SET last_sent=?,expires_at=? WHERE id=?",
        &[json!(at), json!(at + 600), json!(ip)],
    )
    .await?;
    exec(&mut tx,"UPDATE vp_email_codes SET code_hash=?,expires_at=?,last_sent=?,attempts=0,used=0 WHERE id=?",&[json!(hash),json!(at+600),json!(at),json!(id)]).await?;
    tx.commit().await?;
    let should_send = {
        let mut db = ctx.app.store.pool.acquire().await?;
        if purpose == "reset" {
            count(&mut db,"SELECT COUNT(*) n FROM vp_accounts WHERE email=? AND email_verified=1 AND disabled=0",&[json!(email)]).await?>0
        } else {
            count(
                &mut db,
                "SELECT COUNT(*) n FROM vp_accounts WHERE email=?",
                &[json!(email)],
            )
            .await?
                == 0
        }
    };
    if should_send {
        let site = setting(ctx.app, "site_name", json!("Vistart Ports")).await?;
        send(ctx.app,&email,&format!("{} · 验证码",site.as_str().unwrap_or("Vistart Ports")),&format!("你的{}验证码为：{code}\n\n10 分钟内有效，请勿向他人透露。\n如非本人操作，请忽略本邮件。",if purpose=="register"{"注册"}else{"密码重置"})).await?;
    }
    Ok(json!({"ok":true,"retry_after":60,"message":"如邮箱符合条件，验证码将发送至该邮箱"}))
}
pub async fn verify_code(app: &App, email: &str, purpose: &str, code: &str) -> Result<()> {
    let id = hex::encode(Sha256::digest(format!("{purpose}|{email}")));
    let mut tx = app.store.pool.begin().await?;
    let list = rows(
        &mut tx,
        "SELECT * FROM vp_email_codes WHERE id=? FOR UPDATE",
        &[json!(id)],
    )
    .await?;
    let Some(v) = list.first() else {
        return Err(fail(400, "验证码已失效，请重新获取"));
    };
    if b(v, "used") || n(v, "expires_at") < now() || n(v, "attempts") >= 5 {
        return Err(fail(400, "验证码已失效，请重新获取"));
    }
    exec(
        &mut tx,
        "UPDATE vp_email_codes SET attempts=attempts+1 WHERE id=?",
        &[json!(id)],
    )
    .await?;
    let correct = code.len() == 6
        && code.bytes().all(|b| b.is_ascii_digit())
        && bool::from(
            s(v, "code_hash")
                .as_bytes()
                .ct_eq(code_hash(app, email, purpose, code)?.as_bytes()),
        );
    if correct {
        exec(
            &mut tx,
            "UPDATE vp_email_codes SET used=1 WHERE id=?",
            &[json!(id)],
        )
        .await?;
    }
    tx.commit().await?;
    if !correct {
        return Err(fail(400, "验证码不正确"));
    }
    Ok(())
}
pub async fn save(ctx: &Context<'_>, v: &Value) -> Result<Value> {
    let mut c = config(ctx.app)?;
    let enabled = b(v, "enabled");
    c["enabled"] = json!(enabled);
    for (k, max, req) in [
        ("host", 253, enabled),
        ("username", 150, false),
        ("from_email", 150, enabled),
        ("from_name", 60, enabled),
    ] {
        c[k] = json!(text(v, k, max, req)?);
    }
    c["port"] = json!(integer(v, "port", 1, 65535)?);
    c["security"] = json!(text(v, "security", 10, true)?);
    if enabled {
        s(&c, "from_email")
            .parse::<lettre::Address>()
            .map_err(|_| fail(400, "发件邮箱格式不正确"))?;
        if !s(&c, "host")
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
        {
            return Err(fail(400, "SMTP 地址不正确"));
        }
    }
    if !["ssl", "tls"].contains(&s(&c, "security"))
        && !(s(&c, "security") == "none" && ["127.0.0.1", "::1"].contains(&s(&c, "host")))
    {
        return Err(fail(400, "请选择 SSL 或 STARTTLS 加密"));
    }
    if !s(v, "password").is_empty() {
        if s(v, "password").len() > 512 {
            return Err(fail(400, "SMTP 密码格式不正确"));
        }
        c["password"] = json!(seal(ctx.app, s(v, "password"))?);
    }
    if !enabled
        && setting(ctx.app, "registration_email_verification", json!(false))
            .await?
            .as_bool()
            == Some(true)
    {
        return Err(fail(400, "请先关闭注册邮箱验证，再停用邮件服务"));
    }
    atomic_write(
        &ctx.app.root.join("state/mail.json"),
        &serde_json::to_vec_pretty(&c)?,
        0o600,
    )?;
    ctx.audit("修改邮件配置", "", "").await?;
    Ok(json!({"ok":true}))
}
