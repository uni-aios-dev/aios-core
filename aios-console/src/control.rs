//! Injection seams for host-owned subsystems that cannot be direct dependencies.
//!
//! `aios-watchdog` and `aios-bridge` sit above `aios-console` in the dependency
//! graph, so the console cannot depend on them. Instead the host composes the
//! backend with small control objects that expose only text results. When no
//! control object is injected the corresponding commands report `unavailable`
//! instead of faking success.

use alloc::string::String;
use alloc::vec::Vec;

/// Control surface for the watchdog owned by the host orchestrator.
pub trait WatchdogControl: Send + Sync {
    /// Returns human-readable watchdog state lines.
    fn status(&self) -> Vec<String>;
    /// Starts watchdog monitoring; returns an error when it is already running.
    fn start(&self) -> Result<(), String>;
    /// Stops watchdog monitoring; returns an error when it is not running.
    fn stop(&self) -> Result<(), String>;
    /// Forces the watchdog into safe mode.
    fn force_safe_mode(&self);
    /// Returns the actions produced by one escalation step.
    fn escalate(&self) -> Vec<String>;
}

/// Control surface for the HTTP bridge owned by the host orchestrator.
pub trait BridgeControl: Send + Sync {
    /// Returns human-readable bridge state lines.
    fn status(&self) -> Vec<String>;
    /// Starts the bridge listener; returns an error when it cannot bind.
    fn start(&self) -> Result<(), String>;
    /// Stops the bridge listener; returns an error when it is not running.
    fn stop(&self) -> Result<(), String>;
}

/// Authentication surface for bridge users owned by the host orchestrator.
pub trait AuthProvider: Send + Sync {
    /// Registers a user and returns a bearer token.
    fn register(&self, username: &str, password: &str) -> Result<String, String>;
    /// Authenticates a user and returns a bearer token.
    fn login(&self, username: &str, password: &str) -> Result<String, String>;
    /// Returns the identity associated with a bearer token.
    fn me(&self, token: &str) -> Result<String, String>;
}
