//! Framebuffer text console for the AIOS kernel.
//!
//! This is a small, allocation-free replacement for the old VGA text-mode
//! console. Glyphs come from the public-domain `font8x8` bitmap font and are
//! rendered directly into the linear framebuffer owned by the kernel, so the
//! console works identically under UEFI GOP and legacy VBE.
//!
//! The public surface (`print`, `write_bytes`, the `vprintln!` macro) mirrors
//! the previous `vga` module, so call sites elsewhere in the kernel are
//! unchanged.

use crate::font8x8::BASIC;
use crate::framebuffer::{colors, Framebuffer};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use limine::framebuffer::Framebuffer as LimineFramebuffer;

/// Integer render scale applied to each 8x8 glyph.
const SCALE: usize = 2;
/// Glyph cell width in pixels.
const GLYPH_W: usize = 8 * SCALE;
/// Glyph cell height in pixels.
const GLYPH_H: usize = 8 * SCALE;

struct Console {
    fb: Option<Framebuffer>,
    cols: usize,
    rows: usize,
    cursor_x: usize,
    cursor_y: usize,
}

impl Console {
    const fn new() -> Self {
        Self {
            fb: None,
            cols: 0,
            rows: 0,
            cursor_x: 0,
            cursor_y: 0,
        }
    }

    fn init(&mut self, fb: Option<&LimineFramebuffer>) {
        self.fb = fb.map(Framebuffer::new);
        if let Some(fb) = self.fb.as_ref().filter(|fb| fb.is_usable()) {
            self.cols = fb.width() / GLYPH_W;
            // Reserve the bottom GLYPH_H row for the fixed overlay strip
            // (heartbeat square). The console never draws there, and scrolling
            // is bounded so the overlay pixels are never dragged up.
            self.rows = (fb.height() / GLYPH_H).saturating_sub(1);
        } else {
            self.cols = 0;
            self.rows = 0;
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    fn clear(&mut self) {
        if let Some(fb) = self.fb.as_ref().filter(|fb| fb.is_usable()) {
            unsafe { fb.clear(colors::BG) };
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    fn draw_glyph(&mut self, byte: u8) {
        let (Some(fb), true) = (self.fb.as_ref(), self.cols > 0) else {
            return;
        };
        let glyph = if (byte as usize) < BASIC.len() {
            BASIC[byte as usize]
        } else {
            [0u8; 8]
        };
        let px = self.cursor_x * GLYPH_W;
        let py = self.cursor_y * GLYPH_H;
        unsafe {
            fb.fill_rect(px, py, GLYPH_W, GLYPH_H, colors::BG);
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8usize {
                    if bits & (1 << col) != 0 {
                        fb.fill_rect(px + col * SCALE, py + row * SCALE, SCALE, SCALE, colors::FG);
                    }
                }
            }
        }
    }

    fn newline(&mut self) {
        self.cursor_x = 0;
        if self.cursor_y + 1 >= self.rows {
            if let Some(fb) = self.fb.as_ref().filter(|fb| fb.is_usable()) {
                unsafe { fb.scroll_up(GLYPH_H, colors::BG, self.rows * GLYPH_H) };
            }
        } else {
            self.cursor_y += 1;
        }
    }

    fn write_byte(&mut self, byte: u8) {
        if self.cols == 0 {
            return;
        }
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.cursor_x = 0,
            b'\t' => {
                let next = (self.cursor_x + 8) & !7;
                self.cursor_x = next;
                while self.cursor_x >= self.cols {
                    self.newline();
                    if self.cursor_x == 0 {
                        break;
                    }
                }
            }
            _ => {
                self.draw_glyph(byte);
                self.cursor_x += 1;
                if self.cursor_x >= self.cols {
                    self.newline();
                }
            }
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match byte {
                0x20..=0x7e | b'\n' | b'\r' | b'\t' => self.write_byte(byte),
                _ => self.write_byte(b'?'),
            }
        }
    }
}

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}

struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
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

    fn try_lock(&self) -> bool {
        !self.locked.swap(true, Ordering::Acquire)
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

static CONSOLE_LOCK: SpinLock = SpinLock::new();
static mut CONSOLE: Console = Console::new();

/// Installs the Limine framebuffer as the kernel console.
///
/// Passing `None` keeps the console inactive (all output then goes to serial
/// only), which is useful on headless systems.
pub fn init(fb: Option<&LimineFramebuffer>) {
    CONSOLE_LOCK.lock();
    let console = unsafe { &mut *core::ptr::addr_of_mut!(CONSOLE) };
    console.init(fb);
    CONSOLE_LOCK.unlock();
}

/// Clears the console and homes the cursor.
pub fn clear() {
    CONSOLE_LOCK.lock();
    let console = unsafe { &mut *core::ptr::addr_of_mut!(CONSOLE) };
    console.clear();
    CONSOLE_LOCK.unlock();
}

/// Sets the cursor position in glyph cells.
pub fn set_cursor(x: usize, y: usize) {
    CONSOLE_LOCK.lock();
    let console = unsafe { &mut *core::ptr::addr_of_mut!(CONSOLE) };
    console.cursor_x = x;
    console.cursor_y = y;
    CONSOLE_LOCK.unlock();
}

/// Whether a usable framebuffer console is active.
#[allow(dead_code)]
pub fn is_active() -> bool {
    CONSOLE_LOCK.lock();
    let active = unsafe { (&*core::ptr::addr_of!(CONSOLE)).cols > 0 };
    CONSOLE_LOCK.unlock();
    active
}

/// Returns a reference to the underlying framebuffer, if one is active.
/// Does NOT acquire CONSOLE_LOCK, so it is safe to call from interrupt context.
pub fn framebuffer() -> Option<&'static Framebuffer> {
    unsafe { (&*core::ptr::addr_of!(CONSOLE)).fb.as_ref() }
}

/// Formats and prints to the framebuffer console.
/// Uses try_lock to avoid deadlock when called from interrupt context.
pub fn print(args: fmt::Arguments) {
    use core::fmt::Write;
    if !CONSOLE_LOCK.try_lock() {
        return;
    }
    let console = unsafe { &mut *core::ptr::addr_of_mut!(CONSOLE) };
    let _ = console.write_fmt(args);
    CONSOLE_LOCK.unlock();
}

/// Writes raw bytes to the console, sanitizing non-printables to `?` (used by
/// the `SYS_WRITE` syscall so a user program cannot corrupt the console).
/// Uses try_lock to avoid deadlock when called from interrupt context.
pub fn write_bytes(bytes: &[u8]) {
    if !CONSOLE_LOCK.try_lock() {
        return;
    }
    let console = unsafe { &mut *core::ptr::addr_of_mut!(CONSOLE) };
    console.write_bytes(bytes);
    CONSOLE_LOCK.unlock();
}

/// Prints a line to the framebuffer console (companion to `kprintln!`).
#[macro_export]
macro_rules! vprintln {
    () => ($crate::console::print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::console::print(format_args!("{}\n", format_args!($($arg)*))));
}
