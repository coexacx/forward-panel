use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use vistart_forward::{
    agent::{engine::Engine, journal::Journal, resolver::Resolver},
    id, now,
    protocol::{Config, Rule, Target},
};
async fn echo() -> u16 {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let udp = UdpSocket::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move {
        loop {
            let (mut c, _) = tcp.accept().await.unwrap();
            tokio::spawn(async move {
                let mut data = vec![0u8; 65536];
                while let Ok(n) = c.read(&mut data).await {
                    if n == 0 {
                        break;
                    }
                    if c.write_all(&data[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    tokio::spawn(async move {
        let mut b = vec![0u8; 65536];
        loop {
            let (n, a) = udp.recv_from(&mut b).await.unwrap();
            let _ = udp.send_to(&b[..n], a).await;
        }
    });
    port
}
#[tokio::test]
async fn real_tcp_udp_half_close_live_cycle_and_shutdown() {
    let dir = std::env::temp_dir().join(format!("forward-engine-{}", id()));
    std::fs::create_dir(&dir).unwrap();
    let j = Journal::open(&dir.join("traffic.json")).unwrap();
    let resolver = Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![]));
    let e = Engine::new(j.clone(), resolver, 8, 8);
    let target = echo().await;
    let target2 = echo().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let rule = Rule {
        limits: Default::default(),
        id: "rule".into(),
        user_id: "user".into(),
        lease_id: "lease".into(),
        cycle_id: "cycle1".into(),
        listen_ip: "127.0.0.1".into(),
        listen_port: port,
        target_host: "127.0.0.1".into(),
        target_port: target,
        expires_at: now() + 300,
        load_balance: true,
        targets: vec![
            Target {
                host: "127.0.0.1".into(),
                port: target,
            },
            Target {
                host: "127.0.0.1".into(),
                port: target2,
            },
        ],
    };
    let mut config = Config {
        version: 1,
        revision: 1,
        rules: vec![rule],
        valid_for_seconds: 30,
        ..Default::default()
    };
    assert!(e.apply(&config).await.is_empty());
    assert_eq!(e.active_rule_ids().await, vec!["rule"]);
    let mut c = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let sent = vec![0x5a; 2 * 1024 * 1024];
    let (mut r, mut w) = c.split();
    let upload = async {
        w.write_all(&sent).await.unwrap();
        w.shutdown().await.unwrap()
    };
    let download = async {
        let mut got = Vec::new();
        r.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, sent);
    };
    tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(upload, download);
    })
    .await
    .unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp.connect(("127.0.0.1", port)).await.unwrap();
    for size in [0, 1, 1024, 32000] {
        let data = vec![7u8; size];
        udp.send(&data).await.unwrap();
        let mut buf = vec![0u8; 65536];
        let n = tokio::time::timeout(Duration::from_secs(2), udp.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], data);
    }
    // A scheduler burst must not be dropped by an undersized per-client queue.
    for sequence in 0u32..64 {
        let mut packet = vec![0x2b; 128];
        packet[..4].copy_from_slice(&sequence.to_be_bytes());
        udp.send(&packet).await.unwrap();
    }
    let mut sequences = std::collections::BTreeSet::new();
    for _ in 0..64 {
        let mut packet = [0u8; 128];
        let size = tokio::time::timeout(Duration::from_secs(2), udp.recv(&mut packet))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(size, 128);
        assert!(packet[4..].iter().all(|v| *v == 0x2b));
        sequences.insert(u32::from_be_bytes(packet[..4].try_into().unwrap()));
    }
    assert_eq!(sequences.len(), 64);
    let mut persistent = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    persistent.write_all(b"one").await.unwrap();
    let mut buf = [0u8; 3];
    persistent.read_exact(&mut buf).await.unwrap();
    let total = (sent.len() + 1 + 1024 + 32000 + 64 * 128 + 3) as u64;
    assert_eq!(j.meter("rule", "cycle1").counter().up, total);
    assert_eq!(j.meter("rule", "cycle1").counter().down, total);
    config.rules[0].cycle_id = "cycle2".into();
    config.revision = 2;
    assert!(e.apply(&config).await.is_empty());
    persistent.write_all(b"two").await.unwrap();
    persistent.read_exact(&mut buf).await.unwrap();
    assert_eq!(j.meter("rule", "cycle2").counter().up, 3);
    assert_eq!(j.meter("rule", "cycle1").counter().up, total);
    e.suspend().await;
    assert!(e.active_rule_ids().await.is_empty());
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    let closed = tokio::time::timeout(Duration::from_secs(2), persistent.read(&mut buf))
        .await
        .unwrap();
    assert!(closed.is_err() || closed.unwrap() == 0);
    e.close().await;
    drop(e);
    drop(j);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn tcp_udp_bind_is_atomic_and_capacity_is_bounded() {
    let dir = std::env::temp_dir().join(format!("forward-capacity-{}", id()));
    std::fs::create_dir(&dir).unwrap();
    let j = Journal::open(&dir.join("traffic.json")).unwrap();
    let e = Engine::new(
        j.clone(),
        Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![])),
        1,
        1,
    );
    let target = echo().await;
    let blocker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = blocker.local_addr().unwrap().port();
    let cfg = Config {
        version: 1,
        revision: 1,
        valid_for_seconds: 30,
        rules: vec![Rule {
            id: "r".into(),
            cycle_id: "c".into(),
            listen_ip: "127.0.0.1".into(),
            listen_port: port,
            target_host: "127.0.0.1".into(),
            target_port: target,
            expires_at: now() + 60,
            lease_id: "lease".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let errors = e.apply(&cfg).await;
    assert_eq!(errors[0].code, "bind_failed");
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    drop(blocker);
    assert!(e.apply(&cfg).await.is_empty());
    let mut a = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    a.write_all(b"a").await.unwrap();
    let mut buf = [0u8; 1];
    a.read_exact(&mut buf).await.unwrap();
    let mut excess = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), excess.read(&mut buf))
        .await
        .unwrap();
    assert!(result.is_err() || result.unwrap() == 0);
    e.close().await;
    drop(e);
    drop(j);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn incremental_changes_preserve_unrelated_live_connections() {
    let dir = std::env::temp_dir().join(format!("forward-delta-{}", id()));
    std::fs::create_dir(&dir).unwrap();
    let j = Journal::open(&dir.join("traffic.json")).unwrap();
    let e = Engine::new(
        j.clone(),
        Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![])),
        8,
        8,
    );
    let target = echo().await;
    let target2 = echo().await;
    let spare = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = spare.local_addr().unwrap().port();
    drop(spare);
    let a = Rule {
        id: "unchanged".into(),
        cycle_id: "cycle".into(),
        listen_ip: "127.0.0.1".into(),
        listen_port: port,
        target_host: "127.0.0.1".into(),
        target_port: target,
        expires_at: now() + 300,
        lease_id: "lease".into(),
        ..Default::default()
    };
    let mut baseline = Config {
        version: 1,
        revision: 1,
        valid_for_seconds: 30,
        rules: vec![a.clone()],
        ..Default::default()
    };
    let mut desired = baseline.expand(None).unwrap();
    assert!(e.apply(&desired).await.is_empty());
    let mut live = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    async fn check(c: &mut TcpStream) {
        c.write_all(b"still here").await.unwrap();
        let mut b = [0; 10];
        tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut b))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&b, b"still here");
    }
    check(&mut live).await;
    let blocker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let other_port = blocker.local_addr().unwrap().port();
    let mut added = a.clone();
    added.id = "added".into();
    added.listen_port = other_port;
    baseline.revision = 2;
    baseline.rules.push(added.clone());
    let wire = baseline.delta_from(&desired);
    assert!(wire.delta);
    assert_eq!(wire.rules, vec![added.clone()]);
    assert!(wire.removed_rules.is_empty());
    assert!(wire.expand(None).is_err(), "reconnect requires full sync");
    desired = wire.expand(Some(&desired)).unwrap();
    assert_eq!(e.apply(&desired).await[0].rule_id, "added");
    assert_eq!(e.active_rule_ids().await, vec!["unchanged"]);
    check(&mut live).await;
    drop(blocker);
    let heartbeat = baseline.delta_from(&desired);
    assert!(heartbeat.rules.is_empty() && heartbeat.removed_rules.is_empty());
    desired = heartbeat.expand(Some(&desired)).unwrap();
    assert!(
        e.apply(&desired).await.is_empty(),
        "retry failed bind without replaying other rules"
    );
    check(&mut live).await;
    let mut second = TcpStream::connect(("127.0.0.1", other_port)).await.unwrap();
    check(&mut second).await;
    baseline.revision = 3;
    baseline.rules[1].target_port = target2;
    let wire = baseline.delta_from(&desired);
    assert_eq!(wire.rules.len(), 1);
    assert_eq!(wire.rules[0].id, "added");
    desired = wire.expand(Some(&desired)).unwrap();
    assert!(e.apply(&desired).await.is_empty());
    check(&mut live).await;
    baseline.revision = 4;
    baseline.rules.retain(|r| r.id == "unchanged");
    let wire = baseline.delta_from(&desired);
    assert!(wire.rules.is_empty());
    assert_eq!(wire.removed_rules, vec!["added"]);
    desired = wire.expand(Some(&desired)).unwrap();
    assert!(e.apply(&desired).await.is_empty());
    check(&mut live).await;
    assert_eq!(e.active_rule_ids().await, vec!["unchanged"]);
    assert!(TcpStream::connect(("127.0.0.1", other_port)).await.is_err());
    assert!(
        wire.expand(Some(&desired)).is_err(),
        "stale patch is rejected"
    );
    assert_eq!(desired.fingerprints()["unchanged"], a.fingerprint());
    let mut conflict = baseline.delta_from(&desired);
    conflict.rules = vec![a.clone()];
    conflict.removed_rules = vec![a.id.clone()];
    assert!(conflict.expand(Some(&desired)).is_err());
    let mut too_many = baseline.clone();
    too_many.rules = (0..513)
        .map(|i| {
            let mut r = a.clone();
            r.id = format!("r{i}");
            r
        })
        .collect();
    assert!(too_many.expand(None).is_err());
    let legacy: vistart_forward::protocol::Report =
        serde_json::from_str(r#"{"version":1,"applied_revision":1}"#).unwrap();
    assert!(!legacy.supports_delta && legacy.applied_rules.is_none());
    let legacy_cfg: Config =
        serde_json::from_str(r#"{"version":1,"revision":5,"valid_for_seconds":10,"rules":[]}"#)
            .unwrap();
    assert!(!legacy_cfg.delta);
    desired = legacy_cfg.expand(Some(&desired)).unwrap();
    assert!(e.apply(&desired).await.is_empty());
    assert!(e.active_rule_ids().await.is_empty());
    e.close().await;
    drop(e);
    drop(j);
    std::fs::remove_dir_all(dir).unwrap();
}

async fn counts(e: &Engine, rule: &str, tcp: u32, udp: u32) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let c = e.rule_connections().await;
            if c.get(rule).is_some_and(|c| c.tcp == tcp && c.udp == udp) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("unexpected connection count for {rule}: expected TCP {tcp}, UDP {udp}")
    });
}
async fn tcp_peer(port: u16) -> TcpStream {
    let mut c = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    c.write_all(b"x").await.unwrap();
    let mut b = [0u8; 1];
    c.read_exact(&mut b).await.unwrap();
    assert_eq!(b, *b"x");
    c
}
async fn udp_peer(port: u16) -> UdpSocket {
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c.connect(("127.0.0.1", port)).await.unwrap();
    // Several packets from one source are one session, never several connections.
    for _ in 0..3 {
        c.send(b"udp").await.unwrap();
        let mut b = [0u8; 3];
        let n = tokio::time::timeout(Duration::from_secs(2), c.recv(&mut b))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&b[..n], b"udp");
    }
    c
}
#[tokio::test]
async fn live_rule_counts_survive_refresh_and_release_on_idle_close_and_retarget() {
    let dir = std::env::temp_dir().join(format!("forward-counts-{}", id()));
    std::fs::create_dir(&dir).unwrap();
    let j = Journal::open(&dir.join("traffic.json")).unwrap();
    let e = Engine::new(
        j.clone(),
        Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![])),
        16,
        16,
    );
    let target = echo().await;
    let mut config = Config {
        version: 1,
        valid_for_seconds: 30,
        ..Default::default()
    };
    for id in ["a", "b", "failed"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.rules.push(Rule {
            id: id.into(),
            lease_id: "same-lease".into(),
            cycle_id: "cycle".into(),
            listen_ip: "127.0.0.1".into(),
            listen_port: listener.local_addr().unwrap().port(),
            target_host: "127.0.0.1".into(),
            target_port: target,
            expires_at: now() + 300,
            ..Default::default()
        });
    }
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.rules[2].target_port = unavailable.local_addr().unwrap().port();
    drop(unavailable);
    assert!(e.apply(&config).await.is_empty());
    counts(&e, "a", 0, 0).await;
    let mut a1 = tcp_peer(config.rules[0].listen_port).await;
    let a2 = tcp_peer(config.rules[0].listen_port).await;
    let mut b = tcp_peer(config.rules[1].listen_port).await;
    let _u1 = udp_peer(config.rules[0].listen_port).await;
    let _u2 = udp_peer(config.rules[0].listen_port).await;
    let _u3 = udp_peer(config.rules[1].listen_port).await;
    counts(&e, "a", 2, 2).await;
    counts(&e, "b", 1, 1).await;
    let mut failed = TcpStream::connect(("127.0.0.1", config.rules[2].listen_port))
        .await
        .unwrap();
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(2), failed.read(&mut buf))
        .await
        .unwrap();
    assert!(closed.is_err() || closed.unwrap() == 0);
    counts(&e, "failed", 0, 0).await;
    config.rules[0].cycle_id = "next-cycle".into();
    config.rules[0].expires_at += 30;
    assert!(e.apply(&config).await.is_empty());
    counts(&e, "a", 2, 2).await;
    drop(a2);
    counts(&e, "a", 1, 2).await;
    // Keep the config lease alive while the real UDP idle timer expires.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(e.apply(&config).await.is_empty());
    }
    counts(&e, "a", 1, 0).await;
    counts(&e, "b", 1, 0).await;
    // Background TCP target probes do not inflate connection counts.
    a1.write_all(b"x").await.unwrap();
    a1.read_exact(&mut buf).await.unwrap();
    config.rules.retain(|r| r.id != "b");
    assert!(e.apply(&config).await.is_empty());
    assert!(!e.rule_connections().await.contains_key("b"));
    let closed = tokio::time::timeout(Duration::from_secs(2), b.read(&mut buf))
        .await
        .unwrap();
    assert!(closed.is_err() || closed.unwrap() == 0);
    counts(&e, "a", 1, 0).await;
    config.rules[0].target_port = echo().await;
    assert!(e.apply(&config).await.is_empty());
    counts(&e, "a", 0, 0).await;
    let closed = tokio::time::timeout(Duration::from_secs(2), a1.read(&mut buf))
        .await
        .unwrap();
    assert!(closed.is_err() || closed.unwrap() == 0);
    let _new = tcp_peer(config.rules[0].listen_port).await;
    counts(&e, "a", 1, 0).await;
    e.close().await;
    assert!(e.rule_connections().await.is_empty());
    drop(e);
    drop(j);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn slow_half_closed_reader_receives_every_byte_without_reset() {
    // Large transfer and a fully queued half-close whose reader waits >15s.
    for (size, wait_ms) in [(512 * 1024, 300), (32 * 1024, 16000)] {
        let dir = std::env::temp_dir().join(format!("forward-slow-close-{}", id()));
        std::fs::create_dir(&dir).unwrap();
        let j = Journal::open(&dir.join("traffic.json")).unwrap();
        let e = Engine::new(
            j.clone(),
            Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![])),
            8,
            8,
        );
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = backend.local_addr().unwrap().port();
        let sender = tokio::spawn(async move {
            let (mut c, _) = backend.accept().await.unwrap();
            let mut request = Vec::new();
            c.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, b"request");
            c.write_all(&vec![0x53; size]).await.unwrap();
            c.shutdown().await.unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let cfg = Config {
            version: 1,
            valid_for_seconds: 30,
            rules: vec![Rule {
                id: "slow".into(),
                lease_id: "lease".into(),
                cycle_id: "cycle".into(),
                listen_ip: "127.0.0.1".into(),
                listen_port: port,
                target_host: "127.0.0.1".into(),
                target_port: target,
                expires_at: now() + 60,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(e.apply(&cfg).await.is_empty());
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        let mut client = socket
            .connect(("127.0.0.1".parse::<std::net::IpAddr>().unwrap(), port).into())
            .await
            .unwrap();
        client.write_all(b"request").await.unwrap();
        client.shutdown().await.unwrap();
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        counts(&e, "slow", 1, 0).await;
        let mut response = vec![];
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response, vec![0x53; size]);
        sender.await.unwrap();
        counts(&e, "slow", 0, 0).await;
        e.close().await;
        drop(e);
        drop(j);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
#[ignore = "requires isolated forwarding-node network namespace"]
async fn wildcard_multinic_preserves_udp_sources_and_live_tcp() {
    // Run on the forwarding test node in an isolated network namespace; never
    // bind a public wildcard socket on the development panel host.
    assert_eq!(std::env::var("VISTART_MULTINIC_TEST").as_deref(), Ok("1"));
    let ips: Vec<std::net::Ipv4Addr> = std::env::var("VISTART_MULTINIC_IPS")
        .unwrap_or_else(|_| "127.0.0.1,127.0.0.2".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(ips.len(), 2);
    let dir = std::env::temp_dir().join(format!("forward-multinic-{}", id()));
    std::fs::create_dir(&dir).unwrap();
    let j = Journal::open(&dir.join("traffic.json")).unwrap();
    let e = Engine::new(
        j.clone(),
        Arc::new(Resolver::new(vec!["127.0.0.0/8".parse().unwrap()], vec![])),
        16,
        16,
    );
    let target = echo().await;
    let tmp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tmp.local_addr().unwrap().port();
    drop(tmp);
    let rule = Rule {
        id: id(),
        user_id: id(),
        lease_id: id(),
        cycle_id: id(),
        listen_ip: "0.0.0.0".into(),
        listen_port: port,
        target_host: "127.0.0.1".into(),
        target_port: target,
        expires_at: now() + 300,
        ..Default::default()
    };
    let mut config = Config {
        version: 1,
        revision: 1,
        valid_for_seconds: 30,
        rules: vec![rule.clone()],
        ..Default::default()
    };
    assert!(e.apply(&config).await.is_empty());
    let mut streams = vec![];
    for ip in &ips {
        let mut c = TcpStream::connect((*ip, port)).await.unwrap();
        c.write_all(b"two-nics").await.unwrap();
        let mut out = [0u8; 8];
        c.read_exact(&mut out).await.unwrap();
        assert_eq!(&out, b"two-nics");
        streams.push(c);
    }
    // Same client IP:port to both local NICs must remain independent flows.
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for _ in 0..32 {
        for ip in &ips {
            udp.send_to(&ip.octets(), (*ip, port)).await.unwrap();
            let mut out = [0u8; 32];
            let (n, from) = tokio::time::timeout(Duration::from_secs(2), udp.recv_from(&mut out))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&out[..n], &ip.octets());
            assert_eq!(from, (*ip, port).into());
        }
    }
    // A connected client silently discards replies with the wrong local source.
    let connected = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    connected.connect((ips[1], port)).await.unwrap();
    connected.send(b"connected").await.unwrap();
    let mut buf = [0u8; 32];
    let n = tokio::time::timeout(Duration::from_secs(2), connected.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"connected");
    let counts = e.rule_connections().await;
    assert_eq!(counts[&rule.id].tcp, 2);
    assert_eq!(counts[&rule.id].udp, 3);
    for _ in 0..3 {
        config.revision += 1;
        assert!(e.apply(&config).await.is_empty());
        for c in &mut streams {
            c.write_all(b"live").await.unwrap();
            c.read_exact(&mut buf[..4]).await.unwrap();
            assert_eq!(&buf[..4], b"live");
        }
    }
    let mut clash = rule.clone();
    clash.id = id();
    clash.listen_ip = ips[1].to_string();
    config.rules.push(clash.clone());
    let errors = e.apply(&config).await;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].rule_id, clash.id);
    assert_eq!(errors[0].code, "bind_failed");
    for c in &mut streams {
        c.write_all(b"safe").await.unwrap();
        c.read_exact(&mut buf[..4]).await.unwrap();
        assert_eq!(&buf[..4], b"safe");
    }

    let billed = j.meter(&rule.id, &rule.cycle_id).counter();
    assert_eq!(billed.up, 313);
    assert_eq!(billed.down, 313);
    e.close().await;
    drop(e);
    drop(j);
    std::fs::remove_dir_all(dir).unwrap();
}
