use serde::Serialize;
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct CpuInfo {
    pub brand: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    pub architecture: String,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub total_gb: f64,
    pub used_gb: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub model: String,
    pub vram_bytes: u64,
    pub vram_gb: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OsInfo {
    pub name: String,
    pub kernel_version: String,
    pub os_version: String,
    pub uptime_secs: u64,
    pub hostname: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HwProfile {
    pub cpu: CpuInfo,
    pub memory: MemInfo,
    pub gpu: Option<GpuInfo>,
    pub os: OsInfo,
    pub ai_tier: String,
}

pub fn probe() -> HwProfile {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    std::thread::sleep(Duration::from_millis(200));
    sys.refresh_memory();

    let cpu = probe_cpu(&sys);
    let memory = probe_memory(&sys);
    let gpu = probe_gpu();
    let os = probe_os(&sys);
    let ai_tier = determine_ai_tier(&cpu, &memory, &gpu);

    HwProfile {
        cpu,
        memory,
        gpu,
        os,
        ai_tier,
    }
}

fn probe_cpu(sys: &sysinfo::System) -> CpuInfo {
    let cpus = sys.cpus();
    let brand = cpus
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();
    let logical_cores = cpus.len();
    let physical_cores = sys.physical_core_count().unwrap_or(logical_cores);

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "ARM64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "unknown"
    };

    let mut flags = Vec::new();
    if cfg!(target_arch = "x86_64") || cfg!(target_arch = "x86") {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                flags.push("AVX2".into());
            }
            if std::arch::is_x86_feature_detected!("avx512f") {
                flags.push("AVX-512".into());
            }
            if std::arch::is_x86_feature_detected!("avx") {
                flags.push("AVX".into());
            }
            if std::arch::is_x86_feature_detected!("sse4.2") {
                flags.push("SSE4.2".into());
            }
            if std::arch::is_x86_feature_detected!("sse4.1") {
                flags.push("SSE4.1".into());
            }
            if std::arch::is_x86_feature_detected!("aes") {
                flags.push("AES-NI".into());
            }
        }
    }
    if cfg!(target_arch = "aarch64") {
        flags.push("NEON".into());
    }

    CpuInfo {
        brand,
        physical_cores,
        logical_cores,
        architecture: arch.into(),
        flags,
    }
}

fn probe_memory(sys: &sysinfo::System) -> MemInfo {
    let total = sys.total_memory();
    let used = sys.used_memory();
    let free = sys.free_memory();
    MemInfo {
        total_bytes: total,
        used_bytes: used,
        free_bytes: free,
        total_gb: total as f64 / 1_073_741_824.0,
        used_gb: used as f64 / 1_073_741_824.0,
    }
}

fn gpu_from_hal(gpu: &aios_hal::hardware::GpuInfo) -> GpuInfo {
    GpuInfo {
        model: gpu.name.clone(),
        vram_bytes: gpu.vram_mb.saturating_mul(1_048_576),
        vram_gb: gpu.vram_mb as f64 / 1024.0,
    }
}

fn probe_gpu() -> Option<GpuInfo> {
    // Prefer the HAL detection: it reads the real VRAM through nvidia-smi
    // (`memory.total`, MiB) / rocm-smi. WMI's win32_VideoController.AdapterRAM
    // is a 32-bit field that wraps for GPUs above 4 GiB (an RTX 3060 reports
    // 0xFFF00000 -> "4.0 GB"), so it is only kept as a last-resort name source.
    if let Some(gpu) = aios_hal::hardware::HardwareProfile::detect().gpu {
        return Some(gpu_from_hal(&gpu));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(out) = String::from_utf8(
            Command::new("wmic")
                .args([
                    "path",
                    "win32_VideoController",
                    "get",
                    "Name,AdapterRAM",
                    "/format:value",
                ])
                .output()
                .ok()?
                .stdout,
        ) {
            let mut model = String::new();
            let mut vram = 0u64;
            for line in out.lines() {
                if let Some(val) = line.strip_prefix("Name=") {
                    model = val.trim().to_string();
                }
                if let Some(val) = line.strip_prefix("AdapterRAM=") {
                    let parsed = val.trim().parse::<u64>().unwrap_or(0);
                    // AdapterRAM is a 32-bit field; drivers report 0xFFFFFFFF
                    // for cards with >4 GB VRAM (or unknown size), which is not
                    // a real amount of memory.
                    vram = if parsed == 0xFFFF_FFFF { 0 } else { parsed };
                }
            }
            if !model.is_empty() {
                return Some(GpuInfo {
                    model,
                    vram_bytes: vram,
                    vram_gb: vram as f64 / 1_073_741_824.0,
                });
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = String::from_utf8(
            Command::new("nvidia-smi")
                .args([
                    "--query-gpu=name,memory.total",
                    "--format=csv,noheader,nounits",
                ])
                .output()
                .ok()?
                .stdout,
        ) {
            if let Some(line) = out.lines().next() {
                let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                if parts.len() == 2 {
                    if let Ok(vram) = parts[1].parse::<u64>() {
                        return Some(GpuInfo {
                            model: parts[0].to_string(),
                            vram_bytes: vram * 1_048_576,
                            vram_gb: vram as f64 / 1024.0,
                        });
                    }
                }
            }
        }
        if let Ok(out) = String::from_utf8(Command::new("lspci").arg("-v").output().ok()?.stdout) {
            for line in out.lines() {
                if line.contains("VGA") || line.contains("3D") || line.contains("Display") {
                    let clean = line.split(':').nth(1).unwrap_or(line).trim().to_string();
                    return Some(GpuInfo {
                        model: clean,
                        vram_bytes: 0,
                        vram_gb: 0.0,
                    });
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = String::from_utf8(
            Command::new("system_profiler")
                .args(["SPDisplaysDataType"])
                .output()
                .ok()?
                .stdout,
        ) {
            let mut model = String::new();
            for line in out.lines() {
                if let Some(val) = line.split(':').nth(1) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty()
                        && !trimmed.contains("Display")
                        && !trimmed.contains("Resolution")
                    {
                        model = trimmed.to_string();
                    }
                }
            }
            if !model.is_empty() {
                return Some(GpuInfo {
                    model,
                    vram_bytes: 0,
                    vram_gb: 0.0,
                });
            }
        }
    }

    None
}

fn probe_os(_sys: &sysinfo::System) -> OsInfo {
    let name = sysinfo::System::name().unwrap_or_else(|| {
        if cfg!(target_os = "windows") {
            "Windows".into()
        } else if cfg!(target_os = "linux") {
            "Linux".into()
        } else if cfg!(target_os = "macos") {
            "macOS".into()
        } else {
            "Unknown".into()
        }
    });
    let kernel = sysinfo::System::kernel_version().unwrap_or_default();
    let version = sysinfo::System::os_version().unwrap_or_default();
    let uptime = sysinfo::System::uptime();
    let hostname = sysinfo::System::host_name().unwrap_or_default();

    OsInfo {
        name,
        kernel_version: kernel,
        os_version: version,
        uptime_secs: uptime,
        hostname,
    }
}

fn determine_ai_tier(cpu: &CpuInfo, mem: &MemInfo, gpu: &Option<GpuInfo>) -> String {
    let has_avx512 = cpu.flags.iter().any(|f| f == "AVX-512");
    let has_avx2 = cpu.flags.iter().any(|f| f == "AVX2" || f == "NEON");
    let has_gpu = gpu.is_some();
    let ram_gb = mem.total_gb;

    if (has_avx512 || (has_avx2 && ram_gb >= 16.0)) && has_gpu && ram_gb >= 8.0 {
        "Tier1 (High Performance)".into()
    } else if has_avx2 && ram_gb >= 4.0 {
        "Tier2 (Mid Range)".into()
    } else {
        "Tier3 (Fallback)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hal_gpu(vram_mb: u64) -> aios_hal::hardware::GpuInfo {
        aios_hal::hardware::GpuInfo {
            name: "NVIDIA GeForce RTX 3060".into(),
            vram_mb,
            compute_shaders: true,
            vendor: "NVIDIA".into(),
            driver_version: "test".into(),
            cuda_cores: 3584,
            compute_capability: "8.6".into(),
        }
    }

    #[test]
    fn hal_gpu_vram_mb_is_converted_to_gi_bytes() {
        let gpu = gpu_from_hal(&hal_gpu(12288));
        assert_eq!(gpu.vram_bytes, 12_884_901_888);
        assert!((gpu.vram_gb - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn hal_gpu_without_vram_reports_zero_gib() {
        let gpu = gpu_from_hal(&hal_gpu(0));
        assert_eq!(gpu.vram_bytes, 0);
        assert_eq!(gpu.vram_gb, 0.0);
    }

    #[test]
    fn wmi_adapterram_wrap_no_longer_reaches_the_tui() {
        let gpu = gpu_from_hal(&hal_gpu(12_288));
        assert!(gpu.vram_gb > 4.0);
    }
}
