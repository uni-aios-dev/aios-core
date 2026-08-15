use crate::gdt;

pub const IDT_ENTRIES: usize = 256;
pub const INTERRUPT_GATE: u8 = 0x8E;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    flags: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const fn new() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            flags: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn set_handler(&mut self, handler: u64, flags: u8) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.selector = gdt::KERNEL_CS;
        self.ist = 0;
        self.flags = flags;
    }
}

#[repr(C)]
struct Idt {
    entries: [IdtEntry; IDT_ENTRIES],
}

impl Idt {
    const fn new() -> Self {
        Self {
            entries: [IdtEntry::new(); IDT_ENTRIES],
        }
    }
}

#[repr(C, packed)]
struct Descriptor {
    limit: u16,
    base: u64,
}

static mut IDT: Idt = Idt::new();
static mut DESCRIPTOR: Descriptor = Descriptor { limit: 0, base: 0 };

extern "C" {
    static aios_handler_table: [u64; 256];
}

pub fn init() {
    unsafe {
        let idt = &mut *core::ptr::addr_of_mut!(IDT);
        let table = &aios_handler_table;
        for (index, entry) in idt.entries.iter_mut().enumerate() {
            entry.set_handler(table[index], INTERRUPT_GATE);
        }
        set_ist(8, gdt::DOUBLE_FAULT_IST_INDEX);
        let descriptor = &mut *core::ptr::addr_of_mut!(DESCRIPTOR);
        descriptor.limit = (core::mem::size_of::<Idt>() - 1) as u16;
        descriptor.base = idt as *const Idt as u64;
        core::arch::asm!("lidt [{}]", in(reg) descriptor, options(nostack, preserves_flags, readonly));
    }
}

fn set_ist(vector: u8, ist: u8) {
    unsafe {
        let idt = &mut *core::ptr::addr_of_mut!(IDT);
        idt.entries[vector as usize].ist = ist;
    }
}
