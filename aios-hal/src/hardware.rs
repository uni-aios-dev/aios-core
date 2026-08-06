use aios_core::block::{BlockId, BlockState, StatefulBlock};
use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    pub cores: u32,
    pub threads: u32,
    pub model: String,
    pub has_avx512: bool,
    pub has_avx2: bool,
    pub has_sse42: bool,
    pub has_neon: bool,
    pub base_freq_mhz: u32,
    pub vendor: CpuVendor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuVendor {
    Intel,
    AMD,
    ARM,
    Apple,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vram_mb: u64,
    pub compute_shaders: bool,
    pub vendor: String,
    pub driver_version: String,
    pub cuda_cores: u32,
    pub compute_capability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpuInfo {
    pub name: String,
    pub tops: u64,
    pub supported_frameworks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDevice {
    pub name: String,
    pub interface: StorageInterface,
    pub capacity_gb: u64,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageInterface {
    NVMe,
    SATA,
    USB,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsbSpeed {
    Usb11,
    Usb20,
    Usb30,
    Usb31,
    Usb32,
    Usb40,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThunderboltSpeed {
    Tb1,
    Tb2,
    Tb3,
    Tb4,
    Tb5,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbDevice {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub speed: UsbSpeed,
    pub is_hub: bool,
    pub port: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThunderboltDevice {
    pub name: String,
    pub vendor_id: u16,
    pub device_id: u16,
    pub speed: ThunderboltSpeed,
    pub max_power_watts: u32,
    pub port: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PciDevice {
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInfo {
    pub total_mb: u64,
    pub available_mb: u64,
    pub speed_mhz: u32,
    pub dimm_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub cpu: CpuInfo,
    pub gpu: Option<GpuInfo>,
    pub npu: Option<NpuInfo>,
    pub memory: MemoryInfo,
    pub pci_devices: Vec<PciDevice>,
    pub storage_devices: Vec<StorageDevice>,
    pub usb_devices: Vec<UsbDevice>,
    pub thunderbolt_devices: Vec<ThunderboltDevice>,
}

impl HardwareProfile {
    pub fn detect() -> Self {
        let cpu = Self::detect_cpu();
        let memory = Self::detect_memory();
        let gpu = Self::detect_gpu();
        let npu = Self::detect_npu();
        let pci_devices = Self::scan_pci();
        let storage_devices = Self::detect_storage();
        let usb_devices = Self::detect_usb();
        let thunderbolt_devices = Self::detect_thunderbolt();

        log::info!(
            "HAL: Detected {} cores, {}MB RAM, GPU={}, NPU={}, PCI={}, Storage={}, USB={}, TB={}",
            cpu.cores,
            memory.total_mb,
            gpu.is_some(),
            npu.is_some(),
            pci_devices.len(),
            storage_devices.len(),
            usb_devices.len(),
            thunderbolt_devices.len(),
        );

        Self {
            cpu,
            gpu,
            npu,
            memory,
            pci_devices,
            storage_devices,
            usb_devices,
            thunderbolt_devices,
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn detect_cpu() -> CpuInfo {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        let has_avx2 = std::arch::is_x86_feature_detected!("avx2");
        let has_avx512 = std::arch::is_x86_feature_detected!("avx512f");
        let has_sse42 = std::arch::is_x86_feature_detected!("sse4.2");
        let vendor = detect_cpu_vendor_x86();

        CpuInfo {
            cores,
            threads: cores,
            model: detect_cpu_model(),
            has_avx512,
            has_avx2,
            has_sse42,
            has_neon: false,
            base_freq_mhz: 0,
            vendor,
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn detect_cpu() -> CpuInfo {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        CpuInfo {
            cores,
            threads: cores,
            model: "Unknown CPU".into(),
            has_avx512: false,
            has_avx2: false,
            has_sse42: false,
            has_neon: cfg!(target_arch = "aarch64"),
            base_freq_mhz: 0,
            vendor: CpuVendor::Unknown,
        }
    }

    fn detect_memory() -> MemoryInfo {
        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "memorychip",
                    "get",
                    "Capacity,Speed,DimmLocator",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                return Self::parse_wmic_memory_csv(&stdout);
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if let Some(val) = line.strip_prefix("MemTotal:") {
                        let kb: u64 = val
                            .trim()
                            .split_whitespace()
                            .next()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        return MemoryInfo {
                            total_mb: kb / 1024,
                            available_mb: kb / 1024,
                            speed_mhz: 0,
                            dimm_count: 1,
                        };
                    }
                }
            }
        }

        MemoryInfo {
            total_mb: 0,
            available_mb: 0,
            speed_mhz: 0,
            dimm_count: 0,
        }
    }

    /// Parses `wmic memorychip ... /format:csv` output into (total_bytes, speed_mhz, dimm_count).
    ///
    /// Rows have a leading Node column; short or malformed lines are skipped instead of panicking.
    fn parse_wmic_memory_csv(stdout: &str) -> MemoryInfo {
        let mut total_bytes: u64 = 0;
        let mut dimm_count = 0u32;
        let mut speed = 0u32;
        for line in stdout.lines().skip(1) {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 3 {
                if let Ok(cap) = parts[1].trim().parse::<u64>() {
                    total_bytes += cap;
                    dimm_count += 1;
                }
                if let Ok(s) = parts[2].trim().parse::<u32>() {
                    speed = s;
                }
            }
        }
        MemoryInfo {
            total_mb: total_bytes / (1024 * 1024),
            available_mb: total_bytes / (1024 * 1024),
            speed_mhz: speed,
            dimm_count,
        }
    }

    fn detect_gpu() -> Option<GpuInfo> {
        if let Some(gpu) = Self::detect_gpu_nvidia() {
            return Some(gpu);
        }
        if let Some(gpu) = Self::detect_gpu_amd() {
            return Some(gpu);
        }
        if let Some(gpu) = Self::detect_gpu_wmic() {
            return Some(gpu);
        }
        None
    }

    fn detect_gpu_nvidia() -> Option<GpuInfo> {
        let output = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,memory.total,driver_version,compute_cap",
                "--format=csv,noheader,nounits",
            ])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next()?;
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 4 {
            return None;
        }

        let name = parts[0].to_string();
        let vram_mb: u64 = parts[1].parse().unwrap_or(0);
        let driver_version = parts[2].to_string();
        let compute_capability = parts[3].to_string();

        let cuda_cores = Self::estimate_cuda_cores(&name);

        log::info!(
            "HAL: NVIDIA GPU detected — {} ({}MB, CC={}, driver={})",
            name,
            vram_mb,
            compute_capability,
            driver_version
        );

        Some(GpuInfo {
            name,
            vram_mb,
            compute_shaders: true,
            vendor: "NVIDIA".into(),
            driver_version,
            cuda_cores,
            compute_capability,
        })
    }

    pub fn estimate_cuda_cores(gpu_name: &str) -> u32 {
        let lower = gpu_name.to_lowercase();
        if lower.contains("4090") {
            16384
        } else if lower.contains("4080") {
            9728
        } else if lower.contains("4070 ti") {
            7680
        } else if lower.contains("4070") {
            5888
        } else if lower.contains("3090") {
            10496
        } else if lower.contains("3080") {
            8704
        } else if lower.contains("3070") {
            5888
        } else if lower.contains("a100") {
            6912
        } else if lower.contains("h100") {
            16896
        } else if lower.contains("l40") {
            18176
        } else {
            0
        }
    }

    fn detect_gpu_amd() -> Option<GpuInfo> {
        let output = std::process::Command::new("rocm-smi")
            .args(["--showproductname", "--showmeminfo", "vram", "--csv"])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut name = String::new();
        let mut vram_mb: u64 = 0;

        for line in stdout.lines().skip(1) {
            let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if parts.len() >= 2 {
                if name.is_empty() {
                    name = parts[0].to_string();
                }
                if let Ok(v) = parts[1].parse::<u64>() {
                    vram_mb = v / (1024 * 1024);
                }
            }
        }

        if name.is_empty() || vram_mb == 0 {
            return None;
        }

        log::info!("HAL: AMD GPU detected — {} ({}MB)", name, vram_mb);

        Some(GpuInfo {
            name,
            vram_mb,
            compute_shaders: true,
            vendor: "AMD".into(),
            driver_version: String::new(),
            cuda_cores: 0,
            compute_capability: String::new(),
        })
    }

    fn detect_gpu_wmic() -> Option<GpuInfo> {
        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "path",
                    "win32_videocontroller",
                    "get",
                    "Name,AdapterRAM",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines().skip(1) {
                    let parts: Vec<&str> = line.split(',').collect();
                    if parts.len() >= 2 {
                        let name = parts[1].trim().to_string();
                        let vram: u64 = parts[2].trim().parse().unwrap_or(0);
                        if !name.is_empty() {
                            return Some(GpuInfo {
                                name,
                                vram_mb: vram / (1024 * 1024),
                                compute_shaders: true,
                                vendor: "Unknown".into(),
                                driver_version: String::new(),
                                cuda_cores: 0,
                                compute_capability: String::new(),
                            });
                        }
                    }
                }
            }
        }
        None
    }

    fn detect_npu() -> Option<NpuInfo> {
        if let Some(npu) = Self::detect_npu_intel() {
            return Some(npu);
        }
        if let Some(npu) = Self::detect_npu_qualcomm() {
            return Some(npu);
        }
        if let Some(npu) = Self::detect_npu_windows_pnp() {
            return Some(npu);
        }
        None
    }

    fn detect_npu_intel() -> Option<NpuInfo> {
        let cpu_model = detect_cpu_model();
        let lower = cpu_model.to_lowercase();
        let is_meteor_or_lunar = lower.contains("meteor lake")
            || lower.contains("lunar lake")
            || lower.contains("core ultra")
            || lower.contains("arrow lake");

        #[cfg(target_os = "linux")]
        {
            if is_meteor_or_lunar {
                if let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") {
                    for entry in entries.flatten() {
                        let dev_path = entry.path();
                        let vendor_path = dev_path.join("vendor");
                        let device_path = dev_path.join("device");
                        let class_path = dev_path.join("class");
                        if let (Ok(vendor), Ok(device)) = (
                            std::fs::read_to_string(&vendor_path),
                            std::fs::read_to_string(&device_path),
                        ) {
                            let vendor_id = vendor.trim().trim_start_matches("0x");
                            let device_id = device.trim().trim_start_matches("0x");
                            if vendor_id == "8086" && device_id == "7d0b" {
                                let class = std::fs::read_to_string(&class_path)
                                    .ok()
                                    .map(|s| s.trim().to_string())
                                    .unwrap_or_default();
                                log::info!(
                                    "HAL: Intel NPU detected via PCI 8086:7d0B (class={})",
                                    class
                                );
                                return Some(NpuInfo {
                                    name: "Intel AI Boost (Meteor Lake NPU)".into(),
                                    tops: 11,
                                    supported_frameworks: vec![
                                        "ONNX".into(),
                                        "OpenVINO".into(),
                                        "DirectML".into(),
                                    ],
                                });
                            }
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if is_meteor_or_lunar {
                if let Ok(output) = std::process::Command::new("wmic")
                    .args([
                        "path",
                        "win32_pnpentity",
                        "get",
                        "Name,DeviceID",
                        "/format:csv",
                    ])
                    .output()
                {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        let lower_line = line.to_lowercase();
                        if (lower_line.contains("intel")
                            && (lower_line.contains("npu") || lower_line.contains("neural")))
                            || line.contains("PCI\\VEN_8086&DEV_7D0B")
                        {
                            return Some(NpuInfo {
                                name: "Intel AI Boost (Meteor Lake NPU)".into(),
                                tops: 11,
                                supported_frameworks: vec![
                                    "ONNX".into(),
                                    "OpenVINO".into(),
                                    "DirectML".into(),
                                ],
                            });
                        }
                    }
                }
            }
        }

        None
    }

    fn detect_npu_qualcomm() -> Option<NpuInfo> {
        let cpu_model = detect_cpu_model();
        let lower = cpu_model.to_lowercase();
        let is_snapdragon_x = lower.contains("snapdragon") && lower.contains("x ");

        #[cfg(target_os = "linux")]
        {
            if is_snapdragon_x {
                if let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") {
                    for entry in entries.flatten() {
                        let dev_path = entry.path();
                        let vendor_path = dev_path.join("vendor");
                        let device_path = dev_path.join("device");
                        if let (Ok(vendor), Ok(device)) = (
                            std::fs::read_to_string(&vendor_path),
                            std::fs::read_to_string(&device_path),
                        ) {
                            let vendor_id = vendor.trim().trim_start_matches("0x");
                            let device_id = device.trim().trim_start_matches("0x");
                            if vendor_id == "17cb" && device_id == "1100" {
                                log::info!("HAL: Qualcomm Hexagon NPU detected via PCI 17CB:1100");
                                return Some(NpuInfo {
                                    name: "Qualcomm Hexagon NPU".into(),
                                    tops: 45,
                                    supported_frameworks: vec![
                                        "ONNX".into(),
                                        "QNN".into(),
                                        "DirectML".into(),
                                        "TensorFlow Lite".into(),
                                    ],
                                });
                            }
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if is_snapdragon_x {
                if let Ok(output) = std::process::Command::new("wmic")
                    .args([
                        "path",
                        "win32_pnpentity",
                        "get",
                        "Name,DeviceID",
                        "/format:csv",
                    ])
                    .output()
                {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        let lower_line = line.to_lowercase();
                        if (lower_line.contains("qualcomm")
                            && (lower_line.contains("npu")
                                || lower_line.contains("hexagon")
                                || lower_line.contains("neural")))
                            || (line.contains("VEN_17CB") && line.contains("DEV_1100"))
                        {
                            return Some(NpuInfo {
                                name: "Qualcomm Hexagon NPU".into(),
                                tops: 45,
                                supported_frameworks: vec![
                                    "ONNX".into(),
                                    "QNN".into(),
                                    "DirectML".into(),
                                    "TensorFlow Lite".into(),
                                ],
                            });
                        }
                    }
                }
            }
        }

        None
    }

    fn detect_npu_windows_pnp() -> Option<NpuInfo> {
        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "path",
                    "win32_pnpentity",
                    "get",
                    "Name,DeviceID",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("neural")
                        || lower.contains("npu")
                        || lower.contains("ai engine")
                    {
                        return Some(NpuInfo {
                            name: line.trim().to_string(),
                            tops: 10,
                            supported_frameworks: vec!["ONNX".into(), "DirectML".into()],
                        });
                    }
                }
            }
        }
        None
    }

    fn detect_storage() -> Vec<StorageDevice> {
        let mut devices = Vec::new();

        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "diskdrive",
                    "get",
                    "Model,InterfaceType,Size",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines().skip(1) {
                    let parts: Vec<&str> = line.split(',').collect();
                    if parts.len() >= 4 {
                        let model = parts[1].trim().to_string();
                        let interface_str = parts[2].trim().to_uppercase();
                        let size_str = parts[3].trim();

                        if model.is_empty() || model == "Model" {
                            continue;
                        }

                        let interface = match interface_str.as_str() {
                            "NVME" => StorageInterface::NVMe,
                            "SCSI" | "IDE" | "SERIAL ATA" | "SATA" => StorageInterface::SATA,
                            "USB" => StorageInterface::USB,
                            _ => StorageInterface::Unknown,
                        };

                        let capacity_gb = size_str
                            .parse::<u64>()
                            .map(|b| b / (1024 * 1024 * 1024))
                            .unwrap_or(0);

                        devices.push(StorageDevice {
                            name: model.clone(),
                            interface,
                            capacity_gb,
                            model,
                        });
                    }
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(entries) = std::fs::read_dir("/sys/block") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("nvme") || name.starts_with("sd") {
                        let interface = if name.starts_with("nvme") {
                            StorageInterface::NVMe
                        } else {
                            StorageInterface::SATA
                        };

                        let size_path = entry.path().join("size");
                        let capacity_gb = std::fs::read_to_string(&size_path)
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .map(|sectors| (sectors * 512) / (1024 * 1024 * 1024))
                            .unwrap_or(0);

                        let model_path = entry.path().join("device/model");
                        let model = std::fs::read_to_string(&model_path)
                            .ok()
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|| name.clone());

                        devices.push(StorageDevice {
                            name: name.clone(),
                            interface,
                            capacity_gb,
                            model,
                        });
                    }
                }
            }
        }

        devices
    }

    fn detect_usb() -> Vec<UsbDevice> {
        let mut devices = Vec::new();

        #[cfg(target_os = "linux")]
        {
            if let Ok(output) = std::process::Command::new("lsusb").output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Some(rest) = line.strip_prefix("Bus ") {
                        let parts: Vec<&str> = rest.split_whitespace().collect();
                        if parts.len() < 4 {
                            continue;
                        }
                        let port = format!("Bus {}", parts[0].trim_end_matches(':'));
                        let id_str = parts[1];
                        let (vid, pid) = if let Some(colon_pos) = id_str.find(':') {
                            let vid = u16::from_str_radix(&id_str[..colon_pos], 16).unwrap_or(0);
                            let pid =
                                u16::from_str_radix(&id_str[colon_pos + 1..], 16).unwrap_or(0);
                            (vid, pid)
                        } else {
                            (0, 0)
                        };
                        let name = parts[3..].join(" ");
                        let is_hub = name.to_lowercase().contains("hub");
                        let speed = Self::classify_usb_speed(line);
                        devices.push(UsbDevice {
                            name,
                            vendor_id: vid,
                            product_id: pid,
                            speed,
                            is_hub,
                            port,
                        });
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "path",
                    "win32_pnpentity",
                    "get",
                    "Name,DeviceID",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if line.contains("USB\\") {
                        let parts: Vec<&str> = line.split(',').collect();
                        if parts.len() >= 2 {
                            let name = parts[1].trim().to_string();
                            let dev_id = parts.get(2).unwrap_or(&"").trim();
                            let vid = Self::extract_pnp_vendor_id(dev_id);
                            let pid = Self::extract_pnp_product_id(dev_id);
                            let speed = UsbSpeed::Unknown;
                            let is_hub = name.to_lowercase().contains("hub");
                            let port = Self::extract_pnp_parent(dev_id);
                            devices.push(UsbDevice {
                                name,
                                vendor_id: vid,
                                product_id: pid,
                                speed,
                                is_hub,
                                port,
                            });
                        }
                    }
                }
            }
        }

        devices
    }

    fn detect_thunderbolt() -> Vec<ThunderboltDevice> {
        let mut devices = Vec::new();

        #[cfg(target_os = "linux")]
        {
            let tb_path = "/sys/bus/thunderbolt/devices";
            if let Ok(entries) = std::fs::read_dir(tb_path) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !name.contains('-') {
                        continue;
                    }
                    let dev_path = entry.path();
                    let vendor_path = dev_path.join("vendor_name");
                    let device_path = dev_path.join("device_name");
                    let vendor = std::fs::read_to_string(&vendor_path)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| "Unknown".into());
                    let device_name = std::fs::read_to_string(&device_path)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| name.clone());
                    let speed = Self::classify_tb_speed_from_device(&dev_path);
                    let max_power = Self::read_tb_max_power(&dev_path);
                    let parts: Vec<&str> = name.split('-').collect();
                    let vid = parts
                        .first()
                        .and_then(|s| u16::from_str_radix(s, 16).ok())
                        .unwrap_or(0);
                    let did = parts
                        .get(1)
                        .and_then(|s| u16::from_str_radix(s, 16).ok())
                        .unwrap_or(0);

                    log::info!(
                        "HAL: Thunderbolt device detected — {} {} (speed={:?}, {}W)",
                        vendor,
                        device_name,
                        speed,
                        max_power
                    );

                    devices.push(ThunderboltDevice {
                        name: format!("{} {}", vendor, device_name),
                        vendor_id: vid,
                        device_id: did,
                        speed,
                        max_power_watts: max_power,
                        port: name,
                    });
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "path",
                    "win32_pnpentity",
                    "get",
                    "Name,DeviceID",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("thunderbolt") || line.contains("TBT\\") {
                        let parts: Vec<&str> = line.split(',').collect();
                        if parts.len() >= 2 {
                            let name = parts[1].trim().to_string();
                            let dev_id = parts.get(2).unwrap_or(&"").trim();
                            let vid = Self::extract_pnp_vendor_id(dev_id);
                            let did = Self::extract_pnp_product_id(dev_id);
                            let speed = if lower.contains("usb4") || lower.contains("tb5") {
                                ThunderboltSpeed::Tb5
                            } else if lower.contains("tb4") || lower.contains("40gbps") {
                                ThunderboltSpeed::Tb4
                            } else {
                                ThunderboltSpeed::Tb3
                            };

                            devices.push(ThunderboltDevice {
                                name,
                                vendor_id: vid,
                                device_id: did,
                                speed,
                                max_power_watts: 100,
                                port: dev_id.to_string(),
                            });
                        }
                    }
                }
            }
        }

        devices
    }

    #[cfg(target_os = "linux")]
    fn classify_usb_speed(line: &str) -> UsbSpeed {
        let lower = line.to_lowercase();
        if lower.contains("super speed") || lower.contains("5000") {
            UsbSpeed::Usb30
        } else if lower.contains("super speed plus") || lower.contains("10000") {
            UsbSpeed::Usb31
        } else if lower.contains("20000") {
            UsbSpeed::Usb32
        } else if lower.contains("40000") || lower.contains("usb4") {
            UsbSpeed::Usb40
        } else if lower.contains("high speed") || lower.contains("480") {
            UsbSpeed::Usb20
        } else if lower.contains("full speed") || lower.contains("low speed") {
            UsbSpeed::Usb11
        } else {
            UsbSpeed::Unknown
        }
    }

    #[cfg(target_os = "linux")]
    fn classify_tb_speed_from_device(dev_path: &std::path::Path) -> ThunderboltSpeed {
        let speed_path = dev_path.join("speed");
        if let Ok(speed) = std::fs::read_to_string(&speed_path) {
            let speed_str = speed.trim().to_lowercase();
            if speed_str.contains("40") {
                ThunderboltSpeed::Tb3
            } else if speed_str.contains("80") {
                ThunderboltSpeed::Tb4
            } else if speed_str.contains("120") {
                ThunderboltSpeed::Tb5
            } else {
                ThunderboltSpeed::Tb3
            }
        } else {
            ThunderboltSpeed::Tb3
        }
    }

    #[cfg(target_os = "linux")]
    fn read_tb_max_power(dev_path: &std::path::Path) -> u32 {
        let power_path = dev_path.join("max_power");
        std::fs::read_to_string(power_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn extract_pnp_vendor_id(dev_id: &str) -> u16 {
        dev_id
            .split('\\')
            .find(|s| s.starts_with("VEN_"))
            .and_then(|s| s.strip_prefix("VEN_"))
            .and_then(|s| u16::from_str_radix(s, 16).ok())
            .unwrap_or(0)
    }

    fn extract_pnp_product_id(dev_id: &str) -> u16 {
        dev_id
            .split('\\')
            .find(|s| s.starts_with("DEV_"))
            .and_then(|s| s.strip_prefix("DEV_"))
            .and_then(|s| u16::from_str_radix(s, 16).ok())
            .unwrap_or(0)
    }

    fn extract_pnp_parent(dev_id: &str) -> String {
        dev_id.split('\\').nth(2).unwrap_or("unknown").to_string()
    }

    fn scan_pci() -> Vec<PciDevice> {
        let mut devices = Vec::new();

        #[cfg(target_os = "windows")]
        {
            if let Ok(output) = std::process::Command::new("wmic")
                .args([
                    "path",
                    "win32_pnpentity",
                    "get",
                    "DeviceID,Name",
                    "/format:csv",
                ])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines().skip(1) {
                    let parts: Vec<&str> = line.split(',').collect();
                    if parts.len() >= 2 {
                        let name = parts[1].trim().to_string();
                        let dev_id = parts[2].trim();
                        if let Some(vid_str) = dev_id.strip_prefix("PCI\\VEN_") {
                            let vid_hex: String = vid_str.chars().take(4).collect();
                            let did_hex: String = vid_str.chars().skip(5).take(4).collect();
                            if let (Ok(vid), Ok(did)) = (
                                u16::from_str_radix(&vid_hex, 16),
                                u16::from_str_radix(&did_hex, 16),
                            ) {
                                devices.push(PciDevice {
                                    vendor_id: vid,
                                    device_id: did,
                                    class: 0,
                                    subclass: 0,
                                    name,
                                });
                            }
                        }
                    }
                }
            }
        }

        devices
    }

    pub fn mock_legacy() -> Self {
        Self {
            cpu: CpuInfo {
                cores: 2,
                threads: 4,
                model: "Intel Core i5-3570".into(),
                has_avx512: false,
                has_avx2: true,
                has_sse42: true,
                has_neon: false,
                base_freq_mhz: 3400,
                vendor: CpuVendor::Intel,
            },
            gpu: None,
            npu: None,
            memory: MemoryInfo {
                total_mb: 8192,
                available_mb: 4096,
                speed_mhz: 1600,
                dimm_count: 2,
            },
            pci_devices: vec![PciDevice {
                vendor_id: 0x8086,
                device_id: 0x1503,
                class: 2,
                subclass: 0,
                name: "Intel Ethernet Controller".into(),
            }],
            storage_devices: vec![StorageDevice {
                name: "Samsung SSD 840".into(),
                interface: StorageInterface::SATA,
                capacity_gb: 256,
                model: "Samsung SSD 840".into(),
            }],
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        }
    }

    pub fn mock_intel_meteor_lake() -> Self {
        Self {
            cpu: CpuInfo {
                cores: 14,
                threads: 18,
                model: "Intel Core Ultra 7 155H".into(),
                has_avx512: true,
                has_avx2: true,
                has_sse42: true,
                has_neon: false,
                base_freq_mhz: 3800,
                vendor: CpuVendor::Intel,
            },
            gpu: Some(GpuInfo {
                name: "Intel Arc Graphics".into(),
                vram_mb: 0,
                compute_shaders: true,
                vendor: "Intel".into(),
                driver_version: "31.0.101.5186".into(),
                cuda_cores: 0,
                compute_capability: String::new(),
            }),
            npu: Some(NpuInfo {
                name: "Intel AI Boost (Meteor Lake NPU)".into(),
                tops: 11,
                supported_frameworks: vec!["ONNX".into(), "OpenVINO".into(), "DirectML".into()],
            }),
            memory: MemoryInfo {
                total_mb: 32768,
                available_mb: 24000,
                speed_mhz: 6400,
                dimm_count: 2,
            },
            pci_devices: vec![PciDevice {
                vendor_id: 0x8086,
                device_id: 0x7D0B,
                class: 4,
                subclass: 80,
                name: "Intel Meteor Lake NPU".into(),
            }],
            storage_devices: vec![StorageDevice {
                name: "WD Black SN850X".into(),
                interface: StorageInterface::NVMe,
                capacity_gb: 2048,
                model: "WD Black SN850X 2TB".into(),
            }],
            usb_devices: Vec::new(),
            thunderbolt_devices: vec![ThunderboltDevice {
                name: "Intel Thunderbolt Controller".into(),
                vendor_id: 0x8086,
                device_id: 0x1137,
                speed: ThunderboltSpeed::Tb4,
                max_power_watts: 100,
                port: "0-0".into(),
            }],
        }
    }

    pub fn mock_qualcomm_x_elite() -> Self {
        Self {
            cpu: CpuInfo {
                cores: 12,
                threads: 12,
                model: "Snapdragon(R) X Elite - X1E-80-100".into(),
                has_avx512: false,
                has_avx2: false,
                has_sse42: false,
                has_neon: true,
                base_freq_mhz: 3400,
                vendor: CpuVendor::ARM,
            },
            gpu: Some(GpuInfo {
                name: "Qualcomm Adreno GPU".into(),
                vram_mb: 0,
                compute_shaders: true,
                vendor: "Qualcomm".into(),
                driver_version: String::new(),
                cuda_cores: 0,
                compute_capability: String::new(),
            }),
            npu: Some(NpuInfo {
                name: "Qualcomm Hexagon NPU".into(),
                tops: 45,
                supported_frameworks: vec![
                    "ONNX".into(),
                    "QNN".into(),
                    "DirectML".into(),
                    "TensorFlow Lite".into(),
                ],
            }),
            memory: MemoryInfo {
                total_mb: 32768,
                available_mb: 24000,
                speed_mhz: 8533,
                dimm_count: 1,
            },
            pci_devices: Vec::new(),
            storage_devices: vec![StorageDevice {
                name: "Samsung PM991a".into(),
                interface: StorageInterface::NVMe,
                capacity_gb: 512,
                model: "Samsung PM991a NVMe".into(),
            }],
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        }
    }

    pub fn mock_modern() -> Self {
        Self {
            cpu: CpuInfo {
                cores: 16,
                threads: 32,
                model: "AMD Ryzen 9 7950X".into(),
                has_avx512: true,
                has_avx2: true,
                has_sse42: true,
                has_neon: false,
                base_freq_mhz: 4500,
                vendor: CpuVendor::AMD,
            },
            gpu: Some(GpuInfo {
                name: "NVIDIA RTX 4090".into(),
                vram_mb: 24576,
                compute_shaders: true,
                vendor: "NVIDIA".into(),
                driver_version: "546.33".into(),
                cuda_cores: 16384,
                compute_capability: "8.9".into(),
            }),
            npu: Some(NpuInfo {
                name: "AMD XDNA2 NPU".into(),
                tops: 50,
                supported_frameworks: vec!["ONNX".into(), "DirectML".into(), "ROCm".into()],
            }),
            memory: MemoryInfo {
                total_mb: 65536,
                available_mb: 48000,
                speed_mhz: 6000,
                dimm_count: 4,
            },
            pci_devices: vec![PciDevice {
                vendor_id: 0x10DE,
                device_id: 0x2684,
                class: 3,
                subclass: 0,
                name: "NVIDIA RTX 4090".into(),
            }],
            storage_devices: vec![
                StorageDevice {
                    name: "Samsung 990 PRO".into(),
                    interface: StorageInterface::NVMe,
                    capacity_gb: 2048,
                    model: "Samsung 990 PRO 2TB".into(),
                },
                StorageDevice {
                    name: "WD Black SN850X".into(),
                    interface: StorageInterface::NVMe,
                    capacity_gb: 4096,
                    model: "WD Black SN850X 4TB".into(),
                },
            ],
            usb_devices: vec![UsbDevice {
                name: "Logitech USB Receiver".into(),
                vendor_id: 0x046D,
                product_id: 0xC52B,
                speed: UsbSpeed::Usb20,
                is_hub: false,
                port: "Bus 1".into(),
            }],
            thunderbolt_devices: Vec::new(),
        }
    }

    pub fn mock_legacy_2012() -> Self {
        Self {
            cpu: CpuInfo {
                cores: 2,
                threads: 4,
                model: "Intel Core i3-3220".into(),
                has_avx512: false,
                has_avx2: false,
                has_sse42: false,
                has_neon: false,
                base_freq_mhz: 3300,
                vendor: CpuVendor::Intel,
            },
            gpu: Some(GpuInfo {
                name: "Intel HD Graphics 2500".into(),
                vram_mb: 0,
                compute_shaders: false,
                vendor: "Intel".into(),
                driver_version: String::new(),
                cuda_cores: 0,
                compute_capability: String::new(),
            }),
            npu: None,
            memory: MemoryInfo {
                total_mb: 4096,
                available_mb: 2048,
                speed_mhz: 1333,
                dimm_count: 2,
            },
            pci_devices: Vec::new(),
            storage_devices: vec![StorageDevice {
                name: "Seagate HDD".into(),
                interface: StorageInterface::SATA,
                capacity_gb: 500,
                model: "Seagate Barracuda 500GB".into(),
            }],
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        }
    }

    pub fn mock_nvidia() -> Self {
        Self {
            cpu: CpuInfo {
                cores: 24,
                threads: 48,
                model: "AMD Ryzen 9 7950X3D".into(),
                has_avx512: true,
                has_avx2: true,
                has_sse42: true,
                has_neon: false,
                base_freq_mhz: 4200,
                vendor: CpuVendor::AMD,
            },
            gpu: Some(GpuInfo {
                name: "NVIDIA GeForce RTX 4090".into(),
                vram_mb: 24576,
                compute_shaders: true,
                vendor: "NVIDIA".into(),
                driver_version: "551.86".into(),
                cuda_cores: 16384,
                compute_capability: "8.9".into(),
            }),
            npu: None,
            memory: MemoryInfo {
                total_mb: 131072,
                available_mb: 96000,
                speed_mhz: 6000,
                dimm_count: 4,
            },
            pci_devices: vec![PciDevice {
                vendor_id: 0x10DE,
                device_id: 0x2684,
                class: 3,
                subclass: 0,
                name: "NVIDIA GeForce RTX 4090".into(),
            }],
            storage_devices: vec![StorageDevice {
                name: "Samsung 990 PRO".into(),
                interface: StorageInterface::NVMe,
                capacity_gb: 4096,
                model: "Samsung 990 PRO 4TB".into(),
            }],
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        }
    }
}

fn detect_cpu_model() -> String {
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("wmic")
            .args(["cpu", "get", "name"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = stdout.lines().collect();
            if lines.len() > 1 {
                let name = lines[1].trim();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in content.lines() {
                if let Some(model) = line.strip_prefix("model name") {
                    let name = model.trim_start_matches(':').trim();
                    if !name.is_empty() {
                        return name.to_string();
                    }
                }
            }
        }
    }
    "Unknown CPU".into()
}

#[cfg(target_arch = "x86_64")]
fn detect_cpu_vendor_x86() -> CpuVendor {
    let model = detect_cpu_model();
    let lower = model.to_lowercase();
    if lower.contains("intel") || lower.contains("core") || lower.contains("xeon") {
        CpuVendor::Intel
    } else if lower.contains("amd") || lower.contains("ryzen") || lower.contains("epyc") {
        CpuVendor::AMD
    } else {
        CpuVendor::Unknown
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn detect_cpu_vendor_x86() -> CpuVendor {
    CpuVendor::Unknown
}

pub struct HalBlock {
    id: BlockId,
    profile: HardwareProfile,
}

impl HalBlock {
    pub fn new(id: BlockId) -> Self {
        let profile = HardwareProfile::detect();
        log::info!(
            "HAL: Detected hardware — {} cores, AVX2={}, AVX512={}, NPU={}",
            profile.cpu.cores,
            profile.cpu.has_avx2,
            profile.cpu.has_avx512,
            profile.npu.is_some()
        );
        Self { id, profile }
    }

    pub fn with_profile(id: BlockId, profile: HardwareProfile) -> Self {
        Self { id, profile }
    }

    pub fn profile(&self) -> &HardwareProfile {
        &self.profile
    }
}

impl StatefulBlock for HalBlock {
    fn id(&self) -> BlockId {
        self.id
    }

    fn name(&self) -> &str {
        "hal"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn state(&self) -> BlockState {
        BlockState::Active
    }

    fn handle_message(&mut self, packet: &IpcPacket) -> Result<Option<IpcPacket>> {
        match packet.header.command_id {
            cmd if cmd == CommandId::HealthCheck as u16 => {
                let data = bincode::serialize(&self.profile)
                    .map_err(|e| AIOSException::SerializationError(e.to_string()))?;
                Ok(Some(IpcPacket::response_ok(
                    self.id.0,
                    packet.header.source_block,
                    packet.header.packet_id,
                    Payload::Binary(data),
                )))
            }
            cmd if cmd == CommandId::Custom as u16 => {
                if let Payload::Custom(ref cmd_name, _) = packet.payload {
                    if cmd_name == "get_hardware_profile" {
                        let data = bincode::serialize(&self.profile)
                            .map_err(|e| AIOSException::SerializationError(e.to_string()))?;
                        return Ok(Some(IpcPacket::response_ok(
                            self.id.0,
                            packet.header.source_block,
                            packet.header.packet_id,
                            Payload::Binary(data),
                        )));
                    }
                }
                Err(AIOSException::IPCError("HAL unknown custom command".into()))
            }
            _ => Err(AIOSException::IPCError(format!(
                "HAL does not handle command 0x{:04X}",
                packet.header.command_id
            ))),
        }
    }

    fn health_check(&self) -> bool {
        self.profile.cpu.cores > 0
    }

    fn extract_state(&self) -> Result<Vec<u8>> {
        bincode::serialize(&self.profile)
            .map_err(|e| AIOSException::StateExtractionFailed(e.to_string()))
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<()> {
        self.profile = bincode::deserialize(state)
            .map_err(|e| AIOSException::StateRestoreFailed(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wmic_memory_csv_full_rows() {
        let csv = "\nNode,Capacity,Speed,DimmLocator\n\nMYPC,8589934592,3200,DIMM_A\n\nMYPC,8589934592,3200,DIMM_B\n\n";
        let mem = HardwareProfile::parse_wmic_memory_csv(csv);
        assert_eq!(mem.total_mb, 16384);
        assert_eq!(mem.speed_mhz, 3200);
        assert_eq!(mem.dimm_count, 2);
    }

    #[test]
    fn test_parse_wmic_memory_csv_short_rows_no_panic() {
        let csv =
            "\nNode,Capacity,Speed,DimmLocator\n\nMYPC,8589934592\n\nMYPC,8589934592,3200,DIMM_A\n";
        let mem = HardwareProfile::parse_wmic_memory_csv(csv);
        assert_eq!(mem.dimm_count, 1);
        assert_eq!(mem.speed_mhz, 3200);
    }

    #[test]
    fn test_gpu_info_new_fields() {
        let gpu = GpuInfo {
            name: "RTX 4090".into(),
            vram_mb: 24576,
            compute_shaders: true,
            vendor: "NVIDIA".into(),
            driver_version: "546.33".into(),
            cuda_cores: 16384,
            compute_capability: "8.9".into(),
        };
        assert_eq!(gpu.cuda_cores, 16384);
        assert_eq!(gpu.compute_capability, "8.9");
        assert_eq!(gpu.driver_version, "546.33");
    }

    #[test]
    fn test_mock_nvidia_profile() {
        let profile = HardwareProfile::mock_nvidia();
        let gpu = profile.gpu.as_ref().unwrap();
        assert_eq!(gpu.vendor, "NVIDIA");
        assert!(gpu.cuda_cores > 0);
        assert!(!gpu.compute_capability.is_empty());
        assert!(!gpu.driver_version.is_empty());
    }

    #[test]
    fn test_estimate_cuda_cores() {
        assert_eq!(
            HardwareProfile::estimate_cuda_cores("NVIDIA RTX 4090"),
            16384
        );
        assert_eq!(
            HardwareProfile::estimate_cuda_cores("NVIDIA RTX 3080"),
            8704
        );
        assert_eq!(HardwareProfile::estimate_cuda_cores("NVIDIA A100"), 6912);
        assert_eq!(HardwareProfile::estimate_cuda_cores("NVIDIA H100"), 16896);
        assert_eq!(HardwareProfile::estimate_cuda_cores("Intel UHD 630"), 0);
    }

    #[test]
    fn test_mock_modern_has_nvidia_gpu() {
        let profile = HardwareProfile::mock_modern();
        let gpu = profile.gpu.as_ref().unwrap();
        assert_eq!(gpu.vendor, "NVIDIA");
        assert_eq!(gpu.cuda_cores, 16384);
    }

    #[test]
    fn test_mock_legacy_no_gpu() {
        let profile = HardwareProfile::mock_legacy();
        assert!(profile.gpu.is_none());
    }

    #[test]
    fn test_mock_legacy_2012_integrated_gpu() {
        let profile = HardwareProfile::mock_legacy_2012();
        let gpu = profile.gpu.as_ref().unwrap();
        assert_eq!(gpu.vendor, "Intel");
        assert_eq!(gpu.cuda_cores, 0);
    }

    #[test]
    fn test_gpu_serialization_roundtrip() {
        let gpu = GpuInfo {
            name: "Test GPU".into(),
            vram_mb: 8192,
            compute_shaders: true,
            vendor: "NVIDIA".into(),
            driver_version: "1.0.0".into(),
            cuda_cores: 4096,
            compute_capability: "8.0".into(),
        };
        let bytes = bincode::serialize(&gpu).unwrap();
        let restored: GpuInfo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.name, "Test GPU");
        assert_eq!(restored.cuda_cores, 4096);
        assert_eq!(restored.compute_capability, "8.0");
    }

    #[test]
    fn test_mock_modern_has_nvme_storage() {
        let profile = HardwareProfile::mock_modern();
        assert_eq!(profile.storage_devices.len(), 2);
        assert_eq!(profile.storage_devices[0].interface, StorageInterface::NVMe);
        assert!(profile.storage_devices[0].capacity_gb > 0);
    }

    #[test]
    fn test_mock_legacy_has_sata_storage() {
        let profile = HardwareProfile::mock_legacy();
        assert_eq!(profile.storage_devices.len(), 1);
        assert_eq!(profile.storage_devices[0].interface, StorageInterface::SATA);
    }

    #[test]
    fn test_mock_legacy_2012_has_sata_storage() {
        let profile = HardwareProfile::mock_legacy_2012();
        assert_eq!(profile.storage_devices.len(), 1);
        assert_eq!(profile.storage_devices[0].interface, StorageInterface::SATA);
    }

    #[test]
    fn test_mock_nvidia_has_nvme_storage() {
        let profile = HardwareProfile::mock_nvidia();
        assert_eq!(profile.storage_devices.len(), 1);
        assert_eq!(profile.storage_devices[0].interface, StorageInterface::NVMe);
    }

    #[test]
    fn test_storage_device_serialization_roundtrip() {
        let dev = StorageDevice {
            name: "Test SSD".into(),
            interface: StorageInterface::NVMe,
            capacity_gb: 1024,
            model: "Test Model".into(),
        };
        let bytes = bincode::serialize(&dev).unwrap();
        let restored: StorageDevice = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.name, "Test SSD");
        assert_eq!(restored.interface, StorageInterface::NVMe);
        assert_eq!(restored.capacity_gb, 1024);
    }

    #[test]
    fn test_hardware_profile_serialization_with_storage() {
        let profile = HardwareProfile::mock_modern();
        let bytes = bincode::serialize(&profile).unwrap();
        let restored: HardwareProfile = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.storage_devices.len(), 2);
        assert_eq!(restored.storage_devices[0].name, "Samsung 990 PRO");
    }

    #[test]
    fn test_mock_intel_meteor_lake_npu() {
        let profile = HardwareProfile::mock_intel_meteor_lake();
        let npu = profile.npu.as_ref().unwrap();
        assert_eq!(npu.name, "Intel AI Boost (Meteor Lake NPU)");
        assert_eq!(npu.tops, 11);
        assert!(npu.supported_frameworks.contains(&"OpenVINO".to_string()));
    }

    #[test]
    fn test_mock_qualcomm_x_elite_npu() {
        let profile = HardwareProfile::mock_qualcomm_x_elite();
        let npu = profile.npu.as_ref().unwrap();
        assert_eq!(npu.name, "Qualcomm Hexagon NPU");
        assert_eq!(npu.tops, 45);
        assert!(npu.supported_frameworks.contains(&"QNN".to_string()));
    }

    #[test]
    fn test_mock_qualcomm_x_elite_has_neon() {
        let profile = HardwareProfile::mock_qualcomm_x_elite();
        assert!(profile.cpu.has_neon);
        assert_eq!(profile.cpu.vendor, CpuVendor::ARM);
    }

    #[test]
    fn test_mock_intel_meteor_lake_has_npu_pci_device() {
        let profile = HardwareProfile::mock_intel_meteor_lake();
        assert!(profile.pci_devices.iter().any(|d| d.device_id == 0x7D0B));
    }

    #[test]
    fn test_mock_intel_meteor_lake_has_thunderbolt() {
        let profile = HardwareProfile::mock_intel_meteor_lake();
        assert_eq!(profile.thunderbolt_devices.len(), 1);
        assert_eq!(profile.thunderbolt_devices[0].speed, ThunderboltSpeed::Tb4);
        assert_eq!(profile.thunderbolt_devices[0].max_power_watts, 100);
    }

    #[test]
    fn test_mock_modern_has_usb_devices() {
        let profile = HardwareProfile::mock_modern();
        assert_eq!(profile.usb_devices.len(), 1);
        assert_eq!(profile.usb_devices[0].vendor_id, 0x046D);
    }

    #[test]
    fn test_usb_device_serialization_roundtrip() {
        let dev = UsbDevice {
            name: "Test USB Device".into(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            speed: UsbSpeed::Usb32,
            is_hub: false,
            port: "Bus 2".into(),
        };
        let bytes = bincode::serialize(&dev).unwrap();
        let restored: UsbDevice = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.name, "Test USB Device");
        assert_eq!(restored.speed, UsbSpeed::Usb32);
        assert_eq!(restored.vendor_id, 0x1234);
    }

    #[test]
    fn test_thunderbolt_device_serialization_roundtrip() {
        let dev = ThunderboltDevice {
            name: "Intel TB4 Controller".into(),
            vendor_id: 0x8086,
            device_id: 0x1137,
            speed: ThunderboltSpeed::Tb4,
            max_power_watts: 100,
            port: "0-0".into(),
        };
        let bytes = bincode::serialize(&dev).unwrap();
        let restored: ThunderboltDevice = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.name, "Intel TB4 Controller");
        assert_eq!(restored.speed, ThunderboltSpeed::Tb4);
        assert_eq!(restored.max_power_watts, 100);
    }

    #[test]
    fn test_hardware_profile_serialization_with_usb_and_tb() {
        let profile = HardwareProfile::mock_modern();
        let bytes = bincode::serialize(&profile).unwrap();
        let restored: HardwareProfile = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.usb_devices.len(), 1);
        assert_eq!(restored.thunderbolt_devices.len(), 0);
    }

    #[test]
    fn test_intel_meteor_lake_profile_serialization() {
        let profile = HardwareProfile::mock_intel_meteor_lake();
        let bytes = bincode::serialize(&profile).unwrap();
        let restored: HardwareProfile = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.thunderbolt_devices.len(), 1);
        assert!(restored.npu.is_some());
    }

    #[test]
    fn test_mock_legacy_has_no_usb_or_tb() {
        let profile = HardwareProfile::mock_legacy();
        assert!(profile.usb_devices.is_empty());
        assert!(profile.thunderbolt_devices.is_empty());
    }
}
