use aios_vfs::ai_preview::analyze_file;
use aios_vfs::operations::{
    copy_recursive, delete_item, move_item, read_head, total_bytes, CopyStats, DeleteStats,
    Progress,
};
use aios_vfs::security::{AclContext, HOST_READ_CAP, HOST_WRITE_CAP};
use aios_vfs::vfs::{VfsPath, VirtualFileSystem};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::commands::{Ack, Command};
use crate::state::{PanelSide, PanelState};

const DEFAULT_LEFT: &str = "AIOS:///sandbox";
const DEFAULT_RIGHT: &str = "AIOS:///store";
const PREVIEW_LIMIT: usize = 64 * 1024;

/// Lifecycle status of a background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
    Canceled,
}

/// A background copy/move/delete job with a live progress tracker.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: u64,
    pub label: String,
    pub total: u64,
    pub progress: Arc<Progress>,
    pub status: JobStatus,
    pub error: Option<String>,
}

impl JobInfo {
    /// Completion percentage `0.0..=100.0`.
    pub fn percent(&self) -> f64 {
        self.progress.fraction() * 100.0
    }
}

/// Immutable snapshot of the engine used for rendering.
#[derive(Debug, Clone)]
pub struct FmSnapshot {
    pub panels: Vec<PanelState>,
    pub active: usize,
    pub jobs: Vec<JobInfo>,
    pub acl: Vec<String>,
}

#[derive(Debug)]
struct FmState {
    panels: Vec<PanelState>,
    active: usize,
    jobs: Vec<JobInfo>,
    next_job_id: u64,
}

/// The file-manager engine: owns the two panels, the capability ACL and the
/// VFS handle, and processes `Command`s arriving over a `tokio::mpsc` channel
/// from any UI (TUI or GUI). Cloneable — every clone shares the same state.
#[derive(Clone)]
pub struct FileManager {
    state: Arc<Mutex<FmState>>,
    fs: Arc<dyn VirtualFileSystem>,
    acl: Arc<AclContext>,
    cmd_tx: UnboundedSender<Command>,
}

impl FileManager {
    /// Create the engine, spawn its command loop, and return a handle plus the
    /// `Ack` receiver the UI polls for results.
    pub fn new(
        fs: Arc<dyn VirtualFileSystem>,
        acl: Arc<AclContext>,
    ) -> (Self, UnboundedReceiver<Ack>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ack_tx, ack_rx) = mpsc::unbounded_channel();
        let panels = vec![
            PanelState::new(
                PanelSide::Left,
                VfsPath::parse(DEFAULT_LEFT).expect("static AIOS URI"),
            ),
            PanelState::new(
                PanelSide::Right,
                VfsPath::parse(DEFAULT_RIGHT).expect("static AIOS URI"),
            ),
        ];
        let state = Arc::new(Mutex::new(FmState {
            panels,
            active: 0,
            jobs: Vec::new(),
            next_job_id: 1,
        }));
        let fm = FileManager {
            state: Arc::clone(&state),
            fs: Arc::clone(&fs),
            acl: Arc::clone(&acl),
            cmd_tx: cmd_tx.clone(),
        };
        tokio::spawn(run_loop(state, fs, acl, cmd_rx, ack_tx));
        (fm, ack_rx)
    }

    /// Queue a command for the engine.
    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Clone of the command sender (e.g. for detached UI threads).
    pub fn cmd_tx(&self) -> UnboundedSender<Command> {
        self.cmd_tx.clone()
    }

    /// The capability ACL backing `HOST://` access.
    pub fn acl(&self) -> &AclContext {
        &self.acl
    }

    /// The underlying VFS handle.
    pub fn fs(&self) -> &Arc<dyn VirtualFileSystem> {
        &self.fs
    }

    /// Index (0 = left, 1 = right) of the active panel.
    pub fn active(&self) -> usize {
        self.state.lock().unwrap().active
    }

    /// Side of the active panel.
    pub fn active_side(&self) -> PanelSide {
        side_for(self.active())
    }

    /// Immutable snapshot for rendering.
    pub fn snapshot(&self) -> FmSnapshot {
        let st = self.state.lock().unwrap();
        FmSnapshot {
            panels: st.panels.clone(),
            active: st.active,
            jobs: st.jobs.clone(),
            acl: self.acl.tokens(),
        }
    }

    /// Move focus to the other panel.
    pub fn switch_panel(&self) {
        let mut st = self.state.lock().unwrap();
        st.active = 1 - st.active;
    }

    /// Make the panel at `index` (0 = left, 1 = right) the active one.
    pub fn set_active(&self, index: usize) {
        let mut st = self.state.lock().unwrap();
        st.active = index % 2;
    }

    /// Move a panel's cursor by `delta` rows.
    pub fn move_cursor(&self, side: PanelSide, delta: isize) {
        let mut st = self.state.lock().unwrap();
        let idx = side_index(side);
        st.panels[idx].move_cursor(delta);
        st.panels[idx].clamp_cursor(20);
    }

    /// Place a panel's cursor on a specific row (used by GUI click selection).
    pub fn set_cursor(&self, side: PanelSide, index: usize) {
        let mut st = self.state.lock().unwrap();
        let idx = side_index(side);
        st.panels[idx].cursor = index.min(st.panels[idx].entries.len().saturating_sub(1));
    }

    /// Clamp the viewport of a panel to `rows` visible rows.
    pub fn clamp_viewport(&self, side: PanelSide, rows: usize) {
        let mut st = self.state.lock().unwrap();
        st.panels[side_index(side)].clamp_cursor(rows);
    }

    /// Current directory of a panel.
    pub fn panel_path(&self, side: PanelSide) -> VfsPath {
        let st = self.state.lock().unwrap();
        st.panels[side_index(side)].path.clone()
    }

    /// Full path of the selected entry in a panel, if any.
    pub fn selected(&self, side: PanelSide) -> Option<VfsPath> {
        let st = self.state.lock().unwrap();
        st.panels[side_index(side)].selected_path()
    }

    /// Name of the selected entry in a panel, if any.
    pub fn selected_name(&self, side: PanelSide) -> Option<String> {
        let st = self.state.lock().unwrap();
        st.panels[side_index(side)]
            .selected()
            .map(|e| e.name.clone())
    }

    /// Whether the selected entry in a panel is a directory, if any.
    pub fn selected_is_dir(&self, side: PanelSide) -> Option<bool> {
        let st = self.state.lock().unwrap();
        st.panels[side_index(side)].selected().map(|e| e.is_dir)
    }

    /// Toggle the sort direction of a panel.
    pub fn toggle_sort(&self, side: PanelSide) {
        let mut st = self.state.lock().unwrap();
        st.panels[side_index(side)].toggle_sort();
    }

    /// Default copy/move target for the selected entry in `side`: the same
    /// name inside the other panel's current directory.
    pub fn default_target(&self, side: PanelSide) -> Option<VfsPath> {
        let src = self.selected(side)?;
        let name = src.file_name()?;
        let dst_dir = self.panel_path(side.opposite());
        Some(dst_dir.join(&name))
    }
}

fn side_index(side: PanelSide) -> usize {
    match side {
        PanelSide::Left => 0,
        PanelSide::Right => 1,
    }
}

fn side_for(index: usize) -> PanelSide {
    if index == 0 {
        PanelSide::Left
    } else {
        PanelSide::Right
    }
}

#[derive(Debug, Clone, Copy)]
enum OpKind {
    Copy,
    Move,
    Delete,
}

enum JobOutcome {
    Copied(CopyStats),
    Moved(CopyStats),
    Deleted(DeleteStats),
}

async fn run_loop(
    state: Arc<Mutex<FmState>>,
    fs: Arc<dyn VirtualFileSystem>,
    acl: Arc<AclContext>,
    mut cmd_rx: UnboundedReceiver<Command>,
    ack_tx: UnboundedSender<Ack>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        if matches!(cmd, Command::Shutdown) {
            break;
        }
        if let Err(e) = dispatch(&state, &fs, &acl, &ack_tx, cmd).await {
            let _ = ack_tx.send(Ack::Error(e.to_string()));
        }
    }
}

async fn dispatch(
    state: &Arc<Mutex<FmState>>,
    fs: &Arc<dyn VirtualFileSystem>,
    acl: &Arc<AclContext>,
    ack_tx: &UnboundedSender<Ack>,
    cmd: Command,
) -> aios_core::error::Result<()> {
    match cmd {
        Command::Navigate { side, path } => {
            let entries = fs.list_dir(&path).await?;
            let mut st = state.lock().unwrap();
            let idx = side_index(side);
            st.panels[idx].path = path;
            st.panels[idx].set_entries(entries);
            drop(st);
            let _ = ack_tx.send(Ack::DirChanged { side });
            Ok(())
        }
        Command::Refresh { side } => {
            let path = {
                let st = state.lock().unwrap();
                st.panels[side_index(side)].path.clone()
            };
            if let Ok(entries) = fs.list_dir(&path).await {
                let mut st = state.lock().unwrap();
                st.panels[side_index(side)].set_entries(entries);
                drop(st);
                let _ = ack_tx.send(Ack::DirChanged { side });
            }
            Ok(())
        }
        Command::Mkdir { side, parent, name } => {
            let new = parent.join(&name);
            fs.create_dir(&new).await?;
            refresh_panel(state, fs, ack_tx, side).await;
            let _ = ack_tx.send(Ack::CreatedDir { path: new });
            Ok(())
        }
        Command::Rename { side, from, to } => {
            fs.rename(&from, &to).await?;
            refresh_panel(state, fs, ack_tx, side).await;
            let _ = ack_tx.send(Ack::Renamed { from, to });
            Ok(())
        }
        Command::Copy { src, dst } => {
            spawn_job(state, fs, ack_tx, OpKind::Copy, src, Some(dst)).await;
            Ok(())
        }
        Command::Move { src, dst } => {
            spawn_job(state, fs, ack_tx, OpKind::Move, src, Some(dst)).await;
            Ok(())
        }
        Command::Delete { path } => {
            spawn_job(state, fs, ack_tx, OpKind::Delete, path, None).await;
            Ok(())
        }
        Command::View { path } => {
            let name = path.file_name().unwrap_or_else(|| "file".to_string());
            let head = read_head(&**fs, &path, PREVIEW_LIMIT).await?;
            let preview = analyze_file(&name, &head);
            let _ = ack_tx.send(Ack::View { path, preview });
            Ok(())
        }
        Command::GrantHostRead => {
            acl.grant(HOST_READ_CAP);
            Ok(())
        }
        Command::GrantHostWrite => {
            acl.grant(HOST_WRITE_CAP);
            Ok(())
        }
        Command::Shutdown => Ok(()),
    }
}

async fn spawn_job(
    state: &Arc<Mutex<FmState>>,
    fs: &Arc<dyn VirtualFileSystem>,
    ack_tx: &UnboundedSender<Ack>,
    kind: OpKind,
    src: VfsPath,
    dst: Option<VfsPath>,
) {
    let total = total_bytes(&**fs, &src).await.unwrap_or(0);
    let progress = Arc::new(Progress::new());
    progress.set_total(total);
    let label = match src.file_name() {
        Some(n) => format!("{verb} {n}", verb = kind_verb(kind)),
        None => kind_verb(kind).to_string(),
    };
    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_job_id;
        st.next_job_id += 1;
        st.jobs.push(JobInfo {
            id,
            label: label.clone(),
            total,
            progress: Arc::clone(&progress),
            status: JobStatus::Running,
            error: None,
        });
        id
    };
    let fs2 = Arc::clone(fs);
    let state2 = Arc::clone(state);
    let ack2 = ack_tx.clone();
    tokio::spawn(async move {
        let result: aios_core::error::Result<JobOutcome> = match kind {
            OpKind::Copy => {
                let target = dst.expect("copy needs a destination");
                copy_recursive(&*fs2, &*fs2, &src, &target, &progress)
                    .await
                    .map(JobOutcome::Copied)
            }
            OpKind::Move => {
                let target = dst.expect("move needs a destination");
                move_item(&*fs2, &*fs2, &src, &target, &progress)
                    .await
                    .map(JobOutcome::Moved)
            }
            OpKind::Delete => delete_item(&*fs2, &src, &progress)
                .await
                .map(JobOutcome::Deleted),
        };
        let cancelled = progress.is_cancelled();
        let status = match &result {
            Ok(_) => JobStatus::Done,
            Err(_) if cancelled => JobStatus::Canceled,
            Err(_) => JobStatus::Failed,
        };
        let error = match &result {
            Err(e) if !cancelled => Some(e.to_string()),
            _ => None,
        };
        {
            let mut st = state2.lock().unwrap();
            if let Some(job) = st.jobs.iter_mut().find(|j| j.id == id) {
                job.status = status;
                job.error = error;
            }
        }
        let ack = match result {
            Ok(JobOutcome::Copied(s)) => Ack::Copied(s),
            Ok(JobOutcome::Moved(s)) => Ack::Moved(s),
            Ok(JobOutcome::Deleted(s)) => Ack::Deleted(s),
            Err(e) => Ack::Error(e.to_string()),
        };
        let _ = ack2.send(ack);
        refresh_all(state2, fs2).await;
    });
}

fn kind_verb(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Copy => "Copy",
        OpKind::Move => "Move",
        OpKind::Delete => "Delete",
    }
}

async fn refresh_panel(
    state: &Arc<Mutex<FmState>>,
    fs: &Arc<dyn VirtualFileSystem>,
    ack_tx: &UnboundedSender<Ack>,
    side: PanelSide,
) {
    let path = {
        let st = state.lock().unwrap();
        st.panels[side_index(side)].path.clone()
    };
    if let Ok(entries) = fs.list_dir(&path).await {
        let mut st = state.lock().unwrap();
        st.panels[side_index(side)].set_entries(entries);
        drop(st);
        let _ = ack_tx.send(Ack::DirChanged { side });
    }
}

async fn refresh_all(state: Arc<Mutex<FmState>>, fs: Arc<dyn VirtualFileSystem>) {
    let paths: Vec<VfsPath> = {
        let st = state.lock().unwrap();
        st.panels.iter().map(|p| p.path.clone()).collect()
    };
    for (idx, path) in paths.iter().enumerate() {
        if let Ok(entries) = fs.list_dir(path).await {
            let mut st = state.lock().unwrap();
            st.panels[idx].set_entries(entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_vfs::vfs::AiosVfs;

    fn test_fs(dir: &tempfile::TempDir) -> Arc<dyn VirtualFileSystem> {
        Arc::new(AiosVfs::new(dir.path().join("sandbox")).unwrap())
    }

    #[tokio::test]
    async fn test_mkdir_and_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let (fm, mut ack_rx) = FileManager::new(test_fs(&dir), Arc::new(AclContext::new()));
        fm.send(Command::Mkdir {
            side: PanelSide::Left,
            parent: VfsPath::parse("AIOS:///sandbox").unwrap(),
            name: "tmp".into(),
        });
        let mut found = false;
        while let Some(ack) = ack_rx.recv().await {
            if let Ack::CreatedDir { path } = ack {
                assert_eq!(path.to_uri(), "AIOS:///sandbox/tmp");
                found = true;
                break;
            }
        }
        assert!(found);
        let snap = fm.snapshot();
        assert!(snap
            .panels
            .iter()
            .any(|p| p.entries.iter().any(|e| e.name == "tmp")));
    }

    #[tokio::test]
    async fn test_copy_job_reports_stats() {
        let dir = tempfile::tempdir().unwrap();
        let fs = test_fs(&dir);
        let src = VfsPath::parse("AIOS:///sandbox/a.txt").unwrap();
        fs.write_file(&src, b"hello").await.unwrap();
        let (fm, mut ack_rx) = FileManager::new(Arc::clone(&fs), Arc::new(AclContext::new()));
        fm.send(Command::Copy {
            src: src.clone(),
            dst: VfsPath::parse("AIOS:///store/b.txt").unwrap(),
        });
        let mut ok = false;
        while let Some(ack) = ack_rx.recv().await {
            if let Ack::Copied(stats) = ack {
                assert_eq!(stats.files, 1);
                assert_eq!(stats.bytes, 5);
                ok = true;
                break;
            }
        }
        assert!(ok);
        assert!(fs
            .exists(&VfsPath::parse("AIOS:///store/b.txt").unwrap())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_delete_job() {
        let dir = tempfile::tempdir().unwrap();
        let fs = test_fs(&dir);
        let src = VfsPath::parse("AIOS:///sandbox/del.txt").unwrap();
        fs.write_file(&src, b"bye").await.unwrap();
        let (fm, mut ack_rx) = FileManager::new(Arc::clone(&fs), Arc::new(AclContext::new()));
        fm.send(Command::Delete { path: src.clone() });
        let mut ok = false;
        while let Some(ack) = ack_rx.recv().await {
            if let Ack::Deleted(stats) = ack {
                assert_eq!(stats.files, 1);
                ok = true;
                break;
            }
        }
        assert!(ok);
        assert!(!fs.exists(&src).await.unwrap());
    }

    #[tokio::test]
    async fn test_move_uses_rename() {
        let dir = tempfile::tempdir().unwrap();
        let fs = test_fs(&dir);
        let from = VfsPath::parse("AIOS:///sandbox/m.txt").unwrap();
        fs.write_file(&from, b"mv").await.unwrap();
        let (fm, mut ack_rx) = FileManager::new(Arc::clone(&fs), Arc::new(AclContext::new()));
        fm.send(Command::Move {
            src: from.clone(),
            dst: VfsPath::parse("AIOS:///store/m2.txt").unwrap(),
        });
        let mut ok = false;
        while let Some(ack) = ack_rx.recv().await {
            if let Ack::Moved(stats) = ack {
                assert_eq!(stats.files, 1);
                ok = true;
                break;
            }
        }
        assert!(ok);
        assert!(!fs.exists(&from).await.unwrap());
        assert!(fs
            .exists(&VfsPath::parse("AIOS:///store/m2.txt").unwrap())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_view_preview_and_acl_grant() {
        let dir = tempfile::tempdir().unwrap();
        let fs = test_fs(&dir);
        let file = VfsPath::parse("AIOS:///config/app.log").unwrap();
        fs.write_file(&file, b"boot ok\npanicked\n").await.unwrap();
        let (fm, mut ack_rx) = FileManager::new(Arc::clone(&fs), Arc::new(AclContext::new()));
        fm.send(Command::View { path: file });
        let mut ok = false;
        while let Some(ack) = ack_rx.recv().await {
            if let Ack::View { preview, .. } = ack {
                assert!(preview.lines.iter().any(|(_, l)| l.contains("Panics: 1")));
                ok = true;
                break;
            }
        }
        assert!(ok);

        fm.send(Command::GrantHostRead);
        fm.send(Command::GrantHostWrite);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        while tokio::time::Instant::now() < deadline {
            if fm.acl().has(HOST_READ_CAP) && fm.acl().has(HOST_WRITE_CAP) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(fm.acl().has(HOST_READ_CAP));
        assert!(fm.acl().has(HOST_WRITE_CAP));
    }

    #[tokio::test]
    async fn test_cursor_and_default_target() {
        let dir = tempfile::tempdir().unwrap();
        let (fm, _) = FileManager::new(test_fs(&dir), Arc::new(AclContext::new()));
        fm.move_cursor(PanelSide::Left, -5);
        assert_eq!(fm.snapshot().panels[0].cursor, 0);
        fm.switch_panel();
        assert_eq!(fm.active(), 1);
        assert!(fm.selected(PanelSide::Left).is_none());
    }

    #[tokio::test]
    async fn test_shutdown_stops_loop() {
        let dir = tempfile::tempdir().unwrap();
        let (fm, _ack_rx) = FileManager::new(test_fs(&dir), Arc::new(AclContext::new()));
        fm.send(Command::Shutdown);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let snap = fm.snapshot();
        assert_eq!(snap.panels.len(), 2);
    }
}
