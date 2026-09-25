//! xHCI (USB 2/3 dual-mode) host controller driver.
//!
//! Walks the PCI bus for a USB `0x0C:0x03` function, brings the controller
//! out of reset by programming the capability/operational registers over
//! [`crate::memory::map_mmio`], sets up the DCBAA, scratchpad, event ring,
//! command ring and endpoint transfer rings, then raises the first connected
//! root-port device: port reset, Enable Slot, Address Device, EP0 control
//! transfers (device descriptor + configuration descriptor + set
//! configuration + boot protocol) and a Configure Endpoint command for the
//! HID boot keyboard's interrupt IN pipe.
//!
//! There is no MSI support in the kernel yet, so the driver is polled: the
//! init path spins on the event ring waiting for command/transfer completions,
//! and the scheduler idle task calls [`poll`] to harvest HID boot reports and
//! re-arm the interrupt endpoint.

use crate::memory;
use crate::pci::{self, PciDevice};
use core::ptr;
use core::sync::atomic::{fence, AtomicU64, Ordering};

const SPIN_LIMIT: u32 = 200_000_000;
const TRBS: usize = 32;

// Capability register offsets (relative to the BAR base).
const HCS_OFF: u64 = 0x04;
const HCC_OFF: u64 = 0x10;
const DBOFF_OFF: u64 = 0x14;

// Operational register offsets (relative to the CAPLENGTH base).
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CRCR_RCS: u32 = 1 << 0;
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_CNR: u32 = 1 << 11;

// Interrupter register offsets (relative to the run/base).
const ERSTSZ: u64 = 0x28;
const ERSTBA: u64 = 0x30;
const ERDP: u64 = 0x38;

// Port register set: port `n` lives at op + 0x400 + (n - 1) * 0x10.
const PORT_BASE: u64 = 0x400;
const PORT_CCS: u32 = 1 << 0;
const PORT_PE: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
const PORT_POWER: u32 = 1 << 9;
const PORT_RC: u32 = 1 << 21;
const PORT_CHANGE: u32 = 0xFE0000;

const TRB_CYCLE: u32 = 1;
const TRB_TC: u32 = 1 << 1;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_DIR_IN: u32 = 1 << 16;

const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDR_DEV: u32 = 11;
const TRB_CONFIG_EP: u32 = 12;
const TRB_TRANSFER: u32 = 32;
const TRB_COMPLETION: u32 = 33;

const COMP_SUCCESS: u32 = 1;
const COMP_SHORT_PACKET: u32 = 13;

const SLOT_FLAG: u32 = 1;
const EP0_FLAG: u32 = 1 << 1;
const EP1_IN_FLAG: u32 = 1 << 3;
const CTRL_EP: u32 = 4;
const INT_IN_EP: u32 = 7;
const ERROR_COUNT: u32 = 3 << 1;

// Context size for 32-byte contexts (CSZ=0); 64 when the HC reports CSZ=1.
const CTX_32: u64 = 32;

// USB descriptor walking.
const DESC_INTERFACE: u8 = 4;
const DESC_ENDPOINT: u8 = 5;
const IFACE_CLASS_HID: u8 = 3;

// HID boot keyboard control requests.
const REQ_SET_CONFIGURATION: u8 = 0x09;
const REQ_SET_PROTOCOL: u8 = 0x0B;
const REQ_HID_GET_REPORT: u8 = 0x01;
const IFACE_DIR_IN: u8 = 0xA1;
const USBSTS_HCE: u32 = 1 << 12;

// Set-1 scancodes for HID usages 0x04..=0x1D (letters a-z).
const LETTER_SCANS: [u8; 26] = [
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
];

/// Monotonic counter bumped every time a fresh USB keyboard report arrives
/// with at least one newly-pressed key. The scheduler idle loop watches this.
pub static KEY_SEQ: AtomicU64 = AtomicU64::new(0);

/// Last emitted key as a PS/2 set-1 make code (so `scancode_to_char` works).
pub static KEY_SCANCODE: AtomicU64 = AtomicU64::new(0);

/// Packed 8-byte boot report of the previous poll, for press detection.
static LAST_REPORT: AtomicU64 = AtomicU64::new(0);

static mut XB_OP: u64 = 0;
static mut XB_RUN: u64 = 0;
static mut XB_DB: u64 = 0;
static mut XB_SLOT: u8 = 0;
static mut XB_EV: u64 = 0;
static mut XB_EV_DEQ: usize = 0;
static mut XB_EV_CYCLE: bool = false;
static mut XB_EP0: u64 = 0;
static mut XB_EP0_DEQ: usize = 0;
static mut XB_EP0_CYCLE: bool = false;
static mut XB_DATA: u64 = 0;
static mut XB_EP1: u64 = 0;
static mut XB_EP1_DEQ: usize = 0;
static mut XB_EP1_CYCLE: bool = false;
static mut XB_EP1_BUF: u64 = 0;

/// GET_REPORT probe cadence: issue one HID GET_REPORT control transfer every
/// N idle-loop polls so the driver logs what QEMU's device HID queue holds.
const PROBE_EVERY: u32 = 40;
static mut PROBE_POLLS: u32 = 0;
static mut PROBE_ENTERED: bool = false;

/// Per-controller state kept while the controller is being brought up.
struct Core {
    run: u64,
    op: u64,
    db: u64,
    csz: u64,
    slot: u8,
    port: u8,
    speed: u8,
    cmd: u64,
    cmd_deq: usize,
    cmd_cycle: bool,
    ev: u64,
    ev_deq: usize,
    ev_cycle: bool,
    dcbaa: u64,
    in_ctx: u64,
    out_ctx: u64,
    data: u64,
    ep0: u64,
    ep0_deq: usize,
    ep0_cycle: bool,
    ep0_maxpkt: u32,
    ep1: u64,
    ep1_deq: usize,
    ep1_cycle: bool,
    ep1_buf: u64,
}

/// A brought-up xHCI controller, handed back to the caller for bookkeeping.
pub struct Xhci {
    /// 1-based root hub port the HID keyboard was found on.
    pub port: u8,
    /// Slot index the keyboard was assigned to.
    pub slot: u8,
    /// USB device speed at the port (1 = full speed).
    pub speed: u8,
}

impl Xhci {
    /// Probes an xHCI PCI function, brings the controller up and configures
    /// the first boot keyboard found on a root port.
    pub fn init(dev: &PciDevice) -> Result<Xhci, &'static str> {
        if dev.bars[0] & 0x1 != 0 {
            return Err("xhci: BAR0 is an I/O BAR");
        }
        let bar = ((dev.bars[0] & 0xFFFF_FFF0) as u64) | ((dev.bars[1] as u64) << 32);
        if bar == 0 {
            return Err("xhci: BAR0 is zero");
        }
        if !dev.is_usb() || dev.subclass != 0x03 {
            return Err("xhci: not an xHCI USB controller");
        }
        unsafe {
            let command = pci::config_read32(dev.bus, dev.device, dev.function, 0x04);
            pci::config_write32(dev.bus, dev.device, dev.function, 0x04, command | 0x6);
        }

        let base = memory::map_mmio(bar, 0x8000)?;
        let cap = mmio32(base);
        let capl = u64::from(cap & 0xFF);
        let op = base + capl;
        let db = base + u64::from(mmio32(base + DBOFF_OFF) & 0xFFFF_FFFC);

        // Some controllers (QEMU's qemu-xhci included) advertise a runtime
        // offset that collides with the doorbell space; probe the advertised
        // position first, then fall back to the standard 0x1000 location.
        let advertised = base + (u64::from(cap >> 16) << 5);
        let mut run = 0;
        for candidate in [advertised, base + 0x1000] {
            mmio32w(candidate + ERSTSZ, 1);
            if mmio32(candidate + ERSTSZ) & 0xFFFF == 1 {
                run = candidate;
                break;
            }
        }
        if run == 0 {
            return Err("xhci: runtime registers not found");
        }

        let hcs1 = mmio32(base + HCS_OFF);
        let max_slots = hcs1 & 0xFF;
        let max_ports = (hcs1 >> 24) as u8;
        let hcs2 = mmio32(base + HCS_OFF + 0x04);
        let scratch = ((hcs2 >> 16) & 0x3E0) | ((hcs2 >> 27) & 0x1F);
        let csz = if mmio32(base + HCC_OFF) & 0x4 != 0 {
            64
        } else {
            CTX_32
        };
        if max_slots == 0 || max_ports == 0 {
            return Err("xhci: empty capability registers");
        }

        // Stop the controller (if it was running) and bring it out of reset.
        if mmio32(op + USBCMD) & CMD_RUN != 0 {
            mmio32w(op + USBCMD, mmio32(op + USBCMD) & !CMD_RUN);
            spin_until(|| mmio32(op + USBSTS) & USBSTS_HCH != 0)
                .map_err(|_| "xhci: halt timeout")?;
        }
        mmio32w(op + USBCMD, CMD_RESET);
        spin_until(|| mmio32(op + USBCMD) & CMD_RESET == 0).map_err(|_| "xhci: reset timeout")?;
        spin_until(|| mmio32(op + USBSTS) & USBSTS_CNR == 0)
            .map_err(|_| "xhci: controller not ready")?;

        let dcbaa = alloc_zero().ok_or("xhci: no DCBAA frame")?;
        let out_ctx = alloc_zero().ok_or("xhci: no output context frame")?;
        let in_ctx = alloc_zero().ok_or("xhci: no input context frame")?;
        let data = alloc_zero().ok_or("xhci: no data frame")?;
        let ev = alloc_zero().ok_or("xhci: no event ring frame")?;
        let cmd = alloc_zero().ok_or("xhci: no command ring frame")?;
        let ep0 = alloc_zero().ok_or("xhci: no EP0 ring frame")?;
        let ep1 = alloc_zero().ok_or("xhci: no EP1 ring frame")?;
        let ep1_buf = alloc_zero().ok_or("xhci: no EP1 buffer frame")?;
        let erst = alloc_zero().ok_or("xhci: no ERST frame")?;
        setup_ring(ep0, true);
        setup_ring(ep1, true);

        if scratch > 0 {
            let sp_arr = alloc_zero().ok_or("xhci: no scratchpad array frame")?;
            put_u64(dcbaa, 0, sp_arr);
            for index in 0..scratch as usize {
                let sp = alloc_zero().ok_or("xhci: no scratchpad frame")?;
                put_u64(sp_arr, (index * 8) as u64, sp);
            }
        }

        setup_ring(cmd, true);
        mmio64w(op + DCBAAP, dcbaa);
        mmio32w(op + CONFIG, 1);
        mmio64w(op + CRCR, cmd | u64::from(CRCR_RCS));

        put_u64(erst, 0, ev);
        put_u16(erst, 8, TRBS as u16);
        mmio32w(run + ERSTSZ, 1);
        mmio64w(run + ERSTBA, erst);
        mmio64w(run + ERDP, ev | 8);

        mmio32w(op + USBCMD, CMD_RUN);
        spin_until(|| mmio32(op + USBSTS) & USBSTS_HCH == 0).map_err(|_| "xhci: start timeout")?;

        crate::kprintln!(
            "[serial] [xhci] run=0x{:x} op=0x{:x} db=0x{:x} capl={} rtmsoff={}",
            run,
            op,
            db,
            capl,
            (cap >> 16) << 5
        );
        crate::kprintln!(
            "[serial] [xhci] ev=0x{:x} cmd=0x{:x} ep0=0x{:x} ep1=0x{:x} dcbaa=0x{:x} erst=0x{:x}",
            ev,
            cmd,
            ep0,
            ep1,
            dcbaa,
            erst
        );
        crate::kprintln!(
            "[serial] [xhci] readback ERSTSZ=0x{:08x} USBCMD=0x{:08x} USBSTS=0x{:08x}",
            mmio32(run + ERSTSZ),
            mmio32(op + USBCMD),
            mmio32(op + USBSTS)
        );
        crate::kprintln!(
            "[serial] [xhci] readback ERDP=0x{:016x} ERSTBA=0x{:016x} CRCR=0x{:016x} DCBAAP=0x{:016x}",
            mmio64(run + ERDP),
            mmio64(run + ERSTBA),
            mmio64(op + CRCR),
            mmio64(op + DCBAAP)
        );

        let mut core = Core {
            run,
            op,
            db,
            csz,
            slot: 0,
            port: 0,
            speed: 0,
            cmd,
            cmd_deq: 0,
            cmd_cycle: true,
            ev,
            ev_deq: 0,
            ev_cycle: true,
            dcbaa,
            in_ctx,
            out_ctx,
            data,
            ep0,
            ep0_deq: 0,
            ep0_cycle: true,
            ep0_maxpkt: 8,
            ep1,
            ep1_deq: 0,
            ep1_cycle: true,
            ep1_buf,
        };

        let (port, speed, slot, maxpkt, interval) = find_hid_port(&mut core, max_ports)?;
        core.port = port;
        core.speed = speed;
        core.slot = slot;

        let maxpkt0 = if speed == 3 { 64 } else { 8 };
        core.ep0_maxpkt = maxpkt0;

        let config_value = frame(core.data)[5];

        // Bring the device to its configured state.
        ep0_set(
            &mut core,
            0x00,
            REQ_SET_CONFIGURATION,
            u16::from(config_value),
            0,
        )?;
        ep0_set(&mut core, 0x21, REQ_SET_PROTOCOL, 0, 0)?;

        // Configure Endpoint: add the interrupt IN pipe to slot + EP0.
        configure_ep(&mut core, speed, maxpkt, interval)?;

        // Persist the state the idle loop needs, then arm the first report.
        unsafe {
            XB_OP = core.op;
            XB_RUN = core.run;
            XB_DB = core.db;
            XB_SLOT = core.slot;
            XB_EV = core.ev;
            XB_EV_DEQ = core.ev_deq;
            XB_EV_CYCLE = core.ev_cycle;
            XB_EP0 = core.ep0;
            XB_EP0_DEQ = core.ep0_deq;
            XB_EP0_CYCLE = core.ep0_cycle;
            XB_DATA = core.data;
            XB_EP1 = core.ep1;
            XB_EP1_DEQ = core.ep1_deq;
            XB_EP1_CYCLE = core.ep1_cycle;
            XB_EP1_BUF = core.ep1_buf;
        }
        arm_ep1();

        Ok(Xhci {
            port: core.port,
            slot: core.slot,
            speed: core.speed,
        })
    }
}

/// Polls the event ring for HID boot reports and re-arms the keyboard endpoint.
///
/// Called from the scheduler idle task; only the keyboard interrupt IN pipe is
/// watched, everything else on the event ring is skipped.
pub fn poll() {
    if !unsafe { PROBE_ENTERED } {
        unsafe {
            PROBE_ENTERED = true;
        }
        crate::kprintln!("[serial] [xhci] POLL ENTER");
    }
    let run = unsafe { XB_RUN };
    if run == 0 {
        return;
    }
    let slot = u32::from(unsafe { XB_SLOT });
    let ev = unsafe { XB_EV };
    let mut deq = unsafe { XB_EV_DEQ };
    let mut cycle = unsafe { XB_EV_CYCLE };

    if slot != 0 {
        unsafe {
            PROBE_POLLS = (PROBE_POLLS + 1) % PROBE_EVERY;
            if PROBE_POLLS == 0 {
                probe_hid_state();
            }
        }
    }

    for _ in 0..TRBS {
        let trb = trb_read(ev, deq);
        if trb[3] & TRB_CYCLE != cycle as u32 {
            break;
        }
        deq += 1;
        if deq == TRBS {
            deq = 0;
            cycle = !cycle;
        }
        mmio64w(run + ERDP, (ev + (deq as u64) * 16) | 8);

        let kind = (trb[3] >> 10) & 0x3F;
        let ev_slot = (trb[3] >> 24) & 0xFF;
        let ep_id = (trb[3] >> 16) & 0x1F;
        crate::kprintln!(
            "[serial] [xhci] any ev kind={} slot={} ep={} w={:08x} {:08x} {:08x} {:08x}",
            kind,
            ev_slot,
            ep_id,
            trb[0],
            trb[1],
            trb[2],
            trb[3]
        );
        if kind == TRB_TRANSFER && ev_slot == slot && ep_id == 3 {
            let cc = trb[2] >> 24;
            if cc == COMP_SUCCESS || cc == COMP_SHORT_PACKET {
                harvest_report();
                arm_ep1();
            }
        }
    }

    unsafe {
        XB_EV_DEQ = deq;
        XB_EV_CYCLE = cycle;
    }

    if slot != 0 {
        let db = unsafe { XB_DB };
        mmio32w(db + u64::from(slot) * 4, 3);
    }
}

fn harvest_report() {
    let buf = unsafe { XB_EP1_BUF };
    let src = memory::physical_to_virtual(buf) as *const u8;
    let mut bytes = [0u8; 8];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = unsafe { ptr::read_volatile(src.add(index)) };
    }
    let current = pack_report(bytes);
    let previous = LAST_REPORT.load(Ordering::Relaxed);
    LAST_REPORT.store(current, Ordering::Relaxed);
    for key in &bytes[2..8] {
        if *key != 0 && !report_has_key(previous, *key) {
            if let Some(sc) = usage_to_scancode(*key) {
                KEY_SCANCODE.store(u64::from(sc), Ordering::Relaxed);
                KEY_SEQ.fetch_add(1, Ordering::Relaxed);
            }
            break;
        }
    }
}

fn pack_report(bytes: [u8; 8]) -> u64 {
    let mut packed = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        packed |= u64::from(*byte) << (8 * index);
    }
    packed
}

fn report_has_key(report: u64, key: u8) -> bool {
    for index in 0..6u64 {
        if (report >> (16 + 8 * index)) & 0xFF == u64::from(key) {
            return true;
        }
    }
    false
}

fn usage_to_scancode(usage: u8) -> Option<u8> {
    match usage {
        0x04..=0x1D => Some(LETTER_SCANS[(usage - 0x04) as usize]),
        0x1E..=0x26 => Some(0x02 + (usage - 0x1E)),
        0x27 => Some(0x0B),
        0x2C => Some(0x39),
        0x2D => Some(0x0C),
        0x2E => Some(0x0D),
        0x30 => Some(0x1A),
        0x31 => Some(0x1B),
        0x33 => Some(0x27),
        0x34 => Some(0x28),
        0x35 => Some(0x2B),
        0x36 => Some(0x2B),
        0x37 => Some(0x33),
        0x38 => Some(0x34),
        0x39 => Some(0x35),
        _ => None,
    }
}

fn arm_ep1() {
    let ep1 = unsafe { XB_EP1 };
    let buf = unsafe { XB_EP1_BUF };
    let mut deq = unsafe { XB_EP1_DEQ };
    let mut cycle = unsafe { XB_EP1_CYCLE };

    let trb = [
        buf as u32,
        (buf >> 32) as u32,
        8,
        trb_type(TRB_NORMAL) | TRB_IOC | cycle as u32,
    ];
    trb_write(ep1, deq, &trb);
    if deq + 1 == TRBS - 1 {
        deq = 0;
        cycle = !cycle;
        set_link_cycle(ep1, cycle, true);
    } else {
        deq += 1;
    }
    unsafe {
        XB_EP1_DEQ = deq;
        XB_EP1_CYCLE = cycle;
    }

    fence(Ordering::SeqCst);
    let db = unsafe { XB_DB };
    let slot = u64::from(unsafe { XB_SLOT });
    crate::kprintln!("[serial] [xhci] arm_ep1 deq={} dbell=3", deq);
    mmio32w(db + slot * 4, 3);
}

/// Diagnostic probe: issues an EP0 HID GET_REPORT control transfer and prints
/// the 8-byte boot report plus USBSTS. Lets us see whether QEMU's device HID
/// queue actually received injected keys, decoupling input routing from the
/// interrupt-IN harvest path.
fn probe_hid_state() {
    let mut core = Core {
        run: unsafe { XB_RUN },
        op: unsafe { XB_OP },
        db: unsafe { XB_DB },
        csz: 32,
        slot: unsafe { XB_SLOT },
        port: 0,
        speed: 0,
        cmd: 0,
        cmd_deq: 0,
        cmd_cycle: false,
        ev: unsafe { XB_EV },
        ev_deq: unsafe { XB_EV_DEQ },
        ev_cycle: unsafe { XB_EV_CYCLE },
        dcbaa: 0,
        in_ctx: 0,
        out_ctx: 0,
        data: unsafe { XB_DATA },
        ep0: unsafe { XB_EP0 },
        ep0_deq: unsafe { XB_EP0_DEQ },
        ep0_cycle: unsafe { XB_EP0_CYCLE },
        ep0_maxpkt: 64,
        ep1: 0,
        ep1_deq: 0,
        ep1_cycle: false,
        ep1_buf: 0,
    };

    let data = core.data;
    match ep0_ctrl(&mut core, IFACE_DIR_IN, REQ_HID_GET_REPORT, 0, 0, 8, data) {
        Ok(()) => {
            let mut bytes = [0u8; 8];
            for (index, slot) in bytes.iter_mut().enumerate() {
                *slot = unsafe {
                    ptr::read_volatile(
                        (memory::physical_to_virtual(core.data) as *const u8).add(index),
                    )
                };
            }
            let usbsts = mmio32(core.op + USBSTS);
            crate::kprintln!(
                "[serial] [xhci] GET_REPORT {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} usbsts={:08x}{}",
                bytes[0],
                bytes[1],
                bytes[2],
                bytes[3],
                bytes[4],
                bytes[5],
                bytes[6],
                bytes[7],
                usbsts,
                if usbsts & USBSTS_HCE != 0 { " HCE!" } else { "" }
            );
        }
        Err(e) => {
            crate::kprintln!("[serial] [xhci] GET_REPORT failed: {}", e);
        }
    }

    unsafe {
        XB_EV_DEQ = core.ev_deq;
        XB_EV_CYCLE = core.ev_cycle;
        XB_EP0_DEQ = core.ep0_deq;
        XB_EP0_CYCLE = core.ep0_cycle;
    }
}

/// Resets a specific root-port device and returns its port number and speed.
fn reset_port(core: &mut Core, port: u8) -> Result<(u8, u8), &'static str> {
    let psc = core.op + PORT_BASE + u64::from(port - 1) * 0x10;
    crate::kprintln!("[serial] [xhci] port {} portsc=0x{:08x}", port, mmio32(psc));
    if mmio32(psc) & PORT_CCS == 0 {
        return Err("xhci: no device on this port");
    }
    let v = mmio32(psc);
    mmio32w(psc, (v & !PORT_CHANGE) | PORT_POWER | PORT_RESET);
    spin_until(|| mmio32(psc) & PORT_RC != 0).map_err(|_| "xhci: port reset timeout")?;
    let v = mmio32(psc);
    mmio32w(psc, (v & !PORT_CHANGE) | PORT_CHANGE);
    spin_until(|| mmio32(psc) & PORT_PE != 0).map_err(|_| "xhci: port did not enable")?;
    let speed = ((mmio32(psc) >> 10) & 0xF) as u8;
    Ok((port, speed))
}

/// Finds the first root port with a connected HID keyboard by iterating
/// all ports, resetting each, and checking the configuration descriptors
/// for a HID interrupt IN endpoint. Returns the port number, speed, and
/// the maxpkt/interval for the HID endpoint.
fn find_hid_port(core: &mut Core, max_ports: u8) -> Result<(u8, u8, u8, u16, u8), &'static str> {
    for port in 1..=max_ports {
        let (p, speed) = match reset_port(core, port) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let slot = match enable_slot(core) {
            Ok(s) => s,
            Err(_) => continue,
        };
        core.slot = slot;
        let maxpkt0 = if speed == 3 { 64 } else { 8 };
        core.ep0_maxpkt = maxpkt0;
        address_device(core, p, speed)?;
        get_descriptor(core, 0x01, 0, 18)?;
        let _ = frame(core.data)[7];
        get_descriptor(core, 0x02, 0, 9)?;
        let total = usize::from(read_u16(frame(core.data), 2));
        if !(18..=4096).contains(&total) {
            continue;
        }
        get_descriptor(core, 0x02, 0, total as u16)?;
        let _config_value = frame(core.data)[5];
        let (maxpkt, interval) = match find_hid_ep(frame(core.data), total) {
            Ok(v) => v,
            Err(_) => continue,
        };
        return Ok((p, speed, slot, maxpkt, interval));
    }
    Err("xhci: no HID interrupt IN endpoint in configuration")
}

fn enable_slot(core: &mut Core) -> Result<u8, &'static str> {
    let done = cmd_run(core, [0, 0, 0, trb_type(TRB_ENABLE_SLOT)])?;
    let cc = done[2] >> 24;
    if cc != COMP_SUCCESS {
        return Err("xhci: enable slot failed");
    }
    Ok(((done[3] >> 24) & 0xFF) as u8)
}

fn address_device(core: &mut Core, port: u8, speed: u8) -> Result<(), &'static str> {
    zero_region(core.in_ctx, 256);
    let csz = core.csz;

    // Point the slot's DCBAA entry at the output context the HC fills in.
    put_u64(core.dcbaa, u64::from(core.slot) * 8, core.out_ctx);

    // Input control context: add the slot and EP0.
    put_u32(core.in_ctx, 4, SLOT_FLAG | EP0_FLAG);
    // Slot context at ictx+csz: speed (20:23), Context Entries = 2 (27:31).
    let slot_start = core.in_ctx + csz;
    put_u32(slot_start, 0, u32::from(speed) << 20 | (2 << 27));
    // Root hub port number (bits 16:23).
    put_u32(slot_start, 4, u32::from(port) << 16);
    // EP0 context at ictx+2*csz.
    let ep0_start = core.in_ctx + 2 * csz;
    put_u32(
        ep0_start,
        4,
        ERROR_COUNT | (CTRL_EP << 3) | (core.ep0_maxpkt << 16),
    );
    put_u64(ep0_start, 8, core.ep0 | 1);
    put_u32(ep0_start, 16, core.ep0_maxpkt);

    let done = cmd_run(
        core,
        [
            core.in_ctx as u32,
            (core.in_ctx >> 32) as u32,
            0,
            (u32::from(core.slot) << 24) | trb_type(TRB_ADDR_DEV),
        ],
    )?;
    let cc = done[2] >> 24;
    if cc != COMP_SUCCESS {
        return Err("xhci: address device failed");
    }
    Ok(())
}

fn get_descriptor(core: &mut Core, dtype: u8, index: u8, length: u16) -> Result<(), &'static str> {
    let value = u16::from(dtype) << 8 | u16::from(index);
    ep0_ctrl(core, 0x80, 0x06, value, 0, length, core.data)
}

fn ep0_set(core: &mut Core, bm: u8, req: u8, value: u16, index: u16) -> Result<(), &'static str> {
    ep0_ctrl(core, bm, req, value, index, 0, 0)
}

fn ep0_ctrl(
    core: &mut Core,
    bm: u8,
    req: u8,
    value: u16,
    index: u16,
    length: u16,
    buffer: u64,
) -> Result<(), &'static str> {
    let data_in = bm & 0x80 != 0;
    let trt = if length == 0 {
        0
    } else if data_in {
        2
    } else {
        3
    };

    // Setup stage: transfer length 8, TRT at 16:17, IDT flag in control.
    let dw0 = u32::from(bm) | (u32::from(req) << 8) | (u32::from(value) << 16);
    let dw1 = u32::from(index) | (u32::from(length) << 16);
    push_ep0(
        core,
        [dw0, dw1, 8 | (trt << 16), trb_type(TRB_SETUP) | TRB_IDT],
    );

    if length > 0 {
        let td_size = packet_count(u32::from(length), core.ep0_maxpkt).min(31);
        let dir = if data_in { TRB_DIR_IN } else { 0 };
        push_ep0(
            core,
            [
                buffer as u32,
                (buffer >> 32) as u32,
                u32::from(length) | (td_size << 17),
                trb_type(TRB_DATA) | dir,
            ],
        );
    }

    let status_in = !data_in || length == 0;
    let dir = if status_in { TRB_DIR_IN } else { 0 };
    push_ep0(core, [0, 0, 0, trb_type(TRB_STATUS) | TRB_IOC | dir]);

    let done = ep0_wait(core)?;
    let cc = done[2] >> 24;
    if cc != COMP_SUCCESS && cc != COMP_SHORT_PACKET {
        return Err("xhci: control transfer failed");
    }
    Ok(())
}

fn push_ep0(core: &mut Core, mut trb: [u32; 4]) {
    trb[3] |= core.ep0_cycle as u32;
    trb_write(core.ep0, core.ep0_deq, &trb);
    if core.ep0_deq + 1 == TRBS - 1 {
        core.ep0_deq = 0;
        core.ep0_cycle = !core.ep0_cycle;
        set_link_cycle(core.ep0, core.ep0_cycle, true);
    } else {
        core.ep0_deq += 1;
    }
}

fn ep0_wait(core: &mut Core) -> Result<[u32; 4], &'static str> {
    fence(Ordering::SeqCst);
    mmio32w(core.db + u64::from(core.slot) * 4, 1);
    let mut waited = 0u32;
    loop {
        let trb = trb_read(core.ev, core.ev_deq);
        if trb[3] & TRB_CYCLE != core.ev_cycle as u32 {
            waited += 1;
            if waited > SPIN_LIMIT {
                return Err("xhci: control transfer timeout");
            }
            core::hint::spin_loop();
            continue;
        }
        core.ev_deq += 1;
        if core.ev_deq == TRBS {
            core.ev_deq = 0;
            core.ev_cycle = !core.ev_cycle;
        }
        mmio64w(core.run + ERDP, (core.ev + (core.ev_deq as u64) * 16) | 8);

        let kind = (trb[3] >> 10) & 0x3F;
        let ev_slot = (trb[3] >> 24) & 0xFF;
        let ep_id = (trb[3] >> 16) & 0x1F;
        if kind == TRB_TRANSFER && ev_slot == u32::from(core.slot) && ep_id == 1 {
            return Ok(trb);
        }
    }
}

fn configure_ep(
    core: &mut Core,
    speed: u8,
    maxpkt: u16,
    interval_ms: u8,
) -> Result<(), &'static str> {
    zero_region(core.in_ctx, 256);
    let csz = core.csz;

    // Keep the slot + EP0 contexts the HC adopted (address, EP0 dequeue).
    // Slot context lives at ictx+csz, EP0 at ictx+2*csz.
    copy_ctx(core.out_ctx, core.in_ctx + csz, csz);
    copy_ctx(core.out_ctx + csz, core.in_ctx + 2 * csz, csz);
    // Add slot + EP1-IN only; QEMU rejects a set EP0 add flag here.
    put_u32(core.in_ctx, 4, SLOT_FLAG | EP1_IN_FLAG);
    // Context Entries = 4 covers context index 3 (EP1-IN).
    put_u32(core.in_ctx + csz, 0, u32::from(speed) << 20 | (4 << 27));

    let ep1_start = core.in_ctx + 4 * csz;
    put_u32(ep1_start, 0, poll_interval(interval_ms) << 16);
    put_u32(
        ep1_start,
        4,
        ERROR_COUNT | (INT_IN_EP << 3) | (u32::from(maxpkt) << 16),
    );
    put_u64(ep1_start, 8, core.ep1 | 1);
    put_u32(ep1_start, 16, u32::from(maxpkt));

    let done = cmd_run(
        core,
        [
            core.in_ctx as u32,
            (core.in_ctx >> 32) as u32,
            0,
            (u32::from(core.slot) << 24) | trb_type(TRB_CONFIG_EP),
        ],
    )?;
    let cc = done[2] >> 24;
    if cc != COMP_SUCCESS {
        return Err("xhci: configure endpoint failed");
    }
    Ok(())
}

fn zero_region(phys: u64, size: usize) {
    let virt = memory::physical_to_virtual(phys) as *mut u8;
    unsafe { ptr::write_bytes(virt, 0, size) };
}

fn copy_ctx(src: u64, dst: u64, size: u64) {
    let s = memory::physical_to_virtual(src) as *const u8;
    let d = memory::physical_to_virtual(dst) as *mut u8;
    unsafe { ptr::copy_nonoverlapping(s, d, size as usize) };
}

fn poll_interval(interval_ms: u8) -> u32 {
    let target = u32::from(interval_ms) * 8;
    let mut exponent = 0;
    while (1u32 << exponent) < target && exponent < 16 {
        exponent += 1;
    }
    exponent
}

fn packet_count(length: u32, maxpkt: u32) -> u32 {
    if length.is_multiple_of(maxpkt) {
        length / maxpkt
    } else {
        length / maxpkt + 1
    }
}

fn find_hid_ep(config: &[u8], total: usize) -> Result<(u16, u8), &'static str> {
    let mut offset = 0usize;
    let mut hid = false;
    while offset + 1 < total {
        let len = config[offset] as usize;
        if len == 0 {
            break;
        }
        let kind = config[offset + 1];
        match kind {
            DESC_INTERFACE => {
                if offset + 8 <= total {
                    hid = config[offset + 5] == IFACE_CLASS_HID;
                }
            }
            DESC_ENDPOINT if hid && offset + 7 <= total => {
                let attrs = config[offset + 3];
                let addr = config[offset + 2];
                if attrs & 0x03 == 3 && addr & 0x80 != 0 {
                    let maxpkt = read_u16(config, offset + 4);
                    let interval = config[offset + 6];
                    return Ok((maxpkt, interval));
                }
            }
            _ => {}
        }
        offset += len;
    }
    Err("xhci: no HID interrupt IN endpoint in configuration")
}

fn cmd_run(core: &mut Core, mut trb: [u32; 4]) -> Result<[u32; 4], &'static str> {
    trb[3] |= core.cmd_cycle as u32;
    trb_write(core.cmd, core.cmd_deq, &trb);
    if core.cmd_deq + 1 == TRBS - 1 {
        core.cmd_deq = 0;
        core.cmd_cycle = !core.cmd_cycle;
        set_link_cycle(core.cmd, core.cmd_cycle, true);
    } else {
        core.cmd_deq += 1;
    }

    fence(Ordering::SeqCst);
    mmio32w(core.db, 0);

    let mut waited = 0u32;
    loop {
        let trb = trb_read(core.ev, core.ev_deq);
        if trb[3] & TRB_CYCLE != core.ev_cycle as u32 {
            waited += 1;
            if waited > SPIN_LIMIT {
                return Err("xhci: command timeout");
            }
            core::hint::spin_loop();
            continue;
        }
        core.ev_deq += 1;
        if core.ev_deq == TRBS {
            core.ev_deq = 0;
            core.ev_cycle = !core.ev_cycle;
        }
        mmio64w(core.run + ERDP, (core.ev + (core.ev_deq as u64) * 16) | 8);

        if (trb[3] >> 10) & 0x3F == TRB_COMPLETION {
            return Ok(trb);
        }
    }
}

/// Places the ring's closing Link TRB (toggle-cycle) before the caller starts
/// producing TRBs at index 0.
fn setup_ring(phys: u64, cycle: bool) {
    let trb = [
        phys as u32,
        (phys >> 32) as u32,
        0,
        trb_type(TRB_LINK) | TRB_TC | cycle as u32,
    ];
    trb_write(phys, TRBS - 1, &trb);
}

/// Re-arms the closing Link TRB with a fresh cycle bit after a wrap.
fn set_link_cycle(phys: u64, cycle: bool, _toggle: bool) {
    trb_write(
        phys,
        TRBS - 1,
        &[
            phys as u32,
            (phys >> 32) as u32,
            0,
            trb_type(TRB_LINK) | TRB_TC | cycle as u32,
        ],
    );
}

fn trb_type(kind: u32) -> u32 {
    kind << 10
}

fn trb_read(phys: u64, index: usize) -> [u32; 4] {
    let base = memory::physical_to_virtual(phys) as *const u32;
    let offset = index * 4;
    [
        unsafe { ptr::read_volatile(base.add(offset)) },
        unsafe { ptr::read_volatile(base.add(offset + 1)) },
        unsafe { ptr::read_volatile(base.add(offset + 2)) },
        unsafe { ptr::read_volatile(base.add(offset + 3)) },
    ]
}

fn trb_write(phys: u64, index: usize, trb: &[u32; 4]) {
    let base = memory::physical_to_virtual(phys) as *mut u32;
    let offset = index * 4;
    unsafe {
        ptr::write_volatile(base.add(offset), trb[0]);
        ptr::write_volatile(base.add(offset + 1), trb[1]);
        ptr::write_volatile(base.add(offset + 2), trb[2]);
        ptr::write_volatile(base.add(offset + 3), trb[3]);
    }
}

fn alloc_zero() -> Option<u64> {
    let phys = memory::alloc_frame()?;
    let virt = memory::physical_to_virtual(phys) as *mut u8;
    unsafe { ptr::write_bytes(virt, 0, memory::PAGE_SIZE as usize) };
    Some(phys)
}

fn frame(phys: u64) -> &'static [u8] {
    let virt = memory::physical_to_virtual(phys) as *const u8;
    unsafe { core::slice::from_raw_parts(virt, memory::PAGE_SIZE as usize) }
}

fn put_u16(buf: u64, offset: u64, value: u16) {
    put_u32(buf, offset, u32::from(value) & 0xFFFF);
}

fn put_u32(buf: u64, offset: u64, value: u32) {
    let ptr = memory::physical_to_virtual(buf) as *mut u8;
    unsafe {
        ptr::write_volatile(ptr.add(offset as usize), value as u8);
        ptr::write_volatile(ptr.add(offset as usize + 1), (value >> 8) as u8);
        ptr::write_volatile(ptr.add(offset as usize + 2), (value >> 16) as u8);
        ptr::write_volatile(ptr.add(offset as usize + 3), (value >> 24) as u8);
    }
}

fn put_u64(buf: u64, offset: u64, value: u64) {
    put_u32(buf, offset, value as u32);
    put_u32(buf, offset + 4, (value >> 32) as u32);
}

fn read_u16(buf: &[u8], offset: usize) -> u16 {
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&buf[offset..offset + 2]);
    u16::from_le_bytes(bytes)
}

fn mmio32(addr: u64) -> u32 {
    unsafe { ptr::read_volatile(addr as *const u32) }
}

fn mmio64(addr: u64) -> u64 {
    unsafe { ptr::read_volatile(addr as *const u64) }
}

fn mmio32w(addr: u64, value: u32) {
    unsafe { ptr::write_volatile(addr as *mut u32, value) }
}

fn mmio64w(addr: u64, value: u64) {
    unsafe { ptr::write_volatile(addr as *mut u64, value) }
}

fn spin_until<F: FnMut() -> bool>(mut condition: F) -> Result<(), ()> {
    for _ in 0..SPIN_LIMIT {
        if condition() {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(())
}
