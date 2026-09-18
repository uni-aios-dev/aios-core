#![no_std]
#![no_main]

extern crate alloc;

mod ahci;
mod console;
mod font8x8;
mod framebuffer;
mod gdt;
mod heap;
mod idt;
mod interrupts;
mod memory;
mod pci;
mod port;
mod sched;
mod serial;
mod syscalls;
mod user;

use crate::framebuffer::{colors, Framebuffer};
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;
use limine::request::{
    EntryPointRequest, FramebufferRequest, HhdmRequest, MemmapRequest, RsdpRequest,
    StackSizeRequest,
};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

// --- Limine boot protocol requests ----------------------------------------
//
// All of these live in the `.requests*` sections that `linker.ld` keeps alive.
// Limine scans them between the start/end markers and fills in the response
// pointers before entering `_start`.

#[used]
#[link_section = ".requests_start"]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".requests"]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[link_section = ".requests"]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[link_section = ".requests"]
static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new(64 * 1024);

#[used]
#[link_section = ".requests"]
static ENTRY_POINT_REQUEST: EntryPointRequest = EntryPointRequest::new(_start);

#[used]
#[link_section = ".requests_end"]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

/// Requested kernel stack size, in bytes.
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;
/// Upper bound on usable memory regions handed to the frame allocator.
const MAX_USABLE_REGIONS: usize = 64;
/// Upper bound on PCI functions recorded during enumeration.
const MAX_PCI_DEVICES: usize = 64;

#[repr(align(16))]
#[allow(dead_code)]
struct DoubleFaultStack([u8; DOUBLE_FAULT_STACK_SIZE]);

#[link_section = ".bss"]
static DOUBLE_FAULT_STACK: DoubleFaultStack = DoubleFaultStack([0; DOUBLE_FAULT_STACK_SIZE]);

/// Returns the current stack pointer (captured at kernel entry, i.e. the top of
/// the stack Limine provided).
#[inline(always)]
fn current_rsp() -> u64 {
    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }
    rsp
}

/// Kernel entry point invoked by Limine (via `EntryPointRequest`).
///
/// # Safety
/// Called exactly once by the bootloader with the Limine requests resolved.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    let kernel_stack_top = current_rsp();

    serial::init();
    serial::_print(format_args!("[serial] aios-kernel: Limine handoff\n"));

    if !BASE_REVISION.is_supported() {
        kprintln!(
            "[serial] Limine base revision unsupported (actual {:?})",
            BASE_REVISION.actual_revision()
        );
    }

    let hhdm_offset = HHDM_REQUEST.response().map(|r| r.offset).unwrap_or(0);
    let rsdp = RSDP_REQUEST.response().map(|r| r.address as u64);

    let limine_fb = FRAMEBUFFER_REQUEST
        .response()
        .and_then(|r| r.framebuffers().first().copied());

    console::init(limine_fb);
    console::clear();

    vprintln!("AIOS bare-metal kernel (Limine + GOP)");
    vprintln!("====================================");
    kprintln!("[serial] aios-kernel: Limine + GOP boot");
    kprintln!("[serial] HHDM offset = 0x{:x}", hhdm_offset);
    match rsdp {
        Some(addr) => {
            vprintln!("ACPI RSDP: 0x{:x}", addr);
            kprintln!("[serial] rsdp = 0x{:x}", addr);
        }
        None => {
            vprintln!("ACPI RSDP: none");
            kprintln!("[serial] rsdp = none");
        }
    }

    // --- Framebuffer self-check: write a probe pixel, read it back ---------
    if let Some(fb) = limine_fb {
        let fb = Framebuffer::new(fb);
        kprintln!(
            "[serial] framebuffer = {}x{} pitch={} bpp={} usable={}",
            fb.width(),
            fb.height(),
            fb.pitch(),
            fb.bytes_per_pixel(),
            fb.is_usable()
        );
        vprintln!(
            "Framebuffer: {}x{} ({} bpp)",
            fb.width(),
            fb.height(),
            fb.bytes_per_pixel() * 8
        );
        if fb.is_usable() {
            let probe_x = fb.width().saturating_sub(8);
            let probe_y = fb.height().saturating_sub(8);
            unsafe { fb.fill_rect(probe_x, probe_y, 8, 8, colors::OK) };
            let want = fb.pack_color(colors::OK);
            let got = unsafe { fb.read_pixel(probe_x, probe_y) };
            if got == want {
                kprintln!(
                    "[serial] framebuffer self-check OK (readback 0x{:08x})",
                    got
                );
                vprintln!("GOP framebuffer: writes verified");
            } else {
                kprintln!(
                    "[serial] framebuffer self-check MISMATCH want=0x{:08x} got=0x{:08x}",
                    want,
                    got
                );
                vprintln!("GOP framebuffer: readback mismatch (0x{:08x})", got);
            }
        } else {
            vprintln!("GOP framebuffer: unsupported pixel format, serial only");
        }
    } else {
        vprintln!("GOP framebuffer: none (serial only)");
        kprintln!("[serial] framebuffer = none");
    }

    // --- PCI buses ---------------------------------------------------------
    let mut pci_devices = [pci::PciDevice::EMPTY; MAX_PCI_DEVICES];
    let pci_count = unsafe { pci::enumerate(&mut pci_devices) };
    let mut storage_count = 0usize;
    let mut usb_count = 0usize;
    for dev in &pci_devices[..pci_count] {
        if dev.is_storage() {
            storage_count += 1;
        }
        if dev.is_usb() {
            usb_count += 1;
        }
        kprintln!(
            "[serial] pci {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}:{:02x} ht={:02x} pi={:02x} {} bar0=0x{:08x} irq={}",
            dev.bus,
            dev.device,
            dev.function,
            dev.vendor_id,
            dev.device_id,
            dev.class,
            dev.subclass,
            dev.header_type,
            dev.prog_if,
            dev.class_name(),
            dev.bars[0],
            dev.irq
        );
    }
    kprintln!(
        "[serial] pci devices = {} (storage {}, usb {})",
        pci_count,
        storage_count,
        usb_count
    );
    vprintln!(
        "PCI: {} devices ({} storage, {} USB)",
        pci_count,
        storage_count,
        usb_count
    );

    // --- Memory map --------------------------------------------------------
    let mut usable = [memory::MemRegion { start: 0, end: 0 }; MAX_USABLE_REGIONS];
    let mut usable_count = 0usize;
    if let Some(resp) = MEMMAP_REQUEST.response() {
        for entry in resp.entries() {
            if entry.type_ != limine::memmap::MEMMAP_USABLE {
                continue;
            }
            if usable_count >= MAX_USABLE_REGIONS {
                break;
            }
            usable[usable_count] = memory::MemRegion {
                start: entry.base,
                end: entry.base + entry.length,
            };
            usable_count += 1;
        }
        vprintln!("Memory map: {} usable regions", usable_count);
        kprintln!("[serial] usable memory regions = {}", usable_count);
    } else {
        vprintln!("Memory map: none!");
        kprintln!("[serial] memory map missing");
    }

    memory::init(hhdm_offset, &usable[..usable_count]);
    vprintln!(
        "Frame allocator: {} usable regions",
        memory::frame_region_count()
    );
    kprintln!(
        "[serial] frame allocator init, usable regions = {}",
        memory::frame_region_count()
    );

    // --- Paging self-test --------------------------------------------------
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

    // --- Kernel heap -------------------------------------------------------
    heap::init_heap();
    heap::test_heap();
    vprintln!(
        "Heap: {} MiB mapped at 0x{:x}",
        heap::HEAP_SIZE / 1024 / 1024,
        heap::HEAP_START
    );
    kprintln!("[serial] heap online.");
    vprintln!("Paging + kernel heap online");

    // --- GDT / IDT / interrupts -------------------------------------------
    let double_fault_stack_top =
        &DOUBLE_FAULT_STACK as *const DoubleFaultStack as u64 + DOUBLE_FAULT_STACK_SIZE as u64;

    idt::init();
    gdt::init(double_fault_stack_top);
    gdt::set_kernel_stack(kernel_stack_top);
    interrupts::init_pic();
    interrupts::init_pit();

    vprintln!("Interrupts online (GDT/TSS, IDT, PIC, PIT, keyboard)");
    kprintln!("[serial] interrupts online.");

    unsafe {
        core::arch::asm!("sti", options(nostack, preserves_flags));
    }

    // --- AHCI SATA driver --------------------------------------------------
    if let Some(controller) = pci_devices[..pci_count]
        .iter()
        .find(|dev| dev.class == pci::CLASS_STORAGE && dev.subclass == 0x06)
    {
        match ahci::Ahci::init(controller) {
            Ok(ahci) => {
                let drives = ahci.drives();
                vprintln!("AHCI: {} SATA drive(s)", drives.len());
                kprintln!(
                    "[serial] ahci controller {:04x}:{:04x} drives = {}",
                    controller.vendor_id,
                    controller.device_id,
                    drives.len()
                );
                for drive in drives {
                    let mib = drive.sectors / 2048;
                    vprintln!(
                        "SATA port {}: {} ({} MiB)",
                        drive.port,
                        drive.model_str(),
                        mib
                    );
                    kprintln!(
                        "[serial] ahci port {} model=\"{}\" sectors={} ({} MiB)",
                        drive.port,
                        drive.model_str(),
                        drive.sectors,
                        mib
                    );
                }
                if !drives.is_empty() {
                    let mut sector = [0u8; 512];
                    match ahci.read_sectors(0, 0, 1, &mut sector) {
                        Ok(()) => {
                            kprintln!(
                                "[serial] ahci LBA0 = {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} | {}",
                                sector[0],
                                sector[1],
                                sector[2],
                                sector[3],
                                sector[4],
                                sector[5],
                                sector[6],
                                sector[7],
                                core::str::from_utf8(&sector[..16]).unwrap_or("?")
                            );
                            vprintln!("AHCI read LBA0 OK");
                        }
                        Err(e) => {
                            kprintln!("[serial] ahci read LBA0 FAILED: {}", e);
                            vprintln!("AHCI read LBA0 FAILED: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                kprintln!("[serial] ahci init failed: {}", e);
                vprintln!("AHCI init failed: {}", e);
            }
        }
    } else {
        kprintln!("[serial] ahci: no SATA controller found");
        vprintln!("AHCI: no SATA controller");
    }

    // --- Scheduler + ring-3 demo tasks + IPC ------------------------------
    sched::init();

    static mut WORKER_STACK: [u8; 16 * 1024] = [0; 16 * 1024];
    let worker_top = core::ptr::addr_of!(WORKER_STACK) as u64 + 16 * 1024;
    match sched::spawn_kernel(kernel_worker as *const () as u64, worker_top) {
        Some(slot) => {
            vprintln!("[sched] kernel worker spawned (slot {})", slot);
            kprintln!("[serial] [sched] kernel worker spawned (slot {})", slot);
        }
        None => {
            vprintln!("[sched] WARNING: no slot for kernel worker");
            kprintln!("[serial] [sched] no slot for kernel worker");
        }
    }

    match user::init() {
        Ok(()) => {}
        Err(e) => {
            vprintln!("[user] FAILED: {}", e);
            kprintln!("[serial] [user] FAILED: {}", e);
        }
    }

    vprintln!("Preemptive round-robin scheduler + ring 3 armed");
    vprintln!("Kernel IPC mailboxes behind the int 0x80 gate");
    vprintln!("User syscalls (write/getpid/sleep) + idle fallback");
    kprintln!("[serial] scheduler online, IPC + user syscalls armed.");

    sched::boot_finished();
    loop {
        core::arch::asm!("hlt", options(nomem, nostack));
    }
}

/// Ring-0 demo thread: proves kernel tasks are preempted too. Yields
/// cooperatively at fixed intervals so the PIT never needs to switch it while
/// it is mid-print.
fn kernel_worker() -> ! {
    let mut alive: u64 = 0;
    loop {
        let mut i = 0u64;
        while i < 20_000_000u64 {
            core::hint::spin_loop();
            i += 1;
            if i & 0xFFFFF == 0 {
                crate::sched::yield_kernel();
            }
        }
        alive += 1;
        vprintln!("[ktask] alive #{}", alive);
        kprintln!("[serial] [ktask] alive #{}", alive);
        crate::sched::yield_kernel();
    }
}

/// Kernel idle task: parked until preempted, prints a rolling tick timestamp
/// each second. Runs on its own dedicated stack on a fabricated frame.
pub fn idle_loop() -> ! {
    let mut last_tick_print = 0u64;
    let mut last_stats_print = 0u64;
    let mut last_scancode = 0u64;
    loop {
        crate::sched::yield_kernel();
        let ticks = interrupts::TICKS.load(Ordering::Relaxed);
        if ticks >= interrupts::TIMER_HZ && ticks - last_tick_print >= interrupts::TIMER_HZ {
            vprintln!("[tick] {}s", ticks / interrupts::TIMER_HZ);
            kprintln!("[serial] tick {}s", ticks / interrupts::TIMER_HZ);
            last_tick_print = ticks;
        }
        // Every 5 seconds: scheduler + IPC proof counters.
        let stats_window = 5 * interrupts::TIMER_HZ;
        if ticks >= stats_window && ticks - last_stats_print >= stats_window {
            let (sent, recv) = syscalls::stats();
            let switches = sched::switch_count();
            vprintln!(
                "[stats] switches={} ipc_sent={} ipc_recv={}",
                switches,
                sent,
                recv
            );
            kprintln!(
                "[serial] [stats] switches={} sent={} recv={}",
                switches,
                sent,
                recv
            );
            last_stats_print = ticks;
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
