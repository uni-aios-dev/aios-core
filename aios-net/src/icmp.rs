use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Default number of echo requests sent by one `ping` call.
pub const DEFAULT_COUNT: u32 = 4;
/// Default per-request reply timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1000);

const PAYLOAD: &[u8] = b"aios-console-icmp-echo-payload!!";
const MAX_COUNT: u32 = 16;

/// One received ICMP echo reply with its measured round-trip time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IcmpReply {
    /// Echo identifier from the reply header.
    pub identifier: u16,
    /// Echo sequence number from the reply header.
    pub sequence: u16,
    /// Round-trip time measured from the send call to the matching reply.
    pub rtt: Duration,
    /// IPv4 time-to-live reported by the platform, or `0` when unavailable.
    pub ttl: u8,
    /// Number of echo payload bytes reported by the platform.
    pub bytes: usize,
}

/// Structured result of one ICMP echo run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingReport {
    /// Host argument as supplied by the caller.
    pub host: String,
    /// IPv4 address the host argument resolved to.
    pub address: IpAddr,
    /// Number of echo requests sent.
    pub transmitted: u32,
    /// Number of matching echo replies received.
    pub received: u32,
    /// Replies in arrival order.
    pub replies: Vec<IcmpReply>,
}

impl PingReport {
    /// Shortest round-trip time, if any reply arrived.
    pub fn min_rtt(&self) -> Option<Duration> {
        self.replies.iter().map(|reply| reply.rtt).min()
    }

    /// Longest round-trip time, if any reply arrived.
    pub fn max_rtt(&self) -> Option<Duration> {
        self.replies.iter().map(|reply| reply.rtt).max()
    }

    /// Arithmetic mean of all round-trip times, if any reply arrived.
    pub fn avg_rtt(&self) -> Option<Duration> {
        if self.replies.is_empty() {
            return None;
        }
        let total: u128 = self.replies.iter().map(|reply| reply.rtt.as_nanos()).sum();
        Some(Duration::from_nanos(
            (total / self.replies.len() as u128) as u64,
        ))
    }

    /// Percentage of echo requests that received no reply.
    pub fn packet_loss_percent(&self) -> f64 {
        (self.transmitted - self.received) as f64 * 100.0 / self.transmitted as f64
    }

    /// Payload size reported by the first reply.
    pub fn payload_bytes(&self) -> usize {
        self.replies.first().map(|reply| reply.bytes).unwrap_or(0)
    }
}

/// Header fields extracted from a validated ICMP echo reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoReplyHeader {
    /// Echo identifier from the reply header.
    pub identifier: u16,
    /// Echo sequence number from the reply header.
    pub sequence: u16,
    /// IPv4 TTL when the datagram carried an IP header.
    pub ttl: u8,
    /// Total length of the ICMP message.
    pub bytes: usize,
}

/// RFC 1071 one's-complement checksum used by ICMPv4.
pub fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in bytes.chunks(2) {
        let word = if pair.len() == 2 {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], 0])
        };
        sum += u32::from(word);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build an ICMPv4 echo request with a correct checksum.
pub fn build_echo_request(identifier: u16, sequence: u16, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(8 + payload.len());
    packet.extend_from_slice(&[8, 0, 0, 0]);
    packet.extend_from_slice(&identifier.to_be_bytes());
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(payload);
    let checksum = internet_checksum(&packet);
    packet[2..4].copy_from_slice(&checksum.to_be_bytes());
    packet
}

/// Validate an echo reply, accepting a bare ICMP message or a full IPv4
/// datagram, and optionally correlate it with the expected identifier.
///
/// Linux unprivileged ICMP sockets have the kernel rewrite the identifier to
/// the local socket port, so callers retry with that alternate identifier.
pub fn parse_echo_reply(
    packet: &[u8],
    expected_identifier: u16,
    expected_sequence: u16,
) -> Option<EchoReplyHeader> {
    let (icmp, ttl) = if packet.first().is_some_and(|byte| byte >> 4 == 4) {
        let header_len = usize::from(packet[0] & 0x0f) * 4;
        if header_len < 20 || packet.len() < header_len + 8 {
            return None;
        }
        (&packet[header_len..], packet[8])
    } else {
        (packet, 0)
    };
    if icmp.len() < 8 || icmp[0] != 0 || icmp[1] != 0 || internet_checksum(icmp) != 0 {
        return None;
    }
    let identifier = u16::from_be_bytes([icmp[4], icmp[5]]);
    let sequence = u16::from_be_bytes([icmp[6], icmp[7]]);
    if identifier != expected_identifier || sequence != expected_sequence {
        return None;
    }
    Some(EchoReplyHeader {
        identifier,
        sequence,
        ttl,
        bytes: icmp.len(),
    })
}

/// Resolve a hostname or literal address to an IPv4 destination.
pub fn resolve_ipv4(host: &str) -> Result<Ipv4Addr, String> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return match address {
            IpAddr::V4(ipv4) => Ok(ipv4),
            IpAddr::V6(_) => Err("ICMPv6 echo is not implemented; use an IPv4 host".to_string()),
        };
    }
    let mut addresses = (host, 0)
        .to_socket_addrs()
        .map_err(|error| format!("resolve '{host}': {error}"))?;
    addresses
        .find_map(|socket| match socket.ip() {
            IpAddr::V4(ipv4) => Some(ipv4),
            IpAddr::V6(_) => None,
        })
        .ok_or_else(|| format!("'{host}' has no IPv4 address; ICMPv6 echo is not implemented"))
}

/// Send `count` real ICMP echo requests and collect matching replies.
///
/// The platform API performs the echo: `IcmpSendEcho` on Windows and an ICMP
/// datagram/raw socket on Unix. A missing privilege or an unusable socket is
/// reported as an error; a silent request without a reply is reported as packet
/// loss. TCP connect and cached link state are never used as substitutes.
pub fn ping(host: &str, count: u32, timeout: Duration) -> Result<PingReport, String> {
    if !(1..=MAX_COUNT).contains(&count) {
        return Err(format!("ping count must be 1..={MAX_COUNT}"));
    }
    if timeout.is_zero() || timeout > Duration::from_secs(60) {
        return Err("ping timeout must be between 1ms and 60s".to_string());
    }
    let address = resolve_ipv4(host)?;
    let identifier = (std::process::id() & 0xffff) as u16;
    let mut replies = Vec::new();
    for sequence in 1..=count as u16 {
        if let Some(reply) = probe_once(address, identifier, sequence, timeout)? {
            replies.push(reply);
        }
    }
    Ok(PingReport {
        host: host.to_string(),
        address: IpAddr::V4(address),
        transmitted: count,
        received: replies.len() as u32,
        replies,
    })
}

#[cfg(windows)]
fn probe_once(
    address: Ipv4Addr,
    identifier: u16,
    sequence: u16,
    timeout: Duration,
) -> Result<Option<IcmpReply>, String> {
    unsafe {
        let handle = IcmpCreateFile();
        if handle as isize == -1 {
            return Err("IcmpCreateFile failed: no ICMP handle available".to_string());
        }
        let packet = build_echo_request(identifier, sequence, PAYLOAD);
        let reply_capacity = std::mem::size_of::<IcmpEchoReply>() + packet.len() + 8;
        let mut aligned = vec![0u64; reply_capacity.div_ceil(8)];
        let started = Instant::now();
        let reply_count = IcmpSendEcho(
            handle,
            u32::from_ne_bytes(address.octets()),
            packet.as_ptr().cast(),
            u16::try_from(packet.len()).unwrap_or(u16::MAX),
            std::ptr::null_mut(),
            aligned.as_mut_ptr().cast(),
            u32::try_from(reply_capacity).unwrap_or(u32::MAX),
            u32::try_from(timeout.as_millis().min(u128::from(u32::MAX))).unwrap_or(u32::MAX),
        );
        IcmpCloseHandle(handle);
        if reply_count == 0 {
            return Ok(None);
        }
        let reply = &*aligned.as_ptr().cast::<IcmpEchoReply>();
        let _ = reply.data;
        if reply.status != 0 {
            return Ok(None);
        }
        let rtt = if reply.round_trip_time == 0 {
            started.elapsed()
        } else {
            Duration::from_millis(u64::from(reply.round_trip_time))
        };
        Ok(Some(IcmpReply {
            identifier,
            sequence,
            rtt,
            ttl: reply.options.ttl,
            bytes: usize::from(reply.data_size),
        }))
    }
}

#[cfg(unix)]
fn probe_once(
    address: Ipv4Addr,
    identifier: u16,
    sequence: u16,
    timeout: Duration,
) -> Result<Option<IcmpReply>, String> {
    let packet = build_echo_request(identifier, sequence, PAYLOAD);
    unsafe {
        let datagram = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP);
        let socket = if datagram >= 0 {
            datagram
        } else {
            libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP)
        };
        if socket < 0 {
            return Err(format!(
                "ICMP socket unavailable (CAP_NET_RAW or unprivileged ICMP sockets required): {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = probe_on_socket(socket, address, identifier, sequence, timeout, &packet);
        libc::close(socket);
        result
    }
}

#[cfg(unix)]
unsafe fn probe_on_socket(
    socket: i32,
    address: Ipv4Addr,
    identifier: u16,
    sequence: u16,
    timeout: Duration,
    packet: &[u8],
) -> Result<Option<IcmpReply>, String> {
    let timeval = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    if libc::setsockopt(
        socket,
        libc::SOL_SOCKET,
        libc::SO_RCVTIMEO,
        &timeval as *const libc::timeval as *const libc::c_void,
        std::mem::size_of::<libc::timeval>() as libc::socklen_t,
    ) < 0
    {
        return Err(format!(
            "ICMP SO_RCVTIMEO failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut destination: libc::sockaddr_in = std::mem::zeroed();
    destination.sin_family = libc::AF_INET as libc::sa_family_t;
    destination.sin_addr.s_addr = u32::from_ne_bytes(address.octets());
    let started = Instant::now();
    if libc::sendto(
        socket,
        packet.as_ptr().cast(),
        packet.len(),
        0,
        &destination as *const libc::sockaddr_in as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
    ) < 0
    {
        return Err(format!(
            "ICMP sendto failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut buffer = [0u8; 2048];
    let mut from: libc::sockaddr_in = std::mem::zeroed();
    let mut from_len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    loop {
        let received = libc::recvfrom(
            socket,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
            &mut from as *mut libc::sockaddr_in as *mut libc::sockaddr,
            &mut from_len,
        );
        if received < 0 {
            let error = std::io::Error::last_os_error();
            return if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) {
                Ok(None)
            } else {
                Err(format!("ICMP recvfrom failed: {error}"))
            };
        }
        let round_trip = started.elapsed();
        let datagram = &buffer[..received as usize];
        let header = parse_echo_reply(datagram, identifier, sequence)
            .or_else(|| parse_echo_reply(datagram, socket as u16, sequence));
        if let Some(header) = header {
            return Ok(Some(IcmpReply {
                identifier: header.identifier,
                sequence: header.sequence,
                rtt: round_trip,
                ttl: header.ttl,
                bytes: header.bytes,
            }));
        }
    }
}

#[cfg(not(any(windows, unix)))]
fn probe_once(
    _address: Ipv4Addr,
    _identifier: u16,
    _sequence: u16,
    _timeout: Duration,
) -> Result<Option<IcmpReply>, String> {
    Err("ICMP echo is not implemented on this platform".to_string())
}

#[cfg(windows)]
#[allow(dead_code)]
#[repr(C)]
struct IpOptionInformation {
    ttl: u8,
    tos: u8,
    flags: u8,
    options_size: u8,
    options_data: *mut u8,
}

#[cfg(windows)]
#[allow(dead_code)]
#[repr(C)]
struct IcmpEchoReply {
    address: u32,
    status: u32,
    round_trip_time: u32,
    data_size: u16,
    reserved: u16,
    data: *mut std::ffi::c_void,
    options: IpOptionInformation,
}

#[cfg(windows)]
#[link(name = "iphlpapi")]
extern "system" {
    fn IcmpCreateFile() -> *mut std::ffi::c_void;
    fn IcmpCloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn IcmpSendEcho(
        handle: *mut std::ffi::c_void,
        destination: u32,
        request_data: *const std::ffi::c_void,
        request_size: u16,
        request_options: *mut IpOptionInformation,
        reply_buffer: *mut std::ffi::c_void,
        reply_size: u32,
        timeout: u32,
    ) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_of_a_complete_echo_request_is_zero() {
        let packet = build_echo_request(0x1234, 7, PAYLOAD);
        assert_eq!(packet.len(), 8 + PAYLOAD.len());
        assert_eq!(packet[0], 8);
        assert_eq!(packet[1], 0);
        assert_eq!(internet_checksum(&packet), 0);
    }

    #[test]
    fn echo_reply_parsing_verifies_checksum_id_and_sequence() {
        let request = build_echo_request(0x1234, 7, PAYLOAD);
        let mut reply = request.clone();
        reply[0] = 0;
        reply[2..4].copy_from_slice(&[0, 0]);
        let checksum = internet_checksum(&reply);
        reply[2..4].copy_from_slice(&checksum.to_be_bytes());

        let header = parse_echo_reply(&reply, 0x1234, 7).unwrap();
        assert_eq!(header.identifier, 0x1234);
        assert_eq!(header.sequence, 7);
        assert_eq!(header.bytes, reply.len());
        assert_eq!(header.ttl, 0);
        assert!(parse_echo_reply(&reply, 0x1234, 8).is_none());
        assert!(parse_echo_reply(&reply, 0x9999, 7).is_none());

        reply[10] ^= 0xff;
        assert!(parse_echo_reply(&reply, 0x1234, 7).is_none());
    }

    #[test]
    fn echo_reply_parsing_accepts_an_ipv4_datagram() {
        let request = build_echo_request(42, 3, PAYLOAD);
        let mut icmp = request;
        icmp[0] = 0;
        icmp[2..4].copy_from_slice(&[0, 0]);
        let checksum = internet_checksum(&icmp);
        icmp[2..4].copy_from_slice(&checksum.to_be_bytes());

        let mut datagram = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64];
        datagram.extend_from_slice(&[0; 11]);
        datagram.extend_from_slice(&icmp);
        let header = parse_echo_reply(&datagram, 42, 3).unwrap();
        assert_eq!(header.ttl, 64);
        assert_eq!(header.bytes, icmp.len());
    }

    #[test]
    fn resolve_ipv4_handles_literals_and_rejects_ipv6() {
        assert_eq!(
            resolve_ipv4("127.0.0.1").unwrap(),
            Ipv4Addr::new(127, 0, 0, 1)
        );
        assert!(resolve_ipv4("::1")
            .unwrap_err()
            .contains("ICMPv6 echo is not implemented"));
    }

    #[test]
    fn ping_validates_arguments_before_any_io() {
        assert!(ping("127.0.0.1", 0, DEFAULT_TIMEOUT).is_err());
        assert!(ping("127.0.0.1", 17, DEFAULT_TIMEOUT).is_err());
        assert!(ping("127.0.0.1", 1, Duration::ZERO).is_err());
    }

    #[test]
    fn ping_loopback_returns_a_report_or_a_privilege_error() {
        match ping("127.0.0.1", 1, Duration::from_millis(500)) {
            Ok(report) => {
                assert_eq!(report.transmitted, 1);
                assert_eq!(report.address, IpAddr::V4(Ipv4Addr::LOCALHOST));
                assert!(report.received <= report.transmitted);
                assert!(report.received as usize == report.replies.len());
                assert!(report.avg_rtt().is_some() == (report.received > 0));
            }
            Err(error) => assert!(error.contains("ICMP"), "unexpected ping error: {error}"),
        }
    }
}
