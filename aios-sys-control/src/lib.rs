//! AIOS System Control Stack (`aios-sys-control`).
//!
//! The crate groups the four services required to run AIOS on physical
//! laptops/desktops:
//!
//! - [`net_manager`] — Wi-Fi scanning/association engine with a built-in
//!   asynchronous DHCP client and `aios-net-config` integration.
//! - [`input_i18n`] — global keyboard layout switcher (RU/EN) driven by
//!   hotkey combos, exposing an indicator for the TUI status bar / GUI panel.
//! - [`power_mgr`] — battery/AC/lid ACPI state plus CPU/GPU thermal sensors,
//!   with a reactive thermal governor that moves LLM inference from the local
//!   Candle backend to a cloud provider above 80 °C (hysteresis at 70 °C).
//! - [`keyring`] — master encrypted vault (AES-256-GCM) persisted in redb,
//!   unlocked either by a TEE/TPM2 platform binding or a master password.
//!
//! [`SysControlHub`] ties the services together and renders a single
//! [`SysStatusSnapshot`] consumed by the kernel TUI header and the GUI panel.

pub mod dhcp;
pub mod input_i18n;
pub mod keyring;
pub mod net_manager;
pub mod power_mgr;

use std::sync::Mutex;

use input_i18n::LayoutManager;
use net_manager::{LinkStatus, NetManager};
use power_mgr::PowerManager;
use tokio::sync::RwLock;

/// Aggregated system-control snapshot for the UI status bars.
///
/// Rendered as `[Wi-Fi: ...] [RU] [BAT: 84% | 48°C]` segments by both
/// frontends; every segment degrades gracefully to `--` when the data is
/// unavailable on the host.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SysStatusSnapshot {
    /// Current Wi-Fi link summary (None when no adapter/backend is present).
    pub wifi: Option<LinkStatus>,
    /// Active keyboard layout indicator ("EN"/"RU").
    pub layout: String,
    /// Battery percent (0-100) when a battery is present.
    pub battery_percent: Option<u8>,
    /// AC adapter plugged in (None when unknown).
    pub ac_online: Option<bool>,
    /// Hottest observed sensor temperature in Celsius.
    pub max_temp_c: f32,
    /// True while the thermal governor keeps LLM inference on the cloud.
    pub throttled: bool,
}

impl SysStatusSnapshot {
    /// One-line status string for the TUI/GUI status bars, e.g.
    /// `[Wi-Fi: Connected (5GHz) -70dBm] [EN/RU] [BAT: 84% | 48°C]`.
    ///
    /// Every segment degrades to a placeholder when its data source is
    /// unavailable; a thermal-throttle suffix appears only while active.
    pub fn status_line(&self) -> String {
        let wifi = self
            .wifi
            .as_ref()
            .map(|l| l.status_segment())
            .unwrap_or_else(|| "[Wi-Fi: --]".to_string());
        let layout = layout_pair(&self.layout);
        let battery = match self.battery_percent {
            Some(pct) => {
                let ac = if self.ac_online == Some(true) {
                    "+"
                } else {
                    ""
                };
                format!("[BAT{ac}: {pct}% | {:.0}°C]", self.max_temp_c)
            }
            None => format!("[{:.0}°C]", self.max_temp_c),
        };
        let cloud = if self.throttled { " [LLM: cloud]" } else { "" };
        format!("{wifi} {layout} {battery}{cloud}")
    }
}

/// `[EN/RU]`-style pair with the active layout first.
fn layout_pair(active: &str) -> String {
    let other = match active {
        "RU" => "EN",
        _ => "RU",
    };
    format!("[{active}/{other}]")
}

impl Default for SysStatusSnapshot {
    fn default() -> Self {
        Self {
            wifi: None,
            layout: "EN".into(),
            battery_percent: None,
            ac_online: None,
            max_temp_c: 0.0,
            throttled: false,
        }
    }
}

/// Facade bundling all system-control services behind one shared object.
///
/// Both frontends keep one hub and call [`SysControlHub::refresh_status`]
/// from their tick/frame loop; the last snapshot is cached for cheap
/// synchronous rendering between refreshes.
pub struct SysControlHub {
    /// Net engine behind an async lock: `scan`/`connect` await inside.
    net: RwLock<NetManager>,
    layout: RwLock<LayoutManager>,
    power: Mutex<PowerManager>,
    cached: Mutex<SysStatusSnapshot>,
}

impl SysControlHub {
    /// Build a hub over explicit service instances.
    pub fn new(net: NetManager, layout: LayoutManager, power: PowerManager) -> Self {
        let cached = SysStatusSnapshot {
            layout: layout.indicator().to_string(),
            ..SysStatusSnapshot::default()
        };
        Self {
            net: RwLock::new(net),
            layout: RwLock::new(layout),
            power: Mutex::new(power),
            cached: Mutex::new(cached),
        }
    }

    /// Hub using the default simulated Wi-Fi backend and mock-free power
    /// readers (real ACPI/sysfs probes when available, zeros otherwise).
    pub fn defaults() -> Self {
        Self::new(
            NetManager::default(),
            LayoutManager::default(),
            PowerManager::default(),
        )
    }

    /// Toggle the keyboard layout through the given hotkey combo.
    ///
    /// Returns true when the combo matched a registered hotkey and the
    /// layout flipped.
    pub async fn feed_hotkey(&self, combo: input_i18n::Hotkey) -> bool {
        let mut layout = self.layout.write().await;
        layout.apply_hotkey(combo)
    }

    /// Current layout indicator without refreshing hardware state.
    pub async fn layout_indicator(&self) -> String {
        self.layout.read().await.indicator().to_string()
    }

    /// Ask the thermal governor for the current LLM backend decision.
    pub fn llm_backend_decision(&self) -> aios_llm::BackendKind {
        self.power.lock().expect("power lock").llm_backend()
    }

    /// Scan Wi-Fi networks through the net engine.
    pub async fn scan_networks(&self) -> aios_core::error::Result<Vec<net_manager::WifiNetwork>> {
        self.net.read().await.scan().await
    }

    /// Associate with `ssid` (DHCP + lease persistence happen inside).
    ///
    /// Open networks ignore `password`; secured ones require it.
    pub async fn connect_wifi(
        &self,
        ssid: &str,
        password: Option<&str>,
    ) -> aios_core::error::Result<LinkStatus> {
        self.net.write().await.connect(ssid, password).await
    }

    /// Poll network + power state and cache a fresh snapshot.
    pub async fn refresh_status(&self) -> SysStatusSnapshot {
        let link = { self.net.read().await.status() };
        let layout = self.layout.read().await.indicator().to_string();
        let (battery_percent, ac_online, max_temp_c, throttled) = {
            let power = self.power.lock().expect("power lock");
            (
                power.battery_percent(),
                power.ac_online(),
                power.max_temp_c(),
                power.is_throttled(),
            )
        };
        let snap = SysStatusSnapshot {
            wifi: Some(link),
            layout,
            battery_percent,
            ac_online,
            max_temp_c,
            throttled,
        };
        *self.cached.lock().expect("cache lock") = snap.clone();
        snap
    }

    /// Last cached snapshot (cheap; use between refreshes).
    pub fn snapshot(&self) -> SysStatusSnapshot {
        self.cached.lock().expect("cache lock").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hub_default_snapshot_reports_en_layout() {
        let hub = SysControlHub::defaults();
        assert_eq!(hub.layout_indicator().await, "EN");
        let snap = hub.refresh_status().await;
        assert_eq!(snap.layout, "EN");
        assert!(!snap.throttled);
        assert_eq!(hub.snapshot(), snap);
    }

    #[tokio::test]
    async fn hub_hotkey_flips_layout_indicator() {
        let hub = SysControlHub::defaults();
        assert!(hub.feed_hotkey(input_i18n::Hotkey::AltShift).await);
        assert_eq!(hub.layout_indicator().await, "RU");
        assert!(hub.feed_hotkey(input_i18n::Hotkey::CtrlShift).await);
        assert_eq!(hub.layout_indicator().await, "EN");
    }

    #[test]
    fn default_backend_decision_is_micro_local() {
        let hub = SysControlHub::defaults();
        assert!(matches!(
            hub.llm_backend_decision(),
            aios_llm::BackendKind::MicroLocal
        ));
    }
}
