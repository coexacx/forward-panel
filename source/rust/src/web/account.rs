use super::*;
use auth::*;
use data_encoding::BASE32_NOPAD;
use rand::RngCore;
pub(super) async fn create_account(
    ctx: &Context<'_>,
    v: &Value,
    role: &str,
    verify: bool,
) -> Result<Value> {
    let username = text(v, "username", 150, true)?.to_ascii_lowercase();
    if username.len() < 3
        || !username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@+-".contains(&b))
    {
        return Err(fail(400, "用户名应为 3–150 位字母、数字或邮箱"));
    }
    if !["admin", "user"].contains(&role) {
        return Err(fail(400, "账户角色不正确"));
    }
    let name = text(v, "name", 40, true)?;
    let password = password(v, "password")?;
    let email = if !s(v, "email").is_empty() {
        Some(mail::email(v)?)
    } else if username.parse::<lettre::Address>().is_ok() {
        Some(username.clone())
    } else {
        None
    };
    let verified =
        verify && setting(ctx.app, "registration_email_verification", json!(false)).await? == true;
    if verified {
        mail::verify_code(
            ctx.app,
            email
                .as_deref()
                .ok_or_else(|| fail(400, "请填写注册邮箱"))?,
            "register",
            &text(v, "email_code", 6, true)?,
        )
        .await?;
    }
    let hash = make_password(ctx.app, password).await?;
    let id = id();
    let mut tx = ctx.app.store.pool.begin().await?;
    if count(
        &mut tx,
        "SELECT COUNT(*) AS n FROM vp_accounts WHERE username=? OR (email IS NOT NULL AND email=?)",
        &[json!(username), json!(email)],
    )
    .await?
        > 0
    {
        return Err(fail(409, "用户名或邮箱已被使用"));
    }
    exec(
        &mut tx,
        "INSERT INTO vp_users VALUES(?,?,0)",
        &[json!(id), json!(name)],
    )
    .await?;
    exec(&mut tx,"INSERT INTO vp_accounts(id,username,name,password_hash,role,disabled,created_at,auth_version,email,email_verified) VALUES(?,?,?,?,?,0,?,1,?,?)",&[json!(id),json!(username),json!(name),json!(hash),json!(role),json!(now()),json!(email),json!(verified)]).await?;
    let a = one(
        &mut tx,
        "SELECT * FROM vp_accounts WHERE id=?",
        &[json!(id)],
    )
    .await?;
    tx.commit().await?;
    Ok(a)
}
pub(super) async fn public(ctx: &mut Context<'_>, route: &str, v: &Value) -> Result<Option<Value>> {
    let out = match route {
        "login" => {
            let username = text(v, "username", 150, true)?.to_ascii_lowercase();
            let password = s(v, "password");
            rate(ctx.app, &format!("login-ip:{}", ctx.ip), 80, 600).await?;
            rate(ctx.app, &format!("login-user:{username}"), 12, 600).await?;
            let accounts = {
                let mut db = ctx.app.store.pool.acquire().await?;
                rows(
                    &mut db,
                    "SELECT * FROM vp_accounts WHERE username=?",
                    &[json!(username)],
                )
                .await?
            };
            let dummy = "$2y$10$92IXUNpkjO0rOQ5byMi.Ye4oKoEa3Ro9llC/.og/at2uheWG/igi.";
            let valid = verify_password(
                ctx.app,
                password,
                accounts
                    .first()
                    .map(|a| s(a, "password_hash"))
                    .unwrap_or(dummy),
            )
            .await?;
            let a = accounts
                .first()
                .filter(|a| valid && !b(a, "disabled"))
                .ok_or_else(|| fail(401, "用户名或密码不正确"))?
                .clone();
            if !s(&a, "password_hash").starts_with("$argon2id$") {
                let hash = make_password(ctx.app, password.into()).await?;
                let mut db = ctx.app.store.pool.acquire().await?;
                exec(
                    &mut db,
                    "UPDATE vp_accounts SET password_hash=? WHERE id=?",
                    &[json!(hash), a["id"].clone()],
                )
                .await?;
            }
            if !s(&a, "totp_secret").is_empty() {
                ctx.session.renew(None);
                ctx.session.data = json!({"pending_login":{"id":a["id"],"expires":now()+300,"version":a["auth_version"]}});
                json!({"two_factor_required":true,"csrf":ctx.session.csrf})
            } else {
                ctx.session.renew(Some(a.clone()));
                ctx.audit("登录", s(&a, "id"), "账户登录").await?;
                json!({"ok":true,"user":safe_account(a),"csrf":ctx.session.csrf})
            }
        }
        "login/2fa" => {
            let pending = &ctx.session.data["pending_login"];
            if n(pending, "expires") < now() {
                return Err(fail(401, "验证已过期，请重新登录"));
            }
            let a = {
                let mut db = ctx.app.store.pool.acquire().await?;
                one(
                    &mut db,
                    "SELECT * FROM vp_accounts WHERE id=? AND disabled=0 AND auth_version=?",
                    &[pending["id"].clone(), pending["version"].clone()],
                )
                .await
                .map_err(|_| fail(401, "账户状态已改变，请重新登录"))?
            };
            if !second_factor(ctx.app, &a, &text(v, "code", 32, true)?).await? {
                return Err(fail(401, "二步验证码或恢复码不正确，或已使用"));
            }
            ctx.session.renew(Some(a.clone()));
            ctx.audit("二步验证登录", s(&a, "id"), "完成二步验证")
                .await?;
            json!({"ok":true,"user":safe_account(a),"csrf":ctx.session.csrf})
        }
        "register" => {
            if setting(ctx.app, "registration", json!(false)).await? != true {
                return Err(fail(403, "当前已关闭新用户注册"));
            }
            rate(ctx.app, &format!("register:{}", ctx.ip), 5, 3600).await?;
            let a = create_account(ctx, v, "user", true).await?;
            ctx.session.renew(Some(a.clone()));
            ctx.audit("注册", s(&a, "id"), "新用户注册").await?;
            json!({"ok":true,"user":safe_account(a),"csrf":ctx.session.csrf})
        }
        "logout" => {
            ctx.session.renew(None);
            json!({"ok":true,"csrf":ctx.session.csrf})
        }
        "email/code" => mail::send_code(ctx, v).await?,
        "password/reset" => {
            let email = mail::email(v)?;
            let password = password(v, "password")?;
            mail::verify_code(ctx.app, &email, "reset", &text(v, "email_code", 6, true)?).await?;
            let a = {
                let mut db = ctx.app.store.pool.acquire().await?;
                one(
                    &mut db,
                    "SELECT id FROM vp_accounts WHERE email=? AND email_verified=1 AND disabled=0",
                    &[json!(email)],
                )
                .await
                .map_err(|_| fail(400, "验证码或账户状态不正确"))?
            };
            let hash = make_password(ctx.app, password).await?;
            {
                let mut db = ctx.app.store.pool.acquire().await?;
                exec(
                    &mut db,
                    "UPDATE vp_accounts SET password_hash=?,auth_version=auth_version+1 WHERE id=?",
                    &[json!(hash), a["id"].clone()],
                )
                .await?;
            }
            ctx.audit("邮箱重置密码", s(&a, "id"), "已重置登录密码")
                .await?;
            json!({"ok":true})
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}
pub(super) async fn private(
    ctx: &mut Context<'_>,
    route: &str,
    v: &Value,
) -> Result<Option<Value>> {
    let a = ctx.user()?.clone();
    let uid = s(&a, "id");
    let out = match route {
        "2fa/setup" => {
            check_password(ctx, v, "password").await?;
            if !s(&a, "totp_secret").is_empty() {
                return Err(fail(400, "二步验证已经启用"));
            }
            let mut key = [0u8; 20];
            rand::rngs::OsRng.fill_bytes(&mut key);
            let secret = BASE32_NOPAD.encode(&key);
            ctx.session.data["totp_setup"] =
                json!({"secret":seal(ctx.app,&secret)?,"expires":now()+600,"uid":uid});
            ctx.session.dirty = true;
            let issuer = setting(ctx.app, "site_name", json!("Vistart Ports"))
                .await?
                .as_str()
                .unwrap_or("Vistart Ports")
                .to_owned();
            let mut uri = url::Url::parse("otpauth://totp/")?;
            uri.path_segments_mut()
                .map_err(|_| fail(500, "验证器地址无效"))?
                .push(&format!("{issuer}:{}", s(&a, "username")));
            uri.query_pairs_mut()
                .append_pair("secret", &secret)
                .append_pair("issuer", &issuer)
                .append_pair("algorithm", "SHA1")
                .append_pair("digits", "6")
                .append_pair("period", "30");
            json!({"secret":secret,"uri":uri.as_str()})
        }
        "2fa/enable" => {
            let p = &ctx.session.data["totp_setup"];
            if s(p, "uid") != uid || n(p, "expires") < now() {
                return Err(fail(400, "设置已过期，请重新开始"));
            }
            rate(ctx.app, &format!("2fa-setup:{uid}"), 10, 600).await?;
            let secret = unseal(ctx.app, s(p, "secret"))?;
            let step = totp_step(&secret, &text(v, "code", 6, true)?)?;
            if step < 0 {
                return Err(fail(400, "验证码不正确，请检查验证器时间"));
            }
            use sha2::{Digest, Sha256};
            let mut codes = Vec::new();
            let mut hashes = Vec::new();
            for _ in 0..8 {
                let mut bytes = [0u8; 10];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                let code = hex::encode(bytes);
                hashes.push(hex::encode(Sha256::digest(&code)));
                codes.push(
                    code.as_bytes()
                        .chunks(5)
                        .map(|v| std::str::from_utf8(v).unwrap())
                        .collect::<Vec<_>>()
                        .join("-"),
                );
            }
            let updated = {
                let mut db = ctx.app.store.pool.acquire().await?;
                if exec(&mut db,"UPDATE vp_accounts SET totp_secret=?,totp_last=?,recovery_hashes=?,auth_version=auth_version+1 WHERE id=? AND totp_secret IS NULL",&[json!(seal(ctx.app,&secret)?),json!(step),json!(hashes),json!(uid)]).await?!=1{return Err(fail(409,"二步验证已经启用"))}
                one(
                    &mut db,
                    "SELECT * FROM vp_accounts WHERE id=?",
                    &[json!(uid)],
                )
                .await?
            };
            ctx.session.renew(Some(updated));
            ctx.audit("启用二步验证", uid, "已绑定验证器").await?;
            json!({"ok":true,"recovery_codes":codes,"csrf":ctx.session.csrf})
        }
        "2fa/disable" => {
            check_password(ctx, v, "password").await?;
            if !second_factor(ctx.app, &a, &text(v, "code", 32, true)?).await? {
                return Err(fail(400, "二步验证码或恢复码不正确，或已使用"));
            }
            let updated = {
                let mut db = ctx.app.store.pool.acquire().await?;
                exec(&mut db,"UPDATE vp_accounts SET totp_secret=NULL,totp_last=-1,recovery_hashes=NULL,auth_version=auth_version+1 WHERE id=?",&[json!(uid)]).await?;
                one(
                    &mut db,
                    "SELECT * FROM vp_accounts WHERE id=?",
                    &[json!(uid)],
                )
                .await?
            };
            ctx.session.renew(Some(updated));
            ctx.audit("关闭二步验证", uid, "已解绑验证器").await?;
            json!({"ok":true,"csrf":ctx.session.csrf})
        }
        "account" => {
            let name = text(v, "name", 40, true)?;
            let hash = if !s(v, "new_password").is_empty() {
                let next = password(v, "new_password")?;
                check_password(ctx, v, "current_password").await?;
                if !s(&a, "totp_secret").is_empty()
                    && !second_factor(ctx.app, &a, &text(v, "code", 32, true)?).await?
                {
                    return Err(fail(400, "二步验证码或恢复码不正确"));
                }
                Some(make_password(ctx.app, next).await?)
            } else {
                None
            };
            let mut tx = ctx.app.store.pool.begin().await?;
            if let Some(hash) = &hash {
                exec(
                    &mut tx,
                    "UPDATE vp_accounts SET password_hash=?,auth_version=auth_version+1 WHERE id=?",
                    &[json!(hash), json!(uid)],
                )
                .await?;
            }
            exec(
                &mut tx,
                "UPDATE vp_accounts SET name=? WHERE id=?",
                &[json!(name), json!(uid)],
            )
            .await?;
            exec(
                &mut tx,
                "UPDATE vp_users SET name=? WHERE id=?",
                &[json!(name), json!(uid)],
            )
            .await?;
            let updated = one(
                &mut tx,
                "SELECT * FROM vp_accounts WHERE id=?",
                &[json!(uid)],
            )
            .await?;
            tx.commit().await?;
            if hash.is_some() {
                ctx.session.renew(Some(updated))
            } else {
                ctx.session.user = Some(updated)
            }
            ctx.audit("修改账户", uid, "更新账户资料").await?;
            json!({"ok":true,"csrf":ctx.session.csrf})
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}
