use std::{
    io, mem,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    os::fd::AsRawFd,
};
use tokio::{io::Interest, net::UdpSocket};

/// A peer using two local NIC addresses has two independent UDP flows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct Route {
    peer: SocketAddr,
    local: Option<Ipv4Addr>,
}

pub(super) struct Front {
    socket: UdpSocket,
    packet_info: bool,
}

impl Front {
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        let packet_info = addr.is_ipv4() && addr.ip().is_unspecified();
        if packet_info {
            let on: libc::c_int = 1;
            // The descriptor is owned by socket; on is a live, correctly sized int.
            let result = unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::IPPROTO_IP,
                    libc::IP_PKTINFO,
                    (&on as *const libc::c_int).cast(),
                    mem::size_of_val(&on) as _,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            socket,
            packet_info,
        })
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, Route)> {
        if !self.packet_info {
            let (n, peer) = self.socket.recv_from(buf).await?;
            return Ok((n, Route { peer, local: None }));
        }
        self.socket
            .async_io(Interest::READABLE, || recv(self.socket.as_raw_fd(), buf))
            .await
    }

    pub async fn send_to(&self, buf: &[u8], route: Route) -> io::Result<usize> {
        match route.local {
            Some(local) => {
                self.socket
                    .async_io(Interest::WRITABLE, || {
                        send(self.socket.as_raw_fd(), buf, route.peer, local)
                    })
                    .await
            }
            None => self.socket.send_to(buf, route.peer).await,
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid UDP packet metadata")
}

// glibc uses usize and musl uses u32 for the ancillary length fields.
#[allow(clippy::unnecessary_cast)]
fn recv(fd: libc::c_int, data: &mut [u8]) -> io::Result<(usize, Route)> {
    // All syscall pointers refer to owned stack storage or the exclusively borrowed
    // payload. usize storage provides cmsghdr alignment on supported Linux targets.
    let mut peer: libc::sockaddr_in = unsafe { mem::zeroed() };
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_name = (&mut peer as *mut libc::sockaddr_in).cast();
    msg.msg_namelen = mem::size_of_val(&peer) as _;
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = mem::size_of_val(&control) as _;
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
        || n as usize > data.len()
        || msg.msg_namelen as usize != mem::size_of_val(&peer)
        || peer.sin_family != libc::AF_INET as libc::sa_family_t
        || msg.msg_controllen as usize > mem::size_of_val(&control)
    {
        return Err(invalid());
    }
    let mut local = None;
    // Linux supplied the ancillary headers; bounds and payload sizes are checked
    // before reading in_pktinfo, and no pointers escape this synchronous function.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            let offset = (cmsg as usize)
                .checked_sub(control.as_ptr() as usize)
                .ok_or_else(invalid)?;
            if offset + mem::size_of::<libc::cmsghdr>() > msg.msg_controllen as usize {
                return Err(invalid());
            }
            let len = (*cmsg).cmsg_len as usize;
            if len < libc::CMSG_LEN(0) as usize || len > msg.msg_controllen as usize - offset {
                return Err(invalid());
            }
            if (*cmsg).cmsg_level == libc::IPPROTO_IP && (*cmsg).cmsg_type == libc::IP_PKTINFO {
                if len < libc::CMSG_LEN(mem::size_of::<libc::in_pktinfo>() as _) as usize {
                    return Err(invalid());
                }
                let info =
                    std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<libc::in_pktinfo>());
                let ip = Ipv4Addr::from(info.ipi_addr.s_addr.to_ne_bytes());
                // The packet destination determines the reply source. The route-preferred
                // source can belong to another NIC and must not override it.
                if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
                    return Err(invalid());
                }
                local = Some(ip);
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    let local = local.ok_or_else(invalid)?;
    let peer = SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::from(peer.sin_addr.s_addr.to_ne_bytes()),
        u16::from_be(peer.sin_port),
    ));
    Ok((
        n as usize,
        Route {
            peer,
            local: Some(local),
        },
    ))
}

fn send(fd: libc::c_int, data: &[u8], peer: SocketAddr, local: Ipv4Addr) -> io::Result<usize> {
    let SocketAddr::V4(peer) = peer else {
        return Err(invalid());
    };
    let mut to: libc::sockaddr_in = unsafe { mem::zeroed() };
    to.sin_family = libc::AF_INET as _;
    to.sin_port = peer.port().to_be();
    to.sin_addr.s_addr = u32::from_ne_bytes(peer.ip().octets());
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: data.as_ptr().cast_mut().cast(),
        iov_len: data.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_name = (&mut to as *mut libc::sockaddr_in).cast();
    msg.msg_namelen = mem::size_of_val(&to) as _;
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    // sendmsg reads the payload and does not mutate data. Header + in_pktinfo
    // fit in the aligned stack buffer, checked before constructing the header.
    unsafe {
        let size = libc::CMSG_SPACE(mem::size_of::<libc::in_pktinfo>() as _) as usize;
        if size > mem::size_of_val(&control) {
            return Err(invalid());
        }
        msg.msg_controllen = size as _;
        let header = libc::CMSG_FIRSTHDR(&msg);
        if header.is_null() {
            return Err(invalid());
        }
        (*header).cmsg_level = libc::IPPROTO_IP;
        (*header).cmsg_type = libc::IP_PKTINFO;
        (*header).cmsg_len = libc::CMSG_LEN(mem::size_of::<libc::in_pktinfo>() as _) as _;
        let mut info: libc::in_pktinfo = mem::zeroed();
        info.ipi_spec_dst.s_addr = u32::from_ne_bytes(local.octets());
        // Keep normal routing policy; only the source address must match ingress.
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<libc::in_pktinfo>(), info);
        let n = libc::sendmsg(fd, &msg, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL);
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}
