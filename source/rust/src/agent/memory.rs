//! Reserve socket buffers as well as userspace buffers before accepting work.
//! Linux doubles SO_{SND,RCV}BUF for accounting. Never change host-wide sysctls.
use std::{
    fs,
    path::{Component, Path},
    sync::Arc,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
pub const KIB: usize = 1024;
pub const MIB: usize = 1024 * KIB;
const TCP_BASE: usize = 256 * KIB;
const MIN_BUFFER: usize = 32 * KIB;
const MAX_BUFFER: usize = 4 * MIB;
pub const UDP_BUFFER: usize = 256 * KIB;

pub fn service_memory_mib(host_kib: u64) -> u64 {
    (host_kib / 4096).clamp(64, 2048)
}
fn host_memory_kib() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find(|s| s.starts_with("MemTotal:"))
        .and_then(|s| s.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(128 * 1024)
}
fn hierarchy_limit(root: &Path, relative: &str, file: &str) -> Option<u64> {
    let relative = Path::new(relative.trim_start_matches('/'));
    if relative
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        // The empty path is the cgroup namespace root.
        if !relative.as_os_str().is_empty() {
            return None;
        }
    }
    let mut path = root.join(relative);
    let mut limit = None;
    loop {
        if let Some(value) = fs::read_to_string(path.join(file))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n < (1u64 << 60))
        {
            limit = Some(limit.map_or(value, |n: u64| n.min(value)));
        }
        if path == root || !path.pop() {
            break;
        }
    }
    limit
}
pub fn effective_memory_bytes() -> usize {
    let mut budget = service_memory_mib(host_memory_kib()) * MIB as u64;
    let groups = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    for line in groups.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(_), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let limit = if controllers.is_empty() {
            hierarchy_limit(Path::new("/sys/fs/cgroup"), path, "memory.max")
        } else if controllers.split(',').any(|c| c == "memory") {
            hierarchy_limit(
                Path::new("/sys/fs/cgroup/memory"),
                path,
                "memory.limit_in_bytes",
            )
        } else {
            None
        };
        if let Some(limit) = limit {
            budget = budget.min(limit);
        }
    }
    budget.min(usize::MAX as u64) as usize
}
pub struct Memory {
    slots: Arc<Semaphore>,
    max_buffer: usize,
}
pub struct TcpMemory {
    _permit: OwnedSemaphorePermit,
    pub buffer: usize,
}
impl Memory {
    pub fn discover() -> Self {
        let mut memory = Self::new(effective_memory_bytes());
        // SO_*BUF cannot exceed these unprivileged kernel caps. Do not reserve
        // large buffers the kernel would clamp, and never change host sysctls.
        for file in ["/proc/sys/net/core/rmem_max", "/proc/sys/net/core/wmem_max"] {
            if let Some(cap) = fs::read_to_string(file)
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
            {
                memory.max_buffer = memory.max_buffer.min(cap.max(MIN_BUFFER));
            }
        }
        memory
    }
    fn new(limit: usize) -> Self {
        // Keep half of the remainder for kernel overhead, accept queues, control
        // messages, journal snapshots and buffers awaiting kernel reclamation.
        let bytes = limit.saturating_sub(24 * MIB) / 2;
        Self {
            slots: Arc::new(Semaphore::new(bytes / KIB)),
            max_buffer: MAX_BUFFER,
        }
    }
    fn reserve(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        self.slots
            .clone()
            .try_acquire_many_owned(bytes.div_ceil(KIB) as u32)
            .ok()
    }
    pub fn tcp(&self) -> Option<TcpMemory> {
        let available = self.slots.available_permits().saturating_mul(KIB);
        // Large windows on lightly loaded nodes; bounded smaller windows when
        // capacity is shared. Existing streams keep their reserved buffers.
        let buffer = ((available / 32).saturating_sub(TCP_BASE) / 8 / MIN_BUFFER * MIN_BUFFER)
            .clamp(MIN_BUFFER, self.max_buffer);
        // Two sockets, send+receive on each, and Linux's doubling: 2*2*2.
        let permit = self.reserve(TCP_BASE + 8 * buffer)?;
        Some(TcpMemory {
            _permit: permit,
            buffer,
        })
    }
    pub fn udp(&self) -> Option<OwnedSemaphorePermit> {
        self.reserve(4 * UDP_BUFFER + 128 * KIB)
    }
    pub fn listener(&self) -> Option<OwnedSemaphorePermit> {
        self.reserve(4 * UDP_BUFFER + 512 * KIB)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservations_are_bounded_and_drop_releases_capacity() {
        let memory = Memory::new(128 * MIB);
        let capacity = memory.slots.available_permits();
        let mut held = vec![];
        while let Some(slot) = memory.tcp() {
            held.push(slot);
        }
        assert!(held.len() > 8 && held.len() < 512);
        assert!(memory.tcp().is_none());
        let cost: usize = held.iter().map(|s| (TCP_BASE + 8 * s.buffer) / KIB).sum();
        assert!(cost <= capacity);
        assert!(
            held.iter()
                .all(|s| s.buffer >= MIN_BUFFER && s.buffer <= MAX_BUFFER)
        );
        held.clear();
        assert_eq!(memory.slots.available_permits(), capacity);
        let udp = memory.udp().unwrap();
        let listener = memory.listener().unwrap();
        assert!(memory.slots.available_permits() < capacity);
        drop((udp, listener));
        assert_eq!(memory.slots.available_permits(), capacity);
        assert!(Memory::new(16 * MIB).tcp().is_none());
    }
    #[test]
    fn cgroup_parent_limits_and_unlimited_children_are_honored() {
        let dir = std::env::temp_dir().join(format!("forward-cgroup-{}", crate::id()));
        let child = dir.join("slice/agent");
        fs::create_dir_all(&child).unwrap();
        fs::write(dir.join("memory.max"), "max").unwrap();
        fs::write(dir.join("slice/memory.max"), (128 * MIB).to_string()).unwrap();
        fs::write(child.join("memory.max"), "max").unwrap();
        assert_eq!(
            hierarchy_limit(&dir, "/slice/agent", "memory.max"),
            Some((128 * MIB) as u64)
        );
        fs::write(child.join("memory.max"), (64 * MIB).to_string()).unwrap();
        assert_eq!(
            hierarchy_limit(&dir, "/slice/agent", "memory.max"),
            Some((64 * MIB) as u64)
        );
        assert_eq!(hierarchy_limit(&dir, "/../escape", "memory.max"), None);
        assert_eq!(service_memory_mib(128 * 1024), 64);
        assert_eq!(service_memory_mib(1024 * 1024), 256);
        assert_eq!(service_memory_mib(8 * 1024 * 1024), 2048);
        assert_eq!(service_memory_mib(u64::MAX), 2048);
        fs::remove_dir_all(dir).unwrap();
    }
}
