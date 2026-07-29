use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    IntentParseFailed(String),
    CapabilityDenied(String),
    SystemCallFailed(String),
    SerializationFailed(String),
    ServerError(String),
    InvalidRequest(String),
    WebSocketError(String),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntentParseFailed(msg) => write!(f, "Intent parse failed: {msg}"),
            Self::CapabilityDenied(msg) => write!(f, "Capability denied: {msg}"),
            Self::SystemCallFailed(msg) => write!(f, "System call failed: {msg}"),
            Self::SerializationFailed(msg) => write!(f, "Serialization failed: {msg}"),
            Self::ServerError(msg) => write!(f, "Server error: {msg}"),
            Self::InvalidRequest(msg) => write!(f, "Invalid request: {msg}"),
            Self::WebSocketError(msg) => write!(f, "WebSocket error: {msg}"),
        }
    }
}

impl std::error::Error for BridgeError {}

pub type Result<T> = std::result::Result<T, BridgeError>;
