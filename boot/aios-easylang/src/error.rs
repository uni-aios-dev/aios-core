//! A compile error with a source position.

use alloc::string::String;
use core::fmt;

/// A compile error: a human-readable message and its `1`-based position.
///
/// The column counts bytes on the line, which is exact for the ASCII syntax and
/// the first byte of a multi-byte string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// `1`-based line of the offending token.
    pub line: usize,
    /// `1`-based byte column of the offending token.
    pub column: usize,
    /// What went wrong, in lowercase, without a trailing period.
    pub message: String,
}

impl Error {
    /// Builds an error at `line:column`.
    pub(crate) fn new(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

impl core::error::Error for Error {}
