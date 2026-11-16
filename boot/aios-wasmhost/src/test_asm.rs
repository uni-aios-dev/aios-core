//! Builders for the WebAssembly binaries used by the unit tests.
//!
//! Hand-writing a module in hex is unreadable, so the tests assemble one from
//! these helpers; every helper is the smallest encoder for one binary-format
//! production.

use alloc::vec::Vec;

/// Unsigned LEB128.
pub fn leb32(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return out;
        }
    }
}

/// Signed LEB128.
pub fn sleb32(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        let sign = byte & 0x40 != 0;
        let done = (value == 0 && !sign) || (value == -1 && sign);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            return out;
        }
    }
}

/// An `i32.const` instruction.
pub fn i32_const(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x41);
    out.extend(sleb32(value));
    out
}

/// A vector: count followed by the encoded items.
fn vector(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = leb32(items.len() as u32);
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

/// A length-prefixed UTF-8 name.
pub fn name(text: &str) -> Vec<u8> {
    let mut out = leb32(text.len() as u32);
    out.extend_from_slice(text.as_bytes());
    out
}

/// One section.
pub fn section(id: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(id);
    out.extend(leb32(payload.len() as u32));
    out.extend_from_slice(payload);
    out
}

/// The header plus `sections`, in the order given.
pub fn module(sections: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    for section in sections {
        out.extend_from_slice(section);
    }
    out
}

/// Type section: `(params, results)` pairs of `i32` counts.
pub fn types(entries: &[(u8, u8)]) -> Vec<u8> {
    let mut items = Vec::new();
    for (params, results) in entries {
        let mut item = Vec::new();
        item.push(0x60);
        item.extend(leb32(u32::from(*params)));
        item.resize(item.len() + usize::from(*params), 0x7F);
        item.extend(leb32(u32::from(*results)));
        item.resize(item.len() + usize::from(*results), 0x7F);
        items.push(item);
    }
    section(1, &vector(&items))
}

/// One function import.
pub fn import_func(module_name: &str, field: &str, type_index: u32) -> Vec<u8> {
    let mut out = name(module_name);
    out.extend(name(field));
    out.push(0x00);
    out.extend(leb32(type_index));
    out
}

/// Import section.
pub fn imports(items: &[Vec<u8>]) -> Vec<u8> {
    section(2, &vector(items))
}

/// Function section: one type index per defined function.
pub fn functions(type_indices: &[u32]) -> Vec<u8> {
    let items: Vec<Vec<u8>> = type_indices.iter().map(|index| leb32(*index)).collect();
    section(3, &vector(&items))
}

/// Memory section with `min` pages and no maximum.
pub fn memory(min: u32) -> Vec<u8> {
    let mut payload = leb32(1);
    payload.push(0x00);
    payload.extend(leb32(min));
    section(5, &payload)
}

/// One function export.
pub fn export_func(export_name: &str, func: u32) -> Vec<u8> {
    let mut out = name(export_name);
    out.push(0x00);
    out.extend(leb32(func));
    out
}

/// Export section.
pub fn exports(items: &[Vec<u8>]) -> Vec<u8> {
    section(7, &vector(items))
}

/// Start section.
pub fn start(func: u32) -> Vec<u8> {
    section(8, &leb32(func))
}

/// One code entry: the locals declaration followed by the body.
pub fn body(locals: &[(u32, u8)], code: &[u8]) -> Vec<u8> {
    let mut payload = leb32(locals.len() as u32);
    for (count, value_type) in locals {
        payload.extend(leb32(*count));
        payload.push(*value_type);
    }
    payload.extend_from_slice(code);
    let mut out = leb32(payload.len() as u32);
    out.extend(payload);
    out
}

/// Code section.
pub fn code(bodies: &[Vec<u8>]) -> Vec<u8> {
    section(10, &vector(bodies))
}

/// An active data segment at a constant offset.
pub fn data_active(offset: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = leb32(0);
    out.push(0x41);
    out.extend(sleb32(offset as i32));
    out.push(0x0B);
    out.extend(leb32(bytes.len() as u32));
    out.extend_from_slice(bytes);
    out
}

/// Data section.
pub fn data(segments: &[Vec<u8>]) -> Vec<u8> {
    section(11, &vector(segments))
}
