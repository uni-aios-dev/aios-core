//! Kernel IPv4 socket layer (v2.49.0): UDP datagrams plus a minimal
//! active-open TCP client, exposed to ring-3 tasks through the `SYS_SOCK_*`
//! syscall family.
//!
//! Everything is tick driven and lock-free (single CPU): [`tick`] is called
//! from [`crate::net::poll`] each 10 ms and advances retransmit/time-wait
//! timers, while inbound segments are demultiplexed by
//! [`deliver_udp`]/[`deliver_tcp`] from the IPv4 receive path. Outbound frames
//! go through [`crate::net::emit_ipv4`], which resolves the next-hop MAC via the
//! ARP cache and falls back to broadcast, so the sockets share routing/ARP with
//! the DHCP/DNS probe client.
//!
//! The TCP side is deliberately small: sends larger than one segment
//! are queued per socket and segmented into MSS-sized chunks with one
//! segment in flight at a time (stop-and-wait over the queue, since
//! v2.53.0), with Reno-style congestion-control accounting
//! (`cwnd`/`ssthresh`, since v2.54.0), an 8 KiB receive ring and
//! an 8 KiB transmit queue per socket. Since v2.50.0 it covers both
//! directions — a minimal **client** (active open) and a minimal
//! **server** ([`listen`] + [`accept`] with a per-listener child
//! created at the SYN-ACK and handed out once the three-way handshake
//! completes). It is enough for a request/response exchange such as a
//! plain HTTP GET or a small echo server; it is not a general
//! streaming server. UDP sockets queue up to [`UDP_SLOTS`] datagrams
//! with their source address for `recv`.

use core::ptr::addr_of_mut;

use crate::net;

/// Maximum number of simultaneously open sockets.
pub const MAX_SOCKS: usize = 16;
/// TCP receive ring capacity per socket.
const TCP_RXCAP: usize = 8192;
/// Largest payload accepted by [`send`] on a TCP socket (one segment).
const TCP_MSS: usize = 1460;
/// Transmit queue capacity per TCP socket (segmentation backlog).
const TX_QCAP: usize = 8192;
/// Largest UDP datagram payload accepted by [`send`].
const UDP_MTU: usize = 1024;
/// Queued inbound datagrams per UDP socket.
const UDP_SLOTS: usize = 3;
/// Retransmit timeout in ticks (0.5 s).
const REXMIT_TICKS: u64 = 50;
/// Retries before a TCP handshake/segment is abandoned.
const REXMIT_MAX: u8 = 4;
/// Time the TimeWait state is held before the slot is released (4 s).
const TIME_WAIT_TICKS: u64 = 400;
/// Initial TCP send window advertised in every segment.
const TCP_WINDOW: u16 = 64240;

/// Transport selector passed to [`open`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Connectionless datagrams.
    Udp,
    /// Reliable byte stream (active open only).
    Tcp,
}

/// TCP connection states tracked per socket.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TcpState {
    Closed,
    /// Server endpoint awaiting an inbound SYN (created by `listen`).
    Listen,
    /// Server handshake in progress: SYN-ACK sent, awaiting the client ACK.
    SynRecv,
    SynSent,
    Established,
    CloseWait,
    FinWait1,
    FinWait2,
    LastAck,
    TimeWait,
}

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

/// A queued inbound UDP datagram with its 5-tuple source.
#[derive(Clone, Copy)]
struct UdpDgram {
    len: usize,
    src_ip: [u8; 4],
    src_port: u16,
    data: [u8; UDP_MTU],
}

impl UdpDgram {
    const EMPTY: UdpDgram = UdpDgram {
        len: 0,
        src_ip: [0; 4],
        src_port: 0,
        data: [0; UDP_MTU],
    };
}

/// One socket slot. UDP and TCP share the record; the unused half stays zeroed.
#[derive(Clone, Copy)]
struct Socket {
    used: bool,
    kind: Kind,
    pid: u32,
    port: u16,
    rip: [u8; 4],
    rport: u16,
    eof: bool,
    // UDP receive queue (ring of [`UDP_SLOTS`]).
    udp_head: usize,
    udp_len: usize,
    udp_rx: [UdpDgram; UDP_SLOTS],
    // TCP connection.
    state: TcpState,
    /// Listener descriptor that created this child connection (`usize::MAX`
    /// for sockets opened directly). Accepted children reset it.
    parent: usize,
    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    rcv_nxt: u32,
    rx_head: usize,
    rx_count: usize,
    rx_buf: [u8; TCP_RXCAP],
    tx_pending: bool,
    tx_len: usize,
    tx_seq: u32,
    tx_buf: [u8; TCP_MSS],
    /// Segmentation backlog (v2.53.0): bytes accepted by [`send`] but
    /// not yet handed to a segment, read from `tx_q_head`.
    tx_q_head: usize,
    tx_q_len: usize,
    tx_q: [u8; TX_QCAP],
    sent_at: u64,
    retries: u8,
    time_at: u64,
    // Broadcast flag (v2.56.0): set when the peer address is 255.255.255.255;
    // marks the socket as a broadcast member that accepts datagrams from any
    // source sent to its bound port.
    broadcast: bool,
    // Congestion control (v2.54.0).
    cwnd: u32,     // congestion window in bytes (>= 1 MSS)
    ssthresh: u32, // slow-start threshold
}

impl Socket {
    const EMPTY: Socket = Socket {
        used: false,
        kind: Kind::Udp,
        pid: 0,
        port: 0,
        rip: [0; 4],
        rport: 0,
        eof: false,
        udp_head: 0,
        udp_len: 0,
        udp_rx: [UdpDgram::EMPTY; UDP_SLOTS],
        state: TcpState::Closed,
        parent: usize::MAX,
        iss: 0,
        snd_una: 0,
        snd_nxt: 0,
        rcv_nxt: 0,
        rx_head: 0,
        rx_count: 0,
        rx_buf: [0; TCP_RXCAP],
        tx_pending: false,
        tx_len: 0,
        tx_seq: 0,
        tx_buf: [0; TCP_MSS],
        tx_q_head: 0,
        tx_q_len: 0,
        tx_q: [0; TX_QCAP],
        sent_at: 0,
        retries: 0,
        time_at: 0,
        broadcast: false,
        cwnd: TCP_MSS as u32,
        ssthresh: 10 * TCP_MSS as u32,
    };
}

static mut SOCKS: [Socket; MAX_SOCKS] = [Socket::EMPTY; MAX_SOCKS];
static mut NEXT_PORT: u16 = 49152;

/// Opens a socket owned by `pid`; returns its descriptor (index) or `None`
/// when the table is full.
pub fn open(pid: u32, kind: Kind) -> Option<usize> {
    #[allow(static_mut_refs)]
    let arr = unsafe { &mut *addr_of_mut!(SOCKS) };
    for (fd, slot) in arr.iter_mut().enumerate() {
        if !slot.used {
            let port = unsafe { alloc_port() };
            *slot = Socket {
                state: TcpState::Closed,
                used: true,
                kind,
                pid,
                port,
                ..Socket::EMPTY
            };
            return Some(fd);
        }
    }
    None
}

unsafe fn alloc_port() -> u16 {
    let p = NEXT_PORT;
    NEXT_PORT = if NEXT_PORT >= 60000 {
        49152
    } else {
        NEXT_PORT + 1
    };
    p
}

#[allow(static_mut_refs)]
unsafe fn get_mut(fd: usize) -> Option<&'static mut Socket> {
    if fd >= MAX_SOCKS {
        return None;
    }
    let arr = &mut *addr_of_mut!(SOCKS);
    let slot = arr.get_mut(fd)?;
    if slot.used {
        Some(slot)
    } else {
        None
    }
}

/// Binds `fd` to a fixed local `port`.
pub fn bind(fd: usize, port: u16) -> bool {
    unsafe {
        match get_mut(fd) {
            Some(sk) => {
                sk.port = port;
                true
            }
            None => false,
        }
    }
}

/// Turns a bound TCP socket into a passive endpoint awaiting inbound SYNs.
/// Only plain, unconnected TCP sockets may listen.
pub fn listen(fd: usize) -> bool {
    unsafe {
        match get_mut(fd) {
            Some(sk) => {
                if sk.kind != Kind::Tcp {
                    return false;
                }
                sk.state = TcpState::Listen;
                sk.eof = false;
                true
            }
            None => false,
        }
    }
}

/// Hands out the next established connection accepted by the listener at
/// `fd`. Returns the child descriptor, or `None` when the socket is not a
/// listener or no handshake has finished yet.
pub fn accept(fd: usize) -> Option<usize> {
    unsafe {
        #[allow(static_mut_refs)]
        let arr = &mut *addr_of_mut!(SOCKS);
        let listener = arr.get(fd)?;
        if !listener.used || listener.kind != Kind::Tcp || listener.state != TcpState::Listen {
            return None;
        }
        let mut found = None;
        for (i, sk) in arr.iter().enumerate() {
            if sk.used
                && sk.kind == Kind::Tcp
                && sk.parent == fd
                && sk.state == TcpState::Established
            {
                found = Some(i);
                break;
            }
        }
        if let Some(i) = found {
            let child = &mut arr[i];
            child.parent = usize::MAX;
        }
        found
    }
}

/// Sets the peer address. For UDP this merely arms [`send`]; for TCP it starts
/// the three-way handshake (SYN) and requires a bound IPv4 address. Connecting
/// a UDP socket to `255.255.255.255` puts it into broadcast mode (v2.56.0):
/// [`send`]/[`deliver_udp`] then use the broadcast fan-out instead of a unicast
/// peer, and the link loops the frame back to every member on the port.
pub fn connect(fd: usize, ip: [u8; 4], port: u16) -> bool {
    let now = net::now_ticks();
    unsafe {
        let sk = match get_mut(fd) {
            Some(s) => s,
            None => return false,
        };
        sk.rip = ip;
        sk.rport = port;
        if sk.kind == Kind::Udp {
            sk.broadcast = ip == [255, 255, 255, 255];
            sk.state = TcpState::Established;
            return true;
        }
        if sk.state == TcpState::Listen {
            return false;
        }
        if net::bound_ip() == [0; 4] {
            return false;
        }
        net::arp_resolve(ip);
        let iss = seed(now, fd);
        sk.iss = iss;
        sk.snd_una = iss;
        sk.snd_nxt = iss.wrapping_add(1);
        sk.retries = 0;
        sk.sent_at = now;
        sk.state = TcpState::SynSent;
        send_segment(sk, iss, SYN, &[]);
        true
    }
}

fn seed(now: u64, fd: usize) -> u32 {
    let x = now
        .wrapping_mul(2_654_435_761)
        .wrapping_add((fd as u64 + 1).wrapping_mul(40_503));
    (x ^ (x >> 17)) as u32
}

/// Current lifecycle code of `fd`, for the ring-3 `SYS_SOCK_STATUS` poll.
pub fn status(fd: usize) -> u8 {
    unsafe {
        match get_mut(fd) {
            Some(sk) => match sk.kind {
                Kind::Udp => 2,
                Kind::Tcp => match sk.state {
                    TcpState::Closed => 4,
                    TcpState::Listen => 6,
                    TcpState::SynSent | TcpState::SynRecv => 1,
                    TcpState::Established => 2,
                    TcpState::CloseWait
                    | TcpState::FinWait1
                    | TcpState::FinWait2
                    | TcpState::LastAck => 3,
                    TcpState::TimeWait => 5,
                },
            },
            None => 4,
        }
    }
}

/// Owning task id of `fd`, or `0` when the descriptor is free.
pub fn owner(fd: usize) -> u32 {
    unsafe {
        if fd >= MAX_SOCKS {
            return 0;
        }
        #[allow(static_mut_refs)]
        let arr = &*core::ptr::addr_of!(SOCKS);
        let slot = &arr[fd];
        if slot.used {
            slot.pid
        } else {
            0
        }
    }
}

/// Sends `data` to the connected peer, returning the byte count accepted (UDP)
/// or transmitted (TCP), or `None` when the socket is busy/unconnected.
pub fn send(fd: usize, data: &[u8]) -> Option<usize> {
    unsafe {
        let sk = get_mut(fd)?;
        match sk.kind {
            Kind::Udp => {
                if sk.rip == [0; 4] {
                    return None;
                }
                let n = data.len().min(UDP_MTU);
                let mut seg = [0u8; 8 + UDP_MTU];
                seg[0..2].copy_from_slice(&sk.port.to_be_bytes());
                seg[2..4].copy_from_slice(&sk.rport.to_be_bytes());
                seg[4..6].copy_from_slice(&((8 + n) as u16).to_be_bytes());
                seg[8..8 + n].copy_from_slice(&data[..n]);
                let c = net::l4_checksum(net::bound_ip(), sk.rip, 17, &seg[..8 + n]);
                seg[6..8].copy_from_slice(&c.to_be_bytes());
                net::emit_ipv4(sk.rip, 17, &seg[..8 + n]).ok()?;
                Some(n)
            }
            Kind::Tcp => {
                if sk.state != TcpState::Established {
                    return None;
                }
                let free = TX_QCAP - sk.tx_q_len;
                if free == 0 {
                    return None;
                }
                let n = data.len().min(free);
                let mut w = (sk.tx_q_head + sk.tx_q_len) % TX_QCAP;
                for b in &data[..n] {
                    sk.tx_q[w] = *b;
                    w = (w + 1) % TX_QCAP;
                }
                sk.tx_q_len += n;
                kick(sk);
                Some(n)
            }
        }
    }
}

/// Receives up to `out.len()` bytes. Returns `Some(n)` with `n > 0` on data,
/// `Some(0)` at end-of-stream (TCP FIN, after queued bytes drain), and `None`
/// when nothing is available yet.
pub fn recv(fd: usize, out: &mut [u8]) -> Option<usize> {
    unsafe {
        let sk = get_mut(fd)?;
        match sk.kind {
            Kind::Udp => {
                if sk.udp_len == 0 {
                    return None;
                }
                let d = &sk.udp_rx[sk.udp_head];
                let n = out.len().min(d.len);
                out[..n].copy_from_slice(&d.data[..n]);
                sk.udp_head = (sk.udp_head + 1) % UDP_SLOTS;
                sk.udp_len -= 1;
                Some(n)
            }
            Kind::Tcp => {
                if sk.rx_count == 0 {
                    return if sk.eof { Some(0) } else { None };
                }
                let n = out.len().min(sk.rx_count);
                for (i, slot) in out.iter_mut().enumerate().take(n) {
                    *slot = sk.rx_buf[(sk.rx_head + i) % TCP_RXCAP];
                }
                sk.rx_head = (sk.rx_head + n) % TCP_RXCAP;
                sk.rx_count -= n;
                Some(n)
            }
        }
    }
}

/// Closes `fd`. TCP sends a FIN and keeps the slot until the peer acknowledges
/// (or a second `close` reclaims it immediately); UDP frees the slot at once.
pub fn close(fd: usize) {
    unsafe {
        let arr = &mut *addr_of_mut!(SOCKS);
        let (is_tcp, state) = match arr.get(fd) {
            Some(s) if s.used => (s.kind == Kind::Tcp, s.state),
            _ => return,
        };
        if !is_tcp {
            arr[fd].used = false;
            return;
        }
        // Closing a listener drops it and every child it handed out.
        if state == TcpState::Listen {
            for child in arr.iter_mut() {
                if child.used && child.parent == fd {
                    child.used = false;
                }
            }
            arr[fd].used = false;
            return;
        }
        let sk = &mut arr[fd];
        match sk.state {
            TcpState::Established => {
                let seq = sk.snd_nxt;
                sk.snd_nxt = sk.snd_nxt.wrapping_add(1);
                sk.state = TcpState::FinWait1;
                send_segment(sk, seq, FIN | ACK, &[]);
                return;
            }
            TcpState::CloseWait => {
                let seq = sk.snd_nxt;
                sk.snd_nxt = sk.snd_nxt.wrapping_add(1);
                sk.state = TcpState::LastAck;
                send_segment(sk, seq, FIN | ACK, &[]);
                return;
            }
            _ => {}
        }
        sk.used = false;
    }
}

/// Hands the next queued segment to the wire (v2.53.0). Does
/// nothing while a segment is still in flight or the backlog
/// is empty; called by [`send`] and after every ACK that
/// completes the in-flight segment.
fn kick(sk: &mut Socket) {
    if sk.tx_pending || sk.tx_q_len == 0 {
        return;
    }
    let n = sk.tx_q_len.min(TCP_MSS);
    let mut r = sk.tx_q_head;
    for i in 0..n {
        sk.tx_buf[i] = sk.tx_q[r];
        r = (r + 1) % TX_QCAP;
    }
    sk.tx_q_head = r;
    sk.tx_q_len -= n;
    sk.tx_len = n;
    sk.tx_seq = sk.snd_nxt;
    sk.snd_nxt = sk.snd_nxt.wrapping_add(n as u32);
    sk.tx_pending = true;
    sk.retries = 0;
    sk.sent_at = net::now_ticks();
    // Congestion control (v2.54.0): additive increase after each segment sent
    sk.cwnd = sk.cwnd.saturating_add(TCP_MSS as u32);
    send_segment(sk, sk.tx_seq, PSH | ACK, &sk.tx_buf[..n]);
}

/// Advances TCP retransmit and time-wait timers; called each tick.
pub fn tick(now: u64) {
    #[allow(static_mut_refs)]
    let arr = unsafe { &mut *addr_of_mut!(SOCKS) };
    for sk in arr.iter_mut() {
        if !sk.used || sk.kind != Kind::Tcp {
            continue;
        }
        match sk.state {
            TcpState::SynSent => {
                if now.wrapping_sub(sk.sent_at) >= REXMIT_TICKS {
                    if sk.retries < REXMIT_MAX {
                        sk.retries += 1;
                        sk.sent_at = now;
                        send_segment(sk, sk.iss, SYN, &[]);
                    } else {
                        sk.state = TcpState::Closed;
                        sk.eof = true;
                    }
                }
            }
            TcpState::SynRecv => {
                if now.wrapping_sub(sk.sent_at) >= REXMIT_TICKS {
                    if sk.retries < REXMIT_MAX {
                        sk.retries += 1;
                        sk.sent_at = now;
                        send_segment(sk, sk.iss, SYN | ACK, &[]);
                    } else {
                        sk.used = false;
                    }
                }
            }
            TcpState::Established => {
                if sk.tx_pending && now.wrapping_sub(sk.sent_at) >= REXMIT_TICKS {
                    if sk.retries < REXMIT_MAX {
                        sk.retries += 1;
                        sk.sent_at = now;
                        let seq = sk.tx_seq;
                        let len = sk.tx_len;
                        send_segment(sk, seq, PSH | ACK, &sk.tx_buf[..len]);
                    } else {
                        // Congestion control (v2.54.0): multiplicative decrease on timeout
                        sk.cwnd = sk.ssthresh / 2;
                        sk.ssthresh = sk.cwnd.max(2 * TCP_MSS as u32);
                        sk.tx_pending = false;
                        sk.state = TcpState::Closed;
                        sk.eof = true;
                    }
                }
            }
            TcpState::TimeWait if now.wrapping_sub(sk.time_at) >= TIME_WAIT_TICKS => {
                sk.used = false;
            }
            _ => {}
        }
    }
}

/// Enqueues an inbound UDP datagram addressed to `dst` (`src`/`sport` identify
/// the sender). Unicast frames (since v2.49.0) reach the single socket bound to
/// `dport` that either has no peer or matches the sender; broadcast
/// (`255.255.255.255`) and multicast (class D) frames (v2.56.0) fan out to every
/// member socket — peers connected to the destination address, broadcast
/// members, and unconnected sockets bound to the port.
pub fn deliver_udp(src: [u8; 4], dst: [u8; 4], sport: u16, dport: u16, payload: &[u8]) {
    if payload.len() > UDP_MTU {
        return;
    }
    let fan = dst == [255, 255, 255, 255] || dst[0] & 0xF0 == 0xE0;
    #[allow(static_mut_refs)]
    let arr = unsafe { &mut *addr_of_mut!(SOCKS) };
    for sk in arr.iter_mut() {
        if !sk.used || sk.kind != Kind::Udp || sk.port != dport {
            continue;
        }
        if fan {
            // Any member accepts broadcast/group frames regardless of sender.
            if !(sk.broadcast || sk.rip == [0; 4] || sk.rip == dst) {
                continue;
            }
        } else if sk.rip != [0; 4] && (sk.rip != src || sk.rport != sport) {
            continue;
        }
        if sk.udp_len >= UDP_SLOTS {
            continue;
        }
        let idx = (sk.udp_head + sk.udp_len) % UDP_SLOTS;
        let d = &mut sk.udp_rx[idx];
        d.len = payload.len();
        d.src_ip = src;
        d.src_port = sport;
        d.data[..payload.len()].copy_from_slice(payload);
        sk.udp_len += 1;
        if !fan {
            return;
        }
    }
}

/// Processes one inbound TCP segment.
pub fn deliver_tcp(src: [u8; 4], seg: &[u8]) {
    let now = net::now_ticks();
    if seg.len() < 20 {
        return;
    }
    let sport = u16::from_be_bytes([seg[0], seg[1]]);
    let dport = u16::from_be_bytes([seg[2], seg[3]]);
    let seq = u32::from_be_bytes([seg[4], seg[5], seg[6], seg[7]]);
    let ack = u32::from_be_bytes([seg[8], seg[9], seg[10], seg[11]]);
    let doff = ((seg[12] >> 4) as usize) * 4;
    let flags = seg[13];
    if doff < 20 || seg.len() < doff {
        return;
    }
    let payload = &seg[doff..];

    #[allow(static_mut_refs)]
    let arr = unsafe { &mut *addr_of_mut!(SOCKS) };

    // Passive open: an inbound SYN for a listener starts a new child. This
    // must run before the established-connection match, because a listener has
    // no peer 5-tuple of its own.
    if flags & SYN != 0 && flags & ACK == 0 {
        for (fd, sk) in arr.iter().enumerate() {
            if sk.used && sk.kind == Kind::Tcp && sk.state == TcpState::Listen && sk.port == dport {
                spawn_child(fd, arr, src, sport, seq);
                return;
            }
        }
    }

    for sk in arr.iter_mut() {
        if !sk.used
            || sk.kind != Kind::Tcp
            || sk.port != dport
            || sk.rport != sport
            || sk.rip != src
        {
            continue;
        }
        if flags & RST != 0 {
            sk.state = TcpState::Closed;
            sk.eof = true;
            sk.tx_pending = false;
            return;
        }
        match sk.state {
            TcpState::SynSent => {
                if flags & SYN != 0 && flags & ACK != 0 && ack == sk.snd_nxt {
                    sk.rcv_nxt = seq.wrapping_add(1);
                    sk.snd_una = ack;
                    sk.snd_nxt = ack;
                    sk.state = TcpState::Established;
                    send_segment(sk, sk.snd_nxt, ACK, &[]);
                }
                return;
            }
            TcpState::SynRecv => {
                // Final ACK completes the server handshake; fall through to
                // the shared data/ACK handling once established.
                if flags & ACK != 0 && ack == sk.snd_nxt {
                    sk.snd_una = ack;
                    sk.state = TcpState::Established;
                } else {
                    return;
                }
            }
            TcpState::Closed => return,
            _ => {}
        }
        if flags & ACK != 0 {
            let seg_end = sk.tx_seq.wrapping_add(sk.tx_len as u32);
            if sk.tx_pending && ack == seg_end {
                sk.tx_pending = false;
                sk.snd_una = ack;
                kick(sk);
            }
            match sk.state {
                TcpState::FinWait1 => sk.state = TcpState::FinWait2,
                TcpState::LastAck => sk.state = TcpState::Closed,
                _ => {}
            }
        }
        if !payload.is_empty() && sk.state == TcpState::Established {
            if seq == sk.rcv_nxt {
                append_rx(sk, payload);
                sk.rcv_nxt = sk.rcv_nxt.wrapping_add(payload.len() as u32);
            }
            send_segment(sk, sk.snd_nxt, ACK, &[]);
        }
        if flags & FIN != 0 {
            sk.rcv_nxt = sk.rcv_nxt.wrapping_add(1);
            sk.eof = true;
            send_segment(sk, sk.snd_nxt, ACK, &[]);
            match sk.state {
                TcpState::Established => sk.state = TcpState::CloseWait,
                TcpState::FinWait2 => {
                    sk.state = TcpState::TimeWait;
                    sk.time_at = now;
                }
                _ => {}
            }
        }
        return;
    }
}

/// Allocates a free slot as a server-side connection for the listener at
/// `listener`, answers the SYN with a SYN-ACK and remembers the client's
/// 5-tuple so its final ACK lands on this child. Drops the SYN when the table
/// is full.
fn spawn_child(listener: usize, arr: &mut [Socket], src: [u8; 4], sport: u16, seq: u32) {
    let (pid, port) = {
        let l = &arr[listener];
        (l.pid, l.port)
    };
    let Some(n) = arr.iter().position(|s| !s.used) else {
        return;
    };
    let now = net::now_ticks();
    let iss = seed(now, n);
    let c = &mut arr[n];
    *c = Socket::EMPTY;
    c.used = true;
    c.kind = Kind::Tcp;
    c.pid = pid;
    c.port = port;
    c.rip = src;
    c.rport = sport;
    c.parent = listener;
    c.iss = iss;
    c.snd_una = iss;
    c.snd_nxt = iss.wrapping_add(1);
    c.rcv_nxt = seq.wrapping_add(1);
    c.state = TcpState::SynRecv;
    c.retries = 0;
    c.sent_at = now;
    net::arp_resolve(src);
    send_segment(c, iss, SYN | ACK, &[]);
}

fn append_rx(sk: &mut Socket, data: &[u8]) -> bool {
    if sk.rx_count + data.len() > TCP_RXCAP {
        return false;
    }
    for &b in data {
        let idx = (sk.rx_head + sk.rx_count) % TCP_RXCAP;
        sk.rx_buf[idx] = b;
        sk.rx_count += 1;
    }
    true
}

fn send_segment(sk: &Socket, seq: u32, flags: u8, payload: &[u8]) {
    let plen = payload.len().min(TCP_MSS);
    let mut seg = [0u8; 20 + TCP_MSS];
    seg[0..2].copy_from_slice(&sk.port.to_be_bytes());
    seg[2..4].copy_from_slice(&sk.rport.to_be_bytes());
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    let ack = if flags & ACK != 0 { sk.rcv_nxt } else { 0 };
    seg[8..12].copy_from_slice(&ack.to_be_bytes());
    seg[12] = (20u8 / 4) << 4;
    seg[13] = flags;
    seg[14..16].copy_from_slice(&TCP_WINDOW.to_be_bytes());
    seg[16..18].copy_from_slice(&0u16.to_be_bytes());
    seg[18..20].copy_from_slice(&0u16.to_be_bytes());
    seg[20..20 + plen].copy_from_slice(&payload[..plen]);
    let total = 20 + plen;
    let c = net::l4_checksum(net::bound_ip(), sk.rip, 6, &seg[..total]);
    seg[16..18].copy_from_slice(&c.to_be_bytes());
    let _ = net::emit_ipv4(sk.rip, 6, &seg[..total]);
}
