#![no_std]
#![no_main]

extern crate alloc;

mod gdt;
mod heap;
mod idt;
mod interrupts;
mod memory;
mod port;
mod serial;
mod vga;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{self, entry_point};
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

const KERNEL_STACK_SIZE: usize = 64 * 1024;
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(align(16))]
#[allow(dead_code)]
struct KernelStack([u8; KERNEL_STACK_SIZE]);

#[repr(align(16))]
#[allow(dead_code)]
struct DoubleFaultStack([u8; DOUBLE_FAULT_STACK_SIZE]);

static KERNEL_STACK: KernelStack = KernelStack([0; KERNEL_STACK_SIZE]);
static DOUBLE_FAULT_STACK: DoubleFaultStack = DoubleFaultStack([0; DOUBLE_FAULT_STACK_SIZE]);

fn kernel_main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    let phys_offset = boot_info.physical_memory_offset.into_option().unwrap_or(0);
    serial::init();
    vga::vga_init(phys_offset);
    vga::vga_clear_screen();

    vprintln!("AIOS kernel booting...");
    kprintln!("[serial] AIOS kernel booting...");

    let regions = boot_info.memory_regions.len();
    vprintln!("Memory regions: {}", regions);
    kprintln!("[serial] memory regions = {}", regions);

    vprintln!("Physical memory offset: 0x{:x}", phys_offset);
    kprintln!("[serial] physical memory offset = 0x{:x}", phys_offset);

    match boot_info.framebuffer.as_ref() {
        Some(fb) => {
            let info = fb.info();
            vprintln!("Framebuffer: {}x{}", info.width, info.height);
            kprintln!("[serial] framebuffer = {}x{}", info.width, info.height);
        }
        None => {
            vprintln!("Framebuffer: none");
            kprintln!("[serial] framebuffer = none");
        }
    }

    match boot_info.rsdp_addr.into_option() {
        Some(addr) => {
            vprintln!("RSDP (ACPI): 0x{:x}", addr);
            kprintln!("[serial] rsdp = 0x{:x}", addr);
        }
        None => {
            vprintln!("RSDP (ACPI): none");
            kprintln!("[serial] rsdp = none");
        }
    }

    vprintln!("Milestone 0 OK: serial + VGA console live.");
    kprintln!("[serial] Milestone 0 OK.");

    memory::init(phys_offset, &boot_info.memory_regions);
    vprintln!(
        "Memory manager: {} usable frame regions",
        memory::frame_region_count()
    );
    kprintln!(
        "[serial] memory manager init, usable regions = {}",
        memory::frame_region_count()
    );

    let vga_translated = memory::translate(0xB8000);
    let kernel_virt = &BOOTLOADER_CONFIG as *const BootloaderConfig as u64;
    let kernel_translated = memory::translate(kernel_virt);
    let heap_translated = memory::translate(heap::HEAP_START);
    vprintln!(
        "translate: vga=0x{:x?} kernel=0x{:x?} heap_unmapped=0x{:x?}",
        vga_translated,
        kernel_translated,
        heap_translated
    );
    kprintln!(
        "[serial] translate vga=0x{:x?} kernel=0x{:x?} heap_before=0x{:x?}",
        vga_translated,
        kernel_translated,
        heap_translated
    );

    match memory::selftest() {
        Ok(()) => {
            vprintln!("Paging selftest: OK (map/write/read/translate/unmap)");
            kprintln!("[serial] paging selftest OK.");
        }
        Err(e) => {
            vprintln!("Paging selftest FAILED: {}", e);
            kprintln!("[serial] paging selftest FAILED: {}", e);
        }
    }

    heap::init_heap();
    heap::test_heap();
    vprintln!(
        "Heap: {} MiB mapped at 0x{:x}",
        heap::HEAP_SIZE / 1024 / 1024,
        heap::HEAP_START
    );
    kprintln!("[serial] heap online.");
    vprintln!("Milestone 2: paging + kernel heap online");
    kprintln!("[serial] Milestone 2: paging + kernel heap online.");

    let kernel_stack_top = &KERNEL_STACK as *const KernelStack as u64 + KERNEL_STACK_SIZE as u64;
    let double_fault_stack_top =
        &DOUBLE_FAULT_STACK as *const DoubleFaultStack as u64 + DOUBLE_FAULT_STACK_SIZE as u64;

    idt::init();
    gdt::init(double_fault_stack_top);
    gdt::set_kernel_stack(kernel_stack_top);
    interrupts::init_pic();
    interrupts::init_pit();

    vprintln!("Milestone 1: interrupts online (GDT/TSS, IDT, PIC, PIT, keyboard)");
    kprintln!("[serial] Milestone 1: interrupts online.");

    unsafe {
        core::arch::asm!("sti", options(nostack, preserves_flags));
    }

    idle_loop();
}

fn idle_loop() -> ! {
    let mut last_tick_print = 0u64;
    let mut last_scancode = 0u64;
    loop {
        let ticks = interrupts::TICKS.load(Ordering::Relaxed);
        if ticks >= interrupts::TIMER_HZ && ticks - last_tick_print >= interrupts::TIMER_HZ {
            vprintln!("[tick] {}s", ticks / interrupts::TIMER_HZ);
            kprintln!("[serial] tick {}s", ticks / interrupts::TIMER_HZ);
            last_tick_print = ticks;
        }
        let sc = interrupts::LAST_SCANCODE.load(Ordering::Relaxed);
        if sc != last_scancode {
            last_scancode = sc;
            if sc & 0x80 == 0 {
                if let Some(c) = interrupts::scancode_to_char(sc as u8) {
                    vprintln!("[key] '{}' (0x{:02x})", c, sc);
                    kprintln!("[serial] key '{}' (0x{:02x})", c, sc);
                } else {
                    vprintln!("[key] scancode 0x{:02x}", sc);
                    kprintln!("[serial] key scancode 0x{:02x}", sc);
                }
            }
        }
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

fn halt_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vprintln!("KERNEL PANIC: {}", info);
    kprintln!("[serial] KERNEL PANIC: {}", info);
    halt_loop();
}

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);
