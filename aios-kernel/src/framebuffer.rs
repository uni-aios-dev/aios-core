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
    /// Boot-progress squares (bottom-left step counter, drawn lock-free).
    pub const STEP: Color = 0x00_ff_a0_50;
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

    /// Virtual (HHDM) address of the framebuffer base.
    ///
    /// Limine already adds the higher-half direct map offset to the physical
    /// framebuffer address before handing it over (`address + hhdm_offset`),
    /// so the value is directly usable as a raw pointer — no extra translation
    /// is needed and the offset must NOT be added a second time.
    pub fn base_addr(&self) -> u64 {
        self.base as u64
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

    /// Builds a dense RAM surface (software backbuffer) with the same pixel
    /// format as `model`: identical `bytes_per_pixel`, channel shifts/sizes and
    /// packed layout, but `pitch == width * bytes_per_pixel` (no padding), so
    /// every pixel stores exactly the packed bytes that [`Self::blit_region`]
    /// copies into a physical framebuffer.
    ///
    /// All drawing primitives work on the returned handle unchanged, which lets
    /// the GUI render a whole frame off-screen and then publish it to VRAM in
    /// one atomic push per dirty row.
    ///
    /// # Safety
    /// `base` must point to at least `width * height * bytes_per_pixel`
    /// writable bytes, aligned like the hardware format, for the lifetime of
    /// the returned handle.
    pub unsafe fn from_ram(
        base: *mut u8,
        width: usize,
        height: usize,
        model: &Framebuffer,
    ) -> Self {
        let bpp = model.bytes_per_pixel.max(1);
        Self {
            base,
            width,
            height,
            pitch: width.saturating_mul(bpp),
            bytes_per_pixel: bpp,
            red_shift: model.red_shift,
            red_size: model.red_size,
            green_shift: model.green_shift,
            green_size: model.green_size,
            blue_shift: model.blue_shift,
            blue_size: model.blue_size,
        }
    }

    /// Atomically publishes rectangle `(x0, y0)..(x1, y1)` from the dense
    /// software surface `src` into `self` (VRAM), copying raw packed bytes one
    /// scanline at a time so each row is written with a single
    /// `copy_nonoverlapping` and the scanout buffer is never painted
    /// incrementally (no torn rows, no flicker from partial draws).
    ///
    /// Both surfaces must use the same `bytes_per_pixel` (by construction for
    /// [`Self::from_ram`]); the walk honours each surface's own pitch.
    ///
    /// # Safety
    /// `src` and `self` must be writable for the touched ranges.
    pub unsafe fn blit_region(
        &self,
        src: &Framebuffer,
        x0: usize,
        y0: usize,
        mut x1: usize,
        mut y1: usize,
    ) {
        if !self.is_usable() || !src.is_usable() || self.bytes_per_pixel != src.bytes_per_pixel {
            return;
        }
        x1 = x1.min(self.width).min(src.width);
        y1 = y1.min(self.height).min(src.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let bpp = self.bytes_per_pixel;
        let bytes = (x1 - x0) * bpp;
        for row in y0..y1 {
            let dst = self.base.add(row * self.pitch + x0 * bpp);
            let src_row = src.base.add(row * src.pitch + x0 * bpp);
            core::ptr::copy_nonoverlapping(src_row, dst, bytes);
        }
    }

    /// Copies a `w x h` rectangle from `(sx, sy)` in the dense source surface
    /// into `self` at `(dx, dy)`, honouring each surface's own pitch with one
    /// `copy_nonoverlapping` per scanline. Used to composite a ring-3 client
    /// framebuffer (its own dense RAM surface) into the GUI backbuffer at an
    /// arbitrary window offset — the reader never has to pre-scroll the client
    /// buffer to align with the destination.
    ///
    /// # Safety
    /// `src` and `self` must be readable/writable for the touched ranges.
    pub unsafe fn blit_at(
        &self,
        src: &Framebuffer,
        src_at: (usize, usize),
        dst_at: (usize, usize),
        size: (usize, usize),
    ) {
        let (sx, sy) = src_at;
        let (dx, dy) = dst_at;
        let (w, h) = size;
        if !self.is_usable() || !src.is_usable() || self.bytes_per_pixel != src.bytes_per_pixel {
            return;
        }
        if sx >= src.width || sy >= src.height || dx >= self.width || dy >= self.height {
            return;
        }
        let w = w.min(src.width - sx).min(self.width - dx);
        let h = h.min(src.height - sy).min(self.height - dy);
        if w == 0 || h == 0 {
            return;
        }
        let bpp = self.bytes_per_pixel;
        let bytes = w * bpp;
        for row in 0..h {
            let dst = self.base.add((dy + row) * self.pitch + dx * bpp);
            let src_row = src.base.add((sy + row) * src.pitch + sx * bpp);
            core::ptr::copy_nonoverlapping(src_row, dst, bytes);
        }
    }

    /// Nearest-neighbour scale blit: samples the `src_size` rectangle starting
    /// at `src_at` of `src` and draws it as `dst_size` pixels at `dst_at` of
    /// `self` (raw packed pixels, same format contract as [`Self::blit_at`]),
    /// honouring both surfaces' pitches and clipping on both sides. Used to
    /// composite a ring-3 client buffer into a window the user has resized —
    /// the buffer keeps its native resolution while the window scales.
    ///
    /// # Safety
    /// `src` and `self` must be readable/writable for the touched ranges.
    pub unsafe fn blit_scaled(
        &self,
        src: &Framebuffer,
        src_at: (usize, usize),
        dst_at: (usize, usize),
        src_size: (usize, usize),
        dst_size: (usize, usize),
    ) {
        let (sx0, sy0) = src_at;
        let (dx0, dy0) = dst_at;
        let (sw, sh) = src_size;
        let (dw, dh) = dst_size;
        if !self.is_usable() || !src.is_usable() || self.bytes_per_pixel != src.bytes_per_pixel {
            return;
        }
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return;
        }
        let bpp = self.bytes_per_pixel;
        for row in 0..dh {
            let sy = sy0 + row * sh / dh;
            if sy >= src.height {
                break;
            }
            let dy = dy0 + row;
            if dy >= self.height {
                break;
            }
            let dst_row = dy * self.pitch;
            let src_row = sy * src.pitch;
            for col in 0..dw {
                let sx = sx0 + col * sw / dw;
                if sx >= src.width {
                    break;
                }
                let dx = dx0 + col;
                if dx >= self.width {
                    break;
                }
                let s = src.base.add(src_row + sx * bpp);
                let d = self.base.add(dst_row + dx * bpp);
                match bpp {
                    4 => {
                        let px = core::ptr::read_unaligned(s as *const u32);
                        core::ptr::write_unaligned(d as *mut u32, px);
                    }
                    n => core::ptr::copy_nonoverlapping(s, d, n),
                }
            }
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
    /// Implemented as one pack plus a packed-value write per scanline (no
    /// per-pixel bounds/branch checks), which is the workhorse behind every
    /// GUI repaint: a full 1280x800 clear costs one tight store loop per row
    /// instead of a million `put_pixel` calls.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        if !self.is_usable() || w == 0 || h == 0 {
            return;
        }
        let x0 = x.min(self.width);
        let x1 = (x + w).min(self.width);
        let y0 = y.min(self.height);
        let y1 = (y + h).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let packed = self.pack(color);
        for row in y0..y1 {
            self.write_row(row, x0, x1, packed);
        }
    }

    /// Fills an axis-aligned rectangle with a vertical two-stop gradient:
    /// `top` at the rectangle's first scanline, `bottom` at its last
    /// (linear per-row interpolation of the three `0x00RRGGBB` channels).
    /// Clipped exactly like [`Self::fill_rect`].
    ///
    /// Cost equals [`Self::fill_rect`] plus one channel lerp per row — never
    /// per pixel — so it is safe for large desktop repaints.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn fill_rect_vgrad(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        top: Color,
        bottom: Color,
    ) {
        if !self.is_usable() || w == 0 || h == 0 {
            return;
        }
        let x0 = x.min(self.width);
        let x1 = (x + w).min(self.width);
        let y0 = y.min(self.height);
        let y1 = (y + h).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let span = (y1 - y0).saturating_sub(1) as i32;
        let tr = ((top >> 16) & 0xff) as i32;
        let tg = ((top >> 8) & 0xff) as i32;
        let tb = (top & 0xff) as i32;
        let br = ((bottom >> 16) & 0xff) as i32;
        let bg = ((bottom >> 8) & 0xff) as i32;
        let bb = (bottom & 0xff) as i32;
        for row in y0..y1 {
            let color = if span == 0 {
                top
            } else {
                let k = (row - y0) as i32;
                let mix = |t: i32, b: i32| -> u32 {
                    let v = t + (((b - t) * k + span / 2) / span);
                    v as u32
                };
                (mix(tr, br) << 16) | (mix(tg, bg) << 8) | mix(tb, bb)
            };
            let packed = self.pack(color);
            self.write_row(row, x0, x1, packed);
        }
    }

    /// Reads a pixel back as `0x00RRGGBB` (inverse of [`Self::pack`]), used
    /// by the vector layer to alpha-blend over what is already on the surface.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn get_color(&self, x: usize, y: usize) -> Color {
        if x >= self.width || y >= self.height || !self.is_usable() {
            return 0;
        }
        let packed = self.read_pixel(x, y);
        let unscale = |v: u32, shift: u8, size: u8| -> u32 {
            if size == 0 {
                return 0;
            }
            let masked = (v >> shift) & ((1u32 << size.min(31)) - 1);
            if size >= 8 {
                masked & 0xff
            } else if size >= 4 {
                (masked << (8 - size)) | (masked >> (2 * size - 8))
            } else {
                masked << (8 - size)
            }
        };
        (unscale(packed, self.red_shift, self.red_size) << 16)
            | (unscale(packed, self.green_shift, self.green_size) << 8)
            | unscale(packed, self.blue_shift, self.blue_size)
    }

    /// Writes `[x0, x1)` of scanline `y` with an already-packed hardware
    /// pixel value: one tight store loop per scanline, no bounds checks
    /// (the caller clips). `self.is_usable()` must hold.
    ///
    /// # Safety
    /// The framebuffer must be mapped and `(x0..x1, y)` inside its bounds.
    unsafe fn write_row(&self, y: usize, x0: usize, x1: usize, packed: u32) {
        let line = self.base.add(y * self.pitch + x0 * self.bytes_per_pixel);
        let count = x1 - x0;
        match self.bytes_per_pixel {
            4 => {
                let p = line as *mut u32;
                for i in 0..count {
                    core::ptr::write_unaligned(p.add(i), packed);
                }
            }
            3 => {
                let b0 = (packed & 0xff) as u8;
                let b1 = ((packed >> 8) & 0xff) as u8;
                let b2 = ((packed >> 16) & 0xff) as u8;
                for i in 0..count {
                    let p = line.add(i * 3);
                    core::ptr::write_unaligned(p, b0);
                    core::ptr::write_unaligned(p.add(1), b1);
                    core::ptr::write_unaligned(p.add(2), b2);
                }
            }
            2 => {
                let p = line as *mut u16;
                let v = packed as u16;
                for i in 0..count {
                    core::ptr::write_unaligned(p.add(i), v);
                }
            }
            _ => {}
        }
    }

    /// Clears the whole framebuffer to a single colour.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn clear(&self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// One-shot full-panel solid fill that proves the entire GOP surface is
    /// directly writable from raw pixels.
    ///
    /// Unlike [`Self::fill_rect`] / [`Self::clear`], every pixel is written
    /// with `core::ptr::write_volatile` and addressing honours the physical
    /// stride (`pixels per scanline = pitch / bytes_per_pixel`), so the
    /// compiler cannot elide the "useless" stores to the raw surface and the
    /// scanline walk matches the hardware layout even when
    /// `pitch != width * bytes_per_pixel`.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn direct_test(&self, color: Color) {
        if !self.is_usable() {
            return;
        }
        let packed = self.pack(color);
        let stride = self.pitch / self.bytes_per_pixel;
        match self.bytes_per_pixel {
            4 => {
                let base = self.base as *mut u32;
                for y in 0..self.height {
                    for x in 0..self.width {
                        core::ptr::write_volatile(base.add(y * stride + x), packed);
                    }
                }
            }
            3 => {
                for y in 0..self.height {
                    for x in 0..self.width {
                        let p = self.base.add((y * stride + x) * 3);
                        core::ptr::write_volatile(p, (packed & 0xff) as u8);
                        core::ptr::write_volatile(p.add(1), ((packed >> 8) & 0xff) as u8);
                        core::ptr::write_volatile(p.add(2), ((packed >> 16) & 0xff) as u8);
                    }
                }
            }
            2 => {
                let base = self.base as *mut u16;
                for y in 0..self.height {
                    for x in 0..self.width {
                        core::ptr::write_volatile(base.add(y * stride + x), packed as u16);
                    }
                }
            }
            _ => {}
        }
    }

    /// Scrolls the top `region_height` scanlines up by `pixels` rows and blanks
    /// the freed bottom rows. The region below `region_height` (the reserved
    /// overlay strip, e.g. the heartbeat square) is never touched, so scroll
    /// cannot drag copies of the overlay up the screen.
    ///
    /// Implemented with `ptr::copy` (overlap-safe) so it costs a single memmove
    /// per frame instead of a per-pixel redraw.
    ///
    /// # Safety
    /// The framebuffer must still be mapped.
    pub unsafe fn scroll_up(&self, pixels: usize, fill: Color, region_height: usize) {
        if !self.is_usable() || pixels == 0 || pixels >= region_height {
            return;
        }
        let region = region_height.min(self.height);
        let move_bytes = (region - pixels) * self.pitch;
        core::ptr::copy(self.base.add(pixels * self.pitch), self.base, move_bytes);
        let packed = self.pack(fill);
        for row in (region - pixels)..region {
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
