//! The EasyLang syntax tree: statements and `i32` expressions.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// A prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnOp {
    /// `-x`, compiled as `0 - x`.
    Negate,
    /// `not x`, compiled as `x == 0`.
    Not,
}

/// A binary operator. Every operator works on `i32`; comparisons and the
/// logical operators produce `0` or `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// An expression with its source position.
#[derive(Debug)]
pub(crate) struct Expr {
    /// What the expression is.
    pub kind: ExprKind,
    /// `1`-based source line.
    pub line: usize,
    /// `1`-based source column.
    pub column: usize,
}

/// The shape of an [`Expr`].
#[derive(Debug)]
pub(crate) enum ExprKind {
    /// An `i32` literal, also used for `true` (`1`) and `false` (`0`).
    Int(i32),
    /// A local variable.
    Var(String),
    /// `now_ms()`, the `aios.now_ms` host import.
    NowMs,
    /// A prefix operator.
    Unary(UnOp, Box<Expr>),
    /// A binary operator.
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

/// A statement with its source position.
#[derive(Debug)]
pub(crate) struct Stmt {
    /// What the statement does.
    pub kind: StmtKind,
    /// `1`-based source line.
    pub line: usize,
    /// `1`-based source column.
    pub column: usize,
}

/// The shape of a [`Stmt`].
#[derive(Debug)]
pub(crate) enum StmtKind {
    /// `let name = expr`.
    Let(String, Expr),
    /// `name = expr`.
    Assign(String, Expr),
    /// `log "text"`.
    Log(String),
    /// `exit expr`, the `aios.exit` host import.
    Exit(Expr),
    /// `yield`, the `aios.yield` host import.
    Yield,
    /// `if cond { .. } else { .. }`; the `else` branch may be empty.
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    /// `while cond { .. }`.
    While(Expr, Vec<Stmt>),
}

/// A whole program: the statements of the exported `run` function.
#[derive(Debug)]
pub(crate) struct Program {
    /// Top-level statements, in source order.
    pub statements: Vec<Stmt>,
}
