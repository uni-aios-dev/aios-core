use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DmaAddress(u64);

impl DmaAddress {
    pub fn new(addr: u64) -> Result<Self> {
        if addr == 0 {
            return Err(AIOSException::HardwareNotDetected(
                "DMA address cannot be 0".to_string(),
            ));
        }
        Ok(DmaAddress(addr))
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for DmaAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmaPermission {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl DmaPermission {
    pub fn read_write() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
        }
    }

    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
        }
    }

    pub fn none() -> Self {
        Self {
            read: false,
            write: false,
            execute: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmaBuffer {
    pub host_addr: u64,
    pub dma_addr: DmaAddress,
    pub size_bytes: u64,
    pub permission: DmaPermission,
    pub mapped: bool,
}

impl DmaBuffer {
    pub fn new(
        host_addr: u64,
        dma_addr: DmaAddress,
        size_bytes: u64,
        permission: DmaPermission,
    ) -> Self {
        Self {
            host_addr,
            dma_addr,
            size_bytes,
            permission,
            mapped: false,
        }
    }

    pub fn map(&mut self) -> Result<()> {
        if self.mapped {
            return Err(AIOSException::HardwareNotDetected(
                "Buffer already mapped".to_string(),
            ));
        }
        self.mapped = true;
        Ok(())
    }

    pub fn contains_address(&self, addr: u64) -> bool {
        addr >= self.host_addr && addr < self.host_addr + self.size_bytes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmaRegion {
    pub region_id: u32,
    pub buffers: Vec<DmaBuffer>,
    pub device_id: u32,
}

impl DmaRegion {
    pub fn new(region_id: u32, device_id: u32) -> Self {
        Self {
            region_id,
            buffers: Vec::new(),
            device_id,
        }
    }

    pub fn add_buffer(&mut self, buffer: DmaBuffer) -> Result<()> {
        for existing in &self.buffers {
            if existing.dma_addr.value() == buffer.dma_addr.value() {
                return Err(AIOSException::HardwareNotDetected(
                    "DMA address already in use".to_string(),
                ));
            }
        }
        self.buffers.push(buffer);
        Ok(())
    }

    pub fn find_buffer_by_addr(&self, addr: u64) -> Option<&DmaBuffer> {
        self.buffers.iter().find(|b| b.contains_address(addr))
    }

    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    pub fn total_size(&self) -> u64 {
        self.buffers.iter().map(|b| b.size_bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dma_address_creation() {
        let addr = DmaAddress::new(0x1000).unwrap();
        assert_eq!(addr.value(), 0x1000);
    }

    #[test]
    fn test_dma_address_zero_invalid() {
        let result = DmaAddress::new(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_dma_permission_presets() {
        let rw = DmaPermission::read_write();
        assert!(rw.read && rw.write && !rw.execute);

        let ro = DmaPermission::read_only();
        assert!(ro.read && !ro.write && !ro.execute);

        let none = DmaPermission::none();
        assert!(!none.read && !none.write && !none.execute);
    }

    #[test]
    fn test_dma_buffer_creation() {
        let addr = DmaAddress::new(0x2000).unwrap();
        let buffer = DmaBuffer::new(0x1000, addr, 4096, DmaPermission::read_write());
        assert_eq!(buffer.size_bytes, 4096);
        assert!(!buffer.mapped);
    }

    #[test]
    fn test_dma_buffer_mapping() {
        let addr = DmaAddress::new(0x2000).unwrap();
        let mut buffer = DmaBuffer::new(0x1000, addr, 4096, DmaPermission::read_write());

        assert!(buffer.map().is_ok());
        assert!(buffer.mapped);

        let double_map = buffer.map();
        assert!(double_map.is_err());
    }

    #[test]
    fn test_dma_region_add_buffer() {
        let mut region = DmaRegion::new(0, 1);
        let addr1 = DmaAddress::new(0x1000).unwrap();
        let buffer1 = DmaBuffer::new(0x1000, addr1, 4096, DmaPermission::read_write());

        assert!(region.add_buffer(buffer1).is_ok());
        assert_eq!(region.buffer_count(), 1);
    }

    #[test]
    fn test_dma_region_total_size() {
        let mut region = DmaRegion::new(0, 1);
        let addr1 = DmaAddress::new(0x1000).unwrap();
        let addr2 = DmaAddress::new(0x2000).unwrap();
        let buffer1 = DmaBuffer::new(0x1000, addr1, 4096, DmaPermission::read_write());
        let buffer2 = DmaBuffer::new(0x5000, addr2, 8192, DmaPermission::read_only());

        region.add_buffer(buffer1).unwrap();
        region.add_buffer(buffer2).unwrap();

        assert_eq!(region.total_size(), 12288);
    }

    #[test]
    fn test_dma_buffer_contains_address() {
        let addr = DmaAddress::new(0x2000).unwrap();
        let buffer = DmaBuffer::new(0x1000, addr, 4096, DmaPermission::read_write());

        assert!(buffer.contains_address(0x1000));
        assert!(buffer.contains_address(0x1500));
        assert!(buffer.contains_address(0x1fff));
        assert!(!buffer.contains_address(0x2000));
        assert!(!buffer.contains_address(0x0fff));
    }
}
