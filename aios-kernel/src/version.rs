//! Single source of truth for the release version displayed by the kernel.
//!
//! Every user-visible surface — the boot banner, the TUI status bar and About
//! tab, GUI window titles, shell `ver`, panic reports and serial diagnostics —
//! reads one of these constants. The values come from `CARGO_PKG_VERSION`, so
//! changing `aios-kernel/Cargo.toml` updates every surface at once.

/// Bare semantic version, e.g. `2.84.0`.
pub const SEMVER: &str = env!("CARGO_PKG_VERSION");

/// Product-prefixed version, e.g. `AIOS kernel v<semver>`.
pub const KERNEL: &str = concat!("AIOS kernel v", env!("CARGO_PKG_VERSION"));
