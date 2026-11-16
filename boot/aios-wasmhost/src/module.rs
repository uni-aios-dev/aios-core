//! WebAssembly module parser and static validator.
//!
//! [`Module::parse`] reads the binary format without copying the input: every
//! body, name and data segment borrows the caller's bytes, which is exactly how
//! the kernel keeps a `MODULE_BLOCK` straight from the loader's memory.
//!
//! Parsing also validates the module, so [`crate::Host`] can execute it without
//! re-checking structure: every opcode is supported, branch depths, local and
//! function indices are in range, `if` bodies are balanced and a memory
//! instruction has a memory to use. Value types are still checked at run time,
//! where an empty stack or a missing operand is a [`crate::Trap::Underflow`]
//! instead of undefined behaviour.
//!
//! Unsupported features are rejected with a typed error rather than skipped:
//! a block that uses `f64` or threads must not run with quietly different
//! semantics.

use crate::cap::Rights;
use crate::reader::Reader;
use alloc::vec::Vec;
use core::fmt;

/// Size of one WebAssembly memory page.
pub const PAGE_SIZE: u32 = 65_536;
/// Largest linear memory a block may declare (1 MiB).
pub const MAX_MEMORY_PAGES: u32 = 16;
/// Largest number of function types in one module.
pub const MAX_TYPES: usize = 32;
/// Largest number of functions (imported plus defined) in one module.
pub const MAX_FUNCTIONS: usize = 64;
/// Largest number of host imports in one module.
pub const MAX_IMPORTS: usize = 16;
/// Largest number of exports in one module.
pub const MAX_EXPORTS: usize = 16;
/// Largest number of data segments in one module.
pub const MAX_DATA_SEGMENTS: usize = 16;
/// Largest number of parameters of one function.
pub const MAX_PARAMS: u32 = 8;
/// Largest number of extra `i32` locals of one function.
pub const MAX_LOCALS: u32 = 16;
/// Largest block nesting inside one function body.
pub const MAX_CONTROL_FRAMES: usize = 64;

const TYPE_I32: u8 = 0x7F;
const BLOCK_EMPTY: u8 = 0x40;
const SECTION_COUNT: usize = 13;

/// A function signature. Every value is `i32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FuncType {
    /// Number of parameters.
    pub params: u8,
    /// Number of results, `0` or `1`.
    pub results: u8,
}

/// A host function a block may import from the `aios` module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFunc {
    /// `aios.log(ptr, len)` — log `len` bytes of linear memory as UTF-8.
    Log,
    /// `aios.exit(code)` — terminate the block with `code`.
    Exit,
    /// `aios.now_ms() -> i32` — monotonic milliseconds, truncated to `i32`.
    NowMs,
    /// `aios.yield()` — let the kernel run something else.
    Yield,
}

impl HostFunc {
    /// The import name inside the `aios` module.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Exit => "exit",
            Self::NowMs => "now_ms",
            Self::Yield => "yield",
        }
    }

    /// The capability a token must grant before this import runs.
    pub const fn right(self) -> Rights {
        match self {
            Self::Log => Rights::LOG,
            Self::Exit => Rights::EXIT,
            Self::NowMs => Rights::CLOCK,
            Self::Yield => Rights::YIELD,
        }
    }

    /// The exact signature the ABI requires.
    pub const fn signature(self) -> FuncType {
        match self {
            Self::Log => FuncType {
                params: 2,
                results: 0,
            },
            Self::Exit => FuncType {
                params: 1,
                results: 0,
            },
            Self::NowMs => FuncType {
                params: 0,
                results: 1,
            },
            Self::Yield => FuncType {
                params: 0,
                results: 0,
            },
        }
    }
}

/// One imported host function.
#[derive(Debug, Clone, Copy)]
pub struct Import<'a> {
    /// Import module, always `aios` in the current ABI.
    pub module: &'a str,
    /// Import name, e.g. `log`.
    pub name: &'a str,
    /// The host function this import binds to.
    pub host: HostFunc,
    type_index: u32,
}

impl Import<'_> {
    /// The index into the module's type section.
    pub const fn type_index(&self) -> u32 {
        self.type_index
    }

    /// The capability this import needs.
    pub const fn rights(&self) -> Rights {
        self.host.right()
    }
}

/// One exported function.
#[derive(Debug, Clone, Copy)]
pub struct Export<'a> {
    /// Export name, e.g. `_start`.
    pub name: &'a str,
    /// Index in the function index space.
    pub func: u32,
}

/// One active data segment, already validated against the memory size.
#[derive(Debug, Clone, Copy)]
struct Data<'a> {
    offset: u32,
    bytes: &'a [u8],
}

/// A parsed and statically validated WebAssembly module.
#[derive(Debug, Clone)]
pub struct Module<'a> {
    bytes: &'a [u8],
    types: Vec<FuncType>,
    imports: Vec<Import<'a>>,
    func_types: Vec<u32>,
    local_counts: Vec<u32>,
    bodies: Vec<&'a [u8]>,
    exports: Vec<Export<'a>>,
    data: Vec<Data<'a>>,
    memory_pages: u32,
    start: Option<u32>,
}

impl<'a> Module<'a> {
    /// Parses and validates a module.
    ///
    /// Returns the module borrowing `bytes`, or the first structural problem in
    /// module order. A module with no sections beyond the header is valid and
    /// does nothing: [`crate::Host::instantiate`] succeeds and there is no
    /// export to call.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ModuleError<'a>> {
        let mut header = Reader::new(bytes);
        if header.bytes(4)? != b"\0asm" {
            return Err(ModuleError::Magic);
        }
        if header.bytes(4)? != [1, 0, 0, 0] {
            return Err(ModuleError::Version);
        }

        let mut sections: [Option<&'a [u8]>; SECTION_COUNT] = [None; SECTION_COUNT];
        while !header.eof() {
            let id = header.u8()?;
            let size = header.uleb32()? as usize;
            let payload = header.bytes(size)?;
            let id = usize::from(id);
            if id >= SECTION_COUNT {
                return Err(ModuleError::UnsupportedSection(id as u8));
            }
            // Tables, globals, elements and the data-count section belong to
            // features this runtime does not implement. Ignoring them would
            // change what the module means, so they are rejected instead.
            if !matches!(id, 0 | 1 | 2 | 3 | 5 | 7 | 8 | 10 | 11) {
                return Err(ModuleError::UnsupportedSection(id as u8));
            }
            // A custom section may repeat and is ignored; anything else must
            // appear at most once.
            if id != 0 && sections[id].replace(payload).is_some() {
                return Err(ModuleError::DuplicateSection(id as u8));
            }
        }

        let mut module = Self {
            bytes,
            types: Vec::new(),
            imports: Vec::new(),
            func_types: Vec::new(),
            local_counts: Vec::new(),
            bodies: Vec::new(),
            exports: Vec::new(),
            data: Vec::new(),
            memory_pages: 0,
            start: None,
        };
        if let Some(payload) = sections[1] {
            module.parse_types(payload)?;
        }
        if let Some(payload) = sections[2] {
            module.parse_imports(payload)?;
        }
        let defined = match sections[3] {
            Some(payload) => module.parse_functions(payload)?,
            None => Vec::new(),
        };
        if let Some(payload) = sections[5] {
            module.parse_memory(payload)?;
        }
        if let Some(payload) = sections[7] {
            module.parse_exports(payload)?;
        }
        if let Some(payload) = sections[11] {
            module.parse_data(payload)?;
        }
        if let Some(payload) = sections[8] {
            module.parse_start(payload)?;
        }
        match sections[10] {
            Some(payload) => module.parse_code(payload, &defined)?,
            None if !defined.is_empty() => return Err(ModuleError::BodyCount),
            None => {}
        }
        Ok(module)
    }

    /// The module bytes this view borrows.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The host imports, in index order.
    pub fn imports(&self) -> &[Import<'a>] {
        &self.imports
    }

    /// The exported functions, in declaration order.
    pub fn exports(&self) -> &[Export<'a>] {
        &self.exports
    }

    /// The declared size of the linear memory, in pages (`0` when absent).
    pub const fn memory_pages(&self) -> u32 {
        self.memory_pages
    }

    /// The start function, run once by [`crate::Host::instantiate`].
    pub const fn start(&self) -> Option<u32> {
        self.start
    }

    /// The union of the rights every host import needs.
    ///
    /// The kernel logs this next to the token it grants, so a block that asks
    /// for more than the configuration allows is visible before it runs.
    pub fn required_rights(&self) -> Rights {
        self.imports
            .iter()
            .fold(Rights::NONE, |rights, import| rights.union(import.rights()))
    }

    /// The index of the exported function `name`, if it exists.
    pub fn export(&self, name: &str) -> Option<u32> {
        self.exports
            .iter()
            .find(|export| export.name == name)
            .map(|export| export.func)
    }

    /// Number of functions in the index space (imports first).
    pub fn func_count(&self) -> usize {
        self.func_types.len()
    }

    /// The signature of function `func`.
    pub(crate) fn func_type(&self, func: u32) -> Result<FuncType, ModuleError<'a>> {
        let index = *self
            .func_types
            .get(func as usize)
            .ok_or(ModuleError::BadFunctionIndex(func))?;
        self.types
            .get(index as usize)
            .copied()
            .ok_or(ModuleError::UnknownTypeIndex(index))
    }

    /// The body of defined function `func`, or `None` for an import.
    pub(crate) fn body(&self, func: u32) -> Option<&'a [u8]> {
        let index = (func as usize).checked_sub(self.imports.len())?;
        self.bodies.get(index).copied()
    }

    /// The number of extra locals declared by defined function `func`.
    pub(crate) fn local_count(&self, func: u32) -> u32 {
        let index = (func as usize).saturating_sub(self.imports.len());
        self.local_counts.get(index).copied().unwrap_or(0)
    }

    /// The data segments, `(offset, bytes)`.
    pub(crate) fn data(&self) -> impl Iterator<Item = (u32, &'a [u8])> + '_ {
        self.data.iter().map(|data| (data.offset, data.bytes))
    }

    fn parse_types(&mut self, payload: &'a [u8]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()? as usize;
        if count > MAX_TYPES {
            return Err(ModuleError::TooMany("types"));
        }
        for _ in 0..count {
            if reader.u8()? != 0x60 {
                return Err(ModuleError::BadType);
            }
            let params = reader.uleb32()?;
            if params > MAX_PARAMS {
                return Err(ModuleError::BadType);
            }
            for _ in 0..params {
                let value_type = reader.u8()?;
                if value_type != TYPE_I32 {
                    return Err(ModuleError::UnsupportedType(value_type));
                }
            }
            let results = reader.uleb32()?;
            if results > 1 {
                return Err(ModuleError::BadType);
            }
            for _ in 0..results {
                let value_type = reader.u8()?;
                if value_type != TYPE_I32 {
                    return Err(ModuleError::UnsupportedType(value_type));
                }
            }
            self.types.push(FuncType {
                params: params as u8,
                results: results as u8,
            });
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed("trailing bytes in the type section"));
        }
        Ok(())
    }

    fn parse_imports(&mut self, payload: &'a [u8]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()? as usize;
        if count > MAX_IMPORTS {
            return Err(ModuleError::TooMany("imports"));
        }
        for _ in 0..count {
            let module = reader.name()?;
            let name = reader.name()?;
            let kind = reader.u8()?;
            if kind != 0 {
                return Err(ModuleError::UnsupportedImportKind(kind));
            }
            let type_index = reader.uleb32()?;
            let signature = *self
                .types
                .get(type_index as usize)
                .ok_or(ModuleError::UnknownTypeIndex(type_index))?;
            let host = host_func(module, name)?;
            if host.signature() != signature {
                return Err(ModuleError::BadImport(name));
            }
            if self.func_types.len() >= MAX_FUNCTIONS {
                return Err(ModuleError::TooMany("functions"));
            }
            self.imports.push(Import {
                module,
                name,
                host,
                type_index,
            });
            self.func_types.push(type_index);
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed(
                "trailing bytes in the import section",
            ));
        }
        Ok(())
    }

    fn parse_functions(&mut self, payload: &'a [u8]) -> Result<Vec<u32>, ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()? as usize;
        if count + self.func_types.len() > MAX_FUNCTIONS {
            return Err(ModuleError::TooMany("functions"));
        }
        let mut defined = Vec::with_capacity(count);
        for _ in 0..count {
            let type_index = reader.uleb32()?;
            if type_index as usize >= self.types.len() {
                return Err(ModuleError::UnknownTypeIndex(type_index));
            }
            defined.push(type_index);
            self.func_types.push(type_index);
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed(
                "trailing bytes in the function section",
            ));
        }
        Ok(defined)
    }

    fn parse_memory(&mut self, payload: &'a [u8]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()?;
        if count > 1 {
            return Err(ModuleError::TooMany("memories"));
        }
        if count == 1 {
            let flags = reader.u8()?;
            let min = reader.uleb32()?;
            if flags > 1 {
                return Err(ModuleError::Malformed("memory limits"));
            }
            if flags == 1 {
                reader.uleb32()?;
            }
            if min > MAX_MEMORY_PAGES {
                return Err(ModuleError::MemoryLimitExceeded(min));
            }
            self.memory_pages = min;
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed(
                "trailing bytes in the memory section",
            ));
        }
        Ok(())
    }

    fn parse_exports(&mut self, payload: &'a [u8]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()? as usize;
        if count > MAX_EXPORTS {
            return Err(ModuleError::TooMany("exports"));
        }
        for _ in 0..count {
            let name = reader.name()?;
            let kind = reader.u8()?;
            let index = reader.uleb32()?;
            match kind {
                0 => {
                    if index as usize >= self.func_types.len() {
                        return Err(ModuleError::BadFunctionIndex(index));
                    }
                    if self.exports.iter().any(|export| export.name == name) {
                        return Err(ModuleError::DuplicateExport);
                    }
                    self.exports.push(Export { name, func: index });
                }
                2 => {
                    if index != 0 || self.memory_pages == 0 {
                        return Err(ModuleError::MemoryMissing);
                    }
                }
                _ => return Err(ModuleError::Malformed("unsupported export kind")),
            }
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed(
                "trailing bytes in the export section",
            ));
        }
        Ok(())
    }

    fn parse_start(&mut self, payload: &'a [u8]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let func = reader.uleb32()?;
        if !reader.eof() {
            return Err(ModuleError::Malformed(
                "trailing bytes in the start section",
            ));
        }
        let signature = self.func_type(func)?;
        if signature.params != 0 || signature.results != 0 {
            return Err(ModuleError::StartSignature);
        }
        self.start = Some(func);
        Ok(())
    }

    fn parse_data(&mut self, payload: &'a [u8]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()? as usize;
        if count > MAX_DATA_SEGMENTS {
            return Err(ModuleError::TooMany("data segments"));
        }
        for _ in 0..count {
            let flags = reader.uleb32()?;
            let offset = match flags {
                0 => read_const_offset(&mut reader)?,
                2 => {
                    let memory = reader.uleb32()?;
                    if memory != 0 {
                        return Err(ModuleError::BadDataSegment);
                    }
                    read_const_offset(&mut reader)?
                }
                _ => return Err(ModuleError::BadDataSegment),
            };
            let len = reader.uleb32()? as usize;
            let bytes = reader.bytes(len)?;
            if self.memory_pages == 0 {
                return Err(ModuleError::MemoryMissing);
            }
            let end = u64::from(offset) + bytes.len() as u64;
            if end > u64::from(self.memory_pages) * u64::from(PAGE_SIZE) {
                return Err(ModuleError::BadDataSegment);
            }
            self.data.push(Data { offset, bytes });
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed("trailing bytes in the data section"));
        }
        Ok(())
    }

    fn parse_code(&mut self, payload: &'a [u8], defined: &[u32]) -> Result<(), ModuleError<'a>> {
        let mut reader = Reader::new(payload);
        let count = reader.uleb32()? as usize;
        if count != defined.len() {
            return Err(ModuleError::BodyCount);
        }
        for &type_index in defined {
            let size = reader.uleb32()? as usize;
            let body = reader.bytes(size)?;
            let mut body_reader = Reader::new(body);
            let groups = body_reader.uleb32()?;
            let mut locals = 0u32;
            for _ in 0..groups {
                let count = body_reader.uleb32()?;
                let value_type = body_reader.u8()?;
                if value_type != TYPE_I32 {
                    return Err(ModuleError::UnsupportedType(value_type));
                }
                locals = locals.checked_add(count).ok_or(ModuleError::BadLocals)?;
            }
            if locals > MAX_LOCALS {
                return Err(ModuleError::BadLocals);
            }
            let code = body_reader.rest();
            let signature = self
                .types
                .get(type_index as usize)
                .copied()
                .ok_or(ModuleError::UnknownTypeIndex(type_index))?;
            validate_body(
                code,
                u32::from(signature.params) + locals,
                self.func_types.len(),
                self.memory_pages > 0,
            )?;
            self.local_counts.push(locals);
            self.bodies.push(code);
        }
        if !reader.eof() {
            return Err(ModuleError::Malformed("trailing bytes in the code section"));
        }
        Ok(())
    }
}

/// Maps an import module/name pair to the host ABI, rejecting anything else.
fn host_func<'a>(module: &'a str, name: &'a str) -> Result<HostFunc, ModuleError<'a>> {
    if module != "aios" {
        return Err(ModuleError::UnknownImport(module));
    }
    match name {
        "log" => Ok(HostFunc::Log),
        "exit" => Ok(HostFunc::Exit),
        "now_ms" => Ok(HostFunc::NowMs),
        "yield" => Ok(HostFunc::Yield),
        _ => Err(ModuleError::UnknownImport(name)),
    }
}

/// Reads the `i32.const <offset> end` expression of a data segment.
fn read_const_offset<'a>(reader: &mut Reader<'a>) -> Result<u32, ModuleError<'a>> {
    if reader.u8()? != 0x41 {
        return Err(ModuleError::BadDataSegment);
    }
    let offset = reader.sleb32()?;
    if reader.u8()? != 0x0B {
        return Err(ModuleError::BadDataSegment);
    }
    if offset < 0 {
        return Err(ModuleError::BadDataSegment);
    }
    Ok(offset as u32)
}

/// Checks one function body without executing it.
fn validate_body<'a>(
    code: &'a [u8],
    local_count: u32,
    func_count: usize,
    has_memory: bool,
) -> Result<(), ModuleError<'a>> {
    let mut reader = Reader::new(code);
    let mut kinds = [0u8; MAX_CONTROL_FRAMES];
    let mut results = [false; MAX_CONTROL_FRAMES];
    let mut depth = 0usize;
    let mut finished = false;

    while !reader.eof() {
        if finished {
            return Err(ModuleError::Malformed(
                "instructions after the function end",
            ));
        }
        let opcode = reader.u8()?;
        match opcode {
            0x00 | 0x01 | 0x0F | 0x1A | 0x1B => {}
            0x02..=0x04 => {
                let block_type = reader.u8()?;
                if block_type != BLOCK_EMPTY && block_type != TYPE_I32 {
                    return Err(ModuleError::BadBlockType(block_type));
                }
                if opcode == 0x03 && block_type != BLOCK_EMPTY {
                    return Err(ModuleError::BadBlockType(block_type));
                }
                if depth >= MAX_CONTROL_FRAMES {
                    return Err(ModuleError::TooMany("control frames"));
                }
                kinds[depth] = opcode;
                results[depth] = block_type == TYPE_I32;
                depth += 1;
            }
            0x05 => {
                if depth == 0 {
                    return Err(ModuleError::Malformed("else outside an if"));
                }
                match kinds[depth - 1] {
                    0x04 => kinds[depth - 1] = 0x85,
                    0x85 => return Err(ModuleError::Malformed("duplicate else")),
                    _ => return Err(ModuleError::Malformed("else outside an if")),
                }
            }
            0x0B => {
                if depth == 0 {
                    finished = true;
                } else {
                    depth -= 1;
                    if kinds[depth] == 0x04 && results[depth] {
                        return Err(ModuleError::Malformed("if with a result needs else"));
                    }
                }
            }
            0x0C | 0x0D => {
                let target = reader.uleb32()?;
                if target as usize > depth {
                    return Err(ModuleError::BadBranch);
                }
            }
            0x10 => {
                let func = reader.uleb32()?;
                if func as usize >= func_count {
                    return Err(ModuleError::BadFunctionIndex(func));
                }
            }
            0x20..=0x22 => {
                let local = reader.uleb32()?;
                if local >= local_count {
                    return Err(ModuleError::BadLocalIndex(local));
                }
            }
            0x41 => {
                reader.sleb32()?;
            }
            0x28 | 0x36 => {
                if !has_memory {
                    return Err(ModuleError::MemoryMissing);
                }
                reader.uleb32()?;
                reader.uleb32()?;
            }
            0x3F => {
                if !has_memory {
                    return Err(ModuleError::MemoryMissing);
                }
                if reader.u8()? != 0 {
                    return Err(ModuleError::Malformed("memory index must be zero"));
                }
            }
            0x45..=0x4F | 0x6A..=0x76 => {}
            _ => return Err(ModuleError::UnsupportedOpcode(opcode)),
        }
    }
    if !finished {
        return Err(ModuleError::Malformed("function body has no end"));
    }
    if depth != 0 {
        return Err(ModuleError::Malformed("unbalanced block"));
    }
    Ok(())
}

/// Why a module cannot be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleError<'a> {
    /// The header does not start with `\0asm`.
    Magic,
    /// The binary format version is not 1.
    Version,
    /// A section, name or body ends in the middle of a value.
    Truncated,
    /// A LEB128 value is longer than 32 bits.
    LebOverflow,
    /// A name is not valid UTF-8.
    Utf8,
    /// A section id outside the supported range.
    UnsupportedSection(u8),
    /// A non-custom section appears twice.
    DuplicateSection(u8),
    /// A section payload is structurally wrong; the text says where.
    Malformed(&'static str),
    /// A table grew past its limit; the text names the table.
    TooMany(&'static str),
    /// A function type is not `0x60` or has a bad parameter/result vector.
    BadType,
    /// A value type other than `i32` (`0x7F`) is used.
    UnsupportedType(u8),
    /// An import is not a function import.
    UnsupportedImportKind(u8),
    /// An import name is outside the `aios` host ABI.
    UnknownImport(&'a str),
    /// A known import has the wrong signature.
    BadImport(&'a str),
    /// An export name is declared twice.
    DuplicateExport,
    /// The start function takes or returns values.
    StartSignature,
    /// A block type is neither `0x40` nor `0x7F`.
    BadBlockType(u8),
    /// An instruction outside the supported subset.
    UnsupportedOpcode(u8),
    /// A branch depth points outside the current nesting.
    BadBranch,
    /// A local index is outside the function's locals.
    BadLocalIndex(u32),
    /// A function index is outside the function index space.
    BadFunctionIndex(u32),
    /// A type index is outside the type section.
    UnknownTypeIndex(u32),
    /// A memory instruction or data segment without a memory.
    MemoryMissing,
    /// A module declares more than [`MAX_MEMORY_PAGES`] pages.
    MemoryLimitExceeded(u32),
    /// A data segment is passive, misplaced or out of bounds.
    BadDataSegment,
    /// The declared local count is too large.
    BadLocals,
    /// The code section does not match the function section.
    BodyCount,
}

impl fmt::Display for ModuleError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magic => f.write_str("not a WebAssembly module (bad magic)"),
            Self::Version => f.write_str("unsupported WebAssembly version"),
            Self::Truncated => f.write_str("truncated module"),
            Self::LebOverflow => f.write_str("LEB128 value overflows 32 bits"),
            Self::Utf8 => f.write_str("name is not valid UTF-8"),
            Self::UnsupportedSection(id) => write!(f, "unsupported section id {id}"),
            Self::DuplicateSection(id) => write!(f, "section id {id} appears twice"),
            Self::Malformed(what) => write!(f, "malformed module: {what}"),
            Self::TooMany(what) => write!(f, "too many {what}"),
            Self::BadType => f.write_str("bad function type"),
            Self::UnsupportedType(value_type) => {
                write!(f, "unsupported value type 0x{value_type:02x} (i32 only)")
            }
            Self::UnsupportedImportKind(kind) => {
                write!(f, "unsupported import kind {kind} (functions only)")
            }
            Self::UnknownImport(name) => write!(f, "unknown import {name}"),
            Self::BadImport(name) => write!(f, "import {name} has the wrong signature"),
            Self::DuplicateExport => f.write_str("duplicate export name"),
            Self::StartSignature => f.write_str("the start function must take and return nothing"),
            Self::BadBlockType(block_type) => {
                write!(f, "unsupported block type 0x{block_type:02x}")
            }
            Self::UnsupportedOpcode(opcode) => write!(f, "unsupported opcode 0x{opcode:02x}"),
            Self::BadBranch => f.write_str("branch depth is out of range"),
            Self::BadLocalIndex(index) => write!(f, "local index {index} is out of range"),
            Self::BadFunctionIndex(index) => write!(f, "function index {index} is out of range"),
            Self::UnknownTypeIndex(index) => write!(f, "type index {index} is out of range"),
            Self::MemoryMissing => f.write_str("a memory instruction or data needs a memory"),
            Self::MemoryLimitExceeded(pages) => {
                write!(
                    f,
                    "{pages} memory pages exceed the {MAX_MEMORY_PAGES}-page limit"
                )
            }
            Self::BadDataSegment => f.write_str("bad data segment"),
            Self::BadLocals => f.write_str("too many locals"),
            Self::BodyCount => f.write_str("the code section does not match the function section"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_asm as asm;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    fn add_module() -> Vec<u8> {
        asm::module(&[
            asm::types(&[(2, 1)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("add", 0)]),
            asm::code(&[asm::body(&[], &[0x20, 0x00, 0x20, 0x01, 0x6A, 0x0B])]),
        ])
    }

    #[test]
    fn accepts_the_header_alone() {
        let bytes = asm::module(&[]);
        let module = Module::parse(&bytes).unwrap();
        assert_eq!(module.memory_pages(), 0);
        assert!(module.imports().is_empty());
        assert!(module.exports().is_empty());
        assert!(module.start().is_none());
        assert_eq!(module.required_rights(), Rights::NONE);
        assert_eq!(module.func_count(), 0);
        assert_eq!(module.bytes(), bytes.as_slice());
    }

    #[test]
    fn accepts_a_complete_module() {
        let bytes = add_module();
        let module = Module::parse(&bytes).unwrap();
        assert_eq!(module.func_count(), 1);
        assert_eq!(module.export("add"), Some(0));
        assert_eq!(module.export("sub"), None);
        assert_eq!(
            module.func_type(0),
            Ok(FuncType {
                params: 2,
                results: 1
            })
        );
    }

    #[test]
    fn rejects_a_bad_header() {
        assert_eq!(Module::parse(b"nope").err(), Some(ModuleError::Magic));
        let mut bytes = asm::module(&[]);
        bytes[4] = 2;
        assert_eq!(Module::parse(&bytes).err(), Some(ModuleError::Version));
        assert_eq!(Module::parse(&[]).err(), Some(ModuleError::Truncated));
    }

    #[test]
    fn rejects_unsupported_sections_and_duplicates() {
        let bytes = asm::module(&[asm::section(12, &[0])]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::UnsupportedSection(12))
        );
        let bytes = asm::module(&[asm::section(0, &[]), asm::section(0, &[])]);
        assert!(Module::parse(&bytes).is_ok());
        let bytes = asm::module(&[asm::types(&[]), asm::types(&[])]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::DuplicateSection(1))
        );
    }

    #[test]
    fn rejects_float_types() {
        let mut payload = asm::leb32(1);
        payload.push(0x60);
        payload.extend(asm::leb32(1));
        payload.push(0x7D);
        payload.extend(asm::leb32(0));
        let bytes = asm::module(&[asm::section(1, &payload)]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::UnsupportedType(0x7D))
        );
    }

    #[test]
    fn rejects_imports_outside_the_abi() {
        let bytes = asm::module(&[
            asm::types(&[(2, 0)]),
            asm::imports(&[asm::import_func("wasi", "log", 0)]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::UnknownImport("wasi"))
        );

        let bytes = asm::module(&[
            asm::types(&[(2, 0)]),
            asm::imports(&[asm::import_func("aios", "teleport", 0)]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::UnknownImport("teleport"))
        );
    }

    #[test]
    fn rejects_an_import_with_the_wrong_signature() {
        let bytes = asm::module(&[
            asm::types(&[(1, 0)]),
            asm::imports(&[asm::import_func("aios", "log", 0)]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::BadImport("log"))
        );
    }

    #[test]
    fn reports_the_rights_the_module_needs() {
        let bytes = asm::module(&[
            asm::types(&[(2, 0), (0, 1)]),
            asm::imports(&[
                asm::import_func("aios", "log", 0),
                asm::import_func("aios", "now_ms", 1),
            ]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        assert_eq!(module.required_rights(), Rights::LOG | Rights::CLOCK);
        assert_eq!(module.imports().len(), 2);
        assert_eq!(module.imports()[1].host, HostFunc::NowMs);
    }

    #[test]
    fn rejects_unsupported_opcodes() {
        let bytes = asm::module(&[
            asm::types(&[(0, 1)]),
            asm::functions(&[0]),
            asm::code(&[asm::body(&[], &[0x7C, 0x0B])]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::UnsupportedOpcode(0x7C))
        );
    }

    #[test]
    fn rejects_a_branch_outside_the_function() {
        let bytes = asm::module(&[
            asm::types(&[(0, 0)]),
            asm::functions(&[0]),
            asm::code(&[asm::body(&[], &[0x0C, 0x05, 0x0B])]),
        ]);
        assert_eq!(Module::parse(&bytes).err(), Some(ModuleError::BadBranch));
    }

    #[test]
    fn rejects_an_if_with_a_result_but_no_else() {
        let bytes = asm::module(&[
            asm::types(&[(0, 1)]),
            asm::functions(&[0]),
            asm::code(&[asm::body(
                &[],
                &[0x41, 0x01, 0x04, 0x7F, 0x41, 0x02, 0x0B, 0x0B],
            )]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::Malformed("if with a result needs else"))
        );
    }

    #[test]
    fn rejects_out_of_range_indices() {
        let bytes = asm::module(&[
            asm::types(&[(0, 0)]),
            asm::functions(&[0]),
            asm::code(&[asm::body(&[], &[0x20, 0x03, 0x0B])]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::BadLocalIndex(3))
        );

        let bytes = asm::module(&[
            asm::types(&[(0, 0)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("run", 1)]),
            asm::code(&[asm::body(&[], &[0x0B])]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::BadFunctionIndex(1))
        );
    }

    #[test]
    fn rejects_memory_limits_and_out_of_bounds_data() {
        let bytes = asm::module(&[asm::memory(MAX_MEMORY_PAGES + 1)]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::MemoryLimitExceeded(MAX_MEMORY_PAGES + 1))
        );

        let bytes = asm::module(&[
            asm::memory(1),
            asm::data(&[asm::data_active(65_530, b"0123456789")]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::BadDataSegment)
        );

        let bytes = asm::module(&[
            asm::types(&[(0, 1)]),
            asm::functions(&[0]),
            asm::code(&[asm::body(&[], &[0x41, 0x00, 0x28, 0x02, 0x00, 0x0B])]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::MemoryMissing)
        );
    }

    #[test]
    fn rejects_a_bad_start_signature() {
        let bytes = asm::module(&[
            asm::types(&[(1, 0)]),
            asm::functions(&[0]),
            asm::start(0),
            asm::code(&[asm::body(&[], &[0x0B])]),
        ]);
        assert_eq!(
            Module::parse(&bytes).err(),
            Some(ModuleError::StartSignature)
        );
    }

    #[test]
    fn error_messages_name_the_problem() {
        assert_eq!(
            ModuleError::UnsupportedOpcode(0x7C).to_string(),
            "unsupported opcode 0x7c"
        );
        assert_eq!(
            ModuleError::MemoryLimitExceeded(17).to_string(),
            "17 memory pages exceed the 16-page limit"
        );
    }
}
