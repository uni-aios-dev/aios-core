//! `aios-autohal` — Hardware Auto-Provisioning & Driver Store.
//!
//! Automatically detects attached hardware by fingerprint, locates/adapts
//! open-source drivers into isolated WASM modules, launches them in the
//! Wasmtime sandbox with capability tokens, and caches them locally with a
//! 100% TUI/GUI parity in the Hardware Inspector.
//!
//! Pipeline (see [`engine::AutohalEngine`]): detection -> local store lookup
//! -> network fetch/adaptation -> SHA-256 validation + capability grant +
//! instantiation -> cache & register. After 3 consecutive failures a driver
//! is automatically rolled back to the Generic Fallback Driver.

pub mod adapter;
pub mod catalog;
pub mod engine;
pub mod fetcher;
pub mod fingerprint;
pub mod manifest;
pub mod registry;
#[cfg(feature = "gui")]
pub mod ui_gui;
#[cfg(feature = "tui")]
pub mod ui_tui;

pub use crate::adapter::{AdapterConfig, AdapterError, DriverLanguage, SourceAdapter};
pub use crate::catalog::{BuiltinDriver, GENERIC_WAT};
pub use crate::engine::{
    AutohalEngine, DeviceDriver, DeviceView, DriverState, EngineConfig, EngineError, Toast,
    ToastKind,
};
pub use crate::fetcher::{
    DriverCatalogEntry, DriverCatalogIndex, DriverFetcher, FetchError, FetchedDriver,
    FetcherConfig, Transport,
};
pub use crate::fingerprint::{extract_fingerprints, BusType, HardwareFingerprint};
pub use crate::manifest::{cap_from_name, DriverManifest, DriverSource, SupportedHardware};
pub use crate::registry::{DriverIndex, DriverStore};
