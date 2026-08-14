//! Live device hot-plug monitoring.
//!
//! A background [`HotplugMonitor`] thread re-detects the attached hardware
//! snapshot and diffs the fingerprint set against the previous poll. Changed
//! devices are emitted as [`HotplugEvent`]s (`Added`/`Removed`) over an `mpsc`
//! channel; the owner (kernel TUI or GUI main loop) drains the events on its
//! tick and applies them to the [`crate::engine::AutohalEngine`], which is
//! deliberately kept on the UI thread because it owns non-`Send` `WasmBlock`
//! instances.
//!
//! The full `HardwareProfile::detect()` is comparatively expensive, so the
//! monitor never runs it unconditionally. On Linux a cheap change signal
//! ([`cheap_signal`]) hashes the mtimes of the `/sys/bus/{usb,pci,nvme}/devices`
//! directories — those only change when a device is attached or removed — so a
//! full detection runs the moment the device tree actually moves. On platforms
//! without such a signal the monitor falls back to the fixed-interval full scan
//! cadence ([`HotplugConfig::poll_ms`]).

use crate::fingerprint::{extract_fingerprints, HardwareFingerprint};
use aios_hal::hardware::HardwareProfile;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A single hot-plug transition detected by the monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotplugEvent {
    /// A device that was not present at the previous poll is now attached.
    Added(HardwareFingerprint),
    /// A device that was present at the previous poll is no longer attached.
    Removed(HardwareFingerprint),
}

impl HotplugEvent {
    /// The fingerprint that changed state.
    pub fn fingerprint(&self) -> &HardwareFingerprint {
        match self {
            Self::Added(fp) | Self::Removed(fp) => fp,
        }
    }

    /// Whether this event describes a device arrival (`true`) or removal (`false`).
    pub fn is_added(&self) -> bool {
        matches!(self, Self::Added(_))
    }
}

/// Tunables for the [`HotplugMonitor`].
#[derive(Debug, Clone)]
pub struct HotplugConfig {
    /// Fixed-interval full re-detection cadence in milliseconds. Also the
    /// safety net on Linux: a full scan runs at least this often even when the
    /// cheap change signal reports no device-tree movement.
    pub poll_ms: u64,
    /// Cadence of the cheap change-signal check (Linux) / cadence of the full
    /// scan scheduling on platforms without a signal. Kept small (250 ms) since
    /// it is far cheaper than a full detection.
    pub signal_poll_ms: u64,
}

impl Default for HotplugConfig {
    fn default() -> Self {
        Self {
            poll_ms: 1000,
            signal_poll_ms: 250,
        }
    }
}

/// Fold the mtimes of the given directories into a single hash. `None` when no
/// directory is readable, so a missing tree never counts as a change.
pub fn dir_signal_hash(paths: &[&Path]) -> Option<u64> {
    let mut hash: u64 = 0;
    let mut seen = false;
    for dir in paths {
        if let Ok(metadata) = std::fs::metadata(dir) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(ns) = modified.duration_since(std::time::UNIX_EPOCH) {
                    hash = hash.rotate_left(17) ^ (ns.as_nanos() as u64);
                    seen = true;
                }
            }
        }
    }
    if seen {
        Some(hash)
    } else {
        None
    }
}

/// Cheap platform change signal: `(changed, next_state)`.
///
/// Linux hashes the mtimes of `/sys/bus/{usb,pci,nvme}/devices` (std fs only);
/// these directories change exactly when a device is attached or removed, so a
/// full `HardwareProfile::detect()` can be skipped while nothing moved. On other
/// platforms there is no cheap signal (`None`), which forces the fixed-interval
/// full-scan cadence.
fn cheap_signal(prev: Option<u64>) -> (bool, Option<u64>) {
    #[cfg(target_os = "linux")]
    {
        let paths = [
            Path::new("/sys/bus/usb/devices"),
            Path::new("/sys/bus/pci/devices"),
            Path::new("/sys/bus/nvme/devices"),
        ];
        let state = dir_signal_hash(&paths);
        (state != prev, state)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = prev;
        (true, None)
    }
}

/// Pure set-diff used by the monitor and by unit tests: every fingerprint that
/// changed between two consecutive polls becomes an event.
///
/// Baseline warm-up is the caller's concern: [`HotplugMonitor::start`] skips the
/// first diff so startup does not look like a mass hot-plug.
pub fn diff_fingerprints(
    previous: &HashSet<HardwareFingerprint>,
    current: &HashSet<HardwareFingerprint>,
) -> Vec<HotplugEvent> {
    let mut events = Vec::new();
    for fp in current.difference(previous) {
        events.push(HotplugEvent::Added(fp.clone()));
    }
    for fp in previous.difference(current) {
        events.push(HotplugEvent::Removed(fp.clone()));
    }
    events
}

/// Background thread that turns hardware re-detection into a stream of
/// [`HotplugEvent`]s. Dropping the monitor stops the thread.
pub struct HotplugMonitor {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<HotplugEvent>,
}

impl HotplugMonitor {
    /// Start a monitor thread with the given polling configuration.
    pub fn start(config: HotplugConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut previous: HashSet<HardwareFingerprint> = HashSet::new();
            scan_and_emit(&mut previous, &tx);
            let mut last_full = Instant::now();
            let poll_dur = Duration::from_millis(config.poll_ms);
            let signal_dur = Duration::from_millis(config.signal_poll_ms);
            let (_, initial_signal) = cheap_signal(None);
            let have_cheap = initial_signal.is_some();
            let mut prev_signal = initial_signal;
            while !thread_stop.load(Ordering::Relaxed) {
                let run_scan = if have_cheap {
                    let (changed, state) = cheap_signal(prev_signal);
                    prev_signal = state;
                    changed || last_full.elapsed() >= poll_dur
                } else {
                    last_full.elapsed() >= poll_dur
                };
                if run_scan {
                    scan_and_emit(&mut previous, &tx);
                    last_full = Instant::now();
                }
                thread::sleep(signal_dur);
            }
        });
        Self {
            stop,
            handle: Some(handle),
            rx,
        }
    }

    /// Poll the receiver once without blocking. Returns `None` when idle.
    pub fn try_recv(&self) -> Option<HotplugEvent> {
        self.rx.try_recv().ok()
    }

    /// Drain every event currently buffered in the channel.
    pub fn drain(&self) -> Vec<HotplugEvent> {
        self.rx.try_iter().collect()
    }
}

/// Run one full hardware detection, diff it against `previous` and emit the
/// changes. The very first scan only records the baseline (warm-up).
fn scan_and_emit(previous: &mut HashSet<HardwareFingerprint>, tx: &Sender<HotplugEvent>) {
    let profile = HardwareProfile::detect();
    let current: HashSet<HardwareFingerprint> =
        extract_fingerprints(&profile).into_iter().collect();
    if !previous.is_empty() {
        for event in diff_fingerprints(previous, &current) {
            if tx.send(event).is_err() {
                return;
            }
        }
    }
    *previous = current;
}

impl Drop for HotplugMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::BusType;

    fn fp(bus: BusType, vendor: u16, device: u16) -> HardwareFingerprint {
        HardwareFingerprint {
            bus,
            vendor_id: vendor,
            device_id: device,
            class_code: 0,
            serial_or_acpi: None,
        }
    }

    #[test]
    fn diff_reports_add_and_remove() {
        let mut previous = HashSet::new();
        previous.insert(fp(BusType::USB, 0x046d, 0x0825));
        previous.insert(fp(BusType::PCI, 0x8086, 0x1234));

        let mut current = HashSet::new();
        current.insert(fp(BusType::USB, 0x046d, 0x0825));
        current.insert(fp(BusType::NVMe, 0x15b7, 0x5006));

        let events = diff_fingerprints(&previous, &current);
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| matches!(
            e,
            HotplugEvent::Added(f) if f == &fp(BusType::NVMe, 0x15b7, 0x5006)
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HotplugEvent::Removed(f) if f == &fp(BusType::PCI, 0x8086, 0x1234)
        )));
        assert!(events.iter().any(HotplugEvent::is_added));
    }

    #[test]
    fn diff_reports_new_devices_on_first_scan() {
        let mut current = HashSet::new();
        current.insert(fp(BusType::USB, 0x046d, 0x0825));
        assert_eq!(diff_fingerprints(&HashSet::new(), &current).len(), 1);
    }

    #[test]
    fn monitor_skips_warmup_diff() {
        let mut set = HashSet::new();
        set.insert(fp(BusType::USB, 0x046d, 0x0825));
        let baseline = set.clone();
        let mut events = Vec::new();
        if !baseline.is_empty() {
            events.extend(diff_fingerprints(&baseline, &set));
        }
        assert!(
            events.is_empty(),
            "the first poll records the baseline and emits nothing"
        );
        set.insert(fp(BusType::NVMe, 0x15b7, 0x5006));
        events.extend(diff_fingerprints(&baseline, &set));
        assert_eq!(events.len(), 1, "later polls diff against the baseline");
    }

    #[test]
    fn unchanged_set_produces_no_events() {
        let mut set = HashSet::new();
        set.insert(fp(BusType::ACPI, 0x0, 0x0));
        assert!(diff_fingerprints(&set, &set).is_empty());
    }

    #[test]
    fn event_helpers() {
        let added = HotplugEvent::Added(fp(BusType::USB, 1, 2));
        let removed = HotplugEvent::Removed(fp(BusType::USB, 3, 4));
        assert!(added.is_added());
        assert!(!removed.is_added());
        assert_eq!(added.fingerprint().vendor_id, 1);
        assert_eq!(removed.fingerprint().device_id, 4);
    }

    #[test]
    fn dir_signal_hash_is_stable_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![dir.path()];
        let first = dir_signal_hash(&paths).expect("readable dir yields a signal");
        let second = dir_signal_hash(&paths).expect("stable across calls");
        assert_eq!(first, second, "mtime hash must be deterministic");
        assert_ne!(first, 0, "a real directory must not hash to zero");
    }

    #[test]
    fn dir_signal_hash_ignores_missing_paths() {
        let missing = Path::new("__aios_does_not_exist__");
        assert_eq!(dir_signal_hash(&[missing]), None);
        assert_eq!(dir_signal_hash(&[]), None);
    }

    #[test]
    fn dir_signal_hash_detects_tree_change() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ha = dir_signal_hash(&[a.path()]).unwrap();
        let hb = dir_signal_hash(&[b.path()]).unwrap();
        assert_ne!(ha, hb, "distinct directories must produce distinct hashes");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_has_no_cheap_signal() {
        let (changed, state) = cheap_signal(None);
        assert!(state.is_none(), "no cheap signal off Linux");
        assert!(changed, "no signal means 'run the full scan'");
    }

    #[test]
    fn config_defaults() {
        let cfg = HotplugConfig::default();
        assert_eq!(cfg.poll_ms, 1000);
        assert_eq!(cfg.signal_poll_ms, 250);
    }
}
