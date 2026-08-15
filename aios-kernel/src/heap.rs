use crate::memory;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

pub const HEAP_START: u64 = 0xFFFF_8400_0000_0000;
pub const HEAP_SIZE: u64 = 2 * 1024 * 1024;

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
        let mut prev: *mut BlockHeader = ptr::null_mut();
        let mut current = free_head.read();
        while !current.is_null() {
            if (*current).size >= needed {
                let block = current;
                let remaining = (*block).size - needed;
                (*block).size = needed;
                if prev.is_null() {
                    free_head.write((*block).next);
                } else {
                    (*prev).next = (*block).next;
                }
                if remaining >= min_block_size() {
                    let leftover = (block as *mut u8).add(needed) as *mut BlockHeader;
                    (*leftover).size = remaining;
                    (*leftover).next = free_head.read();
                    free_head.write(leftover);
                }
                return (block as *mut u8).add(HEADER_SIZE);
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
        let adjacent = free_head.read() as usize == (block as usize) + (*block).size;
        if adjacent {
            (*block).size += (*free_head.read()).size;
            (*block).next = (*free_head.read()).next;
        } else {
            (*block).next = free_head.read();
        }
        free_head.write(block);
    }
}
