//! Network connection engine: Wi-Fi scan/association + DHCP + config sync.
//!
//! The engine is backend-dispatched ([`WifiBackend`]) so the same
//! [`NetManager`] API drives a fully simulated ether (deterministic tests,
//! CI) and the real host adapter (`netsh wlan` on Windows, sysfs/iw on Linux).
//! After a successful association the manager runs the [`crate::dhcp`]
//! client and persists the resulting lease into the shared
//! [`aios_net_config::NetworkConfigStore`] so `NetSettingsBlock`, the bridge
//! and the TUI/GUI all observe one source of truth.

use std::net::Ipv4Addr;
use std::time::Duration;

use aios_core::error::{AIOSException, Result};
use aios_net_config::{NetworkConfig, NetworkConfigStore};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::dhcp::DhcpClient;

/// Wi-Fi security mode advertised by an access point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WifiSecurity {
    Open,
    Wpa2,
    Wpa3,
    Wpa2Wpa3,
}

impl WifiSecurity {
    /// Short badge used by UI status lines.
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Wpa2 => "WPA2",
            Self::Wpa3 => "WPA3",
            Self::Wpa2Wpa3 => "WPA2/3",
        }
    }

    /// True when association requires a passphrase.
    pub fn needs_password(self) -> bool {
        !matches!(self, Self::Open)
    }
}

/// Frequency band of an access point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WifiBand {
    Ghz24,
    Ghz5,
    Ghz6,
}

impl WifiBand {
    /// Human label ("2.4GHz"/"5GHz"/"6GHz").
    pub fn label(self) -> &'static str {
        match self {
            Self::Ghz24 => "2.4GHz",
            Self::Ghz5 => "5GHz",
            Self::Ghz6 => "6GHz",
        }
    }
}

/// One scanned access point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub rssi_dbm: i8,
    pub security: WifiSecurity,
    pub band: WifiBand,
}

/// Link lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkState {
    Disconnected,
    Associating,
    Dhcp,
    Connected,
    Failed(String),
}

/// Current link summary rendered in the status bars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkStatus {
    pub state: LinkState,
    pub ssid: Option<String>,
    pub ipv4: Option<Ipv4Addr>,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    pub rssi_dbm: i8,
    pub band: Option<WifiBand>,
}

impl Default for LinkStatus {
    fn default() -> Self {
        Self {
            state: LinkState::Disconnected,
            ssid: None,
            ipv4: None,
            gateway: None,
            dns: None,
            rssi_dbm: 0,
            band: None,
        }
    }
}

impl LinkStatus {
    /// One-line status for the TUI/GUI status bar, e.g.
    /// `[Wi-Fi: Connected (5GHz) -70dBm]` or `[Wi-Fi: --]`.
    pub fn status_segment(&self) -> String {
        match (&self.state, self.band) {
            (LinkState::Connected, Some(band)) => {
                let ssid = self.ssid.as_deref().unwrap_or("Connected");
                format!(
                    "[Wi-Fi: {} ({}) {}dBm]",
                    truncate_ssid(ssid),
                    band.label(),
                    self.rssi_dbm
                )
            }
            (LinkState::Connected, _) => format!("[Wi-Fi: Connected {}dBm]", self.rssi_dbm),
            (LinkState::Associating, _) => "[Wi-Fi: associating...]".into(),
            (LinkState::Dhcp, _) => "[Wi-Fi: DHCP...]".into(),
            (LinkState::Failed(e), _) => format!("[Wi-Fi: error {e}]"),
            _ => "[Wi-Fi: --]".into(),
        }
    }
}

fn truncate_ssid(ssid: &str) -> &str {
    // Keep status segments bounded; 16 chars is plenty for a status bar.
    match ssid.char_indices().nth(16) {
        Some((i, _)) => &ssid[..i],
        None => ssid,
    }
}

/// Deterministic in-process ether used by tests and safe mode.
///
/// Known networks accept fixed passphrases so association success/failure
/// paths are exercisable without hardware:
/// - `aios-lab-5g` / `aios-rocks` (WPA3, 5 GHz)
/// - `home-net` / `homenet-pass` (WPA2, 2.4 GHz)
/// - `coffee-shop` (Open, 5 GHz)
#[derive(Default)]
pub struct SimulatedWifi {
    associated: Option<(String, WifiBand, i8)>,
}

impl SimulatedWifi {
    /// The canned ether table.
    pub fn networks() -> Vec<WifiNetwork> {
        vec![
            WifiNetwork {
                ssid: "aios-lab-5g".into(),
                rssi_dbm: -42,
                security: WifiSecurity::Wpa3,
                band: WifiBand::Ghz5,
            },
            WifiNetwork {
                ssid: "home-net".into(),
                rssi_dbm: -61,
                security: WifiSecurity::Wpa2,
                band: WifiBand::Ghz24,
            },
            WifiNetwork {
                ssid: "coffee-shop".into(),
                rssi_dbm: -70,
                security: WifiSecurity::Open,
                band: WifiBand::Ghz5,
            },
        ]
    }
}

/// Backend dispatch: simulated ether or the real host adapter.
pub enum WifiBackend {
    Simulated(SimulatedWifi),
    Host,
}

impl Default for WifiBackend {
    fn default() -> Self {
        Self::Simulated(SimulatedWifi::default())
    }
}

impl std::fmt::Debug for WifiBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simulated(_) => f.write_str("Simulated"),
            Self::Host => f.write_str("Host"),
        }
    }
}

impl WifiBackend {
    async fn scan(&self) -> Result<Vec<WifiNetwork>> {
        match self {
            Self::Simulated(_) => Ok(SimulatedWifi::networks()),
            Self::Host => host_scan().await,
        }
    }

    async fn connect(
        &mut self,
        ssid: &str,
        password: Option<&str>,
        dhcp_timeout: Duration,
    ) -> Result<LinkStatus> {
        match self {
            Self::Simulated(sim) => {
                let net = SimulatedWifi::networks()
                    .into_iter()
                    .find(|n| n.ssid == ssid)
                    .ok_or_else(|| {
                        AIOSException::HardwareNotDetected(format!("ssid '{ssid}' not in range"))
                    })?;
                if net.security.needs_password()
                    && password != Some("aios-rocks")
                    && password != Some("homenet-pass")
                {
                    return Err(AIOSException::PermissionDenied(format!(
                        "wrong passphrase for '{ssid}'"
                    )));
                }
                sim.associated = Some((ssid.to_string(), net.band, net.rssi_dbm));
                Ok(LinkStatus {
                    state: LinkState::Connected,
                    ssid: Some(ssid.into()),
                    ipv4: Some(Ipv4Addr::new(192, 168, 77, 42)),
                    gateway: Some(Ipv4Addr::new(192, 168, 77, 1)),
                    dns: Some(Ipv4Addr::new(1, 1, 1, 1)),
                    rssi_dbm: net.rssi_dbm,
                    band: Some(net.band),
                })
            }
            Self::Host => {
                host_associate(ssid).await?;
                let mut status = host_status().await.unwrap_or_default();
                status.state = LinkState::Dhcp;
                match DhcpClient::default().acquire(dhcp_timeout).await {
                    Ok(lease) => {
                        status.state = LinkState::Connected;
                        status.ipv4 = Some(lease.ip);
                        status.gateway = lease.gateway;
                        status.dns = lease.dns_servers.first().copied();
                    }
                    Err(e) => {
                        return Err(AIOSException::Timeout(format!(
                            "dhcp after assoc failed: {e}"
                        )));
                    }
                }
                Ok(status)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        match self {
            Self::Simulated(sim) => {
                sim.associated = None;
                Ok(())
            }
            Self::Host => host_disconnect().await,
        }
    }
}

async fn run_netsh(args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("netsh")
        .args(args)
        .output()
        .await
        .map_err(|e| AIOSException::HardwareNotDetected(format!("netsh spawn: {e}")))?;
    if !out.status.success() {
        return Err(AIOSException::HardwareNotDetected(format!(
            "netsh {:?} exited {:?}",
            args,
            out.status.code()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn host_scan() -> Result<Vec<WifiNetwork>> {
    let text = run_netsh(&["wlan", "show", "networks", "mode=bssid"]).await?;
    Ok(parse_netsh_networks(&text))
}

fn parse_netsh_networks(text: &str) -> Vec<WifiNetwork> {
    let mut nets = Vec::new();
    let mut cur: Option<WifiNetwork> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("SSID ") {
            if rest.contains(':') {
                if let Some(prev) = cur.take() {
                    nets.push(prev);
                }
                let name = rest.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
                cur = Some(WifiNetwork {
                    ssid: name.to_string(),
                    rssi_dbm: 0,
                    security: WifiSecurity::Open,
                    band: WifiBand::Ghz24,
                });
            }
        } else if let Some(v) = l.strip_prefix("Authentication") {
            if let Some(n) = cur.as_mut() {
                let v = v.split_once(':').map(|(_, x)| x.trim()).unwrap_or("");
                n.security = if v.starts_with("WPA3") {
                    WifiSecurity::Wpa3
                } else if v.starts_with("WPA2") && v.contains("WPA3") {
                    WifiSecurity::Wpa2Wpa3
                } else if v.starts_with("WPA2") {
                    WifiSecurity::Wpa2
                } else {
                    WifiSecurity::Open
                };
            }
        } else if let Some(v) = l.strip_prefix("Signal") {
            if let Some(n) = cur.as_mut() {
                let pct: u32 = v
                    .split_once(':')
                    .and_then(|(_, x)| x.trim().trim_end_matches('%').parse().ok())
                    .unwrap_or(0);
                n.rssi_dbm = -signal_pct_to_quality(pct);
            }
        } else if l.starts_with("Radio type") {
            if let Some(n) = cur.as_mut() {
                n.band = WifiBand::Ghz5;
            }
        }
    }
    if let Some(prev) = cur.take() {
        nets.push(prev);
    }
    nets.retain(|n| !n.ssid.is_empty());
    nets
}

async fn host_associate(ssid: &str) -> Result<()> {
    // Association requires a prepared OS profile; the connect verb reuses it.
    run_netsh(&["wlan", "connect", &format!("name={ssid}")])
        .await
        .map(|_| ())
}

async fn host_disconnect() -> Result<()> {
    run_netsh(&["wlan", "disconnect"]).await.map(|_| ())
}

async fn host_status() -> Result<LinkStatus> {
    let text = run_netsh(&["wlan", "show", "interfaces"]).await?;
    Ok(parse_netsh_status(&text))
}

fn parse_netsh_status(text: &str) -> LinkStatus {
    let mut st = LinkStatus::default();
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("State") {
            let v = v.split_once(':').map(|(_, x)| x.trim()).unwrap_or("");
            if v.contains("connected") {
                st.state = LinkState::Connected;
            }
        } else if let Some(v) = l.strip_prefix("SSID") {
            st.ssid = v
                .split_once(':')
                .map(|(_, x)| x.trim().to_string())
                .filter(|s| !s.is_empty());
        } else if let Some(v) = l.strip_prefix("Signal") {
            let pct: u32 = v
                .split_once(':')
                .and_then(|(_, x)| x.trim().trim_end_matches('%').parse().ok())
                .unwrap_or(0);
            st.rssi_dbm = -signal_pct_to_quality(pct);
        }
    }
    st
}

/// Map netsh signal percentage (0-100) to an approximate dBm magnitude.
fn signal_pct_to_quality(pct: u32) -> i8 {
    (200 - pct.min(100).saturating_mul(2)).clamp(20, 100) as i8
}

/// High-level network connection engine.
pub struct NetManager {
    backend: Mutex<WifiBackend>,
    config_store: Option<NetworkConfigStore>,
    link: LinkStatus,
}

impl Default for NetManager {
    fn default() -> Self {
        Self::new(WifiBackend::default(), None)
    }
}

impl std::fmt::Debug for NetManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetManager")
            .field("link", &self.link)
            .finish_non_exhaustive()
    }
}

impl NetManager {
    /// Engine over an explicit backend with optional config persistence.
    pub fn new(backend: WifiBackend, config_store: Option<NetworkConfigStore>) -> Self {
        Self {
            backend: Mutex::new(backend),
            config_store,
            link: LinkStatus::default(),
        }
    }

    /// Scan the ether through the active backend.
    pub async fn scan(&self) -> Result<Vec<WifiNetwork>> {
        self.backend.lock().await.scan().await
    }

    /// Current cached link summary (no I/O).
    pub fn link(&self) -> &LinkStatus {
        &self.link
    }

    /// Async snapshot of the link state (safe across await points).
    pub fn status(&self) -> LinkStatus {
        self.link.clone()
    }

    /// Associate with `ssid`, run DHCP, persist the lease into the
    /// network-config store and update the cached link.
    ///
    /// Open networks ignore `password`; secured networks require it.
    pub async fn connect(&mut self, ssid: &str, password: Option<&str>) -> Result<LinkStatus> {
        let outcome = {
            let mut be = self.backend.lock().await;
            be.connect(ssid, password, Duration::from_secs(10)).await
        };
        match outcome {
            Ok(st) => {
                self.persist_lease(ssid, &st);
                self.link = st.clone();
                Ok(st)
            }
            Err(e) => {
                self.link = LinkStatus {
                    state: LinkState::Failed(short_err(&e)),
                    ..LinkStatus::default()
                };
                Err(e)
            }
        }
    }

    fn persist_lease(&mut self, ssid: &str, st: &LinkStatus) {
        let Some(store) = self.config_store.as_mut() else {
            return;
        };
        let cfg = NetworkConfig {
            hostname: format!("aios-{ssid}"),
            interfaces: vec![aios_net_config::InterfaceConfig {
                name: "wlan0".into(),
                ip: st.ipv4.map(|a| a.to_string()),
                netmask: Some("255.255.255.0".into()),
                gateway: st.gateway.map(|g| g.to_string()),
                mtu: None,
                dhcp: true,
            }],
            ..NetworkConfig::default()
        };
        let _ = store.save(&cfg);
    }

    /// Drop the association and reset the cached link.
    pub async fn disconnect(&mut self) -> Result<()> {
        self.backend.lock().await.disconnect().await?;
        self.link = LinkStatus::default();
        Ok(())
    }
}

fn short_err(e: &AIOSException) -> String {
    e.to_string().chars().take(48).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn simulated_scan_lists_three_known_networks() {
        let m = NetManager::default();
        let nets = m.scan().await.unwrap();
        assert_eq!(nets.len(), 3);
        assert!(nets
            .iter()
            .any(|n| n.security == WifiSecurity::Wpa3 && n.band == WifiBand::Ghz5));
    }

    #[tokio::test]
    async fn connect_open_network_without_password_connects() {
        let mut m = NetManager::default();
        let st = m.connect("coffee-shop", None).await.unwrap();
        assert_eq!(st.state, LinkState::Connected);
        assert_eq!(st.ssid.as_deref(), Some("coffee-shop"));
        assert_eq!(st.band, Some(WifiBand::Ghz5));
        assert_eq!(m.link().ipv4, Some(Ipv4Addr::new(192, 168, 77, 42)));
    }

    #[tokio::test]
    async fn wrong_passphrase_is_permission_denied_and_link_failed() {
        let mut m = NetManager::default();
        let err = m.connect("home-net", Some("bad")).await.unwrap_err();
        assert!(matches!(err, AIOSException::PermissionDenied(_)));
        assert!(matches!(m.link().state, LinkState::Failed(_)));
    }

    #[tokio::test]
    async fn unknown_ssid_is_not_detected() {
        let mut m = NetManager::default();
        let err = m.connect("ghost", None).await.unwrap_err();
        assert!(matches!(err, AIOSException::HardwareNotDetected(_)));
    }

    #[tokio::test]
    async fn secured_network_needs_matching_passphrase() {
        let mut m = NetManager::default();
        let st = m.connect("aios-lab-5g", Some("aios-rocks")).await.unwrap();
        assert_eq!(st.rssi_dbm, -42);
        m.disconnect().await.unwrap();
        assert_eq!(m.link().state, LinkState::Disconnected);
    }

    #[test]
    fn status_segment_formats_connected_5ghz() {
        let st = LinkStatus {
            state: LinkState::Connected,
            ssid: Some("aios-lab-5g".into()),
            rssi_dbm: -70,
            band: Some(WifiBand::Ghz5),
            ..Default::default()
        };
        assert_eq!(st.status_segment(), "[Wi-Fi: aios-lab-5g (5GHz) -70dBm]");
    }

    #[test]
    fn status_segment_degrades_when_disconnected() {
        assert_eq!(LinkStatus::default().status_segment(), "[Wi-Fi: --]");
        let fail = LinkStatus {
            state: LinkState::Failed("dhcp timeout".into()),
            ..Default::default()
        };
        assert_eq!(fail.status_segment(), "[Wi-Fi: error dhcp timeout]");
    }

    #[test]
    fn long_ssid_is_truncated_in_status_segment() {
        let st = LinkStatus {
            state: LinkState::Connected,
            ssid: Some("a-very-long-network-name-exceeding-limit".into()),
            rssi_dbm: -50,
            band: Some(WifiBand::Ghz24),
            ..Default::default()
        };
        let seg = st.status_segment();
        assert!(!seg.contains('…'), "{seg}");
        assert!(seg.len() < 60, "{seg}");
    }

    #[test]
    fn netsh_parser_extracts_ssid_auth_signal() {
        let sample = "\nInterface name: Wi-Fi\nThere are 3 networks currently visible.\n\nSSID 1 : aios-lab\n    Authentication         : WPA3-Personal\n    Signal                 : 86%\n    Radio type             : 802.11ax\n\nSSID 2 : open-cafe\n    Authentication         : Open\n    Signal                 : 55%\n";
        let nets = parse_netsh_networks(sample);
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].ssid, "aios-lab");
        assert_eq!(nets[0].security, WifiSecurity::Wpa3);
        assert_eq!(nets[0].band, WifiBand::Ghz5);
        assert_eq!(nets[1].security, WifiSecurity::Open);
        assert_eq!(nets[0].rssi_dbm, -28); // 86% → 200-172=28 → -28
    }

    #[test]
    fn netsh_status_parser_reads_state_ssid_signal() {
        let sample = "    Name                   : Wi-Fi\n    State                  : connected\n    SSID                   : home-net\n    Signal                 : 66%\n";
        let st = parse_netsh_status(sample);
        assert_eq!(st.state, LinkState::Connected);
        assert_eq!(st.ssid.as_deref(), Some("home-net"));
        assert_eq!(st.rssi_dbm, -68);
    }
}
