use aios_core::error::Result;

#[cfg(target_os = "windows")]
mod windows_affinity {
    use aios_core::error::{AIOSException, Result};

    type Handle = *mut core::ffi::c_void;

    extern "system" {
        fn SetThreadAffinityMask(hThread: Handle, dwThreadAffinityMask: usize) -> usize;
    }

    extern "system" {
        fn GetCurrentThread() -> Handle;
    }

    pub fn validate_cores(cores: &[usize]) -> Result<()> {
        if cores.is_empty() {
            return Err(AIOSException::SchedulerError(
                "CPU affinity requires at least one core".into(),
            ));
        }
        for &core in cores {
            if core >= 64 {
                return Err(AIOSException::SchedulerError(format!(
                    "Core index {} exceeds maximum (63)",
                    core
                )));
            }
        }
        Ok(())
    }

    pub fn set_thread_affinity(cores: &[usize]) -> Result<()> {
        validate_cores(cores)?;

        let mut mask: usize = 0;
        for &core in cores {
            mask |= 1usize << core;
        }

        let handle = unsafe { GetCurrentThread() };
        let prev = unsafe { SetThreadAffinityMask(handle, mask) };

        if prev == 0 {
            return Err(AIOSException::SchedulerError(format!(
                "SetThreadAffinityMask failed (mask=0x{:X})",
                mask
            )));
        }

        log::info!("CPU affinity: set to cores {:?} (mask=0x{:X})", cores, mask);
        Ok(())
    }

    pub fn available_cores() -> Result<Vec<usize>> {
        let count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Ok((0..count).collect())
    }
}

#[cfg(target_os = "linux")]
mod linux_affinity {
    use aios_core::error::{AIOSException, Result};

    type pid_t = i32;
    type size_t = usize;

    const CPU_SETSIZE: usize = 1024;
    const BITS_PER_LONG: usize = 64;

    #[repr(C)]
    struct cpu_set_t {
        bits: [u64; CPU_SETSIZE / BITS_PER_LONG],
    }

    extern "C" {
        fn sched_setaffinity(pid: pid_t, cpusetsize: size_t, cpuset: *const cpu_set_t) -> pid_t;
    }

    extern "C" {
        fn sysconf(name: i32) -> i64;
    }

    const _SC_NPROCESSORS_ONLN: i32 = 84;

    fn cores_to_set(cores: &[usize]) -> Result<cpu_set_t> {
        let mut set = cpu_set_t {
            bits: [0u64; CPU_SETSIZE / BITS_PER_LONG],
        };
        for &core in cores {
            if core >= CPU_SETSIZE {
                return Err(AIOSException::SchedulerError(format!(
                    "Core index {} exceeds maximum ({})",
                    core, CPU_SETSIZE
                )));
            }
            set.bits[core / BITS_PER_LONG] |= 1u64 << (core % BITS_PER_LONG);
        }
        Ok(set)
    }

    pub fn validate_cores(cores: &[usize]) -> Result<()> {
        if cores.is_empty() {
            return Err(AIOSException::SchedulerError(
                "CPU affinity requires at least one core".into(),
            ));
        }
        for &core in cores {
            if core >= CPU_SETSIZE {
                return Err(AIOSException::SchedulerError(format!(
                    "Core index {} exceeds maximum ({})",
                    core, CPU_SETSIZE
                )));
            }
        }
        Ok(())
    }

    pub fn set_thread_affinity(cores: &[usize]) -> Result<()> {
        validate_cores(cores)?;

        let set = cores_to_set(cores)?;
        let ret = unsafe { sched_setaffinity(0, std::mem::size_of::<cpu_set_t>(), &set) };

        if ret != 0 {
            return Err(AIOSException::SchedulerError(format!(
                "sched_setaffinity failed (cores={:?})",
                cores
            )));
        }

        log::info!("CPU affinity: set to cores {:?}", cores);
        Ok(())
    }

    pub fn available_cores() -> Result<Vec<usize>> {
        let count = unsafe { sysconf(_SC_NPROCESSORS_ONLN) } as usize;
        Ok((0..count).collect())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod fallback_affinity {
    use aios_core::error::{AIOSException, Result};

    pub fn validate_cores(cores: &[usize]) -> Result<()> {
        if cores.is_empty() {
            return Err(AIOSException::SchedulerError(
                "CPU affinity requires at least one core".into(),
            ));
        }
        Ok(())
    }

    pub fn set_thread_affinity(_cores: &[usize]) -> Result<()> {
        log::warn!("CPU affinity not supported on this platform");
        Ok(())
    }

    pub fn available_cores() -> Result<Vec<usize>> {
        let count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Ok((0..count).collect())
    }
}

#[cfg(target_os = "windows")]
use windows_affinity as platform;

#[cfg(target_os = "linux")]
use linux_affinity as platform;

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
use fallback_affinity as platform;

/// Validate a core list without touching the calling thread's OS affinity.
/// Used by the scheduler to fail fast on invalid core sets before handing them
/// to a target process thread.
pub fn validate_cores(cores: &[usize]) -> Result<()> {
    platform::validate_cores(cores)
}

pub fn set_thread_affinity(cores: &[usize]) -> Result<()> {
    platform::set_thread_affinity(cores)
}

pub fn available_cores() -> Result<Vec<usize>> {
    platform::available_cores()
}

pub fn set_current_thread_affinity(cores: &[usize]) -> Result<()> {
    log::info!(
        "CPU affinity: applying to current thread, cores={:?}",
        cores
    );
    platform::set_thread_affinity(cores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_available_cores() {
        let cores = available_cores().unwrap();
        assert!(!cores.is_empty());
        assert!(cores[0] == 0);
    }

    #[test]
    fn test_set_current_thread_affinity_valid() {
        let cores = available_cores().unwrap();
        let result = set_current_thread_affinity(&[cores[0]]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_set_current_thread_affinity_empty() {
        let result = set_current_thread_affinity(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_current_thread_affinity_multi_core() {
        let cores = available_cores().unwrap();
        if cores.len() >= 2 {
            let result = set_current_thread_affinity(&[0, 1]);
            assert!(result.is_ok());
        }
    }
}
