//! Parser for `/aios/boot.conf`.
//!
//! The file is a flat list of `key = value` lines. `#` starts a comment.
//! Recognised keys:
//!
//! ```text
//! kernel = /aios/kernel.elf
//! module = /aios/config/system.conf
//! block  = /aios/blocks/empty.wasm
//! raw    = /aios/fonts/default.psf
//! ```
//!
//! `module`, `block` and `raw` may repeat. Paths use the POSIX form (`/`) in
//! the file and are converted to the UEFI FAT form (`\`) by [`Module::uefi_path`].

use crate::BootError;
use aios_bootinfo::{MAX_MODULES, MODULE_BLOCK, MODULE_CONFIG, MODULE_RAW};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// One file the loader must place in memory for the kernel.
#[derive(Debug, Clone)]
pub struct Module {
    /// Name relative to the AIOS root, e.g. `config/system.conf`.
    pub name: String,
    /// Path inside the AIOS partition, e.g. `/aios/config/system.conf`.
    pub path: String,
    /// [`MODULE_CONFIG`], [`MODULE_BLOCK`] or [`MODULE_RAW`].
    pub kind: u32,
}

impl Module {
    /// The path with UEFI `\` separators, ready for `FileSystem`.
    pub fn uefi_path(&self) -> String {
        self.path.replace('/', "\\")
    }
}

/// The parsed boot configuration.
#[derive(Debug, Clone)]
pub struct BootConfig {
    /// Kernel ELF path, POSIX form.
    pub kernel: String,
    /// Configuration files and blocks, in file order.
    pub modules: Vec<Module>,
}

impl BootConfig {
    /// The kernel path with UEFI `\` separators.
    pub fn kernel_uefi_path(&self) -> String {
        self.kernel.replace('/', "\\")
    }
}

/// Parses a `boot.conf` image.
pub fn parse(text: &str) -> Result<BootConfig, BootError> {
    let mut kernel: Option<String> = None;
    let mut modules = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(BootError::Config)?;
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            return Err(BootError::Config);
        }

        match key {
            "kernel" => kernel = Some(value.to_string()),
            "module" | "block" | "raw" => {
                if modules.len() >= MAX_MODULES {
                    return Err(BootError::Config);
                }
                let kind = match key {
                    "block" => MODULE_BLOCK,
                    "raw" => MODULE_RAW,
                    _ => MODULE_CONFIG,
                };
                modules.push(Module {
                    name: relative_name(value),
                    path: value.to_string(),
                    kind,
                });
            }
            // Unknown keys are ignored so the file can grow without breaking
            // older loaders.
            _ => {}
        }
    }

    let kernel = kernel.ok_or(BootError::Config)?;
    Ok(BootConfig { kernel, modules })
}

/// Strips a leading `/` and the `aios/` prefix from a boot path.
fn relative_name(path: &str) -> String {
    let relative = path.trim_start_matches('/');
    relative
        .strip_prefix("aios/")
        .unwrap_or(relative)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_and_modules() {
        let config = parse(
            "\
# AIOS\n\
kernel = /aios/kernel.elf\n\
module = /aios/config/system.conf\n\
block = /aios/blocks/empty.wasm\n\
raw = /aios/fonts/default.psf\n",
        )
        .unwrap();
        assert_eq!(config.kernel, "/aios/kernel.elf");
        assert_eq!(config.modules.len(), 3);
        assert_eq!(config.modules[0].name, "config/system.conf");
        assert_eq!(config.modules[0].kind, MODULE_CONFIG);
        assert_eq!(config.modules[1].kind, MODULE_BLOCK);
        assert_eq!(config.modules[2].kind, MODULE_RAW);
        assert_eq!(config.modules[0].uefi_path(), "\\aios\\config\\system.conf");
    }

    #[test]
    fn requires_a_kernel() {
        assert!(parse("module = /aios/config/system.conf\n").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn ignores_comments_and_unknown_keys() {
        let config = parse(
            "\
# comment\n\
timeout = 3\n\
kernel=/aios/kernel.elf # inline comment\n",
        )
        .unwrap();
        assert_eq!(config.kernel, "/aios/kernel.elf");
        assert!(config.modules.is_empty());
    }

    #[test]
    fn name_without_aios_prefix() {
        assert_eq!(relative_name("/config/net.conf"), "config/net.conf");
        assert_eq!(relative_name("aios/blocks/a.wasm"), "blocks/a.wasm");
    }
}
