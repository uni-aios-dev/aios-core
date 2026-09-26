//! Fixed-position framebuffer dashboard (the kernel "TUI").
//!
//! Rendered at the top of the screen, above the scrolling console, from the
//! same `fill_rect`/`draw_glyph` primitives the console uses — no external UI
//! library in the boot path. The panel is rewritten once per second by
//! `idle_loop` (or the `HW-IRQ` tick path) and shows live kernel state:
//! ticks/uptime/tick mode, scheduler switches + per-task state, IPC counters
//! and mailbox occupancy, storage/USB driver status, and memory usage.

use crate::font8x8::BASIC;
use crate::framebuffer::{colors, Color, Framebuffer};
use crate::interrupts::{IRQ32_SEEN, TICKS, TIMER_HZ};
use crate::{console, sched, syscalls};
use core::sync::atomic::Ordering;

pub use alloc::format;

/// Number of glyph rows reserved for the dashboard at the top of the screen.
pub const TUI_ROWS: usize = 8;

/// Backdrop tint for the panel, visually distinct from the console background.
const PANEL_BG: Color = 0x00_20_20_48;

/// Writes one ASCII string at absolute pixel `(x, y)`, glyph by glyph from
/// the baked-in `font8x8` bitmap. Each cell is cleared to `colors::BG` first,
/// so stale previous-second digits cannot leak between refreshes.
fn draw_text(fb: &Framebuffer, x: usize, y: usize, s: &str, fg: Color) {
    unsafe {
        for (i, byte) in s.bytes().enumerate() {
            let px = x + i * console::GLYPH_W;
            let glyph = if (byte as usize) < BASIC.len() {
                BASIC[byte as usize]
            } else {
                [0u8; 8]
            };
            fb.fill_rect(px, y, console::GLYPH_W, console::GLYPH_H, colors::BG);
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8usize {
                    if bits & (1 << col) != 0 {
                        fb.fill_rect(
                            px + col * console::SCALE,
                            y + row * console::SCALE,
                            console::SCALE,
                            console::SCALE,
                            fg,
                        );
                    }
                }
            }
        }
    }
}

/// Human-readiness: `1` = online, `0` = present-but-failed, anything else =
/// no controller found.
fn driver_status(code: i32) -> &'static str {
    match code {
        -1 => "none",
        0 => "FAIL",
        _ => "OK",
    }
}

fn task_state(pid: u32) -> &'static str {
    if !sched::task_present(pid) {
        return "-";
    }
    if sched::task_asleep(pid) {
        return "S";
    }
    if sched::current_pid() == pid {
        return "R*";
    }
    "R"
}

/// Paints the dashboard from live kernel state.
///
/// Deliberately lock-free: the panel occupies a pixel region that the console
/// and heartbeat never touch, so a preemptive switch mid-paint at worst leaves
/// one 1 Hz frame partially refreshed, never corrupts a scrolling region.
pub fn render() {
    let Some(fb) = console::framebuffer() else {
        return;
    };
    let w = fb.width();
    let y0 = console::text_height();
    let gh = console::GLYPH_H;
    let gwd = console::GLYPH_W;

    let ticks = TICKS.load(Ordering::Relaxed);
    let uptime_secs = ticks / TIMER_HZ;
    let mode = if IRQ32_SEEN.load(Ordering::Relaxed) {
        "HW-IRQ"
    } else {
        "SOFT"
    };
    let (sent, recv) = syscalls::stats();
    let cur = sched::current_pid();
    let sw = sched::switch_count();
    let frames = crate::memory::frames_allocated();
    let regions = crate::memory::frame_region_count();
    let keyseq = crate::xhci::KEY_SEQ.load(Ordering::Relaxed);
    let ahci = crate::G_AHCI.load(Ordering::Relaxed);
    let nvme = crate::G_NVME.load(Ordering::Relaxed);
    let xhci = crate::G_XHCI.load(Ordering::Relaxed);

    unsafe {
        fb.fill_rect(0, y0, w, TUI_ROWS * gh, PANEL_BG);
    }

    // Row 0 — banner / clock.
    draw_text(
        fb,
        0,
        y0,
        &format!(
            "AIOS MICROKERNEL TUI  {}s ({} ticks)  clock={}  pit={}",
            uptime_secs,
            ticks,
            mode,
            crate::interrupts::pit_count()
        ),
        colors::OK,
    );
    // Row 1 — scheduler.
    draw_text(
        fb,
        0,
        y0 + gh,
        &format!(
            "sched: sw={} cur=pid{}  tasks: pid1={} pid2={} pid3={} pid4={}",
            sw,
            cur,
            task_state(1),
            task_state(2),
            task_state(3),
            task_state(4)
        ),
        colors::FG,
    );
    // Row 2 — IPC.
    draw_text(
        fb,
        0,
        y0 + 2 * gh,
        &format!(
            "ipc: sent={} recv={}  mb: 1={}/16 2={}/16 3={}/16 4={}/16",
            sent,
            recv,
            syscalls::mailbox_len(1),
            syscalls::mailbox_len(2),
            syscalls::mailbox_len(3),
            syscalls::mailbox_len(4)
        ),
        colors::FG,
    );
    // Row 3 — drivers.
    draw_text(
        fb,
        0,
        y0 + 3 * gh,
        &format!(
            "drv: ahci={} nvme={} xhci={} key-seq={}",
            driver_status(ahci),
            driver_status(nvme),
            driver_status(xhci),
            keyseq
        ),
        colors::FG,
    );
    // Row 4 — memory.
    draw_text(
        fb,
        0,
        y0 + 4 * gh,
        &format!("mem: frames={} regions={}", frames, regions),
        colors::FG,
    );
    // Row 5 — one-second progress bar (tick phase).
    let _ = gwd;
    let frac = (ticks % TIMER_HZ) as usize;
    let fill = w * frac / TIMER_HZ as usize;
    unsafe {
        fb.fill_rect(0, y0 + 5 * gh, w, gh, colors::BG);
        if fill > 0 {
            fb.fill_rect(0, y0 + 5 * gh, fill, gh, colors::STEP);
        }
    }
}
