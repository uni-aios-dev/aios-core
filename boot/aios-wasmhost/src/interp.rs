//! The sandbox: a deterministic `i32` interpreter with a capability gate.
//!
//! [`Host`] owns the linear memory of one block, its instruction budget and the
//! capability [`Token`]. Nothing a block does can escape that state: loads and
//! stores are bounds-checked against the declared pages, every call into the
//! kernel goes through [`Env`] and is checked against the token, and every
//! instruction spends fuel so a `loop {}` ends in [`Trap::OutOfFuel`] instead of
//! hanging the CPU.
//!
//! The interpreter never recurses on the Rust stack: calls push a frame into a
//! fixed table and return by popping it, so a malicious call chain cannot
//! overflow the kernel stack.

use crate::cap::Token;
use crate::module::{HostFunc, Module, PAGE_SIZE};
use core::fmt;

/// The default instruction budget of one block run.
pub const DEFAULT_FUEL: u32 = 100_000;

const MAX_VALUE_STACK: usize = 96;
const MAX_LOCAL_SLOTS: usize = 64;
const MAX_CALL_DEPTH: usize = 16;
const MAX_CONTROL: usize = 64;

/// What a block may ask the kernel to do.
///
/// The kernel implements this trait once and every block shares it; a method is
/// only reached when the block's token grants the matching [`Rights`] bit.
pub trait Env {
    /// Writes one UTF-8 string to the kernel log.
    fn log(&mut self, text: &str);

    /// The monotonic clock in milliseconds, truncated to `i32`.
    fn now_ms(&mut self) -> i32;

    /// Called by `aios.yield`; the default keeps the interpreter running.
    fn idle(&mut self) {}
}

/// Why a block stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    /// The instruction budget of the token is spent.
    OutOfFuel,
    /// The token does not grant the right a host import needs.
    Denied(HostFunc),
    /// The block called `aios.exit`.
    Exited(i32),
    /// The requested export does not exist.
    MissingExport,
    /// The exported function expects a different number of arguments.
    BadArgs {
        /// Parameters of the exported function.
        expected: usize,
        /// Arguments the kernel passed.
        got: usize,
    },
    /// The linear memory buffer is smaller than the module declares.
    MemoryTooSmall {
        /// Bytes the module needs.
        required: usize,
        /// Bytes the kernel provided.
        provided: usize,
    },
    /// A memory instruction ran without a memory section.
    NoMemory,
    /// An address is outside the declared pages.
    MemoryOutOfBounds,
    /// An instruction needed a value the stack does not have.
    Underflow,
    /// The value stack is full.
    StackOverflow,
    /// More nested calls than [`MAX_CALL_DEPTH`].
    CallDepth,
    /// More locals than [`MAX_LOCAL_SLOTS`].
    LocalOverflow,
    /// A branch depth outside the current function.
    BadBranch,
    /// An instruction byte that validation should have rejected.
    BadInstruction(u8),
    /// Division or remainder by zero.
    DivideByZero,
    /// `i32.min_value / -1`.
    IntegerOverflow,
    /// The `unreachable` instruction ran.
    Unreachable,
    /// The module is internally inconsistent or not instantiated.
    BadModule,
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfFuel => f.write_str("out of fuel"),
            Self::Denied(host) => write!(f, "capability denied for aios.{}", host.name()),
            Self::Exited(code) => write!(f, "block exited with {code}"),
            Self::MissingExport => f.write_str("missing export"),
            Self::BadArgs { expected, got } => {
                write!(f, "expected {expected} argument(s), got {got}")
            }
            Self::MemoryTooSmall { required, provided } => {
                write!(f, "needs {required} byte(s) of memory, got {provided}")
            }
            Self::NoMemory => f.write_str("memory access without a memory section"),
            Self::MemoryOutOfBounds => f.write_str("memory access out of bounds"),
            Self::Underflow => f.write_str("value stack underflow"),
            Self::StackOverflow => f.write_str("value stack overflow"),
            Self::CallDepth => f.write_str("call depth exceeded"),
            Self::LocalOverflow => f.write_str("too many locals"),
            Self::BadBranch => f.write_str("branch out of range"),
            Self::BadInstruction(opcode) => {
                write!(f, "unexpected instruction 0x{opcode:02x}")
            }
            Self::DivideByZero => f.write_str("integer division by zero"),
            Self::IntegerOverflow => f.write_str("integer overflow"),
            Self::Unreachable => f.write_str("unreachable instruction executed"),
            Self::BadModule => f.write_str("module is not instantiated"),
        }
    }
}

/// One active call, stored in the host instead of the Rust stack.
#[derive(Debug, Clone, Copy)]
struct CallFrame {
    func: u32,
    pc: usize,
    local_base: usize,
    stack_base: usize,
    control_base: usize,
    results: u8,
}

/// One enclosing block, loop or if.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Block,
    Loop,
    If,
}

#[derive(Debug, Clone, Copy)]
struct ControlFrame {
    kind: Kind,
    start: usize,
    end: usize,
    height: usize,
    results: usize,
}

/// How a branch left the instruction stream.
enum Flow {
    /// Execution continues at the current frame.
    Continue,
    /// The function returned and its frame was popped.
    Return(Option<i32>),
}

/// The execution state of one block.
///
/// The host borrows the linear memory buffer for its whole lifetime, so a block
/// sees exactly the pages it declared and nothing else.
pub struct Host<'m, E: Env> {
    memory: &'m mut [u8],
    env: E,
    token: Token,
    fuel: u32,
    pages: u32,
    stack: [i32; MAX_VALUE_STACK],
    sp: usize,
    locals: [i32; MAX_LOCAL_SLOTS],
    local_top: usize,
    frames: [CallFrame; MAX_CALL_DEPTH],
    fp: usize,
    control: [ControlFrame; MAX_CONTROL],
    cp: usize,
}

impl<'m, E: Env> Host<'m, E> {
    /// Wraps a caller-provided memory buffer and environment.
    ///
    /// The module is not installed yet: call [`Host::instantiate`] before
    /// [`Host::call`].
    pub fn new(memory: &'m mut [u8], env: E, token: Token) -> Self {
        Self {
            memory,
            env,
            token,
            fuel: token.fuel(),
            pages: 0,
            stack: [0; MAX_VALUE_STACK],
            sp: 0,
            locals: [0; MAX_LOCAL_SLOTS],
            local_top: 0,
            frames: [CallFrame {
                func: 0,
                pc: 0,
                local_base: 0,
                stack_base: 0,
                control_base: 0,
                results: 0,
            }; MAX_CALL_DEPTH],
            fp: 0,
            control: [ControlFrame {
                kind: Kind::Block,
                start: 0,
                end: 0,
                height: 0,
                results: 0,
            }; MAX_CONTROL],
            cp: 0,
        }
    }

    /// The linear memory of the block.
    pub fn memory(&self) -> &[u8] {
        self.memory
    }

    /// The linear memory of the block, for the kernel to inspect after a run.
    pub fn memory_mut(&mut self) -> &mut [u8] {
        self.memory
    }

    /// The number of instantiated pages.
    pub fn pages(&self) -> u32 {
        self.pages
    }

    /// The instructions left in the current budget.
    pub fn fuel(&self) -> u32 {
        self.fuel
    }

    /// The token this host enforces.
    pub fn token(&self) -> Token {
        self.token
    }

    /// The environment behind the host ABI.
    pub fn env(&self) -> &E {
        &self.env
    }

    /// The environment behind the host ABI.
    pub fn env_mut(&mut self) -> &mut E {
        &mut self.env
    }

    /// Installs `module`: checks the memory size, applies data segments and runs
    /// the start function.
    ///
    /// Calling it again with another module starts from a fresh page set, which
    /// is how the kernel reuses one host for a block store.
    pub fn instantiate(&mut self, module: &Module<'m>) -> Result<(), Trap> {
        let pages = module.memory_pages();
        let required = pages as usize * PAGE_SIZE as usize;
        if self.memory.len() < required {
            return Err(Trap::MemoryTooSmall {
                required,
                provided: self.memory.len(),
            });
        }
        self.pages = pages;
        self.memory[..required].fill(0);
        for (offset, bytes) in module.data() {
            let start = offset as usize;
            let end = start + bytes.len();
            if end > required {
                return Err(Trap::MemoryOutOfBounds);
            }
            self.memory[start..end].copy_from_slice(bytes);
        }
        if let Some(start) = module.start() {
            self.run(module, start, &[])?;
        }
        Ok(())
    }

    /// Calls the exported function `name` with up to eight `i32` arguments.
    ///
    /// Each call starts from a full budget, so `instantiate` plus `call` gives
    /// the entry point the same allowance as a start function.
    pub fn call(
        &mut self,
        module: &Module<'m>,
        name: &str,
        args: &[i32],
    ) -> Result<Option<i32>, Trap> {
        let func = module.export(name).ok_or(Trap::MissingExport)?;
        if module.memory_pages() > 0 && self.pages == 0 {
            return Err(Trap::BadModule);
        }
        self.run(module, func, args)
    }

    /// Calls function `func` by index, without an export lookup.
    pub fn call_func(
        &mut self,
        module: &Module<'m>,
        func: u32,
        args: &[i32],
    ) -> Result<Option<i32>, Trap> {
        self.run(module, func, args)
    }

    fn run(&mut self, module: &Module<'m>, func: u32, args: &[i32]) -> Result<Option<i32>, Trap> {
        let signature = module.func_type(func).map_err(|_| Trap::BadModule)?;
        if args.len() != usize::from(signature.params) {
            return Err(Trap::BadArgs {
                expected: usize::from(signature.params),
                got: args.len(),
            });
        }
        self.fuel = self.token.fuel();
        self.sp = 0;
        self.cp = 0;
        self.fp = 0;
        self.local_top = 0;

        if (func as usize) < module.imports().len() {
            for &arg in args {
                self.push(arg)?;
            }
            return self.invoke_host(module, func);
        }

        let locals = usize::from(signature.params) + module.local_count(func) as usize;
        if locals > MAX_LOCAL_SLOTS {
            return Err(Trap::LocalOverflow);
        }
        for (slot, &arg) in args.iter().enumerate() {
            self.locals[slot] = arg;
        }
        for slot in args.len()..locals {
            self.locals[slot] = 0;
        }
        self.local_top = locals;
        self.frames[0] = CallFrame {
            func,
            pc: 0,
            local_base: 0,
            stack_base: 0,
            control_base: 0,
            results: signature.results,
        };
        self.fp = 1;
        self.execute(module)
    }

    fn execute(&mut self, module: &Module<'m>) -> Result<Option<i32>, Trap> {
        loop {
            let frame_index = self.fp - 1;
            let frame = self.frames[frame_index];
            let Some(body) = module.body(frame.func) else {
                return Err(Trap::BadModule);
            };
            let mut pc = frame.pc;
            let mut reload = false;

            while !reload {
                let Some(&opcode) = body.get(pc) else {
                    return Err(Trap::BadInstruction(0));
                };
                pc += 1;
                if self.fuel == 0 {
                    return Err(Trap::OutOfFuel);
                }
                self.fuel -= 1;

                match opcode {
                    0x00 => return Err(Trap::Unreachable),
                    0x01 => {}
                    0x02 => {
                        let block_type = read_block_type(body, &mut pc)?;
                        let end = scan_block(body, pc, false)?.1;
                        self.push_control(Kind::Block, pc, end, block_type)?;
                    }
                    0x03 => {
                        let block_type = read_block_type(body, &mut pc)?;
                        let end = scan_block(body, pc, false)?.1;
                        self.push_control(Kind::Loop, pc, end, block_type)?;
                    }
                    0x04 => {
                        let block_type = read_block_type(body, &mut pc)?;
                        let (else_at, end) = scan_block(body, pc, true)?;
                        let condition = self.pop()?;
                        self.push_control(Kind::If, pc, end, block_type)?;
                        if condition == 0 {
                            pc = match else_at {
                                Some(else_at) => else_at + 1,
                                None => end,
                            };
                        }
                    }
                    0x05 => {
                        let control = *self
                            .control
                            .get(self.cp.checked_sub(1).ok_or(Trap::BadBranch)?)
                            .ok_or(Trap::BadBranch)?;
                        self.settle(control.height, control.results)?;
                        pc = control.end;
                    }
                    0x0B => {
                        if self.cp == frame.control_base {
                            let result = self.leave(&frame)?;
                            if self.fp == 0 {
                                return Ok(result);
                            }
                            if let Some(value) = result {
                                self.push(value)?;
                            }
                            reload = true;
                        } else {
                            let control = *self
                                .control
                                .get(self.cp.checked_sub(1).ok_or(Trap::BadBranch)?)
                                .ok_or(Trap::BadBranch)?;
                            self.cp -= 1;
                            self.settle(control.height, control.results)?;
                        }
                    }
                    0x0C => {
                        let depth = read_uleb32(body, &mut pc)?;
                        if let Flow::Return(result) = self.branch(&frame, depth, &mut pc)? {
                            if self.fp == 0 {
                                return Ok(result);
                            }
                            if let Some(value) = result {
                                self.push(value)?;
                            }
                            reload = true;
                        }
                    }
                    0x0D => {
                        let depth = read_uleb32(body, &mut pc)?;
                        if self.pop()? != 0 {
                            if let Flow::Return(result) = self.branch(&frame, depth, &mut pc)? {
                                if self.fp == 0 {
                                    return Ok(result);
                                }
                                if let Some(value) = result {
                                    self.push(value)?;
                                }
                                reload = true;
                            }
                        }
                    }
                    0x0F => {
                        let result = self.leave(&frame)?;
                        if self.fp == 0 {
                            return Ok(result);
                        }
                        if let Some(value) = result {
                            self.push(value)?;
                        }
                        reload = true;
                    }
                    0x10 => {
                        let target = read_uleb32(body, &mut pc)?;
                        self.frames[frame_index].pc = pc;
                        if (target as usize) < module.imports().len() {
                            let result = self.invoke_host(module, target)?;
                            if let Some(value) = result {
                                self.push(value)?;
                            }
                        } else {
                            self.push_callee(module, target)?;
                            reload = true;
                        }
                    }
                    0x1A => {
                        self.pop()?;
                    }
                    0x1B => {
                        let condition = self.pop()?;
                        let right = self.pop()?;
                        let left = self.pop()?;
                        self.push(if condition != 0 { left } else { right })?;
                    }
                    0x20 => {
                        let index = read_uleb32(body, &mut pc)?;
                        let value = *self
                            .locals
                            .get(frame.local_base + index as usize)
                            .ok_or(Trap::LocalOverflow)?;
                        self.push(value)?;
                    }
                    0x21 => {
                        let index = read_uleb32(body, &mut pc)?;
                        let value = self.pop()?;
                        let slot = self
                            .locals
                            .get_mut(frame.local_base + index as usize)
                            .ok_or(Trap::LocalOverflow)?;
                        *slot = value;
                    }
                    0x22 => {
                        let index = read_uleb32(body, &mut pc)?;
                        let top = self.sp.checked_sub(1).ok_or(Trap::Underflow)?;
                        let value = *self.stack.get(top).ok_or(Trap::Underflow)?;
                        let slot = self
                            .locals
                            .get_mut(frame.local_base + index as usize)
                            .ok_or(Trap::LocalOverflow)?;
                        *slot = value;
                    }
                    0x41 => {
                        let value = read_sleb32(body, &mut pc)?;
                        self.push(value)?;
                    }
                    0x45 => {
                        let value = self.pop()?;
                        self.push(if value == 0 { 1 } else { 0 })?;
                    }
                    0x46..=0x4F => {
                        let right = self.pop()?;
                        let left = self.pop()?;
                        self.push(compare(opcode, left, right))?;
                    }
                    0x28 => {
                        let _align = read_uleb32(body, &mut pc)?;
                        let offset = read_uleb32(body, &mut pc)? as usize;
                        let address = self.pop()? as u32 as usize;
                        let at = address.checked_add(offset).ok_or(Trap::MemoryOutOfBounds)?;
                        let bytes = self.load(at)?;
                        self.push(i32::from_le_bytes(bytes))?;
                    }
                    0x36 => {
                        let _align = read_uleb32(body, &mut pc)?;
                        let offset = read_uleb32(body, &mut pc)? as usize;
                        let value = self.pop()?;
                        let address = self.pop()? as u32 as usize;
                        let at = address.checked_add(offset).ok_or(Trap::MemoryOutOfBounds)?;
                        self.store(at, value.to_le_bytes())?;
                    }
                    0x3F => {
                        if body.get(pc) != Some(&0) {
                            return Err(Trap::BadInstruction(0x3F));
                        }
                        pc += 1;
                        self.push(self.pages as i32)?;
                    }
                    0x6A..=0x76 => {
                        let right = self.pop()?;
                        let left = self.pop()?;
                        self.push(arithmetic(opcode, left, right)?)?;
                    }
                    _ => return Err(Trap::BadInstruction(opcode)),
                }
            }
        }
    }

    /// Pops the result of `frame` and drops its call state.
    fn leave(&mut self, frame: &CallFrame) -> Result<Option<i32>, Trap> {
        let result = if frame.results == 1 {
            Some(self.pop()?)
        } else {
            None
        };
        self.sp = frame.stack_base;
        self.local_top = frame.local_base;
        self.cp = frame.control_base;
        self.fp -= 1;
        Ok(result)
    }

    fn branch(&mut self, frame: &CallFrame, depth: u32, pc: &mut usize) -> Result<Flow, Trap> {
        let relative = self.cp - frame.control_base;
        let depth = depth as usize;
        if depth == relative {
            return Ok(Flow::Return(self.leave(frame)?));
        }
        let target = frame.control_base + relative.checked_sub(1 + depth).ok_or(Trap::BadBranch)?;
        let control = *self.control.get(target).ok_or(Trap::BadBranch)?;
        if control.kind == Kind::Loop {
            self.settle(control.height, 0)?;
            self.cp = target + 1;
            *pc = control.start;
        } else {
            self.settle(control.height, control.results)?;
            self.cp = target;
            // Resume after the `end` of the block: the frame was already
            // dropped here, so letting the `end` run would pop its parent.
            *pc = control.end + 1;
        }
        Ok(Flow::Continue)
    }

    /// Truncates the value stack to `height + results`, checking that the
    /// instructions really produced those values.
    fn settle(&mut self, height: usize, results: usize) -> Result<(), Trap> {
        if self.sp < height + results {
            return Err(Trap::Underflow);
        }
        self.sp = height + results;
        Ok(())
    }

    fn push_control(
        &mut self,
        kind: Kind,
        start: usize,
        end: usize,
        block_type: u8,
    ) -> Result<(), Trap> {
        let frame = ControlFrame {
            kind,
            start,
            end,
            height: self.sp,
            results: usize::from(block_type == 0x7F),
        };
        let slot = self.control.get_mut(self.cp).ok_or(Trap::StackOverflow)?;
        *slot = frame;
        self.cp += 1;
        Ok(())
    }

    fn push_callee(&mut self, module: &Module<'m>, func: u32) -> Result<(), Trap> {
        if self.fp >= MAX_CALL_DEPTH {
            return Err(Trap::CallDepth);
        }
        let signature = module.func_type(func).map_err(|_| Trap::BadModule)?;
        let locals = usize::from(signature.params) + module.local_count(func) as usize;
        if self.local_top + locals > MAX_LOCAL_SLOTS {
            return Err(Trap::LocalOverflow);
        }
        let base = self.local_top;
        for slot in (0..usize::from(signature.params)).rev() {
            self.locals[base + slot] = self.pop()?;
        }
        for slot in usize::from(signature.params)..locals {
            self.locals[base + slot] = 0;
        }
        let stack_base = self.sp;
        self.frames[self.fp] = CallFrame {
            func,
            pc: 0,
            local_base: base,
            stack_base,
            control_base: self.cp,
            results: signature.results,
        };
        self.local_top = base + locals;
        self.fp += 1;
        Ok(())
    }

    fn invoke_host(&mut self, module: &Module<'m>, func: u32) -> Result<Option<i32>, Trap> {
        let import = *module.imports().get(func as usize).ok_or(Trap::BadModule)?;
        if !self.token.allows(import.rights()) {
            return Err(Trap::Denied(import.host));
        }
        let signature = module.func_type(func).map_err(|_| Trap::BadModule)?;
        let mut args = [0i32; 8];
        for slot in (0..usize::from(signature.params)).rev() {
            args[slot] = self.pop()?;
        }
        match import.host {
            HostFunc::Log => {
                let len = args[1];
                if len < 0 {
                    return Err(Trap::MemoryOutOfBounds);
                }
                let start = args[0] as u32 as usize;
                let end = start
                    .checked_add(len as usize)
                    .ok_or(Trap::MemoryOutOfBounds)?;
                if end > self.memory_limit() || end > self.memory.len() {
                    return Err(Trap::MemoryOutOfBounds);
                }
                let cost = 1 + len as u32 / 64;
                if self.fuel < cost {
                    return Err(Trap::OutOfFuel);
                }
                self.fuel -= cost;
                let text = core::str::from_utf8(&self.memory[start..end])
                    .unwrap_or("<block log is not UTF-8>");
                self.env.log(text);
                Ok(None)
            }
            HostFunc::Exit => Err(Trap::Exited(args[0])),
            HostFunc::NowMs => Ok(Some(self.env.now_ms())),
            HostFunc::Yield => {
                self.env.idle();
                Ok(None)
            }
        }
    }

    fn memory_limit(&self) -> usize {
        self.pages as usize * PAGE_SIZE as usize
    }

    fn load(&self, at: usize) -> Result<[u8; 4], Trap> {
        let end = at.checked_add(4).ok_or(Trap::MemoryOutOfBounds)?;
        if end > self.memory_limit() {
            return Err(Trap::MemoryOutOfBounds);
        }
        let bytes = self.memory.get(at..end).ok_or(Trap::MemoryOutOfBounds)?;
        Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    fn store(&mut self, at: usize, bytes: [u8; 4]) -> Result<(), Trap> {
        let end = at.checked_add(4).ok_or(Trap::MemoryOutOfBounds)?;
        if end > self.memory_limit() {
            return Err(Trap::MemoryOutOfBounds);
        }
        let slot = self
            .memory
            .get_mut(at..end)
            .ok_or(Trap::MemoryOutOfBounds)?;
        slot.copy_from_slice(&bytes);
        Ok(())
    }

    fn push(&mut self, value: i32) -> Result<(), Trap> {
        let slot = self.stack.get_mut(self.sp).ok_or(Trap::StackOverflow)?;
        *slot = value;
        self.sp += 1;
        Ok(())
    }

    fn pop(&mut self) -> Result<i32, Trap> {
        self.sp = self.sp.checked_sub(1).ok_or(Trap::Underflow)?;
        Ok(self.stack[self.sp])
    }
}

/// Reads a block type byte: `0x40` (no result) or `0x7F` (`i32` result).
fn read_block_type(code: &[u8], pc: &mut usize) -> Result<u8, Trap> {
    let byte = *code.get(*pc).ok_or(Trap::BadInstruction(0))?;
    *pc += 1;
    Ok(byte)
}

/// Reads a `u32` LEB128 from an already validated body.
pub(crate) fn read_uleb32(code: &[u8], pc: &mut usize) -> Result<u32, Trap> {
    let mut result = 0u32;
    for shift in [0, 7, 14, 21, 28] {
        let byte = *code.get(*pc).ok_or(Trap::BadInstruction(0))?;
        *pc += 1;
        result |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err(Trap::BadInstruction(0))
}

/// Reads an `i32` LEB128 from an already validated body.
pub(crate) fn read_sleb32(code: &[u8], pc: &mut usize) -> Result<i32, Trap> {
    let mut result = 0i32;
    let mut shift = 0u32;
    loop {
        let byte = *code.get(*pc).ok_or(Trap::BadInstruction(0))?;
        *pc += 1;
        if shift == 28 {
            result |= i32::from(byte & 0x0F) << 28;
            return Ok(result);
        }
        result |= i32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            if byte & 0x40 != 0 {
                result |= !0 << (shift + 7);
            }
            return Ok(result);
        }
        shift += 7;
    }
}

/// Skips the immediates of one instruction.
fn skip_immediates(code: &[u8], opcode: u8, pc: &mut usize) -> Result<(), Trap> {
    match opcode {
        0x02..=0x04 => *pc += 1,
        0x0C | 0x0D | 0x10 | 0x20 | 0x21 | 0x22 => {
            read_uleb32(code, pc)?;
        }
        0x28 | 0x36 => {
            read_uleb32(code, pc)?;
            read_uleb32(code, pc)?;
        }
        0x3F => *pc += 1,
        0x41 => {
            read_sleb32(code, pc)?;
        }
        _ => {}
    }
    Ok(())
}

/// Finds the `else` (when `want_else`) and `end` of a block starting at `from`.
///
/// Validation already proved the block is balanced, so this is only used with
/// bodies from a parsed [`Module`].
fn scan_block(code: &[u8], from: usize, want_else: bool) -> Result<(Option<usize>, usize), Trap> {
    let mut pc = from;
    let mut depth = 0usize;
    let mut else_at = None;
    loop {
        let opcode = *code.get(pc).ok_or(Trap::BadInstruction(0))?;
        pc += 1;
        match opcode {
            0x02..=0x04 => {
                depth += 1;
                pc += 1;
            }
            0x05 if depth == 0 && want_else && else_at.is_none() => {
                else_at = Some(pc - 1);
            }
            0x0B => {
                if depth == 0 {
                    return Ok((else_at, pc - 1));
                }
                depth -= 1;
            }
            _ => skip_immediates(code, opcode, &mut pc)?,
        }
    }
}

fn compare(opcode: u8, left: i32, right: i32) -> i32 {
    let value = match opcode {
        0x46 => left == right,
        0x47 => left != right,
        0x48 => left < right,
        0x49 => (left as u32) < (right as u32),
        0x4A => left > right,
        0x4B => (left as u32) > (right as u32),
        0x4C => left <= right,
        0x4D => (left as u32) <= (right as u32),
        0x4E => left >= right,
        0x4F => (left as u32) >= (right as u32),
        _ => false,
    };
    if value {
        1
    } else {
        0
    }
}

fn arithmetic(opcode: u8, left: i32, right: i32) -> Result<i32, Trap> {
    let value = match opcode {
        0x6A => left.wrapping_add(right),
        0x6B => left.wrapping_sub(right),
        0x6C => left.wrapping_mul(right),
        0x6D => {
            if right == 0 {
                return Err(Trap::DivideByZero);
            }
            left.checked_div(right).ok_or(Trap::IntegerOverflow)?
        }
        0x6E => {
            if right == 0 {
                return Err(Trap::DivideByZero);
            }
            ((left as u32) / (right as u32)) as i32
        }
        0x6F => {
            if right == 0 {
                return Err(Trap::DivideByZero);
            }
            left.wrapping_rem(right)
        }
        0x70 => {
            if right == 0 {
                return Err(Trap::DivideByZero);
            }
            ((left as u32) % (right as u32)) as i32
        }
        0x71 => left & right,
        0x72 => left | right,
        0x73 => left ^ right,
        0x74 => left.wrapping_shl((right as u32) & 31),
        0x75 => left >> ((right as u32) & 31),
        0x76 => ((left as u32) >> ((right as u32) & 31)) as i32,
        _ => return Err(Trap::BadInstruction(opcode)),
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::Rights;
    use crate::test_asm as asm;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    struct Mock {
        logs: Vec<String>,
        now: i32,
        idle: u32,
    }

    impl Mock {
        fn new() -> Self {
            Self {
                logs: Vec::new(),
                now: 42,
                idle: 0,
            }
        }
    }

    impl Env for Mock {
        fn log(&mut self, text: &str) {
            self.logs.push(String::from(text));
        }

        fn now_ms(&mut self) -> i32 {
            self.now
        }

        fn idle(&mut self) {
            self.idle += 1;
        }
    }

    fn add_module() -> Vec<u8> {
        asm::module(&[
            asm::types(&[(2, 1)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("add", 0)]),
            asm::code(&[asm::body(&[], &[0x20, 0x00, 0x20, 0x01, 0x6A, 0x0B])]),
        ])
    }

    #[test]
    fn runs_arithmetic() {
        let bytes = add_module();
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(7, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "add", &[2, 40]), Ok(Some(42)));
        assert_eq!(host.pages(), 0);
        assert_eq!(host.token().block(), 7);
        assert!(host.fuel() < 1_000);
    }

    #[test]
    fn uses_locals() {
        let bytes = asm::module(&[
            asm::types(&[(2, 1)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("sub", 0)]),
            asm::code(&[asm::body(
                &[(1, 0x7F)],
                &[0x20, 0x00, 0x20, 0x01, 0x6B, 0x21, 0x02, 0x20, 0x02, 0x0B],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "sub", &[50, 8]), Ok(Some(42)));
    }

    #[test]
    fn loops_and_branches() {
        let bytes = asm::module(&[
            asm::types(&[(1, 1)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("sum", 0)]),
            asm::code(&[asm::body(
                &[(1, 0x7F)],
                &[
                    0x41, 0x00, // i32.const 0
                    0x21, 0x01, // local.set 1
                    0x02, 0x40, // block
                    0x03, 0x40, // loop
                    0x20, 0x00, // local.get 0
                    0x45, // i32.eqz
                    0x0D, 0x01, // br_if 1
                    0x20, 0x01, // local.get 1
                    0x20, 0x00, // local.get 0
                    0x6A, // i32.add
                    0x21, 0x01, // local.set 1
                    0x20, 0x00, // local.get 0
                    0x41, 0x01, // i32.const 1
                    0x6B, // i32.sub
                    0x21, 0x00, // local.set 0
                    0x0C, 0x00, // br 0
                    0x0B, // end loop
                    0x0B, // end block
                    0x20, 0x01, // local.get 1
                    0x0B, // end function
                ],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(
            &mut memory,
            Mock::new(),
            Token::new(0, Rights::NONE, 10_000),
        );
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "sum", &[5]), Ok(Some(15)));
        assert_eq!(host.call(&module, "sum", &[0]), Ok(Some(0)));
    }

    #[test]
    fn if_and_else() {
        let bytes = asm::module(&[
            asm::types(&[(1, 1)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("pick", 0)]),
            asm::code(&[asm::body(
                &[],
                &[
                    0x20, 0x00, // local.get 0
                    0x04, 0x7F, // if (result i32)
                    0x41, 0x0A, // i32.const 10
                    0x05, // else
                    0x41, 0x14, // i32.const 20
                    0x0B, // end if
                    0x0B, // end function
                ],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "pick", &[1]), Ok(Some(10)));
        assert_eq!(host.call(&module, "pick", &[0]), Ok(Some(20)));
    }

    #[test]
    fn calls_functions() {
        let bytes = asm::module(&[
            asm::types(&[(1, 1)]),
            asm::functions(&[0, 0]),
            asm::exports(&[asm::export_func("double", 0), asm::export_func("quad", 1)]),
            asm::code(&[
                asm::body(&[], &[0x20, 0x00, 0x20, 0x00, 0x6A, 0x0B]),
                asm::body(
                    &[],
                    &[
                        0x20, 0x00, // local.get 0
                        0x10, 0x00, // call double
                        0x20, 0x00, // local.get 0
                        0x10, 0x00, // call double
                        0x6A, // i32.add
                        0x0B,
                    ],
                ),
            ]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "double", &[21]), Ok(Some(42)));
        assert_eq!(host.call(&module, "quad", &[10]), Ok(Some(40)));
        assert_eq!(host.call_func(&module, 1, &[3]), Ok(Some(12)));
    }
    #[test]
    fn compares_and_selects() {
        let bytes = asm::module(&[
            asm::types(&[(2, 1)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("pick", 0)]),
            asm::code(&[asm::body(
                &[],
                &[
                    0x20, 0x00, // local.get 0
                    0x20, 0x01, // local.get 1
                    0x49, // i32.lt_u
                    0x20, 0x00, // local.get 0
                    0x20, 0x01, // local.get 1
                    0x46, // i32.eq
                    0x6A, // i32.add
                    0x20, 0x00, // local.get 0
                    0x20, 0x01, // local.get 1
                    0x20, 0x00, // local.get 0
                    0x1B, // i32.select
                    0x6A, // i32.add
                    0x0B,
                ],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "pick", &[5, 10]), Ok(Some(6)));
    }

    #[test]
    fn divides_with_traps() {
        let bytes = asm::module(&[
            asm::types(&[(2, 1)]),
            asm::functions(&[0, 0]),
            asm::exports(&[asm::export_func("div", 0), asm::export_func("rem", 1)]),
            asm::code(&[
                asm::body(&[], &[0x20, 0x00, 0x20, 0x01, 0x6D, 0x0B]),
                asm::body(&[], &[0x20, 0x00, 0x20, 0x01, 0x6F, 0x0B]),
            ]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "div", &[7, 2]), Ok(Some(3)));
        assert_eq!(host.call(&module, "rem", &[-7, 2]), Ok(Some(-1)));
        assert_eq!(host.call(&module, "div", &[1, 0]), Err(Trap::DivideByZero));
        assert_eq!(host.call(&module, "rem", &[1, 0]), Err(Trap::DivideByZero));
        assert_eq!(
            host.call(&module, "div", &[i32::MIN, -1]),
            Err(Trap::IntegerOverflow)
        );
    }

    #[test]
    fn loads_and_stores_memory() {
        let bytes = asm::module(&[
            asm::types(&[(0, 1)]),
            asm::functions(&[0]),
            asm::memory(1),
            asm::exports(&[asm::export_func("run", 0)]),
            asm::data(&[asm::data_active(4, &[0x2A, 0x00, 0x00, 0x00])]),
            asm::code(&[asm::body(
                &[],
                &[
                    0x41, 0x08, // i32.const 8
                    0x41, 0xE4, 0x00, // i32.const 100
                    0x36, 0x02, 0x00, // i32.store
                    0x41, 0x08, // i32.const 8
                    0x28, 0x02, 0x00, // i32.load
                    0x41, 0x04, // i32.const 4
                    0x28, 0x02, 0x00, // i32.load
                    0x6A, // i32.add
                    0x0B,
                ],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = alloc::vec![0u8; 1 << 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "run", &[]), Ok(Some(142)));
        assert_eq!(host.pages(), 1);
        assert_eq!(&host.memory()[8..12], &100i32.to_le_bytes());
        assert_eq!(&host.memory()[4..8], &42i32.to_le_bytes());
    }

    #[test]
    fn memory_access_is_bounds_checked() {
        let mut code = asm::i32_const(65_534);
        code.extend_from_slice(&[0x28, 0x02, 0x00, 0x0B]);
        let bytes = asm::module(&[
            asm::types(&[(0, 1)]),
            asm::functions(&[0]),
            asm::memory(1),
            asm::exports(&[asm::export_func("run", 0)]),
            asm::code(&[asm::body(&[], &code)]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = alloc::vec![0u8; 1 << 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "run", &[]), Err(Trap::MemoryOutOfBounds));
    }

    #[test]
    fn memory_too_small_is_rejected() {
        let bytes = asm::module(&[asm::memory(1)]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        assert_eq!(
            host.instantiate(&module),
            Err(Trap::MemoryTooSmall {
                required: 65_536,
                provided: 16
            })
        );
    }

    #[test]
    fn runs_the_start_function() {
        let bytes = asm::module(&[
            asm::types(&[(0, 0)]),
            asm::functions(&[0]),
            asm::memory(1),
            asm::start(0),
            asm::code(&[asm::body(
                &[],
                &[
                    0x41, 0x00, // i32.const 0
                    0x41, 0x07, // i32.const 7
                    0x36, 0x02, 0x00, // i32.store
                    0x0B,
                ],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        assert_eq!(module.start(), Some(0));
        let mut memory = alloc::vec![0u8; 1 << 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(&host.memory()[0..4], &7i32.to_le_bytes());
    }

    fn log_module() -> Vec<u8> {
        asm::module(&[
            asm::types(&[(2, 0), (0, 0)]),
            asm::imports(&[asm::import_func("aios", "log", 0)]),
            asm::functions(&[1]),
            asm::memory(1),
            asm::exports(&[asm::export_func("run", 1)]),
            asm::data(&[asm::data_active(8, b"hello")]),
            asm::code(&[asm::body(
                &[],
                &[
                    0x41, 0x08, // i32.const 8
                    0x41, 0x05, // i32.const 5
                    0x10, 0x00, // call aios.log
                    0x0B,
                ],
            )]),
        ])
    }

    #[test]
    fn logs_through_the_host_abi() {
        let bytes = log_module();
        let module = Module::parse(&bytes).unwrap();
        assert_eq!(module.required_rights(), Rights::LOG);
        let mut memory = alloc::vec![0u8; 1 << 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::LOG, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "run", &[]), Ok(None));
        assert_eq!(host.env().logs, alloc::vec![String::from("hello")]);
    }

    #[test]
    fn denies_imports_without_the_right() {
        let bytes = log_module();
        let module = Module::parse(&bytes).unwrap();
        let mut memory = alloc::vec![0u8; 1 << 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(1, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(
            host.call(&module, "run", &[]),
            Err(Trap::Denied(HostFunc::Log))
        );
        assert!(host.env().logs.is_empty());
    }

    fn clock_module() -> Vec<u8> {
        asm::module(&[
            asm::types(&[(0, 1), (0, 0)]),
            asm::imports(&[
                asm::import_func("aios", "now_ms", 0),
                asm::import_func("aios", "yield", 1),
            ]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("run", 2)]),
            asm::code(&[asm::body(&[], &[0x10, 0x00, 0x10, 0x01, 0x0B])]),
        ])
    }

    #[test]
    fn reads_the_clock_and_yields() {
        let bytes = clock_module();
        let module = Module::parse(&bytes).unwrap();
        assert_eq!(module.required_rights(), Rights::CLOCK.union(Rights::YIELD));
        let mut memory = [0u8; 16];
        let mut host = Host::new(
            &mut memory,
            Mock::new(),
            Token::new(0, Rights::CLOCK.union(Rights::YIELD), 1_000),
        );
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "run", &[]), Ok(Some(42)));
        assert_eq!(host.env().idle, 1);
    }

    #[test]
    fn a_missing_right_stops_the_block() {
        let bytes = clock_module();
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(
            &mut memory,
            Mock::new(),
            Token::new(0, Rights::CLOCK, 1_000),
        );
        host.instantiate(&module).unwrap();
        assert_eq!(
            host.call(&module, "run", &[]),
            Err(Trap::Denied(HostFunc::Yield))
        );
    }

    #[test]
    fn stops_on_fuel() {
        let bytes = asm::module(&[
            asm::types(&[(0, 0)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("spin", 0)]),
            asm::code(&[asm::body(
                &[],
                &[
                    0x03, 0x40, // loop
                    0x0C, 0x00, // br 0
                    0x0B, // end loop
                    0x0B, // end function
                ],
            )]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 64));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "spin", &[]), Err(Trap::OutOfFuel));
        assert_eq!(host.fuel(), 0);
    }

    #[test]
    fn exits_with_a_code() {
        let bytes = asm::module(&[
            asm::types(&[(1, 0)]),
            asm::imports(&[asm::import_func("aios", "exit", 0)]),
            asm::exports(&[asm::export_func("run", 0)]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::EXIT, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "run", &[3]), Err(Trap::Exited(3)));
    }

    #[test]
    fn reports_missing_exports_and_arguments() {
        let bytes = add_module();
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "nope", &[]), Err(Trap::MissingExport));
        assert_eq!(
            host.call(&module, "add", &[1]),
            Err(Trap::BadArgs {
                expected: 2,
                got: 1
            })
        );
    }

    #[test]
    fn rejects_unreachable() {
        let bytes = asm::module(&[
            asm::types(&[(0, 0)]),
            asm::functions(&[0]),
            asm::exports(&[asm::export_func("run", 0)]),
            asm::code(&[asm::body(&[], &[0x00, 0x0B])]),
        ]);
        let module = Module::parse(&bytes).unwrap();
        let mut memory = [0u8; 16];
        let mut host = Host::new(&mut memory, Mock::new(), Token::new(0, Rights::NONE, 1_000));
        host.instantiate(&module).unwrap();
        assert_eq!(host.call(&module, "run", &[]), Err(Trap::Unreachable));
    }

    #[test]
    fn traps_explain_themselves() {
        assert_eq!(format!("{}", Trap::OutOfFuel), "out of fuel");
        assert_eq!(
            format!("{}", Trap::Denied(HostFunc::Log)),
            "capability denied for aios.log"
        );
        assert_eq!(
            format!(
                "{}",
                Trap::MemoryTooSmall {
                    required: 65_536,
                    provided: 16
                }
            ),
            "needs 65536 byte(s) of memory, got 16"
        );
    }
}
