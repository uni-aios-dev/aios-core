use crate::port;
use core::fmt::{self, Write};

pub const COM1: u16 = 0x3F8;

pub struct SerialWriter;

pub fn init() {
    unsafe {
        port::outb(COM1 + 1, 0x00);
        port::outb(COM1 + 3, 0x80);
        port::outb(COM1, 0x03);
        port::outb(COM1 + 1, 0x00);
        port::outb(COM1 + 3, 0x03);
        port::outb(COM1 + 2, 0xC7);
        port::outb(COM1 + 4, 0x0B);
    }
}

impl Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            unsafe {
                while port::inb(COM1 + 5) & 0x20 == 0 {}
                port::outb(COM1, byte);
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
