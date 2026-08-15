use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

pub const BUFFER_HEIGHT: usize = 25;
pub const BUFFER_WIDTH: usize = 80;

const VGA_ADDR: usize = 0xB8000;
const LIGHT_GRAY_ON_BLACK: u8 = 0x07;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    #[allow(dead_code)]
    const fn new(fg: Color, bg: Color) -> Self {
        Self((bg as u8) << 4 | (fg as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: u8,
}

const BLANK: ScreenChar = ScreenChar {
    ascii_character: b' ',
    color_code: LIGHT_GRAY_ON_BLACK,
};

struct VgaWriter {
    column: usize,
    row: usize,
    color: ColorCode,
    buffer_addr: usize,
}

impl VgaWriter {
    const fn new() -> Self {
        Self {
            column: 0,
            row: 0,
            color: ColorCode(LIGHT_GRAY_ON_BLACK),
            buffer_addr: VGA_ADDR,
        }
    }

    unsafe fn buffer(&mut self) -> &'static mut [ScreenChar] {
        let ptr = self.buffer_addr as *mut ScreenChar;
        core::slice::from_raw_parts_mut(ptr, BUFFER_HEIGHT * BUFFER_WIDTH)
    }

    fn clear(&mut self) {
        for cell in unsafe { self.buffer() }.iter_mut() {
            *cell = BLANK;
        }
        self.row = 0;
        self.column = 0;
    }

    fn newline(&mut self) {
        self.column = 0;
        if self.row + 1 == BUFFER_HEIGHT {
            for r in 1..BUFFER_HEIGHT {
                for c in 0..BUFFER_WIDTH {
                    let idx = r * BUFFER_WIDTH + c;
                    let prev = (r - 1) * BUFFER_WIDTH + c;
                    unsafe { self.buffer()[idx] = self.buffer()[prev] };
                }
            }
            for c in 0..BUFFER_WIDTH {
                unsafe { self.buffer()[(BUFFER_HEIGHT - 1) * BUFFER_WIDTH + c] = BLANK };
            }
        } else {
            self.row += 1;
        }
    }

    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            b'\t' => {
                self.column = (self.column + 8) & !7;
                if self.column >= BUFFER_WIDTH {
                    self.newline();
                }
            }
            byte => {
                let idx = self.row * BUFFER_WIDTH + self.column;
                unsafe {
                    self.buffer()[idx] = ScreenChar {
                        ascii_character: byte,
                        color_code: self.color.0,
                    }
                };
                self.column += 1;
                if self.column >= BUFFER_WIDTH {
                    self.newline();
                }
            }
        }
    }

    fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7E | b'\n' | b'\t' => self.write_byte(byte),
                _ => self.write_byte(0xFE),
            }
        }
    }
}

impl Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

struct SpinMutex {
    locked: AtomicBool,
}

impl SpinMutex {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock(&self) {
        while self.locked.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

static VGA_LOCK: SpinMutex = SpinMutex::new();
static mut VGA_WRITER: VgaWriter = VgaWriter::new();

pub fn vga_print(args: fmt::Arguments) {
    use core::fmt::Write;
    VGA_LOCK.lock();
    let w = unsafe { &mut *core::ptr::addr_of_mut!(VGA_WRITER) };
    let _ = w.write_fmt(args);
    VGA_LOCK.unlock();
}

pub fn vga_init(offset: u64) {
    VGA_LOCK.lock();
    let w = unsafe { &mut *core::ptr::addr_of_mut!(VGA_WRITER) };
    w.buffer_addr = VGA_ADDR + offset as usize;
    VGA_LOCK.unlock();
}

pub fn vga_clear_screen() {
    VGA_LOCK.lock();
    let w = unsafe { &mut *core::ptr::addr_of_mut!(VGA_WRITER) };
    w.clear();
    VGA_LOCK.unlock();
}

#[macro_export]
macro_rules! vprintln {
    () => ($crate::vga::vga_print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::vga::vga_print(format_args!("{}\n", format_args!($($arg)*))));
}
