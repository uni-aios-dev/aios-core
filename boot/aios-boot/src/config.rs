//! Parser for `/aios/boot.conf` and drop-in discovery for `/aios/config` and
//! `/aios/blocks`.
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
//!
//! `boot.conf` only bootstraps the boot: every other `*.conf` file in
//! [`CONFIG_DIR`] and every `*.wasm` block in [`BLOCK_DIR`] is discovered by
//! [`BootConfig::discover`], so dropping a file onto the ESP is enough to add
//! it. Explicit entries always win over discovery (a file listed in
//! `boot.conf` is not loaded twice) and discovered files are added in
//! case-insensitive name order, which is also the order the kernel applies
//! them: later files override earlier ones.

use crate::BootError;
use aios_bootinfo::{MAX_MODULES, MODULE_BLOCK, MODULE_CONFIG, MODULE_RAW};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Directory whose `*.conf` files are discovered as configuration drop-ins.
pub const CONFIG_DIR: &str = "/aios/config";
/// Directory whose `*.wasm` files are discovered as blocks.
pub const BLOCK_DIR: &str = "/aios/blocks";
/// Extension of a configuration file.
pub const CONFIG_EXTENSION: &str = "conf";
/// Extension of a WebAssembly block.
pub const BLOCK_EXTENSION: &str = "wasm";

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

    /// Appends the files of `dir` that are not already listed in the
    /// configuration, in case-insensitive name order.
    ///
    /// `names` is the directory listing (bare file names, as returned by the
    /// firmware), `extension` filters the accepted files (`conf` or `wasm`) and
    /// `kind` is the [`aios_bootinfo::MODULE_CONFIG`],
    /// [`aios_bootinfo::MODULE_BLOCK`] or `MODULE_RAW` tag handed to the
    /// kernel. Returns how many modules were added.
    ///
    /// A name is skipped when it is empty, contains a path separator, starts
    /// with `.` or does not end in `extension`. A file already present in
    /// `boot.conf` is never added twice, regardless of case: the ESP is FAT32,
    /// where `System.CONF` and `system.conf` are the same file.
    pub fn discover<I, S>(
        &mut self,
        dir: &str,
        extension: &str,
        kind: u32,
        names: I,
    ) -> Result<usize, BootError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let dir = dir.trim_end_matches('/');
        let mut candidates: Vec<String> = names
            .into_iter()
            .filter_map(|name| {
                let name = name.as_ref().trim();
                (is_plain_file_name(name) && has_extension(name, extension))
                    .then(|| name.to_string())
            })
            .collect();
        candidates.sort_by_key(|name| name.to_ascii_lowercase());

        let mut added = 0;
        for name in candidates {
            let path = format!("{dir}/{name}");
            let module_name = relative_name(&path);
            if self
                .modules
                .iter()
                .any(|module| module.name.eq_ignore_ascii_case(&module_name))
            {
                continue;
            }
            if self.modules.len() >= MAX_MODULES {
                return Err(BootError::Config);
            }
            self.modules.push(Module {
                name: module_name,
                path,
                kind,
            });
            added += 1;
        }
        Ok(added)
    }
}

/// Whether `name` looks like a bare file name the loader may open.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\\')
}

/// Whether `name` ends in `.extension`, ASCII case-insensitively.
fn has_extension(name: &str, extension: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) => !stem.is_empty() && ext.eq_ignore_ascii_case(extension),
        None => false,
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
    use alloc::vec::Vec;

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

    fn boot_config() -> BootConfig {
        parse(
            "\
kernel = /aios/kernel.elf\n\
module = /aios/config/system.conf\n\
block = /aios/blocks/empty.wasm\n",
        )
        .unwrap()
    }

    #[test]
    fn discovers_drop_ins_in_name_order() {
        let mut config = boot_config();
        let added = config
            .discover(
                CONFIG_DIR,
                CONFIG_EXTENSION,
                MODULE_CONFIG,
                ["net.conf", "audio.conf"],
            )
            .unwrap();
        assert_eq!(added, 2);
        assert_eq!(config.modules.len(), 4);
        assert_eq!(config.modules[0].name, "config/system.conf");
        assert_eq!(config.modules[1].name, "blocks/empty.wasm");
        assert_eq!(config.modules[2].name, "config/audio.conf");
        assert_eq!(config.modules[3].name, "config/net.conf");
        assert_eq!(config.modules[2].path, "/aios/config/audio.conf");
        assert_eq!(config.modules[2].kind, MODULE_CONFIG);
        assert_eq!(config.modules[3].uefi_path(), "\\aios\\config\\net.conf");
    }

    #[test]
    fn discovery_never_duplicates_boot_conf_entries() {
        let mut config = boot_config();
        let added = config
            .discover(
                CONFIG_DIR,
                CONFIG_EXTENSION,
                MODULE_CONFIG,
                ["system.conf", "System.CONF"],
            )
            .unwrap();
        assert_eq!(added, 0);
        assert_eq!(config.modules.len(), 2);

        let added = config
            .discover(BLOCK_DIR, BLOCK_EXTENSION, MODULE_BLOCK, ["EMPTY.WASM"])
            .unwrap();
        assert_eq!(added, 0);
    }

    #[test]
    fn discovery_ignores_foreign_names() {
        let mut config = parse("kernel = /aios/kernel.elf\n").unwrap();
        let added = config
            .discover(
                CONFIG_DIR,
                CONFIG_EXTENSION,
                MODULE_CONFIG,
                [
                    "",
                    ".hidden.conf",
                    "nested/system.conf",
                    "nested\\system.conf",
                    "readme.txt",
                    "no-extension",
                    ".conf",
                ],
            )
            .unwrap();
        assert_eq!(added, 0);
        assert!(config.modules.is_empty());
    }

    #[test]
    fn discovery_normalises_a_trailing_slash() {
        let mut config = parse("kernel = /aios/kernel.elf\n").unwrap();
        config
            .discover(
                "/aios/config/",
                CONFIG_EXTENSION,
                MODULE_CONFIG,
                ["net.conf"],
            )
            .unwrap();
        assert_eq!(config.modules[0].path, "/aios/config/net.conf");
        assert_eq!(config.modules[0].name, "config/net.conf");
    }

    #[test]
    fn discovery_stops_at_the_module_limit() {
        let mut config = parse("kernel = /aios/kernel.elf\n").unwrap();
        let names: Vec<String> = (0..MAX_MODULES + 1)
            .map(|i| alloc::format!("f{i}.conf"))
            .collect();
        assert_eq!(
            config.discover(CONFIG_DIR, CONFIG_EXTENSION, MODULE_CONFIG, names),
            Err(BootError::Config)
        );
        assert_eq!(config.modules.len(), MAX_MODULES);
    }
}
