use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotInfo {
    pub slot: BootSlot,
    pub version: String,
    pub active: bool,
    pub boot_count: u32,
    pub last_success: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootSlot {
    SlotA,
    SlotB,
}

impl BootSlot {
    pub fn other(&self) -> Self {
        match self {
            BootSlot::SlotA => BootSlot::SlotB,
            BootSlot::SlotB => BootSlot::SlotA,
        }
    }

    pub fn path(&self, base: &std::path::Path) -> PathBuf {
        match self {
            BootSlot::SlotA => base.join("slot_a"),
            BootSlot::SlotB => base.join("slot_b"),
        }
    }
}

pub struct DualBootManager {
    base_path: PathBuf,
    active_slot: BootSlot,
    max_boot_attempts: u32,
}

impl DualBootManager {
    pub fn new(base_path: PathBuf, max_boot_attempts: u32) -> Self {
        let active = Self::detect_active_slot(&base_path);
        Self {
            base_path,
            active_slot: active,
            max_boot_attempts,
        }
    }

    fn detect_active_slot(_base: &std::path::Path) -> BootSlot {
        let slot_b = _base.join("slot_b").join("active.flag");
        if slot_b.exists() {
            BootSlot::SlotB
        } else {
            BootSlot::SlotA
        }
    }

    pub fn active_slot(&self) -> BootSlot {
        self.active_slot
    }

    pub fn inactive_slot(&self) -> BootSlot {
        self.active_slot.other()
    }

    pub fn swap_slot(&mut self) {
        let old = self.active_slot;
        self.active_slot = self.active_slot.other();

        let old_flag = old.path(&self.base_path).join("active.flag");
        let new_flag = self.active_slot.path(&self.base_path).join("active.flag");

        let _ = std::fs::remove_file(&old_flag);
        let _ = std::fs::write(&new_flag, b"1");

        log::info!("Swapped boot slot from {old:?} to {:?}", self.active_slot);
    }

    pub fn record_boot_success(&self) {
        let path = self.active_slot.path(&self.base_path);
        let _ = std::fs::create_dir_all(&path);
        let info_path = path.join("boot_info.json");
        if let Ok(info) = Self::read_boot_info(&info_path) {
            let updated = SlotInfo {
                boot_count: info.boot_count + 1,
                last_success: Some(chrono_now_rfc3339()),
                ..info
            };
            if let Ok(json) = serde_json::to_string(&updated) {
                let _ = std::fs::write(&info_path, json);
            }
        }
    }

    pub fn should_rollback(&self) -> bool {
        let path = self.active_slot.path(&self.base_path).join("boot_info.json");
        if let Ok(info) = Self::read_boot_info(&path) {
            if let Some(last) = &info.last_success {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(last) {
                    let elapsed = chrono::Utc::now().signed_duration_since(ts);
                    return elapsed.num_seconds() > 30;
                }
            }
            info.boot_count > 0 && info.boot_count > self.max_boot_attempts
        } else {
            false
        }
    }

    pub fn get_slot_info(&self, slot: BootSlot) -> Option<SlotInfo> {
        let path = slot.path(&self.base_path).join("boot_info.json");
        Self::read_boot_info(&path).ok()
    }

    fn read_boot_info(path: &std::path::Path) -> Result<SlotInfo, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&data).map_err(|e| e.to_string())
    }
}

fn chrono_now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_slot_other() {
        assert_eq!(BootSlot::SlotA.other(), BootSlot::SlotB);
        assert_eq!(BootSlot::SlotB.other(), BootSlot::SlotA);
    }

    #[test]
    fn test_slot_paths() {
        let base = std::path::Path::new("/test");
        assert!(BootSlot::SlotA.path(base).ends_with("slot_a"));
        assert!(BootSlot::SlotB.path(base).ends_with("slot_b"));
    }

    #[test]
    fn test_detect_active_slot_defaults_to_a() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DualBootManager::new(dir.path().to_path_buf(), 3);
        assert_eq!(mgr.active_slot(), BootSlot::SlotA);
    }

    #[test]
    fn test_swap_slot() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = DualBootManager::new(dir.path().to_path_buf(), 3);
        assert_eq!(mgr.active_slot(), BootSlot::SlotA);
        mgr.swap_slot();
        assert_eq!(mgr.active_slot(), BootSlot::SlotB);
    }

    #[test]
    fn test_should_rollback_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DualBootManager::new(dir.path().to_path_buf(), 3);
        assert!(!mgr.should_rollback());
    }
}
