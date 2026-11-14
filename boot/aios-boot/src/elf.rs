//! Minimal ELF64 loader.
//!
//! The kernel is a static `ET_EXEC` image: no interpreter, no relocations. The
//! loader only needs to find the `PT_LOAD` segments, check that they land in the
//! kernel PML4 slot, copy their file-backed bytes into a freshly allocated
//! physical buffer and derive per-page W^X flags from `p_flags`.

use crate::BootError;
use alloc::vec::Vec;

/// `PT_LOAD`.
const PT_LOAD: u32 = 1;
/// `PF_X`.
const PF_X: u32 = 1;
/// `PF_W`.
const PF_W: u32 = 2;

/// Lowest virtual address accepted for a kernel segment.
///
/// `0xFFFF_FFFF_8000_0000` is the first byte of the upper half of PML4 slot
/// 511. Everything below it is reserved for the direct map (slot 256), the boot
/// stack (slot 508) and the kernel heap/MMIO window (slot 510).
pub const KERNEL_MIN_VADDR: u64 = 0xFFFF_FFFF_8000_0000;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

/// One loadable segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Link-time virtual address.
    pub vaddr: u64,
    /// Offset of the segment in the file.
    pub offset: u64,
    /// Number of bytes stored in the file.
    pub filesz: u64,
    /// Number of bytes occupied in memory (`filesz` plus BSS).
    pub memsz: u64,
    /// ELF `p_flags` (`PF_X`, `PF_W`, `PF_R`).
    pub flags: u32,
}

impl Segment {
    /// Whether the segment is executable.
    pub fn is_executable(&self) -> bool {
        self.flags & PF_X != 0
    }

    /// Whether the segment is writable.
    pub fn is_writable(&self) -> bool {
        self.flags & PF_W != 0
    }

    /// First page touched by the segment.
    pub fn first_page(&self) -> u64 {
        self.vaddr & !0xfff
    }

    /// One past the last page touched by the segment.
    pub fn end_page(&self) -> u64 {
        (self.vaddr + self.memsz).div_ceil(0x1000) * 0x1000
    }
}

/// A parsed kernel image.
#[derive(Debug, Clone)]
pub struct Elf {
    /// Entry point (virtual).
    pub entry: u64,
    /// Page-aligned first virtual address of the image.
    pub base: u64,
    /// Page-aligned image size.
    pub size: u64,
    /// Loadable segments.
    pub segments: Vec<Segment>,
}

/// Per-page access rights extracted from the ELF segments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageRights {
    /// Page must be writable.
    pub writable: bool,
    /// Page must be executable.
    pub executable: bool,
}

impl Elf {
    /// Parses and validates the ELF image.
    pub fn parse(data: &[u8]) -> Result<Self, BootError> {
        if data.len() < EHDR_SIZE || &data[0..4] != b"\x7fELF" {
            return Err(BootError::Kernel);
        }
        if data[4] != 2 || data[5] != 1 {
            // 64-bit, little-endian only.
            return Err(BootError::Kernel);
        }
        if u16_at(data, 16)? != 2 || u16_at(data, 18)? != 0x3e {
            // ET_EXEC, EM_X86_64.
            return Err(BootError::Kernel);
        }

        let entry = u64_at(data, 24)?;
        let phoff = u64_at(data, 32)?;
        let phentsize = u16_at(data, 54)? as usize;
        let phnum = u16_at(data, 56)? as usize;
        if phentsize < PHDR_SIZE || phnum == 0 {
            return Err(BootError::Kernel);
        }
        let table_end = phoff
            .checked_add((phentsize * phnum) as u64)
            .ok_or(BootError::Kernel)?;
        if table_end > data.len() as u64 {
            return Err(BootError::Kernel);
        }

        let mut segments = Vec::new();
        for index in 0..phnum {
            let offset = phoff as usize + index * phentsize;
            if u32_at(data, offset)? != PT_LOAD {
                continue;
            }
            let flags = u32_at(data, offset + 4)?;
            let seg = Segment {
                offset: u64_at(data, offset + 8)?,
                vaddr: u64_at(data, offset + 16)?,
                filesz: u64_at(data, offset + 32)?,
                memsz: u64_at(data, offset + 40)?,
                flags,
            };
            if seg.memsz < seg.filesz {
                return Err(BootError::Kernel);
            }
            let file_end = seg
                .offset
                .checked_add(seg.filesz)
                .ok_or(BootError::Kernel)?;
            if file_end > data.len() as u64 {
                return Err(BootError::Kernel);
            }
            let mem_end = seg.vaddr.checked_add(seg.memsz).ok_or(BootError::Kernel)?;
            if seg.vaddr < KERNEL_MIN_VADDR || mem_end <= seg.vaddr {
                return Err(BootError::Kernel);
            }
            segments.push(seg);
        }
        if segments.is_empty() {
            return Err(BootError::Kernel);
        }

        let base = segments.iter().map(|s| s.first_page()).min().unwrap_or(0);
        let end = segments.iter().map(|s| s.end_page()).max().unwrap_or(0);
        let size = end.checked_sub(base).ok_or(BootError::Kernel)?;
        if size == 0 || entry < base || entry >= end {
            return Err(BootError::Kernel);
        }

        Ok(Self {
            entry,
            base,
            size,
            segments,
        })
    }

    /// Number of 4 KiB pages the loaded image occupies.
    pub fn page_count(&self) -> u64 {
        self.size / 0x1000
    }

    /// Copies every file-backed byte into `phys_base + (vaddr - base)`.
    ///
    /// # Safety
    /// `phys_base` must point to at least [`Elf::size`] bytes of writable,
    /// zero-initialised physical memory.
    pub unsafe fn copy_into(&self, data: &[u8], phys_base: u64) -> Result<(), BootError> {
        for segment in &self.segments {
            let src = usize::try_from(segment.offset).map_err(|_| BootError::Kernel)?;
            let len = usize::try_from(segment.filesz).map_err(|_| BootError::Kernel)?;
            let dst = phys_base + (segment.vaddr - self.base);
            // SAFETY: the caller guarantees the destination buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr().add(src), dst as *mut u8, len);
            }
        }
        Ok(())
    }

    /// Access rights for one page of the image (`base..base + size`).
    pub fn rights(&self, page: u64) -> PageRights {
        let mut rights = PageRights::default();
        for segment in &self.segments {
            if page >= segment.first_page() && page < segment.end_page() {
                rights.writable |= segment.is_writable();
                rights.executable |= segment.is_executable();
            }
        }
        // Never hand out a page that is both writable and executable.
        if rights.writable && rights.executable {
            rights.writable = false;
        }
        rights
    }
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, BootError> {
    data.get(offset..offset + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(BootError::Kernel)
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32, BootError> {
    data.get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(BootError::Kernel)
}

fn u64_at(data: &[u8], offset: usize) -> Result<u64, BootError> {
    data.get(offset..offset + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or(BootError::Kernel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Builds a minimal kernel ELF with one RX segment and one RW segment.
    fn sample_elf() -> Vec<u8> {
        let phnum = 2usize;
        let phoff = EHDR_SIZE;
        let text_off = 0x1000usize;
        let data_off = 0x2000usize;
        let mut data = vec![0u8; 0x2000 + 0x40];

        data[0..4].copy_from_slice(b"\x7fELF");
        data[4] = 2;
        data[5] = 1;
        data[6] = 1;
        data[16..18].copy_from_slice(&2u16.to_le_bytes());
        data[18..20].copy_from_slice(&0x3eu16.to_le_bytes());
        data[20..24].copy_from_slice(&1u32.to_le_bytes());
        data[24..32].copy_from_slice(&0xffff_ffff_8000_1000u64.to_le_bytes());
        data[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        data[52..54].copy_from_slice(&(EHDR_SIZE as u16).to_le_bytes());
        data[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        data[56..58].copy_from_slice(&(phnum as u16).to_le_bytes());

        let text = phoff;
        data[text..text + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        data[text + 4..text + 8].copy_from_slice(&5u32.to_le_bytes());
        data[text + 8..text + 16].copy_from_slice(&(text_off as u64).to_le_bytes());
        data[text + 16..text + 24].copy_from_slice(&0xffff_ffff_8000_0000u64.to_le_bytes());
        data[text + 32..text + 40].copy_from_slice(&0x80u64.to_le_bytes());
        data[text + 40..text + 48].copy_from_slice(&0x180u64.to_le_bytes());

        let rodata = phoff + PHDR_SIZE;
        data[rodata..rodata + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        data[rodata + 4..rodata + 8].copy_from_slice(&6u32.to_le_bytes());
        data[rodata + 8..rodata + 16].copy_from_slice(&(data_off as u64).to_le_bytes());
        data[rodata + 16..rodata + 24].copy_from_slice(&0xffff_ffff_8000_1000u64.to_le_bytes());
        data[rodata + 32..rodata + 40].copy_from_slice(&0x40u64.to_le_bytes());
        data[rodata + 40..rodata + 48].copy_from_slice(&0x100u64.to_le_bytes());

        for index in 0..0x80 {
            data[text_off + index] = 0x90;
        }
        for index in 0..0x40 {
            data[data_off + index] = 0xaa;
        }
        data
    }

    #[test]
    fn parses_a_kernel_image() {
        let elf = Elf::parse(&sample_elf()).unwrap();
        assert_eq!(elf.base, 0xffff_ffff_8000_0000);
        assert_eq!(elf.entry, 0xffff_ffff_8000_1000);
        assert_eq!(elf.segments.len(), 2);
        assert_eq!(elf.size, 0x2000);
        assert_eq!(elf.page_count(), 2);
    }

    #[test]
    fn derives_w_xor_x_rights() {
        let elf = Elf::parse(&sample_elf()).unwrap();
        let text = elf.rights(0xffff_ffff_8000_0000);
        assert!(text.executable && !text.writable);
        let data = elf.rights(0xffff_ffff_8000_1000);
        assert!(data.writable && !data.executable);
        let untouched = elf.rights(0xffff_ffff_8000_2000);
        assert!(!untouched.executable && !untouched.writable);
    }

    #[test]
    fn rejects_bad_images() {
        assert!(Elf::parse(b"not an elf").is_err());

        let mut bad = sample_elf();
        bad[4] = 1; // 32-bit
        assert!(Elf::parse(&bad).is_err());

        let mut bad = sample_elf();
        bad[18] = 0x28; // AArch64
        assert!(Elf::parse(&bad).is_err());

        let mut bad = sample_elf();
        bad[24..32].copy_from_slice(&0xffff_ffff_9000_0000u64.to_le_bytes());
        assert!(Elf::parse(&bad).is_err());
    }

    #[test]
    fn rejects_low_virtual_addresses() {
        let mut bad = sample_elf();
        bad[64 + 16..64 + 24].copy_from_slice(&0x0000_0000_4000_0000u64.to_le_bytes());
        assert!(Elf::parse(&bad).is_err());
    }

    #[test]
    fn rejects_truncated_segments() {
        let mut bad = sample_elf();
        bad[64 + 32..64 + 40].copy_from_slice(&0x10000u64.to_le_bytes());
        assert!(Elf::parse(&bad).is_err());
    }
}
