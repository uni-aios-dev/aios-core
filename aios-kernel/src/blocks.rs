//! Sandboxed WebAssembly blocks (`/aios/blocks/*.wasm`).
//!
//! `aios-boot` hands every block to the kernel as a `MODULE_BLOCK` module.
//! [`init`] runs each one through [`aios_wasmhost`]: the binary is parsed and
//! validated in place (no copy), the capability token comes from
//! `blocks.<name>.rights`, the fuel budget from `blocks.<name>.fuel`, and the
//! module's start function plus its `run`/`_start` export execute on a private
//! linear memory allocated from the kernel heap.
//!
//! Blocks run before the scheduler starts, so nothing can preempt a block: the
//! fuel budget is the only time limit. A block that traps or runs out of fuel
//! is reported and skipped instead of stopping the kernel.

use crate::boot;
use crate::config;
use crate::interrupts;
use aios_bootinfo::{ModuleInfo, MODULE_BLOCK};
use aios_wasmhost::{Env, Host, Module, Rights, Token, Trap, DEFAULT_FUEL, PAGE_SIZE};
use alloc::format;
use alloc::vec;
use core::sync::atomic::Ordering;

/// What [`init`] did with the block store.
#[derive(Debug, Default, Clone, Copy)]
pub struct Summary {
    /// `MODULE_BLOCK` modules handed over by the loader.
    pub found: usize,
    /// Blocks that ran to completion (or called `aios.exit`).
    pub ran: usize,
    /// Blocks skipped before execution: bad name, config, rights or image.
    pub skipped: usize,
    /// Blocks that parsed but trapped while running.
    pub failed: usize,
    /// Instructions spent across every block that ran.
    pub fuel: u64,
}

/// Why one block did not run to completion.
enum Outcome {
    /// The block was rejected before execution.
    Skipped,
    /// The block started and trapped.
    Failed,
}

/// The kernel side of the host ABI: the serial log and the PIT clock.
struct SerialEnv {
    name: &'static str,
}

impl Env for SerialEnv {
    fn log(&mut self, text: &str) {
        crate::kprintln!("[serial] [block] {}: {}", self.name, text);
    }

    fn now_ms(&mut self) -> i32 {
        let ticks = interrupts::TICKS.load(Ordering::Relaxed);
        ((ticks * 1000) / interrupts::TIMER_HZ) as i32
    }
}

/// Parses, validates and runs every block the loader handed over.
///
/// Call once, after [`crate::heap::init_heap`] and [`crate::config::init`],
/// while the kernel is still single-threaded.
pub fn init() -> Summary {
    let mut summary = Summary::default();
    for (index, module) in boot::modules().iter().enumerate() {
        if module.kind != MODULE_BLOCK {
            continue;
        }
        summary.found += 1;
        match run_block(module, index as u32) {
            Ok(fuel) => {
                summary.ran += 1;
                summary.fuel += u64::from(fuel);
            }
            Err(Outcome::Skipped) => summary.skipped += 1,
            Err(Outcome::Failed) => summary.failed += 1,
        }
    }
    if summary.found == 0 {
        crate::kprintln!("[serial] [block] no blocks; the store is empty");
    } else {
        crate::kprintln!(
            "[serial] [block] {} block(s): {} ran, {} skipped, {} failed, {} instruction(s)",
            summary.found,
            summary.ran,
            summary.skipped,
            summary.failed,
            summary.fuel
        );
    }
    summary
}

/// Runs one `MODULE_BLOCK` and returns the fuel it spent.
fn run_block(module: &'static ModuleInfo, index: u32) -> Result<u32, Outcome> {
    let name = module.name_str().unwrap_or("<invalid>");
    let Some(stem) = block_stem(name) else {
        crate::kprintln!(
            "[serial] [block] '{}' skipped: a block must be named '<name>.wasm'",
            name
        );
        return Err(Outcome::Skipped);
    };
    let Some(bytes) = block_bytes(module) else {
        crate::kprintln!(
            "[serial] [block] {} skipped: no image (phys=0x{:x}, size={})",
            name,
            module.phys,
            module.size
        );
        return Err(Outcome::Skipped);
    };
    let parsed = match Module::parse(bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            crate::kprintln!("[serial] [block] {} skipped: {}", name, error);
            return Err(Outcome::Skipped);
        }
    };

    let rights_key = format!("blocks.{stem}.rights");
    let rights = match config::get(&rights_key) {
        Some(text) => match Rights::from_names(text) {
            Some(rights) => rights,
            None => {
                crate::kprintln!(
                    "[serial] [block] {} skipped: '{}' names an unknown right",
                    name,
                    rights_key
                );
                return Err(Outcome::Skipped);
            }
        },
        None => {
            crate::kprintln!(
                "[serial] [block] {} runs with no rights: '{}' is not configured",
                name,
                rights_key
            );
            Rights::NONE
        }
    };
    let fuel_key = format!("blocks.{stem}.fuel");
    let fuel = config::get_u32(&fuel_key).unwrap_or(DEFAULT_FUEL);
    let token = Token::new(index, rights, fuel);

    let needed = parsed.required_rights();
    if !rights.contains(needed) {
        crate::kprintln!(
            "[serial] [block] {} skipped: needs '{}' but the token grants '{}'",
            name,
            needed,
            rights
        );
        return Err(Outcome::Skipped);
    }

    let bytes_needed = parsed.memory_pages() as usize * PAGE_SIZE as usize;
    let mut memory = vec![0u8; bytes_needed];
    let mut host = Host::new(&mut memory, SerialEnv { name }, token);
    if let Err(error) = host.instantiate(&parsed) {
        crate::kprintln!("[serial] [block] {} stopped in start(): {}", name, error);
        return Err(Outcome::Failed);
    }

    let entry = ["run", "_start"]
        .into_iter()
        .find(|entry| parsed.export(entry).is_some());
    let spent = match entry {
        Some(entry) => match host.call(&parsed, entry, &[]) {
            Ok(result) => {
                match result {
                    Some(value) => crate::kprintln!(
                        "[serial] [block] {} ran: entry '{}' -> {} ({} page(s))",
                        name,
                        entry,
                        value,
                        host.pages()
                    ),
                    None => crate::kprintln!(
                        "[serial] [block] {} ran: entry '{}' ({} page(s))",
                        name,
                        entry,
                        host.pages()
                    ),
                }
                host.token().fuel() - host.fuel()
            }
            Err(Trap::Exited(code)) => {
                crate::kprintln!(
                    "[serial] [block] {} ran: entry '{}' exited with code {}",
                    name,
                    entry,
                    code
                );
                host.token().fuel() - host.fuel()
            }
            Err(error) => {
                crate::kprintln!(
                    "[serial] [block] {} trapped in '{}': {}",
                    name,
                    entry,
                    error
                );
                return Err(Outcome::Failed);
            }
        },
        None => {
            crate::kprintln!(
                "[serial] [block] {} ran: start() only ({} page(s))",
                name,
                host.pages()
            );
            host.token().fuel() - host.fuel()
        }
    };
    crate::kprintln!(
        "[serial] [block] {} used {} of {} instruction(s)",
        name,
        spent,
        fuel
    );
    Ok(spent)
}

/// The bytes of a block image through the direct map.
fn block_bytes(module: &'static ModuleInfo) -> Option<&'static [u8]> {
    if module.phys == 0 || module.size == 0 {
        return None;
    }
    let len = usize::try_from(module.size).ok()?;
    let base = boot::hhdm_offset().checked_add(module.phys)?;
    // SAFETY: `aios-boot` copied exactly `size` bytes to `phys`, and the direct
    // map covers every physical page the loader allocated.
    Some(unsafe { core::slice::from_raw_parts(base as *const u8, len) })
}

/// The `<name>` in a `blocks/<name>.wasm` module name.
fn block_stem(name: &str) -> Option<&str> {
    let file = name.rsplit('/').next()?;
    if file.len() <= ".wasm".len()
        || !file[file.len() - ".wasm".len()..].eq_ignore_ascii_case(".wasm")
    {
        return None;
    }
    let stem = &file[..file.len() - ".wasm".len()];
    (!stem.is_empty()).then_some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_directory_and_extension() {
        assert_eq!(block_stem("blocks/hello.wasm"), Some("hello"));
        assert_eq!(block_stem("blocks/HELLO.WASM"), Some("HELLO"));
        assert_eq!(block_stem("hello.wasm"), Some("hello"));
        assert_eq!(block_stem("blocks/.wasm"), None);
        assert_eq!(block_stem("blocks/hello"), None);
        assert_eq!(block_stem("blocks/hello.wat"), None);
    }

    #[test]
    fn an_empty_image_is_skipped() {
        let module = ModuleInfo::EMPTY;
        assert!(block_bytes(&module).is_none());
    }
}
