//! Full software Wi-Fi stack over a simulated test radio (v2.38.34):
//! 802.11 management and data frames, WPA2-PSK four-way handshake with real
//! cryptography (PBKDF2/SHA-1 PMK, PRF-512 PTK, HMAC-SHA1-128 EAPOL MICs,
//! RFC 3394 AES key wrap for the GTK, AES-CCMP data protection) and a local
//! AP-side LAN with DHCP/ARP/ICMP/DNS responders on `10.0.9.0/24`.
//!
//! QEMU provides no Wi-Fi device, so [`TestRadio`] synthesises three access
//! points ("AIOS-Test" open, "SecureNet" and "Neighbor" WPA2-PSK) and plays
//! both the station and the access-point side of every exchange. Everything
//! below the radio is production protocol code — only the medium is
//! simulated (`sim` in status lines); a future real chip plugs in behind
//! [`RealRadio`].
//!
//! The station exposes an Ethernet-shaped interface to [crate::net] via
//! [`send_frame`] / [`poll_frame`]; the AP side terminates the LAN and
//! answers DHCP, ARP, ICMP echo and DNS for the station, which is what makes
//! the stack's reachability check report internet over Wi-Fi.

use crate::interrupts::TICKS;
use core::ptr::{addr_of, addr_of_mut};

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_EAPOL: u16 = 0x888E;
const PROTO_ICMP: u8 = 1;
const PROTO_UDP: u8 = 17;
const ICMP_ECHO: u8 = 8;
const ICMP_REPLY: u8 = 0;
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];

const FC_PROBE_REQ: u16 = 0x0040;
const FC_PROBE_RESP: u16 = 0x0050;
const FC_BEACON: u16 = 0x0080;
const FC_ASSOC_REQ: u16 = 0x0000;
const FC_ASSOC_RESP: u16 = 0x0010;
const FC_AUTH: u16 = 0x00B0;
const FC_DEAUTH: u16 = 0x00C0;
const FC_DATA_TODS: u16 = 0x0108;
const FC_DATA_FROMDS: u16 = 0x0208;
const FC_PROTECTED: u16 = 0x0040;

const KEY_INFO_M1: u16 = 0x008A;
const KEY_INFO_M2: u16 = 0x010A;
const KEY_INFO_M3: u16 = 0x13CA;
const KEY_INFO_M4: u16 = 0x030A;

const QDEPTH: usize = 16;
const MGMT_HDR: usize = 24;
const CCMP_HDR: usize = 8;
const MIC_LEN: usize = 8;
const MAX_FRAME: usize = 1600;
const SCAN_TICKS: u64 = 50;
const CONNECT_TICKS: u64 = 3000;
const BEACON_TICKS: u64 = 50;
const STATION_IP: [u8; 4] = [10, 0, 9, 15];
const AP_IP: [u8; 4] = [10, 0, 9, 1];
const DNS_IP: [u8; 4] = [10, 0, 9, 3];
const AP_MASK: [u8; 4] = [255, 255, 255, 0];
const FAKE_INTERNET_IP: [u8; 4] = [203, 0, 113, 10];
const STATION_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0xAA];
const BCAST_MAC: [u8; 6] = [0xFF; 6];
const QOS_PRIORITY: u8 = 0;

/// Association state of the simulated station.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WifiState {
    /// Radio up, no scan and no association.
    Idle,
    /// Active probe scan (results land in [`scan_results`]).
    Scanning,
    /// Auth/assoc/4-way handshake in progress.
    Connecting,
    /// Associated (and WPA2 handshake complete when required).
    Associated,
}

/// Security mode advertised by an access point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Open network (no encryption).
    Open,
    /// WPA2-PSK with AES-CCMP.
    Wpa2,
}

/// One entry returned by [`scan_results`].
#[derive(Clone, Copy)]
pub struct ScanResult {
    /// SSID bytes (not NUL terminated; see [`ssid_len`](Self::ssid_len)).
    pub ssid: [u8; 32],
    /// Significant length of [`ssid`](Self::ssid).
    pub ssid_len: u8,
    /// Signal strength in dBm.
    pub rssi: i8,
    /// Advertised security mode.
    pub security: Security,
    /// Advertised channel.
    pub channel: u8,
}

impl ScanResult {
    /// Empty slot.
    pub const EMPTY: ScanResult = ScanResult {
        ssid: [0; 32],
        ssid_len: 0,
        rssi: 0,
        security: Security::Open,
        channel: 0,
    };
}

struct ApInfo {
    ssid: &'static [u8],
    bssid: [u8; 6],
    channel: u8,
    rssi: i8,
    secure: bool,
    psk: &'static [u8],
}

/// The three synthetic access points of the test radio.
const APS: [ApInfo; 3] = [
    ApInfo {
        ssid: b"AIOS-Test",
        bssid: [0x02, 0, 0, 0, 0, 0x01],
        channel: 6,
        rssi: -38,
        secure: false,
        psk: b"",
    },
    ApInfo {
        ssid: b"SecureNet",
        bssid: [0x02, 0, 0, 0, 0, 0x02],
        channel: 6,
        rssi: -42,
        secure: true,
        psk: b"AIOS-Test",
    },
    ApInfo {
        ssid: b"Neighbor",
        bssid: [0x02, 0, 0, 0, 0, 0x03],
        channel: 11,
        rssi: -67,
        secure: true,
        psk: b"NeighborNet",
    },
];

const AP_NONE: usize = usize::MAX;

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

fn xtime(v: u8) -> u8 {
    (v << 1) ^ if v & 0x80 != 0 { 0x1b } else { 0 }
}

fn aes128_ek(key: &[u8; 16]) -> [u8; 176] {
    let mut w = [0u8; 176];
    w[..16].copy_from_slice(key);
    let mut rcon: u8 = 1;
    for i in 4..44 {
        let mut t = [
            w[(i - 1) * 4],
            w[(i - 1) * 4 + 1],
            w[(i - 1) * 4 + 2],
            w[(i - 1) * 4 + 3],
        ];
        if i % 4 == 0 {
            let tmp = t;
            t[0] = SBOX[tmp[1] as usize] ^ rcon;
            t[1] = SBOX[tmp[2] as usize];
            t[2] = SBOX[tmp[3] as usize];
            t[3] = SBOX[tmp[0] as usize];
            rcon = if rcon & 0x80 != 0 {
                (rcon << 1) ^ 0x1b
            } else {
                rcon << 1
            };
        }
        for j in 0..4 {
            w[i * 4 + j] = w[(i - 4) * 4 + j] ^ t[j];
        }
    }
    w
}

fn aes128_enc(rk: &[u8; 176], block: &mut [u8; 16]) {
    for j in 0..16 {
        block[j] ^= rk[j];
    }
    for round in 1..10 {
        for b in block.iter_mut() {
            *b = SBOX[*b as usize];
        }
        let s = *block;
        block[1] = s[5];
        block[5] = s[9];
        block[9] = s[13];
        block[13] = s[1];
        block[2] = s[10];
        block[6] = s[14];
        block[10] = s[2];
        block[14] = s[6];
        block[3] = s[15];
        block[7] = s[3];
        block[11] = s[7];
        block[15] = s[11];
        for c in 0..4 {
            let a = [
                block[c * 4],
                block[c * 4 + 1],
                block[c * 4 + 2],
                block[c * 4 + 3],
            ];
            block[c * 4] = xtime(a[0]) ^ (xtime(a[1]) ^ a[1]) ^ a[2] ^ a[3];
            block[c * 4 + 1] = a[0] ^ xtime(a[1]) ^ (xtime(a[2]) ^ a[2]) ^ a[3];
            block[c * 4 + 2] = a[0] ^ a[1] ^ xtime(a[2]) ^ (xtime(a[3]) ^ a[3]);
            block[c * 4 + 3] = (xtime(a[0]) ^ a[0]) ^ a[1] ^ a[2] ^ xtime(a[3]);
        }
        let off = round * 16;
        for j in 0..16 {
            block[j] ^= rk[off + j];
        }
    }
    for b in block.iter_mut() {
        *b = SBOX[*b as usize];
    }
    let s = *block;
    block[1] = s[5];
    block[5] = s[9];
    block[9] = s[13];
    block[13] = s[1];
    block[2] = s[10];
    block[6] = s[14];
    block[10] = s[2];
    block[14] = s[6];
    block[3] = s[15];
    block[7] = s[3];
    block[11] = s[7];
    block[15] = s[11];
    for j in 0..16 {
        block[j] ^= rk[160 + j];
    }
}

fn inv_shift_sub(block: &mut [u8; 16]) {
    let s = *block;
    block[0] = inv_sbox(s[0] as usize);
    block[4] = inv_sbox(s[4] as usize);
    block[8] = inv_sbox(s[8] as usize);
    block[12] = inv_sbox(s[12] as usize);
    block[1] = inv_sbox(s[13] as usize);
    block[5] = inv_sbox(s[1] as usize);
    block[9] = inv_sbox(s[5] as usize);
    block[13] = inv_sbox(s[9] as usize);
    block[2] = inv_sbox(s[10] as usize);
    block[6] = inv_sbox(s[14] as usize);
    block[10] = inv_sbox(s[2] as usize);
    block[14] = inv_sbox(s[6] as usize);
    block[3] = inv_sbox(s[7] as usize);
    block[7] = inv_sbox(s[11] as usize);
    block[11] = inv_sbox(s[15] as usize);
    block[15] = inv_sbox(s[3] as usize);
}

fn inv_mix_columns(block: &mut [u8; 16]) {
    let s = *block;
    for c in 0..4 {
        let a = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
        block[c * 4] = mul14(a[0]) ^ mul11(a[1]) ^ mul13(a[2]) ^ mul9(a[3]);
        block[c * 4 + 1] = mul9(a[0]) ^ mul14(a[1]) ^ mul11(a[2]) ^ mul13(a[3]);
        block[c * 4 + 2] = mul13(a[0]) ^ mul9(a[1]) ^ mul14(a[2]) ^ mul11(a[3]);
        block[c * 4 + 3] = mul11(a[0]) ^ mul13(a[1]) ^ mul9(a[2]) ^ mul14(a[3]);
    }
}

fn aes128_dec(rk: &[u8; 176], block: &mut [u8; 16]) {
    for j in 0..16 {
        block[j] ^= rk[160 + j];
    }
    for round in (1..10).rev() {
        inv_shift_sub(block);
        let off = round * 16;
        for j in 0..16 {
            block[j] ^= rk[off + j];
        }
        inv_mix_columns(block);
    }
    inv_shift_sub(block);
    for j in 0..16 {
        block[j] ^= rk[j];
    }
}

fn inv_sbox(v: usize) -> u8 {
    for (i, s) in SBOX.iter().enumerate() {
        if *s as usize == v {
            return i as u8;
        }
    }
    0
}

fn mul(v: u8, by: u8) -> u8 {
    let mut r = 0u8;
    let mut a = v;
    let mut b = by;
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    r
}

fn mul9(v: u8) -> u8 {
    mul(v, 9)
}
fn mul11(v: u8) -> u8 {
    mul(v, 11)
}
fn mul13(v: u8) -> u8 {
    mul(v, 13)
}
fn mul14(v: u8) -> u8 {
    mul(v, 14)
}

fn sha1_parts(parts: &[&[u8]]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut block = [0u8; 64];
    let mut blen = 0usize;
    let mut total: u64 = 0;
    for p in parts {
        total = total.wrapping_add(p.len() as u64);
        for b in *p {
            block[blen] = *b;
            blen += 1;
            if blen == 64 {
                sha1_block(&mut h, &block);
                blen = 0;
            }
        }
    }
    block[blen] = 0x80;
    blen += 1;
    if blen > 56 {
        while blen < 64 {
            block[blen] = 0;
            blen += 1;
        }
        sha1_block(&mut h, &block);
        blen = 0;
    }
    while blen < 56 {
        block[blen] = 0;
        blen += 1;
    }
    block[56..64].copy_from_slice(&total.wrapping_mul(8).to_be_bytes());
    sha1_block(&mut h, &block);
    let mut out = [0u8; 20];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

fn sha1(data: &[u8]) -> [u8; 20] {
    sha1_parts(&[data])
}

fn sha1_block(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
    for (i, wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
            20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
            _ => (b ^ c ^ d, 0xCA62C1D6),
        };
        let tmp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = tmp;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
}

fn hmac_sha1(key: &[u8], msgs: &[&[u8]]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let h = sha1(key);
        k[..20].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut buf = [0u8; 704];
    let mut o = 64;
    for i in 0..64 {
        buf[i] = k[i] ^ 0x36;
    }
    for m in msgs {
        buf[o..o + m.len()].copy_from_slice(m);
        o += m.len();
    }
    let inner = sha1(&buf[..o]);
    for i in 0..64 {
        buf[i] = k[i] ^ 0x5C;
    }
    buf[64..84].copy_from_slice(&inner);
    sha1(&buf[..84])
}

fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32, dk: &mut [u8]) {
    for (ctr, chunk) in (1_u32..).zip(dk.chunks_mut(20)) {
        let cbytes = ctr.to_be_bytes();
        let mut u = hmac_sha1(password, &[salt, &cbytes]);
        let mut t = [0u8; 20];
        let n = chunk.len();
        t[..n].copy_from_slice(&u[..n]);
        for _ in 1..iterations {
            u = hmac_sha1(password, &[&u]);
            for j in 0..n {
                t[j] ^= u[j];
            }
        }
        chunk.copy_from_slice(&t[..n]);
    }
}

fn prf512(pmk: &[u8; 32], label: &[u8], parts: &[&[u8]]) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut o = 0;
    for i in 0u32..4 {
        let c = i.to_be_bytes();
        let mut msgs: [&[u8]; 8] = [&[]; 8];
        msgs[0] = label;
        msgs[1] = &[0];
        for (j, p) in parts.iter().enumerate() {
            msgs[2 + j] = p;
        }
        msgs[2 + parts.len()] = &c;
        let t = hmac_sha1(pmk, &msgs[..3 + parts.len()]);
        let n = (64 - o).min(20);
        out[o..o + n].copy_from_slice(&t[..n]);
        o += n;
    }
    out
}

fn derive_gtk(pmk: &[u8; 32], bssid: &[u8; 6], anonce: &[u8; 32]) -> [u8; 16] {
    let t = hmac_sha1(pmk, &[b"GK", bssid, anonce]);
    let mut g = [0u8; 16];
    g.copy_from_slice(&t[..16]);
    g
}

fn aes_key_wrap(kek: &[u8; 16], pt: &[u8; 16]) -> [u8; 24] {
    let rk = aes128_ek(kek);
    let mut a = [0xA6u8; 8];
    let mut r = [[0u8; 8]; 2];
    r[0].copy_from_slice(&pt[..8]);
    r[1].copy_from_slice(&pt[8..]);
    for j in 0u64..6 {
        for (i, rb) in r.iter_mut().enumerate() {
            let mut blk = [0u8; 16];
            blk[..8].copy_from_slice(&a);
            blk[8..].copy_from_slice(rb);
            aes128_enc(&rk, &mut blk);
            let t = (2 * j + (i as u64) + 1).to_be_bytes();
            for k in 0..8 {
                blk[k] ^= t[k];
            }
            a.copy_from_slice(&blk[..8]);
            rb.copy_from_slice(&blk[8..]);
        }
    }
    let mut out = [0u8; 24];
    out[..8].copy_from_slice(&a);
    out[8..16].copy_from_slice(&r[0]);
    out[16..].copy_from_slice(&r[1]);
    out
}

fn aes_key_unwrap(kek: &[u8; 16], ct: &[u8; 24]) -> Option<[u8; 16]> {
    let rk = aes128_ek(kek);
    let mut a = [0u8; 8];
    a.copy_from_slice(&ct[..8]);
    let mut r = [[0u8; 8]; 2];
    r[0].copy_from_slice(&ct[8..16]);
    r[1].copy_from_slice(&ct[16..]);
    for j in (0u64..6).rev() {
        for (i, rb) in r.iter_mut().enumerate().rev() {
            let t = (2 * j + (i as u64) + 1).to_be_bytes();
            let mut blk = [0u8; 16];
            for k in 0..8 {
                blk[k] = a[k] ^ t[k];
            }
            blk[8..].copy_from_slice(rb);
            aes128_dec(&rk, &mut blk);
            a.copy_from_slice(&blk[..8]);
            rb.copy_from_slice(&blk[8..]);
        }
    }
    if a == [0xA6; 8] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&r[0]);
        out[8..].copy_from_slice(&r[1]);
        Some(out)
    } else {
        None
    }
}

fn ccm_ctr_block(rk: &[u8; 176], nonce: &[u8; 13], counter: u16, out: &mut [u8; 16]) {
    out[0] = 1;
    out[1..14].copy_from_slice(nonce);
    out[14..16].copy_from_slice(&counter.to_be_bytes());
    aes128_enc(rk, out);
}

fn ccm_tag(rk: &[u8; 176], nonce: &[u8; 13], aad: &[u8], pt: &[u8]) -> [u8; 8] {
    let mut y = [0u8; 16];
    y[0] = 0x59;
    y[1..14].copy_from_slice(nonce);
    y[14..16].copy_from_slice(&(pt.len() as u16).to_be_bytes());
    aes128_enc(rk, &mut y);

    let mut block = [0u8; 16];
    let la = (aad.len() as u16).to_be_bytes();
    let mut feed = |y: &mut [u8; 16], data: &[u8]| {
        for chunk in data.chunks(16) {
            block.fill(0);
            block[..chunk.len()].copy_from_slice(chunk);
            for j in 0..16 {
                block[j] ^= y[j];
            }
            aes128_enc(rk, &mut block);
            y.copy_from_slice(&block);
        }
    };
    let mut aad_stream = [0u8; 40];
    aad_stream[..2].copy_from_slice(&la);
    aad_stream[2..2 + aad.len()].copy_from_slice(aad);
    feed(&mut y, &aad_stream[..2 + aad.len()]);
    feed(&mut y, pt);
    let mut s0 = [0u8; 16];
    ccm_ctr_block(rk, nonce, 0, &mut s0);
    let mut tag = [0u8; 8];
    for j in 0..8 {
        tag[j] = y[j] ^ s0[j];
    }
    tag
}

fn ccm_xor(rk: &[u8; 176], nonce: &[u8; 13], start: u16, data: &mut [u8]) {
    let mut ks = [0u8; 16];
    for (counter, chunk) in (start..).zip(data.chunks_mut(16)) {
        ccm_ctr_block(rk, nonce, counter, &mut ks);
        for (j, b) in chunk.iter_mut().enumerate() {
            *b ^= ks[j];
        }
    }
}

fn ccm_encrypt(key: &[u8; 16], nonce: &[u8; 13], aad: &[u8], pt: &[u8], out: &mut [u8]) {
    let rk = aes128_ek(key);
    let tag = ccm_tag(&rk, nonce, aad, pt);
    out[..pt.len()].copy_from_slice(pt);
    ccm_xor(&rk, nonce, 1, &mut out[..pt.len()]);
    out[pt.len()..pt.len() + 8].copy_from_slice(&tag);
}

fn ccm_decrypt(key: &[u8; 16], nonce: &[u8; 13], aad: &[u8], frame: &mut [u8]) -> Option<()> {
    if frame.len() < 8 {
        return None;
    }
    let (body, tag) = frame.split_at_mut(frame.len() - 8);
    let mut expected = [0u8; 8];
    expected.copy_from_slice(tag);
    let rk = aes128_ek(key);
    ccm_xor(&rk, nonce, 1, body);
    let got = ccm_tag(&rk, nonce, aad, body);
    if got == expected {
        Some(())
    } else {
        None
    }
}

fn selftest() -> Result<(), &'static str> {
    let want_sha1 = [
        0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2,
        0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
    ];
    if sha1(b"abc") != want_sha1 {
        return Err("sha1 vector");
    }
    let mut dk = [0u8; 20];
    pbkdf2(b"password", b"salt", 1, &mut dk);
    if dk
        != [
            0x0c, 0x60, 0xc8, 0x0f, 0x96, 0x1f, 0x0e, 0x71, 0xf3, 0xa9, 0xb5, 0x24, 0xaf, 0x60,
            0x12, 0x06, 0x2f, 0xe0, 0x37, 0xa6,
        ]
    {
        return Err("pbkdf2 c=1 vector");
    }
    pbkdf2(b"password", b"salt", 2, &mut dk);
    if dk
        != [
            0xea, 0x6c, 0x01, 0x4d, 0xc7, 0x2d, 0x6f, 0x8c, 0xcd, 0x1e, 0xd9, 0x2a, 0xce, 0x1d,
            0x41, 0xf0, 0xd8, 0xde, 0x89, 0x57,
        ]
    {
        return Err("pbkdf2 c=2 vector");
    }
    pbkdf2(b"password", b"salt", 4096, &mut dk);
    if dk
        != [
            0x4b, 0x00, 0x79, 0x01, 0xb7, 0x65, 0x48, 0x9a, 0xbe, 0xad, 0x49, 0xd9, 0x26, 0xf7,
            0x21, 0xd0, 0x65, 0xa4, 0x29, 0xc1,
        ]
    {
        return Err("pbkdf2 c=4096 vector");
    }
    if hmac_sha1(b"Jefe", &[b"what do ya want for nothing?"])
        != [
            0xef, 0xfc, 0xdf, 0x6a, 0xe5, 0xeb, 0x2f, 0xa2, 0xd2, 0x74, 0x16, 0xd5, 0xf1, 0x84,
            0xdf, 0x9c, 0x25, 0x9a, 0x7c, 0x79,
        ]
    {
        return Err("hmac jefe vector");
    }
    let key0b = [0x0bu8; 20];
    if hmac_sha1(&key0b, &[b"Hi There"])
        != [
            0xb6, 0x17, 0x31, 0x86, 0x55, 0x05, 0x72, 0x64, 0xe2, 0x8b, 0xc0, 0xb6, 0xfb, 0x37,
            0x8c, 0x8e, 0xf1, 0x46, 0xbe, 0x00,
        ]
    {
        return Err("hmac 0b vector");
    }
    let mut pmk = [0u8; 32];
    pbkdf2(b"AIOS-Test", b"AIOS-Test", 4096, &mut pmk);
    if pmk
        != [
            0x6c, 0x1b, 0x26, 0x74, 0x0d, 0xb0, 0xf6, 0x7c, 0x27, 0xc6, 0xe3, 0x48, 0x8f, 0x66,
            0x9e, 0x97, 0xf7, 0x2f, 0x45, 0x5c, 0x01, 0x1c, 0x73, 0x2b, 0xad, 0x6d, 0x1f, 0x4c,
            0xba, 0x87, 0x10, 0xda,
        ]
    {
        return Err("pmk vector");
    }
    let rk = aes128_ek(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    let mut blk = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    aes128_enc(&rk, &mut blk);
    if blk
        != [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
            0xc5, 0x5a,
        ]
    {
        return Err("aes vector");
    }
    let kek = [0x10u8; 16];
    let gtk_probe = [0x20u8; 16];
    let wrapped = aes_key_wrap(&kek, &gtk_probe);
    if aes_key_unwrap(&kek, &wrapped) != Some(gtk_probe) {
        return Err("key wrap roundtrip");
    }
    let rfc_kek = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    let rfc_pt = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    if aes_key_wrap(&rfc_kek, &rfc_pt)
        != [
            0x1f, 0xa6, 0x8b, 0x0a, 0x81, 0x12, 0xb4, 0x47, 0xae, 0xf3, 0x4b, 0xd8, 0xfb, 0x5a,
            0x7b, 0x82, 0x9d, 0x3e, 0x86, 0x23, 0x71, 0xd2, 0xcf, 0xe5,
        ]
    {
        return Err("rfc3394 vector");
    }
    let nonce = [0x11u8; 13];
    let aad = b"aad-sample";
    let pt = b"ccmp-roundtrip-payload";
    let mut enc = [0u8; 64];
    ccm_encrypt(&kek, &nonce, aad, pt, &mut enc[..pt.len() + 8]);
    let mut dec = [0u8; 64];
    dec[..pt.len() + 8].copy_from_slice(&enc[..pt.len() + 8]);
    match ccm_decrypt(&kek, &nonce, aad, &mut dec[..pt.len() + 8]) {
        Some(()) if &dec[..pt.len()] == pt => {}
        _ => return Err("ccm roundtrip"),
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    None,
    WaitAuth,
    WaitAssoc,
    WaitM1,
    WaitM3,
}

/// Fixed-depth frame ring used by all three internal queues.
struct FrameQueue {
    bufs: [[u8; MAX_FRAME]; QDEPTH],
    lens: [u16; QDEPTH],
    head: usize,
    tail: usize,
    count: usize,
}

impl FrameQueue {
    const EMPTY: FrameQueue = FrameQueue {
        bufs: [[0; MAX_FRAME]; QDEPTH],
        lens: [0; QDEPTH],
        head: 0,
        tail: 0,
        count: 0,
    };

    fn push(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.len() > MAX_FRAME {
            return Err("wifi: frame too large");
        }
        if self.count == QDEPTH {
            return Err("wifi: queue full");
        }
        self.bufs[self.tail][..frame.len()].copy_from_slice(frame);
        self.lens[self.tail] = frame.len() as u16;
        self.tail = (self.tail + 1) % QDEPTH;
        self.count += 1;
        Ok(())
    }

    fn pop(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let h = self.head;
        let n = self.lens[h] as usize;
        self.head = (h + 1) % QDEPTH;
        self.count -= 1;
        if n > out.len() {
            return None;
        }
        out[..n].copy_from_slice(&self.bufs[h][..n]);
        Some(n)
    }

    fn busy(&self) -> bool {
        self.count > 0
    }
}

static mut TXQ: FrameQueue = FrameQueue::EMPTY;
static mut RXQ: FrameQueue = FrameQueue::EMPTY;
static mut ETHQ: FrameQueue = FrameQueue::EMPTY;
static mut WSTATE: WifiState = WifiState::Idle;
static mut STAGE: Stage = Stage::None;
static mut SCAN_END: u64 = 0;
static mut CONNECT_AT: u64 = 0;
static mut BEACON_AT: u64 = 0;
static mut RESULTS: [ScanResult; 8] = [ScanResult::EMPTY; 8];
static mut RESULTS_N: usize = 0;
static mut CONNECT_SSID: [u8; 32] = [0; 32];
static mut CONNECT_SSID_LEN: usize = 0;
static mut CONNECT_AP: usize = AP_NONE;
static mut PASS: [u8; 64] = [0; 64];
static mut PASS_LEN: usize = 0;
static mut PMK: [u8; 32] = [0; 32];
static mut STA_PTK: [u8; 64] = [0; 64];
static mut STA_GTK: [u8; 16] = [0; 16];
static mut ANONCE: [u8; 32] = [0; 32];
static mut SNONCE: [u8; 32] = [0; 32];
static mut STA_PN: u32 = 0;
static mut AP_PN: u32 = 0;
static mut AP_CUR: usize = AP_NONE;
static mut AP_ASSOCED: bool = false;
static mut AP_HS_OK: bool = false;
static mut AP_PTK: [u8; 64] = [0; 64];
static mut AP_GTK: [u8; 16] = [0; 16];
static mut AP_ANONCE: [u8; 32] = [0; 32];
static mut AP_SNONCE: [u8; 32] = [0; 32];
static mut SEQ11: u16 = 0;
static mut RX_RSSI: i8 = 0;

fn ticks() -> u64 {
    TICKS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Placeholder for future real Wi-Fi hardware: QEMU exposes no controller
/// and no driver exists yet, so the probe always fails and the stack runs on
/// the simulated [`TestRadio`].
struct RealRadio;

impl RealRadio {
    fn probe() -> Result<(), &'static str> {
        Err("no controller")
    }
}

/// Runs the crypto self-test, probes for real hardware (none today) and
/// arms the test radio. Must run before [crate::net] starts polling.
pub fn init() -> Result<(), &'static str> {
    selftest()?;
    match RealRadio::probe() {
        Ok(()) => crate::kprintln!("[serial] [wifi] real radio found"),
        Err(e) => crate::kprintln!("[serial] [wifi] real radio: {} (using test radio)", e),
    }
    crate::kprintln!("[serial] [wifi] crypto self-test ok");
    crate::kprintln!("[serial] [wifi] radio: test-radio (sim), 3 APs");
    crate::vprintln!("WiFi: test radio (sim) ready");
    Ok(())
}

/// Advances the radio one tick: pumps the three queues, emits scan beacons
/// and times out scans/association attempts.
pub fn poll() {
    let now = ticks();
    unsafe {
        for _ in 0..8 {
            let mut frame = [0u8; MAX_FRAME];
            match (*addr_of_mut!(TXQ)).pop(&mut frame) {
                Some(n) => ap_rx(&frame[..n]),
                None => break,
            }
        }
        if *addr_of!(WSTATE) == WifiState::Scanning && now >= *addr_of!(BEACON_AT) {
            beacon_burst();
            BEACON_AT = now + BEACON_TICKS;
        }
        for _ in 0..8 {
            let mut frame = [0u8; MAX_FRAME];
            match (*addr_of_mut!(RXQ)).pop(&mut frame) {
                Some(n) => sta_rx(&frame[..n]),
                None => break,
            }
        }
        if *addr_of!(WSTATE) == WifiState::Scanning && now >= *addr_of!(SCAN_END) {
            let n = *addr_of!(RESULTS_N);
            WSTATE = WifiState::Idle;
            crate::kprintln!("[serial] [wifi] scan done ({} networks)", n);
        }
        if *addr_of!(WSTATE) == WifiState::Connecting
            && now.wrapping_sub(*addr_of!(CONNECT_AT)) > CONNECT_TICKS
        {
            WSTATE = WifiState::Idle;
            STAGE = Stage::None;
            AP_CUR = AP_NONE;
            crate::kprintln!("[serial] [wifi] connect timeout");
        }
    }
}

/// Current radio state.
pub fn state() -> WifiState {
    unsafe { *addr_of!(WSTATE) }
}

/// `true` when the station is associated (handshake complete for WPA2).
pub fn associated() -> bool {
    state() == WifiState::Associated
}

/// `true` while an association/handshake is running.
pub fn connecting() -> bool {
    state() == WifiState::Connecting
}

/// `true` while a probe scan is running.
pub fn scanning() -> bool {
    state() == WifiState::Scanning
}

/// Station MAC address (`02:00:00:00:00:AA`).
pub fn mac() -> [u8; 6] {
    STATION_MAC
}

/// Signal of the associated AP in dBm (0 when idle).
pub fn rssi() -> i8 {
    unsafe { *addr_of!(RX_RSSI) }
}

/// Queues one Ethernet frame from [crate::net] for transmission. Fails
/// while not associated or when the air queue is full.
pub fn send_frame(eth: &[u8]) -> Result<(), &'static str> {
    if state() != WifiState::Associated {
        return Err("wifi: not associated");
    }
    if eth.len() < 14 || eth.len() > 1514 {
        return Err("wifi: bad frame length");
    }
    let ap = unsafe { *addr_of!(CONNECT_AP) };
    if ap == AP_NONE {
        return Err("wifi: no ap");
    }
    let mut dst = [0u8; 6];
    dst.copy_from_slice(&eth[0..6]);
    let et = u16::from_be_bytes([eth[12], eth[13]]);
    let (frame, n) = sta_data_frame(dst, et, &eth[14..], APS[ap].secure)?;
    unsafe { (*addr_of_mut!(TXQ)).push(&frame[..n]) }
}

/// `true` when at least one decrypted inbound frame waits for [crate::net].
pub fn has_frame() -> bool {
    unsafe { (*addr_of!(ETHQ)).busy() }
}

/// Pops one decrypted inbound Ethernet frame for [crate::net].
pub fn poll_frame(out: &mut [u8]) -> Option<usize> {
    unsafe { (*addr_of_mut!(ETHQ)).pop(out) }
}

/// Starts a wildcard probe scan; results appear after ~500 ms.
pub fn start_scan() {
    unsafe {
        if *addr_of!(WSTATE) == WifiState::Scanning {
            return;
        }
        RESULTS_N = 0;
        WSTATE = WifiState::Scanning;
        let now = ticks();
        SCAN_END = now + SCAN_TICKS;
        BEACON_AT = now;
        let mut probe = [0u8; 40];
        let n = build_probe_req(&mut probe, &[]);
        let _ = (*addr_of_mut!(TXQ)).push(&probe[..n]);
        crate::kprintln!("[serial] [wifi] scan start");
    }
}

/// Copies the latest scan results into the caller's slot; returns the count.
pub fn scan_results() -> ([ScanResult; 8], usize) {
    unsafe { (*addr_of!(RESULTS), *addr_of!(RESULTS_N)) }
}

/// Starts association to `ssid` with `pass` (WPA2 passphrase; ignored for
/// open networks). PMK derivation blocks for a few milliseconds.
pub fn connect(ssid: &[u8], pass: &[u8]) -> Result<(), &'static str> {
    if ssid.is_empty() || ssid.len() > 32 {
        return Err("wifi: bad ssid");
    }
    if pass.len() > 63 {
        return Err("wifi: pass too long");
    }
    let ap = APS
        .iter()
        .position(|a| a.ssid.len() == ssid.len() && a.ssid == ssid)
        .ok_or("wifi: no such network")?;
    unsafe {
        CONNECT_SSID[..ssid.len()].copy_from_slice(ssid);
        CONNECT_SSID_LEN = ssid.len();
        PASS[..pass.len()].copy_from_slice(pass);
        PASS_LEN = pass.len();
        CONNECT_AP = ap;
        PMK = [0; 32];
        pbkdf2(pass, ssid, 4096, &mut *addr_of_mut!(PMK));
        STAGE = Stage::WaitAuth;
        WSTATE = WifiState::Connecting;
        CONNECT_AT = ticks();
        let mut auth = [0u8; 40];
        let n = build_auth_req(&mut auth, APS[ap].bssid);
        let _ = (*addr_of_mut!(TXQ)).push(&auth[..n]);
    }
    crate::kprintln!(
        "[serial] [wifi] connect {}{}",
        ascii_of(ssid),
        if APS[ap].secure { " (wpa2)" } else { " (open)" }
    );
    Ok(())
}

/// Tears down the association (sends deauth) and returns to idle.
pub fn disconnect() {
    unsafe {
        if *addr_of!(WSTATE) == WifiState::Associated && *addr_of!(CONNECT_AP) != AP_NONE {
            let mut deauth = [0u8; 32];
            let n = build_deauth(&mut deauth, APS[*addr_of!(CONNECT_AP)].bssid);
            let _ = (*addr_of_mut!(TXQ)).push(&deauth[..n]);
            AP_CUR = AP_NONE;
            AP_ASSOCED = false;
            AP_HS_OK = false;
        }
        WSTATE = WifiState::Idle;
        STAGE = Stage::None;
    }
    crate::kprintln!("[serial] [wifi] disconnect");
}

/// Writes a short status string (`"assoc SecureNet -42dB sim"`); returns
/// the length written.
pub fn status_line(dst: &mut [u8]) -> usize {
    let mut o = 0;
    match state() {
        WifiState::Idle => put(dst, &mut o, b"idle (sim)"),
        WifiState::Scanning => put(dst, &mut o, b"scanning..."),
        WifiState::Connecting => {
            put(dst, &mut o, b"connecting ");
            unsafe {
                put(dst, &mut o, &CONNECT_SSID[..CONNECT_SSID_LEN]);
            }
        }
        WifiState::Associated => {
            put(dst, &mut o, b"assoc ");
            unsafe {
                put(dst, &mut o, &CONNECT_SSID[..CONNECT_SSID_LEN]);
            }
            let mut rssi_s = [0u8; 6];
            let n = fmt_rssi(rssi(), &mut rssi_s);
            put(dst, &mut o, &rssi_s[..n]);
            put(dst, &mut o, b"dB sim");
        }
    }
    o
}

/// Writes the associated SSID; returns the length written.
pub fn connected_ssid(dst: &mut [u8]) -> usize {
    let mut o = 0;
    unsafe {
        put(dst, &mut o, &CONNECT_SSID[..CONNECT_SSID_LEN]);
    }
    o
}

fn put(dst: &mut [u8], o: &mut usize, bytes: &[u8]) {
    for b in bytes {
        if *o < dst.len() {
            dst[*o] = *b;
            *o += 1;
        }
    }
}

fn fmt_rssi(v: i8, dst: &mut [u8]) -> usize {
    let mut o = 0;
    if v < 0 && o < dst.len() {
        dst[o] = b'-';
        o += 1;
    }
    let mut n = (v.unsigned_abs()) as u32;
    let mut digits = [0u8; 3];
    let mut d = 0;
    loop {
        digits[d] = b'0' + (n % 10) as u8;
        d += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    while d > 0 {
        d -= 1;
        if o < dst.len() {
            dst[o] = digits[d];
            o += 1;
        }
    }
    o
}

fn ascii_of(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

static mut AP_PMK: [u8; 32] = [0; 32];

const BASIC_RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];

fn next_seq() -> u16 {
    unsafe {
        let v = SEQ11.wrapping_add(1);
        SEQ11 = v;
        v
    }
}

fn write_hdr(f: &mut [u8], fc: u16, a1: [u8; 6], a2: [u8; 6], a3: [u8; 6]) -> u16 {
    let seqctl = next_seq() << 4;
    f[0..2].copy_from_slice(&fc.to_le_bytes());
    f[2..4].copy_from_slice(&0u16.to_le_bytes());
    f[4..10].copy_from_slice(&a1);
    f[10..16].copy_from_slice(&a2);
    f[16..22].copy_from_slice(&a3);
    f[22..24].copy_from_slice(&seqctl.to_le_bytes());
    seqctl
}

fn fill_ap_body(f: &mut [u8], mut p: usize, ssid: &[u8], channel: u8, secure: bool) -> usize {
    f[p..p + 8].copy_from_slice(&0u64.to_le_bytes());
    p += 8;
    f[p..p + 2].copy_from_slice(&100u16.to_le_bytes());
    p += 2;
    let capab: u16 = if secure { 0x0111 } else { 0x0001 };
    f[p..p + 2].copy_from_slice(&capab.to_le_bytes());
    p += 2;
    f[p] = 0;
    p += 1;
    f[p] = ssid.len() as u8;
    p += 1;
    f[p..p + ssid.len()].copy_from_slice(ssid);
    p += ssid.len();
    f[p] = 1;
    p += 1;
    f[p] = 8;
    p += 1;
    f[p..p + 8].copy_from_slice(&BASIC_RATES);
    p += 8;
    f[p] = 3;
    p += 1;
    f[p] = 1;
    p += 1;
    f[p] = channel;
    p += 1;
    if secure {
        f[p] = 48;
        p += 1;
        f[p] = 18;
        p += 1;
        f[p..p + 2].copy_from_slice(&1u16.to_le_bytes());
        p += 2;
        f[p..p + 4].copy_from_slice(&[0x00, 0x0f, 0xac, 0x04]);
        p += 4;
        f[p] = 1;
        p += 1;
        f[p..p + 4].copy_from_slice(&[0x00, 0x0f, 0xac, 0x04]);
        p += 4;
        f[p] = 1;
        p += 1;
        f[p..p + 4].copy_from_slice(&[0x00, 0x0f, 0xac, 0x02]);
        p += 4;
        f[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
        p += 2;
    }
    p
}

fn build_probe_req(f: &mut [u8], ssid: &[u8]) -> usize {
    write_hdr(f, FC_PROBE_REQ, BCAST_MAC, STATION_MAC, BCAST_MAC);
    let p = MGMT_HDR;
    f[p] = 0;
    f[p + 1] = ssid.len() as u8;
    f[p + 2..p + 2 + ssid.len()].copy_from_slice(ssid);
    p + 2 + ssid.len()
}

fn build_probe_resp(f: &mut [u8], a1: [u8; 6], ap: &ApInfo) -> usize {
    write_hdr(f, FC_PROBE_RESP, a1, ap.bssid, ap.bssid);
    fill_ap_body(f, MGMT_HDR, ap.ssid, ap.channel, ap.secure)
}

fn build_auth_req(f: &mut [u8], bssid: [u8; 6]) -> usize {
    write_hdr(f, FC_AUTH, bssid, STATION_MAC, bssid);
    let p = MGMT_HDR;
    f[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
    f[p + 2..p + 4].copy_from_slice(&1u16.to_le_bytes());
    f[p + 4..p + 6].copy_from_slice(&0u16.to_le_bytes());
    p + 6
}

fn build_auth_resp(f: &mut [u8], a1: [u8; 6], bssid: [u8; 6]) -> usize {
    write_hdr(f, FC_AUTH, a1, bssid, bssid);
    let p = MGMT_HDR;
    f[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
    f[p + 2..p + 4].copy_from_slice(&2u16.to_le_bytes());
    f[p + 4..p + 6].copy_from_slice(&0u16.to_le_bytes());
    p + 6
}

fn build_assoc_req(f: &mut [u8], bssid: [u8; 6], ssid: &[u8], secure: bool) -> usize {
    write_hdr(f, FC_ASSOC_REQ, bssid, STATION_MAC, bssid);
    let mut p = MGMT_HDR;
    let capab: u16 = if secure { 0x0111 } else { 0x0001 };
    f[p..p + 2].copy_from_slice(&capab.to_le_bytes());
    p += 2;
    f[p..p + 2].copy_from_slice(&10u16.to_le_bytes());
    p += 2;
    f[p] = 0;
    p += 1;
    f[p] = ssid.len() as u8;
    p += 1;
    f[p..p + ssid.len()].copy_from_slice(ssid);
    p += ssid.len();
    f[p] = 1;
    p += 1;
    f[p] = 8;
    p += 1;
    f[p..p + 8].copy_from_slice(&BASIC_RATES);
    p += 8;
    p
}

fn build_assoc_resp(f: &mut [u8], a1: [u8; 6], bssid: [u8; 6], secure: bool) -> usize {
    write_hdr(f, FC_ASSOC_RESP, a1, bssid, bssid);
    let mut p = MGMT_HDR;
    let capab: u16 = if secure { 0x0111 } else { 0x0001 };
    f[p..p + 2].copy_from_slice(&capab.to_le_bytes());
    p += 2;
    f[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
    p += 2;
    f[p..p + 2].copy_from_slice(&1u16.to_le_bytes());
    p += 2;
    f[p] = 1;
    p += 1;
    f[p] = 8;
    p += 1;
    f[p..p + 8].copy_from_slice(&BASIC_RATES);
    p += 8;
    p
}

fn build_deauth(f: &mut [u8], bssid: [u8; 6]) -> usize {
    write_hdr(f, FC_DEAUTH, STATION_MAC, bssid, bssid);
    let p = MGMT_HDR;
    f[p..p + 2].copy_from_slice(&1u16.to_le_bytes());
    p + 2
}

fn beacon_burst() {
    unsafe {
        for ap in APS.iter() {
            let mut f = [0u8; 128];
            write_hdr(&mut f, FC_BEACON, BCAST_MAC, ap.bssid, ap.bssid);
            let end = fill_ap_body(&mut f, MGMT_HDR, ap.ssid, ap.channel, ap.secure);
            let _ = (*addr_of_mut!(RXQ)).push(&f[..end]);
        }
    }
}

fn snap_plain(plain: &mut [u8], ethertype: u16, payload: &[u8]) -> Result<usize, &'static str> {
    if 8 + payload.len() > plain.len() {
        return Err("wifi: payload too large");
    }
    plain[0..6].copy_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00]);
    plain[6..8].copy_from_slice(&ethertype.to_be_bytes());
    plain[8..8 + payload.len()].copy_from_slice(payload);
    Ok(8 + payload.len())
}

fn ccmp_aad(fc: u16, a1: [u8; 6], a2: [u8; 6], a3: [u8; 6], seqctl: u16, out: &mut [u8]) {
    let fc_aad = (fc & (0x0100 | 0x0200 | 0x0400 | 0x4000)) | 0x0008;
    out[0..2].copy_from_slice(&fc_aad.to_be_bytes());
    out[2..8].copy_from_slice(&a1);
    out[8..14].copy_from_slice(&a2);
    out[14..20].copy_from_slice(&a3);
    out[20..22].copy_from_slice(&(seqctl & 0xFFF0).to_le_bytes());
}

fn ccmp_nonce(sender: [u8; 6], pn: u64, out: &mut [u8]) {
    out[0] = QOS_PRIORITY;
    out[1..7].copy_from_slice(&sender);
    for i in 0..6 {
        out[7 + i] = ((pn >> (40 - 8 * i)) & 0xFF) as u8;
    }
}

fn sta_data_frame(
    a3: [u8; 6],
    ethertype: u16,
    payload: &[u8],
    protect: bool,
) -> Result<([u8; MAX_FRAME], usize), &'static str> {
    let ap = unsafe { *addr_of!(CONNECT_AP) };
    if ap == AP_NONE {
        return Err("wifi: no ap");
    }
    let bssid = APS[ap].bssid;
    let mut f = [0u8; MAX_FRAME];
    let fc = FC_DATA_TODS | if protect { FC_PROTECTED } else { 0 };
    let seqctl = write_hdr(&mut f, fc, bssid, STATION_MAC, a3);
    let mut plain = [0u8; 1514];
    let plen = snap_plain(&mut plain, ethertype, payload)?;
    let mut o = MGMT_HDR;
    if protect {
        unsafe {
            STA_PN += 1;
        }
        let pn = unsafe { *addr_of!(STA_PN) };
        if o + 8 + plen + 8 > MAX_FRAME {
            return Err("wifi: frame too large");
        }
        f[o] = (pn & 0xFF) as u8;
        f[o + 1] = ((pn >> 8) & 0xFF) as u8;
        f[o + 2] = 0;
        f[o + 3] = 0x20;
        f[o + 4] = ((pn >> 16) & 0xFF) as u8;
        f[o + 5] = ((pn >> 24) & 0xFF) as u8;
        f[o + 6] = 0;
        f[o + 7] = 0;
        let mut nonce = [0u8; 13];
        ccmp_nonce(STATION_MAC, pn as u64, &mut nonce);
        let mut aad = [0u8; 22];
        ccmp_aad(fc, bssid, STATION_MAC, a3, seqctl, &mut aad);
        let ptk = unsafe { *addr_of!(STA_PTK) };
        let mut tk = [0u8; 16];
        tk.copy_from_slice(&ptk[32..48]);
        ccm_encrypt(&tk, &nonce, &aad, &plain[..plen], &mut f[o + 8..]);
        o += 8 + plen + 8;
    } else {
        if o + plen > MAX_FRAME {
            return Err("wifi: frame too large");
        }
        f[o..o + plen].copy_from_slice(&plain[..plen]);
        o += plen;
    }
    Ok((f, o))
}

fn ap_data_frame(
    a1: [u8; 6],
    ethertype: u16,
    payload: &[u8],
    protect: bool,
) -> Result<([u8; MAX_FRAME], usize), &'static str> {
    let ap = unsafe { *addr_of!(AP_CUR) };
    if ap == AP_NONE {
        return Err("wifi: no ap");
    }
    let bssid = APS[ap].bssid;
    let mut f = [0u8; MAX_FRAME];
    let fc = FC_DATA_FROMDS | if protect { FC_PROTECTED } else { 0 };
    let seqctl = write_hdr(&mut f, fc, a1, bssid, bssid);
    let mut plain = [0u8; 1514];
    let plen = snap_plain(&mut plain, ethertype, payload)?;
    let mut o = MGMT_HDR;
    if protect {
        unsafe {
            AP_PN += 1;
        }
        let pn = unsafe { *addr_of!(AP_PN) };
        if o + 8 + plen + 8 > MAX_FRAME {
            return Err("wifi: frame too large");
        }
        f[o] = (pn & 0xFF) as u8;
        f[o + 1] = ((pn >> 8) & 0xFF) as u8;
        f[o + 2] = 0;
        f[o + 3] = 0x20;
        f[o + 4] = ((pn >> 16) & 0xFF) as u8;
        f[o + 5] = ((pn >> 24) & 0xFF) as u8;
        f[o + 6] = 0;
        f[o + 7] = 0;
        let mut nonce = [0u8; 13];
        ccmp_nonce(bssid, pn as u64, &mut nonce);
        let mut aad = [0u8; 22];
        ccmp_aad(fc, a1, bssid, bssid, seqctl, &mut aad);
        let ptk = unsafe { *addr_of!(AP_PTK) };
        let mut tk = [0u8; 16];
        tk.copy_from_slice(&ptk[32..48]);
        ccm_encrypt(&tk, &nonce, &aad, &plain[..plen], &mut f[o + 8..]);
        o += 8 + plen + 8;
    } else {
        if o + plen > MAX_FRAME {
            return Err("wifi: frame too large");
        }
        f[o..o + plen].copy_from_slice(&plain[..plen]);
        o += plen;
    }
    Ok((f, o))
}

fn plain_to_eth(plain: &[u8], dst: [u8; 6], src: [u8; 6], et: u16, out: &mut [u8]) -> usize {
    if plain.len() < 8 || 14 + plain.len() - 8 > out.len() {
        return 0;
    }
    out[0..6].copy_from_slice(&dst);
    out[6..12].copy_from_slice(&src);
    out[12..14].copy_from_slice(&et.to_be_bytes());
    out[14..14 + plain.len() - 8].copy_from_slice(&plain[8..]);
    14 + plain.len() - 8
}

fn parse_ssid_ie(ies: &[u8]) -> Option<([u8; 32], usize)> {
    let mut i = 0;
    while i + 2 <= ies.len() {
        let id = ies[i];
        let l = ies[i + 1] as usize;
        i += 2;
        if i + l > ies.len() {
            break;
        }
        if id == 0 {
            let n = l.min(32);
            let mut s = [0u8; 32];
            s[..n].copy_from_slice(&ies[i..i + n]);
            return Some((s, n));
        }
        i += l;
    }
    None
}

fn derive_ptk(
    pmk: &[u8; 32],
    aa: [u8; 6],
    sa: [u8; 6],
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> [u8; 64] {
    let (min_a, max_a): (&[u8], &[u8]) = if aa[..] <= sa[..] {
        (&aa[..], &sa[..])
    } else {
        (&sa[..], &aa[..])
    };
    let (min_n, max_n): (&[u8], &[u8]) = if anonce[..] <= snonce[..] {
        (&anonce[..], &snonce[..])
    } else {
        (&snonce[..], &anonce[..])
    };
    prf512(
        pmk,
        b"Pairwise key expansion",
        &[min_a, max_a, min_n, max_n],
    )
}

fn fill_nonce(out: &mut [u8; 32]) {
    let t = ticks();
    for (i, b) in out.iter_mut().enumerate() {
        *b = (t as u8)
            .wrapping_add((i as u8).wrapping_mul(31))
            .wrapping_add(0xA5);
    }
}

fn build_eapol_key(
    out: &mut [u8],
    key_info: u16,
    replay: u64,
    nonce: &[u8; 32],
    data: &[u8],
) -> usize {
    out[0] = 1;
    out[1] = 3;
    out[2..4].copy_from_slice(&((95 + data.len()) as u16).to_be_bytes());
    out[4] = 2;
    out[5..7].copy_from_slice(&key_info.to_be_bytes());
    out[7..9].copy_from_slice(&16u16.to_be_bytes());
    out[9..17].copy_from_slice(&replay.to_be_bytes());
    out[17..49].copy_from_slice(nonce);
    out[49..65].fill(0);
    out[65..73].fill(0);
    out[73..81].fill(0);
    out[81..97].fill(0);
    out[97..99].copy_from_slice(&(data.len() as u16).to_be_bytes());
    out[99..99 + data.len()].copy_from_slice(data);
    99 + data.len()
}

fn eapol_read_mic(eapol: &[u8]) -> [u8; 16] {
    let mut m = [0u8; 16];
    m.copy_from_slice(&eapol[81..97]);
    m
}

fn eapol_sign(kck: &[u8; 16], eapol: &mut [u8], total: usize) -> [u8; 16] {
    let saved = eapol_read_mic(eapol);
    eapol[81..97].fill(0);
    let mic = hmac_sha1(kck, &[&eapol[..total]]);
    eapol[81..97].copy_from_slice(&mic[..16]);
    saved
}

fn eapol_mic_ok(kck: &[u8; 16], eapol: &mut [u8], total: usize) -> bool {
    let saved = eapol_sign(kck, eapol, total);
    let now = eapol_read_mic(eapol);
    saved == now
}

fn sta_rx(frame: &[u8]) {
    if frame.len() < MGMT_HDR {
        return;
    }
    let fc = u16::from_le_bytes([frame[0], frame[1]]);
    match (fc >> 2) & 0x3 {
        0 => sta_rx_mgmt(fc, frame),
        2 => sta_rx_data(fc, frame),
        _ => {}
    }
}

fn sta_rx_mgmt(fc: u16, frame: &[u8]) {
    match fc {
        FC_BEACON | FC_PROBE_RESP => sta_rx_scan(frame),
        FC_AUTH => sta_rx_auth(frame),
        FC_ASSOC_RESP => sta_rx_assoc(frame),
        FC_DEAUTH => unsafe {
            if *addr_of!(WSTATE) == WifiState::Associated {
                WSTATE = WifiState::Idle;
                STAGE = Stage::None;
                AP_CUR = AP_NONE;
                crate::kprintln!("[serial] [wifi] deauth recv");
            }
        },
        _ => {}
    }
}

fn sta_rx_scan(frame: &[u8]) {
    if frame.len() < 38 {
        return;
    }
    let mut p = 36;
    let mut ssid = [0u8; 32];
    let mut ssid_len = 0usize;
    let mut channel = 0u8;
    let mut secure = false;
    while p + 2 <= frame.len() {
        let id = frame[p];
        let l = frame[p + 1] as usize;
        p += 2;
        if p + l > frame.len() {
            break;
        }
        match id {
            0 if ssid_len == 0 => {
                ssid_len = l.min(32);
                ssid[..ssid_len].copy_from_slice(&frame[p..p + ssid_len]);
            }
            3 if l == 1 => channel = frame[p],
            48 => secure = true,
            _ => {}
        }
        p += l;
    }
    if ssid_len == 0 {
        return;
    }
    unsafe {
        if *addr_of!(WSTATE) != WifiState::Scanning {
            return;
        }
        let mut rssi: i8 = -50;
        for ap in APS.iter() {
            if ap.bssid[..] == frame[10..16] {
                rssi = ap.rssi;
            }
        }
        let n = *addr_of!(RESULTS_N);
        let results = &*core::ptr::addr_of!(RESULTS);
        for r in results.iter().take(n) {
            if r.ssid_len as usize == ssid_len && r.ssid[..] == ssid[..] {
                return;
            }
        }
        if n < 8 {
            let r = &mut RESULTS[n];
            r.ssid = ssid;
            r.ssid_len = ssid_len as u8;
            r.rssi = rssi;
            r.security = if secure {
                Security::Wpa2
            } else {
                Security::Open
            };
            r.channel = channel;
            RESULTS_N = n + 1;
        }
    }
}

fn sta_rx_auth(frame: &[u8]) {
    if frame.len() < 30 {
        return;
    }
    let alg = u16::from_le_bytes([frame[24], frame[25]]);
    let seq = u16::from_le_bytes([frame[26], frame[27]]);
    let status = u16::from_le_bytes([frame[28], frame[29]]);
    unsafe {
        if *addr_of!(STAGE) != Stage::WaitAuth {
            return;
        }
        if alg != 0 || seq != 2 || status != 0 {
            WSTATE = WifiState::Idle;
            STAGE = Stage::None;
            AP_CUR = AP_NONE;
            crate::kprintln!("[serial] [wifi] auth rejected ({})", status);
            return;
        }
        let ap = *addr_of!(CONNECT_AP);
        let mut f = [0u8; 64];
        let n = build_assoc_req(
            &mut f,
            APS[ap].bssid,
            &CONNECT_SSID[..CONNECT_SSID_LEN],
            APS[ap].secure,
        );
        let _ = (*addr_of_mut!(TXQ)).push(&f[..n]);
        STAGE = Stage::WaitAssoc;
        crate::kprintln!("[serial] [wifi] auth ok, associating");
    }
}

fn sta_rx_assoc(frame: &[u8]) {
    if frame.len() < 30 {
        return;
    }
    let status = u16::from_le_bytes([frame[26], frame[27]]);
    unsafe {
        if *addr_of!(STAGE) != Stage::WaitAssoc {
            return;
        }
        if status != 0 {
            WSTATE = WifiState::Idle;
            STAGE = Stage::None;
            AP_CUR = AP_NONE;
            crate::kprintln!("[serial] [wifi] assoc rejected ({})", status);
            return;
        }
        let ap = *addr_of!(CONNECT_AP);
        RX_RSSI = APS[ap].rssi;
        let rssi = RX_RSSI;
        if !APS[ap].secure {
            WSTATE = WifiState::Associated;
            STAGE = Stage::None;
            crate::kprintln!(
                "[serial] [wifi] assoc {} rssi {}dB (sim)",
                ascii_of(&CONNECT_SSID[..CONNECT_SSID_LEN]),
                rssi
            );
        } else {
            STAGE = Stage::WaitM1;
            crate::kprintln!("[serial] [wifi] assoc ok, waiting handshake");
        }
    }
}

fn sta_rx_data(fc: u16, frame: &[u8]) {
    if frame.len() < MGMT_HDR {
        return;
    }
    if fc & 0x0200 == 0 {
        return;
    }
    let mut a1 = [0u8; 6];
    a1.copy_from_slice(&frame[4..10]);
    let mut a2 = [0u8; 6];
    a2.copy_from_slice(&frame[10..16]);
    let mut a3 = [0u8; 6];
    a3.copy_from_slice(&frame[16..22]);
    let seqctl = u16::from_le_bytes([frame[22], frame[23]]);
    let protected = (fc & FC_PROTECTED) != 0;
    let body = &frame[MGMT_HDR..];
    let mut plain = [0u8; 1514];
    let plen;
    if protected {
        if body.len() < CCMP_HDR + MIC_LEN {
            return;
        }
        let pn = body[0] as u64
            | (body[1] as u64) << 8
            | (body[4] as u64) << 16
            | (body[5] as u64) << 24
            | (body[6] as u64) << 32
            | (body[7] as u64) << 40;
        let mut nonce = [0u8; 13];
        ccmp_nonce(a2, pn, &mut nonce);
        let mut aad = [0u8; 22];
        ccmp_aad(fc, a1, a2, a3, seqctl, &mut aad);
        let mut work = [0u8; 1514];
        let rest = &body[CCMP_HDR..];
        if rest.len() > work.len() {
            return;
        }
        work[..rest.len()].copy_from_slice(rest);
        let key = unsafe {
            if a1 == STATION_MAC {
                let ptk = *addr_of!(STA_PTK);
                let mut tk = [0u8; 16];
                tk.copy_from_slice(&ptk[32..48]);
                tk
            } else if a1 == BCAST_MAC {
                let gtk = *addr_of!(STA_GTK);
                if gtk == [0u8; 16] {
                    return;
                }
                gtk
            } else {
                return;
            }
        };
        if ccm_decrypt(&key, &nonce, &aad, &mut work[..rest.len()]).is_none() {
            crate::kprintln!("[serial] [wifi] ccmp decrypt failed");
            return;
        }
        plen = rest.len() - MIC_LEN;
        plain[..plen].copy_from_slice(&work[..plen]);
    } else {
        if body.len() > plain.len() {
            return;
        }
        plen = body.len();
        plain[..plen].copy_from_slice(body);
    }
    if plen < 8 {
        return;
    }
    let et = u16::from_be_bytes([plain[6], plain[7]]);
    let mut eth = [0u8; 1514];
    let elen = plain_to_eth(&plain[..plen], a1, a3, et, &mut eth);
    if elen == 0 {
        return;
    }
    if et == ETHERTYPE_EAPOL {
        sta_eapol(&mut eth[..elen]);
    } else {
        unsafe {
            let _ = (*addr_of_mut!(ETHQ)).push(&eth[..elen]);
        }
    }
}

fn sta_eapol(eth: &mut [u8]) {
    if eth.len() < 14 + 99 {
        return;
    }
    if u16::from_be_bytes([eth[12], eth[13]]) != ETHERTYPE_EAPOL {
        return;
    }
    let ap = unsafe { *addr_of!(CONNECT_AP) };
    if ap == AP_NONE {
        return;
    }
    if eth[6..12] != APS[ap].bssid[..] {
        return;
    }
    let e = &mut eth[14..];
    if e[1] != 3 {
        return;
    }
    let ki = u16::from_be_bytes([e[5], e[6]]);
    let replay = u64::from_be_bytes([e[9], e[10], e[11], e[12], e[13], e[14], e[15], e[16]]);
    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&e[17..49]);
    let data_len = u16::from_be_bytes([e[97], e[98]]) as usize;
    if 99 + data_len > e.len() {
        return;
    }
    let total = 99 + data_len;
    unsafe {
        let st = *addr_of!(STAGE);
        match (ki, st) {
            (KEY_INFO_M1, Stage::WaitM1) => {
                if data_len != 0 {
                    return;
                }
                ANONCE = nonce;
                let mut sn = [0u8; 32];
                fill_nonce(&mut sn);
                SNONCE = sn;
                let anonce = *addr_of!(ANONCE);
                let snonce = *addr_of!(SNONCE);
                let pmk = *addr_of!(PMK);
                STA_PTK = derive_ptk(&pmk, APS[ap].bssid, STATION_MAC, &anonce, &snonce);
                let ptk = *addr_of!(STA_PTK);
                let mut kck = [0u8; 16];
                kck.copy_from_slice(&ptk[0..16]);
                let mut m2 = [0u8; 300];
                let total2 = build_eapol_key(&mut m2, KEY_INFO_M2, replay, &snonce, &[]);
                eapol_sign(&kck, &mut m2, total2);
                if let Ok((f, n)) =
                    sta_data_frame(APS[ap].bssid, ETHERTYPE_EAPOL, &m2[..total2], false)
                {
                    let _ = (*addr_of_mut!(TXQ)).push(&f[..n]);
                }
                STAGE = Stage::WaitM3;
                crate::kprintln!("[serial] [wifi] eapol M1 recv, M2 send");
            }
            (KEY_INFO_M3, Stage::WaitM3) => {
                if data_len != 24 {
                    return;
                }
                let ptk = *addr_of!(STA_PTK);
                let snonce = *addr_of!(SNONCE);
                let mut kck = [0u8; 16];
                kck.copy_from_slice(&ptk[0..16]);
                let mut kek = [0u8; 16];
                kek.copy_from_slice(&ptk[16..32]);
                if !eapol_mic_ok(&kck, e, total) {
                    crate::kprintln!("[serial] [wifi] M3 MIC failed");
                    return;
                }
                let mut ct = [0u8; 24];
                ct.copy_from_slice(&e[99..123]);
                match aes_key_unwrap(&kek, &ct) {
                    Some(gtk) => STA_GTK = gtk,
                    None => {
                        crate::kprintln!("[serial] [wifi] gtk unwrap failed");
                        return;
                    }
                }
                let mut m4 = [0u8; 300];
                let total4 = build_eapol_key(&mut m4, KEY_INFO_M4, replay, &snonce, &[]);
                eapol_sign(&kck, &mut m4, total4);
                if let Ok((f, n)) =
                    sta_data_frame(APS[ap].bssid, ETHERTYPE_EAPOL, &m4[..total4], false)
                {
                    let _ = (*addr_of_mut!(TXQ)).push(&f[..n]);
                }
                WSTATE = WifiState::Associated;
                STAGE = Stage::None;
                let rssi = RX_RSSI;
                crate::kprintln!(
                    "[serial] [wifi] handshake complete, assoc {} rssi {}dB (sim)",
                    ascii_of(&CONNECT_SSID[..CONNECT_SSID_LEN]),
                    rssi
                );
            }
            _ => {}
        }
    }
}

fn ap_rx(frame: &[u8]) {
    if frame.len() < MGMT_HDR {
        return;
    }
    let fc = u16::from_le_bytes([frame[0], frame[1]]);
    match (fc >> 2) & 0x3 {
        0 => ap_rx_mgmt(fc, frame),
        2 => ap_rx_data(fc, frame),
        _ => {}
    }
}

fn ap_rx_mgmt(fc: u16, frame: &[u8]) {
    match fc {
        FC_PROBE_REQ => ap_rx_probe(frame),
        FC_AUTH => ap_rx_auth(frame),
        FC_ASSOC_REQ => ap_rx_assoc(frame),
        FC_DEAUTH => unsafe {
            AP_ASSOCED = false;
            AP_HS_OK = false;
        },
        _ => {}
    }
}

fn ap_rx_probe(frame: &[u8]) {
    if frame.len() < MGMT_HDR {
        return;
    }
    let mut sta = [0u8; 6];
    sta.copy_from_slice(&frame[10..16]);
    let wanted = parse_ssid_ie(&frame[MGMT_HDR..]);
    for ap in APS.iter() {
        if let Some((ssid, len)) = &wanted {
            if *len != 0 && (*len != ap.ssid.len() || ssid[..*len] != ap.ssid[..]) {
                continue;
            }
        }
        let mut f = [0u8; 160];
        let n = build_probe_resp(&mut f, sta, ap);
        unsafe {
            let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
        }
    }
}

fn ap_rx_auth(frame: &[u8]) {
    if frame.len() < 30 {
        return;
    }
    let mut a1 = [0u8; 6];
    a1.copy_from_slice(&frame[4..10]);
    let mut sta = [0u8; 6];
    sta.copy_from_slice(&frame[10..16]);
    let idx = APS.iter().position(|ap| ap.bssid == a1);
    if let Some(ap) = idx {
        unsafe {
            AP_CUR = ap;
            AP_ASSOCED = false;
            AP_HS_OK = false;
            AP_PTK = [0; 64];
            AP_GTK = [0; 16];
            let mut f = [0u8; 64];
            let n = build_auth_resp(&mut f, sta, APS[ap].bssid);
            let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
        }
        crate::kprintln!("[serial] [wifi] ap auth ok");
    }
}

fn ap_rx_assoc(frame: &[u8]) {
    if frame.len() < 34 {
        return;
    }
    let ap = unsafe { *addr_of!(AP_CUR) };
    if ap == AP_NONE {
        return;
    }
    let wanted = match parse_ssid_ie(&frame[MGMT_HDR + 4..]) {
        Some(w) => w,
        None => return,
    };
    let mut sta = [0u8; 6];
    sta.copy_from_slice(&frame[10..16]);
    let ssid_ok = wanted.1 == APS[ap].ssid.len() && wanted.0[..wanted.1] == APS[ap].ssid[..];
    if !ssid_ok {
        crate::kprintln!("[serial] [wifi] ap assoc ssid mismatch");
        return;
    }
    let secure = APS[ap].secure;
    unsafe {
        let mut f = [0u8; 64];
        let n = build_assoc_resp(&mut f, sta, APS[ap].bssid, secure);
        let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
        AP_ASSOCED = true;
    }
    if secure {
        let mut anonce = [0u8; 32];
        fill_nonce(&mut anonce);
        unsafe {
            AP_ANONCE = anonce;
            AP_PMK = [0; 32];
            pbkdf2(APS[ap].psk, APS[ap].ssid, 4096, &mut *addr_of_mut!(AP_PMK));
            let pmk = *addr_of!(AP_PMK);
            let anonce = *addr_of!(AP_ANONCE);
            AP_GTK = derive_gtk(&pmk, &APS[ap].bssid, &anonce);
            let mut m1 = [0u8; 300];
            let total = build_eapol_key(&mut m1, KEY_INFO_M1, 1, &anonce, &[]);
            if let Ok((f, n)) = ap_data_frame(sta, ETHERTYPE_EAPOL, &m1[..total], false) {
                let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
            }
        }
        crate::kprintln!("[serial] [wifi] ap assoc ok, eapol M1 send");
    } else {
        crate::kprintln!("[serial] [wifi] ap assoc ok");
    }
}

fn ap_rx_data(fc: u16, frame: &[u8]) {
    if frame.len() < MGMT_HDR {
        return;
    }
    if fc & 0x0100 == 0 {
        return;
    }
    let mut a1 = [0u8; 6];
    a1.copy_from_slice(&frame[4..10]);
    let mut a2 = [0u8; 6];
    a2.copy_from_slice(&frame[10..16]);
    let mut a3 = [0u8; 6];
    a3.copy_from_slice(&frame[16..22]);
    let seqctl = u16::from_le_bytes([frame[22], frame[23]]);
    let protected = (fc & FC_PROTECTED) != 0;
    let body = &frame[MGMT_HDR..];
    let mut plain = [0u8; 1514];
    let plen;
    let secure_ap = unsafe {
        let ap = *addr_of!(AP_CUR);
        ap != AP_NONE && APS[ap].secure
    };
    if protected {
        let hs_ok = unsafe { *addr_of!(AP_HS_OK) };
        if !hs_ok || body.len() < CCMP_HDR + MIC_LEN {
            return;
        }
        let pn = body[0] as u64
            | (body[1] as u64) << 8
            | (body[4] as u64) << 16
            | (body[5] as u64) << 24
            | (body[6] as u64) << 32
            | (body[7] as u64) << 40;
        let mut nonce = [0u8; 13];
        ccmp_nonce(a2, pn, &mut nonce);
        let mut aad = [0u8; 22];
        ccmp_aad(fc, a1, a2, a3, seqctl, &mut aad);
        let mut work = [0u8; 1514];
        let rest = &body[CCMP_HDR..];
        if rest.len() > work.len() {
            return;
        }
        work[..rest.len()].copy_from_slice(rest);
        let ptk = unsafe { *addr_of!(AP_PTK) };
        let mut tk = [0u8; 16];
        tk.copy_from_slice(&ptk[32..48]);
        if ccm_decrypt(&tk, &nonce, &aad, &mut work[..rest.len()]).is_none() {
            crate::kprintln!("[serial] [wifi] ap: ccmp decrypt failed");
            return;
        }
        plen = rest.len() - MIC_LEN;
        plain[..plen].copy_from_slice(&work[..plen]);
    } else {
        if body.len() > plain.len() {
            return;
        }
        plen = body.len();
        plain[..plen].copy_from_slice(body);
    }
    if plen < 8 {
        return;
    }
    let et = u16::from_be_bytes([plain[6], plain[7]]);
    let mut eth = [0u8; 1514];
    let elen = plain_to_eth(&plain[..plen], a3, a2, et, &mut eth);
    if elen == 0 {
        return;
    }
    if et == ETHERTYPE_EAPOL {
        ap_eapol(&eth[..elen]);
        return;
    }
    if !protected && secure_ap {
        crate::kprintln!("[serial] [wifi] ap: plain data dropped");
        return;
    }
    ap_lan(&eth[..elen]);
}

fn ap_eapol(eth: &[u8]) {
    if eth.len() < 14 + 99 {
        return;
    }
    if u16::from_be_bytes([eth[12], eth[13]]) != ETHERTYPE_EAPOL {
        return;
    }
    let ap = unsafe { *addr_of!(AP_CUR) };
    if ap == AP_NONE || !APS[ap].secure {
        return;
    }
    unsafe {
        if !*addr_of!(AP_ASSOCED) {
            return;
        }
    }
    let e = &eth[14..];
    if e[1] != 3 {
        return;
    }
    let ki = u16::from_be_bytes([e[5], e[6]]);
    let replay = u64::from_be_bytes([e[9], e[10], e[11], e[12], e[13], e[14], e[15], e[16]]);
    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&e[17..49]);
    let data_len = u16::from_be_bytes([e[97], e[98]]) as usize;
    if 99 + data_len > e.len() {
        return;
    }
    let total = 99 + data_len;
    unsafe {
        match ki {
            KEY_INFO_M2 => {
                if *addr_of!(AP_HS_OK) || data_len != 0 {
                    return;
                }
                let pmk = *addr_of!(AP_PMK);
                let anonce = *addr_of!(AP_ANONCE);
                let ptk = derive_ptk(&pmk, APS[ap].bssid, STATION_MAC, &anonce, &nonce);
                let mut kck = [0u8; 16];
                kck.copy_from_slice(&ptk[0..16]);
                let mut kek = [0u8; 16];
                kek.copy_from_slice(&ptk[16..32]);
                let mut eb = [0u8; 300];
                if total > eb.len() {
                    return;
                }
                eb[..total].copy_from_slice(&e[..total]);
                if !eapol_mic_ok(&kck, &mut eb, total) {
                    crate::kprintln!("[serial] [wifi] ap: M2 MIC failed (wrong psk?)");
                    return;
                }
                AP_PTK = ptk;
                AP_SNONCE = nonce;
                let gtk = *addr_of!(AP_GTK);
                let wrapped = aes_key_wrap(&kek, &gtk);
                let mut m3 = [0u8; 300];
                let total3 = build_eapol_key(&mut m3, KEY_INFO_M3, replay, &anonce, &wrapped);
                eapol_sign(&kck, &mut m3, total3);
                let sta = a2_of_eapol(eth);
                if let Ok((f, n)) = ap_data_frame(sta, ETHERTYPE_EAPOL, &m3[..total3], false) {
                    let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
                }
                crate::kprintln!("[serial] [wifi] eapol M3 send");
            }
            KEY_INFO_M4 => {
                if data_len != 0 {
                    return;
                }
                let ptk = *addr_of!(AP_PTK);
                if ptk == [0u8; 64] {
                    return;
                }
                let mut kck = [0u8; 16];
                kck.copy_from_slice(&ptk[0..16]);
                let mut eb = [0u8; 300];
                if total > eb.len() {
                    return;
                }
                eb[..total].copy_from_slice(&e[..total]);
                if !eapol_mic_ok(&kck, &mut eb, total) {
                    crate::kprintln!("[serial] [wifi] ap: M4 MIC failed");
                    return;
                }
                AP_HS_OK = true;
                crate::kprintln!("[serial] [wifi] ap handshake complete");
            }
            _ => {}
        }
    }
}

fn a2_of_eapol(eth: &[u8]) -> [u8; 6] {
    let mut m = [0u8; 6];
    m.copy_from_slice(&eth[6..12]);
    m
}

fn ap_lan(eth: &[u8]) {
    if eth.len() < 14 {
        return;
    }
    match u16::from_be_bytes([eth[12], eth[13]]) {
        ETHERTYPE_ARP => ap_lan_arp(eth),
        ETHERTYPE_IPV4 => ap_lan_ipv4(eth),
        _ => {}
    }
}

fn lan_protect() -> bool {
    unsafe {
        let ap = *addr_of!(AP_CUR);
        ap != AP_NONE && APS[ap].secure && *addr_of!(AP_HS_OK)
    }
}

fn ap_lan_arp(eth: &[u8]) {
    if eth.len() < 42 {
        return;
    }
    let ap = unsafe { *addr_of!(AP_CUR) };
    if ap == AP_NONE {
        return;
    }
    let bssid = APS[ap].bssid;
    let p = 14;
    if u16::from_be_bytes([eth[p], eth[p + 1]]) != 1 {
        return;
    }
    if eth[p + 4] != 6 || eth[p + 5] != 4 {
        return;
    }
    if u16::from_be_bytes([eth[p + 6], eth[p + 7]]) != 1 {
        return;
    }
    if eth[p + 24..p + 28] != AP_IP[..] {
        return;
    }
    let mut sta = [0u8; 6];
    sta.copy_from_slice(&eth[p + 8..p + 14]);
    let mut arp = [0u8; 28];
    arp[0..2].copy_from_slice(&1u16.to_be_bytes());
    arp[2..4].copy_from_slice(&0x0800u16.to_be_bytes());
    arp[4] = 6;
    arp[5] = 4;
    arp[6..8].copy_from_slice(&2u16.to_be_bytes());
    arp[8..14].copy_from_slice(&bssid);
    arp[14..18].copy_from_slice(&AP_IP);
    arp[18..24].copy_from_slice(&eth[p + 8..p + 14]);
    arp[24..28].copy_from_slice(&eth[p + 14..p + 18]);
    if let Ok((f, n)) = ap_data_frame(sta, ETHERTYPE_ARP, &arp, lan_protect()) {
        unsafe {
            let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
        }
    }
}

fn ap_lan_ipv4(eth: &[u8]) {
    if eth.len() < 34 {
        return;
    }
    let ip = &eth[14..];
    if ip[0] >> 4 != 4 {
        return;
    }
    let ihl = (ip[0] & 0xF) as usize * 4;
    if ihl < 20 || 14 + ihl + 8 > eth.len() {
        return;
    }
    match ip[9] {
        PROTO_UDP => ap_lan_udp(eth, ihl),
        PROTO_ICMP => ap_lan_icmp(eth, ihl),
        _ => {}
    }
}

fn ap_lan_udp(eth: &[u8], ihl: usize) {
    let l4 = 14 + ihl;
    let dport = u16::from_be_bytes([eth[l4 + 2], eth[l4 + 3]]);
    match dport {
        DHCP_SERVER_PORT => ap_lan_dhcp(eth, ihl),
        53 => ap_lan_dns(eth, ihl),
        _ => {}
    }
}

fn inet_csum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u32::from(u16::from_be_bytes([data[i], data[i + 1]]));
        i += 2;
    }
    if i < data.len() {
        sum += u32::from(data[i]) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn ap_lan_dhcp(eth: &[u8], ihl: usize) {
    let l4 = 14 + ihl;
    let bootp = l4 + 8;
    if eth.len() < bootp + 241 {
        return;
    }
    let d = &eth[bootp..];
    if d[0] != 1 || d[236..240] != DHCP_MAGIC[..] {
        return;
    }
    let mut msg = 0u8;
    let mut i = 240;
    while i + 1 < d.len() {
        let id = d[i];
        if id == 255 {
            break;
        }
        if id == 0 {
            i += 1;
            continue;
        }
        let l = d[i + 1] as usize;
        if id == 53 && i + 2 < d.len() {
            msg = d[i + 2];
        }
        i += 2 + l;
    }
    if msg != DHCP_DISCOVER && msg != DHCP_REQUEST {
        return;
    }
    let reply = if msg == DHCP_DISCOVER {
        DHCP_OFFER
    } else {
        DHCP_ACK
    };
    let mut dst = [0u8; 6];
    dst.copy_from_slice(&eth[6..12]);
    let mut xid = [0u8; 4];
    xid.copy_from_slice(&d[4..8]);
    let mut chaddr = [0u8; 6];
    chaddr.copy_from_slice(&d[28..34]);
    let mut out = [0u8; 512];
    let n = build_dhcp_reply(&mut out, reply, &xid, &chaddr);
    if let Ok((f, fn_)) = ap_data_frame(dst, ETHERTYPE_IPV4, &out[..n], lan_protect()) {
        unsafe {
            let _ = (*addr_of_mut!(RXQ)).push(&f[..fn_]);
        }
        crate::kprintln!(
            "[serial] [wifi] ap dhcp {} -> 10.0.9.15",
            if reply == DHCP_OFFER { "offer" } else { "ack" }
        );
    }
}

fn build_dhcp_reply(out: &mut [u8], msg: u8, xid: &[u8; 4], chaddr: &[u8; 6]) -> usize {
    out.fill(0);
    let mut o;
    let dns: [u8; 4] = DNS_IP;
    let opts = 3 + 6 + 6 + 6 + 6 + 6 + 1;
    let total = 20 + 8 + 240 + opts;
    out[0] = 0x45;
    out[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    out[8] = 64;
    out[9] = PROTO_UDP;
    out[12..16].copy_from_slice(&AP_IP);
    out[16..20].copy_from_slice(&[255, 255, 255, 255]);
    let c = inet_csum(&out[..20]);
    out[10..12].copy_from_slice(&c.to_be_bytes());
    o = 20;
    out[o..o + 2].copy_from_slice(&DHCP_SERVER_PORT.to_be_bytes());
    out[o + 2..o + 4].copy_from_slice(&DHCP_CLIENT_PORT.to_be_bytes());
    out[o + 4..o + 6].copy_from_slice(&((8 + 240 + opts) as u16).to_be_bytes());
    o = 28;
    out[o] = 2;
    out[o + 1] = 1;
    out[o + 2] = 6;
    out[o + 4..o + 8].copy_from_slice(xid);
    out[o + 16..o + 20].copy_from_slice(&STATION_IP);
    out[o + 20..o + 24].copy_from_slice(&AP_IP);
    out[o + 28..o + 34].copy_from_slice(chaddr);
    out[o + 236..o + 240].copy_from_slice(&DHCP_MAGIC);
    o += 240;
    out[o] = 53;
    out[o + 1] = 1;
    out[o + 2] = msg;
    o += 3;
    out[o] = 1;
    out[o + 1] = 4;
    out[o + 2..o + 6].copy_from_slice(&AP_MASK);
    o += 6;
    out[o] = 3;
    out[o + 1] = 4;
    out[o + 2..o + 6].copy_from_slice(&AP_IP);
    o += 6;
    out[o] = 6;
    out[o + 1] = 4;
    out[o + 2..o + 6].copy_from_slice(&dns);
    o += 6;
    out[o] = 51;
    out[o + 1] = 4;
    out[o + 2..o + 6].copy_from_slice(&3600u32.to_be_bytes());
    o += 6;
    out[o] = 54;
    out[o + 1] = 4;
    out[o + 2..o + 6].copy_from_slice(&AP_IP);
    o += 6;
    out[o] = 255;
    o += 1;
    o
}

fn ap_lan_dns(eth: &[u8], ihl: usize) {
    let l4 = 14 + ihl;
    let sport = u16::from_be_bytes([eth[l4], eth[l4 + 1]]);
    let dns_off = l4 + 8;
    if eth.len() < dns_off + 13 {
        return;
    }
    let dns = &eth[dns_off..];
    let mut i = 12;
    loop {
        if i >= dns.len() {
            return;
        }
        let l = dns[i] as usize;
        if l == 0 {
            i += 1;
            break;
        }
        if l > 63 {
            return;
        }
        i += 1 + l;
    }
    if i + 4 > dns.len() {
        return;
    }
    let qlen = i + 4 - 12;
    let dnslen = 12 + qlen + 16;
    let total = 20 + 8 + dnslen;
    if total > 512 {
        return;
    }
    let mut out = [0u8; 512];
    let mut dst = [0u8; 6];
    dst.copy_from_slice(&eth[6..12]);
    out[0] = 0x45;
    out[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    out[8] = 64;
    out[9] = PROTO_UDP;
    out[12..16].copy_from_slice(&DNS_IP);
    out[16..20].copy_from_slice(&dst_ip_of(eth));
    let c = inet_csum(&out[..20]);
    out[10..12].copy_from_slice(&c.to_be_bytes());
    out[20..22].copy_from_slice(&53u16.to_be_bytes());
    out[22..24].copy_from_slice(&sport.to_be_bytes());
    out[24..26].copy_from_slice(&(8 + dnslen as u16).to_be_bytes());
    let d = 28;
    out[d..d + 2].copy_from_slice(&dns[0..2]);
    out[d + 2] = 0x81;
    out[d + 3] = 0x80;
    out[d + 4..d + 6].copy_from_slice(&1u16.to_be_bytes());
    out[d + 6..d + 8].copy_from_slice(&1u16.to_be_bytes());
    out[d + 12..d + 12 + qlen].copy_from_slice(&dns[12..12 + qlen]);
    let a = d + 12 + qlen;
    out[a] = 0xC0;
    out[a + 1] = 0x0C;
    out[a + 2..a + 4].copy_from_slice(&1u16.to_be_bytes());
    out[a + 4..a + 6].copy_from_slice(&1u16.to_be_bytes());
    out[a + 6..a + 10].copy_from_slice(&60u32.to_be_bytes());
    out[a + 10..a + 12].copy_from_slice(&4u16.to_be_bytes());
    out[a + 12..a + 16].copy_from_slice(&FAKE_INTERNET_IP);
    if let Ok((f, n)) = ap_data_frame(dst, ETHERTYPE_IPV4, &out[..total], lan_protect()) {
        unsafe {
            let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
        }
    }
}

fn dst_ip_of(eth: &[u8]) -> [u8; 4] {
    let mut ip = [0u8; 4];
    ip.copy_from_slice(&eth[30..34]);
    ip
}

fn ap_lan_icmp(eth: &[u8], ihl: usize) {
    let icmp_off = 14 + ihl;
    if eth.len() < icmp_off + 8 {
        return;
    }
    if eth[icmp_off] != ICMP_ECHO {
        return;
    }
    let mut dst = [0u8; 6];
    dst.copy_from_slice(&eth[6..12]);
    let mut dst_ip = [0u8; 4];
    dst_ip.copy_from_slice(&eth[26..30]);
    let icmp_len = eth.len() - icmp_off;
    let total = 20 + icmp_len;
    if total > 576 {
        return;
    }
    let mut out = [0u8; 576];
    out[0] = 0x45;
    out[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    out[8] = 64;
    out[9] = PROTO_ICMP;
    out[12..16].copy_from_slice(&AP_IP);
    out[16..20].copy_from_slice(&dst_ip);
    let c = inet_csum(&out[..20]);
    out[10..12].copy_from_slice(&c.to_be_bytes());
    out[20] = ICMP_REPLY;
    out[24..20 + icmp_len].copy_from_slice(&eth[icmp_off + 4..]);
    out[22..24].fill(0);
    let ci = inet_csum(&out[20..total]);
    out[22..24].copy_from_slice(&ci.to_be_bytes());
    if let Ok((f, n)) = ap_data_frame(dst, ETHERTYPE_IPV4, &out[..total], lan_protect()) {
        unsafe {
            let _ = (*addr_of_mut!(RXQ)).push(&f[..n]);
        }
    }
}
