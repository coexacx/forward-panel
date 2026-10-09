use super::*;
use crate::payment::Provider;
use auth::{check_password, make_password, password, second_factor};
pub async fn dispatch(ctx: &mut Context<'_>, route: &str, v: &Value) -> Result<Value> {
    let user = ctx.admin()?.clone();
    match route {
        "admin/export-rules" => {
            let nid = ident(v, "node_id")?;
            let payload = ctx
                .app
                .store
                .command(json!({"action":"export-node-rules","actor_id":user["id"],"node_id":nid}))
                .await?;
            let signature = sign_rules(ctx.app, &payload)?;
            ctx.audit("导出转发规则", "", &nid).await?;
            Ok(json!({"file":{"payload":payload,"signature":signature}}))
        }
        "admin/preview-rules" | "admin/import-rules" => {
            rate(ctx.app, &format!("rule-import:{}", s(&user, "id")), 10, 60).await?;
            let nid = ident(v, "node_id")?;
            let file = &v["file"]["payload"];
            verify_rules(ctx.app, file, s(&v["file"], "signature"))?;
            if s(file, "format") != "vistart-forward-rules" || n(file, "schema") != 1 {
                return Err(fail(400, "不支持的规则文件"));
            }
            use sha2::{Digest, Sha256};
            let file_hash = hex::encode(Sha256::digest(serde_json::to_vec(file)?));
            let request = migration_ssh(ctx.app, &nid).await?;
            let occupied = crate::deploy::occupied_ports(&ctx.app.root, &request)
                .await
                .map_err(|e| fail(400, format!("目标服务器检查失败：{e}")))?;
            let mut command = json!({"action":"preview-node-migration","actor_id":user["id"],"node_id":nid,
                "file":file,"file_hash":file_hash,"occupied_ports":occupied});
            if route == "admin/preview-rules" {
                let preview = ctx.app.store.command(command).await?;
                let token = auth::seal(
                    ctx.app,
                    &json!({"purpose":"rule-migration","actor_id":user["id"],
                    "file_hash":file_hash,"preview":preview,"expires":now()+300})
                    .to_string(),
                )?;
                Ok(json!({"preview":preview,"preview_token":token}))
            } else {
                let raw = text(v, "preview_token", 900_000, true)?;
                let approved: Value = serde_json::from_str(&auth::unseal(ctx.app, &raw)?)?;
                if s(&approved, "purpose") != "rule-migration"
                    || approved["actor_id"] != user["id"]
                    || s(&approved, "file_hash") != file_hash
                    || s(&approved["preview"], "node_id") != nid
                    || n(&approved, "expires") < now()
                {
                    return Err(fail(409, "预览已失效，请重新检查后确认"));
                }
                command["action"] = json!("import-node-rules");
                command["preview"] = approved["preview"].clone();
                let result = ctx.app.store.command(command).await?;
                ctx.audit(
                    "导入转发规则",
                    "",
                    &format!("{} → {nid}", s(&file["source_node"], "id")),
                )
                .await?;
                Ok(result)
            }
        }

        "admin/order-status" => {
            let id = ident(v, "id")?;
            let status = text(v, "status", 20, true)?;
            let note = text(v, "note", 200, true)?;
            if !["paid", "cancelled"].contains(&status.as_str()) {
                return Err(fail(400, "订单状态不正确"));
            }
            let o=ctx.app.store.command(json!({"action":"admin-order-status","actor_id":user["id"],"id":id,"status":status,"note":note})).await?;
            ctx.audit(
                if status == "paid" {
                    "管理员确认收款"
                } else {
                    "管理员取消订单"
                },
                s(&o, "user_id"),
                &format!("{id} · {note}"),
            )
            .await?;
            Ok(json!({"ok":true,"order":o}))
        }
        "admin/inspect-ssh" => {
            rate(ctx.app, &format!("ssh-inspect:{}", s(&user, "id")), 12, 60).await?;
            let req = json!({"ssh_host":text(v,"ssh_host",64,true)?,
                "ssh_port":integer(v,"ssh_port",1,65535)?,"username":text(v,"username",32,true)?});
            crate::deploy::inspect_host(&ctx.app.root, &req)
                .await
                .map_err(|e| fail(400, e.to_string()))
        }
        "admin/check-agent-release" => Ok(ctx.app.jobs.release_status(true).await),
        "admin/connection" | "admin/deploy" | "admin/update-agent" => {
            rate(ctx.app, &format!("ssh-operation:{}", s(&user, "id")), 8, 60).await?;
            let existing = !s(v, "node_id").is_empty();
            if route != "admin/deploy" && !existing {
                return Err(fail(400, "请选择转发服务器"));
            }
            let nid = if existing { ident(v, "node_id")? } else { id() };
            let meta = metadata(ctx.app, &nid).await?;
            let node = if existing {
                let mut db = ctx.app.store.pool.acquire().await?;
                let n = one(
                    &mut db,
                    "SELECT name,enabled,agent_version FROM vp_nodes WHERE id=? AND deleted=0",
                    &[json!(nid)],
                )
                .await?;
                if count(
                    &mut db,
                    "SELECT COUNT(*) n FROM vp_node_removals WHERE node_id=?",
                    &[json!(nid)],
                )
                .await?
                    > 0
                {
                    return Err(fail(409, "服务器正在移除"));
                }
                n
            } else {
                json!({})
            };
            let updating = route == "admin/update-agent";
            let input = if updating { &meta } else { v };
            let host = text(input, "ssh_host", 64, true)?;
            let port = integer(input, "ssh_port", 1, 65535)?;
            let username = text(input, "username", 32, true)?;
            let password = if !updating && !s(v, "password").is_empty() {
                s(v, "password").to_owned()
            } else {
                if s(&meta, "ssh_secret").is_empty() {
                    return Err(fail(400, "请先在连接信息中保存 SSH 密码"));
                }
                let secret: Value =
                    serde_json::from_str(&auth::unseal(ctx.app, s(&meta, "ssh_secret"))?)?;
                if s(&secret, "node_id") != nid
                    || s(&secret, "ssh_host") != host
                    || n(&secret, "ssh_port") != port
                    || s(&secret, "username") != username
                {
                    return Err(fail(400, "连接地址或用户名已改变，请重新填写 SSH 密码"));
                }
                s(&secret, "password").to_owned()
            };
            let name = if existing {
                s(&node, "name").to_owned()
            } else {
                text(v, "name", 40, true)?
            };
            let region = if existing {
                text(&meta, "region", 40, true)?
            } else {
                text(v, "region", 40, true)?
            };
            let public_ip = if existing {
                text(&meta, "public_ip", 64, true)?
            } else {
                text(v, "public_ip", 64, true)?
            };
            let secret = json!({"node_id":nid,"ssh_host":host,"ssh_port":port,"username":username,"password":password});
            let mut request = secret.clone();
            request["name"] = json!(name);
            request["region"] = json!(region);
            request["public_ip"] = json!(public_ip);
            request["sealed_password"] = json!(auth::seal(ctx.app, &secret.to_string())?);
            if !updating {
                request["confirmed_fingerprint"] =
                    json!(text(v, "confirmed_fingerprint", 100, false)?);
            }
            crate::deploy::validate(&request)
                .map_err(|_| fail(400, "请填写有效的 SSH 连接信息"))?;
            if updating {
                let config = ctx.app.config.read().await.clone();
                let version = crate::deploy::verified_version(&ctx.app.root, &config)
                    .await
                    .map_err(|_| fail(503, "无法验证最新版本，请稍后重试"))?;
                if !crate::deploy::newer_version(&version, s(&node, "agent_version")) {
                    return Err(fail(409, "未检测到可更新的 Agent 版本"));
                }
                request["expected_version"] = json!(version);
            }
            if route == "admin/connection" {
                crate::deploy::check_connection(&ctx.app.root, &request, &ctx.app.store)
                    .await
                    .map_err(|e| fail(400, e.to_string()))?;
                ctx.audit("保存 SSH 连接信息", "", &name).await?;
                if !b(v, "redeploy") {
                    return Ok(json!({"ok":true}));
                }
            }
            if ctx.app.config.read().await["controller_urls"]
                .as_array()
                .is_none_or(|a| a.is_empty())
            {
                return Err(fail(400, "请先设置有效 HTTPS 面板地址"));
            }
            let result = ctx.app.jobs.start(request, ctx.app.store.clone()).await?;
            ctx.audit(
                if updating {
                    "更新 Agent"
                } else {
                    "部署节点"
                },
                "",
                &name,
            )
            .await?;
            Ok(result)
        }
        "admin/node" => {
            let id = ident(v, "id")?;
            let name = text(v, "name", 40, true)?;
            let node = json!({"id":id,"name":name,"region":text(v,"region",40,true)?,"public_ip":text(v,"public_ip",64,true)?,"enabled":b(v,"enabled")});
            ctx.app
                .store
                .command(json!({"action":"edit-node","actor_id":user["id"],"node":node}))
                .await?;
            ctx.audit("编辑节点", "", &name).await?;
            Ok(json!({"ok":true}))
        }
        "admin/remove-node" => {
            let id = ident(v, "id")?;
            let node = {
                let mut db = ctx.app.store.pool.acquire().await?;
                one(
                    &mut db,
                    "SELECT name FROM vp_nodes WHERE id=? AND deleted=0",
                    &[json!(id)],
                )
                .await?
            };
            if text(v, "confirm_name", 100, true)? != s(&node, "name") {
                return Err(fail(400, "请输入正确的服务器名称"));
            }
            check_password(ctx, v, "password").await?;
            if !s(&user, "totp_secret").is_empty()
                && !second_factor(ctx.app, &user, &text(v, "code", 32, true)?).await?
            {
                return Err(fail(400, "二步验证码或恢复码不正确"));
            }
            let r = ctx
                .app
                .store
                .command(json!({"action":"remove-node","id":id}))
                .await?;
            ctx.audit("删除转发服务器", "", s(&node, "name")).await?;
            Ok(r)
        }
        "admin/release" => {
            let id = ident(v, "id")?;
            let a = {
                let mut db = ctx.app.store.pool.acquire().await?;
                one(&mut db,"SELECT l.user_id FROM vp_allocations a JOIN vp_leases l ON l.id=a.lease_id WHERE a.id=?",&[json!(id)]).await?
            };
            ctx.app
                .store
                .command(json!({"action":"release","id":id,"user_id":a["user_id"]}))
                .await?;
            ctx.audit("管理员释放端口", s(&a, "user_id"), &id).await?;
            Ok(json!({"ok":true}))
        }
        "admin/pool" => {
            let id = ident(v, "node_id")?;
            let meta = metadata(ctx.app, &id).await?;
            if s(&meta, "bind_ip").is_empty() {
                return Err(fail(400, "请先完成节点部署并取得网卡地址"));
            }
            let pool = json!({"node_id":id,"public_ip":text(v,"public_ip",64,true)?,"bind_ip":meta["bind_ip"],"start":integer(v,"start",1024,65535)?,"end":integer(v,"end",1024,65535)?});
            ctx.app
                .store
                .command(json!({"action":if b(v,"remove"){"remove-pool"}else{"pool"},"pool":pool}))
                .await?;
            ctx.audit(
                if b(v, "remove") {
                    "删除端口池"
                } else {
                    "添加端口池"
                },
                "",
                &format!("{id} {}–{}", n(&pool, "start"), n(&pool, "end")),
            )
            .await?;
            Ok(json!({"ok":true}))
        }
        "admin/plan" => {
            let ids = v["node_ids"]
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 50)
                .ok_or_else(|| fail(400, "请选择 1–50 台转发节点"))?;
            for n in ids {
                if !n.as_str().is_some_and(crate::valid_id) {
                    return Err(fail(400, "节点编号无效"));
                }
            }
            let p = json!({"id":if s(v,"id").is_empty(){id()}else{ident(v,"id")?},"name":text(v,"name",40,true)?,"port_limit":integer(v,"port_limit",1,500)?,"traffic_limit_bytes":integer(v,"traffic_limit_bytes",0,1_000_000_000_000_000)?,"period_days":integer(v,"period_days",1,366)?,"price_cents":integer(v,"price_cents",0,9_999_999)?,"reset_price_cents":integer(v,"reset_price_cents",0,9_999_999)?,"node_ids":ids,"enabled":b(v,"enabled")});
            ctx.app
                .store
                .command(json!({"action":"plan","plan":p,"update_existing":b(v,"update_existing")}))
                .await?;
            ctx.audit("保存套餐", "", s(&p, "name")).await?;
            Ok(p)
        }
        "admin/delete-plan" => {
            let id = ident(v, "id")?;
            let p = {
                let mut db = ctx.app.store.pool.acquire().await?;
                one(
                    &mut db,
                    "SELECT name FROM vp_plans WHERE id=?",
                    &[json!(id)],
                )
                .await?
            };
            if text(v, "confirm_name", 40, true)? != s(&p, "name") {
                return Err(fail(400, "请输入正确的套餐名称"));
            }
            let result = ctx
                .app
                .store
                .command(json!({"action":"delete-plan","actor_id":user["id"],"id":id}))
                .await?;
            ctx.audit("删除套餐商品", "", s(&p, "name")).await?;
            Ok(result)
        }
        "admin/delete-lease" => {
            let id = ident(v, "id")?;
            let l = ctx
                .app
                .store
                .command(json!({"action":"lease","lease_id":id}))
                .await?;
            if text(v, "confirm_name", 40, true)? != s(&l, "plan_name") {
                return Err(fail(400, "请输入正确的套餐名称"));
            }
            let result = ctx
                .app
                .store
                .command(json!({"action":"admin-delete-lease","actor_id":user["id"],"id":id}))
                .await?;
            ctx.audit("删除用户套餐", s(&l, "user_id"), &id).await?;
            Ok(result)
        }
        "admin/grant" => {
            let uid = ident(v, "user_id")?;
            let r = ctx
                .app
                .store
                .command(json!({"action":"grant","user_id":uid,"plan_id":ident(v,"plan_id")?}))
                .await?;
            ctx.audit("开通用户套餐", &uid, s(&r, "plan_name")).await?;
            Ok(r)
        }
        "admin/lease" => {
            let id = ident(v, "id")?;
            let action = text(v, "action", 30, true)?;
            if !["pause", "resume", "end", "expiry"].contains(&action.as_str()) {
                return Err(fail(400, "操作不正确"));
            }
            let l = ctx
                .app
                .store
                .command(json!({"action":"lease","lease_id":id}))
                .await?;
            let mut command = json!({"action":match action.as_str(){"end"=>"end-lease","expiry"=>"expiry",_=>"pause"},"lease_id":id,"paused":action=="pause"});
            if action == "expiry" {
                command["expires"] = json!(integer(v, "expires", now() + 1, now() + 86400 * 3660)?);
            }
            ctx.app.store.command(command).await?;
            ctx.audit("管理用户套餐", s(&l, "user_id"), &format!("{action} {id}"))
                .await?;
            Ok(json!({"ok":true}))
        }
        "admin/user" => {
            if s(v, "id").is_empty() {
                let a = account::create_account(ctx, v, &text(v, "role", 10, true)?, false).await?;
                ctx.audit("创建账户", s(&a, "id"), s(&a, "username"))
                    .await?;
                return Ok(safe_account(a));
            }
            let id = ident(v, "id")?;
            let name = text(v, "name", 40, true)?;
            let role = text(v, "role", 10, true)?;
            let disabled = b(v, "disabled");
            if !["admin", "user"].contains(&role.as_str()) {
                return Err(fail(400, "权限类型不正确"));
            }
            if id == s(&user, "id") && (disabled || role != "admin") {
                return Err(fail(400, "不能停用当前管理员或移除自己的管理权限"));
            }
            let hash = if !s(v, "password").is_empty() {
                Some(make_password(ctx.app, password(v, "password")?).await?)
            } else {
                None
            };
            let mut tx = ctx.app.store.pool.begin().await?;
            let admins=rows(&mut tx,"SELECT id FROM vp_accounts WHERE role='admin' AND disabled=0 ORDER BY id FOR UPDATE",&[]).await?;
            let a = one(
                &mut tx,
                "SELECT * FROM vp_accounts WHERE id=? FOR UPDATE",
                &[json!(id)],
            )
            .await?;
            if s(&a, "role") == "admin"
                && !b(&a, "disabled")
                && (disabled || role != "admin")
                && admins.len() < 2
            {
                return Err(fail(400, "必须保留至少一个启用的管理员"));
            }
            exec(&mut tx,"UPDATE vp_accounts SET name=?,role=?,disabled=?,auth_version=auth_version+1 WHERE id=?",&[json!(name),json!(role),json!(disabled),json!(id)]).await?;
            exec(
                &mut tx,
                "UPDATE vp_users SET name=?,disabled=? WHERE id=?",
                &[json!(name), json!(disabled), json!(id)],
            )
            .await?;
            if let Some(hash) = hash {
                exec(
                    &mut tx,
                    "UPDATE vp_accounts SET password_hash=? WHERE id=?",
                    &[json!(hash), json!(id)],
                )
                .await?;
            }
            let a = one(
                &mut tx,
                "SELECT * FROM vp_accounts WHERE id=?",
                &[json!(id)],
            )
            .await?;
            tx.commit().await?;
            if id == s(&user, "id") {
                ctx.session.renew(Some(a.clone()));
            }
            ctx.audit("管理账户", &id, s(&a, "username")).await?;
            Ok(json!({"ok":true,"csrf":ctx.session.csrf}))
        }
        "admin/settings" => {
            if b(v, "email_verification") && !b(&mail::config(ctx.app)?, "enabled") {
                return Err(fail(400, "请先启用并配置邮件服务"));
            }
            set_setting(ctx.app, "site_name", json!(text(v, "site_name", 60, true)?)).await?;
            set_setting(ctx.app, "registration", json!(b(v, "registration"))).await?;
            set_setting(
                ctx.app,
                "registration_email_verification",
                json!(b(v, "email_verification")),
            )
            .await?;
            ctx.audit("修改站点设置", "", "").await?;
            Ok(json!({"ok":true}))
        }
        "admin/mail" => mail::save(ctx, v).await,
        "admin/mail/test" => {
            rate(ctx.app, &format!("test-mail:{}", s(&user, "id")), 5, 600).await?;
            let to = mail::email(v)?;
            let site = setting(ctx.app, "site_name", json!("Vistart Ports")).await?;
            mail::send(
                ctx.app,
                &to,
                &format!(
                    "{} · 邮件配置测试",
                    site.as_str().unwrap_or("Vistart Ports")
                ),
                "这是一封邮件配置测试消息。\nSMTP 投递已成功。",
            )
            .await?;
            ctx.audit("发送测试邮件", "", &to).await?;
            Ok(json!({"ok":true}))
        }
        "admin/payment" => {
            let mut config = ctx.app.config.write().await;
            let mut p = config["payment"].clone();
            let enabled = b(v, "enabled");
            p["enabled"] = json!(enabled);
            p["gateway"] = json!(text(v, "gateway", 250, enabled)?.trim_end_matches('/'));
            p["merchant_id"] = json!(text(v, "merchant_id", 18, enabled)?);
            p["version"] = json!("v1");
            p["exclude_zero"] = json!(b(v, "exclude_zero"));
            p["public_origin"] = config["origin"].clone();
            if !s(v, "secret").is_empty() {
                p["secret"] = json!(text(v, "secret", 256, true)?);
            }
            let methods = v["methods"]
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 12)
                .ok_or_else(|| fail(400, "请设置 1–12 种支付方式"))?;
            p["methods"]=json!(methods.iter().map(|m|Ok(json!({"id":ident(m,"id")?,"name":text(m,"name",30,true)?,"type":text(m,"type",30,true)?,"enabled":b(m,"enabled")}))).collect::<Result<Vec<Value>>>()?);
            let provider = Provider::new(p.clone())?;
            let mut next = config.clone();
            next["payment"] = p;
            atomic_write(
                &ctx.app.root.join("state/controller.json"),
                &serde_json::to_vec_pretty(&next)?,
                0o600,
            )?;
            *config = next;
            *ctx.app.payment.write().await = provider;
            drop(config);
            ctx.audit("修改支付配置", "", "").await?;
            Ok(json!({"ok":true}))
        }
        _ => Err(fail(404, "接口不存在")),
    }
}

fn sign_rules(app: &App, payload: &Value) -> Result<String> {
    use hmac::{Hmac, Mac};
    let key = std::fs::read(app.root.join("state/app.key"))?;
    if key.len() != 32 {
        return Err(fail(503, "加密密钥不可用"));
    }
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&key).map_err(|_| fail(503, "签名失败"))?;
    mac.update(b"vistart-rule-export-v1\0");
    mac.update(&serde_json::to_vec(payload)?);
    Ok(hex::encode(mac.finalize().into_bytes()))
}
fn verify_rules(app: &App, payload: &Value, signature: &str) -> Result<()> {
    let expected = sign_rules(app, payload)?;
    if signature.len() != 64 || !bool::from(expected.as_bytes().ct_eq(signature.as_bytes())) {
        return Err(fail(400, "规则文件已被修改，或不属于当前面板"));
    }
    Ok(())
}
async fn migration_ssh(app: &App, nid: &str) -> Result<Value> {
    let meta = metadata(app, nid).await?;
    if s(&meta, "ssh_secret").is_empty() {
        return Err(fail(400, "请先在目标服务器的连接信息中保存 SSH 密码"));
    }
    let mut request: Value = serde_json::from_str(&auth::unseal(app, s(&meta, "ssh_secret"))?)?;
    if s(&request, "node_id") != nid
        || request["ssh_host"] != meta["ssh_host"]
        || request["ssh_port"] != meta["ssh_port"]
        || request["username"] != meta["username"]
    {
        return Err(fail(400, "目标服务器连接信息已改变，请重新保存 SSH 密码"));
    }
    for key in ["name", "region", "public_ip"] {
        request[key] = meta[key].clone();
    }
    Ok(request)
}
