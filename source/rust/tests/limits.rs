use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use vistart_forward::{
    agent::{engine::Engine, journal::Journal, resolver::Resolver},
    id, now,
    protocol::{Budget, Config, LeaseGrant, Limits, Rule},
};

async fn port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}
async fn target() -> u16 {
    let t = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = t.local_addr().unwrap().port();
    let u = UdpSocket::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = t.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 32768];
                while let Ok(n) = c.read(&mut b).await {
                    if n == 0 {
                        break;
                    }
                    if c.write_all(&b[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    tokio::spawn(async move {
        let mut b = vec![0u8; 65535];
        while let Ok((n, a)) = u.recv_from(&mut b).await {
            let _ = u.send_to(&b[..n], a).await;
        }
    });
    port
}
async fn roundtrip(c: &mut TcpStream, msg: &[u8]) {
    c.write_all(msg).await.unwrap();
    let mut out = vec![0u8; msg.len()];
    tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out, msg);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_limits_duplex_pacing_pending_capacity_and_unlimited_isolation() {
    let dir = std::env::temp_dir().join(format!("forward-limits-{}", id()));
    std::fs::create_dir(&dir).unwrap();
    let j = Journal::open(&dir.join("traffic.json")).unwrap();
    let e = Engine::new(
        j,
        Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![])),
        16,
        16,
    );
    let backend = target().await;
    let p1 = port().await;
    let p2 = port().await;
    let p3 = port().await;
    let limits = Limits {
        bandwidth_mbps: 4,
        tcp_limit: 2,
        udp_limit: 1,
    };
    let make = |rid: &str, lease: &str, port, limits: Limits| Rule {
        id: rid.into(),
        lease_id: lease.into(),
        user_id: "owner".into(),
        cycle_id: "cycle".into(),
        listen_ip: "127.0.0.1".into(),
        listen_port: port,
        target_host: "127.0.0.1".into(),
        target_port: backend,
        expires_at: now() + 300,
        limits,
        ..Default::default()
    };
    let cfg = Config {
        version: 1,
        revision: 1,
        valid_for_seconds: 30,
        rules: vec![
            make("one", "lease", p1, limits.clone()),
            make("two", "lease", p2, limits.clone()),
            make("other", "other", p3, Limits::default()),
        ],
        lease_limits: vec![LeaseGrant {
            lease_id: "lease".into(),
            id: "grant".into(),
            policy: limits,
            budget: Budget {
                bandwidth: 500_000,
                tcp: 2,
                udp: 1,
            },
        }],
        ..Default::default()
    };
    assert!(e.apply(&cfg).await.is_empty());
    let mut a = TcpStream::connect(("127.0.0.1", p1)).await.unwrap();
    roundtrip(&mut a, b"a").await;
    let mut b = TcpStream::connect(("127.0.0.1", p2)).await.unwrap();
    roundtrip(&mut b, b"b").await;
    let mut pending = TcpStream::connect(("127.0.0.1", p1)).await.unwrap();
    pending.write_all(b"pending").await.unwrap();
    let mut reply = [0u8; 7];
    assert!(
        tokio::time::timeout(Duration::from_millis(180), pending.read_exact(&mut reply))
            .await
            .is_err()
    );
    let mut other = TcpStream::connect(("127.0.0.1", p3)).await.unwrap();
    roundtrip(&mut other, b"unlimited").await;
    drop(a);
    tokio::time::timeout(Duration::from_secs(2), pending.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"pending");
    let u1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let u2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    u1.send_to(b"udp", ("127.0.0.1", p1)).await.unwrap();
    let mut data = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(2), u1.recv(&mut data))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&data[..n], b"udp");
    u2.send_to(b"blocked", ("127.0.0.1", p2)).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(180), u2.recv(&mut data))
            .await
            .is_err()
    );
    let usage = e
        .lease_usage()
        .into_iter()
        .find(|u| u.lease_id == "lease")
        .unwrap();
    assert_eq!(usage.tcp_active, 2);
    assert_eq!(usage.udp_active, 1);
    let engine = e.clone();
    let config = cfg.clone();
    let refresh = tokio::spawn(async move {
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(engine.apply(&config).await.is_empty());
        }
    });
    async fn transfer(mut c: TcpStream) {
        let payload = vec![0x5au8; 200_000];
        let (mut reader, mut writer) = c.split();
        let send = async {
            writer.write_all(&payload).await.unwrap();
            writer.shutdown().await.unwrap();
        };
        let receive = async {
            let mut got = Vec::new();
            reader.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, payload);
        };
        tokio::join!(send, receive);
    }
    let start = Instant::now();
    tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(transfer(b), transfer(pending));
    })
    .await
    .unwrap();
    let elapsed = start.elapsed().as_secs_f64();
    // Both directions and both ports share 4 Mbps. Reapplying an identical
    // configuration cannot replenish a fresh burst bucket.
    assert!(elapsed >= 1.35, "shared bandwidth escaped: {elapsed}");
    assert!(elapsed < 7.5, "pacer stalled: {elapsed}");
    refresh.abort();
    e.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}
