//! Runtime system configuration (`/aios/config/*.conf`).
//!
//! `aios-boot` hands every configuration file to the kernel as a
//! `MODULE_CONFIG` module: the files listed in `boot.conf` first, then the
//! drop-ins discovered in `\aios\config` in case-insensitive name order. This
//! module reads them straight out of the loader's memory through the direct
//! map, using the same [`aios_config`] parser as the loader.
//!
//! Values are resolved last-to-first, so a later layer overrides an earlier
//! one: `config/system.conf` sets the baseline and a drop-in such as
//! `config/net.conf` can override any key without editing the base file.
//!
//! The kernel keeps no built-in copy of the defaults: when a key is absent
//! [`get`] returns `None` and the caller uses its own compile-time constant,
//! which is also logged at boot so a mismatch is visible in the serial log.

use crate::boot;
use aios_bootinfo::{ModuleInfo, MODULE_CONFIG};
use aios_config::{Config, Entry};

/// What [`init`] found in the configuration modules.
#[derive(Debug, Default, Clone, Copy)]
pub struct Summary {
    /// Number of `MODULE_CONFIG` modules handed over by the loader.
    pub layers: usize,
    /// Number of `key = value` pairs across every layer.
    pub entries: usize,
    /// Layers dropped because their bytes are not valid UTF-8.
    pub invalid: usize,
}

/// Validates every configuration layer and logs it to the serial console.
///
/// Call once, after [`crate::boot::init`] and before reading any key.
pub fn init() -> Summary {
    let mut summary = Summary::default();
    for module in boot::modules() {
        if module.kind != MODULE_CONFIG {
            continue;
        }
        summary.layers += 1;
        let name = module.name_str().unwrap_or("<invalid>");
        match module_text(module) {
            None => {
                summary.invalid += 1;
                crate::kprintln!(
                    "[serial] [cfg] layer '{}' skipped: {} bytes are not UTF-8",
                    name,
                    module.size
                );
            }
            Some(text) => {
                let config = Config::new(text);
                let entries = config.count();
                summary.entries += entries;
                crate::kprintln!(
                    "[serial] [cfg] layer '{}' = {} key(s), {} byte(s)",
                    name,
                    entries,
                    module.size
                );
                for key in config.duplicates() {
                    crate::kprintln!(
                        "[serial] [cfg] warning: '{}' is set twice in '{}', the last one wins",
                        key,
                        name
                    );
                }
            }
        }
    }
    if summary.layers == 0 {
        crate::kprintln!(
            "[serial] [cfg] no configuration modules; every value falls back to the kernel default"
        );
    }
    summary
}

/// The `(name, text)` of every valid configuration module, earliest first.
pub fn layers() -> impl Iterator<Item = (&'static str, &'static str)> {
    boot::modules()
        .iter()
        .filter(|module| module.kind == MODULE_CONFIG)
        .filter_map(|module| Some((module.name_str()?, module_text(module)?)))
}

/// Every accepted entry paired with the layer that provides it, earliest
/// layer first. The last entry for a key is the effective one.
pub fn entries() -> impl Iterator<Item = (&'static str, Entry<'static>)> {
    layers().flat_map(|(name, text)| Config::new(text).entries().map(move |entry| (name, entry)))
}

/// The effective value of `key` and the layer it came from.
///
/// Layers are scanned last-to-first so a drop-in overrides the base file.
pub fn source(key: &str) -> Option<(&'static str, &'static str)> {
    for module in boot::modules().iter().rev() {
        if module.kind != MODULE_CONFIG {
            continue;
        }
        let Some(text) = module_text(module) else {
            continue;
        };
        if let Some(value) = Config::new(text).get(key) {
            return Some((module.name_str().unwrap_or("<invalid>"), value));
        }
    }
    None
}

/// The effective value of `key`, or `None` when no layer sets it.
pub fn get(key: &str) -> Option<&'static str> {
    source(key).map(|(_, value)| value)
}

/// The effective value of `key` parsed as an unsigned 32-bit integer.
pub fn get_u32(key: &str) -> Option<u32> {
    aios_config::parse_u32(get(key)?)
}

/// The effective value of `key` parsed as a boolean.
pub fn get_bool(key: &str) -> Option<bool> {
    aios_config::parse_bool(get(key)?)
}

/// The text of one configuration module, if it is valid UTF-8.
///
/// The loader copied the file to a physical buffer that the direct map covers,
/// so the bytes stay valid for the lifetime of the kernel.
fn module_text(module: &'static ModuleInfo) -> Option<&'static str> {
    if module.phys == 0 || module.size == 0 {
        return None;
    }
    let len = usize::try_from(module.size).ok()?;
    let base = boot::hhdm_offset().checked_add(module.phys)?;
    // SAFETY: `aios-boot` copied exactly `size` bytes to `phys`, and the direct
    // map covers every physical page the loader allocated.
    let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, len) };
    core::str::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_layer_data_is_ignored() {
        let mut module = ModuleInfo::EMPTY;
        module.kind = MODULE_CONFIG;
        assert!(module_text(&module).is_none());
    }

    #[test]
    fn summary_defaults_to_empty() {
        let summary = Summary::default();
        assert_eq!(summary.layers, 0);
        assert_eq!(summary.entries, 0);
        assert_eq!(summary.invalid, 0);
    }
}
