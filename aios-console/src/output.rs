use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Result code of a console command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    /// Command completed successfully.
    #[default]
    Ok = 0,
    /// Wrong or missing arguments.
    Usage = 1,
    /// Required capability was not granted.
    Denied = 2,
    /// Command is not implemented by the active backend.
    Unavailable = 3,
    /// Backend reported a runtime failure.
    BackendError = 4,
}

impl ExitCode {
    /// Numeric code used by scripts and by the console prompt.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Short status label (`OK` or `ERR`).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            _ => "ERR",
        }
    }
}

/// Side effect the frontend must apply after printing the output lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputControl {
    /// No side effect.
    #[default]
    None,
    /// Clear the visible output and its scrollback before rendering.
    Clear,
    /// Close the console with the given process exit code.
    Exit(u8),
}

/// Text-only result of one command invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Output lines in display order; never contains widgets or ANSI art.
    pub lines: Vec<String>,
    /// Result code of the invocation.
    pub code: ExitCode,
    /// Wall-clock milliseconds measured by the frontend; the core leaves it at zero.
    pub duration_ms: u64,
    /// Optional frontend control action.
    pub control: OutputControl,
}

impl CommandOutput {
    /// Builds a successful result from a list of lines.
    pub fn ok(lines: Vec<String>) -> Self {
        Self {
            lines,
            code: ExitCode::Ok,
            ..Self::default()
        }
    }

    /// Builds a successful result from a single line.
    pub fn text(line: impl Into<String>) -> Self {
        Self::ok(alloc::vec![line.into()])
    }

    /// Builds a usage error. `usage` must be a canonical usage string from the registry.
    pub fn usage(usage: &str) -> Self {
        Self {
            lines: alloc::vec![alloc::format!("ERR usage: {usage}")],
            code: ExitCode::Usage,
            ..Self::default()
        }
    }

    /// Builds a capability denial error.
    pub fn denied(message: &str) -> Self {
        Self {
            lines: alloc::vec![alloc::format!("ERR denied: {message}")],
            code: ExitCode::Denied,
            ..Self::default()
        }
    }

    /// Builds a backend/unavailable error.
    pub fn unavailable(message: &str) -> Self {
        Self {
            lines: alloc::vec![alloc::format!("ERR unavailable: {message}")],
            code: ExitCode::Unavailable,
            ..Self::default()
        }
    }

    /// Builds a backend runtime error.
    pub fn error(message: &str) -> Self {
        Self {
            lines: alloc::vec![alloc::format!("ERR backend: {message}")],
            code: ExitCode::BackendError,
            ..Self::default()
        }
    }

    /// Builds a screen-clear control result.
    pub fn clear() -> Self {
        Self {
            control: OutputControl::Clear,
            ..Self::default()
        }
    }

    /// Builds an exit control result.
    pub fn exit(code: u8) -> Self {
        Self {
            control: OutputControl::Exit(code),
            ..Self::default()
        }
    }

    /// Returns whether the command succeeded.
    pub fn is_ok(&self) -> bool {
        self.code == ExitCode::Ok
    }

    /// Renders the one-line status shown after the output.
    pub fn status_line(&self) -> String {
        alloc::format!("{} in {} ms", self.code.label(), self.duration_ms)
    }

    /// Appends one line to a successful result.
    pub fn push_line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    /// Creates an empty successful result for backends that fill lines later.
    pub fn builder() -> Self {
        Self::default()
    }

    /// Converts the lines into a single string with trailing newlines.
    pub fn as_text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Returns a string suitable for `Display` in tests and logs.
    pub fn to_test_string(&self) -> String {
        self.as_text() + &self.status_line()
    }
}

impl From<&str> for CommandOutput {
    fn from(value: &str) -> Self {
        Self::text(value.to_string())
    }
}
