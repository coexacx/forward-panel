use super::*;
use rand::RngCore;
pub async fn initialize(
    root: &std::path::Path,
    store: &crate::store::Store,
    v: &Value,
) -> Result<()> {
    let name = text(v, "name", 60, true)?;
    let username = text(v, "username", 150, true)?.to_ascii_lowercase();
    if username.len() < 3
        || !username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@+-".contains(&b))
    {
        return Err(fail(400, "管理员用户名无效"));
    }
    let password = auth::password(v, "password")?;
    let hash = auth::hash_password(&password)?;
    let mut tx = store.pool.begin().await?;
    if count(&mut tx, "SELECT COUNT(*) n FROM vp_accounts", &[]).await? > 0 {
        return Err(fail(409, "已有账户，不能覆盖安装"));
    }
    let key = root.join("state/app.key");
    if !key.exists() {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        atomic_write(&key, &bytes, 0o600)?;
    }
    if std::fs::read(key)?.len() != 32 {
        return Err(fail(500, "加密密钥格式无效"));
    }
    let id = id();
    exec(
        &mut tx,
        "INSERT INTO vp_users(id,name,disabled) VALUES(?,?,0)",
        &[json!(id), json!(name)],
    )
    .await?;
    exec(&mut tx,"INSERT INTO vp_accounts(id,username,name,password_hash,role,disabled,created_at,auth_version) VALUES(?,?,?,?,'admin',0,?,1)",&[json!(id),json!(username),json!(name),json!(hash),json!(now())]).await?;
    for (k, v) in [
        ("site_name", json!(name)),
        ("registration", json!(false)),
        ("registration_email_verification", json!(false)),
    ] {
        exec(&mut tx,"INSERT INTO vp_settings(name,value) VALUES(?,?) ON DUPLICATE KEY UPDATE value=VALUES(value)",&[json!(k),json!(v.to_string())]).await?;
    }
    tx.commit().await?;
    Ok(())
}
pub async fn reset_password(store: &crate::store::Store, v: &Value) -> Result<()> {
    let username = text(v, "username", 150, true)?;
    let password = auth::password(v, "password")?;
    let hash = auth::hash_password(&password)?;
    let mut db = store.pool.acquire().await?;
    if exec(&mut db,"UPDATE vp_accounts SET password_hash=?,auth_version=auth_version+1 WHERE username=? AND role='admin' AND disabled=0",&[json!(hash),json!(username)]).await?!=1{return Err(fail(404,"启用的管理员账户不存在"))}
    Ok(())
}
