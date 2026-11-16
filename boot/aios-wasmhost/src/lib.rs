//! AIOS WebAssembly host for sandboxed blocks.
//!
//! Blocks are WebAssembly modules stored on the ESP (`\aios\blocks\*.wasm`).
//! `aios-boot` hands their bytes to the kernel as `MODULE_BLOCK` modules and the
//! kernel executes them through this crate: a self-contained parser, validator
//! and interpreter that runs in ring 0 without a JIT and without threads.
//!
//! The subset is small on purpose, because every feature here is a feature the
//! kernel has to trust:
//!
//! * types use `i32` only (no `i64`, `f32`, `f64`), with 8 parameters and at most
//!   one result per function;
//! * one linear memory of at most [`module::MAX_MEMORY_PAGES`] pages, always
//!   bounded by the buffer the caller provides;
//! * imports are limited to the [`interp`] host ABI (`aios.log`, `aios.exit`,
//!   `aios.now_ms`, `aios.yield`) and every one of them is gated by a
//!   capability right in [`cap::Rights`];
//! * execution is deterministic and bounded: every instruction spends a unit of
//!   [`cap::Token::fuel`], so a block cannot spin forever.
//!
//! The crate is `no_std` and only needs `alloc` for the module metadata, so the
//! kernel can use it as soon as its heap is up. It has no hardware dependency
//! and the whole runtime is unit-tested on the host.
//!
//! ```
//! use aios_wasmhost::{Host, Module, Rights, Token};
//!
//! struct Log;
//! impl aios_wasmhost::Env for Log {
//!     fn log(&mut self, _text: &str) {}
//!     fn now_ms(&mut self) -> i32 { 0 }
//! }
//!
//! # let bytes: &[u8] = b"\0asm\x01\0\0\0";
//! let module = Module::parse(bytes).unwrap();
//! let mut memory = [0u8; 1 << 16];
//! let mut host = Host::new(&mut memory, Log, Token::deny_all(0));
//! host.instantiate(&module).unwrap();
//! assert_eq!(host.pages(), 0);
//! ```

#![no_std]

extern crate alloc;

pub mod cap;
pub mod interp;
pub mod module;

#[cfg(test)]
mod test_asm;

mod reader;

pub use cap::{Rights, Token};
pub use interp::{Env, Host, Trap, DEFAULT_FUEL};
pub use module::{FuncType, HostFunc, Import, Module, ModuleError, MAX_MEMORY_PAGES, PAGE_SIZE};
