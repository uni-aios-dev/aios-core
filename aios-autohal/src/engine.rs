use crate::catalog::generic_fallback;
use crate::fetcher::{DriverFetcher, FetchedDriver};
use crate::fingerprint::{extract_fingerprints, HardwareFingerprint};
use crate::manifest::DriverManifest;
use crate::registry::DriverStore;
use aios_hal::hardware::HardwareProfile;
use aios_security::capability::{Capability, CapabilityToken};
use aios_wasm::{IsolationConfig, SandboxConfig, WasmBlock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Consecutive failures after which the dedicated driver is disabled and the
/// device is switched to the Generic Fallback Driver (auto-rollback).
pub const DEFAULT_MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Driver id of the built-in safe-mode driver used when a dedicated driver
/// crashes or is unavailable.
pub const GENERIC_FALLBACK_ID: &str = "driver.generic.fallback";

const MAX_TOASTS: usize = 32;

/// Lifecycle state of one provisioned device/driver pair. The `label()` and
/// `severity()` pairs are the shared TUI/GUI vocabulary so both UIs render the
/// same status text and color family (100% parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverState {
    /// Dedicated WASM driver loaded and running.
    Active,
    /// Fetching from a remote registry / builtin catalog.
    Downloading,
    /// Adapting C/Rust source -> `wasm32-wasi`.
    Compiling,
    /// Generic fallback driver in use (no capabilities).
    Generic,
    /// Provisioning failed and no driver could be brought up.
    Failed,
    /// Switched off after 3+ consecutive failures (auto-rollback).
    RolledBack,
}

impl DriverState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Downloading => "Downloading...",
            Self::Compiling => "Compiling...",
            Self::Generic => "Fallback/Generic",
            Self::Failed => "Failed",
            Self::RolledBack => "Rolled Back",
        }
    }

    /// Coarse color family shared by the TUI and GUI color maps.
    pub fn severity(&self) -> Severity {
        match self {
            Self::Active | Self::Generic => Severity::Good,
            Self::Downloading | Self::Compiling => Severity::Busy,
            Self::RolledBack => Severity::Warn,
            Self::Failed => Severity::Bad,
        }
    }
}

/// Color family for a driver status; each UI maps it to its own palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Good,
    Busy,
    Warn,
    Bad,
}

/// One provisioned device entry (in-memory bookkeeping + UI source of truth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceDriver {
    pub fingerprint: HardwareFingerprint,
    pub driver_id: String,
    pub manifest: Option<DriverManifest>,
    pub state: DriverState,
    pub failures: u32,
    /// Download/compile/instantiate progress in percent (0..100).
    pub progress: u32,
    /// Capabilities actually granted (after user overrides).
    pub capabilities: Vec<Capability>,
    pub last_error: Option<String>,
    pub updated_ms: u64,
}

impl DeviceDriver {
    /// UI snapshot of the driver name.
    pub fn driver_name(&self) -> &str {
        self.manifest
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or(&self.driver_id)
    }
}

/// Cheap, `Clone`-friendly snapshot handed to the TUI/GUI renderers.
#[derive(Debug, Clone)]
pub struct DeviceView {
    pub fingerprint: HardwareFingerprint,
    pub driver_id: String,
    pub driver_name: String,
    pub source: Option<String>,
    pub state: DriverState,
    pub failures: u32,
    pub progress: u32,
    pub capabilities: Vec<Capability>,
    pub last_error: Option<String>,
}

impl From<&DeviceDriver> for DeviceView {
    fn from(d: &DeviceDriver) -> Self {
        Self {
            fingerprint: d.fingerprint.clone(),
            driver_id: d.driver_id.clone(),
            driver_name: d.driver_name().to_string(),
            source: d.manifest.as_ref().map(|m| m.source.to_string()),
            state: d.state,
            failures: d.failures,
            progress: d.progress,
            capabilities: d.capabilities.clone(),
            last_error: d.last_error.clone(),
        }
    }
}

/// Kind of a toast notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

/// A transient UI notification, e.g. hot-plug detection.
#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created_ms: u64,
}

impl Toast {
    fn new(kind: ToastKind, message: String) -> Self {
        Self {
            message,
            kind,
            created_ms: now_ms(),
        }
    }
}

/// Full engine configuration; `store_root` doubles as the `AIOS://store/drivers/`
/// backing directory so the persisted `DriverStore` lives at the same place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Directory hosting `index.json` + per-driver `driver.json`/`driver.wasm`.
    pub store_root: PathBuf,
    /// Wasmtime sandbox limits for driver instances.
    pub sandbox: SandboxConfig,
    /// Isolation boundary applied to every driver instance.
    pub isolation: IsolationConfig,
    /// On-demand fetch sources (builtin catalog, custom store, mirrors).
    pub fetcher: crate::fetcher::FetcherConfig,
    /// Failures before auto-rollback to the generic driver.
    pub max_consecutive_failures: u32,
    /// TTL of the issued capability tokens.
    pub capability_ttl_ms: u64,
    /// Secret used to sign capability tokens (never logged).
    pub issuer_secret: Vec<u8>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            store_root: default_driver_root(),
            sandbox: SandboxConfig::default(),
            isolation: IsolationConfig::restrictive(),
            fetcher: crate::fetcher::FetcherConfig::default(),
            max_consecutive_failures: DEFAULT_MAX_CONSECUTIVE_FAILURES,
            capability_ttl_ms: 3_600_000,
            issuer_secret: b"aios-autohal-driver-issuer-v1".to_vec(),
        }
    }
}

fn default_driver_root() -> PathBuf {
    if let Ok(dir) = std::env::var("AIOS_DATA_DIR") {
        return PathBuf::from(dir).join("drivers");
    }
    PathBuf::from("drivers")
}

/// Errors raised by the provisioning pipeline.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("driver store error: {0}")]
    Store(String),
    #[error("no driver found for {0}")]
    NotFound(String),
    #[error("driver fetch failed: {0}")]
    Fetch(String),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("source adaptation failed: {0}")]
    Adapt(String),
    #[error("wasm sandbox error: {0}")]
    Sandbox(String),
    #[error("driver call failed: {0}")]
    DriverCall(String),
}

/// The auto-provisioning engine: owns the driver store, the fetcher, the
/// loaded WASM instances and the shared UI state (devices + toasts).
pub struct AutohalEngine {
    config: EngineConfig,
    fetcher: DriverFetcher,
    store: DriverStore,
    devices: Vec<DeviceDriver>,
    instances: HashMap<String, WasmBlock>,
    toasts: VecDeque<Toast>,
    next_block_id: u32,
}

impl AutohalEngine {
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        let store = DriverStore::new(config.store_root.clone()).map_err(EngineError::Store)?;
        let fetcher = DriverFetcher::new(config.fetcher.clone());
        Ok(Self {
            config,
            fetcher,
            store,
            devices: Vec::new(),
            instances: HashMap::new(),
            toasts: VecDeque::new(),
            next_block_id: 1,
        })
    }

    /// Build an engine with an injected fetcher (used to install a mock
    /// transport in tests).
    pub fn with_fetcher(config: EngineConfig, fetcher: DriverFetcher) -> Result<Self, EngineError> {
        let store = DriverStore::new(config.store_root.clone()).map_err(EngineError::Store)?;
        Ok(Self {
            config,
            fetcher,
            store,
            devices: Vec::new(),
            instances: HashMap::new(),
            toasts: VecDeque::new(),
            next_block_id: 1,
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn store(&self) -> &DriverStore {
        &self.store
    }

    pub fn devices(&self) -> &[DeviceDriver] {
        &self.devices
    }

    pub fn toasts(&self) -> &VecDeque<Toast> {
        &self.toasts
    }

    /// Drain at most `max` toasts (oldest first) for the UI toast strip.
    pub fn pop_toasts(&mut self, max: usize) -> Vec<Toast> {
        let take = max.min(self.toasts.len());
        self.toasts.drain(..take).collect()
    }

    /// Snapshot for the TUI/GUI renderers (keeps the UI decoupled from the
    /// live `WasmBlock` instances).
    pub fn device_views(&self) -> Vec<DeviceView> {
        self.devices.iter().map(DeviceView::from).collect()
    }

    /// Detect every device in a hardware snapshot and provision the ones that
    /// are not yet tracked. Returns the detected fingerprints.
    pub fn rescan(&mut self, profile: &HardwareProfile) -> Vec<HardwareFingerprint> {
        let fingerprints = extract_fingerprints(profile);
        for fp in &fingerprints {
            if self.devices.iter().any(|d| &d.fingerprint == fp) {
                continue;
            }
            self.provision_blocking(fp.clone());
        }
        fingerprints
    }

    /// Synchronous wrapper of [`AutohalEngine::provision`] for block/IPC and
    /// UI paths that do not own a runtime.
    pub fn provision_blocking(&mut self, fp: HardwareFingerprint) {
        let rt = tokio::runtime::Runtime::new().map_err(|e| EngineError::Sandbox(e.to_string()));
        match rt {
            Ok(rt) => {
                let _ = rt.block_on(self.provision(fp));
            }
            Err(e) => {
                self.push_toast(
                    ToastKind::Error,
                    format!(
                        "[Hardware] {} -> runtime error ({e}) -> Generic Fallback",
                        fp.display_name()
                    ),
                );
                self.activate_generic(&fp);
            }
        }
    }

    /// The full provisioning pipeline for one fingerprint. On any failure the
    /// device is brought up on the Generic Fallback Driver with a toast.
    pub async fn provision(&mut self, fp: HardwareFingerprint) -> Result<(), EngineError> {
        self.push_toast(
            ToastKind::Info,
            format!(
                "[Hardware] Detected {} -> looking up driver...",
                fp.display_name()
            ),
        );
        self.set_state(&fp, DriverState::Downloading);
        self.set_progress(&fp, 5);

        match self.provision_dedicated(&fp).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let reason = e.to_string();
                log::warn!(
                    "AUTOHAL: provisioning {} failed: {reason}",
                    fp.display_name()
                );
                self.push_toast(
                    ToastKind::Error,
                    format!(
                        "[Hardware] {} -> {reason} -> Generic Fallback",
                        fp.display_name()
                    ),
                );
                self.activate_generic(&fp);
                Err(e)
            }
        }
    }

    /// Steps 1-5 of the pipeline for a dedicated driver (no fallback).
    async fn provision_dedicated(&mut self, fp: &HardwareFingerprint) -> Result<(), EngineError> {
        // Step 1: local store lookup (instant offline reuse).
        if let Some(driver_id) = self.store.index().get(&fp.key()).map(str::to_string) {
            if let Some(manifest) = self.store.load_manifest(&driver_id) {
                if let Ok(wasm) = self.store.load_wasm(&driver_id) {
                    if manifest_hash_matches(&manifest, &wasm) {
                        self.set_progress(fp, 30);
                        return self.activate(fp.clone(), driver_id, manifest, wasm);
                    }
                }
                log::warn!("AUTOHAL: cached driver {driver_id} failed validation, re-fetching");
                self.store.remove_driver(&driver_id);
            }
        }

        // Step 2: network fetch / builtin catalog.
        self.set_progress(fp, 20);
        let fetched = self
            .fetcher
            .find_driver(fp)
            .await
            .map_err(|e| EngineError::Fetch(e.to_string()))?;
        let was_source = matches!(&fetched, FetchedDriver::Source { .. });

        // Step 3: adapt source drivers to wasm32-wasi.
        let (mut manifest, bytes) = match fetched {
            FetchedDriver::Wasm { manifest, bytes } => {
                self.set_progress(fp, 50);
                (manifest, bytes)
            }
            FetchedDriver::Source {
                manifest,
                language,
                code,
            } => {
                self.set_state(fp, DriverState::Compiling);
                self.set_progress(fp, 55);
                let adapted = self.fetcher.adapter().adapt(&code, language);
                let compiled = self
                    .fetcher
                    .adapter()
                    .compile(&adapted, language, &manifest.entry_point)
                    .map_err(|e| EngineError::Adapt(e.to_string()))?;
                self.set_progress(fp, 75);
                (manifest, compiled)
            }
        };

        // Step 4: SHA-256 validation. Source drivers are hashed on the compiled
        // artifact (the upstream hash describes source, not wasm).
        if was_source {
            manifest.hash_sha256 = hex::encode(Sha256::digest(&bytes));
        } else if !manifest.hash_sha256.is_empty() {
            let actual = hex::encode(Sha256::digest(&bytes));
            if actual != manifest.hash_sha256 {
                return Err(EngineError::HashMismatch {
                    expected: manifest.hash_sha256,
                    actual,
                });
            }
        }

        // Step 5: cache & register, then instantiate under the sandbox.
        self.store
            .save_driver(&fp.key(), &manifest, &bytes)
            .map_err(EngineError::Store)?;
        self.set_progress(fp, 90);
        self.activate(fp.clone(), manifest.id.clone(), manifest, bytes)
    }

    /// Instantiate a driver module in the sandbox, grant capability tokens and
    /// mark the device Active.
    fn activate(
        &mut self,
        fp: HardwareFingerprint,
        driver_id: String,
        manifest: DriverManifest,
        wasm: Vec<u8>,
    ) -> Result<(), EngineError> {
        let version = manifest.version.clone();
        let entry_point = manifest.entry_point.clone();
        let driver_name = manifest.name.clone();

        let mut block = WasmBlock::new(
            driver_id.clone(),
            version,
            &wasm,
            self.config.sandbox.clone(),
            self.config.isolation.clone(),
        )
        .map_err(|e| EngineError::Sandbox(e.to_string()))?;
        let mut store = block
            .create_store()
            .map_err(|e| EngineError::Sandbox(e.to_string()))?;
        block
            .instantiate(&mut store)
            .map_err(|e| EngineError::Sandbox(e.to_string()))?;
        block
            .call_func(&mut store, &entry_point, &[])
            .map_err(|e| EngineError::DriverCall(e.to_string()))?;

        let block_id = self.next_block_id;
        self.next_block_id += 1;
        let caps = self.granted_capabilities(&manifest, &driver_id);
        let _token = CapabilityToken::new(
            block_id,
            caps.clone(),
            self.config.capability_ttl_ms,
            &self.config.issuer_secret,
        );

        self.instances.insert(instance_key(&fp, &driver_id), block);
        self.upsert_device(
            fp.clone(),
            driver_id,
            Some(manifest),
            DriverState::Active,
            caps.clone(),
            100,
            None,
        );

        self.push_toast(
            ToastKind::Success,
            format!(
                "[Hardware] {} -> {} [OK] ({} caps)",
                fp.display_name(),
                driver_name,
                caps.len()
            ),
        );
        log::info!(
            "AUTOHAL: {} active on {} ({} caps)",
            driver_name,
            fp.display_name(),
            caps.len()
        );
        Ok(())
    }

    /// Bring a device up on the built-in Generic Fallback Driver (safe mode,
    /// no capabilities). Used when the dedicated driver is unavailable or was
    /// rolled back.
    pub fn activate_generic(&mut self, fp: &HardwareFingerprint) {
        let fb = generic_fallback();
        let driver_id = fb.manifest.id.clone();
        let version = fb.manifest.version.clone();

        let mut block = match WasmBlock::from_wat(
            driver_id.clone(),
            version,
            fb.wat,
            self.config.sandbox.clone(),
            self.config.isolation.clone(),
        ) {
            Ok(b) => b,
            Err(e) => {
                self.push_toast(
                    ToastKind::Error,
                    format!(
                        "[Hardware] {} -> generic fallback failed to start ({e})",
                        fp.display_name()
                    ),
                );
                self.upsert_device(
                    fp.clone(),
                    driver_id,
                    Some(fb.manifest),
                    DriverState::Failed,
                    Vec::new(),
                    100,
                    Some(e.to_string()),
                );
                return;
            }
        };

        match block.create_store().and_then(|mut st| {
            block.instantiate(&mut st)?;
            block.call_func(&mut st, "_start_driver", &[])?;
            Ok(())
        }) {
            Ok(()) => {
                self.instances.insert(instance_key(fp, &driver_id), block);
                self.upsert_device(
                    fp.clone(),
                    driver_id,
                    Some(fb.manifest),
                    DriverState::Generic,
                    Vec::new(),
                    100,
                    None,
                );
                self.push_toast(
                    ToastKind::Warn,
                    format!("[Hardware] {} -> Generic Fallback [OK]", fp.display_name()),
                );
            }
            Err(e) => {
                self.push_toast(
                    ToastKind::Error,
                    format!(
                        "[Hardware] {} -> generic fallback error ({e})",
                        fp.display_name()
                    ),
                );
                self.upsert_device(
                    fp.clone(),
                    driver_id,
                    Some(fb.manifest),
                    DriverState::Failed,
                    Vec::new(),
                    100,
                    Some(e.to_string()),
                );
            }
        }
    }

    /// Self-healing: record a failure for `driver_id` (e.g. a crashed wasm
    /// call reported by the watchdog/supervisor). After
    /// `max_consecutive_failures` the driver is disabled for every device
    /// using it, removed from the index and replaced with the generic driver.
    /// Returns the rolled-back driver id, if any.
    pub fn record_failure(&mut self, driver_id: &str) -> Option<String> {
        if driver_id == GENERIC_FALLBACK_ID {
            return None;
        }
        let failures = self.store.index_mut().bump_failure(driver_id);
        let max = self.config.max_consecutive_failures.max(1);
        log::warn!("AUTOHAL: driver {driver_id} failure {failures}/{max}");

        let mut rolled_back: Vec<HardwareFingerprint> = Vec::new();
        for dev in self.devices.iter_mut().filter(|d| d.driver_id == driver_id) {
            dev.failures = failures;
            dev.updated_ms = now_ms();
            if failures >= max {
                dev.state = DriverState::RolledBack;
                rolled_back.push(dev.fingerprint.clone());
            }
        }

        if !rolled_back.is_empty() {
            for fp in &rolled_back {
                self.instances.remove(&instance_key(fp, driver_id));
                self.store.index_mut().remove(&fp.key());
            }
            self.store
                .save_index()
                .map_err(|e| log::warn!("AUTOHAL: index save failed: {e}"))
                .ok();
            self.push_toast(
                ToastKind::Warn,
                format!("[Hardware] {driver_id} failed {failures}x -> rolling back to Generic"),
            );
            for fp in &rolled_back {
                self.activate_generic(fp);
            }
            return Some(driver_id.to_string());
        }

        self.store
            .save_index()
            .map_err(|e| log::warn!("AUTOHAL: index save failed: {e}"))
            .ok();
        None
    }

    /// User-requested rollback of one device to the generic driver (GUI/TUI
    /// [Rollback to Generic] button).
    pub fn rollback_to_generic(&mut self, fp: &HardwareFingerprint) {
        let driver_id = self
            .devices
            .iter()
            .find(|d| &d.fingerprint == fp)
            .map(|d| d.driver_id.clone());
        if let Some(id) = driver_id {
            self.instances.remove(&instance_key(fp, &id));
            self.store.index_mut().remove(&fp.key());
        }
        self.activate_generic(fp);
    }

    /// Remove a driver from the store and every device that uses it.
    /// The generic fallback can never be uninstalled.
    pub fn uninstall_driver(&mut self, driver_id: &str) -> bool {
        if driver_id == GENERIC_FALLBACK_ID {
            return false;
        }
        let keys: Vec<String> = self
            .devices
            .iter()
            .filter(|d| d.driver_id == driver_id)
            .map(|d| instance_key(&d.fingerprint, &d.driver_id))
            .collect();
        for k in keys {
            self.instances.remove(&k);
        }
        self.devices.retain(|d| d.driver_id != driver_id);
        self.store.remove_driver(driver_id)
    }

    /// Persist a capability override for a driver (security matrix checkboxes
    /// in the GUI). Applied on the next (re)grant of that driver.
    pub fn set_cap_override(&mut self, driver_id: &str, caps: Vec<Capability>) {
        self.store.index_mut().set_cap_override(driver_id, caps);
        let granted = match self.devices.iter().find(|d| d.driver_id == driver_id) {
            Some(dev) => match &dev.manifest {
                Some(m) => self.granted_capabilities(m, driver_id),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        if let Some(dev) = self.devices.iter_mut().find(|d| d.driver_id == driver_id) {
            dev.capabilities = granted;
            dev.updated_ms = now_ms();
        }
        let _ = self.store.save_index();
    }

    /// Effective capabilities for a driver: user override wins over the
    /// manifest's `required_capabilities`.
    fn granted_capabilities(&self, manifest: &DriverManifest, driver_id: &str) -> Vec<Capability> {
        match self.store.index().cap_override(driver_id) {
            Some(caps) => caps.to_vec(),
            None => manifest.required_capabilities.clone(),
        }
    }

    fn push_toast(&mut self, kind: ToastKind, message: String) {
        if self.toasts.len() >= MAX_TOASTS {
            self.toasts.pop_front();
        }
        self.toasts.push_back(Toast::new(kind, message));
    }

    fn set_state(&mut self, fp: &HardwareFingerprint, state: DriverState) {
        if let Some(dev) = self.devices.iter_mut().find(|d| &d.fingerprint == fp) {
            dev.state = state;
            dev.updated_ms = now_ms();
        }
    }

    fn set_progress(&mut self, fp: &HardwareFingerprint, progress: u32) {
        if let Some(dev) = self.devices.iter_mut().find(|d| &d.fingerprint == fp) {
            dev.progress = progress;
            dev.updated_ms = now_ms();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_device(
        &mut self,
        fp: HardwareFingerprint,
        driver_id: String,
        manifest: Option<DriverManifest>,
        state: DriverState,
        capabilities: Vec<Capability>,
        progress: u32,
        last_error: Option<String>,
    ) {
        match self.devices.iter_mut().find(|d| d.fingerprint == fp) {
            Some(dev) => {
                dev.driver_id = driver_id;
                dev.manifest = manifest;
                dev.state = state;
                dev.progress = progress;
                dev.capabilities = capabilities;
                dev.last_error = last_error;
                dev.updated_ms = now_ms();
            }
            None => self.devices.push(DeviceDriver {
                fingerprint: fp,
                driver_id,
                manifest,
                state,
                failures: 0,
                progress,
                capabilities,
                last_error,
                updated_ms: now_ms(),
            }),
        }
    }
}

fn manifest_hash_matches(manifest: &DriverManifest, wasm: &[u8]) -> bool {
    manifest.hash_sha256.is_empty() || hex::encode(Sha256::digest(wasm)) == manifest.hash_sha256
}

fn instance_key(fp: &HardwareFingerprint, driver_id: &str) -> String {
    format!("{driver_id}::{}", fp.key())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::GENERIC_WAT;
    use crate::fetcher::{FetcherConfig, Transport};
    use crate::fingerprint::BusType;
    use crate::manifest::DriverSource;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};

    fn c270_fp() -> HardwareFingerprint {
        HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x046D,
            device_id: 0x0825,
            class_code: 0,
            serial_or_acpi: None,
        }
    }

    fn unknown_fp() -> HardwareFingerprint {
        HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x9999,
            device_id: 0x0001,
            class_code: 0,
            serial_or_acpi: None,
        }
    }

    fn tmp_config() -> (EngineConfig, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            store_root: dir.path().join("drivers"),
            ..Default::default()
        };
        (config, dir)
    }

    #[test]
    fn test_default_config_has_three_strikes() {
        assert_eq!(EngineConfig::default().max_consecutive_failures, 3);
        assert_eq!(DEFAULT_MAX_CONSECUTIVE_FAILURES, 3);
    }

    #[test]
    fn test_provision_builtin_c270() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(c270_fp());

        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == c270_fp())
            .expect("device tracked");
        assert_eq!(dev.state, DriverState::Active);
        assert_eq!(dev.driver_id, "driver.usb.046d.0825");
        assert_eq!(dev.progress, 100);
        assert!(dev.manifest.is_some());
        assert!(!dev.capabilities.is_empty());

        // Cached for offline reuse.
        assert_eq!(
            engine.store().index().get(&c270_fp().key()),
            Some("driver.usb.046d.0825")
        );
        let cached = engine
            .store()
            .load_wasm("driver.usb.046d.0825")
            .expect("wasm cached");
        assert!(!cached.is_empty());
    }

    #[test]
    fn test_unknown_device_falls_back_to_generic() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(unknown_fp());

        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == unknown_fp())
            .expect("device tracked");
        assert_eq!(dev.state, DriverState::Generic);
        assert_eq!(dev.driver_id, GENERIC_FALLBACK_ID);
        assert!(dev.capabilities.is_empty());
        assert_eq!(
            dev.manifest.as_ref().unwrap().source,
            DriverSource::GenericFallback
        );
    }

    #[test]
    fn test_offline_reuse_from_cache() {
        let (config, dir) = tmp_config();
        {
            let mut engine = AutohalEngine::new(config.clone()).unwrap();
            engine.provision_blocking(c270_fp());
        }
        // Second engine instance reuses the persisted store (no network path).
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(c270_fp());
        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == c270_fp())
            .unwrap();
        assert_eq!(dev.state, DriverState::Active);
        assert_eq!(dev.driver_id, "driver.usb.046d.0825");
        drop(dir);
    }

    #[test]
    fn test_auto_rollback_after_three_failures() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(c270_fp());
        let driver_id = "driver.usb.046d.0825";

        assert!(engine.record_failure(driver_id).is_none());
        assert!(engine.record_failure(driver_id).is_none());
        let rolled = engine.record_failure(driver_id);
        assert_eq!(rolled.as_deref(), Some(driver_id));

        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == c270_fp())
            .unwrap();
        assert_eq!(dev.state, DriverState::Generic);
        assert_eq!(dev.driver_id, GENERIC_FALLBACK_ID);
        assert!(engine.store().index().get(&c270_fp().key()).is_none());
        // A toast announces the rollback.
        let has_rollback = engine
            .toasts()
            .iter()
            .any(|t| t.message.contains("rolling back to Generic"));
        assert!(has_rollback, "rollback toast expected");
    }

    #[test]
    fn test_record_failure_ignores_generic() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(unknown_fp());
        assert!(engine.record_failure(GENERIC_FALLBACK_ID).is_none());
    }

    #[test]
    fn test_rescan_provisions_all_devices() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        let profile = aios_hal::hardware::HardwareProfile::mock_modern();
        let fps = engine.rescan(&profile);
        assert!(!fps.is_empty());
        assert_eq!(engine.devices().len(), fps.len());
        // Logitech receiver (usb 046d:c52b) is in the builtin catalog.
        let logitech = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint.vendor_id == 0x046D)
            .expect("logitech device provisioned");
        assert_eq!(logitech.state, DriverState::Active);
        // Unknown PCI/NVMe devices must be on the generic fallback.
        for dev in engine.devices() {
            assert!(
                dev.state == DriverState::Active || dev.state == DriverState::Generic,
                "unexpected state {:?} for {}",
                dev.state,
                dev.driver_id
            );
        }
    }

    #[test]
    fn test_hash_mismatch_falls_back_to_generic() {
        let manifest = crate::manifest::DriverManifest {
            id: "driver.usb.046d.0825".into(),
            name: "Tampered Remote Driver".into(),
            version: "1.0.0".into(),
            description: "".into(),
            supported_hardware: vec![crate::manifest::SupportedHardware {
                bus: "usb".into(),
                vendor_id: Some(0x046D),
                device_id: Some(0x0825),
            }],
            required_capabilities: vec![Capability::HwAccess],
            hash_sha256: hex::encode(Sha256::digest(b"expected")),
            entry_point: "_start_driver".into(),
            source: crate::manifest::DriverSource::CustomStore,
            size_bytes: 1,
        };
        let map: HashMap<String, Vec<u8>> = HashMap::from([
            (
                "https://store.test/drivers/driver.usb.046d.0825/driver.json".to_string(),
                manifest.to_json().unwrap().into_bytes(),
            ),
            (
                "https://store.test/drivers/driver.usb.046d.0825/driver.wasm".to_string(),
                b"tampered".to_vec(),
            ),
        ]);
        let fetcher = DriverFetcher::with_transport(
            FetcherConfig {
                use_builtin_catalog: false,
                registry_url: Some("https://store.test".into()),
                ..Default::default()
            },
            Transport::Mock(Arc::new(Mutex::new(map))),
        );
        let (mut config, _dir) = tmp_config();
        config.fetcher = FetcherConfig {
            use_builtin_catalog: false,
            registry_url: Some("https://store.test".into()),
            ..Default::default()
        };
        let mut engine = AutohalEngine::with_fetcher(config, fetcher).unwrap();
        engine.provision_blocking(c270_fp());

        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == c270_fp())
            .unwrap();
        assert_eq!(
            dev.state,
            DriverState::Generic,
            "hash mismatch must fall back"
        );
        assert!(engine
            .toasts()
            .iter()
            .any(|t| t.message.contains("hash mismatch")));
    }

    #[test]
    fn test_uninstall_removes_driver_and_devices() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(c270_fp());
        assert!(engine.uninstall_driver("driver.usb.046d.0825"));
        assert!(engine.devices().is_empty());
        assert!(!engine.uninstall_driver(GENERIC_FALLBACK_ID));
    }

    #[test]
    fn test_cap_override_applies_to_device() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(c270_fp());
        engine.set_cap_override("driver.usb.046d.0825", vec![Capability::HwAccess]);
        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == c270_fp())
            .unwrap();
        assert_eq!(dev.capabilities, vec![Capability::HwAccess]);
        assert_eq!(
            engine.store().index().cap_override("driver.usb.046d.0825"),
            Some(&[Capability::HwAccess][..])
        );
    }

    #[test]
    fn test_device_view_snapshot() {
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(c270_fp());
        let views = engine.device_views();
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.state, DriverState::Active);
        assert_eq!(view.driver_name, "Logitech C270 Webcam");
        assert_eq!(view.source.as_deref(), Some("Builtin"));
    }

    #[test]
    fn test_generic_wat_is_valid() {
        // The fallback template must compile in the sandbox (smoke test).
        let (config, _dir) = tmp_config();
        let mut engine = AutohalEngine::new(config).unwrap();
        engine.provision_blocking(unknown_fp());
        let dev = engine
            .devices()
            .iter()
            .find(|d| d.fingerprint == unknown_fp())
            .unwrap();
        assert_eq!(dev.state, DriverState::Generic);
        assert!(!GENERIC_WAT.is_empty());
    }

    #[test]
    fn test_toasts_bounded() {
        let mut engine = AutohalEngine::new(tmp_config().0).unwrap();
        for i in 0..(MAX_TOASTS + 10) as u64 {
            engine.push_toast(ToastKind::Info, format!("toast {i}"));
        }
        assert_eq!(engine.toasts().len(), MAX_TOASTS);
        let drained = engine.pop_toasts(MAX_TOASTS);
        assert_eq!(drained.len(), MAX_TOASTS);
        assert_eq!(engine.toasts().len(), 0);
    }
}
