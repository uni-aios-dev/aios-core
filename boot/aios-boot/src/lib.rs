//! Pure logic shared by the AIOS UEFI loader: boot configuration parsing,
//! ELF loading and page-table construction.
//!
//! This crate is `no_std` and deliberately free of firmware dependencies, so
//! the parsing and layout rules can be unit-tested on the host:
//!
//! ```text
//! cargo test --manifest-path boot/aios-boot/Cargo.toml --lib
//! ```
//!
//! The UEFI-facing half lives in the `aios-boot` binary.

#![no_std]

extern crate alloc;

pub mod config;
pub mod elf;
pub mod paging;

/// Why the loader stopped before jumping to the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootError {
    /// The AIOS partition could not be opened or read.
    Partition,
    /// `/aios/boot.conf` is missing or malformed.
    Config,
    /// `/aios/kernel.elf` is missing, truncated or not a loadable ELF64 image.
    Kernel,
    /// The firmware memory map is unusable.
    MemoryMap,
    /// Page tables could not be built (out of frames or an invalid mapping).
    Paging,
    /// The firmware does not provide what the loader needs (GOP, FS, ...).
    Unsupported,
}

impl BootError {
    /// Human-readable description for the firmware console and serial log.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Partition => "cannot open the AIOS partition",
            Self::Config => "cannot read /aios/boot.conf",
            Self::Kernel => "cannot load /aios/kernel.elf",
            Self::MemoryMap => "cannot obtain the UEFI memory map",
            Self::Paging => "cannot build kernel page tables",
            Self::Unsupported => "unsupported firmware configuration",
        }
    }
}
