use super::{engine::Engine, journal::Journal, probe::Sampler, resolver::Resolver};
use crate::{VERSION, atomic_write, protocol::*, valid_id};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
#[derive(Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub controller_urls: Vec<String>,
    pub node_id: String,
    pub token: String,
    #[serde(default)]
    pub interface: String,
    pub state_path: PathBuf,
    #[serde(default)]
    pub allowed_target_cidrs: Vec<String>,
    #[serde(default)]
    pub max_tcp: usize,
    #[serde(default)]
    pub max_udp: usize,
}
pub fn validate_urls(urls: &[String]) -> Result<()> {
    if urls.is_empty() || urls.len() > 3 {
        bail!("one to three controller URLs required")
    }
    for raw in urls {
        let u = url::Url::parse(raw)?;
        if u.scheme() != "wss"
            || u.host_str().is_none()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || u.path() != "/control/agent"
        {
            bail!("WSS controller URL required")
        }
    }
    Ok(())
}
fn capacity() -> (usize, usize) {
    let mem = std::fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse::<usize>().ok())
        .unwrap_or(131072);
    // Keep default application buffers below 1/8 physical memory, capped for the
    // hardened 128 MiB service budget. Existing explicit limits remain effective.
    let tcp = (mem / 512).clamp(64, 512);
    let udp = (mem / 1024).clamp(64, 256);
    (tcp, udp)
}
pub async fn run(path: &Path) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut conf: AgentConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    validate_urls(&conf.controller_urls)?;
    if !valid_id(&conf.node_id)
        || conf.token.len() < 40
        || conf.token.len() > 200
        || conf.max_tcp > 65536
        || conf.max_udp > 16384
    {
        bail!("invalid agent configuration")
    }
    let j = Journal::open(&conf.state_path)?;
    let mut denied = Vec::<IpAddr>::new();
    if let Ok(out) = tokio::process::Command::new("ip")
        .args(["-j", "address", "show"])
        .output()
        .await
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        && let Some(a) = v.as_array()
    {
        for iface in a {
            if let Some(addrs) = iface["addr_info"].as_array() {
                for addr in addrs {
                    if let Some(ip) = addr["local"].as_str().and_then(|s| s.parse().ok()) {
                        denied.push(ip);
                    }
                }
            }
        }
    }
    let allowed = conf
        .allowed_target_cidrs
        .iter()
        .map(|s| s.parse())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let resolver = Arc::new(Resolver::new(allowed, denied));
    let (tcp, udp) = capacity();
    let e = Engine::new(
        j.clone(),
        resolver,
        if conf.max_tcp > 0 { conf.max_tcp } else { tcp },
        if conf.max_udp > 0 { conf.max_udp } else { udp },
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM");
        tokio::select! {_=tokio::signal::ctrl_c()=>(),_=term.recv()=>()};
        c.cancel();
    });
    let mut sampler = Sampler::default();
    sampler.interface = conf.interface.clone();
    let mut backoff = 1u64;
    let mut index = 0;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let endpoint = conf.controller_urls[index % conf.controller_urls.len()].clone();
        index += 1;
        let result = tokio::select! {_=cancel.cancelled()=>break,r=session(&endpoint,path,&mut conf,&e,&mut sampler)=>r};
        e.suspend().await;
        match result {
            Ok(true) => {
                e.close().await;
                j.flush()?;
                clear_identity(path, &conf)?;
                return Ok(());
            }
            Ok(false) => {
                backoff = 1;
                index = 0;
            }
            Err(error) => {
                // The final acknowledgement may be lost after the controller revokes the token.
                // A pending marker is only written after an authenticated removal command.
                let revoked=error.downcast_ref::<tokio_tungstenite::tungstenite::Error>().is_some_and(|e|matches!(e,tokio_tungstenite::tungstenite::Error::Http(r) if r.status().as_u16()==401));
                if revoked && path.with_extension("removing").is_file() {
                    e.close().await;
                    j.flush()?;
                    clear_identity(path, &conf)?;
                    return Ok(());
                }
                eprintln!("controller session interrupted; forwarding suspended");
            }
        }
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(backoff*1000+(rand::random::<u16>()as u64%500)))=>()}
        backoff = (backoff * 2).min(15);
    }
    e.close().await;
    j.flush()?;
    Ok(())
}
fn clear_identity(path: &Path, conf: &AgentConfig) -> Result<()> {
    atomic_write(
        &path.with_extension("removed"),
        b"Disconnected by panel administrator\n",
        0o600,
    )?;
    for file in [
        path.to_path_buf(),
        path.with_extension("previous"),
        path.with_extension("removing"),
        std::path::PathBuf::from(&conf.state_path),
    ] {
        match std::fs::remove_file(file) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
async fn session(
    endpoint: &str,
    path: &Path,
    conf: &mut AgentConfig,
    e: &Arc<Engine>,
    sampler: &mut Sampler,
) -> Result<bool> {
    let mut req = endpoint.into_client_request()?;
    req.headers_mut()
        .insert("Authorization", format!("Bearer {}", conf.token).parse()?);
    req.headers_mut().insert("X-Node-ID", conf.node_id.parse()?);
    let cfg = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
        .max_write_buffer_size(MAX_FRAME * 2);
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(12),
        connect_async_with_config(req, Some(cfg), true),
    )
    .await??;
    let mut errors = Vec::new();
    let mut revision = 0;
    let mut decommission =
        std::fs::read_to_string(path.with_extension("removing")).unwrap_or_default();
    if !decommission.is_empty() && !valid_id(&decommission) {
        bail!("invalid pending removal state")
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let journal = e.journal.clone();
        let mut r = tokio::task::spawn_blocking(move || journal.report()).await??;
        r.agent_version = VERSION.into();
        r.kernel_version = VERSION.into();
        r.errors = errors.clone();
        r.applied_revision = revision;
        r.probe = sampler.sample();
        r.probe.target_checks = Some(e.target_checks().await);
        r.decommission_ack = decommission.clone();
        let exchange = async {
            ws.send(Message::Text(serde_json::to_string(&r)?.into()))
                .await?;
            loop {
                match ws.next().await.context("controller disconnected")?? {
                    Message::Text(raw) => {
                        return Ok::<_, anyhow::Error>(serde_json::from_str::<Config>(&raw)?);
                    }
                    Message::Ping(v) => ws.send(Message::Pong(v)).await?,
                    Message::Pong(_) => (),
                    _ => bail!("unexpected controller message"),
                }
            }
        };
        let cfg = tokio::time::timeout(Duration::from_secs(8), exchange).await??;
        if cfg.version != 1 || cfg.ack_epoch != r.epoch || cfg.ack_sequence != r.sequence {
            bail!("invalid acknowledgement")
        }
        e.journal.ack(&r);
        if !decommission.is_empty() && cfg.decommission == decommission {
            return Ok(true);
        }
        if !cfg.decommission.is_empty() {
            if !valid_id(&cfg.decommission) {
                bail!("invalid removal token")
            }
            e.suspend().await;
            atomic_write(
                &path.with_extension("removing"),
                cfg.decommission.as_bytes(),
                0o600,
            )?;
            decommission = cfg.decommission.clone();
            continue;
        }
        errors = e.apply(&cfg).await;
        e.journal.prune(&cfg.rules)?;
        revision = cfg.revision;
        if let Some(urls) = cfg.controller_urls {
            validate_urls(&urls)?;
            if urls.first() != conf.controller_urls.first() {
                let mut next = urls;
                for old in &conf.controller_urls {
                    if next.len() < 3 && !next.contains(old) {
                        next.push(old.clone());
                    }
                }
                conf.controller_urls = next;
                atomic_write(path, &serde_json::to_vec(conf)?, 0o600)?;
                return Ok(false);
            }
        }
    }
}
