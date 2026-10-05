//! Analytic anti-aliased vector primitives for the GUI.
//!
//! A tiny, allocation-free drawing layer on top of [`Framebuffer`]: rounded
//! rectangles, circles/ellipses, strokes and soft drop shadows, all computed
//! with integer math (no `f32`/libm, no lookup tables, no heap). Coverage is
//! evaluated per pixel only inside the one-pixel boundary band — solid
//! interiors go through the fast row fill of `Framebuffer::fill_rect` — so the
//! cost of a shape is its *perimeter*, not its area.
//!
//! Conventions: all coordinates are `i32` (rect spans `[x, x+w)`, so straight
//! edges are pixel-exact and only the curved corners are antialiased), and
//! coverage blends `0..=255` source-over. The layer is used by [`crate::gui`]
//! for window chrome, icon glyphs and the tray globe.

use crate::framebuffer::{Color, Framebuffer};

/// Rounded-rect corner mask: top-left.
pub const TL: u8 = 1;
/// Rounded-rect corner mask: top-right.
pub const TR: u8 = 2;
/// Rounded-rect corner mask: bottom-right.
pub const BR: u8 = 4;
/// Rounded-rect corner mask: bottom-left.
pub const BL: u8 = 8;
/// All four corners rounded.
pub const ALL: u8 = TL | TR | BR | BL;
/// Only the two top corners rounded (title-bar caps).
pub const TOP: u8 = TL | TR;

/// Integer floor square root over `u32` (Newton iteration, exact).
fn isqrt(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = v;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// Integer floor square root over `u64` (Newton iteration, exact).
fn isqrt64(v: u64) -> u64 {
    if v == 0 {
        return 0;
    }
    let mut x = v;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// Linear blend between two `0x00RRGGBB` colours (`t = 0` keeps `a`).
pub fn mix(a: Color, b: Color, t: u8) -> Color {
    let k = i32::from(t);
    let ch =
        |s: u32, d: u32| -> u32 { (s as i32 + ((d as i32 - s as i32) * k + 127) / 255) as u32 };
    (ch((a >> 16) & 0xff, (b >> 16) & 0xff) << 16)
        | (ch((a >> 8) & 0xff, (b >> 8) & 0xff) << 8)
        | ch(a & 0xff, b & 0xff)
}

/// Source-over blend of `color` at coverage `cov` onto the surface pixel.
/// `cov = 0` is a no-op, `cov = 255` a plain store.
///
/// # Safety
/// The framebuffer must be mapped; out-of-bounds coordinates are ignored.
pub unsafe fn blend(fb: &Framebuffer, px: i32, py: i32, color: Color, cov: u8) {
    if cov == 0 || px < 0 || py < 0 || px as usize >= fb.width() || py as usize >= fb.height() {
        return;
    }
    if cov == 255 {
        fb.put_pixel(px as usize, py as usize, color);
        return;
    }
    let dst = fb.get_color(px as usize, py as usize);
    let k = i32::from(cov);
    let ch =
        |s: u32, d: u32| -> u32 { (d as i32 + ((s as i32 - d as i32) * k + 127) / 255) as u32 };
    let out = (ch((color >> 16) & 0xff, (dst >> 16) & 0xff) << 16)
        | (ch((color >> 8) & 0xff, (dst >> 8) & 0xff) << 8)
        | ch(color & 0xff, dst & 0xff);
    fb.put_pixel(px as usize, py as usize, out);
}

/// Coverage of a pixel centre against a circle: `255` fully inside
/// `radius - 0.5`, `0` outside `radius + 0.5`, linear in between. Works in
/// doubled coordinates so the half-pixel centre (`2x+1`) stays integer.
fn circle_cov(px: i32, py: i32, cx: i32, cy: i32, r: i32) -> u8 {
    let dx = 2 * px + 1 - 2 * cx;
    let dy = 2 * py + 1 - 2 * cy;
    let d2 = dx * dx + dy * dy;
    let r2 = 2 * r;
    if d2 <= (r2 - 1) * (r2 - 1) {
        return 255;
    }
    if d2 >= (r2 + 1) * (r2 + 1) {
        return 0;
    }
    let d = isqrt(d2 as u32) as i32;
    (((r2 + 1 - d) * 255) / 2).clamp(0, 255) as u8
}

/// Coverage of a pixel centre against a rounded rectangle: straight edges are
/// pixel-exact (`255` inside the span), only pixels inside one of the active
/// `r x r` corner boxes consult the corner circle.
#[allow(clippy::too_many_arguments)]
fn rect_cov(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32, r: i32, corners: u8) -> u8 {
    if px < x || px >= x + w || py < y || py >= y + h {
        return 0;
    }
    if r <= 0 {
        return 255;
    }
    let left = px < x + r;
    let right = px >= x + w - r;
    let top = py < y + r;
    let bottom = py >= y + h - r;
    if top && left && (corners & TL) != 0 {
        circle_cov(px, py, x + r, y + r, r)
    } else if top && right && (corners & TR) != 0 {
        circle_cov(px, py, x + w - 1 - r, y + r, r)
    } else if bottom && left && (corners & BL) != 0 {
        circle_cov(px, py, x + r, y + h - 1 - r, r)
    } else if bottom && right && (corners & BR) != 0 {
        circle_cov(px, py, x + w - 1 - r, y + h - 1 - r, r)
    } else {
        255
    }
}

/// Fills a rounded rectangle with a solid colour. `r` is clamped to half the
/// shorter side; `r <= 0` degrades to a plain rectangle fill.
///
/// # Safety
/// The framebuffer must be mapped.
#[allow(clippy::too_many_arguments)]
pub unsafe fn fill_round(
    fb: &Framebuffer,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    corners: u8,
    color: Color,
) {
    fill_round_grad(fb, x, y, w, h, r, corners, color, color);
}

/// Fills a rounded rectangle with a vertical gradient from `top` (first
/// scanline) to `bottom` (last scanline); equal stops make it solid.
///
/// # Safety
/// The framebuffer must be mapped.
#[allow(clippy::too_many_arguments)]
pub unsafe fn fill_round_grad(
    fb: &Framebuffer,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    corners: u8,
    top: Color,
    bottom: Color,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = r.min((w + 1) / 2).min((h + 1) / 2);
    if r <= 0 {
        if top == bottom {
            fill_rect_i(fb, x, y, w, h, top);
        } else {
            fill_vgrad_i(fb, x, y, w, h, top, bottom);
        }
        return;
    }
    let row_color = |row: i32| -> Color {
        if top == bottom || h <= 1 {
            return top;
        }
        let k = (row - y) as i64;
        let span = (h - 1) as i64;
        let ch = |a: u32, b: u32| -> u32 {
            (a as i64 + ((b as i64 - a as i64) * k + span / 2) / span) as u32
        };
        (ch((top >> 16) & 0xff, (bottom >> 16) & 0xff) << 16)
            | (ch((top >> 8) & 0xff, (bottom >> 8) & 0xff) << 8)
            | ch(top & 0xff, bottom & 0xff)
    };
    // Middle band: full-width rows (straight edges need no coverage).
    for row in (y + r)..(y + h - r) {
        fill_rect_i(fb, x, row, w, 1, row_color(row));
    }
    // Top corner band: two corner boxes (blended) plus the straight middle run.
    for k in 0..r {
        let row = y + k;
        corner_span(fb, x, y, w, h, r, row, corners & TOP, row_color(row), true);
    }
    // Bottom corner band.
    for k in 0..r {
        let row = y + h - 1 - k;
        corner_span(
            fb,
            x,
            y,
            w,
            h,
            r,
            row,
            corners & (BL | BR),
            row_color(row),
            false,
        );
    }
}

/// Draws one scanline of a rounded rect's corner band: blends the active
/// corner boxes through [`rect_cov`] and solid-fills everything in between.
///
/// # Safety
/// The framebuffer must be mapped.
#[allow(clippy::too_many_arguments)]
unsafe fn corner_span(
    fb: &Framebuffer,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    row: i32,
    active: u8,
    color: Color,
    is_top: bool,
) {
    let (left_bit, right_bit) = if is_top { (TL, TR) } else { (BL, BR) };
    let ex = x + w;
    let left_end = x + r;
    let right_start = ex - r;
    if right_start > left_end {
        fill_rect_i(fb, left_end, row, right_start - left_end, 1, color);
    }
    for (box_start, bit) in [(x, left_bit), (right_start, right_bit)] {
        let active_here = (active & bit) != 0;
        for px in box_start..(box_start + r) {
            if active_here {
                blend(fb, px, row, color, rect_cov(px, row, x, y, w, h, r, ALL));
            } else {
                fill_rect_i(fb, px, row, 1, 1, color);
            }
        }
    }
}

/// 1px anti-aliased outline of a rounded rectangle (outer coverage minus
/// inner coverage, so the stroke straddles the boundary symmetrically).
///
/// # Safety
/// The framebuffer must be mapped.
#[allow(clippy::too_many_arguments)]
pub unsafe fn stroke_round(
    fb: &Framebuffer,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    corners: u8,
    color: Color,
) {
    if w <= 2 || h <= 2 {
        return;
    }
    let r = r.min((w + 1) / 2).min((h + 1) / 2);
    let ir = (r - 1).max(0);
    for py in y..(y + h) {
        let edge_row = py == y || py == y + h - 1;
        if edge_row {
            for px in x..(x + w) {
                let c = rect_cov(px, py, x, y, w, h, r, corners);
                if c != 0 {
                    blend(fb, px, py, color, c);
                }
            }
            continue;
        }
        let in_corner_band = py < y + r || py >= y + h - r;
        let left_end = if in_corner_band { x + r } else { x + 1 };
        let right_start = if in_corner_band { x + w - r } else { x + w - 1 };
        for px in x..left_end {
            let c = rect_cov(px, py, x, y, w, h, r, corners).saturating_sub(rect_cov(
                px,
                py,
                x + 1,
                y + 1,
                w - 2,
                h - 2,
                ir,
                corners,
            ));
            if c != 0 {
                blend(fb, px, py, color, c);
            }
        }
        for px in right_start..(x + w) {
            let c = rect_cov(px, py, x, y, w, h, r, corners).saturating_sub(rect_cov(
                px,
                py,
                x + 1,
                y + 1,
                w - 2,
                h - 2,
                ir,
                corners,
            ));
            if c != 0 {
                blend(fb, px, py, color, c);
            }
        }
    }
}

/// Normalized radial terms shared by the ellipse fill/ring: with doubled
/// pixel-centre coordinates, `s = isqrt(dx^2 * ry^2 + dy^2 * rx^2)` over
/// `den = 2 * rx * ry` equals `den` exactly on the ellipse boundary, and
/// `(den - s) * min_radius / den` is the inside distance in pixels.
fn ellipse_terms(px: i32, py: i32, cx: i32, cy: i32, rx: i32, ry: i32) -> Option<(u64, u64)> {
    if rx <= 0 || ry <= 0 {
        return None;
    }
    let dx = i64::from(2 * px + 1 - 2 * cx);
    let dy = i64::from(2 * py + 1 - 2 * cy);
    let rxi = i64::from(rx);
    let ryi = i64::from(ry);
    let num = dx * dx * ryi * ryi + dy * dy * rxi * rxi;
    let den = 2 * rxi * ryi;
    Some((isqrt64(num as u64), den as u64))
}

/// Anti-aliased filled circle (coverage band of one pixel at the rim).
///
/// # Safety
/// The framebuffer must be mapped.
pub unsafe fn fill_circle(fb: &Framebuffer, cx: i32, cy: i32, rad: i32, color: Color) {
    if rad <= 0 {
        blend(fb, cx, cy, color, 255);
        return;
    }
    let minr = u64::from(rad.unsigned_abs());
    for py in (cy - rad)..=(cy + rad) {
        for px in (cx - rad)..=(cx + rad) {
            let Some((s, den)) = ellipse_terms(px, py, cx, cy, rad, rad) else {
                return;
            };
            if s >= den {
                continue;
            }
            let cov = (((den - s) * minr * 255) / den).min(255) as u8;
            blend(fb, px, py, color, cov);
        }
    }
}

/// Anti-aliased 1px elliptical ring (a circle when `rx == ry`): meridians of
/// the tray globe and any decorative outline.
///
/// # Safety
/// The framebuffer must be mapped.
pub unsafe fn stroke_ellipse(fb: &Framebuffer, cx: i32, cy: i32, rx: i32, ry: i32, color: Color) {
    if rx <= 0 || ry <= 0 {
        return;
    }
    let minr = u64::from(rx.min(ry).unsigned_abs());
    for py in (cy - ry)..=(cy + ry) {
        for px in (cx - rx)..=(cx + rx) {
            let Some((s, den)) = ellipse_terms(px, py, cx, cy, rx, ry) else {
                return;
            };
            let diff = s.abs_diff(den);
            let band = den as i64 - (minr * diff) as i64;
            if band <= 0 {
                continue;
            }
            let cov = ((band * 255) / den as i64).min(255) as u8;
            blend(fb, px, py, color, cov);
        }
    }
}

/// Anti-aliased 1px line from `(x0, y0)` to `(x1, y1)`: coverage comes from
/// the perpendicular distance to the line, evaluated over the bounding box
/// (fine for the short icon-detail segments this is built for).
///
/// # Safety
/// The framebuffer must be mapped.
pub unsafe fn line(fb: &Framebuffer, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = isqrt((dx * dx + dy * dy) as u32) as i32;
    if len == 0 {
        blend(fb, x0, y0, color, 255);
        return;
    }
    for py in y0.min(y1)..=y0.max(y1) {
        for px in x0.min(x1)..=x0.max(x1) {
            let cross = (px - x0) * dy - (py - y0) * dx;
            let c = len - 2 * cross.abs();
            if c > 0 {
                blend(fb, px, py, color, ((c * 255) / len).clamp(0, 255) as u8);
            }
        }
    }
}

/// Soft drop shadow outside a rounded rectangle: a `spread`-pixel band whose
/// alpha falls off linearly with the signed distance to the boundary, blended
/// over whatever the desktop already shows. Pixels under the shape are never
/// touched (draw the shadow *before* the shape).
///
/// # Safety
/// The framebuffer must be mapped.
#[allow(clippy::too_many_arguments)]
pub unsafe fn shadow_round(
    fb: &Framebuffer,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    spread: i32,
    strength: u8,
    color: Color,
) {
    if w <= 0 || h <= 0 || spread <= 0 {
        return;
    }
    let r = r.min((w + 1) / 2).min((h + 1) / 2).max(0);
    let cx = 2 * x + w;
    let cy = 2 * y + h;
    let hx = (w - 2 * r).max(0);
    let hy = (h - 2 * r).max(0);
    let limit = 2 * spread;
    for py in (y - spread)..=(y + h + spread) {
        for px in (x - spread)..=(x + w + spread) {
            let qx = (2 * px + 1 - cx).abs() - hx;
            let qy = (2 * py + 1 - cy).abs() - hy;
            let outer = isqrt((qx.max(0) * qx.max(0) + qy.max(0) * qy.max(0)) as u32) as i32;
            let d = outer + qx.max(qy).min(0) - 2 * r;
            if d <= 0 || d >= limit {
                continue;
            }
            let cov = ((limit - d) as u64 * u64::from(strength) / limit as u64) as u8;
            blend(fb, px, py, color, cov);
        }
    }
}

/// Rectangle fill through `i32` coordinates with clipping (the vector layer's
/// internal counterpart of `Framebuffer::fill_rect`).
fn fill_rect_i(fb: &Framebuffer, x: i32, y: i32, w: i32, h: i32, color: Color) {
    if w <= 0 || h <= 0 {
        return;
    }
    let (fw, fh) = (fb.width() as i32, fb.height() as i32);
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(fw);
    let y1 = (y + h).min(fh);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    unsafe {
        fb.fill_rect(
            x0 as usize,
            y0 as usize,
            (x1 - x0) as usize,
            (y1 - y0) as usize,
            color,
        );
    }
}

/// Vertical gradient rectangle fill through `i32` coordinates with clipping.
fn fill_vgrad_i(fb: &Framebuffer, x: i32, y: i32, w: i32, h: i32, top: Color, bottom: Color) {
    if w <= 0 || h <= 0 {
        return;
    }
    let (fw, fh) = (fb.width() as i32, fb.height() as i32);
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(fw);
    let y1 = (y + h).min(fh);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    unsafe {
        fb.fill_rect_vgrad(
            x0 as usize,
            y0 as usize,
            (x1 - x0) as usize,
            (y1 - y0) as usize,
            top,
            bottom,
        );
    }
}
