use anyhow::{Result, bail};
use fs2::FileExt;
use std::{
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
};
fn main() {
    if let Err(e) = entry() {
        eprintln!("controller stopped: {e}");
        std::process::exit(1)
    }
}
fn entry() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == "--version" || s == "-version") {
        println!("vistart-controller {}", vistart_forward::VERSION);
        return Ok(());
    }
    let path = Path::new(
        args.windows(2)
            .find(|v| v[0] == "-config" || v[0] == "--config")
            .map(|v| v[1].as_str())
            .unwrap_or("state/controller.json"),
    );
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if vistart_forward::s(&c, "driver") != "mysql" {
        bail!("MySQL database configuration required")
    }
    let dir = Path::new(vistart_forward::s(&c, "socket"))
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid socket"))?;
    std::fs::create_dir_all(dir)?;
    if args.iter().any(|s| s == "-daemon") {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(dir.parent().unwrap().join("controller.log"))?;
        let mut cmd = std::process::Command::new(std::env::current_exe()?);
        cmd.args(["-config", path.canonicalize()?.to_str().unwrap()])
            .stdin(std::process::Stdio::null())
            .stdout(f.try_clone()?)
            .stderr(f);
        // A new process session makes the installer companion independent of PHP-FPM.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        cmd.spawn()?;
        return Ok(());
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("controller.lock"))?;
    lock.try_lock_exclusive()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    if args
        .iter()
        .any(|s| s == "--install" || s == "--reset-admin-password")
    {
        use std::io::Read;
        let mut input = Vec::new();
        std::io::stdin().take(4097).read_to_end(&mut input)?;
        if input.len() > 4096 {
            bail!("initialization input too large")
        }
        let value: serde_json::Value = serde_json::from_slice(&input)?;
        let root = dir
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow::anyhow!("invalid state root"))?;
        runtime.block_on(async {
            let store = vistart_forward::store::Store::open(&c["mysql"]).await?;
            if args.iter().any(|s| s == "--install") {
                vistart_forward::web::install::initialize(root, &store, &value).await
            } else {
                vistart_forward::web::install::reset_password(&store, &value).await
            }
        })?;
        println!("Administrator configuration saved");
        return Ok(());
    }
    if args.iter().any(|s| s == "-init") {
        runtime.block_on(vistart_forward::store::Store::open(&c["mysql"]))?;
        println!("database initialized");
        return Ok(());
    }
    runtime.block_on(vistart_forward::control::run(c))
}
