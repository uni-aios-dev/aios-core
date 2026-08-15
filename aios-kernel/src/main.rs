#![no_std]
#![no_main]

mod serial;
mod vga;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{self, entry_point};
use core::panic::PanicInfo;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

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

    halt_loop();
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
