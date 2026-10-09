use crate::{
    atomic_write, b, id, n, now, s,
    store::{Store, conflict, invalid},
    token, valid_id,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use russh::{
    ChannelMsg, client,
    keys::{HashAlg, PublicKeyOrCertificate},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::Mutex;
pub struct Jobs {
    root: PathBuf,
    items: Mutex<BTreeMap<String, Value>>,
    release: Mutex<Value>,
}
impl Jobs {
    pub fn new(root: &Path, _config: Value) -> Result<Arc<Self>> {
        let file = root.join("state/jobs.json");
        let mut items: BTreeMap<String, Value> = if file.exists() {
            serde_json::from_slice(&std::fs::read(file)?)?
        } else {
            BTreeMap::new()
        };
        for v in items.values_mut() {
            if !b(v, "done") {
                v["done"] = json!(true);
                v["ok"] = json!(false);
                v["stage"] = json!("interrupted");
                v["message"] = json!("主控重启，部署已中断；可重新部署");
            }
        }
        Ok(Arc::new(Self {
            root: root.into(),
            items: Mutex::new(items),
            release: Mutex::new(json!({"status":"checking","checking":false,"checked_at":0})),
        }))
    }
    async fn save(&self, v: Value) -> Result<()> {
        let mut items = self.items.lock().await;
        items.insert(s(&v, "id").into(), v);
        atomic_write(
            &self.root.join("state/jobs.json"),
            &serde_json::to_vec(&*items)?,
            0o600,
        )
    }
    pub async fn list(&self) -> Vec<Value> {
        let mut items = self.items.lock().await;
        let before = items.len();
        items.retain(|_, v| {
            !b(v, "done")
                || if b(v, "ok") {
                    n(v, "finished_at") > now() - 5
                } else {
                    n(v, "at") > now() - 86400
                }
        });
        if items.len() != before {
            let _ = atomic_write(
                &self.root.join("state/jobs.json"),
                &serde_json::to_vec(&*items).unwrap_or_default(),
                0o600,
            );
        }
        items.values().cloned().collect()
    }
    pub async fn release_status(self: &Arc<Self>, force: bool) -> Value {
        let mut state = self.release.lock().await;
        let age = now() - n(&state, "checked_at");
        if !b(&state, "checking") && (age >= 3600 || (force && age >= 30)) {
            state["checking"] = json!(true);
            let jobs = self.clone();
            tokio::spawn(async move {
                let result = async {
                    let config: Value = serde_json::from_slice(&std::fs::read(
                        jobs.root.join("state/controller.json"),
                    )?)?;
                    let base = release_base(&config);
                    let (_, manifest) =
                        read_manifest(base, &jobs.root.join("state/release-cache")).await?;
                    Ok::<_, anyhow::Error>(manifest)
                }
                .await;
                let mut state = jobs.release.lock().await;
                *state = match result {
                    Ok(m) => {
                        json!({"status":"ok","version":m["version"],"checked_at":now(),"checking":false})
                    }
                    Err(_) => {
                        json!({"status":"unavailable","message":"版本检查暂不可用，请稍后重试","checked_at":now(),"checking":false})
                    }
                };
            });
        }
        state.clone()
    }
    async fn update(&self, job: &mut Value, stage: &str, message: &str) {
        job["stage"] = json!(stage);
        job["message"] = json!(message);
        if self.save(job.clone()).await.is_err() {
            eprintln!("deployment progress write failed")
        }
    }
    pub async fn start(self: &Arc<Self>, mut req: Value, store: Arc<Store>) -> Result<Value> {
        validate(&req)?;
        if s(&req, "node_id").is_empty() {
            req["node_id"] = json!(id())
        }
        let mut items = self.items.lock().await;
        if items.values().any(|v| !b(v, "done")) {
            return Err(conflict());
        }
        if items.len() >= 100 {
            items.retain(|_, v| !b(v, "done"))
        }
        let job = json!({"id":id(),"node_id":req["node_id"],"name":req["name"],"stage":"queued","message":"等待连接服务器","done":false,"ok":false,"at":now()});
        items.insert(s(&job, "id").into(), job.clone());
        atomic_write(
            &self.root.join("state/jobs.json"),
            &serde_json::to_vec(&*items)?,
            0o600,
        )?;
        drop(items);
        let jobs = self.clone();
        let mut task_job = job.clone();
        tokio::spawn(async move {
            let r = tokio::time::timeout(
                Duration::from_secs(600),
                jobs.deploy(&req, &store, &mut task_job),
            )
            .await;
            req["password"] = Value::Null;
            req["sealed_password"] = Value::Null;
            task_job["finished_at"] = json!(now());
            match r {
                Ok(Ok(result)) => {
                    task_job["result"] = result;
                    task_job["done"] = json!(true);
                    task_job["ok"] = json!(true);
                    jobs.update(&mut task_job, "complete", "部署完成，WSS 与实时上报已验证")
                        .await;
                }
                _ => {
                    task_job["done"] = json!(true);
                    task_job["ok"] = json!(false);
                    let message = match r {
                        Ok(Err(e)) => e.to_string(),
                        _ => "部署超时，请检查服务器状态后重试".into(),
                    };
                    jobs.update(&mut task_job, "failed", &message).await;
                }
            }
        });
        Ok(job)
    }
    async fn deploy(&self, req: &Value, store: &Store, job: &mut Value) -> Result<Value> {
        let host: IpAddr = s(req, "ssh_host").parse()?;
        let pubip: IpAddr = s(req, "public_ip").parse()?;
        let config: Value =
            serde_json::from_slice(&std::fs::read(self.root.join("state/controller.json"))?)?;
        let urls: Vec<String> = serde_json::from_value(config["controller_urls"].clone())?;
        crate::agent::client::validate_urls(&urls)?;
        if config["panel_ips"].as_array().is_some_and(|v| {
            v.iter().any(|ip| {
                ip.as_str() == Some(s(req, "ssh_host")) || ip.as_str() == Some(s(req, "public_ip"))
            })
        }) {
            bail!("主控服务器不能用作转发节点")
        }
        if let Ok(out) = tokio::process::Command::new("ip")
            .args(["-j", "address", "show"])
            .output()
            .await
            && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
        {
            for iface in v.as_array().into_iter().flatten() {
                for addr in iface["addr_info"].as_array().into_iter().flatten() {
                    if s(addr, "local")
                        .parse::<IpAddr>()
                        .is_ok_and(|v| v == host || v == pubip)
                    {
                        bail!("主控服务器不能用作转发节点")
                    }
                }
            }
        }
        self.update(job, "connect", "验证 SSH 连接与主机指纹").await;
        let (client, fingerprint) = connect(&self.root, req).await?;
        save_connection(store, req, &fingerprint).await?;
        self.update(job, "inspect", "检查系统、架构、权限与可用资源")
            .await;
        let raw=remote(&client,req,"set -eu; . /etc/os-release; printf '%s\\n' \"$ID\"; uname -m; cat /etc/machine-id; awk '/MemTotal/{print $2}' /proc/meminfo; df -Pk / | awk 'NR==2{print $4}'",b"").await.context("系统检查失败")?;
        let lines: Vec<_> = raw.trim().lines().collect();
        if lines.len() != 5 {
            bail!("系统检查返回不完整")
        }
        if !["debian", "ubuntu"].contains(&lines[0]) {
            bail!("目前支持 Debian 与 Ubuntu")
        }
        let arch = match lines[1] {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => bail!("不支持此 CPU 架构"),
        };
        if std::fs::read_to_string("/etc/machine-id")
            .unwrap_or_default()
            .trim()
            == lines[2]
        {
            bail!("不能在主控服务器上部署转发节点")
        }
        if lines[3].parse::<u64>().unwrap_or(0) < 128 * 1024
            || lines[4].parse::<u64>().unwrap_or(0) < 100 * 1024
        {
            bail!("至少需要 128 MiB 内存和 100 MiB 可用磁盘")
        }
        self.update(job, "tools", "检查运行工具").await;
        remote(&client,req,"set -eu; command -v systemctl >/dev/null; if ! command -v ip >/dev/null || ! command -v useradd >/dev/null || [ ! -f /etc/ssl/certs/ca-certificates.crt ]; then export DEBIAN_FRONTEND=noninteractive; apt-get update -qq; apt-get install -y -qq iproute2 coreutils passwd ca-certificates; fi",b"").await.context("运行工具安装失败")?;
        let route = remote(&client, req, "ip -j -4 route get 1.1.1.1", b"")
            .await
            .context("无法识别转发网卡")?;
        let route: Value = serde_json::from_str(&route)?;
        let iface = s(&route[0], "dev");
        let route_ip = s(&route[0], "prefsrc");
        if !route_ip
            .parse::<IpAddr>()
            .is_ok_and(|i| !i.is_unspecified())
            || iface.is_empty()
        {
            bail!("无法识别转发网卡地址")
        }
        // The routing interface is for telemetry, never the ingress listening scope.
        let bind = "0.0.0.0";
        let prior=remote(&client,req,"if [ -f /var/lib/vistart-agent/config.json ]; then cat /var/lib/vistart-agent/config.json; else printf '{}'; fi",b"").await?;
        let prior: Value =
            serde_json::from_str(&prior).context("已有 Agent 配置无效，请先检查服务器")?;
        let replacing =
            !s(&prior, "node_id").is_empty() && s(&prior, "node_id") != s(req, "node_id");
        if replacing {
            // An offline deletion cannot clear the remote config. Only adopt a
            // deleted identity from this same controller after SSH authentication.
            let same_controller = prior["controller_urls"].as_array().is_some_and(|old| {
                old.iter()
                    .any(|v| v.as_str().is_some_and(|u| urls.iter().any(|n| n == u)))
            });
            let mut db = store.pool.acquire().await?;
            let deleted = crate::store::count(
                &mut db,
                "SELECT COUNT(*) n FROM vp_nodes WHERE id=? AND deleted=1",
                &[prior["node_id"].clone()],
            )
            .await?
                == 1;
            if !same_controller || !deleted {
                bail!("服务器已绑定其他节点；请先移除旧节点或使用原节点重新部署")
            }
        }
        let tok = if !replacing && s(&prior, "token").len() >= 40 {
            s(&prior, "token").to_owned()
        } else {
            token()
        };
        self.update(job, "download", "下载并验证 Rust Agent 签名")
            .await;
        let (binary, expected_version) = fetch_release(
            release_base(&config),
            arch,
            &self.root.join("state/release-cache"),
        )
        .await
        .context("程序下载或签名校验失败")?;
        if !s(req, "expected_version").is_empty() && s(req, "expected_version") != expected_version
        {
            bail!("发布版本已变化，请重新检查后更新")
        }
        let digest = hex::encode(Sha256::digest(&binary));
        self.update(job, "upload", "上传程序并保留流量日志").await;
        remote(&client,req,"set -eu; getent passwd vistart-agent >/dev/null || useradd --system --home /var/lib/vistart-agent --shell /usr/sbin/nologin vistart-agent; install -d -m 0755 /opt/vistart-agent /opt/vistart-agent/releases; install -d -o vistart-agent -g vistart-agent -m 0700 /var/lib/vistart-agent; umask 077; cat > /opt/vistart-agent/agent.upload", &binary).await.context("程序上传失败")?;
        if replacing {
            self.update(job, "replace", "更正已移除节点的主控配置")
                .await;
            remote(&client,req,"set -eu; systemctl stop vistart-agent.service; rm -f /var/lib/vistart-agent/config.json /var/lib/vistart-agent/config.previous /var/lib/vistart-agent/config.removing /var/lib/vistart-agent/config.removed /var/lib/vistart-agent/traffic.json",b"").await.context("旧节点配置更正失败")?;
        }
        let config = json!({"controller_urls":urls,"node_id":req["node_id"],"token":tok,"interface":iface,"state_path":"/var/lib/vistart-agent/traffic.json","allowed_target_cidrs":prior.get("allowed_target_cidrs").cloned().unwrap_or(json!([])),"max_tcp":prior.get("max_tcp").cloned().unwrap_or(json!(0)),"max_udp":prior.get("max_udp").cloned().unwrap_or(json!(0))});
        let cfg = serde_json::to_vec(&config)?;
        remote(&client,req,"set -eu; umask 077; cat > /var/lib/vistart-agent/config.next; if [ -f /var/lib/vistart-agent/config.json ]; then cp -p /var/lib/vistart-agent/config.json /var/lib/vistart-agent/config.previous; fi; chown vistart-agent:vistart-agent /var/lib/vistart-agent/config.next; chmod 0600 /var/lib/vistart-agent/config.next; mv /var/lib/vistart-agent/config.next /var/lib/vistart-agent/config.json; rm -f /var/lib/vistart-agent/config.removed", &cfg).await.context("Agent 配置写入失败")?;
        remote(
            &client,
            req,
            "cat > /etc/systemd/system/vistart-agent.service",
            agent_service(lines[3].parse::<u64>().unwrap_or_default()).as_bytes(),
        )
        .await?;
        store
            .put_node(s(req, "node_id"), s(req, "name"), s(&config, "token"))
            .await?;
        self.update(job, "start", "启动 Rust 服务并等待 WSS 上报")
            .await;
        let install = format!(
            "set -eu; chmod 0755 /opt/vistart-agent/agent.upload; mv /opt/vistart-agent/agent.upload /opt/vistart-agent/releases/{digest}; if [ -L /opt/vistart-agent/current ]; then cp -P /opt/vistart-agent/current /opt/vistart-agent/previous; fi; ln -sfn /opt/vistart-agent/releases/{digest} /opt/vistart-agent/current.next; mv -Tf /opt/vistart-agent/current.next /opt/vistart-agent/current; systemctl daemon-reload; systemctl enable vistart-agent.service >/dev/null; systemctl restart vistart-agent.service; systemctl is-active --quiet vistart-agent.service"
        );
        remote(&client, req, &install, b"")
            .await
            .context("Agent 服务启动失败")?;
        let result = json!({"node_id":req["node_id"],"os":lines[0],"arch":arch,"bind_ip":bind,"route_ip":route_ip,"listen_mode":"all_ipv4","interface":iface,"fingerprint":fingerprint,"sha256":digest});
        for _ in 0..30 {
            let mut db = store.pool.acquire().await?;
            let rows = crate::store::rows(
                &mut db,
                "SELECT last_seen,agent_version FROM vp_nodes WHERE id=?",
                &[req["node_id"].clone()],
            )
            .await?;
            drop(db);
            if rows.first().is_some_and(|v| {
                n(v, "last_seen") >= now() - 5 && s(v, "agent_version") == expected_version
            }) {
                let _guard = store.writer.lock().await;
                let mut tx = store.pool.begin().await?;
                let current = crate::store::rows(
                    &mut tx,
                    "SELECT value FROM vp_metadata WHERE id=? FOR UPDATE",
                    &[req["node_id"].clone()],
                )
                .await?;
                let mut meta: Value = current
                    .first()
                    .and_then(|v| serde_json::from_str(s(v, "value")).ok())
                    .unwrap_or(json!({}));
                let old_bind = s(&meta, "bind_ip").to_owned();
                if !old_bind.is_empty() && old_bind != bind {
                    let allocated=crate::store::rows(&mut tx,"SELECT bind_ip,port FROM vp_allocations WHERE node_id=? AND released=0 FOR UPDATE",&[req["node_id"].clone()]).await?;
                    for (at, a) in allocated.iter().enumerate() {
                        let effective = if s(a, "bind_ip") == old_bind {
                            bind
                        } else {
                            s(a, "bind_ip")
                        };
                        for b in &allocated[at + 1..] {
                            let other = if s(b, "bind_ip") == old_bind {
                                bind
                            } else {
                                s(b, "bind_ip")
                            };
                            if n(a, "port") == n(b, "port")
                                && crate::listen_overlap(effective, other)
                            {
                                bail!(
                                    "多个本机地址正在复用同一端口，请先调整重复端口后再启用多网卡监听"
                                );
                            }
                        }
                    }
                    crate::store::exec(
                        &mut tx,
                        "UPDATE vp_pools SET bind_ip=? WHERE node_id=? AND bind_ip=?",
                        &[json!(bind), req["node_id"].clone(), json!(old_bind)],
                    )
                    .await?;
                    crate::store::exec(&mut tx,"UPDATE vp_allocations SET bind_ip=? WHERE node_id=? AND released=0 AND bind_ip=?",&[json!(bind),req["node_id"].clone(),json!(old_bind)]).await?;
                    crate::store::exec(
                        &mut tx,
                        "UPDATE vp_meta SET value=value+1 WHERE meta_key='revision'",
                        &[],
                    )
                    .await?;
                }
                meta.as_object_mut()
                    .ok_or_else(invalid)?
                    .extend(result.as_object().unwrap().clone());
                crate::store::exec(&mut tx, "INSERT INTO vp_metadata(id,value) VALUES(?,?) ON DUPLICATE KEY UPDATE value=VALUES(value)", &[req["node_id"].clone(),meta]).await?;
                tx.commit().await?;
                return Ok(result);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        bail!("程序已安装，但未收到新版 WSS 上报；请检查域名、证书与网络")
    }
}
pub fn validate(v: &Value) -> Result<()> {
    let safe = |s: &str| {
        s.parse::<IpAddr>().is_ok_and(|ip| {
            let ip = crate::agent::resolver::unmap(ip);
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && match ip {
                    IpAddr::V4(v) => !v.is_link_local(),
                    IpAddr::V6(v) => !v.is_unicast_link_local(),
                }
        })
    };
    let u = s(v, "username");
    if (!s(v, "node_id").is_empty() && !valid_id(s(v, "node_id")))
        || !safe(s(v, "ssh_host"))
        || !safe(s(v, "public_ip"))
        || !(1..=65535).contains(&n(v, "ssh_port"))
        || u.is_empty()
        || u.len() > 32
        || !u
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        || u.starts_with('-')
        || s(v, "password").is_empty()
        || s(v, "password").len() > 512
        || s(v, "password").contains(['\r', '\n', '\0'])
        || s(v, "name").is_empty()
        || s(v, "name").len() > 160
    {
        return Err(invalid());
    }
    Ok(())
}
struct HostKey {
    prior: Option<String>,
    inspected: Arc<StdMutex<String>>,
}
impl client::Handler for HostKey {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        if key.certificate().is_some() {
            return Ok(false);
        }
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        *self.inspected.lock().unwrap() = fingerprint.clone();
        Ok(self.prior.as_ref().is_none_or(|p| p == &fingerprint))
    }
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
async fn remote(
    client: &client::Handle<HostKey>,
    req: &Value,
    command: &str,
    input: &[u8],
) -> Result<String> {
    let mut command = command.to_owned();
    let mut payload = Vec::new();
    if s(req, "username") != "root" {
        let marker = format!("vistart-{}", token());
        let wrapper = format!(
            "IFS= read -r vistart_line || exit 97; if [ \"$vistart_line\" != {} ]; then IFS= read -r vistart_line || exit 97; fi; [ \"$vistart_line\" = {} ] || exit 97; unset vistart_line; {command}",
            quote(&marker),
            quote(&marker)
        );
        command = format!("sudo -k -S -p '' sh -c {}", quote(&wrapper));
        payload.extend_from_slice(format!("{}\n{marker}\n", s(req, "password")).as_bytes());
    }
    payload.extend_from_slice(input);
    let result = async {
        let channel = client.channel_open_session().await?;
        channel.exec(true, command.as_str()).await?;
        let (mut reader, writer) = channel.split();
        let send = async {
            if !payload.is_empty() {
                writer.data(payload.as_slice()).await?;
            }
            writer.eof().await?;
            Ok::<_, anyhow::Error>(())
        };
        let recv = async {
            let mut output = Vec::new();
            let mut status = None;
            let mut accepted = false;
            while let Some(msg) = reader.wait().await {
                match msg {
                    ChannelMsg::Success => accepted = true,
                    ChannelMsg::Failure => bail!("远程命令被拒绝"),
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        if output.len() + data.len() > 32768 {
                            bail!("远程输出超出限制")
                        }
                        output.extend_from_slice(&data)
                    }
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => (),
                }
            }
            if !accepted || status != Some(0) {
                bail!("远程命令执行失败")
            }
            Ok::<_, anyhow::Error>(
                String::from_utf8(output)?.replace(s(req, "password"), "[redacted]"),
            )
        };
        let res = tokio::try_join!(send, recv).map(|(_, v)| v);
        let _ = writer.close().await;
        res
    };
    tokio::time::timeout(Duration::from_secs(180), result)
        .await
        .context("远程操作超时")?
}
const RELEASE_KEY: &str = "KIIxr0QlDRHjO6RTCGNmUJ3tYlbbun2wWTYmaMctbOI=";
async fn get(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    let resp = client.get(url).send().await?.error_for_status()?;
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        bail!("release response too large")
    }
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if out.len() + chunk.len() > limit {
            bail!("release response too large")
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
async fn read_manifest(base: &str, cache: &Path) -> Result<(reqwest::Client, Value)> {
    let u = url::Url::parse(base)?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        bail!("HTTPS release URL required")
    }
    let host = u.host_str().unwrap().to_owned();
    let port = u.port_or_known_default();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::custom(move |a| {
            let github_asset = host == "github.com"
                && [
                    "release-assets.githubusercontent.com",
                    "objects.githubusercontent.com",
                ]
                .contains(&a.url().host_str().unwrap_or(""))
                && a.url()
                    .path()
                    .starts_with("/github-production-release-asset");
            if a.url().scheme() == "https"
                && (a.url().host_str() == Some(&host) || github_asset)
                && a.url().port_or_known_default() == port
                && a.url().username().is_empty()
                && a.url().password().is_none()
                && a.previous().len() < 4
            {
                a.follow()
            } else {
                a.error("redirect rejected")
            }
        }))
        .build()?;
    let base = base.trim_end_matches('/');
    let raw = get(&client, &format!("{base}/manifest.json"), 65536).await?;
    let signature = get(&client, &format!("{base}/manifest.sig"), 1024).await?;
    let signature = B64.decode(std::str::from_utf8(&signature)?.trim())?;
    let key: [u8; 32] = B64
        .decode(RELEASE_KEY)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid public key"))?;
    VerifyingKey::from_bytes(&key)?.verify(&raw, &Signature::from_slice(&signature)?)?;
    let manifest: Value = serde_json::from_slice(&raw)?;
    let version = s(&manifest, "version");
    let sequence = n(&manifest, "sequence");
    if sequence < 1
        || !regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?$")?.is_match(version)
    {
        bail!("invalid manifest")
    }
    std::fs::create_dir_all(cache)?;
    let seq = cache.join("release-sequence");
    let prior = std::fs::read_to_string(&seq)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if sequence < prior {
        bail!("release rollback rejected")
    }
    atomic_write(&seq, sequence.to_string().as_bytes(), 0o600)?;
    Ok((client, manifest))
}
async fn fetch_release(base: &str, arch: &str, cache: &Path) -> Result<(Vec<u8>, String)> {
    let (client, manifest) = read_manifest(base, cache).await?;
    let base = base.trim_end_matches('/');
    let version = s(&manifest, "version");
    let artifact = manifest["artifacts"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|v| s(v, "arch") == arch && s(v, "os") == "linux")
        })
        .context("no compatible artifact")?;
    let name = s(artifact, "name");
    let digest = s(artifact, "sha256");
    let size = n(artifact, "size");
    if name.is_empty()
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        || !(1..=32 << 20).contains(&size)
        || digest.len() != 64
        || !digest.bytes().all(|c| c.is_ascii_hexdigit())
    {
        bail!("invalid artifact")
    }
    let path = cache.join(digest);
    let valid = |v: &[u8]| v.len() == size as usize && hex::encode(Sha256::digest(v)) == digest;
    let data = match std::fs::read(&path) {
        Ok(v) if valid(&v) => v,
        _ => {
            let url = if base == "https://github.com/coexacx/forward-panel/releases/latest/download"
            {
                format!(
                    "https://github.com/coexacx/forward-panel/releases/download/v{version}/{name}"
                )
            } else {
                format!("{base}/{version}/{name}")
            };
            let v = get(&client, &url, size as usize).await?;
            if !valid(&v) {
                bail!("artifact checksum mismatch")
            }
            atomic_write(&path, &v, 0o600)?;
            v
        }
    };
    Ok((data, version.to_owned()))
}

pub fn release_base(config: &Value) -> &str {
    if s(config, "release_url").is_empty() {
        "https://github.com/coexacx/forward-panel/releases/latest/download"
    } else {
        s(config, "release_url")
    }
}
pub fn newer_version(latest: &str, current: &str) -> bool {
    fn parse(raw: &str) -> Option<Vec<u64>> {
        let parts: Option<Vec<u64>> = raw.split('.').map(|s| s.parse().ok()).collect();
        parts.filter(|v| v.len() == 3)
    }
    match (parse(latest), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}
pub async fn verified_version(root: &Path, config: &Value) -> Result<String> {
    let (_, manifest) =
        read_manifest(release_base(config), &root.join("state/release-cache")).await?;
    Ok(s(&manifest, "version").into())
}
fn pin_path(root: &Path, address: SocketAddr) -> PathBuf {
    root.join("state/ssh-pins").join(format!(
        "{}.key",
        hex::encode(Sha256::digest(address.to_string()))
    ))
}
pub async fn inspect_host(root: &Path, req: &Value) -> Result<Value> {
    let mut checked = req.clone();
    checked["public_ip"] = req["ssh_host"].clone();
    checked["password"] = json!("preflight");
    checked["name"] = json!("preflight");
    validate(&checked)?;
    let address = SocketAddr::new(s(req, "ssh_host").parse()?, n(req, "ssh_port") as u16);
    let previous = std::fs::read_to_string(pin_path(root, address))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let inspected = Arc::new(StdMutex::new(String::new()));
    let handler = HostKey {
        prior: None,
        inspected: inspected.clone(),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        client::connect(Arc::new(client::Config::default()), address, handler),
    )
    .await;
    let client = result
        .context("SSH 连接超时")?
        .context("无法取得 SSH 主机指纹")?;
    let fingerprint = inspected.lock().unwrap().clone();
    client
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await?;
    Ok(
        json!({"fingerprint":fingerprint,"previous":previous,"changed":!previous.is_empty() && previous!=fingerprint}),
    )
}
async fn connect(root: &Path, req: &Value) -> Result<(client::Handle<HostKey>, String)> {
    validate(req)?;
    let address = SocketAddr::new(s(req, "ssh_host").parse()?, n(req, "ssh_port") as u16);
    let pin = pin_path(root, address);
    std::fs::create_dir_all(pin.parent().unwrap())?;
    let prior = std::fs::read_to_string(&pin)
        .ok()
        .map(|s| s.trim().to_owned());
    let confirmed = s(req, "confirmed_fingerprint");
    let expected = if confirmed.is_empty() {
        prior.clone()
    } else {
        if !confirmed.starts_with("SHA256:") || confirmed.len() > 100 {
            bail!("主机指纹格式不正确")
        }
        Some(confirmed.to_owned())
    };
    let inspected = Arc::new(StdMutex::new(String::new()));
    let handler = HostKey {
        prior: expected,
        inspected: inspected.clone(),
    };
    let cfg = client::Config {
        inactivity_timeout: Some(Duration::from_secs(90)),
        keepalive_interval: Some(Duration::from_secs(20)),
        keepalive_max: 3,
        ..Default::default()
    };
    let mut client = tokio::time::timeout(
        Duration::from_secs(20),
        client::connect(Arc::new(cfg), address, handler),
    )
    .await
    .context("SSH 连接超时")?
    .context("SSH 连接或主机指纹校验失败；系统重装后请在连接信息中确认新指纹")?;
    let success = tokio::time::timeout(
        Duration::from_secs(15),
        client.authenticate_password(s(req, "username"), s(req, "password")),
    )
    .await
    .context("SSH 认证超时")?
    .context("SSH 认证失败")?;
    if !success.success() {
        bail!("SSH 用户名或密码不正确")
    }
    let fingerprint = inspected.lock().unwrap().clone();
    if prior.as_deref() != Some(&fingerprint) {
        atomic_write(&pin, format!("{fingerprint}\n").as_bytes(), 0o600)?;
    }
    Ok((client, fingerprint))
}
async fn save_connection(store: &Store, req: &Value, fingerprint: &str) -> Result<()> {
    if s(req, "sealed_password").is_empty() {
        return Ok(());
    }
    let _guard = store.writer.lock().await;
    let mut tx = store.pool.begin().await?;
    let old = crate::store::rows(
        &mut tx,
        "SELECT value FROM vp_metadata WHERE id=? FOR UPDATE",
        &[req["node_id"].clone()],
    )
    .await?;
    let mut meta: Value = old
        .first()
        .and_then(|v| serde_json::from_str(s(v, "value")).ok())
        .unwrap_or(json!({}));
    for k in ["ssh_host", "ssh_port", "username"] {
        meta[k] = req[k].clone();
    }
    for k in ["name", "region", "public_ip"] {
        if meta[k].is_null() {
            meta[k] = req[k].clone();
        }
    }
    meta["ssh_secret"] = req["sealed_password"].clone();
    meta["fingerprint"] = json!(fingerprint);
    crate::store::exec(
        &mut tx,
        "INSERT INTO vp_metadata(id,value) VALUES(?,?) ON DUPLICATE KEY UPDATE value=VALUES(value)",
        &[req["node_id"].clone(), meta],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
pub async fn check_connection(root: &Path, req: &Value, store: &Store) -> Result<()> {
    let (client, fingerprint) = connect(root, req).await?;
    save_connection(store, req, &fingerprint).await?;
    client
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await?;
    Ok(())
}

pub async fn occupied_ports(root: &Path, req: &Value) -> Result<Vec<i64>> {
    let (client, _) = connect(root, req).await?;
    let output = remote(&client, req, "ss -H -ltnu", b"").await?;
    client
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await?;
    let mut ports = std::collections::BTreeSet::new();
    for line in output.lines() {
        if let Some(port) = line
            .split_whitespace()
            .nth(4)
            .and_then(|s| s.rsplit(':').next())
            .and_then(|p| p.parse::<i64>().ok())
        {
            ports.insert(port);
        }
    }
    Ok(ports.into_iter().collect())
}

fn agent_service(memory_kib: u64) -> String {
    include_str!("agent.service").replace(
        "MemoryMax=128M",
        &format!(
            "MemoryMax={}M",
            crate::agent::memory::service_memory_mib(memory_kib)
        ),
    )
}
