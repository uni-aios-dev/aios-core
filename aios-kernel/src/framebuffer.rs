//! Linear framebuffer drawing primitives for the AIOS kernel.
//!
//! The framebuffer is handed to the kernel by Limine via the
//! `FramebufferRequest` (see `linker.ld` and `main.rs`). Under UEFI this is the
//! GOP linear framebuffer; under legacy BIOS it is the VBE linear framebuffer.
//! The kernel owns the pixels directly — there is no `fbcon`, no DRM and no
//! firmware blob anywhere in the path.
//!
//! Colours passed to this module use the conventional `0x00RRGGBB` layout and
//! are converted to the hardware's pixel format using the packed masks reported
//! by Limine, so the same drawing code works on 32/24/16-bit RGB framebuffers.

use limine::framebuffer::Framebuffer as LimineFramebuffer;

/// A `0x00RRGGBB` colour.
pub type Color = u32;

/// Convenience palette used by the kernel console and diagnostics.
pub mod colors {
    /// A `0x00RRGGBB` colour.
    pub type Color = super::Color;

    /// Console background.
    pub const BG: Color = 0x00_10_10_20;
    /// Console foreground.
    pub const FG: Color = 0x00_d0_d0_e0;
    /// Success green (used by the boot self-check).
    pub const OK: Color = 0x00_5a_d6_7a;
}

/// A linear framebuffer with helper drawing primitives.
///
/// This is a thin, allocation-free wrapper over the raw Limine framebuffer. It
/// is deliberately `Copy`-free and stored once (inside the console), so the raw
/// pointer is never duplicated.
pub struct Framebuffer {
    base: *mut u8,
    width: usize,
    height: usize,
    pitch: usize,
    bytes_per_pixel: usize,
    red_shift: u8,
    red_size: u8,
    green_shift: u8,
    green_size: u8,
    blue_shift: u8,
    blue_size: u8,
}

// The kernel is single-threaded while drawing (a spin lock guards the console);
// the raw pointer is only ever used by the owning core.
unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

/// Scales an 8-bit channel down to a channel of `size` bits.
fn scale(value: u32, size: u8) -> u32 {
    match size {
        0 => 0,
        s if s >= 8 => value & 0xff,
        s => value >> (8 - s),
    }
}

impl Framebuffer {
    /// Builds a framebuffer handle from Limine's description.
    ///
    /// Only `bpp` of 16, 24 or 32 are supported; anything else is reported to
    /// the caller via [`Self::is_usable`].
    pub fn new(fb: &LimineFramebuffer) -> Self {
        Self {
            base: fb.address() as *mut u8,
            width: fb.width as usize,
            height: fb.height as usize,
            pitch: fb.pitch as usize,
            bytes_per_pixel: usize::from(fb.bpp) / 8,
            red_shift: fb.red_mask_shift,
            red_size: fb.red_mask_size,
            green_shift: fb.green_mask_shift,
            green_size: fb.green_mask_size,
            blue_shift: fb.blue_mask_shift,
            blue_size: fb.blue_mask_size,
        }
    }

    /// Whether the pixel depth is one this renderer knows how to drive.
    pub fn is_usable(&self) -> bool {
        matches!(self.bytes_per_pixel, 2..=4)
    }

    /// Framebuffer width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Framebuffer height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Bytes per scanline.
    pub fn pitch(&self) -> usize {
        self.pitch
    }

    /// Bytes per pixel (2, 3 or 4).
    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    /// Packs a `0x00RRGGBB` colour into the hardware pixel format.
    /// Public variant used by the boot self-check.
    pub fn pack_color(&self, color: Color) -> u32 {
        self.pack(color)
    }

    /// Packs a `0x00RRGGBB` colour into the hardware pixel format.
    fn pack(&self, color: Color) -> u32 {
        let r = (color >> 16) & 0xff;
        let g = (color >> 8) & 0xff;
        let b = color & 0xff;
        (scale(r, self.red_size) << self.red_shift)
            | (scale(g, self.green_size) << self.green_shift)
            | (scale(b, self.blue_size) << self.blue_shift)
    }

    /// Writes a single pixel. Out-of-bounds coordinates are ignored.
    ///
    /// # Safety
    /// The framebuffer must still be mapped (it is, for the whole kernel run).
    pub unsafe fn put_pixel(&self, x: usize, y: usize, color: Color) {
        if x >= self.width || y >= self.height || !self.is_usable() {
            return;
        }
        let ptr = self.base.add(y * self.pitch + x * self.bytes_per_pixel);
        let packed = self.pack(color);
        match self.bytes_per_pixel {
            4 => core::ptr::write_unaligned(ptr as *mut u32, packed),
            3 => {
                core::ptr::write_unaligned(ptr, (packed & 0xff) as u8);
                core::ptr::write_unaligned(ptr.add(1), ((packed >> 8) & 0xff) as u8);
                core::ptr::write_unaligned(ptr.add(2), ((packed >> 16) & 0xff) as u8);
            }
            2 => core::ptr::write_unaligned(ptr as *mut u16, packed as u16),
            _ => {}
        }
    }

    /// Reads back a pixel in hardware format (used by the boot self-check).
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn read_pixel(&self, x: usize, y: usize) -> u32 {
        if x >= self.width || y >= self.height || !self.is_usable() {
            return 0;
        }
        let ptr = self.base.add(y * self.pitch + x * self.bytes_per_pixel);
        match self.bytes_per_pixel {
            4 => core::ptr::read_unaligned(ptr as *const u32),
            3 => {
                u32::from(core::ptr::read_unaligned(ptr))
                    | (u32::from(core::ptr::read_unaligned(ptr.add(1))) << 8)
                    | (u32::from(core::ptr::read_unaligned(ptr.add(2))) << 16)
            }
            2 => u32::from(core::ptr::read_unaligned(ptr as *const u16)),
            _ => 0,
        }
    }

    /// Fills an axis-aligned rectangle, clipped to the framebuffer bounds.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        if !self.is_usable() {
            return;
        }
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        for row in y..y1 {
            for col in x..x1 {
                self.put_pixel(col, row, color);
            }
        }
    }

    /// Clears the whole framebuffer to a single colour.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn clear(&self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Scrolls the contents up by `pixels` scanlines and blanks the freed rows.
    ///
    /// Implemented with `ptr::copy` (overlap-safe) so it costs a single memmove
    /// per frame instead of a per-pixel redraw.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn scroll_up(&self, pixels: usize, fill: Color) {
        if !self.is_usable() || pixels == 0 || pixels >= self.height {
            return;
        }
        let move_bytes = (self.height - pixels) * self.pitch;
        core::ptr::copy(self.base.add(pixels * self.pitch), self.base, move_bytes);
        let packed = self.pack(fill);
        for row in (self.height - pixels)..self.height {
            let line = self.base.add(row * self.pitch);
            match self.bytes_per_pixel {
                4 => {
                    let p = line as *mut u32;
                    for col in 0..self.width {
                        core::ptr::write_unaligned(p.add(col), packed);
                    }
                }
                3 => {
                    for col in 0..self.width {
                        let p = line.add(col * 3);
                        core::ptr::write_unaligned(p, (packed & 0xff) as u8);
                        core::ptr::write_unaligned(p.add(1), ((packed >> 8) & 0xff) as u8);
                        core::ptr::write_unaligned(p.add(2), ((packed >> 16) & 0xff) as u8);
                    }
                }
                2 => {
                    let p = line as *mut u16;
                    for col in 0..self.width {
                        core::ptr::write_unaligned(p.add(col), packed as u16);
                    }
                }
                _ => {}
            }
        }
    }
}
