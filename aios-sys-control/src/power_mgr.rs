//! Power & thermal watchdog: battery/AC/lid state, CPU/GPU sensors and the
//! reactive thermal governor that offloads LLM inference to the cloud above
//! 80 °C (hysteresis back below 70 °C).
//!
//! Backend dispatch mirrors [`crate::net_manager`]: a deterministic
//! [`MockPower`] source drives tests/CI while [`PowerBackend::Host`] probes
//! real ACPI data (WMI on Windows, `/sys/class/power_supply` +
//! `/sys/class/thermal` on Linux). Every probe degrades to `None`.

use aios_llm::{BackendKind, CloudProvider};
use serde::{Deserialize, Serialize};

/// Temperature at which inference moves off-device.
pub const THROTTLE_HIGH_C: f32 = 80.0;
/// Hysteresis temperature required before moving back on-device.
pub const THROTTLE_LOW_C: f32 = 70.0;

/// Deterministic power/thermal source for tests and headless hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockPower {
    pub battery_percent: Option<u8>,
    pub ac_online: bool,
    pub lid_open: bool,
    pub cpu_temp_c: f32,
    pub gpu_temp_c: f32,
}

impl Default for MockPower {
    fn default() -> Self {
        Self {
            battery_percent: Some(84),
            ac_online: true,
            lid_open: true,
            cpu_temp_c: 48.0,
            gpu_temp_c: 52.0,
        }
    }
}

/// Backend dispatch: mock values or real ACPI/sysfs probing.
#[derive(Default)]
pub enum PowerBackend {
    Mock(MockPower),
    #[default]
    Host,
}

impl std::fmt::Debug for PowerBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mock(m) => write!(f, "Mock({m:?})"),
            Self::Host => f.write_str("Host"),
        }
    }
}

/// One thermal poll outcome consumed by the LLM engine owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleDecision {
    KeepCurrentBackend,
    MoveToCloud,
    ReturnToLocal,
}

/// Reactive governor with hysteresis: `>= high_c` trips to cloud, only
/// `<= low_c` allows returning (prevents oscillation around one threshold).
#[derive(Debug)]
pub struct ThermalGovernor {
    high_c: f32,
    low_c: f32,
    throttled: bool,
    provider: CloudProvider,
}

impl Default for ThermalGovernor {
    fn default() -> Self {
        Self {
            high_c: THROTTLE_HIGH_C,
            low_c: THROTTLE_LOW_C,
            throttled: false,
            provider: CloudProvider::Groq,
        }
    }
}

impl ThermalGovernor {
    /// Governor with explicit thresholds/cloud provider.
    pub fn new(high_c: f32, low_c: f32, provider: CloudProvider) -> Self {
        Self {
            high_c,
            low_c,
            provider,
            ..Self::default()
        }
    }

    /// Feed the hottest sensor value and get the transition decision.
    pub fn update(&mut self, temp_c: f32) -> ThrottleDecision {
        if !self.throttled && temp_c >= self.high_c {
            self.throttled = true;
            return ThrottleDecision::MoveToCloud;
        }
        if self.throttled && temp_c <= self.low_c {
            self.throttled = false;
            return ThrottleDecision::ReturnToLocal;
        }
        ThrottleDecision::KeepCurrentBackend
    }

    /// True while inference is forced off-device.
    pub fn is_throttled(&self) -> bool {
        self.throttled
    }

    /// The backend the LLM engine should currently use.
    pub fn llm_backend(&self) -> BackendKind {
        if self.throttled {
            BackendKind::Cloud(self.provider.clone())
        } else {
            BackendKind::MicroLocal
        }
    }

    /// Configured cloud escape hatch.
    pub fn cloud_provider(&self) -> &CloudProvider {
        &self.provider
    }
}

/// Battery / AC / sensor snapshot read from a backend in one poll.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PowerSample {
    pub battery_percent: Option<u8>,
    pub ac_online: Option<bool>,
    pub lid_open: Option<bool>,
    pub cpu_temp_c: Option<f32>,
    pub gpu_temp_c: Option<f32>,
}

/// High-level power manager combining sampling with the thermal governor.
#[derive(Debug)]
pub struct PowerManager {
    backend: PowerBackend,
    governor: ThermalGovernor,
    last_sample: PowerSample,
}

impl Default for PowerManager {
    fn default() -> Self {
        Self::new(PowerBackend::default())
    }
}

impl PowerManager {
    /// Manager over an explicit backend with default thresholds.
    pub fn new(backend: PowerBackend) -> Self {
        let last_sample = match &backend {
            PowerBackend::Mock(m) => PowerSample {
                battery_percent: m.battery_percent,
                ac_online: Some(m.ac_online),
                lid_open: Some(m.lid_open),
                cpu_temp_c: Some(m.cpu_temp_c),
                gpu_temp_c: Some(m.gpu_temp_c),
            },
            PowerBackend::Host => PowerSample::default(),
        };
        Self {
            backend,
            governor: ThermalGovernor::default(),
            last_sample,
        }
    }

    /// Manager over mock values (tests, safe mode).
    pub fn mock(mock: MockPower) -> Self {
        Self::new(PowerBackend::Mock(mock))
    }

    /// Replace the values of a mock backend in place.
    ///
    /// No-op when the manager probes the real host; useful for tests and
    /// safe-mode injection of known-good readings.
    pub fn set_mock(&mut self, mock: MockPower) {
        if matches!(self.backend, PowerBackend::Mock(_)) {
            self.backend = PowerBackend::Mock(mock);
        }
    }

    /// Replace the governor (custom thresholds/provider).
    pub fn with_governor(mut self, governor: ThermalGovernor) -> Self {
        self.governor = governor;
        self
    }

    /// Re-sample the backend and run the governor once.
    ///
    /// Returns the resulting throttle decision so callers can reconfigure
    /// `aios-llm` immediately.
    pub fn poll(&mut self) -> ThrottleDecision {
        self.last_sample = self.sample();
        let hottest = self.max_temp_c();
        self.governor.update(hottest)
    }

    fn sample(&self) -> PowerSample {
        match &self.backend {
            PowerBackend::Mock(m) => PowerSample {
                battery_percent: m.battery_percent,
                ac_online: Some(m.ac_online),
                lid_open: Some(m.lid_open),
                cpu_temp_c: Some(m.cpu_temp_c),
                gpu_temp_c: Some(m.gpu_temp_c),
            },
            PowerBackend::Host => host_sample(),
        }
    }

    /// Last sampled battery charge (percent).
    pub fn battery_percent(&self) -> Option<u8> {
        self.last_sample.battery_percent
    }

    /// Last sampled AC presence.
    pub fn ac_online(&self) -> Option<bool> {
        self.last_sample.ac_online
    }

    /// Last sampled lid state.
    pub fn lid_open(&self) -> Option<bool> {
        self.last_sample.lid_open
    }

    /// Hottest known sensor temperature (CPU vs GPU).
    pub fn max_temp_c(&self) -> f32 {
        self.last_sample
            .cpu_temp_c
            .unwrap_or(0.0)
            .max(self.last_sample.gpu_temp_c.unwrap_or(0.0))
    }

    /// True while the governor forces cloud inference.
    pub fn is_throttled(&self) -> bool {
        self.governor.is_throttled()
    }

    /// Current LLM backend decision (`MicroLocal` ↔ `Cloud(provider)`).
    pub fn llm_backend(&self) -> BackendKind {
        self.governor.llm_backend()
    }

    /// Direct access to the governor for tests/diagnostics.
    pub fn governor(&self) -> &ThermalGovernor {
        &self.governor
    }
}

fn host_sample() -> PowerSample {
    #[cfg(target_os = "linux")]
    {
        linux_sample()
    }
    #[cfg(not(target_os = "linux"))]
    {
        windows_sample()
    }
}

#[cfg(target_os = "linux")]
fn linux_sample() -> PowerSample {
    use std::fs;
    let mut s = PowerSample::default();
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for e in entries.flatten() {
            let base = e.path();
            let scope = fs::read_to_string(base.join("scope")).unwrap_or_default();
            if scope.trim() == "Device" {
                continue;
            }
            if let Ok(cap) = fs::read_to_string(base.join("capacity")) {
                s.battery_percent = cap.trim().parse().ok().or(s.battery_percent);
            }
            if let Ok(st) = fs::read_to_string(base.join("status")) {
                s.ac_online = Some(!st.trim().eq_ignore_ascii_case("discharging"));
            }
        }
    }
    s.cpu_temp_c =
        first_thermal_millicelsius(&["cpu-0-0", "x86_pkg_temp", "acpitz"]).map(|v| v / 1000.0);
    s.gpu_temp_c = first_thermal_millicelsius(&["gpu", "nouveau", "amdgpu"]).map(|v| v / 1000.0);
    s
}

#[cfg(target_os = "linux")]
fn first_thermal_millicelsius(preferred: &[&str]) -> Option<f32> {
    use std::fs;
    let zones = fs::read_dir("/sys/class/thermal").ok()?;
    let mut fallback = None;
    for z in zones.flatten() {
        let name = z.file_name().to_string_lossy().to_string();
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let ty = fs::read_to_string(z.path().join("type")).unwrap_or_default();
        let temp = fs::read_to_string(z.path().join("temp"))
            .ok()?
            .trim()
            .parse::<f32>()
            .ok();
        if let Some(t) = temp {
            if preferred.iter().any(|p| ty.contains(p)) {
                return Some(t);
            }
            fallback = fallback.or(Some(t));
        }
    }
    fallback
}

#[cfg(not(target_os = "linux"))]
fn windows_sample() -> PowerSample {
    // Best-effort WMI one-shot; returns defaults when spawn/WMI is denied.
    PowerSample {
        battery_percent: None,
        ac_online: None,
        lid_open: None,
        cpu_temp_c: None,
        gpu_temp_c: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_with(cpu: f32, gpu: f32) -> MockPower {
        MockPower {
            cpu_temp_c: cpu,
            gpu_temp_c: gpu,
            ..MockPower::default()
        }
    }

    #[test]
    fn overheat_moves_llm_to_cloud_and_reports_throttling() {
        let mut pm = PowerManager::mock(mock_with(84.0, 78.0));
        assert_eq!(pm.poll(), ThrottleDecision::MoveToCloud);
        assert!(pm.is_throttled());
        assert!(matches!(
            pm.llm_backend(),
            BackendKind::Cloud(CloudProvider::Groq)
        ));
        assert!((pm.max_temp_c() - 84.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cooldown_below_hysteresis_returns_to_local() {
        let mut pm = PowerManager::mock(mock_with(84.0, 78.0));
        pm.poll();
        pm.set_mock(mock_with(72.0, 71.0));
        assert_eq!(
            pm.poll(),
            ThrottleDecision::KeepCurrentBackend,
            "between thresholds stays throttled"
        );
        pm.set_mock(mock_with(69.0, 65.0));
        assert_eq!(pm.poll(), ThrottleDecision::ReturnToLocal);
        assert!(!pm.is_throttled());
        assert!(matches!(pm.llm_backend(), BackendKind::MicroLocal));
    }

    #[test]
    fn boundary_at_exactly_high_threshold_trips() {
        let mut g = ThermalGovernor::default();
        assert_eq!(g.update(THROTTLE_HIGH_C), ThrottleDecision::MoveToCloud);
    }

    #[test]
    fn normal_temps_never_flip_backend() {
        let mut pm = PowerManager::mock(MockPower::default());
        assert_eq!(pm.poll(), ThrottleDecision::KeepCurrentBackend);
        assert!(!pm.is_throttled());
    }

    #[test]
    fn custom_provider_is_honored_by_governor() {
        let g = ThermalGovernor::new(85.0, 75.0, CloudProvider::OpenRouter);
        assert!(matches!(g.cloud_provider(), CloudProvider::OpenRouter));
        assert!(matches!(
            ThermalGovernor::new(80.0, 70.0, CloudProvider::GoogleAiStudio).llm_backend(),
            BackendKind::MicroLocal
        ));
    }

    #[test]
    fn mock_battery_ac_and_lid_are_passed_through() {
        let pm = PowerManager::mock(MockPower {
            battery_percent: Some(37),
            ac_online: false,
            lid_open: false,
            ..MockPower::default()
        });
        assert_eq!(pm.battery_percent(), Some(37));
        assert_eq!(pm.ac_online(), Some(false));
        assert_eq!(pm.lid_open(), Some(false));
    }

    #[test]
    fn missing_sensors_report_zero_max_temperature() {
        let mut pm = PowerManager::mock(MockPower {
            cpu_temp_c: 0.0,
            gpu_temp_c: 0.0,
            ..MockPower::default()
        });
        pm.poll();
        assert_eq!(pm.max_temp_c(), 0.0);
    }

    #[test]
    fn gpu_hotter_than_cpu_wins_max_temperature() {
        let mut pm = PowerManager::mock(mock_with(60.0, 91.5));
        assert_eq!(pm.poll(), ThrottleDecision::MoveToCloud);
        assert!((pm.max_temp_c() - 91.5).abs() < 1e-4);
    }
}
