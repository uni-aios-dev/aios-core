//! Boot hand-off contract between `aios-boot` (the UEFI loader) and
//! `aios-kernel`.
//!
//! The loader fills one [`BootInfo`] in its own image, enables the kernel page
//! tables (higher-half kernel + higher-half direct map) and jumps to the
//! kernel entry with a pointer to the structure in `rdi`. The pointer is a
//! *virtual* address inside the direct map, so both sides must agree on
//! [`HHDM_BASE`] and on the physical address encoded in the pointer.
//!
//! Everything is `#[repr(C)]`, fixed-size and plain-old-data: the structure is
//! written by one binary and read by another, possibly built by a different
//! compiler version. No pointers are stored — all addresses are physical —
//! except the pointer to the structure itself.

#![no_std]

/// Magic value in [`BootInfo::magic`]: ASCII `AIOSBOOT` in little-endian order.
pub const MAGIC: u64 = 0x4149_4F53_424F_4F54;
/// Layout version understood by this crate. Bump on any field change.
pub const VERSION: u32 = 1;
/// Maximum number of physical memory regions carried in [`BootInfo`].
pub const MAX_MEMORY_REGIONS: usize = 128;
/// Maximum number of modules (configuration files and blocks) in [`BootInfo`].
pub const MAX_MODULES: usize = 64;
/// Maximum length of a module name, including the terminating NUL.
pub const MAX_MODULE_NAME: usize = 64;

/// Direct-map base chosen by the loader.
///
/// Physical address `p` is visible at `HHDM_BASE + p`. The kernel maps its
/// heap, MMIO window and self-test page in PML4 slot 510, so the direct map
/// must stay in slot 256, far away from those.
pub const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;

/// A region of usable physical memory.
pub const MEM_USABLE: u32 = 1;
/// A region that must not be used for allocation (reserved for the loader).
pub const MEM_RESERVED: u32 = 2;

/// A configuration module: text consumed by the kernel after boot.
pub const MODULE_CONFIG: u32 = 1;
/// A WebAssembly block: opaque bytes executed by the `wasmhost` service.
pub const MODULE_BLOCK: u32 = 2;
/// Any other module (fonts, firmware blobs, ...).
pub const MODULE_RAW: u32 = 3;

/// One physical memory range.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryRegion {
    /// First physical byte of the range.
    pub start: u64,
    /// One past the last physical byte of the range.
    pub end: u64,
    /// [`MEM_USABLE`] or [`MEM_RESERVED`].
    pub kind: u32,
    /// Padding, always zero.
    pub reserved: u32,
}

impl MemoryRegion {
    /// An empty region, used to initialise the fixed-size array.
    pub const EMPTY: Self = Self {
        start: 0,
        end: 0,
        kind: 0,
        reserved: 0,
    };

    /// Whether the region may be handed to the frame allocator.
    pub fn is_usable(&self) -> bool {
        self.kind == MEM_USABLE && self.start < self.end
    }
}

/// One module loaded into memory by the bootloader.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ModuleInfo {
    /// NUL-terminated UTF-8 name relative to the AIOS root (`config/net.conf`).
    pub name: [u8; MAX_MODULE_NAME],
    /// Physical address of the first byte of the module.
    pub phys: u64,
    /// Size of the module in bytes.
    pub size: u64,
    /// [`MODULE_CONFIG`], [`MODULE_BLOCK`] or [`MODULE_RAW`].
    pub kind: u32,
    /// Padding, always zero.
    pub reserved: u32,
}

impl ModuleInfo {
    /// An empty module, used to initialise the fixed-size array.
    pub const EMPTY: Self = Self {
        name: [0; MAX_MODULE_NAME],
        phys: 0,
        size: 0,
        kind: 0,
        reserved: 0,
    };

    /// The module name as a byte slice without the terminating NUL.
    pub fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(MAX_MODULE_NAME);
        &self.name[..end]
    }

    /// The module name if it is valid UTF-8.
    pub fn name_str(&self) -> Option<&str> {
        core::str::from_utf8(self.name_bytes()).ok()
    }

    /// One byte per module kind, for logging: `cfg`, `wasm` or `raw`.
    pub fn kind_str(&self) -> &'static str {
        match self.kind {
            MODULE_CONFIG => "cfg",
            MODULE_BLOCK => "wasm",
            _ => "raw",
        }
    }
}

/// The linear framebuffer handed over by the bootloader.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    /// Physical address of the framebuffer base.
    pub address: u64,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Bytes per scanline.
    pub pitch: u32,
    /// Bits per pixel (16, 24 or 32).
    pub bpp: u16,
    /// Red channel shift.
    pub red_shift: u8,
    /// Red channel width in bits.
    pub red_size: u8,
    /// Green channel shift.
    pub green_shift: u8,
    /// Green channel width in bits.
    pub green_size: u8,
    /// Blue channel shift.
    pub blue_shift: u8,
    /// Blue channel width in bits.
    pub blue_size: u8,
    /// Padding, always zero.
    pub reserved: u16,
}

impl FramebufferInfo {
    /// An absent framebuffer. `address == 0` means "serial only".
    pub const NONE: Self = Self {
        address: 0,
        width: 0,
        height: 0,
        pitch: 0,
        bpp: 0,
        red_shift: 0,
        red_size: 0,
        green_shift: 0,
        green_size: 0,
        blue_shift: 0,
        blue_size: 0,
        reserved: 0,
    };

    /// Whether a framebuffer was found by the bootloader.
    pub fn is_present(&self) -> bool {
        self.address != 0 && self.width != 0 && self.height != 0
    }
}

/// The complete boot hand-off.
#[repr(C)]
pub struct BootInfo {
    /// [`MAGIC`].
    pub magic: u64,
    /// [`VERSION`] at load time.
    pub version: u32,
    /// Size of this structure in bytes, used for forward compatibility.
    pub size: u32,
    /// Direct-map base: physical `p` is readable at `hhdm_offset + p`.
    pub hhdm_offset: u64,
    /// Physical address of the kernel image.
    pub kernel_phys: u64,
    /// Virtual address the kernel image was linked at.
    pub kernel_virt: u64,
    /// Size of the loaded kernel image in bytes.
    pub kernel_size: u64,
    /// Physical address of the ACPI RSDP, or 0 when the firmware has none.
    pub rsdp: u64,
    /// The GOP framebuffer, or [`FramebufferInfo::NONE`].
    pub framebuffer: FramebufferInfo,
    /// Number of valid entries in `memory_regions`.
    pub memory_region_count: u32,
    /// Number of valid entries in `modules`.
    pub module_count: u32,
    /// Physical memory map, usable regions only plus loader-reserved ranges.
    pub memory_regions: [MemoryRegion; MAX_MEMORY_REGIONS],
    /// Modules loaded from the AIOS partition.
    pub modules: [ModuleInfo; MAX_MODULES],
}

impl BootInfo {
    /// A zeroed structure with `magic`, `version` and `size` pre-filled.
    pub const fn new() -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            size: core::mem::size_of::<Self>() as u32,
            hhdm_offset: 0,
            kernel_phys: 0,
            kernel_virt: 0,
            kernel_size: 0,
            rsdp: 0,
            framebuffer: FramebufferInfo::NONE,
            memory_region_count: 0,
            module_count: 0,
            memory_regions: [MemoryRegion::EMPTY; MAX_MEMORY_REGIONS],
            modules: [ModuleInfo::EMPTY; MAX_MODULES],
        }
    }

    /// Whether the structure came from a compatible bootloader.
    pub fn is_valid(&self) -> bool {
        self.magic == MAGIC
            && self.version == VERSION
            && self.size as usize == core::mem::size_of::<Self>()
    }

    /// The valid prefix of `memory_regions`.
    pub fn memory_regions(&self) -> &[MemoryRegion] {
        let count = (self.memory_region_count as usize).min(MAX_MEMORY_REGIONS);
        &self.memory_regions[..count]
    }

    /// The valid prefix of `modules`.
    pub fn modules(&self) -> &[ModuleInfo] {
        let count = (self.module_count as usize).min(MAX_MODULES);
        &self.modules[..count]
    }

    /// The virtual address of the structure itself, given its physical address.
    pub fn virtual_address(physical: u64) -> u64 {
        HHDM_BASE + physical
    }
}

impl Default for BootInfo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_stable() {
        assert_eq!(core::mem::size_of::<MemoryRegion>(), 24);
        assert_eq!(core::mem::size_of::<ModuleInfo>(), 88);
        assert_eq!(core::mem::size_of::<FramebufferInfo>(), 32);
        // 96-byte header + 128 regions * 24 + 64 modules * 88.
        assert_eq!(core::mem::size_of::<BootInfo>(), 96 + 3072 + 5632);
        assert_eq!(
            BootInfo::new().size as usize,
            core::mem::size_of::<BootInfo>()
        );
    }

    #[test]
    fn new_is_valid() {
        let info = BootInfo::new();
        assert!(info.is_valid());
        assert_eq!(info.memory_regions().len(), 0);
        assert_eq!(info.modules().len(), 0);
    }

    #[test]
    fn module_name_is_terminated() {
        let mut module = ModuleInfo::EMPTY;
        module.name[..15].copy_from_slice(b"config/net.conf");
        assert_eq!(module.name_str(), Some("config/net.conf"));
        assert_eq!(module.kind_str(), "raw");
        module.kind = MODULE_BLOCK;
        assert_eq!(module.kind_str(), "wasm");
    }

    #[test]
    fn usable_filter() {
        let mut region = MemoryRegion::EMPTY;
        assert!(!region.is_usable());
        region.start = 0x1000;
        region.end = 0x2000;
        assert!(!region.is_usable());
        region.kind = MEM_USABLE;
        assert!(region.is_usable());
    }

    #[test]
    fn framebuffer_presence() {
        let mut fb = FramebufferInfo::NONE;
        assert!(!fb.is_present());
        fb.address = 0xFD00_0000;
        fb.width = 1024;
        fb.height = 768;
        assert!(fb.is_present());
    }
}
