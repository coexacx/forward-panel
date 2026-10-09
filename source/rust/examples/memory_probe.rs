use std::{path::PathBuf, sync::Arc, time::Duration};
use vistart_forward::{
    agent::{engine::Engine, journal::Journal, resolver::Resolver},
    now,
    protocol::{Config, Rule},
};
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4,
        "usage: memory_probe ABSOLUTE_JOURNAL LISTEN_PORT TARGET_PORT"
    );
    let j = Journal::open(&PathBuf::from(&args[1]))?;
    let e = Engine::new(
        j.clone(),
        Arc::new(Resolver::new(vec!["127.0.0.0/8".parse()?], vec![])),
        512,
        256,
    );
    let cfg = Config {
        version: 1,
        valid_for_seconds: 30,
        rules: vec![Rule {
            id: "memory-qa".into(),
            user_id: "qa".into(),
            lease_id: "qa".into(),
            cycle_id: "qa".into(),
            listen_ip: "127.0.0.1".into(),
            listen_port: args[2].parse()?,
            target_host: "127.0.0.2".into(),
            target_port: args[3].parse()?,
            expires_at: now() + 3600,
            ..Default::default()
        }],
        ..Default::default()
    };
    anyhow::ensure!(
        e.apply(&cfg).await.is_empty(),
        "could not bind isolated QA port"
    );
    println!("ready {}", vistart_forward::VERSION);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _=term.recv()=>break,
            _=tokio::signal::ctrl_c()=>break,
            _=tick.tick()=>{
                anyhow::ensure!(e.apply(&cfg).await.is_empty(),"QA config refresh failed");
            }
        }
    }
    e.close().await;
    j.flush()?;
    Ok(())
}
