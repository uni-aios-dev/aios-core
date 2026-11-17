//! The code generator: [`Program`] to the WebAssembly subset of `aios-wasmhost`.
//!
//! Generation runs in two passes over the same tree:
//!
//! 1. analysis resolves variables, collects the host imports the program uses
//!    and packs every `log` string into one blob that has to fit in a single
//!    page;
//! 2. emission walks the tree again in the same order and writes the sections,
//!    reusing the positions recorded by the first pass.
//!
//! The module always exports `run` with no parameters and no result. A bare
//! `return` after `aios.exit` is emitted for clarity only: the host traps with
//! [`aios_wasmhost::Trap::Exited`] instead of returning, so nothing follows it.

use crate::ast::{BinOp, Expr, ExprKind, Program, Stmt, StmtKind, UnOp};
use crate::error::Error;
use aios_wasmhost::module::{MAX_CONTROL_FRAMES, MAX_LOCALS};
use aios_wasmhost::PAGE_SIZE;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Which host imports a program needs.
#[derive(Debug, Default, Clone, Copy)]
struct Used {
    log: bool,
    exit: bool,
    now_ms: bool,
    idle: bool,
}

/// One declared variable and its local slot.
#[derive(Debug)]
struct Local {
    name: String,
    slot: u32,
}

/// Everything the analysis pass gives the emitter.
#[derive(Debug)]
struct Plan {
    /// Variables in declaration order; the position is the local index.
    locals: Vec<Local>,
    /// Where each `log` string starts inside [`Plan::blob`], in source order.
    log_offsets: Vec<u32>,
    /// All string literals, laid out in one data segment.
    blob: Vec<u8>,
    /// The host imports the program uses.
    used: Used,
}

impl Plan {
    fn find(&self, name: &str) -> Option<u32> {
        self.locals
            .iter()
            .find(|local| local.name == name)
            .map(|local| local.slot)
    }
}

/// Compiles a parsed program into a complete module.
pub(crate) fn compile(program: &Program) -> Result<Vec<u8>, Error> {
    let plan = analyze(program)?;
    emit(program, &plan)
}

fn analyze(program: &Program) -> Result<Plan, Error> {
    let mut analyzer = Analyzer {
        plan: Plan {
            locals: Vec::new(),
            log_offsets: Vec::new(),
            blob: Vec::new(),
            used: Used::default(),
        },
    };
    analyzer.statements(&program.statements)?;
    Ok(analyzer.plan)
}

struct Analyzer {
    plan: Plan,
}

impl Analyzer {
    fn statements(&mut self, statements: &[Stmt]) -> Result<(), Error> {
        for statement in statements {
            self.statement(statement)?;
        }
        Ok(())
    }

    fn statement(&mut self, statement: &Stmt) -> Result<(), Error> {
        match &statement.kind {
            StmtKind::Let(name, value) => {
                if self.plan.find(name).is_some() {
                    return Err(Error::new(
                        statement.line,
                        statement.column,
                        format!("variable '{name}' is declared twice"),
                    ));
                }
                self.expr(value)?;
                if self.plan.locals.len() as u32 >= MAX_LOCALS {
                    return Err(Error::new(
                        statement.line,
                        statement.column,
                        format!("too many variables (at most {MAX_LOCALS})"),
                    ));
                }
                let slot = self.plan.locals.len() as u32;
                self.plan.locals.push(Local {
                    name: name.clone(),
                    slot,
                });
            }
            StmtKind::Assign(name, value) => {
                self.expr(value)?;
                if self.plan.find(name).is_none() {
                    return Err(Error::new(
                        statement.line,
                        statement.column,
                        format!("cannot assign to unknown variable '{name}'"),
                    ));
                }
            }
            StmtKind::Log(text) => {
                if self.plan.blob.len() + text.len() > PAGE_SIZE as usize {
                    return Err(Error::new(
                        statement.line,
                        statement.column,
                        "the logged text does not fit in one memory page (65536 bytes)",
                    ));
                }
                self.plan.used.log = true;
                self.plan.log_offsets.push(self.plan.blob.len() as u32);
                self.plan.blob.extend_from_slice(text.as_bytes());
            }
            StmtKind::Exit(value) => {
                self.plan.used.exit = true;
                self.expr(value)?;
            }
            StmtKind::Yield => self.plan.used.idle = true,
            StmtKind::If(condition, then, otherwise) => {
                self.expr(condition)?;
                self.statements(then)?;
                self.statements(otherwise)?;
            }
            StmtKind::While(condition, body) => {
                self.expr(condition)?;
                self.statements(body)?;
            }
        }
        Ok(())
    }

    fn expr(&mut self, expr: &Expr) -> Result<(), Error> {
        match &expr.kind {
            ExprKind::Int(_) => {}
            ExprKind::Var(name) => {
                if self.plan.find(name).is_none() {
                    return Err(Error::new(
                        expr.line,
                        expr.column,
                        format!("unknown variable '{name}'"),
                    ));
                }
            }
            ExprKind::NowMs => self.plan.used.now_ms = true,
            ExprKind::Unary(_, operand) => self.expr(operand)?,
            ExprKind::Binary(_, left, right) => {
                self.expr(left)?;
                self.expr(right)?;
            }
        }
        Ok(())
    }
}

/// Types and imports, interned by signature.
struct Table {
    /// `(params, results)`, `i32` only; index 0 is the `run` signature.
    types: Vec<(u8, u8)>,
    /// `(import name, type index)`, in function index order.
    imports: Vec<(String, u32)>,
}

impl Table {
    fn intern(&mut self, params: u8, results: u8) -> u32 {
        if let Some(index) = self
            .types
            .iter()
            .position(|&signature| signature == (params, results))
        {
            return index as u32;
        }
        self.types.push((params, results));
        (self.types.len() - 1) as u32
    }

    /// Adds an `aios.<name>` import and returns its function index.
    fn import(&mut self, name: &str, params: u8, results: u8) -> u32 {
        let type_index = self.intern(params, results);
        self.imports.push((String::from(name), type_index));
        (self.imports.len() - 1) as u32
    }
}

/// The function indices of the imports a program uses.
#[derive(Debug, Default, Clone, Copy)]
struct Indexes {
    log: Option<u32>,
    exit: Option<u32>,
    now_ms: Option<u32>,
    idle: Option<u32>,
}

fn emit(program: &Program, plan: &Plan) -> Result<Vec<u8>, Error> {
    let mut table = Table {
        types: alloc::vec![(0, 0)],
        imports: Vec::new(),
    };
    let mut indexes = Indexes::default();
    if plan.used.log {
        indexes.log = Some(table.import("log", 2, 0));
    }
    if plan.used.exit {
        indexes.exit = Some(table.import("exit", 1, 0));
    }
    if plan.used.now_ms {
        indexes.now_ms = Some(table.import("now_ms", 0, 1));
    }
    if plan.used.idle {
        indexes.idle = Some(table.import("yield", 0, 0));
    }
    // `run` is the only defined function and comes after the imports.
    let run = table.imports.len() as u32;

    let mut emitter = Emitter {
        code: Vec::new(),
        plan,
        indexes,
        log_cursor: 0,
        depth: 0,
    };
    emitter.statements(&program.statements)?;
    emitter.code.push(0x0B);

    let mut body = Vec::new();
    if plan.locals.is_empty() {
        body.push(0x00);
    } else {
        body.push(0x01);
        uleb(&mut body, plan.locals.len() as u32);
        body.push(0x7F);
    }
    body.extend_from_slice(&emitter.code);

    let mut types = Vec::new();
    uleb(&mut types, table.types.len() as u32);
    for &(params, results) in &table.types {
        types.push(0x60);
        uleb(&mut types, u32::from(params));
        types.extend(core::iter::repeat_n(0x7F, usize::from(params)));
        uleb(&mut types, u32::from(results));
        types.extend(core::iter::repeat_n(0x7F, usize::from(results)));
    }

    let mut imports = Vec::new();
    uleb(&mut imports, table.imports.len() as u32);
    for (name, type_index) in &table.imports {
        name_section(&mut imports, "aios");
        name_section(&mut imports, name);
        imports.push(0x00);
        uleb(&mut imports, *type_index);
    }

    let mut functions = Vec::new();
    uleb(&mut functions, 1);
    uleb(&mut functions, 0);

    let mut exports = Vec::new();
    uleb(&mut exports, 1);
    name_section(&mut exports, "run");
    exports.push(0x00);
    uleb(&mut exports, run);

    let mut code = Vec::new();
    uleb(&mut code, 1);
    uleb(&mut code, body.len() as u32);
    code.extend_from_slice(&body);

    let mut out = Vec::new();
    out.extend_from_slice(b"\0asm");
    out.extend_from_slice(&[1, 0, 0, 0]);
    push_section(&mut out, 1, &types);
    if !imports.is_empty() {
        push_section(&mut out, 2, &imports);
    }
    push_section(&mut out, 3, &functions);
    if !plan.blob.is_empty() {
        // One memory, no maximum, one page.
        push_section(&mut out, 5, &[0x01, 0x00, 0x01]);
    }
    push_section(&mut out, 7, &exports);
    push_section(&mut out, 10, &code);
    if !plan.blob.is_empty() {
        let mut data = Vec::new();
        uleb(&mut data, 1);
        data.push(0x00);
        data.push(0x41);
        sleb(&mut data, 0);
        data.push(0x0B);
        uleb(&mut data, plan.blob.len() as u32);
        data.extend_from_slice(&plan.blob);
        push_section(&mut out, 11, &data);
    }
    Ok(out)
}

struct Emitter<'a> {
    code: Vec<u8>,
    plan: &'a Plan,
    indexes: Indexes,
    log_cursor: usize,
    depth: usize,
}

impl Emitter<'_> {
    fn statements(&mut self, statements: &[Stmt]) -> Result<(), Error> {
        for statement in statements {
            self.statement(statement)?;
        }
        Ok(())
    }

    fn statement(&mut self, statement: &Stmt) -> Result<(), Error> {
        match &statement.kind {
            StmtKind::Let(name, value) | StmtKind::Assign(name, value) => {
                let slot = self.local(name, statement.line, statement.column)?;
                self.expr(value)?;
                self.code.push(0x21);
                uleb(&mut self.code, slot);
            }
            StmtKind::Log(text) => {
                let offset = *self.plan.log_offsets.get(self.log_cursor).ok_or_else(|| {
                    Error::new(
                        statement.line,
                        statement.column,
                        "internal error: missing log offset",
                    )
                })?;
                self.log_cursor += 1;
                let index = required(self.indexes.log, statement.line, statement.column, "log")?;
                self.code.push(0x41);
                sleb(&mut self.code, offset as i32);
                self.code.push(0x41);
                sleb(&mut self.code, text.len() as i32);
                self.code.push(0x10);
                uleb(&mut self.code, index);
            }
            StmtKind::Exit(value) => {
                let index = required(self.indexes.exit, statement.line, statement.column, "exit")?;
                self.expr(value)?;
                self.code.push(0x10);
                uleb(&mut self.code, index);
                self.code.push(0x0F);
            }
            StmtKind::Yield => {
                let index = required(self.indexes.idle, statement.line, statement.column, "yield")?;
                self.code.push(0x10);
                uleb(&mut self.code, index);
            }
            StmtKind::If(condition, then, otherwise) => {
                if self.depth >= MAX_CONTROL_FRAMES {
                    return Err(Error::new(
                        statement.line,
                        statement.column,
                        format!("blocks are nested too deeply (at most {MAX_CONTROL_FRAMES})"),
                    ));
                }
                self.expr(condition)?;
                self.code.extend_from_slice(&[0x04, 0x40]);
                self.depth += 1;
                self.statements(then)?;
                if !otherwise.is_empty() {
                    self.code.push(0x05);
                    self.statements(otherwise)?;
                }
                self.code.push(0x0B);
                self.depth -= 1;
            }
            StmtKind::While(condition, body) => {
                if self.depth + 2 > MAX_CONTROL_FRAMES {
                    return Err(Error::new(
                        statement.line,
                        statement.column,
                        format!("blocks are nested too deeply (at most {MAX_CONTROL_FRAMES})"),
                    ));
                }
                // block { loop { cond; eqz; br_if 1; body; br 0 } }
                self.code.extend_from_slice(&[0x02, 0x40, 0x03, 0x40]);
                self.depth += 2;
                self.expr(condition)?;
                self.code.push(0x45);
                self.code.push(0x0D);
                uleb(&mut self.code, 1);
                self.statements(body)?;
                self.code.push(0x0C);
                uleb(&mut self.code, 0);
                self.code.extend_from_slice(&[0x0B, 0x0B]);
                self.depth -= 2;
            }
        }
        Ok(())
    }

    fn expr(&mut self, expr: &Expr) -> Result<(), Error> {
        match &expr.kind {
            ExprKind::Int(value) => {
                self.code.push(0x41);
                sleb(&mut self.code, *value);
            }
            ExprKind::Var(name) => {
                let slot = self.local(name, expr.line, expr.column)?;
                self.code.push(0x20);
                uleb(&mut self.code, slot);
            }
            ExprKind::NowMs => {
                let index = required(self.indexes.now_ms, expr.line, expr.column, "now_ms")?;
                self.code.push(0x10);
                uleb(&mut self.code, index);
            }
            ExprKind::Unary(UnOp::Negate, operand) => {
                self.code.push(0x41);
                sleb(&mut self.code, 0);
                self.expr(operand)?;
                self.code.push(0x6B);
            }
            ExprKind::Unary(UnOp::Not, operand) => {
                self.expr(operand)?;
                self.code.push(0x45);
            }
            ExprKind::Binary(BinOp::And, left, right) => {
                self.boolean(left)?;
                self.boolean(right)?;
                self.code.push(0x71);
            }
            ExprKind::Binary(BinOp::Or, left, right) => {
                self.boolean(left)?;
                self.boolean(right)?;
                self.code.push(0x72);
            }
            ExprKind::Binary(op, left, right) => {
                self.expr(left)?;
                self.expr(right)?;
                self.code.push(opcode(*op));
            }
        }
        Ok(())
    }

    /// Emits `expr` normalized to `0` or `1`, the operands `and`/`or` need.
    fn boolean(&mut self, expr: &Expr) -> Result<(), Error> {
        self.expr(expr)?;
        self.code.push(0x45);
        self.code.push(0x45);
        Ok(())
    }

    fn local(&self, name: &str, line: usize, column: usize) -> Result<u32, Error> {
        self.plan.find(name).ok_or_else(|| {
            Error::new(
                line,
                column,
                format!("internal error: unknown variable '{name}'"),
            )
        })
    }
}

fn required(slot: Option<u32>, line: usize, column: usize, name: &str) -> Result<u32, Error> {
    slot.ok_or_else(|| {
        Error::new(
            line,
            column,
            format!("internal error: missing host import '{name}'"),
        )
    })
}

/// The opcode for a binary operator, all `i32`.
fn opcode(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0x6A,
        BinOp::Sub => 0x6B,
        BinOp::Mul => 0x6C,
        BinOp::Div => 0x6D,
        BinOp::Rem => 0x6F,
        BinOp::Eq => 0x46,
        BinOp::NotEq => 0x47,
        BinOp::Lt => 0x48,
        BinOp::Le => 0x4C,
        BinOp::Gt => 0x4A,
        BinOp::Ge => 0x4E,
        BinOp::And | BinOp::Or => {
            unreachable!("logical operators are emitted by Emitter::expr")
        }
    }
}

fn push_section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    out.push(id);
    uleb(out, payload.len() as u32);
    out.extend_from_slice(payload);
}

fn name_section(out: &mut Vec<u8>, text: &str) {
    uleb(out, text.len() as u32);
    out.extend_from_slice(text.as_bytes());
}

fn uleb(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn sleb(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        let sign = byte & 0x40 != 0;
        if (value == 0 && !sign) || (value == -1 && sign) {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_uleb128() {
        let mut bytes = Vec::new();
        for (value, expected) in [
            (0, &[0x00][..]),
            (127, &[0x7F][..]),
            (128, &[0x80, 0x01][..]),
            (624_485, &[0xE5, 0x8E, 0x26][..]),
        ] {
            bytes.clear();
            uleb(&mut bytes, value);
            assert_eq!(bytes, expected, "uleb {value}");
        }
    }

    #[test]
    fn encodes_sleb128() {
        let mut bytes = Vec::new();
        for (value, expected) in [
            (0, &[0x00][..]),
            (-1, &[0x7F][..]),
            (63, &[0x3F][..]),
            (64, &[0xC0, 0x00][..]),
            (-64, &[0x40][..]),
            (-65, &[0xBF, 0x7F][..]),
            (i32::MIN, &[0x80, 0x80, 0x80, 0x80, 0x78][..]),
        ] {
            bytes.clear();
            sleb(&mut bytes, value);
            assert_eq!(bytes, expected, "sleb {value}");
        }
    }
}
