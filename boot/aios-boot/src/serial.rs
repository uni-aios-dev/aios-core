//! Minimal COM1 writer used after `ExitBootServices`.
//!
//! The UEFI console protocols die with boot services, but the kernel needs a
//! way to see that the loader reached the hand-off even when the framebuffer is
//! absent. The loader therefore writes a few lines to the first legacy serial
//! port directly; the kernel re-initialises the same port at entry.

use core::arch::asm;
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

/// Programs COM1 to 115200 8N1, matching the kernel's own `serial::init`.
pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x03);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
}

/// Writes raw bytes, waiting for the transmit holding register.
pub fn write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        unsafe {
            let mut timeout = 0xFFFFu32;
            while inb(COM1 + 5) & 0x20 == 0 {
                timeout -= 1;
                if timeout == 0 {
                    return;
                }
            }
            outb(COM1, byte);
        }
    }
}

/// Writes a string to COM1.
pub fn write_str(text: &str) {
    write_bytes(text.as_bytes());
}

/// Formatter adapter used by the [`sprintln!`](crate::sprintln) macro.
pub struct Writer;

impl Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write_str(text);
        Ok(())
    }
}

/// Writes a formatted line to COM1.
#[macro_export]
macro_rules! sprintln {
    () => ($crate::serial::write_str("\n"));
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::serial::Writer, $($arg)*);
        $crate::serial::write_str("\n");
    }};
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
