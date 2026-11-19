use crate::capability::{Capability, Scope};

/// Top-level namespace of a command in generated help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandGroup {
    Session,
    System,
    Process,
    Block,
    Ipc,
    Watchdog,
    Security,
    Hardware,
    Filesystem,
    Network,
    Diagnostics,
    Store,
    Runtime,
    Bridge,
}

impl CommandGroup {
    /// Stable lowercase name used in `help <group>`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::System => "system",
            Self::Process => "process",
            Self::Block => "block",
            Self::Ipc => "ipc",
            Self::Watchdog => "watchdog",
            Self::Security => "security",
            Self::Hardware => "hardware",
            Self::Filesystem => "filesystem",
            Self::Network => "network",
            Self::Diagnostics => "diagnostics",
            Self::Store => "store",
            Self::Runtime => "runtime",
            Self::Bridge => "bridge",
        }
    }
}

/// Every group in display order.
pub const GROUPS: &[CommandGroup] = &[
    CommandGroup::Session,
    CommandGroup::System,
    CommandGroup::Process,
    CommandGroup::Block,
    CommandGroup::Ipc,
    CommandGroup::Watchdog,
    CommandGroup::Security,
    CommandGroup::Hardware,
    CommandGroup::Filesystem,
    CommandGroup::Network,
    CommandGroup::Diagnostics,
    CommandGroup::Store,
    CommandGroup::Runtime,
    CommandGroup::Bridge,
];

/// Immutable declaration of one canonical command.
#[derive(Debug)]
pub struct CommandSpec {
    /// Canonical lowercase identifier, unique across the registry.
    pub id: &'static str,
    /// Help namespace.
    pub group: CommandGroup,
    /// Backends that implement this command.
    pub scope: Scope,
    /// Canonical usage string printed on argument errors.
    pub usage: &'static str,
    /// One-line description printed by `help`.
    pub summary: &'static str,
    /// Declared subcommands; an empty slice means the command takes free arguments.
    pub subcommands: &'static [&'static str],
    /// Capabilities required before the backend is called.
    pub capabilities: &'static [Capability],
    /// Whether an unknown first argument is accepted as a free argument.
    pub allow_bare_args: bool,
}

impl CommandSpec {
    /// Formats the help line for this spec.
    pub fn help_line(&self) -> alloc::string::String {
        alloc::format!(
            "  {:<12} {} {:<48} {}",
            self.id,
            self.scope.tag(),
            self.usage,
            self.summary
        )
    }

    /// Returns whether `name` is a declared subcommand.
    pub fn has_subcommand(&self, name: &str) -> bool {
        self.subcommands.contains(&name)
    }
}
