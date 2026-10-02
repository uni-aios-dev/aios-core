//! IPv4 client stack over the polled [`nic`] driver (v2.38.34): DHCP with
//! persistent settings in CMOS, ARP resolution, ICMP link probing, a DNS
//! reachability check and the GUI-facing network state machine.
//!
//! The stack is deliberately small and entirely tick driven — [`poll`] runs
//! from the idle loop every 10 ms tick, drains at most a bounded number of
//! frames and advances one step of the state machine:
//!
//! ```text
//! Idle → Discover → Request → Bound ⇄ (CheckArp → CheckIcmp → CheckDns)
//! ```
//!
//! Uplink selection prefers the Wi-Fi association when present (the
//! [crate::wifi] test radio terminates on its own `10.0.9.0/24` LAN with a
//! DHCP/ICMP/DNS responder), otherwise the wired NIC (QEMU user-mode SLIRP
//! or a real DHCP server).
//!
//! Settings persist in the CMOS user bank `0x40..0x7B` behind a magic,
//! version and CRC16-CCITT header. The WPA passphrase is intentionally kept
//! out of CMOS (RAM only) and is owned by [crate::wifi].

use crate::interrupts::{TICKS, TIMER_HZ};
use crate::nic::{self, Nic};
use crate::pci::{PciDevice, CLASS_NETWORK};
use crate::port;
use crate::wifi;
use core::ptr::{addr_of, addr_of_mut};

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const PROTO_ICMP: u8 = 1;
const PROTO_UDP: u8 = 17;
const ICMP_ECHO: u8 = 8;
const ICMP_REPLY: u8 = 0;
const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];
const DHCP_OFFER: u8 = 2;
const DHCP_ACK: u8 = 5;
const DHCP_DISCOVER: u8 = 1;
const DHCP_REQUEST: u8 = 3;
const DNS_PORT: u16 = 53;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;
const DNS_SRC_PORT: u16 = 40_000;
const ICMP_PROBE_ID: u16 = 0xA105;
const BROADCAST_IP: [u8; 4] = [255, 255, 255, 255];
const BROADCAST_MAC: [u8; 6] = [0xFF; 6];
const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;
const CMOS_MAGIC: u8 = 0xA7;
const CMOS_VER: u8 = 1;
const CMOS_START: u8 = 0x40;
const CMOS_PAYLOAD: usize = 55;
const ARP_SLOTS: usize = 8;
const MAX_DRAIN: usize = 16;
const RECHECK_TICKS: u64 = 30 * TIMER_HZ;
const FAIL_RECHECK_TICKS: u64 = 5 * TIMER_HZ;

/// High-level link state published to the GUI tray globe.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No wired NIC and no Wi-Fi association.
    NoNic,
    /// A NIC is present but the cable/link is down.
    NoLink,
    /// Link is up but no IPv4 address is configured yet.
    LinkOnly,
    /// Address configured, internet reachability not yet confirmed.
    Local,
    /// A DHCP exchange or reachability probe is in flight (tray pulses).
    Checking,
    /// ICMP + DNS probes succeeded; the uplink reaches the internet.
    Internet,
}

impl State {
    /// Stable lowercase name used in serial logs and the GUI tooltip.
    pub fn name(self) -> &'static str {
        match self {
            State::NoNic => "no-nic",
            State::NoLink => "no-link",
            State::LinkOnly => "link-only",
            State::Local => "local",
            State::Checking => "checking",
            State::Internet => "internet",
        }
    }
}

/// Applied network configuration (persisted to CMOS, never the passphrase).
#[derive(Clone, Copy)]
pub struct Settings {
    /// `true`: obtain address via DHCP; `false`: use the static fields.
    pub dhcp: bool,
    /// Static IPv4 address (ignored when [`dhcp`](Self::dhcp)).
    pub ip: [u8; 4],
    /// Subnet mask (four bytes, e.g. `255.255.255.0`).
    pub mask: [u8; 4],
    /// Static default gateway (empty for local-only links).
    pub gw: [u8; 4],
    /// Primary DNS server.
    pub dns1: [u8; 4],
    /// Secondary DNS server.
    pub dns2: [u8; 4],
}

impl Settings {
    /// DHCP-enabled default with empty static fields.
    pub const DEFAULT: Settings = Settings {
        dhcp: true,
        ip: [0; 4],
        mask: [0; 4],
        gw: [0; 4],
        dns1: [0; 4],
        dns2: [0; 4],
    };
}

/// An IPv4 address block: either DHCP-learned or applied static settings.
#[derive(Clone, Copy, Default)]
pub struct Lease {
    /// Assigned IPv4 address.
    pub ip: [u8; 4],
    /// Subnet mask.
    pub mask: [u8; 4],
    /// Default gateway.
    pub gw: [u8; 4],
    /// Primary DNS.
    pub dns1: [u8; 4],
    /// Secondary DNS.
    pub dns2: [u8; 4],
    /// DHCP server identity (renewals target this address).
    pub server: [u8; 4],
    /// Lease time in seconds (0 = infinite/static).
    pub secs: u32,
}

impl Lease {
    /// Empty lease used before the first binding.
    pub const EMPTY: Lease = Lease {
        ip: [0; 4],
        mask: [0; 4],
        gw: [0; 4],
        dns1: [0; 4],
        dns2: [0; 4],
        server: [0; 4],
        secs: 0,
    };

    /// `true` when an IPv4 address is bound.
    pub fn bound(&self) -> bool {
        self.ip != [0; 4]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Discover,
    Request,
    Renew,
    CheckArp,
    CheckIcmp,
    CheckDns,
    Bound,
}

static mut NIC: Option<Nic> = None;
static mut SETTINGS: Settings = Settings::DEFAULT;
static mut PHASE: Phase = Phase::Idle;
static mut STATE: State = State::NoNic;
static mut OFFER: Lease = Lease::EMPTY;
static mut BOUND: Lease = Lease::EMPTY;
static mut XID: u32 = 0;
static mut DNS_XID: u16 = 0;
static mut SEQ: u16 = 0;
static mut RETRIES: u8 = 0;
static mut TRIES: u8 = 0;
static mut DEADLINE: u64 = 0;
static mut NEXT_ACTION: u64 = 0;
static mut BOUND_AT: u64 = 0;
static mut GOT_OFFER: bool = false;
static mut GOT_ACK: bool = false;
static mut GOT_ARP: bool = false;
static mut GOT_ICMP: bool = false;
static mut GOT_DNS: bool = false;
static mut NET_OK: bool = false;
static mut ARP_TABLE: [([u8; 4], [u8; 6]); ARP_SLOTS] = [([0; 4], [0; 6]); ARP_SLOTS];
static mut ARP_COUNT: usize = 0;

fn ticks() -> u64 {
    TICKS.load(core::sync::atomic::Ordering::Relaxed)
}

fn read_settings() -> Settings {
    unsafe { *addr_of!(SETTINGS) }
}

fn read_bound() -> Lease {
    unsafe { *addr_of!(BOUND) }
}

fn set_dns_xid(v: u16) {
    unsafe { *addr_of_mut!(DNS_XID) = v }
}

fn next_seq() -> u16 {
    unsafe {
        SEQ = SEQ.wrapping_add(1);
        SEQ
    }
}

/// Probes the PCI bus for a class-`0x02` controller, initialises the driver
/// and loads persisted settings from CMOS. Safe to call when no NIC exists —
/// the Wi-Fi uplink then carries the stack.
pub fn init(devices: &[PciDevice]) -> Result<(), &'static str> {
    cmos_load();
    let dev = devices.iter().find(|d| d.class == CLASS_NETWORK);
    if let Some(dev) = dev {
        let nic = unsafe { Nic::init(dev)? };
        let mac = *nic.mac();
        let kind = nic.kind_name();
        unsafe { NIC = Some(nic) };
        crate::kprintln!(
            "[serial] [net] nic {} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            kind,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );
        crate::vprintln!(
            "NET: {} ({})",
            kind,
            if read_settings().dhcp {
                "DHCP"
            } else {
                "static"
            }
        );
    } else {
        crate::kprintln!("[serial] [net] nic: none (wifi uplink only)");
        crate::vprintln!("NET: no wired NIC (wifi uplink)");
    }
    Ok(())
}

/// Advances the stack one 10 ms tick: drains inbound frames, then runs the
/// current state-machine step and republishes [`State`] for the GUI.
pub fn poll() {
    let now = ticks();
    unsafe {
        drain_rx();
        match PHASE {
            Phase::Idle => step_idle(now),
            Phase::Discover | Phase::Request | Phase::Renew => step_dhcp(now),
            Phase::CheckArp => step_arp(now),
            Phase::CheckIcmp => step_icmp(now),
            Phase::CheckDns => step_dns(now),
            Phase::Bound => step_bound(now),
        }
        publish(compute_state());
    }
}

/// Current tray/window state.
pub fn state() -> State {
    unsafe { *addr_of!(STATE) }
}

/// Snapshot of the applied settings (for GUI drafts and status lines).
pub fn settings() -> Settings {
    read_settings()
}

/// Snapshot of the bound lease, or `None` when no address is configured.
pub fn lease() -> Option<Lease> {
    let l = read_bound();
    if l.bound() {
        Some(l)
    } else {
        None
    }
}

/// Wired NIC MAC, when a NIC was found.
pub fn nic_mac() -> Option<[u8; 6]> {
    unsafe {
        let p = addr_of!(NIC);
        if (*p).is_some() {
            Some(*(*p).as_ref().unwrap().mac())
        } else {
            None
        }
    }
}

/// Wired NIC backend name, or `"—"` when absent.
pub fn nic_kind() -> &'static str {
    unsafe {
        let p = addr_of!(NIC);
        if (*p).is_some() {
            (*p).as_ref().unwrap().kind_name()
        } else {
            "—"
        }
    }
}

/// Applies new settings, persists them to CMOS and restarts the address
/// flow on the next tick.
pub fn apply(s: &Settings) {
    unsafe {
        *addr_of_mut!(SETTINGS) = *s;
        *addr_of_mut!(BOUND) = Lease::EMPTY;
        *addr_of_mut!(PHASE) = Phase::Idle;
        RETRIES = 0;
        NEXT_ACTION = ticks();
        NET_OK = false;
    }
    cmos_save();
    crate::kprintln!(
        "[serial] [net] config applied dhcp={} ip={}.{}.{}.{}/{}",
        s.dhcp as u8,
        s.ip[0],
        s.ip[1],
        s.ip[2],
        s.ip[3],
        mask_bits(s.mask)
    );
}

/// Forces an immediate reachability re-probe (the Network window's Test
/// button): clears the cached probe verdict and, when an address is bound
/// and the link is up, restarts the ARP -> ICMP -> DNS chain right away
/// instead of waiting for the periodic [`Phase::Bound`] recheck.
pub fn recheck() {
    unsafe {
        NET_OK = false;
        GOT_ARP = false;
        GOT_ICMP = false;
        GOT_DNS = false;
        if *addr_of!(PHASE) == Phase::Bound && has_ip() && link_ok() {
            start_check(ticks());
        }
    }
    crate::kprintln!("[serial] [net] manual check requested");
}

/// Formats an IPv4 address into `dst` (`"10.0.2.15"`), returning its length.
pub fn fmt_ip(ip: [u8; 4], dst: &mut [u8]) -> usize {
    let mut o = 0;
    for (i, b) in ip.iter().enumerate() {
        if i > 0 && o < dst.len() {
            dst[o] = b'.';
            o += 1;
        }
        o += fmt_u8(*b, &mut dst[o..]);
    }
    o
}

/// Formats a MAC address into `dst` (`"52:54:00:12:34:56"`), returning its
/// length.
pub fn fmt_mac(mac: [u8; 6], dst: &mut [u8]) -> usize {
    let mut o = 0;
    for (i, b) in mac.iter().enumerate() {
        if i > 0 && o + 1 < dst.len() {
            dst[o] = b':';
            o += 1;
        }
        let hi = b >> 4;
        let lo = b & 0xF;
        if o < dst.len() {
            dst[o] = hex(hi);
            o += 1;
        }
        if o < dst.len() {
            dst[o] = hex(lo);
            o += 1;
        }
    }
    o
}

fn hex(v: u8) -> u8 {
    if v < 10 {
        b'0' + v
    } else {
        b'a' + (v - 10)
    }
}

fn fmt_u8(v: u8, dst: &mut [u8]) -> usize {
    let mut o = 0;
    if v >= 100 && o + 3 <= dst.len() {
        dst[o] = b'0' + v / 100;
        o += 1;
        dst[o] = b'0' + (v / 10) % 10;
        o += 1;
        dst[o] = b'0' + v % 10;
        o += 1;
    } else if v >= 10 && o + 2 <= dst.len() {
        dst[o] = b'0' + v / 10;
        o += 1;
        dst[o] = b'0' + v % 10;
        o += 1;
    } else if o < dst.len() {
        dst[o] = b'0' + v;
        o += 1;
    }
    o
}

fn mask_bits(mask: [u8; 4]) -> u8 {
    u32::from_be_bytes(mask).count_ones() as u8
}

fn has_ip() -> bool {
    read_bound().bound()
}

fn link_ok() -> bool {
    if wifi::associated() {
        return true;
    }
    unsafe {
        if let Some(nic) = nic_ref() {
            nic.link_up()
        } else {
            false
        }
    }
}

unsafe fn nic_ref() -> Option<&'static Nic> {
    let p = addr_of!(NIC);
    if (*p).is_some() {
        Some((*p).as_ref().unwrap())
    } else {
        None
    }
}

unsafe fn nic_mut() -> Option<&'static mut Nic> {
    let p = addr_of_mut!(NIC);
    if (*p).is_some() {
        Some((*p).as_mut().unwrap())
    } else {
        None
    }
}

fn uplink_mac() -> [u8; 6] {
    if wifi::associated() {
        wifi::mac()
    } else {
        nic_mac().unwrap_or_default()
    }
}

fn uplink_send(frame: &[u8]) -> Result<(), &'static str> {
    if wifi::associated() {
        wifi::send_frame(frame)
    } else {
        unsafe {
            if let Some(nic) = nic_mut() {
                nic.send(frame)
            } else {
                Err("net: no uplink")
            }
        }
    }
}

fn current_ip() -> [u8; 4] {
    read_bound().ip
}

unsafe fn drain_rx() {
    let mut buf = [0u8; nic::FRAME_MAX];
    for _ in 0..MAX_DRAIN {
        let len = if wifi::has_frame() {
            match wifi::poll_frame(&mut buf) {
                Some(n) => n,
                None => break,
            }
        } else if let Some(nic) = nic_mut() {
            match nic.poll_rx(&mut buf) {
                Some(n) => n,
                None => break,
            }
        } else {
            break;
        };
        handle_frame(&buf[..len]);
    }
}

unsafe fn handle_frame(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    match ethertype {
        ETHERTYPE_ARP => handle_arp(frame),
        ETHERTYPE_IPV4 => handle_ipv4(frame),
        _ => {}
    }
}

unsafe fn handle_arp(frame: &[u8]) {
    if frame.len() < 42 {
        return;
    }
    let op = u16::from_be_bytes([frame[20], frame[21]]);
    let spa = [frame[28], frame[29], frame[30], frame[31]];
    let tpa = [frame[38], frame[39], frame[40], frame[41]];
    match op {
        1 => {
            let ours = (*addr_of!(BOUND)).ip;
            if ours != [0; 4] && tpa == ours {
                let mut reply = [0u8; 42];
                reply[0..6].copy_from_slice(&frame[22..28]);
                reply[6..12].copy_from_slice(&uplink_mac());
                reply[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());
                reply[14..16].copy_from_slice(&1u16.to_be_bytes());
                reply[16..18].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
                reply[18] = 6;
                reply[19] = 4;
                reply[20..22].copy_from_slice(&2u16.to_be_bytes());
                reply[22..28].copy_from_slice(&uplink_mac());
                reply[28..32].copy_from_slice(&ours);
                reply[32..38].copy_from_slice(&frame[22..28]);
                reply[38..42].copy_from_slice(&spa);
                let _ = uplink_send(&reply);
            }
        }
        2 => {
            let sha = [
                frame[22], frame[23], frame[24], frame[25], frame[26], frame[27],
            ];
            arp_insert(spa, sha);
            if spa == (*addr_of!(BOUND)).gw {
                GOT_ARP = true;
            }
        }
        _ => {}
    }
}

unsafe fn handle_ipv4(frame: &[u8]) {
    if frame.len() < 34 || frame[14] >> 4 != 4 {
        return;
    }
    let ihl = ((frame[14] & 0xF) * 4) as usize;
    if ihl < 20 || frame.len() < 14 + ihl {
        return;
    }
    let proto = frame[23];
    let dst = [frame[30], frame[31], frame[32], frame[33]];
    let l4 = 14 + ihl;
    match proto {
        PROTO_ICMP => {
            if frame.len() < l4 + 8 {
                return;
            }
            if frame[l4] == ICMP_REPLY {
                let id = u16::from_be_bytes([frame[l4 + 4], frame[l4 + 5]]);
                let ours = (*addr_of!(BOUND)).ip;
                if id == ICMP_PROBE_ID && (dst == ours || dst == BROADCAST_IP) {
                    GOT_ICMP = true;
                }
            }
        }
        PROTO_UDP => handle_udp(frame, l4),
        _ => {}
    }
}

unsafe fn handle_udp(frame: &[u8], l4: usize) {
    if frame.len() < l4 + 8 {
        return;
    }
    let sport = u16::from_be_bytes([frame[l4], frame[l4 + 1]]);
    let dport = u16::from_be_bytes([frame[l4 + 2], frame[l4 + 3]]);
    let ulen = u16::from_be_bytes([frame[l4 + 4], frame[l4 + 5]]) as usize;
    if ulen < 8 {
        return;
    }
    let data = l4 + 8;
    let payload_len = ulen - 8;
    if frame.len() < data + payload_len {
        return;
    }
    let payload = &frame[data..data + payload_len];
    if sport == DHCP_SERVER_PORT && dport == DHCP_CLIENT_PORT {
        handle_dhcp(payload);
    } else if sport == DNS_PORT && dport == DNS_SRC_PORT {
        handle_dns(payload);
    }
}

unsafe fn handle_dhcp(pkt: &[u8]) {
    if pkt.len() < 240 || pkt[0] != 2 {
        return;
    }
    let xid = u32::from_le_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
    if xid != *addr_of!(XID) {
        return;
    }
    match dhcp_option(pkt, 53) {
        Some(DHCP_OFFER) => {
            let yiaddr = [pkt[16], pkt[17], pkt[18], pkt[19]];
            if yiaddr == [0; 4] {
                return;
            }
            let mut l = Lease {
                ip: yiaddr,
                mask: [255, 255, 255, 0],
                ..Lease::EMPTY
            };
            l.server = dhcp_ip_option(pkt, 54).unwrap_or([pkt[20], pkt[21], pkt[22], pkt[23]]);
            parse_lease_options(pkt, &mut l);
            *addr_of_mut!(OFFER) = l;
            GOT_OFFER = true;
        }
        Some(DHCP_ACK) => {
            let mut l = *addr_of!(OFFER);
            if l.ip == [0; 4] {
                l.ip = [pkt[16], pkt[17], pkt[18], pkt[19]];
            }
            parse_lease_options(pkt, &mut l);
            *addr_of_mut!(OFFER) = l;
            GOT_ACK = true;
        }
        _ => {}
    }
}

fn parse_lease_options(pkt: &[u8], l: &mut Lease) {
    if let Some(ip) = dhcp_ip_option(pkt, 1) {
        l.mask = ip;
    }
    if let Some(ip) = dhcp_ip_option(pkt, 3) {
        l.gw = ip;
    }
    if let Some(lease) = dhcp_u32_option(pkt, 51) {
        l.secs = lease;
    }
    if let Some(dns) = dhcp_dns_option(pkt) {
        l.dns1 = dns.0;
        l.dns2 = dns.1;
    } else if l.dns1 == [0; 4] {
        l.dns1 = l.server;
    }
}

unsafe fn handle_dns(pkt: &[u8]) {
    if pkt.len() < 12 {
        return;
    }
    let id = u16::from_be_bytes([pkt[0], pkt[1]]);
    if id != *addr_of!(DNS_XID) {
        return;
    }
    let flags = u16::from_be_bytes([pkt[2], pkt[3]]);
    let rcode = flags & 0xF;
    let ancount = u16::from_be_bytes([pkt[6], pkt[7]]);
    if rcode == 0 && ancount > 0 {
        GOT_DNS = true;
    }
}

fn walk_options(pkt: &[u8], want: u8, out: &mut [u8]) -> Option<usize> {
    let mut i = 240;
    while i + 1 < pkt.len() {
        let tag = pkt[i];
        if tag == 0xFF {
            return None;
        }
        if tag == 0 {
            i += 1;
            continue;
        }
        let len = pkt[i + 1] as usize;
        if i + 2 + len > pkt.len() {
            return None;
        }
        if tag == want {
            let n = len.min(out.len());
            out[..n].copy_from_slice(&pkt[i + 2..i + 2 + n]);
            return Some(n);
        }
        i += 2 + len;
    }
    None
}

fn dhcp_option(pkt: &[u8], code: u8) -> Option<u8> {
    let mut b = [0u8; 1];
    walk_options(pkt, code, &mut b).map(|_| b[0])
}

fn dhcp_ip_option(pkt: &[u8], code: u8) -> Option<[u8; 4]> {
    let mut b = [0u8; 4];
    let n = walk_options(pkt, code, &mut b)?;
    if n >= 4 {
        Some(b)
    } else {
        None
    }
}

fn dhcp_u32_option(pkt: &[u8], code: u8) -> Option<u32> {
    dhcp_ip_option(pkt, code).map(u32::from_be_bytes)
}

fn dhcp_dns_option(pkt: &[u8]) -> Option<([u8; 4], [u8; 4])> {
    let mut b = [0u8; 8];
    let n = walk_options(pkt, 6, &mut b)?;
    if n >= 4 {
        let second = if n >= 8 {
            [b[4], b[5], b[6], b[7]]
        } else {
            [b[0], b[1], b[2], b[3]]
        };
        Some(([b[0], b[1], b[2], b[3]], second))
    } else {
        None
    }
}

fn arp_lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    unsafe {
        let table = addr_of!(ARP_TABLE);
        for i in 0..*addr_of!(ARP_COUNT) {
            if (*table)[i].0 == ip {
                return Some((*table)[i].1);
            }
        }
        None
    }
}

fn arp_insert(ip: [u8; 4], mac: [u8; 6]) {
    if ip == [0; 4] {
        return;
    }
    unsafe {
        let table = addr_of_mut!(ARP_TABLE);
        let count = *addr_of!(ARP_COUNT);
        for i in 0..count {
            if (*table)[i].0 == ip {
                (*table)[i].1 = mac;
                return;
            }
        }
        if count < ARP_SLOTS {
            (*table)[count].0 = ip;
            (*table)[count].1 = mac;
            *addr_of_mut!(ARP_COUNT) = count + 1;
        }
    }
}

fn reset_arp() {
    unsafe { *addr_of_mut!(ARP_COUNT) = 0 }
}

unsafe fn publish(s: State) {
    if *addr_of!(STATE) != s {
        *addr_of_mut!(STATE) = s;
        crate::kprintln!("[serial] [net] state {}", s.name());
    }
}

unsafe fn compute_state() -> State {
    if wifi::connecting() || wifi::scanning() {
        return State::Checking;
    }
    if !link_ok() {
        return if nic_ref().is_some() {
            State::NoLink
        } else {
            State::NoNic
        };
    }
    match *addr_of!(PHASE) {
        Phase::Discover
        | Phase::Request
        | Phase::Renew
        | Phase::CheckArp
        | Phase::CheckIcmp
        | Phase::CheckDns => State::Checking,
        Phase::Idle => {
            if has_ip() {
                if *addr_of!(NET_OK) {
                    State::Internet
                } else {
                    State::Local
                }
            } else if !(*addr_of!(SETTINGS)).dhcp || *addr_of!(RETRIES) > 5 {
                State::LinkOnly
            } else {
                State::Checking
            }
        }
        Phase::Bound => {
            if !has_ip() {
                State::LinkOnly
            } else if *addr_of!(NET_OK) {
                State::Internet
            } else {
                State::Local
            }
        }
    }
}

unsafe fn step_idle(now: u64) {
    if !link_ok() || now < *addr_of!(NEXT_ACTION) {
        return;
    }
    if (*addr_of!(SETTINGS)).dhcp {
        XID = (now ^ 0xA1_05_00) as u32;
        RETRIES = 0;
        send_discover(now, 100);
    } else {
        let s = *addr_of!(SETTINGS);
        if s.ip == [0; 4] {
            return;
        }
        *addr_of_mut!(BOUND) = Lease {
            ip: s.ip,
            mask: s.mask,
            gw: s.gw,
            dns1: s.dns1,
            dns2: s.dns2,
            server: [0; 4],
            secs: 0,
        };
        NET_OK = false;
        PHASE = Phase::Bound;
        BOUND_AT = now;
        NEXT_ACTION = now + 10;
        RETRIES = 0;
        crate::kprintln!(
            "[serial] [net] static ip={}.{}.{}.{}/{} gw={}.{}.{}.{}",
            s.ip[0],
            s.ip[1],
            s.ip[2],
            s.ip[3],
            mask_bits(s.mask),
            s.gw[0],
            s.gw[1],
            s.gw[2],
            s.gw[3]
        );
    }
}

unsafe fn send_discover(now: u64, deadline_ticks: u64) {
    let mut tx = [0u8; 600];
    let len = build_dhcp(
        &mut tx,
        BROADCAST_MAC,
        DHCP_DISCOVER,
        XID,
        [0; 4],
        [0; 4],
        [0; 4],
        false,
    );
    let _ = uplink_send(&tx[..len]);
    PHASE = Phase::Discover;
    DEADLINE = now + deadline_ticks;
    TRIES = 0;
    let xid = XID;
    crate::kprintln!("[serial] [net] dhcp discover xid={:08x}", xid);
}

unsafe fn send_request(now: u64, renew: bool) {
    let offer = *addr_of!(OFFER);
    let bound = *addr_of!(BOUND);
    let req_ip = if renew { bound.ip } else { offer.ip };
    let dst_mac = arp_lookup(offer.gw).unwrap_or(BROADCAST_MAC);
    let mut tx = [0u8; 600];
    let len = build_dhcp(
        &mut tx,
        dst_mac,
        DHCP_REQUEST,
        XID,
        req_ip,
        offer.server,
        if renew { bound.ip } else { [0; 4] },
        renew,
    );
    let _ = uplink_send(&tx[..len]);
    PHASE = if renew { Phase::Renew } else { Phase::Request };
    DEADLINE = now + 100;
    TRIES = 0;
    crate::kprintln!(
        "[serial] [net] dhcp {} ip={}.{}.{}.{}",
        if renew { "renew" } else { "request" },
        req_ip[0],
        req_ip[1],
        req_ip[2],
        req_ip[3]
    );
}

unsafe fn step_dhcp(now: u64) {
    let phase = *addr_of!(PHASE);
    match phase {
        Phase::Discover => {
            if *addr_of!(GOT_OFFER) {
                GOT_OFFER = false;
                send_request(now, false);
            } else if now > *addr_of!(DEADLINE) {
                dhcp_timeout(now, phase);
            }
        }
        Phase::Request | Phase::Renew => {
            if *addr_of!(GOT_ACK) {
                GOT_ACK = false;
                bind(now, phase == Phase::Renew);
            } else if now > *addr_of!(DEADLINE) {
                dhcp_timeout(now, phase);
            }
        }
        _ => {}
    }
}

unsafe fn dhcp_timeout(now: u64, phase: Phase) {
    RETRIES = RETRIES.saturating_add(1);
    if RETRIES > 5 {
        PHASE = Phase::Idle;
        NEXT_ACTION = now + 1000;
        RETRIES = 6;
        crate::kprintln!("[serial] [net] dhcp timeout (link-local only)");
        return;
    }
    if TRIES < 3 {
        TRIES += 1;
    }
    let backoff = 100u64 << TRIES;
    DEADLINE = now + backoff;
    match phase {
        Phase::Discover => {
            let mut tx = [0u8; 600];
            let len = build_dhcp(
                &mut tx,
                BROADCAST_MAC,
                DHCP_DISCOVER,
                XID,
                [0; 4],
                [0; 4],
                [0; 4],
                false,
            );
            let _ = uplink_send(&tx[..len]);
            PHASE = Phase::Discover;
            let retries = RETRIES;
            crate::kprintln!("[serial] [net] dhcp discover retry {}", retries);
        }
        Phase::Request => send_request(now, false),
        Phase::Renew => send_request(now, true),
        _ => {}
    }
}

unsafe fn bind(now: u64, renewed: bool) {
    let l = *addr_of!(OFFER);
    *addr_of_mut!(BOUND) = l;
    RETRIES = 0;
    BOUND_AT = now;
    PHASE = Phase::Bound;
    NEXT_ACTION = now + 10;
    NET_OK = false;
    reset_arp();
    crate::kprintln!(
        "[serial] [net] dhcp {} ip={}.{}.{}.{}/{} gw={}.{}.{}.{} dns={}.{}.{}.{}/{}.{}.{}.{}",
        if renewed { "renewed" } else { "bound" },
        l.ip[0],
        l.ip[1],
        l.ip[2],
        l.ip[3],
        mask_bits(l.mask),
        l.gw[0],
        l.gw[1],
        l.gw[2],
        l.gw[3],
        l.dns1[0],
        l.dns1[1],
        l.dns1[2],
        l.dns1[3],
        l.dns2[0],
        l.dns2[1],
        l.dns2[2],
        l.dns2[3]
    );
}

unsafe fn step_bound(now: u64) {
    if !has_ip() {
        return;
    }
    if (*addr_of!(SETTINGS)).dhcp {
        let secs = (*addr_of!(BOUND)).secs;
        if secs > 0 {
            let t1 = (secs as u64 / 2) * TIMER_HZ;
            if now.wrapping_sub(*addr_of!(BOUND_AT)) >= t1 {
                send_request(now, true);
                return;
            }
        }
    }
    if now >= *addr_of!(NEXT_ACTION) {
        start_check(now);
    }
}

unsafe fn start_check(now: u64) {
    TRIES = 0;
    let gw = (*addr_of!(BOUND)).gw;
    if gw != [0; 4] && arp_lookup(gw).is_none() {
        send_arp_request(gw);
        PHASE = Phase::CheckArp;
        DEADLINE = now + 100;
        return;
    }
    if gw != [0; 4] {
        send_echo(gw);
        PHASE = Phase::CheckIcmp;
        DEADLINE = now + 100;
        return;
    }
    let dns1 = (*addr_of!(BOUND)).dns1;
    if dns1 != [0; 4] {
        send_dns(dns1);
        PHASE = Phase::CheckDns;
        DEADLINE = now + 100;
        return;
    }
    NEXT_ACTION = now + RECHECK_TICKS;
}

unsafe fn step_arp(now: u64) {
    if *addr_of!(GOT_ARP) {
        GOT_ARP = false;
        send_echo((*addr_of!(BOUND)).gw);
        PHASE = Phase::CheckIcmp;
        DEADLINE = now + 100;
        TRIES = 0;
        return;
    }
    if now > *addr_of!(DEADLINE) {
        if *addr_of!(TRIES) < 2 {
            TRIES += 1;
            send_arp_request((*addr_of!(BOUND)).gw);
            DEADLINE = now + 100;
        } else {
            fail_check(now);
        }
    }
}

unsafe fn step_icmp(now: u64) {
    if *addr_of!(GOT_ICMP) {
        GOT_ICMP = false;
        let dns1 = (*addr_of!(BOUND)).dns1;
        if dns1 != [0; 4] {
            send_dns(dns1);
            PHASE = Phase::CheckDns;
            DEADLINE = now + 100;
            TRIES = 0;
        } else {
            succeed_check(now);
        }
        return;
    }
    if now > *addr_of!(DEADLINE) {
        if *addr_of!(TRIES) < 2 {
            TRIES += 1;
            send_echo((*addr_of!(BOUND)).gw);
            DEADLINE = now + 100;
        } else {
            fail_check(now);
        }
    }
}

unsafe fn step_dns(now: u64) {
    if *addr_of!(GOT_DNS) {
        GOT_DNS = false;
        succeed_check(now);
        return;
    }
    if now > *addr_of!(DEADLINE) {
        if *addr_of!(TRIES) < 2 {
            TRIES += 1;
            send_dns((*addr_of!(BOUND)).dns1);
            DEADLINE = now + 100;
        } else {
            fail_check(now);
        }
    }
}

unsafe fn succeed_check(now: u64) {
    if !*addr_of!(NET_OK) {
        crate::kprintln!("[serial] [net] internet ok");
    }
    NET_OK = true;
    PHASE = Phase::Bound;
    NEXT_ACTION = now + RECHECK_TICKS;
}

unsafe fn fail_check(now: u64) {
    NET_OK = false;
    PHASE = Phase::Bound;
    NEXT_ACTION = now + FAIL_RECHECK_TICKS;
}

fn send_arp_request(target: [u8; 4]) {
    let mut tx = [0u8; 42];
    tx[0..6].copy_from_slice(&BROADCAST_MAC);
    tx[6..12].copy_from_slice(&uplink_mac());
    tx[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    tx[14..16].copy_from_slice(&1u16.to_be_bytes());
    tx[16..18].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    tx[18] = 6;
    tx[19] = 4;
    tx[20..22].copy_from_slice(&1u16.to_be_bytes());
    tx[22..28].copy_from_slice(&uplink_mac());
    tx[28..32].copy_from_slice(&current_ip());
    tx[38..42].copy_from_slice(&target);
    let _ = uplink_send(&tx);
}

fn send_echo(target: [u8; 4]) {
    let payload = b"aios-net-probe-0123456789";
    let icmp_len = 8 + payload.len();
    let total = 20 + icmp_len;
    let mut tx = [0u8; 80];
    start_frame(&mut tx, target_mac(target), target, PROTO_ICMP, icmp_len);
    let o = 34;
    tx[o] = ICMP_ECHO;
    tx[o + 1] = 0;
    tx[o + 4..o + 6].copy_from_slice(&ICMP_PROBE_ID.to_be_bytes());
    let seq = next_seq();
    tx[o + 6..o + 8].copy_from_slice(&seq.to_be_bytes());
    tx[o + 8..o + 8 + payload.len()].copy_from_slice(payload);
    let sum = csum_finish(csum_add(0, &tx[o..o + icmp_len]));
    tx[o + 2..o + 4].copy_from_slice(&sum.to_be_bytes());
    let _ = uplink_send(&tx[..14 + total]);
}

fn send_dns(server: [u8; 4]) {
    let name = b"\x07example\x03com\x00";
    let dns_len = 12 + name.len() + 4;
    let udp_len = 8 + dns_len;
    let total = 20 + udp_len;
    let txid = (ticks() as u16) ^ 0x5A5A;
    set_dns_xid(txid);
    let src = current_ip();
    let mut tx = [0u8; 128];
    start_frame(&mut tx, target_mac(server), server, PROTO_UDP, udp_len);
    let o = 34;
    tx[o..o + 2].copy_from_slice(&DNS_SRC_PORT.to_be_bytes());
    tx[o + 2..o + 4].copy_from_slice(&DNS_PORT.to_be_bytes());
    tx[o + 4..o + 6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    let d = o + 8;
    tx[d..d + 2].copy_from_slice(&txid.to_be_bytes());
    tx[d + 2..d + 4].copy_from_slice(&0x0100u16.to_be_bytes());
    tx[d + 4..d + 6].copy_from_slice(&1u16.to_be_bytes());
    tx[d + 12..d + 12 + name.len()].copy_from_slice(name);
    let q = d + 12 + name.len();
    tx[q..q + 2].copy_from_slice(&1u16.to_be_bytes());
    tx[q + 2..q + 4].copy_from_slice(&1u16.to_be_bytes());
    let mut sum = csum_add(0, &src);
    sum = csum_add(sum, &server);
    sum += u32::from(PROTO_UDP) + udp_len as u32;
    sum = csum_add(sum, &tx[o..o + udp_len]);
    let c = csum_finish(sum);
    tx[o + 6..o + 8].copy_from_slice(&c.to_be_bytes());
    let _ = uplink_send(&tx[..14 + total]);
}

fn target_mac(target: [u8; 4]) -> [u8; 6] {
    arp_lookup(target).unwrap_or(BROADCAST_MAC)
}

/// Writes the Ethernet + IPv4 headers for `payload_len` bytes of L4 data;
/// the caller fills the payload at offset 34 afterwards.
fn start_frame(tx: &mut [u8], dst_mac: [u8; 6], dst_ip: [u8; 4], proto: u8, payload_len: usize) {
    tx[0..6].copy_from_slice(&dst_mac);
    tx[6..12].copy_from_slice(&uplink_mac());
    tx[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    let total = 20 + payload_len;
    tx[14] = 0x45;
    tx[15] = 0;
    tx[16..18].copy_from_slice(&(total as u16).to_be_bytes());
    tx[18..20].copy_from_slice(&0u16.to_be_bytes());
    tx[20..22].copy_from_slice(&0x4000u16.to_be_bytes());
    tx[22] = 64;
    tx[23] = proto;
    tx[24..26].copy_from_slice(&[0; 2]);
    tx[26..30].copy_from_slice(&current_ip());
    tx[30..34].copy_from_slice(&dst_ip);
    let sum = csum_finish(csum_add(0, &tx[14..34]));
    tx[24..26].copy_from_slice(&sum.to_be_bytes());
}

fn csum_add(mut acc: u32, bytes: &[u8]) -> u32 {
    let mut i = 0;
    while i + 1 < bytes.len() {
        acc += u32::from(u16::from_be_bytes([bytes[i], bytes[i + 1]]));
        i += 2;
    }
    if i < bytes.len() {
        acc += u32::from(u16::from_be_bytes([bytes[i], 0]));
    }
    acc
}

fn csum_finish(mut acc: u32) -> u16 {
    while acc >> 16 != 0 {
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    !(acc as u16)
}

/// Builds a complete DHCP client frame; returns the Ethernet frame length.
#[allow(clippy::too_many_arguments)]
fn build_dhcp(
    tx: &mut [u8],
    dst_mac: [u8; 6],
    msg: u8,
    xid: u32,
    req_ip: [u8; 4],
    server: [u8; 4],
    ciaddr: [u8; 4],
    renewing: bool,
) -> usize {
    let mac = uplink_mac();
    let src_ip = ciaddr;
    let dst_ip = if renewing && server != [0; 4] {
        server
    } else {
        BROADCAST_IP
    };
    tx[0..6].copy_from_slice(&dst_mac);
    tx[6..12].copy_from_slice(&mac);
    tx[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

    let bootp = 42;
    tx[bootp] = 1;
    tx[bootp + 1] = 1;
    tx[bootp + 2] = 6;
    tx[bootp + 3] = 0;
    tx[bootp + 4..bootp + 8].copy_from_slice(&xid.to_le_bytes());
    tx[bootp + 8..bootp + 10].copy_from_slice(&0u16.to_be_bytes());
    let flags: u16 = if renewing { 0 } else { 0x8000 };
    tx[bootp + 10..bootp + 12].copy_from_slice(&flags.to_be_bytes());
    tx[bootp + 12..bootp + 16].copy_from_slice(&ciaddr);
    tx[bootp + 28..bootp + 34].copy_from_slice(&mac);

    let magic = bootp + 236;
    tx[magic..magic + 4].copy_from_slice(&DHCP_MAGIC);
    let mut o = magic + 4;
    tx[o] = 53;
    tx[o + 1] = 1;
    tx[o + 2] = msg;
    o += 3;
    tx[o] = 55;
    tx[o + 1] = 4;
    tx[o + 2] = 1;
    tx[o + 3] = 3;
    tx[o + 4] = 6;
    tx[o + 5] = 51;
    o += 6;
    if !renewing && req_ip != [0; 4] {
        tx[o] = 50;
        tx[o + 1] = 4;
        tx[o + 2..o + 6].copy_from_slice(&req_ip);
        o += 6;
    }
    if server != [0; 4] {
        tx[o] = 54;
        tx[o + 1] = 4;
        tx[o + 2..o + 6].copy_from_slice(&server);
        o += 6;
    }
    tx[o] = 0xFF;
    o += 1;

    let bootp_len = o - bootp;
    let udp_len = 8 + bootp_len;
    let ip_total = 20 + udp_len;
    tx[14] = 0x45;
    tx[15] = 0;
    tx[16..18].copy_from_slice(&(ip_total as u16).to_be_bytes());
    tx[18..20].copy_from_slice(&(xid as u16).to_be_bytes());
    tx[20..22].copy_from_slice(&0x4000u16.to_be_bytes());
    tx[22] = 64;
    tx[23] = PROTO_UDP;
    tx[24..26].copy_from_slice(&[0; 2]);
    tx[26..30].copy_from_slice(&src_ip);
    tx[30..34].copy_from_slice(&dst_ip);
    let ip_sum = csum_finish(csum_add(0, &tx[14..34]));
    tx[24..26].copy_from_slice(&ip_sum.to_be_bytes());

    tx[34..36].copy_from_slice(&DHCP_CLIENT_PORT.to_be_bytes());
    tx[36..38].copy_from_slice(&DHCP_SERVER_PORT.to_be_bytes());
    tx[38..40].copy_from_slice(&(udp_len as u16).to_be_bytes());
    let mut sum = csum_add(0, &src_ip);
    sum = csum_add(sum, &dst_ip);
    sum += u32::from(PROTO_UDP) + udp_len as u32;
    sum = csum_add(sum, &tx[34..34 + udp_len]);
    let udp_sum = csum_finish(sum);
    tx[40..42].copy_from_slice(&udp_sum.to_be_bytes());
    14 + ip_total
}

const CMOS_PACKET: u8 = CMOS_START + 4;

fn cmos_read(index: u8) -> u8 {
    unsafe {
        port::outb(CMOS_INDEX, index);
        port::inb(CMOS_DATA)
    }
}

fn cmos_write(index: u8, value: u8) {
    unsafe {
        port::outb(CMOS_INDEX, index);
        port::outb(CMOS_DATA, value);
    }
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for b in data {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn payload_from(s: &Settings) -> [u8; CMOS_PAYLOAD] {
    let mut p = [0u8; CMOS_PAYLOAD];
    p[0] = if s.dhcp { 1 } else { 0 };
    p[1..5].copy_from_slice(&s.ip);
    p[5..9].copy_from_slice(&s.mask);
    p[9..13].copy_from_slice(&s.gw);
    p[13..17].copy_from_slice(&s.dns1);
    p[17..21].copy_from_slice(&s.dns2);
    p
}

fn cmos_load() {
    let mut payload = [0u8; CMOS_PAYLOAD];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = cmos_read(CMOS_PACKET + i as u8);
    }
    let magic_ok = cmos_read(CMOS_START) == CMOS_MAGIC && cmos_read(CMOS_START + 1) == CMOS_VER;
    let stored = u16::from_le_bytes([cmos_read(CMOS_START + 2), cmos_read(CMOS_START + 3)]);
    if magic_ok && crc16(&payload) == stored {
        let mut s = Settings::DEFAULT;
        s.dhcp = payload[0] & 1 != 0;
        s.ip.copy_from_slice(&payload[1..5]);
        s.mask.copy_from_slice(&payload[5..9]);
        s.gw.copy_from_slice(&payload[9..13]);
        s.dns1.copy_from_slice(&payload[13..17]);
        s.dns2.copy_from_slice(&payload[17..21]);
        unsafe { *addr_of_mut!(SETTINGS) = s };
        crate::kprintln!("[serial] [net] config loaded from cmos");
    } else {
        unsafe { *addr_of_mut!(SETTINGS) = Settings::DEFAULT };
        crate::kprintln!("[serial] [net] config defaults (no valid cmos)");
    }
}

fn cmos_save() {
    let payload = payload_from(&read_settings());
    let crc = crc16(&payload);
    for (i, b) in payload.iter().enumerate() {
        cmos_write(CMOS_PACKET + i as u8, *b);
    }
    cmos_write(CMOS_START + 2, crc as u8);
    cmos_write(CMOS_START + 3, (crc >> 8) as u8);
    cmos_write(CMOS_START + 1, CMOS_VER);
    cmos_write(CMOS_START, CMOS_MAGIC);
}
