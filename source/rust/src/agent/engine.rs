use super::{
    journal::{Journal, Meter},
    limits::Gate,
    memory::{Memory, TcpMemory, UDP_BUFFER},
    resolver::Resolver,
};
use crate::{now, protocol::*, valid_id, valid_target};
use anyhow::{Result, bail};
use arc_swap::ArcSwap;
use std::{
    collections::{BTreeMap, HashMap},
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream, UdpSocket},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct Connections {
    tcp: Arc<AtomicU32>,
    udp: Arc<AtomicU32>,
}
impl Connections {
    fn snapshot(&self) -> RuleConnections {
        RuleConnections {
            tcp: self.tcp.load(Ordering::Relaxed),
            udp: self.udp.load(Ordering::Relaxed),
        }
    }
}
// Dropping a forwarding task (including cancellation/errors) always releases its gauge.
struct ConnectionGuard(Arc<AtomicU32>);
impl ConnectionGuard {
    fn enter(count: Arc<AtomicU32>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Active {
    rule: Rule,
    meter: Arc<Meter>,
    gate: Arc<Gate>,
    connections: Arc<Connections>,
}
struct Forward {
    active: Arc<ArcSwap<Active>>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
pub struct Engine {
    rules: Mutex<HashMap<String, Forward>>,
    leases: std::sync::Mutex<HashMap<String, Arc<Gate>>>,
    pub journal: Arc<Journal>,
    pub resolver: Arc<Resolver>,
    tcp: Arc<Semaphore>,
    udp: Arc<Semaphore>,
    queued: Arc<Semaphore>,
    deadline: AtomicI64,
    stop: CancellationToken,
    checks: Mutex<Vec<TargetCheck>>,
    probes: Arc<Semaphore>,
    memory: Memory,
}
impl Engine {
    pub fn new(
        journal: Arc<Journal>,
        resolver: Arc<Resolver>,
        tcp: usize,
        udp: usize,
    ) -> Arc<Self> {
        let e = Arc::new(Self {
            rules: Mutex::new(HashMap::new()),
            leases: std::sync::Mutex::new(HashMap::new()),
            journal,
            resolver,
            tcp: Arc::new(Semaphore::new(tcp)),
            udp: Arc::new(Semaphore::new(udp)),
            queued: Arc::new(Semaphore::new(8 << 20)),
            deadline: AtomicI64::new(0),
            stop: CancellationToken::new(),
            checks: Mutex::new(Vec::new()),
            probes: Arc::new(Semaphore::new(8)),
            memory: Memory::discover(),
        });
        let weak = Arc::downgrade(&e);
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(1));
            loop {
                t.tick().await;
                let Some(e) = weak.upgrade() else { break };
                if e.stop.is_cancelled() {
                    break;
                }
                let mut map = e.rules.lock().await;
                let stale = now() > e.deadline.load(Ordering::Relaxed);
                let ids: Vec<_> = map
                    .iter()
                    .filter(|(_, f)| stale || f.active.load().rule.expires_at <= now())
                    .map(|(k, _)| k.clone())
                    .collect();
                for id in ids {
                    if let Some(f) = map.remove(&id) {
                        f.cancel.cancel();
                        let _ = f.task.await;
                    }
                }
            }
        });
        let weak = Arc::downgrade(&e);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(e) = weak.upgrade() else { break };
                if e.stop.is_cancelled() {
                    break;
                }
                e.check_targets().await;
            }
        });
        e
    }
    pub async fn suspend(&self) {
        let mut map = self.rules.lock().await;
        for (_, f) in map.drain() {
            f.cancel.cancel();
            let _ = f.task.await;
        }
        self.checks.lock().await.clear();
        self.leases.lock().unwrap().clear();
    }
    pub async fn close(&self) {
        self.stop.cancel();
        self.suspend().await;
    }
    pub async fn apply(self: &Arc<Self>, c: &Config) -> Vec<RuleError> {
        let mut errors = Vec::new();
        if c.version != 1
            || c.valid_for_seconds == 0
            || c.valid_for_seconds > 30
            || c.rules.len() > 512
        {
            self.suspend().await;
            return vec![RuleError {
                code: "invalid_config".into(),
                message: "configuration rejected".into(),
                ..Default::default()
            }];
        }
        self.deadline
            .store(now() + c.valid_for_seconds as i64, Ordering::Relaxed);
        let mut wanted = HashMap::new();
        for r in &c.rules {
            let good_ip = r
                .listen_ip
                .parse::<IpAddr>()
                .is_ok_and(|i| !i.is_unspecified() && !i.is_multicast());
            let targets = r.targets();
            if !good_ip
                || r.listen_port < 1024
                || !valid_id(&r.id)
                || !valid_id(&r.cycle_id)
                || r.expires_at <= now()
                || !r.limits.valid()
                || !valid_id(&r.lease_id)
                || !valid_target(&r.target_host, r.target_port)
                || (r.load_balance && (targets.len() < 2 || targets.len() > 16))
                || targets.iter().any(|t| !valid_target(&t.host, t.port))
            {
                errors.push(RuleError {
                    rule_id: r.id.clone(),
                    code: "invalid_rule".into(),
                    message: "invalid or expired rule".into(),
                });
                continue;
            }
            wanted.entry(r.id.clone()).or_insert_with(|| r.clone());
        }
        {
            let mut leases = self.leases.lock().unwrap();
            let policies: HashMap<_, _> =
                wanted.values().map(|r| (&r.lease_id, &r.limits)).collect();
            for (id, policy) in policies {
                let gate = leases
                    .entry(id.clone())
                    .or_insert_with(|| Gate::new(id.clone()));
                gate.configure(
                    policy,
                    c.lease_limits.iter().find(|g| g.lease_id == *id),
                    c.valid_for_seconds,
                );
            }
            leases.retain(|id, g| !g.idle() || wanted.values().any(|r| r.lease_id == *id));
        }
        let mut map = self.rules.lock().await;
        let mut remove = Vec::new();
        for (id, f) in map.iter() {
            let old = f.active.load();
            if let Some(r) = wanted.get(id)
                && !f.task.is_finished()
                && old.rule.lease_id == r.lease_id
                && old.rule.listen_ip == r.listen_ip
                && old.rule.listen_port == r.listen_port
                && old.rule.targets() == r.targets()
            {
                if old.rule != *r {
                    f.active.store(Arc::new(Active {
                        rule: r.clone(),
                        meter: self.journal.meter(&r.id, &r.cycle_id),
                        gate: old.gate.clone(),
                        connections: old.connections.clone(),
                    }));
                }
                continue;
            }
            remove.push(id.clone());
        }
        for id in remove {
            if let Some(f) = map.remove(&id) {
                f.cancel.cancel();
                let _ = f.task.await;
            }
        }
        for (id, r) in wanted {
            if map.contains_key(&id) {
                continue;
            }
            match self.start(r).await {
                Ok(f) => {
                    map.insert(id, f);
                }
                Err(_) => errors.push(RuleError {
                    rule_id: id,
                    code: "bind_failed".into(),
                    message: "TCP/UDP port group could not start".into(),
                }),
            }
        }
        errors
    }
    async fn start(self: &Arc<Self>, r: Rule) -> Result<Forward> {
        let addr = SocketAddr::new(r.listen_ip.parse()?, r.listen_port);
        let listener_memory = self
            .memory
            .listener()
            .ok_or_else(|| anyhow::anyhow!("socket memory budget exhausted"))?;
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        socket.set_reuseaddr(true)?;
        socket.set_send_buffer_size(64 * 1024)?;
        // Leave window scaling negotiation to Linux; size the accepted socket
        // before any task can read/write it. Bound the pending accept queue.
        socket.bind(addr)?;
        let tcp = socket.listen(16)?;
        let udp = Arc::new(UdpSocket::bind(addr).await?);
        tune_udp(&udp)?;
        let active = Arc::new(ArcSwap::from_pointee(Active {
            meter: self.journal.meter(&r.id, &r.cycle_id),
            gate: self.leases.lock().unwrap()[&r.lease_id].clone(),
            connections: Arc::new(Connections::default()),
            rule: r,
        }));
        let cancel = CancellationToken::new();
        let cursor = Arc::new(AtomicUsize::new(0));
        let e = self.clone();
        let a = active.clone();
        let c = cancel.clone();
        let cur = cursor.clone();
        let task = tokio::spawn(async move {
            let _listener_memory = listener_memory;
            let mut t = tokio::spawn(tcp_loop(e.clone(), a.clone(), c.clone(), cur.clone(), tcp));
            let mut u = tokio::spawn(udp_loop(e, a, c.clone(), cur, udp));
            // A paired port is healthy only while both protocol loops are alive.
            tokio::select! {
                _ = &mut t => { c.cancel(); let _ = u.await; },
                _ = &mut u => { c.cancel(); let _ = t.await; },
            }
        });
        Ok(Forward {
            active,
            cancel,
            task,
        })
    }
    pub fn lease_usage(&self) -> Vec<LeaseUsage> {
        self.leases
            .lock()
            .unwrap()
            .values()
            .map(|g| g.usage())
            .collect()
    }
    pub async fn active_rule_ids(&self) -> Vec<String> {
        let map = self.rules.lock().await;
        let mut ids: Vec<_> = map
            .iter()
            .filter(|(_, f)| !f.task.is_finished() && !f.cancel.is_cancelled())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }
    pub async fn rule_connections(&self) -> BTreeMap<String, RuleConnections> {
        self.rules
            .lock()
            .await
            .iter()
            .filter(|(_, f)| !f.task.is_finished() && !f.cancel.is_cancelled())
            .map(|(id, f)| (id.clone(), f.active.load().connections.snapshot()))
            .collect()
    }
    pub async fn target_checks(&self) -> Vec<TargetCheck> {
        self.checks.lock().await.clone()
    }
    async fn check_targets(&self) {
        let items: Vec<_> = {
            let map = self.rules.lock().await;
            map.values()
                .flat_map(|f| {
                    let a = f.active.load();
                    a.rule
                        .targets()
                        .into_iter()
                        .map(|t| (a.rule.id.clone(), t))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let mut tasks = JoinSet::new();
        for (rule, t) in items {
            let resolver = self.resolver.clone();
            let slots = self.probes.clone();
            tasks.spawn(async move {
                let _p = slots.acquire().await.ok();
                let start = Instant::now();
                let check = async {
                    let ip = resolver.resolve(&t.host).await?;
                    let c = tokio::time::timeout(
                        Duration::from_secs(3),
                        TcpStream::connect((ip, t.port)),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("timeout"))??;
                    drop(c);
                    Ok::<_, anyhow::Error>(())
                };
                let result = tokio::time::timeout(Duration::from_secs(4), check).await;
                let (status, latency_ms) = match result {
                    Ok(Ok(())) => (
                        "ok".into(),
                        (start.elapsed().as_secs_f64() * 10000.0).round().max(1.0) / 10.0,
                    ),
                    Err(_) => ("timeout".into(), 0.0),
                    Ok(Err(e)) => {
                        let msg = e.to_string();
                        let code = if msg == "blocked" {
                            "blocked"
                        } else if msg == "dns_error" {
                            "dns_error"
                        } else if msg.contains("refused") {
                            "refused"
                        } else if msg.contains("timeout") || msg.contains("timed out") {
                            "timeout"
                        } else {
                            "unreachable"
                        };
                        (code.into(), 0.0)
                    }
                };
                TargetCheck {
                    rule_id: rule,
                    host: t.host,
                    port: t.port,
                    status,
                    latency_ms,
                    checked_at: now(),
                }
            });
        }
        let mut out = Vec::new();
        loop {
            tokio::select! {_=self.stop.cancelled()=>{tasks.shutdown().await;return},r=tasks.join_next()=>match r{Some(Ok(c))=>out.push(c),Some(Err(_))=>(),None=>break}}
        }
        *self.checks.lock().await = out;
    }
}
fn ordered(a: &ArcSwap<Active>, cursor: &AtomicUsize) -> Vec<Target> {
    let mut list = a.load().rule.targets();
    let n = list.len();
    if n > 1 {
        list.rotate_left(cursor.fetch_add(1, Ordering::Relaxed) % n)
    }
    list
}
fn tune_buffers(sock: &socket2::SockRef<'_>, bytes: usize) -> std::io::Result<()> {
    sock.set_send_buffer_size(bytes)?;
    sock.set_recv_buffer_size(bytes)?;
    Ok(())
}
fn tune_udp(c: &UdpSocket) -> std::io::Result<()> {
    tune_buffers(&socket2::SockRef::from(c), UDP_BUFFER)
}
fn tune(c: &TcpStream, bytes: usize) -> std::io::Result<()> {
    let _ = c.set_nodelay(true);
    let sock = socket2::SockRef::from(c);
    tune_buffers(&sock, bytes)?;
    // Error/cancellation must release queued kernel data before the memory
    // reservation is reused. Successful streams disable this after ACK drain.
    sock.set_linger(Some(Duration::ZERO))?;
    let _ = sock.set_tcp_notsent_lowat(64 * 1024);
    let _ = sock.set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(30))
            .with_interval(Duration::from_secs(10))
            .with_retries(3),
    );
    Ok(())
}
async fn tcp_loop(
    e: Arc<Engine>,
    a: Arc<ArcSwap<Active>>,
    cancel: CancellationToken,
    cursor: Arc<AtomicUsize>,
    listener: TcpListener,
) {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {biased;
         _=cancel.cancelled()=>break,
         Some(_)=tasks.join_next(),if !tasks.is_empty()=>(),
         result=listener.accept()=>match result{
          Ok((client,_))=>{
           let Ok(slot)=e.tcp.clone().try_acquire_owned()else{drop(client);continue};
           let Some(memory)=e.memory.tcp()else{drop(client);continue};
           if tune(&client,memory.buffer).is_err(){continue;}
           let e=e.clone();let a=a.clone();let c=cancel.clone();let targets=ordered(&a,&cursor);
           tasks.spawn(async move{let _slot=slot;tokio::select!{_=c.cancelled()=>(),_=stream(e,a,client,targets,memory)=>()}});
          },
          Err(_)=>tokio::time::sleep(Duration::from_millis(50)).await
         }
        }
    }
    drop(listener);
    tasks.shutdown().await;
}
async fn stream(
    e: Arc<Engine>,
    a: Arc<ArcSwap<Active>>,
    mut client: TcpStream,
    targets: Vec<Target>,
    memory: TcpMemory,
) {
    let gate = a.load().gate.clone();
    let Some(_lease_slot) = gate.tcp().await else {
        return;
    };
    let dial = async {
        for t in targets {
            if let Ok(ip) = e.resolver.resolve(&t.host).await {
                let socket = if ip.is_ipv4() {
                    TcpSocket::new_v4()?
                } else {
                    TcpSocket::new_v6()?
                };
                tune_buffers(&socket2::SockRef::from(&socket), memory.buffer)?;
                if let Ok(Ok(c)) = tokio::time::timeout(
                    Duration::from_secs(3),
                    socket.connect((ip, t.port).into()),
                )
                .await
                {
                    return Ok(c);
                }
            }
        }
        bail!("unreachable")
    };
    let Ok(Ok(mut remote)) = tokio::time::timeout(Duration::from_secs(8), dial).await else {
        return;
    };
    let _connection = ConnectionGuard::enter(a.load().connections.tcp.clone());
    if tune(&remote, memory.buffer).is_err() {
        return;
    }
    let copied = {
        let (cr, cw) = client.split();
        let (rr, rw) = remote.split();
        tokio::try_join!(pump(cr, rw, a.clone(), true), pump(rr, cw, a, false))
    };
    if copied.is_ok()
        && async {
            loop {
                if send_queue_empty(&client)? && send_queue_empty(&remote)? {
                    return Ok::<_, std::io::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        .await
        .is_ok()
    {
        // Keep the reservation while a slow reader drains; expiry/cancellation
        // still closes the stream. Do not time out a healthy half-closed stream.
        // Both payload queues have been acknowledged. Preserve normal FIN
        // semantics and half-close; error/cancellation retains abortive close.
        let _ = socket2::SockRef::from(&client).set_linger(None);
        let _ = socket2::SockRef::from(&remote).set_linger(None);
    }
    drop(remote);
    drop(client);
    drop(memory);
}
fn send_queue_empty(socket: &TcpStream) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut queued: libc::c_int = 0;
    // TIOCOUTQ writes one int; socket remains owned and open for this call.
    if unsafe { libc::ioctl(socket.as_raw_fd(), libc::TIOCOUTQ, &mut queued) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(queued == 0)
}
async fn pump<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    mut src: R,
    mut dst: W,
    a: Arc<ArcSwap<Active>>,
    up: bool,
) -> Result<()> {
    let mut buf = vec![0u8; 32768];
    let gate = a.load().gate.clone();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(300), src.read(&mut buf)).await??;
        if n == 0 {
            dst.shutdown().await?;
            return Ok(());
        }
        let mut at = 0;
        while at < n {
            let allowance = if gate.bandwidth_limited() {
                gate.take(n - at).await
            } else {
                n - at
            };
            let m =
                tokio::time::timeout(Duration::from_secs(15), dst.write(&buf[at..at + allowance]))
                    .await??;
            gate.refund(allowance - m);
            if m == 0 {
                bail!("write zero")
            }
            a.load().meter.add(m, up);
            at += m;
        }
    }
}
type Packet = (Vec<u8>, OwnedSemaphorePermit);
async fn udp_loop(
    e: Arc<Engine>,
    a: Arc<ArcSwap<Active>>,
    cancel: CancellationToken,
    cursor: Arc<AtomicUsize>,
    socket: Arc<UdpSocket>,
) {
    let mut clients: HashMap<SocketAddr, mpsc::Sender<Packet>> = HashMap::new();
    let mut tasks = JoinSet::new();
    let mut buf = vec![0u8; 65535];
    loop {
        tokio::select! {biased;
         _=cancel.cancelled()=>break,
         Some(_)=tasks.join_next(),if !tasks.is_empty()=>{clients.retain(|_,tx|!tx.is_closed());},
         result=socket.recv_from(&mut buf)=>{let Ok((n,client))=result else{break};
          if clients.get(&client).is_some_and(|tx|tx.is_closed()){clients.remove(&client);}
          if let std::collections::hash_map::Entry::Vacant(entry) = clients.entry(client){
           let Ok(slot)=e.udp.clone().try_acquire_owned()else{continue};
           let Some(lease_slot)=a.load().gate.udp()else{continue};
           // Absorb a full scheduler burst; the shared byte semaphore still caps total queued payload at 8 MiB.
           let(tx,rx)=mpsc::channel(128);entry.insert(tx);
           let(e,a,cursor,socket,c)=(e.clone(),a.clone(),cursor.clone(),socket.clone(),cancel.clone());
           tasks.spawn(async move{let _slot=slot;let _lease_slot=lease_slot;tokio::select!{_=c.cancelled()=>(),_=udp_session(e,a,cursor,socket,client,rx)=>()}});
          }
          if let Ok(bytes)=e.queued.clone().try_acquire_many_owned(n.max(1) as u32){let _=clients[&client].try_send((buf[..n].to_vec(),bytes));}
         }
        }
    }
    clients.clear();
    tasks.shutdown().await;
}
async fn udp_session(
    e: Arc<Engine>,
    a: Arc<ArcSwap<Active>>,
    cursor: Arc<AtomicUsize>,
    front: Arc<UdpSocket>,
    client: SocketAddr,
    mut rx: mpsc::Receiver<Packet>,
) {
    let gate = a.load().gate.clone();
    let Some(_memory) = e.memory.udp() else {
        return;
    };
    let mut selected = None;
    for t in ordered(&a, &cursor) {
        if let Ok(ip) = e.resolver.resolve(&t.host).await {
            selected = Some((t, ip));
            break;
        }
    }
    let Some((target, mut ip)) = selected else {
        return;
    };
    async fn connect(ip: IpAddr, port: u16) -> Result<UdpSocket> {
        let s = UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await?;
        tune_udp(&s)?;
        s.connect((ip, port)).await?;
        Ok(s)
    }
    let Ok(mut backend) = connect(ip, target.port).await else {
        return;
    };
    let _connection = ConnectionGuard::enter(a.load().connections.udp.clone());
    let mut checked = Instant::now();
    let mut last = Instant::now();
    let mut buf = vec![0u8; 65535];
    loop {
        tokio::select! {
         _=tokio::time::sleep_until((last+Duration::from_secs(30)).into())=>break,
         packet=rx.recv()=>{let Some((data,_bytes))=packet else{break};
          last=Instant::now();
          if !gate.packet(data.len()) {continue;}
          if checked.elapsed()>=e.resolver.ttl {
           let Ok(new_ip)=e.resolver.resolve(&target.host).await else{break};
           if new_ip!=ip{let Ok(s)=connect(new_ip,target.port).await else{break};backend=s;ip=new_ip;}checked=Instant::now();
          }
          if let Ok(Ok(n))=tokio::time::timeout(Duration::from_secs(3),backend.send(&data)).await{a.load().meter.add(n,true);last=Instant::now();}else{break}
         },
         r=backend.recv(&mut buf)=>{let Ok(n)=r else{break};last=Instant::now();if !gate.packet(n){continue;}
         if let Ok(Ok(w))=tokio::time::timeout(Duration::from_secs(3),front.send_to(&buf[..n],client)).await{a.load().meter.add(w,false);last=Instant::now();}else{break}}
        }
    }
}
