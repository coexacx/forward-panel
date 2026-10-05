use super::*;
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
pub fn seal(app: &App, plain: &str) -> Result<String> {
    let key = std::fs::read(app.root.join("state/app.key"))?;
    if key.len() != 32 {
        return Err(fail(503, "加密密钥不可用"));
    }
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| fail(503, "加密密钥不可用"))?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let mut encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain.as_bytes(),
                aad: b"vistart-v1",
            },
        )
        .map_err(|_| fail(500, "加密失败"))?;
    let tag = encrypted.split_off(encrypted.len() - 16);
    let mut out = nonce.to_vec();
    out.extend(tag);
    out.extend(encrypted);
    Ok(B64.encode(out))
}
pub fn unseal(app: &App, raw: &str) -> Result<String> {
    let raw = B64.decode(raw)?;
    if raw.len() < 29 {
        return Err(fail(503, "加密数据无效"));
    }
    let key = std::fs::read(app.root.join("state/app.key"))?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| fail(503, "加密密钥不可用"))?;
    let mut data = raw[28..].to_vec();
    data.extend_from_slice(&raw[12..28]);
    let clear = cipher
        .decrypt(
            Nonce::from_slice(&raw[..12]),
            Payload {
                msg: &data,
                aad: b"vistart-v1",
            },
        )
        .map_err(|_| fail(503, "加密数据校验失败"))?;
    Ok(String::from_utf8(clear)?)
}
pub fn password(v: &Value, k: &str) -> Result<String> {
    let p = v
        .get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| fail(400, "密码格式不正确"))?;
    if p.len() < 12 || p.len() > 128 || p.contains('\0') {
        return Err(fail(400, "密码应为 12–128 个字符"));
    }
    Ok(p.into())
}
pub fn hash_password(password: &str) -> Result<String> {
    let params =
        argon2::Params::new(32768, 3, 1, None).map_err(|_| fail(500, "密码加密参数无效"))?;
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    let hash = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| fail(500, "密码加密失败"))?;
    Ok(hash.to_string())
}
pub async fn make_password(app: &App, password: String) -> Result<String> {
    let _permit = app.password_slots.clone().acquire_owned().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = _permit;
        hash_password(&password)
    })
    .await?
}
pub async fn verify_password(app: &App, password: &str, hash: &str) -> Result<bool> {
    if password.len() > 128 {
        return Ok(false);
    }
    let _permit = app.password_slots.clone().acquire_owned().await?;
    let password = password.to_owned();
    let hash = hash.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = _permit;
        if let Some(hash) = hash.strip_prefix("{SHA256-BCRYPT}") {
            return bcrypt::verify(hex::encode(Sha256::digest(password)), hash).unwrap_or(false);
        }
        if hash.starts_with("$2") {
            return bcrypt::verify(password, &hash).unwrap_or(false);
        }
        if let Ok(parsed) = PasswordHash::new(&hash) {
            return Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok();
        }
        false
    })
    .await?)
}
pub fn safe_account(mut v: Value) -> Value {
    let two = b(&v, "two_factor") || !s(&v, "totp_secret").is_empty();
    if let Some(o) = v.as_object_mut() {
        for k in [
            "password_hash",
            "auth_version",
            "totp_secret",
            "totp_last",
            "recovery_hashes",
        ] {
            o.remove(k);
        }
    }
    v["two_factor"] = json!(two);
    v
}
pub async fn rate(app: &App, key: &str, max: i64, period: i64) -> Result<()> {
    let bucket = now() / period;
    let key = hex::encode(Sha256::digest(format!("{key}:{bucket}")));
    let mut db = app.store.pool.acquire().await?;
    exec(&mut db,"INSERT INTO vp_limits(id,attempts,expires_at) VALUES(?,1,?) ON DUPLICATE KEY UPDATE attempts=attempts+1",&[json!(key),json!(now()+period*2)]).await?;
    if n(
        &one(
            &mut db,
            "SELECT attempts FROM vp_limits WHERE id=?",
            &[json!(key)],
        )
        .await?,
        "attempts",
    ) > max
    {
        return Err(fail(429, "尝试过于频繁，请稍后再试"));
    }
    Ok(())
}
pub async fn check_password(ctx: &Context<'_>, v: &Value, key: &str) -> Result<()> {
    let a = ctx.user()?;
    rate(ctx.app, &format!("sensitive:{}", s(a, "id")), 15, 600).await?;
    if !verify_password(ctx.app, s(v, key), s(a, "password_hash")).await? {
        return Err(fail(400, "当前密码不正确"));
    }
    Ok(())
}
pub fn totp(secret: &str, step: i64) -> Result<String> {
    let secret = BASE32_NOPAD.decode(secret.to_ascii_uppercase().as_bytes())?;
    let mut mac = <Hmac<sha1::Sha1> as Mac>::new_from_slice(&secret)?;
    mac.update(&(step as u64).to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let off = (digest[19] & 15) as usize;
    let number = u32::from_be_bytes(digest[off..off + 4].try_into().unwrap()) & 0x7fffffff;
    Ok(format!("{:06}", number % 1_000_000))
}
pub fn totp_step(secret: &str, code: &str) -> Result<i64> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(-1);
    }
    for step in [now() / 30, now() / 30 - 1, now() / 30 + 1] {
        if bool::from(totp(secret, step)?.as_bytes().ct_eq(code.as_bytes())) {
            return Ok(step);
        }
    }
    Ok(-1)
}
pub async fn second_factor(app: &App, a: &Value, code: &str) -> Result<bool> {
    if s(a, "totp_secret").is_empty() {
        return Ok(false);
    }
    rate(app, &format!("totp:{}", s(a, "id")), 12, 600).await?;
    let step = totp_step(&unseal(app, s(a, "totp_secret"))?, code)?;
    let mut db = app.store.pool.acquire().await?;
    if step >= 0 {
        return Ok(exec(
            &mut db,
            "UPDATE vp_accounts SET totp_last=? WHERE id=? AND totp_last<?",
            &[json!(step), a["id"].clone(), json!(step)],
        )
        .await?
            == 1);
    }
    let code = code.replace([' ', '-'], "").to_ascii_lowercase();
    if code.len() != 20 || !code.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(false);
    }
    use sqlx::Acquire;
    let mut tx = db.begin().await?;
    let v = one(
        &mut tx,
        "SELECT recovery_hashes FROM vp_accounts WHERE id=? FOR UPDATE",
        &[a["id"].clone()],
    )
    .await?;
    let mut hashes: Vec<String> = serde_json::from_str(if s(&v, "recovery_hashes").is_empty() {
        "[]"
    } else {
        s(&v, "recovery_hashes")
    })?;
    let hash = hex::encode(Sha256::digest(&code));
    let found = hashes
        .iter()
        .position(|h| bool::from(h.as_bytes().ct_eq(hash.as_bytes())));
    if let Some(index) = found {
        hashes.remove(index);
        exec(
            &mut tx,
            "UPDATE vp_accounts SET recovery_hashes=? WHERE id=?",
            &[json!(hashes), a["id"].clone()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(found.is_some())
}
pub struct Session {
    pub token: String,
    pub original: String,
    pub csrf: String,
    pub user: Option<Value>,
    pub data: Value,
    pub version: i64,
    pub dirty: bool,
    pub cookie: bool,
}
impl Session {
    pub async fn load(app: &App, headers: &HeaderMap) -> Result<Self> {
        let raw = headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                s.split(';')
                    .map(str::trim)
                    .find_map(|p| p.strip_prefix("vistart_session="))
            })
            .unwrap_or("");
        if raw.len() == 64 && raw.bytes().all(|c| c.is_ascii_hexdigit()) {
            let hash = hex::encode(Sha256::digest(raw));
            let mut db = app.store.pool.acquire().await?;
            let list = rows(
                &mut db,
                "SELECT * FROM vp_sessions WHERE id=? AND expires_at>?",
                &[json!(hash), json!(now())],
            )
            .await?;
            if let Some(v) = list.first() {
                let user = if s(v, "user_id").is_empty() {
                    None
                } else {
                    rows(
                        &mut db,
                        "SELECT * FROM vp_accounts WHERE id=? AND disabled=0 AND auth_version=?",
                        &[v["user_id"].clone(), v["auth_version"].clone()],
                    )
                    .await?
                    .into_iter()
                    .next()
                };
                let data = serde_json::from_str(s(v, "data"))?;
                return Ok(Self {
                    token: raw.into(),
                    original: hash,
                    csrf: s(v, "csrf").into(),
                    user,
                    data,
                    version: n(v, "auth_version"),
                    dirty: n(v, "seen") < now() - 60,
                    cookie: false,
                });
            }
        }
        Ok(Self {
            token: crate::token(),
            original: String::new(),
            csrf: crate::token(),
            user: None,
            data: json!({}),
            version: 0,
            dirty: true,
            cookie: true,
        })
    }
    pub fn renew(&mut self, user: Option<Value>) {
        self.token = crate::token();
        self.csrf = crate::token();
        self.version = user.as_ref().map(|v| n(v, "auth_version")).unwrap_or(0);
        self.user = user;
        self.data = json!({});
        self.dirty = true;
        self.cookie = true;
    }
    pub async fn save(&mut self, app: &App) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut tx = app.store.pool.begin().await?;
        let hash = hex::encode(Sha256::digest(&self.token));
        let args = [
            json!(self.user.as_ref().map(|u| s(u, "id")).unwrap_or("")),
            json!(self.version),
            json!(self.csrf),
            json!(self.data.to_string()),
            json!(now()),
            json!(now() + 7200),
            json!(hash),
        ];
        if !self.original.is_empty()
            && self.original != hash
            && exec(
                &mut tx,
                "DELETE FROM vp_sessions WHERE id=?",
                &[json!(self.original)],
            )
            .await?
                != 1
        {
            return Err(fail(419, "会话已失效，请刷新重试"));
        }
        if !self.original.is_empty() && self.original == hash {
            if exec(&mut tx,"UPDATE vp_sessions SET user_id=?,auth_version=?,csrf=?,data=?,seen=?,expires_at=? WHERE id=?",&args).await?==0{
    // MySQL can report zero for an unchanged row; existence distinguishes revocation.
    if count(&mut tx,"SELECT COUNT(*) n FROM vp_sessions WHERE id=?",&[json!(hash)]).await?==0{return Err(fail(419,"会话已失效，请刷新重试"))}
   }
        } else {
            exec(&mut tx,"INSERT INTO vp_sessions(user_id,auth_version,csrf,data,seen,expires_at,id) VALUES(?,?,?,?,?,?,?)",&args).await?;
        }
        tx.commit().await?;
        self.original = hash;
        Ok(())
    }
}
