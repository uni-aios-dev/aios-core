use crate::memory;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

/// Base virtual address of the kernel heap.
///
/// Lives in the unused PML4 slot 510 (below the kernel image at slot 511 and
/// above Limine's HHDM window), so it never collides with the direct map.
pub const HEAP_START: u64 = 0xFFFF_FF00_0000_0000;
/// Heap size: 4 MiB (v2.38.33, was 2 MiB). The freelist now coalesces every
/// adjacent pair on free (see [`FreeListAllocator::dealloc`]), but the larger
/// heap buys headroom for long GUI sessions where transient `format!` labels
/// of many sizes interleave with held buffers.
pub const HEAP_SIZE: u64 = 4 * 1024 * 1024;

const HEADER_SIZE: usize = 16;
const BLOCK_ALIGN: usize = 16;
const MIN_PAYLOAD: usize = 16;
const PAGE_SIZE: u64 = memory::PAGE_SIZE;

struct BlockHeader {
    size: usize,
    next: *mut BlockHeader,
}

struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn acquire(&self) -> SpinGuard<'_> {
        while self.locked.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }
        SpinGuard { lock: self }
    }

    unsafe fn release(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

struct SpinGuard<'a> {
    lock: &'a SpinLock,
}

impl Drop for SpinGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.lock.release();
        }
    }
}

struct FreeListAllocator {
    free_head: UnsafeCell<*mut BlockHeader>,
    lock: SpinLock,
}

unsafe impl Sync for FreeListAllocator {}

const fn min_block_size() -> usize {
    HEADER_SIZE + MIN_PAYLOAD
}

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[global_allocator]
static ALLOCATOR: FreeListAllocator = FreeListAllocator {
    free_head: UnsafeCell::new(ptr::null_mut()),
    lock: SpinLock::new(),
};

/// Maps heap frames and prepares the free list so that heap allocations become available.
pub fn init_heap() {
    let frames = (HEAP_SIZE / PAGE_SIZE) as usize;
    for i in 0..frames {
        let frame = memory::alloc_frame().expect("heap: out of frames");
        memory::map_page(HEAP_START + i as u64 * PAGE_SIZE, frame, false)
            .expect("heap: failed to map page");
    }
    unsafe {
        let head = HEAP_START as *mut BlockHeader;
        (*head).size = HEAP_SIZE as usize;
        (*head).next = ptr::null_mut();
        ALLOCATOR.free_head.get().write(head);
    }
}

/// Allocates and prints a few sample values to prove the heap works.
pub fn test_heap() {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    let mut vec = Vec::new();
    for i in 0..1000u64 {
        vec.push(i * 2);
    }
    crate::kprintln!(
        "[serial] heap: Vec<u64> 1000 elems, sum={}",
        vec.iter().sum::<u64>()
    );

    let mut text = String::from("heap string");
    text.push_str(" ok");
    crate::kprintln!("[serial] heap: String '{}'", text);

    let value = Box::new(3.25f64);
    crate::kprintln!("[serial] heap: Box<f64> {}", value);

    let mut total = 0u64;
    for i in 0..200 {
        let mut scratch = String::from("stress");
        for c in 0..(i % 40) {
            scratch.push((b'a' + (c % 26) as u8) as char);
        }
        total += scratch.len() as u64;
    }
    let mut final_vec = Vec::new();
    for i in 0..1000u64 {
        final_vec.push(i * 3);
    }
    crate::kprintln!(
        "[serial] heap: stress 200 strings len_sum={}, final Vec sum={}",
        total,
        final_vec.iter().sum::<u64>()
    );
}

unsafe impl GlobalAlloc for FreeListAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() > BLOCK_ALIGN {
            return ptr::null_mut();
        }
        let _guard = self.lock.acquire();
        let free_head = self.free_head.get();
        let needed = align_up(layout.size(), BLOCK_ALIGN).max(MIN_PAYLOAD) + HEADER_SIZE;
        // First-fit over the address-sorted free list. The list is kept sorted
        // by addr (dealloc inserts in order) so that neighbours are always
        // reachable and coalescing is complete; splitting leaves the remainder
        // at its original address, so the order survives allocs too.
        let mut prev: *mut BlockHeader = ptr::null_mut();
        let mut current = free_head.read();
        while !current.is_null() {
            if (*current).size >= needed {
                let next = (*current).next;
                let remaining = (*current).size - needed;
                if remaining >= min_block_size() {
                    (*current).size = needed;
                    let leftover = (current as *mut u8).add(needed) as *mut BlockHeader;
                    (*leftover).size = remaining;
                    (*leftover).next = next;
                    if prev.is_null() {
                        free_head.write(leftover);
                    } else {
                        (*prev).next = leftover;
                    }
                } else if prev.is_null() {
                    free_head.write(next);
                } else {
                    (*prev).next = next;
                }
                return (current as *mut u8).add(HEADER_SIZE);
            }
            prev = current;
            current = (*current).next;
        }
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, pointer: *mut u8, _layout: Layout) {
        let _guard = self.lock.acquire();
        let free_head = self.free_head.get();
        let block = (pointer as *mut BlockHeader).sub(1);
        // Insert into the address-sorted list, then coalesce with BOTH
        // neighbours when they are adjacent in memory. The old allocator only
        // ever merged the freed block with the list head, so interleaved free
        // patterns (alloc A, alloc B, free A, free B) accumulated
        // adjacent-but-unmerged fragments until a large allocation failed even
        // though the heap was mostly free — the long-run OOM panic seen on
        // real hardware (v2.38.33).
        let mut prev: *mut BlockHeader = ptr::null_mut();
        let mut cur = free_head.read();
        while !cur.is_null() && (cur as usize) < (block as usize) {
            prev = cur;
            cur = (*cur).next;
        }
        // `cur` is the next free block by address (or null); absorb it when
        // directly adjacent, otherwise link it behind the new block.
        let next = cur;
        if !next.is_null() && (block as usize) + (*block).size == (next as usize) {
            (*block).size += (*next).size;
            (*block).next = (*next).next;
        } else {
            (*block).next = next;
        }
        // Absorb the new block into the previous one when adjacent, else link.
        if prev.is_null() {
            free_head.write(block);
        } else if (prev as usize) + (*prev).size == (block as usize) {
            (*prev).size += (*block).size;
            (*prev).next = (*block).next;
        } else {
            (*prev).next = block;
        }
    }
}

/// Boot self-test for freelist coalescing (v2.38.33): carves most of the heap
/// into many same-size blocks, frees them in allocation order — the exact
/// pattern that used to leave adjacent-but-unmerged fragments — and then
/// demands one large contiguous block back. Uses `try_reserve_exact`, so a
/// regression reports instead of panicking. Logs
/// `[serial] heap: fragmentation self-test ok` (smoke-grepped) or `FAILED`.
pub fn fragmentation_selftest() {
    use alloc::vec::Vec;
    const BLOCKS: usize = 24;
    const BLOCK_BYTES: usize = 96 * 1024;
    const BIG_BYTES: usize = 1024 * 1024 + 512 * 1024;
    let mut ok = true;
    {
        let mut held: Vec<Vec<u8>> = Vec::new();
        for _ in 0..BLOCKS {
            let mut v: Vec<u8> = Vec::new();
            if v.try_reserve_exact(BLOCK_BYTES).is_err() {
                ok = false;
                break;
            }
            v.resize(BLOCK_BYTES, 0xA5);
            held.push(v);
        }
        // Forward-order drop: every free is "not adjacent to the head" on a
        // head-only coalescing allocator, maximising fragmentation there.
        drop(held);
    }
    let mut big: Vec<u8> = Vec::new();
    if big.try_reserve_exact(BIG_BYTES).is_err() {
        ok = false;
    } else if ok {
        big.resize(BIG_BYTES, 0x5A);
    }
    if ok {
        crate::kprintln!(
            "[serial] heap: fragmentation self-test ok ({} x 96KiB freed, 1.5MiB re-acquired)",
            BLOCKS
        );
    } else {
        crate::kprintln!("[serial] heap: fragmentation self-test FAILED (coalescing regression)");
    }
}
