use aios_vfs::ai_preview::AiPreview;
use aios_vfs::operations::{CopyStats, DeleteStats};
use aios_vfs::vfs::VfsPath;

use crate::state::PanelSide;

/// Imperative command sent to the file-manager engine over `tokio::mpsc`.
#[derive(Debug, Clone)]
pub enum Command {
    /// Navigate a panel to a new directory and re-list it.
    Navigate { side: PanelSide, path: VfsPath },
    /// Re-list the current directory of a panel.
    Refresh { side: PanelSide },
    /// Copy `src` (file or directory) to `dst` in the background.
    Copy { src: VfsPath, dst: VfsPath },
    /// Move `src` to `dst` in the background.
    Move { src: VfsPath, dst: VfsPath },
    /// Recursively delete `path` in the background.
    Delete { path: VfsPath },
    /// Create a new directory under `parent`.
    Mkdir {
        side: PanelSide,
        parent: VfsPath,
        name: String,
    },
    /// Rename `from` to `to`.
    Rename {
        side: PanelSide,
        from: VfsPath,
        to: VfsPath,
    },
    /// Produce a smart AI preview for `path`.
    View { path: VfsPath },
    /// Grant the host read capability (`vfs:host:read`).
    GrantHostRead,
    /// Grant the host write capability (`vfs:host:write`).
    GrantHostWrite,
    /// Stop the engine loop (used by tests and standalone runs).
    Shutdown,
}

/// Acknowledgment emitted by the engine after a command completes.
#[derive(Debug, Clone)]
pub enum Ack {
    /// A panel directory listing was refreshed.
    DirChanged { side: PanelSide },
    /// A background copy finished.
    Copied(CopyStats),
    /// A background move finished.
    Moved(CopyStats),
    /// A background delete finished.
    Deleted(DeleteStats),
    /// A new directory was created.
    CreatedDir { path: VfsPath },
    /// An object was renamed.
    Renamed { from: VfsPath, to: VfsPath },
    /// A smart AI preview was produced for `path`.
    View { path: VfsPath, preview: AiPreview },
    /// An operation failed.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_constructs() {
        let cmd = Command::Navigate {
            side: PanelSide::Left,
            path: VfsPath::parse("AIOS:///sandbox").unwrap(),
        };
        match cmd {
            Command::Navigate { side, .. } => assert_eq!(side, PanelSide::Left),
            _ => panic!("wrong variant"),
        }
    }
}
