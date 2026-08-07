use aios_vfs::vfs::{VfsEntry, VfsPath};

/// Which of the two file-manager panels is being addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSide {
    Left,
    Right,
}

impl PanelSide {
    /// The other panel.
    pub fn opposite(self) -> PanelSide {
        match self {
            PanelSide::Left => PanelSide::Right,
            PanelSide::Right => PanelSide::Left,
        }
    }

    /// Short display name.
    pub fn name(self) -> &'static str {
        match self {
            PanelSide::Left => "LEFT",
            PanelSide::Right => "RIGHT",
        }
    }
}

/// Column used for sorting a directory listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// Sort rule applied to a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortRule {
    pub by: SortBy,
    pub dir: SortDir,
}

impl Default for SortRule {
    fn default() -> Self {
        Self {
            by: SortBy::Name,
            dir: SortDir::Asc,
        }
    }
}

/// Mutable view state of a single panel: current directory, cached listing,
/// cursor position and sort rule.
#[derive(Debug, Clone)]
pub struct PanelState {
    pub side: PanelSide,
    pub path: VfsPath,
    pub entries: Vec<VfsEntry>,
    pub cursor: usize,
    pub offset: usize,
    pub sort: SortRule,
}

impl PanelState {
    /// Create an empty panel rooted at `path`.
    pub fn new(side: PanelSide, path: VfsPath) -> Self {
        Self {
            side,
            path,
            entries: Vec::new(),
            cursor: 0,
            offset: 0,
            sort: SortRule::default(),
        }
    }

    /// The entry under the cursor, if any.
    pub fn selected(&self) -> Option<&VfsEntry> {
        self.entries.get(self.cursor)
    }

    /// Full `VfsPath` of the entry under the cursor, if any.
    pub fn selected_path(&self) -> Option<VfsPath> {
        self.selected().map(|e| self.path.join(&e.name))
    }

    /// Replace the listing and re-apply the sort rule.
    pub fn set_entries(&mut self, entries: Vec<VfsEntry>) {
        self.entries = entries;
        self.sort_entries();
        self.clamp_cursor(20);
    }

    /// Re-sort the cached listing: directories first, then the active column.
    pub fn sort_entries(&mut self) {
        let by = self.sort.by;
        let dir = self.sort.dir;
        self.entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| {
                let c = match by {
                    SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    SortBy::Size => a.size.cmp(&b.size),
                    SortBy::Modified => match (a.modified, b.modified) {
                        (Some(x), Some(y)) => x.cmp(&y),
                        (Some(_), None) => std::cmp::Ordering::Greater,
                        (None, Some(_)) => std::cmp::Ordering::Less,
                        (None, None) => std::cmp::Ordering::Equal,
                    },
                };
                if dir == SortDir::Desc {
                    c.reverse()
                } else {
                    c
                }
            })
        });
    }

    /// Flip the sort direction (ascending <-> descending).
    pub fn toggle_sort(&mut self) {
        self.sort.dir = match self.sort.dir {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        };
        self.sort_entries();
    }

    /// Move the cursor by `delta` rows, clamped to the listing.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.entries.len() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, len - 1) as usize;
    }

    /// Keep `cursor` inside the listing and scroll `offset` so the cursor is
    /// visible in a viewport `rows` high.
    pub fn clamp_cursor(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.entries.is_empty() {
            self.cursor = 0;
            self.offset = 0;
            return;
        }
        self.cursor = self.cursor.min(self.entries.len() - 1);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + rows {
            self.offset = self.cursor + 1 - rows;
        }
    }
}

/// Render a byte count as a compact human-readable string (`1.5 MB`).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_opposite_and_name() {
        assert_eq!(PanelSide::Left.opposite(), PanelSide::Right);
        assert_eq!(PanelSide::Right.opposite(), PanelSide::Left);
        assert_eq!(PanelSide::Left.name(), "LEFT");
        assert_eq!(PanelSide::Right.name(), "RIGHT");
    }

    #[test]
    fn test_sort_rule_default() {
        let rule = SortRule::default();
        assert_eq!(rule.by, SortBy::Name);
        assert_eq!(rule.dir, SortDir::Asc);
    }

    #[test]
    fn test_cursor_movement_and_clamp() {
        let mut panel =
            PanelState::new(PanelSide::Left, VfsPath::parse("AIOS:///sandbox").unwrap());
        let entries: Vec<VfsEntry> = (0..50)
            .map(|i| VfsEntry {
                name: format!("f{i:02}.txt"),
                is_dir: false,
                size: i as u64,
                modified: None,
                permissions: "rw-".into(),
                acl: Vec::new(),
            })
            .collect();
        panel.set_entries(entries);
        assert_eq!(panel.entries.len(), 50);

        panel.move_cursor(25);
        assert_eq!(panel.cursor, 25);
        panel.clamp_cursor(10);
        assert_eq!(panel.offset, 16);

        panel.move_cursor(50);
        assert_eq!(panel.cursor, 49);
        panel.move_cursor(-500);
        assert_eq!(panel.cursor, 0);
    }

    #[test]
    fn test_directories_sort_first() {
        let mut panel = PanelState::new(PanelSide::Right, VfsPath::parse("AIOS:///store").unwrap());
        let entry = |name: &str, is_dir: bool| VfsEntry {
            name: name.into(),
            is_dir,
            size: 0,
            modified: None,
            permissions: "rw-".into(),
            acl: Vec::new(),
        };
        panel.set_entries(vec![
            entry("z.txt", false),
            entry("a_dir", true),
            entry("b_dir", true),
            entry("a.txt", false),
        ]);
        let names: Vec<&str> = panel.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a_dir", "b_dir", "a.txt", "z.txt"]);
    }

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
