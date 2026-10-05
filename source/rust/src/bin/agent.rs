#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == "--version" || s == "-version") {
        println!("vistart-agent {}", vistart_forward::VERSION);
        return;
    }
    let path = args
        .windows(2)
        .find(|a| a[0] == "-config" || a[0] == "--config")
        .map(|a| a[1].as_str())
        .unwrap_or("/var/lib/vistart-agent/config.json");
    if let Err(e) = vistart_forward::agent::client::run(std::path::Path::new(path)).await {
        eprintln!("agent stopped: {e}");
        std::process::exit(1);
    }
}
