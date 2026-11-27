//! Boot hand-off shared by `aios-boot` (UEFI) and the Limine-protocol path.
//!
//! The loader validates and loads the kernel ELF, sets up higher-half paging
//! and enters [`crate::_start`]. `aios-boot` passes a pointer to
//! [`aios_bootinfo::BootInfo`] in `rdi`; the Limine build translates its
//! resolved requests into the same structure. This module owns the one-time
//! validation and the typed accessors the rest of the kernel uses: the
//! direct-map offset, the physical memory map, the ACPI RSDP, the linear
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
/// `ptr` must point to a readable, page-mapped [`BootInfo`] written by the boot
/// loader (or translated from Limine's responses). Called exactly once, before
/// any other subsystem uses the boot information.
pub unsafe fn init(ptr: *const BootInfo) -> Result<&'static BootInfo, &'static str> {
    if ptr.is_null() {
        return Err("boot: null BootInfo pointer");
    }
    // SAFETY: `init` checked the pointer is non-null and the loader wrote a
    // valid, page-mapped BootInfo before handing it to the kernel.
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
        // SAFETY: the pointer was validated and published by `init` with
        // release/acquire ordering; BootInfo storage is static for the kernel's
        // lifetime.
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

/// The linear framebuffer described by the loader, if one was found.
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
