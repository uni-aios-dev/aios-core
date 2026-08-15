use core::fmt::{self, Write};

pub const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

pub struct SerialWriter;

pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB (set baud divisor)
        outb(COM1, 0x03); // divisor low  (115200 baud)
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8 data bits, no parity, 1 stop bit
        outb(COM1 + 2, 0xC7); // enable + clear FIFOs
        outb(COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set
    }
}

impl Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            unsafe {
                while inb(COM1 + 5) & 0x20 == 0 {}
                outb(COM1, byte);
            }
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    SerialWriter.write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! kprintln {
    () => ($crate::kprint!("\n"));
    ($($arg:tt)*) => ($crate::kprint!("{}\n", format_args!($($arg)*)));
}
