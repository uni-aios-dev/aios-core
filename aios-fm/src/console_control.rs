//! Console control seam over the live file manager.
//!
//! This bridges `aios-console`'s `FileManagerControl` to a real `FileManager`,
//! so the canonical `fm jobs`/`fm cancel` commands observe and cancel actual
//! background transfers instead of fabricating results. The adapter stays in
//! `aios-fm` because that crate owns the live job table; `aios-console` only
//! knows the injection trait.

use std::sync::Arc;

use aios_console::control::{FileManagerControl, FmJobInfo};

use crate::engine::{FileManager, JobStatus};

/// Adapts a live [`FileManager`] to the console's file-manager control seam.
pub struct FileManagerConsoleControl {
    fm: Arc<FileManager>,
}

impl FileManagerConsoleControl {
    /// Wraps a shared file-manager handle.
    pub fn new(fm: Arc<FileManager>) -> Self {
        Self { fm }
    }
}

impl FileManagerControl for FileManagerConsoleControl {
    fn jobs(&self) -> Vec<FmJobInfo> {
        self.fm
            .snapshot()
            .jobs
            .into_iter()
            .map(|job| {
                let percent = job.percent();
                let status = match job.status {
                    JobStatus::Running => "running",
                    JobStatus::Done => "done",
                    JobStatus::Failed => "failed",
                    JobStatus::Canceled => "canceled",
                };
                FmJobInfo {
                    id: job.id,
                    label: job.label,
                    percent,
                    status: status.to_string(),
                    error: job.error,
                }
            })
            .collect()
    }

    fn cancel(&self, id: u64) -> Result<(), String> {
        if self.fm.cancel_job(id) {
            Ok(())
        } else {
            Err(format!("no job {id}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_vfs::security::AclContext;
    use aios_vfs::vfs::{AiosVfs, VfsPath};
    use tempfile::TempDir;

    fn test_fs(dir: &TempDir) -> Arc<dyn aios_vfs::vfs::VirtualFileSystem> {
        Arc::new(AiosVfs::new(dir.path().join("sandbox")).unwrap())
    }

    #[tokio::test]
    async fn unknown_job_cancel_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (fm, _ack) = FileManager::new(test_fs(&dir), Arc::new(AclContext::new()));
        let control = FileManagerConsoleControl::new(Arc::new(fm));
        assert!(control.jobs().is_empty());
        assert_eq!(control.cancel(42), Err("no job 42".to_string()));
    }

    #[tokio::test]
    async fn job_snapshot_and_cancel_use_the_live_engine() {
        let dir = tempfile::tempdir().unwrap();
        let fs = test_fs(&dir);
        let src = VfsPath::parse("AIOS:///sandbox/a.txt").unwrap();
        fs.write_file(&src, b"hello").await.unwrap();
        let (fm, mut ack_rx) = FileManager::new(Arc::clone(&fs), Arc::new(AclContext::new()));
        let fm = Arc::new(fm);
        let control = FileManagerConsoleControl::new(fm.clone());

        fm.send(crate::commands::Command::Copy {
            src,
            dst: VfsPath::parse("AIOS:///store/b.txt").unwrap(),
        });
        let mut copied = false;
        while let Some(ack) = ack_rx.recv().await {
            if matches!(ack, crate::commands::Ack::Copied(_)) {
                copied = true;
                break;
            }
        }
        assert!(copied);

        let jobs = control.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, 1);
        assert_eq!(jobs[0].status, "done");
        assert!(jobs[0].label.starts_with("Copy"));
        assert_eq!(control.cancel(1), Ok(()));
        assert!(control.cancel(999).is_err());
    }
}
