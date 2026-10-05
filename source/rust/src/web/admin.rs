use super::*;
use crate::payment::Provider;
use auth::{check_password, make_password, password, second_factor};
pub async fn dispatch(ctx: &mut Context<'_>, route: &str, v: &Value) -> Result<Value> {
    let user = ctx.admin()?.clone();
    match route {
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
        "admin/deploy" => {
            let id = if s(v, "node_id").is_empty() {
                id()
            } else {
                ident(v, "node_id")?
            };
            if s(v, "password").len() > 512 {
                return Err(fail(400, "SSH 密码格式不正确"));
            }
            let request = json!({"node_id":id,"name":text(v,"name",40,true)?,"region":text(v,"region",40,true)?,"ssh_host":text(v,"ssh_host",64,true)?,"ssh_port":integer(v,"ssh_port",1,65535)?,"username":text(v,"username",32,true)?,"password":s(v,"password"),"public_ip":text(v,"public_ip",64,true)?});
            if ctx.app.config.read().await["controller_urls"]
                .as_array()
                .is_none_or(|a| a.is_empty())
            {
                return Err(fail(
                    400,
                    "请先在管理菜单设置 HTTPS 面板地址，并完成反向代理",
                ));
            }
            let result = ctx
                .app
                .jobs
                .start(request.clone(), ctx.app.store.clone())
                .await?;
            let mut meta = metadata(ctx.app, &id).await?;
            let mut saved = request.as_object().unwrap().clone();
            saved.remove("password");
            saved.remove("node_id");
            meta.as_object_mut().unwrap().extend(saved);
            meta_save(ctx.app, &id, &meta).await?;
            ctx.audit("部署节点", "", s(&request, "name")).await?;
            Ok(result)
        }
        "admin/node" => {
            let id = ident(v, "id")?;
            let name = text(v, "name", 40, true)?;
            ctx.app.store.command(json!({"action":"node-status","node":{"id":id,"name":name,"enabled":b(v,"enabled")}})).await?;
            let mut meta = metadata(ctx.app, &id).await?;
            meta["region"] = json!(text(v, "region", 40, true)?);
            meta_save(ctx.app, &id, &meta).await?;
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
                .command(json!({"action":"plan","plan":p}))
                .await?;
            ctx.audit("保存套餐", "", s(&p, "name")).await?;
            Ok(p)
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
