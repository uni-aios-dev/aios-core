use crate::dma::DmaRegion;
use crate::page_table::PageTable;
use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IommuStatus {
    Enabled,
    Disabled,
    Error,
}

pub struct IommuManager {
    status: IommuStatus,
    domains: HashMap<u32, DmaRegion>,
    page_tables: HashMap<u32, PageTable>,
    next_domain_id: u32,
    max_domains: u32,
}

impl IommuManager {
    pub fn new(max_domains: u32) -> Self {
        Self {
            status: IommuStatus::Enabled,
            domains: HashMap::new(),
            page_tables: HashMap::new(),
            next_domain_id: 0,
            max_domains,
        }
    }

    pub fn status(&self) -> IommuStatus {
        self.status
    }

    pub fn allocate_domain(&mut self) -> Result<u32> {
        if self.next_domain_id >= self.max_domains {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Max domains ({}) reached",
                self.max_domains
            )));
        }

        let domain_id = self.next_domain_id;
        self.next_domain_id += 1;

        let region = DmaRegion::new(domain_id, 0);
        let page_table = PageTable::new(4096);

        self.domains.insert(domain_id, region);
        self.page_tables.insert(domain_id, page_table);

        Ok(domain_id)
    }

    pub fn free_domain(&mut self, domain_id: u32) -> Result<()> {
        if self.domains.contains_key(&domain_id) {
            self.domains.remove(&domain_id);
            self.page_tables.remove(&domain_id);
            Ok(())
        } else {
            Err(AIOSException::HardwareNotDetected(format!(
                "Domain {} not found",
                domain_id
            )))
        }
    }

    pub fn get_domain(&self, domain_id: u32) -> Option<&DmaRegion> {
        self.domains.get(&domain_id)
    }

    pub fn get_domain_mut(&mut self, domain_id: u32) -> Option<&mut DmaRegion> {
        self.domains.get_mut(&domain_id)
    }

    pub fn domain_count(&self) -> usize {
        self.domains.len()
    }

    pub fn disable(&mut self) {
        self.status = IommuStatus::Disabled;
    }

    pub fn enable(&mut self) {
        self.status = IommuStatus::Enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iommu_manager_creation() {
        let manager = IommuManager::new(256);
        assert_eq!(manager.status(), IommuStatus::Enabled);
        assert_eq!(manager.domain_count(), 0);
    }

    #[test]
    fn test_iommu_allocate_domain() {
        let mut manager = IommuManager::new(256);
        let domain_id = manager.allocate_domain().unwrap();

        assert_eq!(domain_id, 0);
        assert_eq!(manager.domain_count(), 1);
    }

    #[test]
    fn test_iommu_allocate_multiple() {
        let mut manager = IommuManager::new(256);

        let id1 = manager.allocate_domain().unwrap();
        let id2 = manager.allocate_domain().unwrap();
        let id3 = manager.allocate_domain().unwrap();

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(id3, 2);
        assert_eq!(manager.domain_count(), 3);
    }

    #[test]
    fn test_iommu_free_domain() {
        let mut manager = IommuManager::new(256);
        let domain_id = manager.allocate_domain().unwrap();

        assert!(manager.free_domain(domain_id).is_ok());
        assert_eq!(manager.domain_count(), 0);
    }

    #[test]
    fn test_iommu_get_domain() {
        let mut manager = IommuManager::new(256);
        let domain_id = manager.allocate_domain().unwrap();

        let domain = manager.get_domain(domain_id);
        assert!(domain.is_some());
    }

    #[test]
    fn test_iommu_status_changes() {
        let mut manager = IommuManager::new(256);

        assert_eq!(manager.status(), IommuStatus::Enabled);
        manager.disable();
        assert_eq!(manager.status(), IommuStatus::Disabled);
        manager.enable();
        assert_eq!(manager.status(), IommuStatus::Enabled);
    }

    #[test]
    fn test_iommu_max_domains() {
        let mut manager = IommuManager::new(2);

        assert!(manager.allocate_domain().is_ok());
        assert!(manager.allocate_domain().is_ok());
        assert!(manager.allocate_domain().is_err());
    }
}
