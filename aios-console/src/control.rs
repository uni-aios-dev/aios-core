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
    /// Resets watchdog state to monitoring; an attached runner stays attached.
    fn reset(&self) -> Result<(), String>;
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

/// One background job reported by a host-owned file manager.
#[derive(Debug, Clone, PartialEq)]
pub struct FmJobInfo {
    /// Stable job identifier shown by `fm jobs` and accepted by `fm cancel`.
    pub id: u64,
    /// Human-readable operation label.
    pub label: String,
    /// Completion percentage in `0.0..=100.0`.
    pub percent: f64,
    /// Lowercase lifecycle status: `running`, `done`, `failed` or `canceled`.
    pub status: String,
    /// Failure detail, present only for failed jobs.
    pub error: Option<String>,
}

/// Control surface for a live file manager owned by a host frontend.
pub trait FileManagerControl: Send + Sync {
    /// Returns a snapshot of the live background jobs.
    fn jobs(&self) -> Vec<FmJobInfo>;
    /// Requests cancellation of one job; returns an error when the id is unknown.
    fn cancel(&self, id: u64) -> Result<(), String>;
}

/// One background job reported by a host-owned shell job table.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellJobInfo {
    /// Job identifier shown by `jobs`.
    pub id: usize,
    /// Command line of the job.
    pub cmd: String,
    /// Process id of the running job.
    pub pid: u32,
}

/// Control surface for a shell job table owned by a host frontend.
///
/// The console never owns process groups or signals; the injected control
/// performs the real `SIGCONT` and updates the table it shares with the shell.
pub trait JobControl: Send + Sync {
    /// Returns a snapshot of the live background jobs.
    fn jobs(&self) -> Vec<ShellJobInfo>;
    /// Brings job `id` to the foreground; returns an error for an unknown id.
    fn fg(&self, id: usize) -> Result<ShellJobInfo, String>;
    /// Resumes job `id` in the background; returns an error for an unknown id.
    fn bg(&self, id: usize) -> Result<ShellJobInfo, String>;
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
