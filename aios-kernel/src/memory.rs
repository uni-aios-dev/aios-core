use core::arch::asm;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A usable physical memory region, `[start, end)`, page-aligned by the caller.
///
/// Built by `main.rs` from the Limine memory map (`MEMMAP_USABLE` entries only),
/// which keeps this module independent of the boot protocol.
#[derive(Clone, Copy)]
pub struct MemRegion {
    /// First physical byte of the region.
    pub start: u64,
    /// One past the last physical byte of the region.
    pub end: u64,
}

pub const PAGE_SIZE: u64 = 0x1000;
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_USER: u64 = 1 << 2;
const PTE_HUGE: u64 = 1 << 7;
const PTE_FRAME: u64 = 0x000f_ffff_ffff_f000;
const INDEX_MASK: u64 = 0x1ff;
const MAX_FRAME_REGIONS: usize = 16;

static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);
static FRAME_NEXT: AtomicU64 = AtomicU64::new(0);
static FRAME_REGION: AtomicUsize = AtomicUsize::new(0);
/// Total physical frames allocated since boot (bump allocator, never freed).
static FRAMES_ALLOC: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct FrameRegion {
    start: u64,
    end: u64,
}

static mut FRAME_REGIONS: [FrameRegion; MAX_FRAME_REGIONS] =
    [FrameRegion { start: 0, end: 0 }; MAX_FRAME_REGIONS];
static mut FRAME_REGION_COUNT: usize = 0;

/// Initializes the memory manager from the bootloader HHDM offset and the list
/// of usable physical regions.
pub fn init(physical_offset: u64, regions: &[MemRegion]) {
    PHYS_OFFSET.store(physical_offset, Ordering::Relaxed);
    let mut count = 0;
    for region in regions {
        if count >= MAX_FRAME_REGIONS {
            break;
        }
        let start = align_up(region.start, PAGE_SIZE);
        let end = align_down(region.end, PAGE_SIZE);
        if start < end {
            unsafe {
                FRAME_REGIONS[count] = FrameRegion { start, end };
            }
            count += 1;
        }
    }
    unsafe {
        FRAME_REGION_COUNT = count;
    }
    if count > 0 {
        FRAME_NEXT.store(unsafe { FRAME_REGIONS[0].start }, Ordering::Relaxed);
    }
}

/// Returns the number of usable frame regions tracked by the frame allocator.
pub fn frame_region_count() -> usize {
    unsafe { FRAME_REGION_COUNT }
}

/// Offsets a physical address with the bootloader HHDM base.
///
/// Used by device drivers to reach physically-contiguous DMA buffers, which
/// live in usable RAM and are therefore covered by the HHDM. MMIO registers are
/// *not* covered by the HHDM and must go through [`map_mmio`].
pub fn physical_to_virtual(phys: u64) -> u64 {
    PHYS_OFFSET.load(Ordering::Relaxed) + phys
}

/// Base of the dedicated MMIO window in PML4 slot 510.
///
/// The boot HHDM only maps RAM, so device registers reached through a PCI BAR
/// must be mapped explicitly. The window sits above the kernel heap and the
/// paging self-test page, both of which also live in slot 510.
const MMIO_BASE: u64 = 0xFFFF_FF00_2000_0000;
static MMIO_NEXT: AtomicU64 = AtomicU64::new(MMIO_BASE);

/// Maps `size` bytes of device memory starting at physical `phys` and returns
/// the virtual address that aliases `phys`.
///
/// The mapping is placed in a dedicated supervisor-only MMIO window; the offset
/// of `phys` within its first page is preserved. Each call consumes fresh
/// virtual space so that several devices never alias.
pub fn map_mmio(phys: u64, size: u64) -> Result<u64, &'static str> {
    let start = align_down(phys, PAGE_SIZE);
    let offset = phys - start;
    let pages = (offset + size).div_ceil(PAGE_SIZE);
    let virt = MMIO_NEXT.fetch_add(pages * PAGE_SIZE, Ordering::Relaxed);
    for page in 0..pages {
        map_page(virt + page * PAGE_SIZE, start + page * PAGE_SIZE, false)?;
    }
    Ok(virt + offset)
}

/// Allocates a single physical page frame, returning its physical address.
pub fn alloc_frame() -> Option<u64> {
    loop {
        let count = unsafe { FRAME_REGION_COUNT };
        let region = FRAME_REGION.load(Ordering::Relaxed);
        if region >= count {
            return None;
        }
        let current = unsafe { FRAME_REGIONS[region] };
        let next = FRAME_NEXT.load(Ordering::Relaxed);
        if next >= current.end {
            if region + 1 >= count {
                return None;
            }
            FRAME_REGION.store(region + 1, Ordering::Relaxed);
            FRAME_NEXT.store(
                unsafe { FRAME_REGIONS[region + 1].start },
                Ordering::Relaxed,
            );
            continue;
        }
        FRAME_NEXT.store(next + PAGE_SIZE, Ordering::Relaxed);
        FRAMES_ALLOC.fetch_add(1, Ordering::Relaxed);
        return Some(next);
    }
}

/// Number of physical frames handed out since boot (diagnostics/TUI display).
pub fn frames_allocated() -> u64 {
    FRAMES_ALLOC.load(Ordering::Relaxed)
}

/// Translates a virtual address to its physical address by walking the current page tables.
pub fn translate(addr: u64) -> Option<u64> {
    let pml4 = cr3() & PTE_FRAME;
    unsafe {
        let pml4e = read_entry(pml4, (addr >> 39) & INDEX_MASK);
        if pml4e & PTE_PRESENT == 0 {
            return None;
        }
        let pdpt = pml4e & PTE_FRAME;
        let pdpte = read_entry(pdpt, (addr >> 30) & INDEX_MASK);
        if pdpte & PTE_PRESENT == 0 {
            return None;
        }
        if pdpte & PTE_HUGE != 0 {
            return Some((pdpte & PTE_FRAME) | (addr & 0x3fff_ffff));
        }
        let pd = pdpte & PTE_FRAME;
        let pde = read_entry(pd, (addr >> 21) & INDEX_MASK);
        if pde & PTE_PRESENT == 0 {
            return None;
        }
        if pde & PTE_HUGE != 0 {
            return Some((pde & PTE_FRAME) | (addr & 0x1f_ffff));
        }
        let pt = pde & PTE_FRAME;
        let pte = read_entry(pt, (addr >> 12) & INDEX_MASK);
        if pte & PTE_PRESENT == 0 {
            return None;
        }
        Some((pte & PTE_FRAME) | (addr & 0xfff))
    }
}

/// Maps a single page, allocating page-table frames as needed.
///
/// Upper-level entries (PML4E/PDPTE/PDE) created by the bootloader are NOT
/// user-accessible by default; when `user` is requested the USER flag is
/// forced into every level the walk touches so ring-3 can actually traverse
/// the table (a single supervisor upper entry faults the whole walk).
pub fn map_page(virt: u64, phys: u64, user: bool) -> Result<(), &'static str> {
    if !is_page_aligned(virt) || !is_page_aligned(phys) {
        return Err("map_page: address not page aligned");
    }
    let flags = PTE_PRESENT | PTE_WRITABLE | if user { PTE_USER } else { 0 };
    let pml4 = cr3() & PTE_FRAME;
    unsafe {
        let idx0 = (virt >> 39) & INDEX_MASK;
        let mut pml4e = ensure_table(pml4, idx0, flags)?;
        if user {
            write_entry(pml4, idx0, pml4e | PTE_USER);
            pml4e |= PTE_USER;
        }
        let idx1 = (virt >> 30) & INDEX_MASK;
        let mut pdpte = ensure_table(pml4e & PTE_FRAME, idx1, flags)?;
        if user {
            write_entry(pml4e & PTE_FRAME, idx1, pdpte | PTE_USER);
            pdpte |= PTE_USER;
        }
        if pdpte & PTE_HUGE != 0 {
            return Err("map_page: 1GiB page in path");
        }
        let idx2 = (virt >> 21) & INDEX_MASK;
        let mut pde = ensure_table(pdpte & PTE_FRAME, idx2, flags)?;
        if user {
            write_entry(pdpte & PTE_FRAME, idx2, pde | PTE_USER);
            pde |= PTE_USER;
        }
        if pde & PTE_HUGE != 0 {
            return Err("map_page: 2MiB page in path");
        }
        let pt = pde & PTE_FRAME;
        write_entry(pt, (virt >> 12) & INDEX_MASK, phys | flags);
    }
    invlpg(virt);
    Ok(())
}

/// Unmaps a single page and flushes the corresponding TLB entry.
pub fn unmap_page(virt: u64) {
    if !is_page_aligned(virt) {
        return;
    }
    let pml4 = cr3() & PTE_FRAME;
    unsafe {
        let pml4e = read_entry(pml4, (virt >> 39) & INDEX_MASK);
        if pml4e & PTE_PRESENT == 0 {
            return;
        }
        let pdpte = read_entry(pml4e & PTE_FRAME, (virt >> 30) & INDEX_MASK);
        if pdpte & PTE_PRESENT == 0 || pdpte & PTE_HUGE != 0 {
            return;
        }
        let pde = read_entry(pdpte & PTE_FRAME, (virt >> 21) & INDEX_MASK);
        if pde & PTE_PRESENT == 0 || pde & PTE_HUGE != 0 {
            return;
        }
        let pt = pde & PTE_FRAME;
        let index = (virt >> 12) & INDEX_MASK;
        if read_entry(pt, index) & PTE_PRESENT != 0 {
            write_entry(pt, index, 0);
            invlpg(virt);
        }
    }
}

unsafe fn ensure_table(parent: u64, index: u64, flags: u64) -> Result<u64, &'static str> {
    let entry = read_entry(parent, index);
    if entry & PTE_PRESENT != 0 {
        return Ok(entry);
    }
    let frame = alloc_frame().ok_or("out of memory for page table")?;
    let table = phys_to_virt(frame) as *mut u64;
    for i in 0..512 {
        table.add(i).write_volatile(0);
    }
    write_entry(parent, index, frame | flags);
    Ok(frame | flags)
}

unsafe fn read_entry(table_phys: u64, index: u64) -> u64 {
    let addr = (phys_to_virt(table_phys) + index * 8) as *const u64;
    addr.read_volatile()
}

unsafe fn write_entry(table_phys: u64, index: u64, value: u64) {
    let addr = (phys_to_virt(table_phys) + index * 8) as *mut u64;
    addr.write_volatile(value);
}

fn phys_to_virt(phys: u64) -> u64 {
    PHYS_OFFSET.load(Ordering::Relaxed) + phys
}

fn cr3() -> u64 {
    let out: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) out, options(nostack, preserves_flags));
    }
    out
}

fn invlpg(addr: u64) {
    unsafe {
        asm!("invlpg [{0}]", in(reg) addr, options(nostack, preserves_flags));
    }
}

const fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

const fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

const fn is_page_aligned(addr: u64) -> bool {
    addr & (PAGE_SIZE - 1) == 0
}

/// Virtual address used by the paging self-test.
///
/// Deliberately placed in the otherwise-unused PML4 slot just below the kernel
/// image (index 510) so the test never collides with Limine's HHDM (which fills
/// the lower half of the kernel half of the address space) nor with the kernel
/// image itself (slot 511).
const SELFTEST_ADDR: u64 = 0xFFFF_FF00_1000_0000;
const SELFTEST_PATTERN: u64 = 0xDEAD_BEEF_CAFE_F00D;

/// Exercises map/unmap and translate on a dedicated virtual page.
pub fn selftest() -> Result<(), &'static str> {
    if translate(SELFTEST_ADDR).is_some() {
        return Err("selftest: target virtual address already mapped");
    }
    let frame = alloc_frame().ok_or("selftest: no frames available")?;
    map_page(SELFTEST_ADDR, frame, false)?;
    let target = SELFTEST_ADDR as *mut u64;
    unsafe {
        target.write_volatile(SELFTEST_PATTERN);
        if target.read_volatile() != SELFTEST_PATTERN {
            return Err("selftest: read-back mismatch");
        }
    }
    if translate(SELFTEST_ADDR) != Some(frame) {
        return Err("selftest: translate does not match mapped frame");
    }
    unmap_page(SELFTEST_ADDR);
    if translate(SELFTEST_ADDR).is_some() {
        return Err("selftest: unmap did not take effect");
    }
    Ok(())
}
