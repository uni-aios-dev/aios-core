use bootloader_api::info::MemoryRegion;
use bootloader_api::info::MemoryRegionKind;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

#[derive(Clone, Copy)]
struct FrameRegion {
    start: u64,
    end: u64,
}

static mut FRAME_REGIONS: [FrameRegion; MAX_FRAME_REGIONS] =
    [FrameRegion { start: 0, end: 0 }; MAX_FRAME_REGIONS];
static mut FRAME_REGION_COUNT: usize = 0;

/// Initializes the memory manager from the boot info physical memory offset and memory map.
pub fn init(physical_offset: u64, regions: &[MemoryRegion]) {
    PHYS_OFFSET.store(physical_offset, Ordering::Relaxed);
    let mut count = 0;
    for region in regions {
        if region.kind != MemoryRegionKind::Usable || count >= MAX_FRAME_REGIONS {
            continue;
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
        return Some(next);
    }
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
pub fn map_page(virt: u64, phys: u64, user: bool) -> Result<(), &'static str> {
    if !is_page_aligned(virt) || !is_page_aligned(phys) {
        return Err("map_page: address not page aligned");
    }
    let flags = PTE_PRESENT | PTE_WRITABLE | if user { PTE_USER } else { 0 };
    let pml4 = cr3() & PTE_FRAME;
    unsafe {
        let pml4e = ensure_table(pml4, (virt >> 39) & INDEX_MASK, flags)?;
        let pdpte = ensure_table(pml4e & PTE_FRAME, (virt >> 30) & INDEX_MASK, flags)?;
        if pdpte & PTE_HUGE != 0 {
            return Err("map_page: 1GiB page in path");
        }
        let pde = ensure_table(pdpte & PTE_FRAME, (virt >> 21) & INDEX_MASK, flags)?;
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
    Ok(frame)
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

const SELFTEST_ADDR: u64 = 0xFFFF_8800_0000_0000;
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
