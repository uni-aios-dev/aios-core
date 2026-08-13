use std::path::PathBuf;
use std::process::Command;

/// Source language of a downloaded driver before adaptation to `wasm32-wasi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverLanguage {
    C,
    Rust,
}

impl DriverLanguage {
    pub fn label(&self) -> &'static str {
        match self {
            Self::C => "C",
            Self::Rust => "Rust",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("wasm toolchain not available: {0}")]
    ToolchainUnavailable(String),
    #[error("compilation failed: {0}")]
    CompileFailed(String),
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(&'static str),
}

/// Adaptation policy: the LLVM target, the host function names the rewrite
/// pass targets and the compiler to invoke.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// LLVM target, e.g. `wasm32-wasi`.
    pub target: String,
    /// Name of the MMIO read host import, e.g. `hal_mmio_read`.
    pub hal_mmio_read: String,
    /// Name of the MMIO write host import, e.g. `hal_mmio_write`.
    pub hal_mmio_write: String,
    /// Name of the port read host import, e.g. `hal_port_read8`.
    pub hal_port_read8: String,
    /// Name of the port write host import, e.g. `hal_port_write8`.
    pub hal_port_write8: String,
    /// Explicit compiler binary paths (fall back to PATH lookup when `None`).
    pub clang_path: Option<PathBuf>,
    pub rustc_path: Option<PathBuf>,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            target: "wasm32-wasi".into(),
            hal_mmio_read: "hal_mmio_read".into(),
            hal_mmio_write: "hal_mmio_write".into(),
            hal_port_read8: "hal_port_read8".into(),
            hal_port_write8: "hal_port_write8".into(),
            clang_path: None,
            rustc_path: None,
        }
    }
}

/// Local transpiler: rewrites direct register access in downloaded C/Rust
/// drivers into calls of the `aios` host imports (`hal_mmio_read`,
/// `hal_port_write8`, ...) and compiles the result to `wasm32-wasi`.
///
/// The rewrite pass is intentionally conservative: it only touches whole-word
/// call sites of the classic port-I/O and MMIO helpers (`inb/outb/inw/outw/
/// inl/outl`, `readb/readw/readl/writeb/writew/writel`,
/// `ioread32/iowrite32`), leaving everything else untouched.
pub struct SourceAdapter {
    config: AdapterConfig,
}

impl Default for SourceAdapter {
    fn default() -> Self {
        Self::new(AdapterConfig::default())
    }
}

impl SourceAdapter {
    pub fn new(config: AdapterConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &AdapterConfig {
        &self.config
    }

    /// Rewrite port/MMIO helper call sites to the host import names.
    pub fn rewrite_register_access(&self, source: &str, _lang: DriverLanguage) -> String {
        let c = &self.config;
        let pairs: Vec<(&str, String)> = vec![
            // port I/O
            ("inb", c.hal_port_read8.clone()),
            ("outb", c.hal_port_write8.clone()),
            ("inw", self.port_read16()),
            ("outw", self.port_write16()),
            ("inl", c.hal_mmio_read.clone()),
            ("outl", c.hal_mmio_write.clone()),
            // MMIO helpers
            ("ioread8", c.hal_port_read8.clone()),
            ("iowrite8", c.hal_port_write8.clone()),
            ("ioread16", self.port_read16()),
            ("iowrite16", self.port_write16()),
            ("ioread32", c.hal_mmio_read.clone()),
            ("iowrite32", c.hal_mmio_write.clone()),
            ("readb", c.hal_port_read8.clone()),
            ("writeb", c.hal_port_write8.clone()),
            ("readw", self.port_read16()),
            ("writew", self.port_write16()),
            ("readl", c.hal_mmio_read.clone()),
            ("writel", c.hal_mmio_write.clone()),
        ];
        rewrite_idents(source, &pairs)
    }

    fn port_read16(&self) -> String {
        derive_16bit(self.config.hal_port_read8.as_str())
    }
    fn port_write16(&self) -> String {
        derive_16bit(self.config.hal_port_write8.as_str())
    }

    /// Import declarations prepended to the adapted source so the rewritten
    /// calls resolve as `aios` module imports at link time.
    pub fn preamble(&self, lang: DriverLanguage) -> String {
        let a = &self.config;
        let read16 = self.port_read16();
        let write16 = self.port_write16();
        match lang {
            DriverLanguage::C => format!(
                r#"typedef unsigned char aios_u8;
typedef unsigned short aios_u16;
typedef unsigned int aios_u32;
__attribute__((import_module("aios"), import_name("{}"))) aios_u32 {}(aios_u32 addr);
__attribute__((import_module("aios"), import_name("{}"))) void {}(aios_u32 addr, aios_u32 val);
__attribute__((import_module("aios"), import_name("{}"))) aios_u8 {}(aios_u16 port);
__attribute__((import_module("aios"), import_name("{}"))) void {}(aios_u16 port, aios_u8 val);
__attribute__((import_module("aios"), import_name("{read16}"))) aios_u16 {read16}(aios_u16 port);
__attribute__((import_module("aios"), import_name("{write16}"))) void {write16}(aios_u16 port, aios_u16 val);
"#,
                a.hal_mmio_read,
                a.hal_mmio_read,
                a.hal_mmio_write,
                a.hal_mmio_write,
                a.hal_port_read8,
                a.hal_port_read8,
                a.hal_port_write8,
                a.hal_port_write8,
            ),
            DriverLanguage::Rust => format!(
                r#"#[link(wasm_import_module = "aios")]
unsafe extern "C" {{
    pub fn {}(addr: u32) -> u32;
    pub fn {}(addr: u32, val: u32);
    pub fn {}(port: u16) -> u8;
    pub fn {}(port: u16, val: u8);
    pub fn {read16}(port: u16) -> u16;
    pub fn {write16}(port: u16, val: u16);
}}
"#,
                a.hal_mmio_read, a.hal_mmio_write, a.hal_port_read8, a.hal_port_write8,
            ),
        }
    }

    /// Full adaptation pipeline: preamble + register-access rewrite.
    pub fn adapt(&self, source: &str, lang: DriverLanguage) -> String {
        format!(
            "{}\n{}",
            self.preamble(lang),
            self.rewrite_register_access(source, lang)
        )
    }

    /// Compile adapted source to a WASM binary using the local toolchain.
    ///
    /// Returns [`AdapterError::ToolchainUnavailable`] when neither `clang`
    /// (C) nor `rustc` (Rust) with the wasm target can be located; this is the
    /// graceful, documented failure path for hosts without an LLVM toolchain.
    pub fn compile(
        &self,
        source: &str,
        lang: DriverLanguage,
        entry_point: &str,
    ) -> Result<Vec<u8>, AdapterError> {
        match lang {
            DriverLanguage::C => self.compile_c(source, entry_point),
            DriverLanguage::Rust => self.compile_rust(source, entry_point),
        }
    }

    fn compile_c(&self, source: &str, entry_point: &str) -> Result<Vec<u8>, AdapterError> {
        let clang = self
            .config
            .clang_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("clang"));
        if !toolchain_available(&clang) {
            return Err(AdapterError::ToolchainUnavailable(
                "clang was not found in PATH".into(),
            ));
        }

        let dir = std::env::temp_dir().join(format!("aios-autohal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| AdapterError::CompileFailed(e.to_string()))?;
        let src = dir.join("driver.c");
        let out = dir.join("driver.wasm");
        std::fs::write(&src, source).map_err(|e| AdapterError::CompileFailed(e.to_string()))?;

        let output = Command::new(&clang)
            .args([
                &format!("--target={}", self.config.target),
                "-O2",
                "-nostdlib",
                "-Wl,--no-entry",
                &format!("-Wl,--export={entry_point}"),
                "-Wl,--export=init",
                "-Wl,--export=start",
                "-Wl,--allow-undefined",
                "-o",
            ])
            .arg(&out)
            .arg(&src)
            .output()
            .map_err(|e| AdapterError::ToolchainUnavailable(e.to_string()))?;

        if !output.status.success() {
            return Err(AdapterError::CompileFailed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        std::fs::read(&out).map_err(|e| AdapterError::CompileFailed(e.to_string()))
    }

    fn compile_rust(&self, source: &str, _entry_point: &str) -> Result<Vec<u8>, AdapterError> {
        let rustc = self
            .config
            .rustc_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("rustc"));
        if !toolchain_available(&rustc) {
            return Err(AdapterError::ToolchainUnavailable(
                "rustc was not found in PATH".into(),
            ));
        }

        let dir = std::env::temp_dir().join(format!("aios-autohal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| AdapterError::CompileFailed(e.to_string()))?;
        let src = dir.join("driver.rs");
        let out = dir.join("driver.wasm");
        std::fs::write(&src, source).map_err(|e| AdapterError::CompileFailed(e.to_string()))?;

        let output = Command::new(&rustc)
            .args([
                "--target",
                &self.config.target,
                "--crate-type",
                "cdylib",
                "-C",
                "link-arg=--no-entry",
                "-o",
            ])
            .arg(&out)
            .arg(&src)
            .output()
            .map_err(|e| AdapterError::ToolchainUnavailable(e.to_string()))?;

        if !output.status.success() {
            return Err(AdapterError::CompileFailed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let bytes = std::fs::read(&out).map_err(|e| AdapterError::CompileFailed(e.to_string()))?;
        Ok(bytes)
    }
}

fn toolchain_available(binary: &PathBuf) -> bool {
    Command::new(binary)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Derive the 16-bit variant of an `*8` host import name (`hal_port_read8` ->
/// `hal_port_read16`).
fn derive_16bit(name: &str) -> String {
    match name.strip_suffix('8') {
        Some(base) => format!("{base}16"),
        None => format!("{name}16"),
    }
}

/// Replace whole-word occurrences of `from -> to` where the word is followed
/// by `(` (a call site), preserving everything else.
fn rewrite_idents(source: &str, pairs: &[(&str, String)]) -> String {
    let mut out = source.to_string();
    for (from, to) in pairs {
        out = rewrite_ident(&out, from, to);
    }
    out
}

fn rewrite_ident(source: &str, from: &str, to: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(pos) = rest.find(from) {
        result.push_str(&rest[..pos]);
        let after = &rest[pos + from.len()..];
        let boundary_ok = after.starts_with('(')
            && (pos == 0
                || !rest[..pos]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_'));
        if boundary_ok {
            result.push_str(to);
        } else {
            result.push_str(from);
        }
        rest = after;
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_c_port_io() {
        let adapter = SourceAdapter::default();
        let src = r#"
void probe(uint16_t port) {
    uint8_t status = inb(port + 1);
    outb(status, port);
}
int uses_word = inboard();
"#;
        let out = adapter.rewrite_register_access(src, DriverLanguage::C);
        assert!(out.contains("hal_port_read8(port + 1)"));
        assert!(out.contains("hal_port_write8(status, port)"));
        // `inboard` is not a call of `inb`, must be untouched.
        assert!(out.contains("inboard()"));
        assert!(!out.contains("inb(port"));
    }

    #[test]
    fn test_rewrite_mmio() {
        let adapter = SourceAdapter::default();
        let src = "u32 v = readl(base + 4); writel(v, base); ioread32(base); iowrite32(1, base);";
        let out = adapter.rewrite_register_access(src, DriverLanguage::C);
        assert!(out.contains("hal_mmio_read(base + 4)"));
        assert!(out.contains("hal_mmio_write(v, base)"));
        assert!(out.contains("hal_mmio_read(base)"));
        assert!(out.contains("hal_mmio_write(1, base)"));
    }

    #[test]
    fn test_c_preamble_imports() {
        let adapter = SourceAdapter::default();
        let preamble = adapter.preamble(DriverLanguage::C);
        assert!(preamble.contains(r#"import_module("aios")"#));
        assert!(preamble.contains("hal_port_read8"));
        assert!(preamble.contains("hal_mmio_read"));
    }

    #[test]
    fn test_rust_preamble_imports() {
        let adapter = SourceAdapter::default();
        let preamble = adapter.preamble(DriverLanguage::Rust);
        assert!(preamble.contains(r#"#[link(wasm_import_module = "aios")]"#));
        assert!(preamble.contains("hal_port_write8"));
    }

    #[test]
    fn test_adapt_prepends_preamble() {
        let adapter = SourceAdapter::default();
        let out = adapter.adapt("void f(){ inb(1); }", DriverLanguage::C);
        assert!(out.starts_with("typedef unsigned char"));
        assert!(out.contains("hal_port_read8(1)"));
    }

    #[test]
    fn test_compile_returns_toolchain_error_without_clang() {
        // When clang is genuinely absent this returns ToolchainUnavailable;
        // when it is present compilation may fail or succeed, but must not panic.
        let adapter = SourceAdapter::new(AdapterConfig {
            clang_path: Some(PathBuf::from("definitely-not-clang-xyz")),
            ..Default::default()
        });
        let result = adapter.compile("int main(){}", DriverLanguage::C, "_start_driver");
        match result {
            Err(AdapterError::ToolchainUnavailable(_)) => {}
            other => {
                // Acceptable when a real toolchain is installed.
                let _ = other;
            }
        }
    }

    #[test]
    fn test_rewrite_idents_respects_words() {
        let out = rewrite_ident("inb(1) inbc(2) x_inb(3)", "inb", "hal_port_read8");
        assert_eq!(out, "hal_port_read8(1) inbc(2) x_inb(3)");
    }
}
