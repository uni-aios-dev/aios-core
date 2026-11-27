//! Audio endpoint and audio-driver inventory shared by HAL, AutoHAL and the bridge.
//!
//! `cpal` supplies the live input/output endpoints (name, native sample format,
//! default flag); the OS supplies driver metadata (kernel driver name, vendor
//! and device ids). Both are merged into [`AudioDevice`] records so the rest of
//! AIOS can answer "which microphone is present and which driver serves it?"
//! without touching platform APIs.

use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};

/// Direction of an audio endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioDirection {
    /// Capture endpoint (microphone, line-in).
    Input,
    /// Playback endpoint (speakers, headphones, HDMI audio).
    Output,
}

impl AudioDirection {
    /// Stable human-readable label used by the console and bridge DTOs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

impl std::fmt::Display for AudioDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Physical bus an audio endpoint is attached to when the OS reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioBus {
    /// The OS did not report a bus for this endpoint.
    Unknown,
    /// USB audio class device.
    Usb,
    /// PCI/PCIe audio controller.
    Pci,
    /// Bluetooth audio endpoint.
    Bluetooth,
}

impl AudioBus {
    /// Stable lowercase tag used in fingerprints and driver ids.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Unknown => "audio",
            Self::Usb => "usb",
            Self::Pci => "pci",
            Self::Bluetooth => "bt",
        }
    }
}

/// One live audio endpoint with its driver metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// Endpoint name as reported by `cpal` or the OS driver record.
    pub name: String,
    /// Capture or playback endpoint.
    pub direction: AudioDirection,
    /// Physical bus, when known.
    pub bus: AudioBus,
    /// Kernel driver name/version; empty when the OS does not expose one.
    pub driver: String,
    /// PCI/USB vendor id, zero when unknown.
    pub vendor_id: u16,
    /// PCI/USB device id, zero when unknown.
    pub device_id: u16,
    /// PCI class word (`class << 16 | subclass << 8`), zero when unknown.
    pub class_code: u32,
    /// Whether this is the system default endpoint for its direction.
    pub is_default: bool,
    /// Native sample rate in Hz, zero when the endpoint has no default config.
    pub sample_rate: u32,
    /// Native channel count, zero when the endpoint has no default config.
    pub channels: u16,
    /// Native sample format, empty when the endpoint has no default config.
    pub sample_format: String,
}

impl AudioDevice {
    /// Whether this endpoint can capture voice input.
    pub fn is_microphone(&self) -> bool {
        self.direction == AudioDirection::Input
    }

    /// PCI multimedia class, used by AutoHAL to find a generic audio driver.
    pub fn audio_class_code(&self) -> u32 {
        if self.class_code == 0 {
            0x0004_0000
        } else {
            self.class_code
        }
    }
}

/// Whether at least one capture endpoint is present.
pub fn has_microphone(devices: &[AudioDevice]) -> bool {
    devices.iter().any(AudioDevice::is_microphone)
}

/// Enumerate live audio endpoints and enrich them with OS driver metadata.
///
/// Never panics: a missing audio stack yields an empty list, which is a valid
/// hardware state on headless machines and in CI.
pub fn detect_audio_devices() -> Vec<AudioDevice> {
    let records = collect_driver_records();
    annotate_devices(collect_cpal_devices(), &records)
}

/// Infer a likely bus from an endpoint name when the OS record is unavailable.
pub fn bus_from_name(name: &str) -> AudioBus {
    let lower = name.to_ascii_lowercase();
    if lower.contains("bluetooth") || lower.contains("bt audio") {
        AudioBus::Bluetooth
    } else if lower.contains("usb") {
        AudioBus::Usb
    } else if lower.contains("pci")
        || lower.contains("hdmi")
        || lower.contains("display audio")
        || lower.contains("realtek")
    {
        AudioBus::Pci
    } else {
        AudioBus::Unknown
    }
}

/// Attach matching OS driver records to `cpal` endpoints.
///
/// Pure function kept separate from platform probing so the merge rules are
/// testable without audio hardware.
pub fn annotate_devices(
    mut devices: Vec<AudioDevice>,
    records: &[DriverRecord],
) -> Vec<AudioDevice> {
    for device in &mut devices {
        let lower = device.name.to_ascii_lowercase();
        let Some(record) = records.iter().find(|record| {
            let record_name = record.name.to_ascii_lowercase();
            (!record_name.is_empty() && lower.contains(&record_name))
                || (!record.driver.is_empty()
                    && lower.contains(&record.driver.to_ascii_lowercase()))
        }) else {
            if device.bus == AudioBus::Unknown {
                device.bus = bus_from_name(&device.name);
            }
            continue;
        };
        device.driver = record.driver.clone();
        if device.vendor_id == 0 {
            device.vendor_id = record.vendor_id;
        }
        if device.device_id == 0 {
            device.device_id = record.device_id;
        }
        if device.class_code == 0 {
            device.class_code = record.class_code;
        }
        if device.bus == AudioBus::Unknown {
            device.bus = record.bus;
        }
    }
    devices
}

/// Driver metadata collected from the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverRecord {
    /// OS name of the controller/endpoint.
    pub name: String,
    /// Kernel driver name or driver version string.
    pub driver: String,
    /// Vendor id, zero when unknown.
    pub vendor_id: u16,
    /// Device id, zero when unknown.
    pub device_id: u16,
    /// Physical bus.
    pub bus: AudioBus,
    /// PCI class word, zero for non-PCI devices.
    pub class_code: u32,
}

fn cpal_sample_format(format: cpal::SampleFormat) -> &'static str {
    match format {
        cpal::SampleFormat::I8 => "i8",
        cpal::SampleFormat::I16 => "i16",
        cpal::SampleFormat::I32 => "i32",
        cpal::SampleFormat::I64 => "i64",
        cpal::SampleFormat::U8 => "u8",
        cpal::SampleFormat::U16 => "u16",
        cpal::SampleFormat::U32 => "u32",
        cpal::SampleFormat::U64 => "u64",
        cpal::SampleFormat::F32 => "f32",
        cpal::SampleFormat::F64 => "f64",
        _ => "unknown",
    }
}

fn collect_cpal_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default_input = host.default_input_device().and_then(|d| d.name().ok());
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    let mut devices = Vec::new();

    for direction in [AudioDirection::Input, AudioDirection::Output] {
        let iterator = match direction {
            AudioDirection::Input => host.input_devices(),
            AudioDirection::Output => host.output_devices(),
        };
        let Ok(iterator) = iterator else {
            continue;
        };
        for device in iterator {
            let Ok(name) = device.name() else {
                continue;
            };
            let config = match direction {
                AudioDirection::Input => device.default_input_config().ok(),
                AudioDirection::Output => device.default_output_config().ok(),
            };
            let (sample_rate, channels, sample_format) = config
                .map(|config| {
                    (
                        config.sample_rate().0,
                        config.channels(),
                        cpal_sample_format(config.sample_format()).to_string(),
                    )
                })
                .unwrap_or_default();
            let is_default = match direction {
                AudioDirection::Input => default_input.as_deref() == Some(name.as_str()),
                AudioDirection::Output => default_output.as_deref() == Some(name.as_str()),
            };
            let bus = bus_from_name(&name);
            devices.push(AudioDevice {
                name,
                direction,
                bus,
                driver: String::new(),
                vendor_id: 0,
                device_id: 0,
                class_code: 0,
                is_default,
                sample_rate,
                channels,
                sample_format,
            });
        }
    }
    devices
}

fn parse_hex(value: &str) -> Option<u32> {
    u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(target_os = "linux")]
fn parse_hex_u16(value: &str) -> u16 {
    parse_hex(value).unwrap_or(0) as u16
}

#[cfg(target_os = "linux")]
fn is_audio_class(class: u32) -> bool {
    class >> 16 == 0x04
}

#[cfg(target_os = "linux")]
fn driver_name(path: &std::path::Path) -> String {
    std::fs::read_link(path.join("driver"))
        .ok()
        .and_then(|link| {
            link.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn collect_driver_records() -> Vec<DriverRecord> {
    let mut records = Vec::new();

    if let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(class_text) = std::fs::read_to_string(path.join("class")) else {
                continue;
            };
            let Some(class) = parse_hex(&class_text) else {
                continue;
            };
            if !is_audio_class(class) {
                continue;
            }
            let vendor_id = std::fs::read_to_string(path.join("vendor"))
                .map(|text| parse_hex_u16(&text))
                .unwrap_or(0);
            let device_id = std::fs::read_to_string(path.join("device"))
                .map(|text| parse_hex_u16(&text))
                .unwrap_or(0);
            records.push(DriverRecord {
                name: format!("PCI Audio {vendor_id:04x}:{device_id:04x}"),
                driver: driver_name(&path),
                vendor_id,
                device_id,
                bus: AudioBus::Pci,
                class_code: class,
            });
        }
    }

    if let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(class_text) = std::fs::read_to_string(path.join("bDeviceClass")) else {
                continue;
            };
            if parse_hex(&class_text) != Some(0x03) {
                continue;
            }
            let vendor_id = std::fs::read_to_string(path.join("idVendor"))
                .map(|text| parse_hex_u16(&text))
                .unwrap_or(0);
            let device_id = std::fs::read_to_string(path.join("idProduct"))
                .map(|text| parse_hex_u16(&text))
                .unwrap_or(0);
            let name = std::fs::read_to_string(path.join("product"))
                .unwrap_or_else(|_| format!("USB Audio {vendor_id:04x}:{device_id:04x}"))
                .trim()
                .to_string();
            records.push(DriverRecord {
                name,
                driver: driver_name(&path),
                vendor_id,
                device_id,
                bus: AudioBus::Usb,
                class_code: 0x0004_0000,
            });
        }
    }

    records
}

/// Parse `wmic ... /format:list` blocks into driver records.
pub fn parse_wmic_driver_records(output: &str) -> Vec<DriverRecord> {
    let mut records = Vec::new();
    let mut fields: Vec<(String, String)> = Vec::new();

    let flush = |fields: &mut Vec<(String, String)>, records: &mut Vec<DriverRecord>| {
        if fields.is_empty() {
            return;
        }
        let get = |key: &str| -> String {
            fields
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        let device_id = get("DeviceID");
        let vendor_id = parse_hex(
            device_id
                .split("VEN_")
                .nth(1)
                .map(|part| part.split('&').next().unwrap_or_default())
                .unwrap_or_default(),
        )
        .unwrap_or(0) as u16;
        let device = parse_hex(
            device_id
                .split("DEV_")
                .nth(1)
                .map(|part| part.split('&').next().unwrap_or_default())
                .unwrap_or_default(),
        )
        .unwrap_or(0) as u16;
        let name = get("Name");
        let manufacturer = get("Manufacturer");
        let version = get("DriverVersion");
        let driver = match (manufacturer.is_empty(), version.is_empty()) {
            (false, false) => format!("{manufacturer} {version}"),
            (false, true) => manufacturer,
            (true, false) => version,
            (true, true) => String::new(),
        };
        records.push(DriverRecord {
            name,
            driver,
            vendor_id,
            device_id: device,
            bus: if device_id.starts_with("USB\\") {
                AudioBus::Usb
            } else if device_id.starts_with("PCI\\") {
                AudioBus::Pci
            } else {
                AudioBus::Unknown
            },
            class_code: if device_id.starts_with("PCI\\") {
                0x0004_0000
            } else {
                0
            },
        });
        fields.clear();
    };

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut fields, &mut records);
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            fields.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    flush(&mut fields, &mut records);
    records
}

#[cfg(target_os = "windows")]
fn collect_driver_records() -> Vec<DriverRecord> {
    let output = std::process::Command::new("wmic")
        .args([
            "sounddev",
            "get",
            "Name,Manufacturer,DriverVersion,DeviceID",
            "/format:list",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    parse_wmic_driver_records(&output)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn collect_driver_records() -> Vec<DriverRecord> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, direction: AudioDirection) -> AudioDevice {
        AudioDevice {
            name: name.into(),
            direction,
            bus: AudioBus::Unknown,
            driver: String::new(),
            vendor_id: 0,
            device_id: 0,
            class_code: 0,
            is_default: false,
            sample_rate: 48_000,
            channels: 2,
            sample_format: "i16".into(),
        }
    }

    #[test]
    fn bus_is_inferred_from_endpoint_name() {
        assert_eq!(bus_from_name("Microphone (USB Audio)"), AudioBus::Usb);
        assert_eq!(
            bus_from_name("Headset (Bluetooth Audio)"),
            AudioBus::Bluetooth
        );
        assert_eq!(bus_from_name("HDMI Output"), AudioBus::Pci);
        assert_eq!(bus_from_name("Microphone"), AudioBus::Unknown);
    }

    #[test]
    fn driver_metadata_enriches_matching_endpoint() {
        let record = DriverRecord {
            name: "Realtek Audio".into(),
            driver: "snd_hda_intel".into(),
            vendor_id: 0x10EC,
            device_id: 0x1234,
            bus: AudioBus::Pci,
            class_code: 0x0004_0300,
        };
        let devices = annotate_devices(
            vec![device("Microphone (Realtek Audio)", AudioDirection::Input)],
            std::slice::from_ref(&record),
        );
        assert_eq!(devices[0].driver, "snd_hda_intel");
        assert_eq!(devices[0].vendor_id, 0x10EC);
        assert_eq!(devices[0].device_id, 0x1234);
        assert_eq!(devices[0].bus, AudioBus::Pci);
        assert_eq!(devices[0].class_code, 0x0004_0300);
        assert!(has_microphone(&devices));
        assert_eq!(devices[0].audio_class_code(), 0x0004_0300);
    }

    #[test]
    fn unmatched_endpoint_keeps_its_own_values() {
        let devices = annotate_devices(vec![device("USB Mic", AudioDirection::Input)], &[]);
        assert_eq!(devices[0].bus, AudioBus::Usb);
        assert_eq!(devices[0].driver, String::new());
    }

    #[test]
    fn wmic_list_output_is_parsed() {
        let output = concat!(
            "Manufacturer=Intel(R) Corporation\n",
            "Name=Intel(R) Display Audio\n",
            "DriverVersion=10.0.1.2\n",
            "DeviceID=PCI\\VEN_8086&DEV_280B&SUBSYS_00000000\n",
            "\n"
        );
        let records = parse_wmic_driver_records(output);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].vendor_id, 0x8086);
        assert_eq!(records[0].device_id, 0x280B);
        assert_eq!(records[0].bus, AudioBus::Pci);
        assert_eq!(records[0].driver, "Intel(R) Corporation 10.0.1.2");
    }

    #[test]
    fn audio_profile_fields_survive_serde() {
        let record = device("Microphone (USB Audio)", AudioDirection::Input);
        let json = serde_json::to_string(&record).expect("serialize audio device");
        let restored: AudioDevice = serde_json::from_str(&json).expect("deserialize audio device");
        assert_eq!(restored, record);
    }
}
