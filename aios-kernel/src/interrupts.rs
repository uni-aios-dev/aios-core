use crate::{kprintln, port, vprintln};
use core::sync::atomic::{AtomicU64, Ordering};

pub const PIC1_CMD: u16 = 0x20;
pub const PIC1_DATA: u16 = 0x21;
pub const PIC2_CMD: u16 = 0xA0;
pub const PIC2_DATA: u16 = 0xA1;
pub const PIC_EOI: u8 = 0x20;
pub const PIT_CMD: u16 = 0x43;
pub const PIT_CH0: u16 = 0x40;
pub const KEYBOARD_PORT: u16 = 0x60;
pub const IRQ_MASTER_OFFSET: u8 = 0x20;
pub const IRQ_SLAVE_OFFSET: u8 = 0x28;
pub const IRQ_BASE: u8 = IRQ_MASTER_OFFSET;
pub const IRQ_END: u8 = IRQ_SLAVE_OFFSET + 7;
pub const TIMER_HZ: u64 = 100;

pub static TICKS: AtomicU64 = AtomicU64::new(0);
pub static LAST_SCANCODE: AtomicU64 = AtomicU64::new(0);

core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/irq_stubs.S")));

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InterruptFrame {
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub ds: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[no_mangle]
pub extern "C" fn aios_handle_interrupt(frame: *mut InterruptFrame) {
    let frame = unsafe { &mut *frame };
    let vector = frame.vector;
    match vector {
        0 => fatal(frame, "DIVIDE BY ZERO"),
        6 => fatal(frame, "INVALID OPCODE"),
        8 => fatal(frame, "DOUBLE FAULT"),
        13 => fatal(frame, "GENERAL PROTECTION FAULT"),
        14 => page_fault(frame),
        v if (IRQ_BASE as u64..=IRQ_END as u64).contains(&v) => {
            match vector {
                32 => {
                    TICKS.fetch_add(1, Ordering::Relaxed);
                }
                33 => {
                    LAST_SCANCODE.store(
                        unsafe { port::inb(KEYBOARD_PORT) } as u64,
                        Ordering::Relaxed,
                    );
                }
                _ => {}
            }
            pic_eoi(vector);
        }
        _ => fatal(frame, "UNHANDLED INTERRUPT"),
    }
}

fn page_fault(frame: &InterruptFrame) -> ! {
    let cr2 = read_cr2();
    kprintln!(
        "PAGE FAULT: addr={:#x} ip={:#x} err={:#x}",
        cr2,
        frame.rip,
        frame.error_code
    );
    vprintln!(
        "PAGE FAULT: addr={:#x} ip={:#x} err={:#x}",
        cr2,
        frame.rip,
        frame.error_code
    );
    halt()
}

fn fatal(frame: &InterruptFrame, name: &str) -> ! {
    kprintln!(
        "{}: vector={} ip={:#x} err={:#x}",
        name,
        frame.vector,
        frame.rip,
        frame.error_code
    );
    vprintln!(
        "{}: vector={} ip={:#x} err={:#x}",
        name,
        frame.vector,
        frame.rip,
        frame.error_code
    );
    halt()
}

fn halt() -> ! {
    unsafe {
        core::arch::asm!("cli", options(nostack, preserves_flags));
        loop {
            core::arch::asm!("hlt", options(nostack, preserves_flags));
        }
    }
}

fn read_cr2() -> u64 {
    let cr2: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
    }
    cr2
}

pub fn init_pic() {
    unsafe {
        port::outb(PIC1_CMD, 0x11);
        port::io_wait();
        port::outb(PIC2_CMD, 0x11);
        port::io_wait();
        port::outb(PIC1_DATA, IRQ_MASTER_OFFSET);
        port::io_wait();
        port::outb(PIC2_DATA, IRQ_SLAVE_OFFSET);
        port::io_wait();
        port::outb(PIC1_DATA, 0x04);
        port::io_wait();
        port::outb(PIC2_DATA, 0x02);
        port::io_wait();
        port::outb(PIC1_DATA, 0x01);
        port::io_wait();
        port::outb(PIC2_DATA, 0x01);
        port::io_wait();
        port::outb(PIC1_DATA, 0xFC);
        port::outb(PIC2_DATA, 0xFF);
    }
}

pub fn init_pit() {
    let divisor = (1193182 / TIMER_HZ) as u16;
    unsafe {
        port::outb(PIT_CMD, 0x36);
        port::outb(PIT_CH0, (divisor & 0xFF) as u8);
        port::outb(PIT_CH0, (divisor >> 8) as u8);
    }
}

fn pic_eoi(vector: u64) {
    unsafe {
        if vector >= IRQ_SLAVE_OFFSET as u64 {
            port::outb(PIC2_CMD, PIC_EOI);
        }
        port::outb(PIC1_CMD, PIC_EOI);
    }
}

pub fn scancode_to_char(sc: u8) -> Option<char> {
    match sc {
        0x02..=0x0A => Some((b'1' + (sc - 0x02)) as char),
        0x0B => Some('0'),
        0x10 => Some('q'),
        0x11 => Some('w'),
        0x12 => Some('e'),
        0x13 => Some('r'),
        0x14 => Some('t'),
        0x15 => Some('y'),
        0x16 => Some('u'),
        0x17 => Some('i'),
        0x18 => Some('o'),
        0x19 => Some('p'),
        0x1A => Some('['),
        0x1B => Some(']'),
        0x1E => Some('a'),
        0x1F => Some('s'),
        0x20 => Some('d'),
        0x21 => Some('f'),
        0x22 => Some('g'),
        0x23 => Some('h'),
        0x24 => Some('j'),
        0x25 => Some('k'),
        0x26 => Some('l'),
        0x27 => Some(';'),
        0x28 => Some('\''),
        0x2B => Some('`'),
        0x2C => Some('z'),
        0x2D => Some('x'),
        0x2E => Some('c'),
        0x2F => Some('v'),
        0x30 => Some('b'),
        0x31 => Some('n'),
        0x32 => Some('m'),
        0x33 => Some(','),
        0x34 => Some('.'),
        0x35 => Some('/'),
        0x39 => Some(' '),
        _ => None,
    }
}
