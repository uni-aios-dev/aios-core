//! Minimal asynchronous DHCP client used by the network engine.
//!
//! The wire format is the classic BOOTP/DHCP packet (RFC 2131). The builder
//! functions are pure and fully unit-tested; [`DhcpClient::acquire`] drives
//! them over a UDP socket bound to the client port 68 and gracefully reports
//! an error when the platform refuses privileged binding.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;

use aios_core::error::{AIOSException, Result};

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const OP_BOOTREPLY: u8 = 2;
const OP_BOOTREQUEST: u8 = 1;

/// Granted DHCP lease parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct DhcpLease {
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub gateway: Option<Ipv4Addr>,
    pub dns_servers: Vec<Ipv4Addr>,
    pub server_id: Option<Ipv4Addr>,
    pub lease_secs: u32,
}

/// Server OFFER subset needed to complete the DORA handshake.
#[derive(Debug, Clone, PartialEq)]
pub struct DhcpOffer {
    pub yiaddr: Ipv4Addr,
    pub server_id: Option<Ipv4Addr>,
}

fn opt_find(packet: &[u8], code: u8) -> Option<Vec<u8>> {
    let mut i = 240;
    while i < packet.len() {
        let c = packet[i];
        if c == 255 {
            break;
        }
        if c == 0 {
            i += 1;
            continue;
        }
        let len = *packet.get(i + 1)? as usize;
        if c == code {
            return Some(packet.get(i + 2..i + 2 + len)?.to_vec());
        }
        i += 2 + len;
    }
    None
}

fn push_opt(out: &mut Vec<u8>, code: u8, data: &[u8]) {
    out.push(code);
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

fn base_packet(
    xid: u32,
    mac: &[u8; 6],
    msg_type: u8,
    server_id: Option<Ipv4Addr>,
    requested: Option<Ipv4Addr>,
) -> Vec<u8> {
    let mut p = vec![0u8; 240];
    p[0] = OP_BOOTREQUEST;
    p[1] = 1; // htype ethernet
    p[2] = 6; // hlen
    p[3..7].copy_from_slice(&xid.to_be_bytes());
    p[236..240].copy_from_slice(&MAGIC_COOKIE);
    p[28..34].copy_from_slice(mac);
    push_opt(&mut p, 53, &[msg_type]);
    if let Some(srv) = server_id {
        push_opt(&mut p, 54, &srv.octets());
    }
    if let Some(ip) = requested {
        push_opt(&mut p, 50, &ip.octets());
    }
    p.push(255);
    p
}

/// Build a DHCPDISCOVER packet.
pub fn build_discover(xid: u32, mac: &[u8; 6]) -> Vec<u8> {
    base_packet(xid, mac, 1, None, None)
}

/// Build a DHCPREQUEST packet selecting an offered address/server.
pub fn build_request(
    xid: u32,
    mac: &[u8; 6],
    server_id: Ipv4Addr,
    requested_ip: Ipv4Addr,
) -> Vec<u8> {
    base_packet(xid, mac, 3, Some(server_id), Some(requested_ip))
}

fn parse_reply(packet: &[u8]) -> Option<(u8, Ipv4Addr)> {
    if packet.len() < 244 || packet[0] != OP_BOOTREPLY {
        return None;
    }
    if packet[236..240] != MAGIC_COOKIE {
        return None;
    }
    let yiaddr = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    let msg_type = *opt_find(packet, 53)?.first()?;
    Some((msg_type, yiaddr))
}

/// Parse a DHCPOFFER reply (message type 2).
pub fn parse_offer(packet: &[u8]) -> Option<DhcpOffer> {
    let (msg_type, yiaddr) = parse_reply(packet)?;
    if msg_type != 2 {
        return None;
    }
    let server_id = opt_find(packet, 54)
        .and_then(|v| <[u8; 4]>::try_from(v.as_slice()).ok().map(Ipv4Addr::from));
    Some(DhcpOffer { yiaddr, server_id })
}

/// Parse a DHCPACK reply (message type 5) into a full lease.
pub fn parse_ack(packet: &[u8]) -> Option<DhcpLease> {
    let (msg_type, yiaddr) = parse_reply(packet)?;
    if msg_type != 5 {
        return None;
    }
    let read4 = |code: u8| -> Option<Ipv4Addr> {
        opt_find(packet, code)
            .and_then(|v| <[u8; 4]>::try_from(v.as_slice()).ok())
            .map(Ipv4Addr::from)
    };
    let dns_servers = opt_find(packet, 6)
        .map(|v| {
            v.chunks_exact(4)
                .filter_map(|c| <[u8; 4]>::try_from(c).ok().map(Ipv4Addr::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let lease_secs = opt_find(packet, 51)
        .and_then(|v| <[u8; 4]>::try_from(v.as_slice()).ok())
        .map(u32::from_be_bytes)
        .unwrap_or(0);
    Some(DhcpLease {
        ip: yiaddr,
        netmask: read4(1).unwrap_or(Ipv4Addr::new(255, 255, 255, 0)),
        gateway: read4(3),
        dns_servers,
        server_id: read4(54),
        lease_secs,
    })
}

/// Random locally-administered MAC used as the client hardware address.
pub fn random_mac() -> [u8; 6] {
    let mut m = [0u8; 6];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut m);
    m[0] = 0x02;
    m
}

/// UDP-driven DHCP client performing DISCOVER→OFFER→REQUEST→ACK.
#[derive(Debug)]
pub struct DhcpClient {
    mac: [u8; 6],
    broadcast: SocketAddr,
}

impl Default for DhcpClient {
    fn default() -> Self {
        Self {
            mac: random_mac(),
            broadcast: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::BROADCAST), SERVER_PORT),
        }
    }
}

impl DhcpClient {
    /// Client bound to a specific MAC (deterministic tests).
    pub fn with_mac(mac: [u8; 6]) -> Self {
        Self {
            mac,
            ..Self::default()
        }
    }

    /// Run the full DORA exchange with the given per-step timeout.
    ///
    /// Returns [`AIOSException::Timeout`] when no server answers and maps
    /// privileged-binding failures to [`AIOSException::PermissionDenied`].
    pub async fn acquire(&self, timeout: Duration) -> Result<DhcpLease> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, CLIENT_PORT))
            .await
            .map_err(|e| AIOSException::PermissionDenied(format!("dhcp client bind: {e}")))?;
        let mut xid: u32 = rand::random();
        let discover = build_discover(xid, &self.mac);
        sock.send_to(&discover, self.broadcast)
            .await
            .map_err(|e| AIOSException::IPCError(format!("dhcp send: {e}")))?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(AIOSException::Timeout("dhcp: no offer".into()));
            }
            let mut buf = [0u8; 1500];
            let (n, _) = match tokio::time::timeout(remaining, sock.recv_from(&mut buf)).await {
                Err(_) => return Err(AIOSException::Timeout("dhcp: no offer".into())),
                Ok(Err(e)) => return Err(AIOSException::IPCError(format!("dhcp recv: {e}"))),
                Ok(Ok(r)) => r,
            };
            if n > 1500 {
                continue;
            }
            let Some(offer) = parse_offer(&buf[..n]) else {
                continue;
            };
            let Some(server_id) = offer.server_id else {
                continue;
            };
            xid = xid.wrapping_add(1);
            let request = build_request(xid, &self.mac, server_id, offer.yiaddr);
            sock.send_to(&request, self.broadcast)
                .await
                .map_err(|e| AIOSException::IPCError(format!("dhcp send: {e}")))?;
            // Wait for ACK with the same deadline semantics.
            let (n2, _) = match tokio::time::timeout(
                remaining.min(timeout),
                sock.recv_from(&mut buf),
            )
            .await
            {
                Err(_) | Ok(Err(_)) => return Err(AIOSException::Timeout("dhcp: no ack".into())),
                Ok(Ok(r)) => r,
            };
            if let Some(lease) = parse_ack(&buf[..n2]) {
                return Ok(lease);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];

    #[test]
    fn discover_contains_cookie_and_discover_type() {
        let p = build_discover(0xDEADBEEF, &MAC);
        assert_eq!(p[0], 1);
        assert_eq!(&p[3..7], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(&p[236..240], &MAGIC_COOKIE);
        assert_eq!(p[28..34], MAC);
        assert!(opt_find(&p, 53).unwrap() == vec![1]);
    }

    #[test]
    fn request_carries_server_id_and_requested_ip() {
        let srv = Ipv4Addr::new(192, 168, 1, 1);
        let ip = Ipv4Addr::new(192, 168, 1, 42);
        let p = build_request(7, &MAC, srv, ip);
        assert_eq!(opt_find(&p, 53).unwrap(), vec![3]);
        assert_eq!(opt_find(&p, 54).unwrap(), srv.octets().to_vec());
        assert_eq!(opt_find(&p, 50).unwrap(), ip.octets().to_vec());
    }

    fn craft(msg_type: u8, yiaddr: Ipv4Addr) -> Vec<u8> {
        let mut p = vec![0u8; 240];
        p[0] = 2;
        p[16..20].copy_from_slice(yiaddr.octets().as_slice());
        p[236..240].copy_from_slice(&MAGIC_COOKIE);
        push_opt(&mut p, 53, &[msg_type]);
        push_opt(&mut p, 54, &[192, 168, 1, 1]);
        p
    }

    #[test]
    fn offer_parser_accepts_type2_and_rejects_others() {
        let o = parse_offer(&craft(2, Ipv4Addr::new(10, 0, 0, 5))).unwrap();
        assert_eq!(o.yiaddr, Ipv4Addr::new(10, 0, 0, 5));
        assert_eq!(o.server_id, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(parse_offer(&craft(5, Ipv4Addr::new(10, 0, 0, 5))).is_none());
        assert!(parse_offer(b"garbage").is_none());
    }

    #[test]
    fn ack_parser_extracts_full_lease_options() {
        let mut ack = craft(5, Ipv4Addr::new(10, 0, 0, 9));
        push_opt(&mut ack, 1, &[255, 255, 255, 0]);
        push_opt(&mut ack, 3, &[10, 0, 0, 1]);
        push_opt(&mut ack, 6, &[8, 8, 8, 8, 1, 1, 1, 1]);
        push_opt(&mut ack, 51, &3600u32.to_be_bytes());
        let lease = parse_ack(&ack).unwrap();
        assert_eq!(lease.ip, Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(lease.netmask, Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(lease.gateway, Some(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(
            lease.dns_servers,
            vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(1, 1, 1, 1)]
        );
        assert_eq!(lease.lease_secs, 3600);
        assert_eq!(lease.server_id, Some(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn pad_option_is_skipped_by_scanner() {
        let mut ack = craft(5, Ipv4Addr::new(10, 0, 0, 9));
        ack.push(0); // PAD between options must not desynchronize the scanner
        push_opt(&mut ack, 3, &[10, 0, 0, 254]);
        let lease = parse_ack(&ack).unwrap();
        assert_eq!(lease.gateway, Some(Ipv4Addr::new(10, 0, 0, 254)));
    }

    #[test]
    fn random_mac_is_locally_administered() {
        let m = random_mac();
        assert_eq!(m[0] & 0b0000_0011, 0x02);
    }

    #[test]
    fn default_client_targets_port67_broadcast() {
        let c = DhcpClient::default();
        assert_eq!(c.broadcast.port(), SERVER_PORT);
        assert!(matches!(c.broadcast.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast()));
    }
}
