//! Boot hand-off from `aios-boot`, the AIOS UEFI loader.
//!
//! The loader validates and loads the kernel ELF, builds the page tables
//! (higher-half kernel + higher-half direct map) and jumps to [`crate::_start`]
//! with a pointer to [`aios_bootinfo::BootInfo`] in `rdi`. This module owns the
//! one-time validation and the typed accessors the rest of the kernel uses:
//! the direct-map offset, the physical memory map, the ACPI RSDP, the GOP
//! framebuffer and the configuration/block modules.
//!
//! The kernel does not depend on the loader's internal types: everything here
//! goes through the shared `aios-bootinfo` contract.

use aios_bootinfo::{BootInfo, FramebufferInfo, ModuleInfo};
use core::sync::atomic::{AtomicUsize, Ordering};

static BOOT_INFO_PTR: AtomicUsize = AtomicUsize::new(0);

/// Validates the boot hand-off and publishes it for the rest of the kernel.
///
/// # Safety
/// `ptr` must point to a readable, page-mapped [`BootInfo`] written by
/// `aios-boot`. Called exactly once, before any other subsystem uses the boot
/// information.
pub unsafe fn init(ptr: *const BootInfo) -> Result<&'static BootInfo, &'static str> {
    if ptr.is_null() {
        return Err("boot: null BootInfo pointer");
    }
    let info = unsafe { &*ptr };
    if !info.is_valid() {
        return Err("boot: invalid BootInfo magic/version/size");
    }
    BOOT_INFO_PTR.store(ptr as usize, Ordering::Release);
    Ok(info)
}

/// The validated boot information, if [`init`] succeeded.
pub fn info() -> Option<&'static BootInfo> {
    let ptr = BOOT_INFO_PTR.load(Ordering::Acquire) as *const BootInfo;
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

/// Direct-map base: physical `p` is accessible at `hhdm_offset() + p`.
pub fn hhdm_offset() -> u64 {
    info().map(|info| info.hhdm_offset).unwrap_or(0)
}

/// Physical address of the ACPI RSDP, if the firmware provided one.
pub fn rsdp() -> Option<u64> {
    info().and_then(|info| (info.rsdp != 0).then_some(info.rsdp))
}

/// The GOP framebuffer described by the loader, if one was found.
pub fn framebuffer() -> Option<FramebufferInfo> {
    info()
        .map(|info| info.framebuffer)
        .filter(|fb| fb.is_present())
}

/// Modules (configuration files and WASM blocks) loaded from the AIOS
/// partition by the loader.
pub fn modules() -> &'static [ModuleInfo] {
    info().map(|info| info.modules()).unwrap_or(&[])
}

/// Number of loaded configuration modules.
pub fn config_count() -> usize {
    modules()
        .iter()
        .filter(|module| module.kind == aios_bootinfo::MODULE_CONFIG)
        .count()
}

/// Number of loaded WASM blocks.
pub fn block_count() -> usize {
    modules()
        .iter()
        .filter(|module| module.kind == aios_bootinfo::MODULE_BLOCK)
        .count()
}
