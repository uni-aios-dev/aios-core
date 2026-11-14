//! aios-boot — the AIOS UEFI bootloader.
//!
//! Responsibilities, in order:
//!
//! 1. open the AIOS partition and read `/aios/boot.conf`;
//! 2. load the kernel ELF and every module listed in `boot.conf`;
//! 3. query GOP and the ACPI RSDP from the firmware;
//! 4. call `ExitBootServices`;
//! 5. build a private set of page tables: a higher-half direct map of all
//!    physical memory and a W^X mapping of the kernel at its link address;
//! 6. publish a [`aios_bootinfo::BootInfo`] and jump to the kernel entry with
//!    the structure pointer in `rdi`.
//!
//! No firmware service is used after step 4: the kernel runs on its own page
//! tables and the firmware is completely out of the picture.

#![no_std]
#![no_main]

extern crate alloc;

mod framebuffer;
mod runtime;
mod serial;

use aios_boot::config::{self, BootConfig};
use aios_boot::elf::Elf;
use aios_boot::paging::{
    FramePool, PageTables, BOOT_STACK_VIRT, HHDM_BASE, HUGE_PAGE_SIZE, PAGE_SIZE, PTE_NX, PTE_PCD,
    PTE_PWT, PTE_WRITE,
};
use aios_boot::BootError;
use aios_bootinfo::{
    BootInfo, FramebufferInfo, MemoryRegion, MAX_MEMORY_REGIONS, MAX_MODULE_NAME, MEM_USABLE,
};
use core::arch::asm;
use uefi::boot::{AllocateType, MemoryType};
use uefi::fs::FileSystem;
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};
use uefi::prelude::*;
use uefi::CString16;

/// Size of the stack handed to the kernel.
const KERNEL_STACK_SIZE: u64 = 64 * 1024;

core::arch::global_asm!(
    ".section .text",
    ".p2align 12",
    ".globl aios_switch_to_kernel",
    "aios_switch_to_kernel:",
    "    cli",
    "    mov cr3, rdi",
    "    mov rsp, rsi",
    "    mov rdi, rdx",
    "    jmp rcx",
);

unsafe extern "sysv64" {
    fn aios_switch_to_kernel(cr3: u64, stack_top: u64, bootinfo: u64, entry: u64) -> !;
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().expect("failed to initialise UEFI helpers");
    serial::init();
    match boot() {
        Ok(()) => halt(),
        Err(error) => {
            uefi::println!("aios-boot: {} ({:?})", error.message(), error);
            sprintln!("aios-boot: {} ({:?})", error.message(), error);
            Status::LOAD_ERROR
        }
    }
}

fn boot() -> Result<(), BootError> {
    let image = boot::image_handle();
    let mut fs =
        FileSystem::new(boot::get_image_file_system(image).map_err(|_| BootError::Partition)?);

    let boot_conf = fs
        .read_to_string(cstr16!("\\aios\\boot.conf"))
        .map_err(|_| BootError::Config)?;
    let config = config::parse(&boot_conf)?;
    drop(boot_conf);
    sprintln!("aios-boot: kernel = {}", config.kernel);

    let kernel_path =
        CString16::try_from(config.kernel_uefi_path().as_str()).map_err(|_| BootError::Config)?;
    let kernel_data = fs
        .read(kernel_path.as_ref())
        .map_err(|_| BootError::Kernel)?;
    let elf = Elf::parse(&kernel_data)?;
    sprintln!(
        "aios-boot: kernel entry = 0x{:x}, base = 0x{:x}, size = 0x{:x}",
        elf.entry,
        elf.base,
        elf.size
    );

    let framebuffer = framebuffer::detect();
    match &framebuffer {
        Some(fb) => sprintln!(
            "aios-boot: framebuffer {}x{} {} bpp at 0x{:x}",
            fb.width,
            fb.height,
            fb.bpp,
            fb.address
        ),
        None => sprintln!("aios-boot: framebuffer = none"),
    }
    let rsdp = acpi_rsdp();
    sprintln!("aios-boot: rsdp = 0x{:x}", rsdp);

    let pre_map = boot::memory_map(MemoryType::LOADER_DATA).map_err(|_| BootError::MemoryMap)?;
    let hhdm_end = hhdm_end(&pre_map, framebuffer.as_ref())?;

    let bootinfo_phys = allocate(pages_for(core::mem::size_of::<BootInfo>() as u64))?;
    // SAFETY: freshly allocated, zeroed LOADER_DATA pages.
    unsafe {
        core::ptr::write(bootinfo_phys as *mut BootInfo, BootInfo::new());
        let info = bootinfo_phys as *mut BootInfo;
        (*info).hhdm_offset = HHDM_BASE;
        (*info).kernel_phys = 0;
        (*info).kernel_virt = elf.base;
        (*info).kernel_size = elf.size;
        (*info).rsdp = rsdp;
        (*info).framebuffer = framebuffer.unwrap_or(FramebufferInfo::NONE);
    }

    let kernel_pages = usize::try_from(elf.page_count()).map_err(|_| BootError::Kernel)?;
    let kernel_phys = allocate(kernel_pages)?;
    // SAFETY: the buffer is `kernel_pages` zeroed pages; `copy_into` bounds the
    // segments against `elf.size`.
    unsafe {
        elf.copy_into(&kernel_data, kernel_phys)?;
        (*(bootinfo_phys as *mut BootInfo)).kernel_phys = kernel_phys;
    }
    drop(kernel_data);

    load_modules(&mut fs, &config, bootinfo_phys)?;
    drop(config);

    let stack_pages = pages_for(KERNEL_STACK_SIZE);
    let stack_phys = allocate(stack_pages)?;

    let page_table_pages = page_table_pages(hhdm_end, elf.page_count(), stack_pages)?;
    let pool_phys = allocate(page_table_pages)?;

    // Everything below must happen before `ExitBootServices`: the firmware
    // allocator is gone afterwards.
    drop(fs);
    drop(pre_map);

    sprintln!("aios-boot: exiting boot services");
    // SAFETY: all protocol handles and boot-services-owned values are dropped.
    let memory_map = unsafe { boot::exit_boot_services(None) };
    // SAFETY: keep interrupts off until the kernel installs its own IDT.
    unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };

    build_and_jump(BuildArgs {
        elf: &elf,
        kernel_phys,
        bootinfo_phys,
        stack_phys,
        stack_pages,
        pool_phys,
        page_table_pages,
        hhdm_end,
        framebuffer,
        memory_map: &memory_map,
    })
}

struct BuildArgs<'a> {
    elf: &'a Elf,
    kernel_phys: u64,
    bootinfo_phys: u64,
    stack_phys: u64,
    stack_pages: usize,
    pool_phys: u64,
    page_table_pages: usize,
    hhdm_end: u64,
    framebuffer: Option<FramebufferInfo>,
    memory_map: &'a MemoryMapOwned,
}

fn build_and_jump(args: BuildArgs<'_>) -> ! {
    // SAFETY: the pool covers a contiguous LOADER_DATA allocation and boot
    // services are already gone, so physical pages stay ours.
    let pool = unsafe { FramePool::new(args.pool_phys, args.page_table_pages as u64 * PAGE_SIZE) };
    let mut tables = match PageTables::new(pool) {
        Some(tables) => tables,
        None => fatal("page-table pool exhausted"),
    };

    let (fb_start, fb_end) = match &args.framebuffer {
        Some(fb) => (
            fb.address,
            fb.address
                .saturating_add(u64::from(fb.pitch).saturating_mul(u64::from(fb.height))),
        ),
        None => (0, 0),
    };

    let huge_pages = args.hhdm_end.div_ceil(HUGE_PAGE_SIZE);
    for index in 0..huge_pages {
        let phys = index * HUGE_PAGE_SIZE;
        let next = phys + HUGE_PAGE_SIZE;
        let mut flags = PTE_WRITE | PTE_NX;
        if fb_end > fb_start && phys < fb_end && next > fb_start {
            flags |= PTE_PCD | PTE_PWT;
        }
        if tables.map_2m(HHDM_BASE + phys, phys, flags).is_none() {
            fatal("direct-map page tables exhausted");
        }
    }

    for page in 0..args.elf.page_count() {
        let virt = args.elf.base + page * PAGE_SIZE;
        let phys = args.kernel_phys + page * PAGE_SIZE;
        let rights = args.elf.rights(virt);
        let mut flags = 0;
        if rights.writable {
            flags |= PTE_WRITE;
        }
        if !rights.executable {
            flags |= PTE_NX;
        }
        if tables.map_4k(virt, phys, flags).is_none() {
            fatal("kernel mapping failed");
        }
    }

    for page in 0..args.stack_pages as u64 {
        let virt = BOOT_STACK_VIRT + page * PAGE_SIZE;
        let phys = args.stack_phys + page * PAGE_SIZE;
        if tables.map_4k(virt, phys, PTE_WRITE | PTE_NX).is_none() {
            fatal("boot stack mapping failed");
        }
    }

    let stub = aios_switch_to_kernel as *const () as usize as u64;
    let stub_page = stub & !(PAGE_SIZE - 1);
    if tables.map_4k(stub_page, stub_page, 0).is_none() {
        fatal("hand-off stub mapping failed");
    }

    enable_nxe();
    memory_regions(args.bootinfo_phys as *mut BootInfo, args.memory_map);

    // SAFETY: BootInfo lives in the direct map; the kernel entry was validated
    // to lie inside the mapped image; the stub switches stacks atomically.
    unsafe {
        sprintln!(
            "aios-boot: entering kernel at 0x{:x} (cr3 = 0x{:x}, stack = 0x{:x})",
            args.elf.entry,
            tables.cr3(),
            BOOT_STACK_VIRT + KERNEL_STACK_SIZE - 8
        );
        aios_switch_to_kernel(
            tables.cr3(),
            BOOT_STACK_VIRT + KERNEL_STACK_SIZE - 8,
            HHDM_BASE + args.bootinfo_phys,
            args.elf.entry,
        )
    }
}

fn load_modules(
    fs: &mut FileSystem,
    config: &BootConfig,
    bootinfo_phys: u64,
) -> Result<(), BootError> {
    let info = bootinfo_phys as *mut BootInfo;
    for (index, module) in config.modules.iter().enumerate() {
        let name = module.name.as_bytes();
        if name.len() >= MAX_MODULE_NAME {
            return Err(BootError::Config);
        }
        let path =
            CString16::try_from(module.uefi_path().as_str()).map_err(|_| BootError::Config)?;
        let data = fs.read(path.as_ref()).map_err(|_| BootError::Partition)?;
        let pages = pages_for(data.len() as u64).max(1);
        let phys = allocate(pages)?;
        // SAFETY: `phys` covers at least `pages` zeroed pages, and `data` fits.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), phys as *mut u8, data.len());
            let slot = &mut (*info).modules[index];
            slot.name[..name.len()].copy_from_slice(name);
            slot.phys = phys;
            slot.size = data.len() as u64;
            slot.kind = module.kind;
            slot.reserved = 0;
            (*info).module_count = (index + 1) as u32;
        }
        sprintln!(
            "aios-boot: module '{}' kind={} size={} at 0x{:x}",
            module.name,
            module.kind,
            data.len(),
            phys
        );
        drop(data);
    }
    Ok(())
}

fn memory_regions(info: *mut BootInfo, map: &MemoryMapOwned) {
    let mut count = 0usize;
    let mut last_end = 0u64;
    for descriptor in map.entries() {
        if descriptor.ty != MemoryType::CONVENTIONAL {
            continue;
        }
        let start = descriptor.phys_start;
        let end = start.saturating_add(descriptor.page_count.saturating_mul(PAGE_SIZE));
        if start >= end {
            continue;
        }
        if count > 0 && start <= last_end {
            if end > last_end {
                // SAFETY: `count > 0` and `info` points at writable memory.
                unsafe { (*info).memory_regions[count - 1].end = end };
                last_end = end;
            }
            continue;
        }
        if count >= MAX_MEMORY_REGIONS {
            break;
        }
        // SAFETY: `count` is in bounds and `info` is writable.
        unsafe {
            (*info).memory_regions[count] = MemoryRegion {
                start,
                end,
                kind: MEM_USABLE,
                reserved: 0,
            };
        }
        last_end = end;
        count += 1;
    }
    // SAFETY: `info` is writable at this point of the hand-off.
    unsafe { (*info).memory_region_count = count as u32 };
}

fn hhdm_end(map: &MemoryMapOwned, framebuffer: Option<&FramebufferInfo>) -> Result<u64, BootError> {
    let mut end = 0u64;
    for descriptor in map.entries() {
        if descriptor.ty == MemoryType::MMIO || descriptor.ty == MemoryType::MMIO_PORT_SPACE {
            continue;
        }
        let region_end = descriptor
            .phys_start
            .saturating_add(descriptor.page_count.saturating_mul(PAGE_SIZE));
        end = end.max(region_end);
    }
    if let Some(fb) = framebuffer {
        let fb_end = fb
            .address
            .saturating_add(u64::from(fb.pitch).saturating_mul(u64::from(fb.height)));
        end = end.max(fb_end);
    }
    if end == 0 {
        return Err(BootError::MemoryMap);
    }
    Ok(end)
}

fn page_table_pages(
    hhdm_end: u64,
    kernel_pages: u64,
    stack_pages: usize,
) -> Result<usize, BootError> {
    const GIB: u64 = 1024 * 1024 * 1024;
    const HALF_TIB: u64 = 512 * GIB;
    let direct_map_directories = hhdm_end.div_ceil(GIB);
    let direct_map_pointers = hhdm_end.div_ceil(HALF_TIB);
    let kernel_tables = kernel_pages.div_ceil(512);
    let stack_tables = 3;
    let identity_tables = 3;
    // PML4 + upper levels + a little slack.
    let total = 1
        + direct_map_pointers
        + direct_map_directories
        + kernel_tables
        + stack_tables
        + identity_tables
        + 8
        + (stack_pages as u64).div_ceil(512);
    usize::try_from(total).map_err(|_| BootError::Paging)
}

fn allocate(count: usize) -> Result<u64, BootError> {
    if count == 0 {
        return Err(BootError::MemoryMap);
    }
    let ptr = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, count)
        .map_err(|_| BootError::MemoryMap)?;
    let address = ptr.as_ptr() as u64;
    // SAFETY: the allocation spans `count` writable pages.
    unsafe { core::ptr::write_bytes(address as *mut u8, 0, count * PAGE_SIZE as usize) };
    Ok(address)
}

fn pages_for(bytes: u64) -> usize {
    bytes.div_ceil(PAGE_SIZE) as usize
}

fn acpi_rsdp() -> u64 {
    uefi::system::with_config_table(|entries| {
        for entry in entries {
            if entry.guid == uefi::table::cfg::ConfigTableEntry::ACPI2_GUID {
                return entry.address as u64;
            }
        }
        for entry in entries {
            if entry.guid == uefi::table::cfg::ConfigTableEntry::ACPI_GUID {
                return entry.address as u64;
            }
        }
        0
    })
}

fn enable_nxe() {
    const EFER: u32 = 0xC000_0080;
    const NXE: u64 = 1 << 11;
    let low: u32;
    let high: u32;
    // SAFETY: `rdmsr`/`wrmsr` on the architectural EFER MSR.
    unsafe {
        asm!("rdmsr", in("ecx") EFER, out("eax") low, out("edx") high, options(nomem, nostack));
        let value = (((high as u64) << 32) | u64::from(low)) | NXE;
        asm!(
            "wrmsr",
            in("ecx") EFER,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack)
        );
    }
}

fn fatal(message: &str) -> ! {
    sprintln!("aios-boot: FATAL: {}", message);
    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: nothing else can make progress in this state.
        unsafe { asm!("cli; hlt", options(nomem, nostack, preserves_flags)) };
    }
}
