#![cfg_attr(not(feature = "std"), no_std)]

//! Text-only console core shared by the AIOS host and the bare-metal kernel.
//!
//! This crate defines one command language, one parser and one output contract.
//! It contains no terminal, framebuffer, GUI or networking code: concrete
//! `CommandBackend` implementations live in the host and kernel crates.

extern crate alloc;

pub mod backend;
pub mod capability;
pub mod deferred;
#[cfg(feature = "std")]
pub mod host;
pub mod output;
pub mod parser;
pub mod registry;
pub mod spec;
