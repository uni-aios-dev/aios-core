use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTableEntry {
    pub physical_addr: u64,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub present: bool,
}

impl PageTableEntry {
    pub fn new(physical_addr: u64) -> Self {
        Self {
            physical_addr,
            read: false,
            write: false,
            execute: false,
            present: false,
        }
    }

    pub fn with_permissions(mut self, read: bool, write: bool, execute: bool) -> Self {
        self.read = read;
        self.write = write;
        self.execute = execute;
        self
    }

    pub fn present(mut self) -> Self {
        self.present = true;
        self
    }
}

pub struct PageTable {
    entries: HashMap<u64, PageTableEntry>,
    page_size: u64,
}

impl PageTable {
    pub fn new(page_size: u64) -> Self {
        Self {
            entries: HashMap::new(),
            page_size,
        }
    }

    pub fn map(&mut self, virtual_addr: u64, entry: PageTableEntry) {
        self.entries.insert(virtual_addr, entry);
    }

    pub fn unmap(&mut self, virtual_addr: u64) {
        self.entries.remove(&virtual_addr);
    }

    pub fn lookup(&self, virtual_addr: u64) -> Option<PageTableEntry> {
        self.entries.get(&virtual_addr).copied()
    }

    pub fn is_mapped(&self, virtual_addr: u64) -> bool {
        self.entries.contains_key(&virtual_addr)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn page_size(&self) -> u64 {
        self.page_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_table_entry_creation() {
        let entry = PageTableEntry::new(0x1000);
        assert_eq!(entry.physical_addr, 0x1000);
        assert!(!entry.present);
    }

    #[test]
    fn test_page_table_entry_with_permissions() {
        let entry = PageTableEntry::new(0x1000)
            .with_permissions(true, true, false)
            .present();

        assert!(entry.read);
        assert!(entry.write);
        assert!(!entry.execute);
        assert!(entry.present);
    }

    #[test]
    fn test_page_table_map() {
        let mut pt = PageTable::new(4096);
        let entry = PageTableEntry::new(0x1000).present();

        pt.map(0x0000, entry);
        assert!(pt.is_mapped(0x0000));
        assert_eq!(pt.entry_count(), 1);
    }

    #[test]
    fn test_page_table_unmap() {
        let mut pt = PageTable::new(4096);
        let entry = PageTableEntry::new(0x1000).present();

        pt.map(0x0000, entry);
        assert!(pt.is_mapped(0x0000));

        pt.unmap(0x0000);
        assert!(!pt.is_mapped(0x0000));
    }

    #[test]
    fn test_page_table_lookup() {
        let mut pt = PageTable::new(4096);
        let entry = PageTableEntry::new(0x1000)
            .with_permissions(true, false, false)
            .present();

        pt.map(0x0000, entry);
        let looked_up = pt.lookup(0x0000).unwrap();

        assert_eq!(looked_up.physical_addr, 0x1000);
        assert!(looked_up.read);
        assert!(!looked_up.write);
    }

    #[test]
    fn test_page_table_multiple_entries() {
        let mut pt = PageTable::new(4096);

        for i in 0..10 {
            let entry = PageTableEntry::new(0x1000 + (i * 0x1000))
                .with_permissions(true, true, false)
                .present();
            pt.map(i * 0x1000, entry);
        }

        assert_eq!(pt.entry_count(), 10);
    }

    #[test]
    fn test_page_table_page_size() {
        let pt = PageTable::new(4096);
        assert_eq!(pt.page_size(), 4096);
    }
}
