use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AIOSException {
    BlockNotFound(String),
    BlockAlreadyRegistered(String),
    InvalidSignature { expected: String, actual: String },
    IntegrityCheckFailed(String),
    StateExtractionFailed(String),
    StateRestoreFailed(String),
    HotSwapFailed(String),
    RollbackFailed(String),
    IPCError(String),
    SchedulerError(String),
    ProcessNotFound(u64),
    ProcessAlreadyExists(u64),
    PermissionDenied(String),
    HardwareNotDetected(String),
    InvalidPayload(String),
    Timeout(String),
    ConfigurationError(String),
    SerializationError(String),
    Generic(String),
}

impl fmt::Display for AIOSException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockNotFound(id) => write!(f, "Block not found: {id}"),
            Self::BlockAlreadyRegistered(id) => write!(f, "Block already registered: {id}"),
            Self::InvalidSignature { expected, actual } => {
                write!(f, "Invalid signature: expected {expected}, got {actual}")
            }
            Self::IntegrityCheckFailed(msg) => write!(f, "Integrity check failed: {msg}"),
            Self::StateExtractionFailed(msg) => write!(f, "State extraction failed: {msg}"),
            Self::StateRestoreFailed(msg) => write!(f, "State restore failed: {msg}"),
            Self::HotSwapFailed(msg) => write!(f, "Hot swap failed: {msg}"),
            Self::RollbackFailed(msg) => write!(f, "Rollback failed: {msg}"),
            Self::IPCError(msg) => write!(f, "IPC error: {msg}"),
            Self::SchedulerError(msg) => write!(f, "Scheduler error: {msg}"),
            Self::ProcessNotFound(pid) => write!(f, "Process not found: {pid}"),
            Self::ProcessAlreadyExists(pid) => write!(f, "Process already exists: {pid}"),
            Self::PermissionDenied(msg) => write!(f, "Permission denied: {msg}"),
            Self::HardwareNotDetected(c) => write!(f, "Hardware not detected: {c}"),
            Self::InvalidPayload(msg) => write!(f, "Invalid payload: {msg}"),
            Self::Timeout(msg) => write!(f, "Timeout: {msg}"),
            Self::ConfigurationError(msg) => write!(f, "Configuration error: {msg}"),
            Self::SerializationError(msg) => write!(f, "Serialization error: {msg}"),
            Self::Generic(msg) => write!(f, "Error: {msg}"),
        }
    }
}

impl std::error::Error for AIOSException {}

pub type Result<T> = std::result::Result<T, AIOSException>;
