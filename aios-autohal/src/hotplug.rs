//! Live device hot-plug monitoring.
//!
//! A background [`HotplugMonitor`] thread periodically re-detects the attached
//! hardware snapshot and diffs the fingerprint set against the previous poll.
//! Changed devices are emitted as [`HotplugEvent`]s (`Added`/`Removed`) over an
//! `mpsc` channel; the owner (kernel TUI or GUI main loop) drains the events on
//! its tick and applies them to the [`crate::engine::AutohalEngine`], which is
//! deliberately kept on the UI thread because it owns non-`Send` `WasmBlock`
//! instances.

use crate::fingerprint::{extract_fingerprints, HardwareFingerprint};
use aios_hal::hardware::HardwareProfile;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
    /// Delay between hardware re-detection polls in milliseconds.
    pub poll_ms: u64,
}

impl Default for HotplugConfig {
    fn default() -> Self {
        Self { poll_ms: 1000 }
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

/// Background thread that turns periodic hardware re-detection into a stream
/// of [`HotplugEvent`]s. Dropping the monitor stops the thread.
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
            while !thread_stop.load(Ordering::Relaxed) {
                let profile = HardwareProfile::detect();
                let current: HashSet<HardwareFingerprint> =
                    extract_fingerprints(&profile).into_iter().collect();
                if !previous.is_empty() {
                    for event in diff_fingerprints(&previous, &current) {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                }
                previous = current;
                thread::sleep(Duration::from_millis(config.poll_ms));
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
}
