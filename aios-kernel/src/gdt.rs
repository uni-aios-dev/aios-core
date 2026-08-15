pub const KERNEL_CS: u16 = 0x08;
#[allow(dead_code)]
pub const KERNEL_DS: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x28;
pub const DOUBLE_FAULT_IST_INDEX: u8 = 1;

core::arch::global_asm!(
    r#"
.text
.global aios_reload_segments
aios_reload_segments:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    ret
"#
);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_middle: u8,
    access: u8,
    granularity: u8,
    base_high: u8,
}

impl GdtEntry {
    const fn code(access: u8) -> Self {
        Self {
            limit_low: 0xFFFF,
            base_low: 0,
            base_middle: 0,
            access,
            granularity: 0xA0,
            base_high: 0,
        }
    }

    const fn data(access: u8) -> Self {
        Self {
            limit_low: 0xFFFF,
            base_low: 0,
            base_middle: 0,
            access,
            granularity: 0xA0,
            base_high: 0,
        }
    }

    fn tss_low(base: u64, limit: u16) -> Self {
        Self {
            limit_low: limit,
            base_low: (base & 0xFFFF) as u16,
            base_middle: ((base >> 16) & 0xFF) as u8,
            access: 0x89,
            granularity: 0,
            base_high: ((base >> 24) & 0xFF) as u8,
        }
    }

    fn tss_high(base: u64) -> Self {
        Self {
            limit_low: 0,
            base_low: ((base >> 32) & 0xFFFF) as u16,
            base_middle: ((base >> 48) & 0xFF) as u8,
            access: 0,
            granularity: 0,
            base_high: ((base >> 56) & 0xFF) as u8,
        }
    }
}

#[repr(C, packed)]
struct Descriptor {
    limit: u16,
    base: u64,
}

#[repr(C, align(16))]
struct TaskStateSegment {
    reserved: u32,
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    reserved2: u64,
    ist: [u64; 7],
    reserved3: u64,
    reserved4: u64,
    reserved5: u16,
    iomap_base: u16,
}

impl TaskStateSegment {
    const fn new() -> Self {
        Self {
            reserved: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved2: 0,
            ist: [0; 7],
            reserved3: 0,
            reserved4: 0,
            reserved5: 0,
            iomap_base: core::mem::size_of::<Self>() as u16,
        }
    }
}

const GDT_ENTRIES: usize = 7;

#[repr(C, align(16))]
struct Gdt {
    entries: [GdtEntry; GDT_ENTRIES],
}

impl Gdt {
    const fn new() -> Self {
        const NULL_ENTRY: GdtEntry = GdtEntry {
            limit_low: 0,
            base_low: 0,
            base_middle: 0,
            access: 0,
            granularity: 0,
            base_high: 0,
        };
        let mut entries = [NULL_ENTRY; GDT_ENTRIES];
        entries[1] = GdtEntry::code(0x9A);
        entries[2] = GdtEntry::data(0x92);
        entries[3] = GdtEntry::code(0xFA);
        entries[4] = GdtEntry::data(0xF2);
        Self { entries }
    }
}

struct GdtManager {
    gdt: Gdt,
    tss: TaskStateSegment,
    descriptor: Descriptor,
}

static mut MANAGER: GdtManager = GdtManager {
    gdt: Gdt::new(),
    tss: TaskStateSegment::new(),
    descriptor: Descriptor { limit: 0, base: 0 },
};

unsafe fn reload_segments() {
    core::arch::asm!("call aios_reload_segments", options(nostack));
}

pub fn init(double_fault_stack_top: u64) {
    unsafe {
        let manager = &mut *core::ptr::addr_of_mut!(MANAGER);
        manager.tss.ist[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_stack_top;
        let tss_addr = &manager.tss as *const TaskStateSegment as u64;
        manager.gdt.entries[5] = GdtEntry::tss_low(
            tss_addr,
            (core::mem::size_of::<TaskStateSegment>() - 1) as u16,
        );
        manager.gdt.entries[6] = GdtEntry::tss_high(tss_addr);
        let descriptor = &mut *core::ptr::addr_of_mut!(manager.descriptor);
        descriptor.limit = (core::mem::size_of::<Gdt>() - 1) as u16;
        descriptor.base = &manager.gdt as *const Gdt as u64;
        core::arch::asm!("lgdt [{}]", in(reg) descriptor, options(nostack, preserves_flags, readonly));
        reload_segments();
        core::arch::asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

pub fn set_kernel_stack(stack_top: u64) {
    unsafe {
        let manager = &mut *core::ptr::addr_of_mut!(MANAGER);
        manager.tss.rsp0 = stack_top;
    }
}
