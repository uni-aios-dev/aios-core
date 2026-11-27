//! Authenticated audio-inventory endpoints backed by `aios-hal::audio`.

use crate::dto::AudioDevicesResponse;
use crate::server::BridgeContext;
use aios_hal::audio::{detect_audio_devices, AudioDevice};
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// Return the cached audio inventory, detecting it on first use.
fn snapshot(state: &Arc<BridgeContext>) -> Vec<AudioDevice> {
    let mut cache = state
        .audio_devices
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(devices) = cache.as_ref() {
        return devices.clone();
    }
    let detected = detect_audio_devices();
    log::info!("AUDIO: detected {} endpoints", detected.len());
    *cache = Some(detected.clone());
    detected
}

/// `GET /api/v1/audio/devices` — authenticated audio endpoint inventory.
pub async fn audio_devices_handler(
    State(state): State<Arc<BridgeContext>>,
) -> Json<AudioDevicesResponse> {
    Json(AudioDevicesResponse::from_devices(snapshot(&state)))
}

/// `POST /api/v1/audio/rescan` — re-detect endpoints after a hot-plug event.
pub async fn audio_rescan_handler(
    State(state): State<Arc<BridgeContext>>,
) -> Json<AudioDevicesResponse> {
    let detected = detect_audio_devices();
    let mut cache = state
        .audio_devices
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *cache = Some(detected.clone());
    drop(cache);
    Json(AudioDevicesResponse::from_devices(detected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;
    use aios_block_mgr::registry::BlockRegistry;
    use aios_hal::audio::{AudioBus, AudioDirection};
    use aios_process_mgr::scheduler::Scheduler;
    use aios_security::access_control::AccessControlLayer;
    use aios_watchdog::watchdog::{Watchdog, WatchdogConfig};
    use std::sync::Mutex;

    fn context() -> Arc<BridgeContext> {
        let ctx = BridgeContext::new(
            Arc::new(Mutex::new(Scheduler::new(4096))),
            BlockRegistry::new(),
            AccessControlLayer::new(b"test-secret".to_vec(), 60_000),
            Watchdog::new(WatchdogConfig::default()),
            99,
        )
        .with_auth(Arc::new(Mutex::new(AuthStore::empty())));
        Arc::new(ctx)
    }

    #[test]
    fn cached_inventory_is_returned_without_hardware_access() {
        let state = context();
        let device = AudioDevice {
            name: "Microphone (Mock)".into(),
            direction: AudioDirection::Input,
            bus: AudioBus::Usb,
            driver: "mock-driver".into(),
            vendor_id: 0,
            device_id: 0,
            id_source: aios_hal::audio::AudioIdSource::Unknown,
            class_code: 0x0004_0000,
            is_default: true,
            sample_rate: 16_000,
            channels: 1,
            sample_format: "f32".into(),
        };
        *state.audio_devices.lock().unwrap() = Some(vec![device]);
        let devices = snapshot(&state);
        assert_eq!(devices.len(), 1);
        assert!(devices[0].is_microphone());
    }
}
