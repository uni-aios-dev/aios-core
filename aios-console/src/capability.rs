use alloc::collections::BTreeSet;

/// Execution environment a command can run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Ordinary host process with `std`, filesystem and bridge access.
    Host,
    /// Freestanding `no_std` kernel environment.
    Kernel,
}

impl BackendKind {
    /// Stable lowercase name used in diagnostics and help.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Kernel => "kernel",
        }
    }
}

/// Environments in which a command is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only the host backend implements this command.
    Host,
    /// Only the kernel backend implements this command.
    Kernel,
    /// Both backends implement this command with equivalent semantics.
    Both,
}

impl Scope {
    /// Returns whether `backend` is allowed to execute this command.
    pub const fn supports(self, backend: BackendKind) -> bool {
        matches!(
            (self, backend),
            (Self::Both, _) | (Self::Host, BackendKind::Host) | (Self::Kernel, BackendKind::Kernel)
        )
    }

    /// Human-readable scope tag used by generated help (`[H]`, `[K]`, `[B]`).
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Host => "[H]",
            Self::Kernel => "[K]",
            Self::Both => "[B]",
        }
    }
}

/// Privilege required by a command, independent of backend availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    ProcessKill,
    SchedModify,
    BlockLoad,
    BlockUnload,
    NetModify,
    FsWrite,
    StoreInstall,
    SecurityAdmin,
    DriverModify,
    WdControl,
    BridgeControl,
}

/// Capabilities granted to the current console session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    granted: BTreeSet<Capability>,
}

impl CapabilitySet {
    /// Creates an empty capability set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a set containing every capability.
    pub fn all() -> Self {
        let mut set = Self::new();
        set.grant(Capability::ProcessKill);
        set.grant(Capability::SchedModify);
        set.grant(Capability::BlockLoad);
        set.grant(Capability::BlockUnload);
        set.grant(Capability::NetModify);
        set.grant(Capability::FsWrite);
        set.grant(Capability::StoreInstall);
        set.grant(Capability::SecurityAdmin);
        set.grant(Capability::DriverModify);
        set.grant(Capability::WdControl);
        set.grant(Capability::BridgeControl);
        set
    }

    /// Grants one capability.
    pub fn grant(&mut self, capability: Capability) {
        self.granted.insert(capability);
    }

    /// Returns whether `capability` is granted.
    pub fn has(&self, capability: Capability) -> bool {
        self.granted.contains(&capability)
    }

    /// Returns the number of granted capabilities.
    pub fn len(&self) -> usize {
        self.granted.len()
    }

    /// Returns whether no capabilities are granted.
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}
