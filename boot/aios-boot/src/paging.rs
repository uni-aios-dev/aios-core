//! Page-table construction for the hand-off.
//!
//! The loader builds a small, private hierarchy before switching `cr3`:
//!
//! * a higher-half direct map of physical memory at [`HHDM_BASE`], using 2 MiB
//!   pages (`P | W | NX`) — the framebuffer gets `PCD | PWT` so writes bypass
//!   the cache;
//! * the kernel image at its link address with 4 KiB pages and W^X flags;
//! * the boot stack at [`BOOT_STACK_VIRT`];
//! * a single identity-mapped page holding [`crate::aios_switch_to_kernel`],
//!   the stub that performs the final `mov cr3` + `jmp`.
//!
//! All page-table frames come from one contiguous `allocate_pages` allocation
//! (`LOADER_DATA`), which the kernel's frame allocator therefore ignores.

/// One 4 KiB page.
pub const PAGE_SIZE: u64 = 0x1000;
/// One 2 MiB huge page.
pub const HUGE_PAGE_SIZE: u64 = 0x20_0000;

/// Page is present.
pub const PTE_PRESENT: u64 = 1 << 0;
/// Page is writable.
pub const PTE_WRITE: u64 = 1 << 1;
/// Page is user-accessible.
pub const PTE_USER: u64 = 1 << 2;
/// Write-through caching.
pub const PTE_PWT: u64 = 1 << 3;
/// Cache disabled.
pub const PTE_PCD: u64 = 1 << 4;
/// This entry maps a 2 MiB page (only valid at the PD level).
pub const PTE_HUGE: u64 = 1 << 7;
/// No-execute.
pub const PTE_NX: u64 = 1 << 63;

/// Physical-address mask of a page-table entry.
pub const PTE_ADDR: u64 = 0x000f_ffff_ffff_f000;

/// Direct-map base.
pub const HHDM_BASE: u64 = aios_bootinfo::HHDM_BASE;
/// Virtual address of the loader-provided kernel stack.
pub const BOOT_STACK_VIRT: u64 = 0xFFFF_FE00_0000_0000;

/// A bump allocator over one contiguous physical allocation of zeroed pages.
pub struct FramePool {
    next: u64,
    end: u64,
}

impl FramePool {
    /// Wraps `bytes` at physical address `start`; every page handed out is
    /// zeroed first.
    ///
    /// # Safety
    /// `start..start + bytes` must be a valid, writable physical range while
    /// the firmware identity map is active.
    pub unsafe fn new(start: u64, bytes: u64) -> Self {
        Self {
            next: start,
            end: start + bytes,
        }
    }

    /// Returns one zeroed 4 KiB frame, or `None` when the pool is exhausted.
    pub fn alloc(&mut self) -> Option<u64> {
        if self.next + PAGE_SIZE > self.end {
            return None;
        }
        let page = self.next;
        self.next += PAGE_SIZE;
        // SAFETY: the pool covers this page and the identity map is active.
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE as usize) };
        Some(page)
    }

    /// Frames still available in the pool.
    pub fn remaining(&self) -> u64 {
        (self.end - self.next) / PAGE_SIZE
    }
}

/// A 4-level page hierarchy built in a [`FramePool`].
pub struct PageTables {
    pml4: u64,
    pool: FramePool,
}

impl PageTables {
    /// Allocates the top-level PML4 from `pool`.
    pub fn new(pool: FramePool) -> Option<Self> {
        let mut pool = pool;
        let pml4 = pool.alloc()?;
        Some(Self { pml4, pool })
    }

    /// Physical address to load into `cr3`.
    pub const fn cr3(&self) -> u64 {
        self.pml4
    }

    /// Maps one 4 KiB page.
    pub fn map_4k(&mut self, virt: u64, phys: u64, flags: u64) -> Option<()> {
        if virt & (PAGE_SIZE - 1) != 0 || phys & (PAGE_SIZE - 1) != 0 {
            return None;
        }
        let pml4 = self.pml4;
        let pdpt = self.ensure(pml4, index(virt, 39))?;
        let pd = self.ensure(pdpt, index(virt, 30))?;
        let pt = self.ensure(pd, index(virt, 21))?;
        write_entry(pt, index(virt, 12), (phys & PTE_ADDR) | flags | PTE_PRESENT);
        Some(())
    }

    /// Maps one 2 MiB page. Both addresses must be 2 MiB aligned.
    pub fn map_2m(&mut self, virt: u64, phys: u64, flags: u64) -> Option<()> {
        let mask = HUGE_PAGE_SIZE - 1;
        if virt & mask != 0 || phys & mask != 0 {
            return None;
        }
        let pml4 = self.pml4;
        let pdpt = self.ensure(pml4, index(virt, 39))?;
        let pd = self.ensure(pdpt, index(virt, 30))?;
        write_entry(
            pd,
            index(virt, 21),
            (phys & PTE_ADDR) | flags | PTE_HUGE | PTE_PRESENT,
        );
        Some(())
    }

    /// Returns the next-level table for `entry`, allocating and linking a fresh
    /// zeroed frame when the slot is empty.
    fn ensure(&mut self, table: u64, slot: usize) -> Option<u64> {
        let entry = read_entry(table, slot);
        if entry & PTE_PRESENT != 0 {
            if entry & PTE_HUGE != 0 {
                return None;
            }
            return Some(entry & PTE_ADDR);
        }
        let next = self.pool.alloc()?;
        // Upper levels are always writable; a read-only leaf still wins for
        // its own page, which is exactly what W^X needs.
        write_entry(table, slot, next | PTE_PRESENT | PTE_WRITE);
        Some(next)
    }
}

fn index(address: u64, shift: u32) -> usize {
    ((address >> shift) & 0x1ff) as usize
}

fn read_entry(table: u64, slot: usize) -> u64 {
    // SAFETY: page-table frames are plain physical memory covered by the
    // firmware identity map until `cr3` is switched.
    unsafe { (table as *const u64).add(slot).read_volatile() }
}

fn write_entry(table: u64, slot: usize, value: u64) {
    // SAFETY: same as `read_entry`; the frame was zeroed when allocated.
    unsafe { (table as *mut u64).add(slot).write_volatile(value) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_match_x86_64_layout() {
        assert_eq!(index(0xffff_ffff_8000_0000, 39), 511);
        assert_eq!(index(0xffff_8000_0000_0000, 39), 256);
        assert_eq!(index(0xffff_fe00_0000_0000, 39), 508);
        assert_eq!(index(0xffff_ff00_0000_0000, 39), 510);
        assert_eq!(index(0xffff_ffff_8000_0000, 30), 510);
        assert_eq!(index(0xffff_ff00_0000_0000, 30), 0);
    }

    #[test]
    fn flags_are_distinct() {
        assert_ne!(PTE_PRESENT, PTE_WRITE);
        assert_eq!(PTE_NX, 1 << 63);
        assert_eq!(PTE_ADDR, 0x000f_ffff_ffff_f000);
    }
}
