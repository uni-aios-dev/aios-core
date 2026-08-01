//! Launch the native AIOS GUI dashboard as a separate process.
//!
//! The GUI (`aios-gui`) is a standalone binary. These helpers locate it next to
//! the calling executable or on `PATH`, then spawn it. Used by the TUI and
//! kernel to jump into the GUI dashboard with a single hotkey.

use std::path::PathBuf;
use std::process::Command;

/// Base name of the GUI binary without the platform executable suffix.
const GUI_BIN: &str = "aios-gui";

/// Platform-specific executable name for the GUI binary.
fn gui_binary_name() -> String {
    if cfg!(windows) {
        format!("{GUI_BIN}.exe")
    } else {
        GUI_BIN.to_string()
    }
}

/// Locate the `aios-gui` binary: sibling of the current executable, then every
/// entry on `PATH`.
pub fn find_gui_binary() -> Option<PathBuf> {
    let name = gui_binary_name();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(&name));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join(&name));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Launch the AIOS GUI dashboard in a new process.
///
/// Returns the resolved binary path on success. The GUI window appears
/// independently; the caller (TUI) keeps running.
pub fn launch_gui() -> Result<PathBuf, String> {
    let path = find_gui_binary().ok_or_else(|| "aios-gui binary not found".to_string())?;
    match Command::new(&path).spawn() {
        Ok(_) => Ok(path),
        Err(e) => Err(format!("failed to launch {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_binary_name_is_platform_aware() {
        let name = gui_binary_name();
        assert!(name.contains("aios-gui"));
        if cfg!(windows) {
            assert_eq!(name, "aios-gui.exe");
        } else {
            assert_eq!(name, "aios-gui");
        }
    }

    #[test]
    fn find_gui_binary_is_best_effort() {
        let _ = find_gui_binary();
    }
}
