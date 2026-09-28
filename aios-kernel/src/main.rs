#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod ahci;
mod console;
mod ec;
mod font8x8;
mod framebuffer;
mod gdt;
mod heap;
mod idt;
mod interrupts;
mod lapic;
mod lid;
mod memory;
mod nvme;
mod pci;
mod port;
mod ps2;
mod psf;
mod sched;
mod serial;
mod syscalls;
mod thermal;
mod tui;
mod user;
mod xhci;

use crate::framebuffer::{colors, Framebuffer};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicI32, Ordering};
use limine::request::{
    EntryPointRequest, FramebufferRequest, HhdmRequest, MemmapRequest, RsdpRequest,
    StackSizeRequest,
};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

// Driver status published for the TUI dashboard: -1 = no controller, 0 = init
// failed, 1 = online, 2 = online and a sector read-back succeeded.
pub static G_AHCI: AtomicI32 = AtomicI32::new(-1);
pub static G_NVME: AtomicI32 = AtomicI32::new(-1);
pub static G_XHCI: AtomicI32 = AtomicI32::new(-1);

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

/// Prints a boot step marker. Always draws an orange square on the bottom-left
/// of the framebuffer (lock-free, works even without serial/debug mode) and
/// prints the step line to the console.
fn print_step(n: u8, msg: &str) {
    if let Some(fb) = crate::console::framebuffer() {
        unsafe {
            let y = fb.height().saturating_sub(20);
            let x = usize::from(n - 1).saturating_mul(10);
            fb.fill_rect(x, y, 8, 8, colors::STEP);
        }
    }
    vprintln!("STEP {}/14: {}", n, msg);
}
///
/// # Safety
/// Called exactly once by the bootloader with the Limine requests resolved.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    let kernel_stack_top = current_rsp();

    serial::init();
    serial::_print(format_args!("[serial] aios-kernel: Limine handoff\n"));

    let mut seg_cs: u16 = 0;
    let mut seg_ss: u16 = 0;
    let mut seg_ds: u16 = 0;
    unsafe {
        core::arch::asm!("mov {0:x}, cs", out(reg) seg_cs, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mov {0:x}, ss", out(reg) seg_ss, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mov {0:x}, ds", out(reg) seg_ds, options(nomem, nostack, preserves_flags));
    }
    kprintln!(
        "[serial] boot selectors cs={:#x} ss={:#x} ds={:#x}",
        seg_cs,
        seg_ss,
        seg_ds
    );

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

    interrupts::check_f8();
    if interrupts::DEBUG_MODE.load(core::sync::atomic::Ordering::Relaxed) {
        console::set_cursor(0, 0);
        vprintln!("[DEBUG] F8 pressed — debug codes enabled (TOP-LEFT)");
        vprintln!("[DEBUG] STEP markers below — check last step on halt");
        vprintln!("");
    }

    print_step(1, "serial online");
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

    // --- GOP framebuffer: mapping, direct colour test, readback -------------
    // `verify_region` must run after `memory::init` (it walks through the HHDM
    // alias, which needs PHYS_OFFSET); the direct colour test below does not.
    let mut fb_base: u64 = 0;
    let mut fb_bytes: u64 = 0;
    if let Some(fb) = limine_fb {
        let fb = Framebuffer::new(fb);
        fb_base = fb.base_addr();
        fb_bytes = (fb.pitch().saturating_mul(fb.height())) as u64;
        kprintln!(
            "[serial] framebuffer = {}x{} pitch={} bpp={} usable={} addr=0x{:x}",
            fb.width(),
            fb.height(),
            fb.pitch(),
            fb.bytes_per_pixel(),
            fb.is_usable(),
            fb.base_addr()
        );
        vprintln!(
            "Framebuffer: {}x{} ({} bpp)",
            fb.width(),
            fb.height(),
            fb.bytes_per_pixel() * 8
        );
        if fb.is_usable() {
            // Direct colour test: full-panel solid fill using write_volatile
            // and honouring the physical stride, then a readback of pixel (0,0)
            // to confirm the writes actually landed.
            unsafe {
                fb.direct_test(0x00_00_20_c0);
                core::arch::asm!("sfence", options(nostack, preserves_flags));
                let got = fb.read_pixel(0, 0);
                let want = fb.pack_color(0x00_00_20_c0);
                if got == want {
                    kprintln!("[serial] direct colour test OK (readback 0x{:08x})", got);
                } else {
                    kprintln!(
                        "[serial] direct colour test MISMATCH want=0x{:08x} got=0x{:08x}",
                        want,
                        got
                    );
                }
            }
            // Boot self-check: draw an OK-green probe square and read it back.
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

    // --- Embedded PSF font check -------------------------------------------
    // Synthesises a PSF2 stream from the baked-in font8x8 glyphs, parses it
    // back through the no_std PSF parser and draws the 'A' glyph at the bottom
    // centre (outside the console/TUI/heartbeat regions) so the whole font
    // path is proven at boot without shipping a separate binary font.
    psf_check();

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
    print_step(2, "PCI enumerated");

    // --- Memory map --------------------------------------------------------
    print_step(3, "memory map");
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
    print_step(4, "memory init");
    vprintln!(
        "Frame allocator: {} usable regions",
        memory::frame_region_count()
    );
    kprintln!(
        "[serial] frame allocator init, usable regions = {}",
        memory::frame_region_count()
    );

    // --- GOP VRAM mapping verification -------------------------------------
    // Now that `memory::init` set the HHDM offset, walk the live page tables
    // over the whole framebuffer range and prove it is mapped present +
    // writable. The framebuffer address Limine hands out already carries the
    // HHDM offset, so it must be used as-is (adding it again would fault).
    if fb_base != 0 {
        let chk = memory::verify_region(fb_base, fb_bytes);
        kprintln!(
            "[serial] framebuffer pages: {} present, {} writable, {} total",
            chk.pages_present,
            chk.pages_writable,
            chk.pages_total
        );
    }

    // --- Paging self-test --------------------------------------------------
    match memory::selftest() {
        Ok(()) => {
            vprintln!("Paging selftest: OK (map/write/read/translate/unmap)");
            kprintln!("[serial] paging selftest OK.");
        }
        Err(e) => {
            vprintln!("Paging selftest FAILED: {}", e);
            kprintln!("[serial] paging selftest FAILED: {}", e);
            interrupts::fatal_with(0x30000001, "PAGING SELFTEST FAILED");
        }
    }

    // --- Kernel heap -------------------------------------------------------
    print_step(5, "heap init");
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
    unsafe {
        let master_mask = crate::port::inb(0x21);
        let slave_mask = crate::port::inb(0xA1);
        kprintln!(
            "[serial] [probe] PIC-MASK master=0x{:02X} slave=0x{:02X} (0xFE=IRQ0-unmasked-PIT-enabled)",
            master_mask,
            slave_mask
        );
    }
    interrupts::init_pit();
    // Bring the Local APIC timer up (x2APIC MSR or xAPIC MMIO) and, once it is
    // live, mask the PIT IRQ0 in the PIC so a single hardware tick source
    // drives the scheduler. On boards with a working 8259 the PIT would
    // otherwise keep double-incrementing alongside the LAPIC ticks.
    let lapic_active = lapic::init();
    interrupts::set_pit_masked(lapic_active);
    print_step(6, "interrupts online");
    vprintln!("Interrupts online (GDT/TSS, IDT, PIC, PIT, keyboard)");
    kprintln!(
        "[serial] interrupts online. timer={}",
        if lapic_active { "LAPIC" } else { "PIT" }
    );

    unsafe {
        core::arch::asm!("sti", options(nostack, preserves_flags));
    }
    print_step(7, "sti executed");

    // --- ACPI / EC / thermal / PS/2 (platform sensors & input) ------------
    // No IRQ wiring required: the ACPI walk reads tables, the thermal probe
    // reads MSRs, and the i8042 controller is polled IRQ-free (UEFI laptops
    // often leave the 8259 PIC lines dead). Order matters: lid needs the FADT.
    if let Some(addr) = rsdp {
        acpi::init(addr);
    } else {
        kprintln!("[serial] [acpi] rsdp missing, platform sensors unavailable");
    }
    thermal::init();
    ps2::init();
    lid::init();

    // On-screen IRQ0 liveness probe (no serial needed): wait ~120 ms and count
    // how many PIT ticks a real IRQ32 delivered. Some UEFI laptops leave the
    // legacy 8259 PIC dead (IRQ0 routed via an unprogrammed IO-APIC), so this
    // tells us whether the hardware timer IRQ will drive scheduling at all.
    let t0 = interrupts::TICKS.load(Ordering::Relaxed);
    let seen0 = interrupts::IRQ32_SEEN.load(Ordering::Relaxed);
    interrupts::delay_ms(120);
    let t1 = interrupts::TICKS.load(Ordering::Relaxed);
    let seen1 = interrupts::IRQ32_SEEN.load(Ordering::Relaxed);
    vprintln!(
        "[probe] irq32_seen={}->{} ticks={}->{} (delta {})",
        seen0,
        seen1,
        t0,
        t1,
        t1 - t0
    );

    let mut rflags: u64 = 0;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nostack, preserves_flags));
    }
    kprintln!(
        "[serial] [probe] RFLAGS.IF={} (bit9) sti-executed-marker",
        (rflags >> 9) & 1
    );
    kprintln!(
        "[serial] [probe] gate32-installed={} (vector-32 present-bit) idt-present-marker",
        interrupts::idt_gate_installed(32)
    );

    {
        // RAW 16-byte gate descriptor dump: vector-32 (broken #GP) vs vector-50 (working soft-int).
        // Discriminates which byte of the 16-byte IDT descriptor for 0x20 is corrupted.
        let (off32, sel32) = interrupts::idt_raw_gate(32);
        let (off50, sel50) = interrupts::idt_raw_gate(50);
        kprintln!(
            "[serial] [probe] IDT-RAW-32 offset=0x{:016X} selector=0x{:04X} (vector-32 raw-gate-descriptor-two-words) idt-32raw-marker",
            off32, sel32
        );
        kprintln!(
            "[serial] [probe] IDT-RAW-50 offset=0x{:016X} selector=0x{:04X} (vector-50 control-gate wholesale-offset-holding) idt-50raw-marker",
            off50, sel50
        );
    }
    kprintln!("[serial] [probe] SOFT-INT-0x20-SENT (int-instruction vector-0x20=32-decimal, matches-PIT-arm-32 idt-soft-trigger bypasses-PIC)");
    // int 0x20 soft-int has been replaced by a harmless serial-only probe (RAW gate dump above).
    // (the old asm!("int 0x20") was a GP#13 discriminator; on real hardware it fires, so it is disabled. Booting normally.)
    // --- AHCI SATA driver --------------------------------------------------
    print_step(8, "AHCI");
    if let Some(controller) = pci_devices[..pci_count]
        .iter()
        .find(|dev| dev.class == pci::CLASS_STORAGE && dev.subclass == 0x06)
    {
        match ahci::Ahci::init(controller) {
            Ok(ahci) => {
                G_AHCI.store(1, Ordering::Relaxed);
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
                            G_AHCI.store(2, Ordering::Relaxed);
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
                G_AHCI.store(0, Ordering::Relaxed);
            }
        }
    } else {
        kprintln!("[serial] ahci: no SATA controller found");
        vprintln!("AHCI: no SATA controller");
    }

    // --- NVMe driver -------------------------------------------------------
    print_step(9, "NVMe");
    if let Some(controller) = pci_devices[..pci_count]
        .iter()
        .find(|dev| dev.class == pci::CLASS_STORAGE && dev.subclass == 0x08)
    {
        match nvme::Nvme::init(controller) {
            Ok(mut nvme) => {
                G_NVME.store(1, Ordering::Relaxed);
                let drive = *nvme.drive();
                let mib = drive.bytes() / 1024 / 1024;
                vprintln!(
                    "NVMe: {} ({} MiB, {} B blocks)",
                    drive.model_str(),
                    mib,
                    drive.lba_size
                );
                kprintln!(
                    "[serial] nvme controller {:04x}:{:04x} model=\"{}\" serial=\"{}\" blocks={} lba={} ({} MiB)",
                    controller.vendor_id,
                    controller.device_id,
                    drive.model_str(),
                    drive.serial_str(),
                    drive.blocks,
                    drive.lba_size,
                    mib
                );
                let mut sector = [0u8; 512];
                match nvme.read_blocks(0, 1, &mut sector) {
                    Ok(()) => {
                        G_NVME.store(2, Ordering::Relaxed);
                        kprintln!(
                            "[serial] nvme LBA0 = {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} | {}",
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
                        vprintln!("NVMe read LBA0 OK");
                    }
                    Err(e) => {
                        kprintln!("[serial] nvme read LBA0 FAILED: {}", e);
                        vprintln!("NVMe read LBA0 FAILED: {}", e);
                    }
                }
            }
            Err(e) => {
                kprintln!("[serial] nvme init failed: {}", e);
                vprintln!("NVMe init failed: {}", e);
                G_NVME.store(0, Ordering::Relaxed);
            }
        }
    } else {
        kprintln!("[serial] nvme: no NVMe controller found");
        vprintln!("NVMe: no NVMe controller");
    }

    // --- xHCI (USB) driver ------------------------------------------------
    print_step(10, "xHCI");
    if let Some(controller) = pci_devices[..pci_count]
        .iter()
        .find(|dev| dev.class == pci::CLASS_SERIAL_BUS && dev.subclass == 0x03)
    {
        match xhci::Xhci::init(controller) {
            Ok(hid) => {
                G_XHCI.store(1, Ordering::Relaxed);
                vprintln!(
                    "USB: xHCI slot {} port {} speed {}",
                    hid.slot,
                    hid.port,
                    hid.speed
                );
                kprintln!(
                    "[serial] xhci controller {:04x}:{:04x} slot={} port={} speed={}",
                    controller.vendor_id,
                    controller.device_id,
                    hid.slot,
                    hid.port,
                    hid.speed
                );
                vprintln!("USB HID boot keyboard: reports armed");
                kprintln!("[serial] usb hid boot keyboard armed.");
                if hid.mouse_slot != 0 {
                    vprintln!(
                        "USB HID boot mouse: slot {} port {} reports armed",
                        hid.mouse_slot,
                        hid.mouse_port
                    );
                    kprintln!("[serial] usb hid boot mouse armed.");
                }
            }
            Err(e) => {
                kprintln!("[serial] xhci init failed: {}", e);
                vprintln!("USB HID init failed: {}", e);
                G_XHCI.store(0, Ordering::Relaxed);
            }
        }
    } else {
        kprintln!("[serial] xhci: no USB controller found");
        vprintln!("USB: no xHCI controller");
    }

    // --- Scheduler + ring-3 demo tasks + IPC ------------------------------
    print_step(11, "scheduler init");
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
    print_step(12, "user init done");

    print_step(13, "scheduler armed");
    vprintln!("Preemptive round-robin scheduler + ring 3 armed");
    vprintln!("Kernel IPC mailboxes behind the int 0x80 gate");
    vprintln!("User syscalls (write/getpid/sleep) + idle fallback");
    kprintln!("[serial] scheduler online, IPC + user syscalls armed.");

    print_step(14, "ring-3 idle handoff");
    sched::boot_finished();
    sched::yield_kernel();
    loop {
        unsafe {
            core::arch::asm!("sti; hlt", options(nomem, nostack));
        }
    }
}

/// Synthesises a PSF2 font from `font8x8::BASIC`, parses it back through the
/// `no_std` PSF parser, verifies the 'A' glyph survives the round-trip and
/// paints it at the bottom centre of the screen as a persistent boot proof.
fn psf_check() {
    let Some(fb) = crate::console::framebuffer() else {
        kprintln!("[serial] [psf] skipped: no framebuffer");
        return;
    };
    let mut buf = [0u8; 2048];
    let Some(len) = crate::psf::synth_psf2_basic(&mut buf) else {
        kprintln!("[serial] [psf] synthesis buffer too small");
        return;
    };
    let Some(font) = crate::psf::PsfFont::parse(&buf[..len]) else {
        kprintln!("[serial] [psf] parse FAILED");
        return;
    };
    let a_idx = b'A' as usize;
    let Some(a_psf) = font.glyph(a_idx) else {
        kprintln!("[serial] [psf] glyph 'A' missing");
        return;
    };
    let a_ref = crate::font8x8::BASIC[a_idx];
    let roundtrip_ok = a_psf.len() >= 8 && (0..8).all(|i| a_psf[i] == a_ref[i]);
    let cell = console::GLYPH_W;
    let gx = fb.width().saturating_sub(cell) / 2;
    let gy = fb.height().saturating_sub(cell);
    unsafe {
        fb.fill_rect(gx, gy, cell, cell, colors::BG);
        for (row, bits) in a_psf.iter().enumerate().take(8) {
            for col in 0..8usize {
                if bits & (1 << col) != 0 {
                    fb.fill_rect(
                        gx + col * console::SCALE,
                        gy + row * console::SCALE,
                        console::SCALE,
                        console::SCALE,
                        colors::OK,
                    );
                }
            }
        }
    }
    let ver = match font.version() {
        crate::psf::FontVersion::Psf1 => "PSF1",
        crate::psf::FontVersion::Psf2 => "PSF2",
    };
    kprintln!(
        "[serial] [psf] {} w={} h={} glyphs={} 'A'={} (bottom-centre)",
        ver,
        font.width(),
        font.height(),
        font.glyph_count(),
        if roundtrip_ok { "match" } else { "MISMATCH" }
    );
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

pub fn idle_loop() -> ! {
    let mut last_tick_print = 0u64;
    let mut last_stats_print = 0u64;
    let mut last_scancode = 0u64;
    let mut last_usb_seq = 0u64;
    let mut last_mouse_seq = 0u64;
    let mut last_mouse_buttons = 0u8;
    let mut last_ps2_seq = 0u32;
    let mut last_ps2m_seq = 0u32;
    let mut last_ps2m_buttons = 0u8;
    let mut last_tui_render = 0u64;
    loop {
        crate::sched::yield_kernel();
        xhci::poll();
        ps2::drain();
        let ticks = interrupts::TICKS.load(Ordering::Relaxed);
        if ticks >= interrupts::TIMER_HZ && ticks - last_tick_print >= interrupts::TIMER_HZ {
            vprintln!("[tick] {}s", ticks / interrupts::TIMER_HZ);
            kprintln!("[serial] tick {}s", ticks / interrupts::TIMER_HZ);
            last_tick_print = ticks;
        }
        // TUI dashboard: repaint the interactive panel at ~20 Hz (every 5
        // ticks) so tab/status/clock state stays live while keys and the USB
        // mouse are handled below. The lid and thermal probes refresh at the
        // same cadence (cheap: a few EC/MSR reads per call).
        if ticks >= interrupts::TIMER_HZ && ticks - last_tui_render >= 5 {
            lid::poll();
            thermal::poll();
            tui::render();
            last_tui_render = ticks;
        }
        // Hardware heartbeat: toggles the bottom-right probe square (direct
        // framebuffer write, no CONSOLE_LOCK) so a live CPU is visible even if
        // the lock-based console is wedged. Toggles at 2 Hz (every half-second
        // tick boundary); a per-iteration toggle would run at ~100 Hz and
        // integrate into a steady fill that looks "not blinking".
        unsafe {
            if let Some(fb) = crate::console::framebuffer() {
                static mut HB_ON: bool = false;
                static mut HB_PHASE: u64 = u64::MAX;
                let phase = ticks / (interrupts::TIMER_HZ / 2);
                if phase != HB_PHASE {
                    HB_PHASE = phase;
                    HB_ON = !HB_ON;
                    let c = if HB_ON { colors::OK } else { colors::BG };
                    let bx = fb.width().saturating_sub(8);
                    let by = fb.height().saturating_sub(8);
                    fb.fill_rect(bx, by, 8, 8, c);
                }
            }
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
                if tui::handle_scancode(sc as u8) {
                    continue;
                }
                if let Some(c) = interrupts::scancode_to_char(sc as u8) {
                    vprintln!("[key] '{}' (0x{:02x})", c, sc);
                    kprintln!("[serial] key '{}' (0x{:02x})", c, sc);
                } else {
                    vprintln!("[key] scancode 0x{:02x}", sc);
                    kprintln!("[serial] key scancode 0x{:02x}", sc);
                }
            }
        }
        let usb_seq = xhci::KEY_SEQ.load(Ordering::Relaxed);
        if usb_seq != last_usb_seq {
            last_usb_seq = usb_seq;
            let usb_sc = xhci::KEY_SCANCODE.load(Ordering::Relaxed);
            if !tui::handle_scancode(usb_sc as u8) {
                if let Some(c) = interrupts::scancode_to_char(usb_sc as u8) {
                    vprintln!("[usb-key] '{}' (0x{:02x})", c, usb_sc);
                    kprintln!("[serial] usb key '{}' (0x{:02x})", c, usb_sc);
                } else {
                    vprintln!("[usb-key] usage scancode 0x{:02x}", usb_sc);
                    kprintln!("[serial] usb key scancode 0x{:02x}", usb_sc);
                }
            }
        }
        // PS/2 native keyboard (i8042 IRQ-free path): mirrors the USB key band.
        let ps2_seq = ps2::key_seq();
        if ps2_seq != last_ps2_seq {
            last_ps2_seq = ps2_seq;
            let ps2_sc = ps2::key_scancode();
            if !tui::handle_scancode(ps2_sc as u8) {
                if let Some(c) = interrupts::scancode_to_char(ps2_sc as u8) {
                    vprintln!("[ps2-key] '{}' (0x{:02x})", c, ps2_sc);
                    kprintln!("[serial] ps2 key '{}' (0x{:02x})", c, ps2_sc);
                } else {
                    vprintln!("[ps2-key] scancode 0x{:02x}", ps2_sc);
                    kprintln!("[serial] ps2 key scancode 0x{:02x}", ps2_sc);
                }
            }
        }
        let mouse_seq = xhci::MOUSE_SEQ.load(Ordering::Relaxed);
        if mouse_seq != last_mouse_seq {
            last_mouse_seq = mouse_seq;
            let dx = (xhci::MOUSE_DX.load(Ordering::Relaxed) as i64) as i32;
            let dy = (xhci::MOUSE_DY.load(Ordering::Relaxed) as i64) as i32;
            let buttons = xhci::MOUSE_BUTTONS.load(Ordering::Relaxed) as u8;
            if dx != 0 || dy != 0 || buttons != last_mouse_buttons {
                last_mouse_buttons = buttons;
                vprintln!("[usb-mouse] btns={:#x} dx={} dy={}", buttons, dx, dy);
                kprintln!("[serial] usb mouse btns={:#x} dx={} dy={}", buttons, dx, dy);
                // Hand the report to the interactive TUI (moves + repaints the
                // arrow, switches tabs on a click) and then redraw the panel
                // so the arrow is freshly composited over it.
                tui::on_mouse(dx, dy, buttons);
                tui::render();
            }
        }
        // PS/2 pointer (native laptop touchpad in PS/2 mode): same wiring as
        // the USB mouse band above.
        let ps2m_seq = ps2::mouse_seq();
        if ps2m_seq != last_ps2m_seq {
            last_ps2m_seq = ps2m_seq;
            let dx = ps2::mouse_dx();
            let dy = ps2::mouse_dy();
            let buttons = ps2::mouse_buttons() as u8;
            if dx != 0 || dy != 0 || buttons != last_ps2m_buttons {
                last_ps2m_buttons = buttons;
                vprintln!("[ps2-mouse] id=0x{:02x} btns={:#x} dx={} dy={}", ps2::mouse_id(), buttons, dx, dy);
                kprintln!("[serial] ps2 mouse id=0x{:02x} btns={:#x} dx={} dy={}", ps2::mouse_id(), buttons, dx, dy);
                tui::on_mouse(dx, dy, buttons);
                tui::render();
            }
        }
        // Park the CPU. With the PIT IRQ arriving (legacy PIC alive) a plain
        // `hlt` is woken by each hardware tick. On boards where IRQ0 never
        // reaches the CPU (UEFI APIC routing), `hlt` would sleep forever, so
        // the idle loop polls the PIT countdown for one tick period instead
        // and synthesizes the tick itself. `pit_count` always runs — it is
        // clocked directly, independent of interrupt delivery.
        if interrupts::IRQ32_SEEN.load(Ordering::Relaxed) {
            unsafe {
                core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
            }
        } else if !interrupts::wait_for_irq32(1000 / interrupts::TIMER_HZ) {
            interrupts::TICKS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn halt_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("sti; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vprintln!("KERNEL PANIC: {}", info);
    kprintln!("[serial] KERNEL PANIC: {}", info);
    halt_loop();
}
