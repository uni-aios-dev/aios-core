//! Host network adapter inspection for wired and wireless interfaces.
//!
//! The pure parsers ([`parse_netsh_config`], [`parse_ip_addr`]) are exercised
//! without hardware; [`collect_adapters`] uses `netsh` on Windows and
//! `ip addr` plus sysfs on Linux. No adapter is ever configured here — this
//! module only reports link state, addresses and DNS servers.

use std::net::Ipv4Addr;
use std::path::Path;

#[cfg(windows)]
use aios_core::error::AIOSException;
use aios_core::error::Result;
use serde::{Deserialize, Serialize};

/// One host network adapter observed by an OS-level query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterState {
    /// OS interface name (`Ethernet`, `eth0`, `wlan0` ...).
    pub name: String,
    /// Link state as reported by the OS (`up`, `down` or `unknown`).
    pub status: String,
    /// Hardware address when the OS exposes it.
    pub mac: Option<String>,
    /// Assigned IPv4 address, if any.
    pub ipv4: Option<Ipv4Addr>,
    /// IPv4 subnet mask, if reported.
    pub netmask: Option<Ipv4Addr>,
    /// Default gateway, if reported.
    pub gateway: Option<Ipv4Addr>,
    /// DNS servers in OS order.
    pub dns: Vec<Ipv4Addr>,
    /// Whether the address came from DHCP.
    pub dhcp: bool,
}

impl AdapterState {
    /// New adapter with no addresses and an unknown link state.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "unknown".into(),
            mac: None,
            ipv4: None,
            netmask: None,
            gateway: None,
            dns: Vec::new(),
            dhcp: false,
        }
    }

    /// Compact status line shared by the console and UI status bars.
    pub fn status_line(&self) -> String {
        let ipv4 = self
            .ipv4
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "-".into());
        let gateway = self
            .gateway
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "-".into());
        format!(
            "{:<18} {:<7} {:<15} {:<15} {}",
            self.name, self.status, ipv4, gateway, self.dhcp
        )
    }
}

fn parse_ip(raw: &str) -> Option<Ipv4Addr> {
    raw.trim().split('/').next()?.trim().parse().ok()
}

fn mask_from_prefix(prefix: u8) -> Ipv4Addr {
    let prefix = prefix.min(32);
    let bits = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    Ipv4Addr::from(bits)
}

fn extend_ips(out: &mut Vec<Ipv4Addr>, text: &str) {
    for token in
        text.split(|c: char| c.is_whitespace() || c == '/' || c == ',' || c == '(' || c == ')')
    {
        if let Ok(ip) = token.parse::<Ipv4Addr>() {
            out.push(ip);
        }
    }
}

/// Parse `netsh interface ipv4 show config` output into adapter states.
pub fn parse_netsh_config(text: &str) -> Vec<AdapterState> {
    let mut out = Vec::new();
    for block in text.split("Configuration for interface").skip(1) {
        let name = block
            .lines()
            .next()
            .and_then(|line| line.split_once('"'))
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name)
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        let mut adapter = AdapterState::new(name);
        adapter.status = "up".into();
        let mut reading_dns = false;
        for line in block.lines().skip(1) {
            let trimmed = line.trim();
            let Some((key, value)) = trimmed.split_once(':') else {
                if reading_dns {
                    extend_ips(&mut adapter.dns, trimmed);
                }
                continue;
            };
            match key.trim() {
                "DHCP enabled" => {
                    adapter.dhcp =
                        matches!(value.trim().to_ascii_lowercase().as_str(), "yes" | "true");
                    reading_dns = false;
                }
                "IP Address" => {
                    adapter.ipv4 = parse_ip(value);
                    reading_dns = false;
                }
                "Subnet Prefix" => {
                    let prefix = value
                        .split('/')
                        .nth(1)
                        .and_then(|tail| tail.split_whitespace().next())
                        .and_then(|digits| digits.parse::<u8>().ok());
                    adapter.netmask = prefix.map(mask_from_prefix).or_else(|| {
                        value
                            .split_once("mask ")
                            .and_then(|(_, rest)| rest.split(')').next())
                            .and_then(parse_ip)
                    });
                    reading_dns = false;
                }
                "Default Gateway" => {
                    adapter.gateway = parse_ip(value);
                    reading_dns = false;
                }
                "DNS servers via DHCP" | "DNS servers" | "DNS Servers" => {
                    reading_dns = true;
                    extend_ips(&mut adapter.dns, value);
                }
                _ => reading_dns = false,
            }
        }
        out.push(adapter);
    }
    out
}

/// Parse `ip -4 addr show` output into adapter states.
pub fn parse_ip_addr(text: &str) -> Vec<AdapterState> {
    let mut out: Vec<AdapterState> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() >= 2 && tokens[0].ends_with(':') && tokens[1].ends_with(':') {
            if let Some(name) = tokens[1].strip_suffix(':') {
                let mut adapter = AdapterState::new(name);
                adapter.status = if trimmed.contains("state UP") {
                    "up".into()
                } else {
                    "down".into()
                };
                out.push(adapter);
            }
            continue;
        }
        let Some(current) = out.last_mut() else {
            continue;
        };
        if tokens.first() == Some(&"inet") {
            if let Some(ip) = tokens.get(1).and_then(|raw| parse_ip(raw)) {
                current.ipv4 = Some(ip);
                current.dhcp = true;
                if let Some(prefix) = tokens[1]
                    .split('/')
                    .nth(1)
                    .and_then(|digits| digits.parse::<u8>().ok())
                {
                    current.netmask = Some(mask_from_prefix(prefix));
                }
            }
        } else if tokens.first() == Some(&"link/ether") {
            current.mac = tokens.get(1).map(|raw| raw.to_string());
        } else if tokens.first() == Some(&"default") && tokens.get(1) == Some(&"via") {
            current.gateway = tokens.get(2).and_then(|raw| parse_ip(raw));
        }
    }
    out
}

/// Fill status and MAC from `/sys/class/net/<name>` for Linux adapters.
pub fn merge_sysfs(root: &Path, adapters: &mut [AdapterState]) {
    for adapter in adapters.iter_mut() {
        let dir = root.join(&adapter.name);
        if let Ok(state) = std::fs::read_to_string(dir.join("operstate")) {
            let state = state.trim().to_ascii_lowercase();
            if !state.is_empty() {
                adapter.status = state;
            }
        }
        if let Ok(mac) = std::fs::read_to_string(dir.join("address")) {
            let mac = mac.trim();
            if !mac.is_empty() {
                adapter.mac = Some(mac.to_string());
            }
        }
    }
}

/// Query the host for wired and wireless adapters.
pub fn collect_adapters() -> Result<Vec<AdapterState>> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("netsh")
            .args(["interface", "ipv4", "show", "config"])
            .output()
            .map_err(|e| AIOSException::HardwareNotDetected(format!("netsh spawn: {e}")))?;
        if !output.status.success() {
            return Err(AIOSException::HardwareNotDetected(format!(
                "netsh interface ipv4 show config exited {:?}",
                output.status.code()
            )));
        }
        Ok(parse_netsh_config(&String::from_utf8_lossy(&output.stdout)))
    }
    #[cfg(not(windows))]
    {
        let mut adapters = std::process::Command::new("ip")
            .args(["-4", "addr", "show"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| parse_ip_addr(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or_default();
        if cfg!(target_os = "linux") {
            merge_sysfs(Path::new("/sys/class/net"), &mut adapters);
        }
        Ok(adapters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netsh_config_parses_addresses_and_dns_continuation() {
        let text = r#"
Configuration for interface "Ethernet"
    DHCP enabled:                         Yes
    IP Address:                           192.168.1.10
    Subnet Prefix:                        192.168.1.0/24 (mask 255.255.255.0)
    Default Gateway:                      192.168.1.1
    DNS servers via DHCP:                 192.168.1.1
                                          8.8.8.8
"#;
        let adapters = parse_netsh_config(text);
        assert_eq!(adapters.len(), 1);
        let a = &adapters[0];
        assert_eq!(a.name, "Ethernet");
        assert!(a.dhcp);
        assert_eq!(a.ipv4, Some(Ipv4Addr::new(192, 168, 1, 10)));
        assert_eq!(a.netmask, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(a.gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(
            a.dns,
            vec![Ipv4Addr::new(192, 168, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
        );
    }

    #[test]
    fn ip_addr_parses_link_and_address() {
        let text = "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 state UP\n\
                    link/ether 52:54:00:12:34:56 brd ff:ff:ff:ff:ff:ff\n\
                    inet 10.0.2.15/24 brd 10.0.2.255 scope global dynamic eth0\n\
                    default via 10.0.2.2 dev eth0\n";
        let adapters = parse_ip_addr(text);
        assert_eq!(adapters.len(), 1);
        let a = &adapters[0];
        assert_eq!(a.name, "eth0");
        assert_eq!(a.status, "up");
        assert_eq!(a.mac.as_deref(), Some("52:54:00:12:34:56"));
        assert_eq!(a.ipv4, Some(Ipv4Addr::new(10, 0, 2, 15)));
        assert_eq!(a.netmask, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(a.gateway, Some(Ipv4Addr::new(10, 0, 2, 2)));
    }

    #[test]
    fn sysfs_overlay_updates_status_and_mac() {
        let dir = tempfile::tempdir().unwrap();
        let eth = dir.path().join("eth0");
        std::fs::create_dir_all(&eth).unwrap();
        std::fs::write(eth.join("operstate"), "down\n").unwrap();
        std::fs::write(eth.join("address"), "02:00:00:00:00:01\n").unwrap();
        let mut adapters = vec![AdapterState::new("eth0")];
        merge_sysfs(dir.path(), &mut adapters);
        assert_eq!(adapters[0].status, "down");
        assert_eq!(adapters[0].mac.as_deref(), Some("02:00:00:00:00:01"));
    }

    #[test]
    fn mask_from_prefix_covers_full_range() {
        assert_eq!(mask_from_prefix(0), Ipv4Addr::UNSPECIFIED);
        assert_eq!(mask_from_prefix(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(mask_from_prefix(32), Ipv4Addr::from(u32::MAX));
    }
}
