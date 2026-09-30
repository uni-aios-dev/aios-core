//! Interactive framebuffer TUI for the AIOS kernel.
//!
//! The TUI is the primary screen: a compact boot-log strip (a few rows) sits at
//! the top and the interactive panel fills every row below it down to the
//! bottom status strip, so the whole tab content is always visible. Seven tabs
//! mirror the host AIOS TUI (System / Sched /
//! USB / IPC / Storage / Shell / About). Keys and the USB mouse drive the panel
//! directly: `1`-`7` (outside the Shell tab) or a left-click on a tab switches
//! tabs; the Shell tab collects text input (`Enter` runs the command,
//! `Backspace` edits, `Esc` clears). The 8x8 mouse arrow is redrawn on top of
//! every repaint so a panel refresh can never erase it. Text is rendered on the
//! PSF path: the embedded PSF2 stream (synthesized from the `font8x8` bitmap at
//! first use) is parsed back through [`crate::psf`] and used as the glyph
//! source, with the `font8x8` table kept as the fallback. The panel never
//! writes the rightmost 16 px (owned by the boot tick bar) or the reserved
//! bottom strip (status bar + heartbeat).

use crate::font8x8::BASIC;
use crate::framebuffer::{colors, Color, Framebuffer};
use crate::interrupts::{IRQ32_SEEN, LAST_SCANCODE, TICKS, TIMER_HZ};
use crate::{console, psf, sched, syscalls};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Number of glyph rows the interactive panel owns. The TUI is the primary
/// screen: a compact `CONSOLE_TOP_ROWS` boot-log strip sits on top and every
/// row below it (down to the bottom status strip) belongs to the panel, so the
/// full tab contents are always visible regardless of the framebuffer height.
pub fn panel_rows() -> usize {
    let total = console::framebuffer()
        .map(|fb| fb.height() / console::GLYPH_H)
        .unwrap_or(0);
    total.saturating_sub(1 + console::rows()).max(4)
}

/// Version banner shown on the About tab, the status bar, the `ver` shell
/// command and the GUI About window.
pub(crate) const VERSION: &str = "AIOS kernel v2.38.28";

/// Tab labels, mirroring the host AIOS TUI numbering (tabs 1..=7).
const TABS: [&str; 7] = ["System", "Sched", "USB", "IPC", "Storage", "Shell", "About"];

const TAB_SHELL: usize = 5;

const PANEL_BG: Color = 0x00_18_1c_38;
const PANEL_SEL: Color = 0x00_3a_6e_ea;
const STATUS_BG: Color = 0x00_0c_10_24;
const TEXT_DIM: Color = 0x00_80_88_a0;
const ACCENT: Color = colors::OK;

static ACTIVE_TAB: AtomicU64 = AtomicU64::new(0);

static PSF_READY: AtomicBool = AtomicBool::new(false);
static mut PSF_BUF: [u8; 2048] = [0; 2048];
static mut PSF_FONT: Option<psf::PsfFont<'static>> = None;

/// Ensures the embedded PSF2 stream is parsed once; the parsed glyphs are then
/// used by every panel draw. Idempotent and cheap after the first call.
fn ensure_font() {
    if PSF_READY.load(Ordering::Relaxed) {
        return;
    }
    let font = unsafe { &mut *core::ptr::addr_of_mut!(PSF_FONT) };
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(PSF_BUF) };
    if let Some(len) = psf::synth_psf2_basic(buf) {
        let data: &'static [u8] = unsafe { core::slice::from_raw_parts(buf.as_ptr(), len) };
        if let Some(f) = psf::PsfFont::parse(data) {
            *font = Some(f);
        }
    }
    PSF_READY.store(true, Ordering::Relaxed);
}

/// Glyph bitmap for one byte (8 rows, MSB-first), preferring the parsed PSF
/// glyph over the raw `font8x8` table.
pub(crate) fn glyph_bits(byte: u8) -> [u8; 8] {
    let mut out = [0u8; 8];
    let idx = byte as usize;
    if let Some(f) = unsafe { &*core::ptr::addr_of!(PSF_FONT) } {
        if f.height() == 8 {
            if let Some(g) = f.glyph(idx) {
                for (dst, src) in out.iter_mut().zip(g.iter()) {
                    *dst = *src;
                }
                return out;
            }
        }
    }
    if idx < BASIC.len() {
        out.copy_from_slice(&BASIC[idx]);
    }
    out
}

/// Paints one glyph cell at pixel `(px, py)`, scaling the 8x8 bitmap by the
/// console `SCALE`.
pub(crate) fn draw_glyph(fb: &Framebuffer, px: usize, py: usize, byte: u8, fg: Color, bg: Color) {
    unsafe {
        fb.fill_rect(px, py, console::GLYPH_W, console::GLYPH_H, bg);
    }
    let bits = glyph_bits(byte);
    for (row, bits) in bits.iter().enumerate() {
        for col in 0..8usize {
            if bits & (1 << col) != 0 {
                unsafe {
                    fb.fill_rect(
                        px + col * console::SCALE,
                        py + row * console::SCALE,
                        console::SCALE,
                        console::SCALE,
                        fg,
                    );
                }
            }
        }
    }
}

/// Draws an ASCII string glyph by glyph, clearing each cell to `bg` first so
/// stale previous-frame digits cannot leak. `max_px` bounds the right edge so
/// the string cannot reach the tick bar's column.
pub(crate) fn draw_text(
    fb: &Framebuffer,
    x: usize,
    y: usize,
    s: &str,
    fg: Color,
    bg: Color,
    max_px: usize,
) {
    let mut px = x;
    for byte in s.bytes() {
        if px + console::GLYPH_W > max_px {
            break;
        }
        draw_glyph(fb, px, y, byte, fg, bg);
        px += console::GLYPH_W;
    }
}

struct Shell {
    history: Vec<String>,
    input: String,
}

const SHELL_CAP: usize = 128;

static mut SHELL: Shell = Shell {
    history: Vec::new(),
    input: String::new(),
};

fn shell_push(line: &str) {
    let s = unsafe { &mut *core::ptr::addr_of_mut!(SHELL) };
    if s.history.len() >= SHELL_CAP {
        s.history.remove(0);
    }
    s.history.push(line.to_string());
}

fn shell_exec() {
    let s = unsafe { &mut *core::ptr::addr_of_mut!(SHELL) };
    let cmd = core::mem::take(&mut s.input);
    let trimmed = cmd.trim();
    if !trimmed.is_empty() {
        shell_push(&format!("> {}", trimmed));
        for line in run_command(trimmed) {
            shell_push(&line);
        }
    }
}

fn run_command(cmd: &str) -> Vec<String> {
    match cmd {
        "help" => vec![
            "commands:".to_string(),
            "  help       this list".to_string(),
            "  tabs       tab overview".to_string(),
            "  info       kernel telemetry".to_string(),
            "  ver        version banner".to_string(),
            "  clear      clear log".to_string(),
            "  echo TEXT  echo text".to_string(),
            "  gui        enter windowed GUI mode".to_string(),
            "  tui        leave GUI, return to this console".to_string(),
        ],
        "tabs" => vec![
            "1 System  2 Sched  3 USB  4 IPC".to_string(),
            "5 Storage  6 Shell  7 About".to_string(),
        ],
        "info" => {
            let (sent, recv) = syscalls::stats();
            vec![
                "ticks/switches/ipc are live on the System, Sched and IPC tabs".to_string(),
                format!(
                    "pid={} switches={} ipc-sent={} ipc-recv={}",
                    sched::current_pid(),
                    sched::switch_count(),
                    sent,
                    recv
                ),
            ]
        }
        "ver" => vec![VERSION.to_string()],
        "clear" => Vec::new(),
        "gui" => {
            crate::gui::enter();
            vec![
                "entering windowed GUI (desktop + windows)".to_string(),
                "Esc with no window focused returns to this console".to_string(),
            ]
        }
        "tui" => {
            crate::gui::leave();
            vec!["returned to the console/TUI".to_string()]
        }
        _ => {
            if cmd == "echo" {
                vec![String::new()]
            } else if let Some(text) = cmd.strip_prefix("echo ") {
                vec![text.to_string()]
            } else {
                vec!["unknown command, type help".to_string()]
            }
        }
    }
}

/// Feeds one make-code to the interactive TUI. Returns `true` when the TUI
/// consumed the key (no console echo needed).
pub fn handle_scancode(sc: u8) -> bool {
    let tab = ACTIVE_TAB.load(Ordering::Relaxed) as usize;
    if tab == TAB_SHELL {
        match sc {
            0x1C => {
                shell_exec();
                return true;
            }
            0x0E => {
                let s = unsafe { &mut *core::ptr::addr_of_mut!(SHELL) };
                s.input.pop();
                return true;
            }
            0x01 => {
                let s = unsafe { &mut *core::ptr::addr_of_mut!(SHELL) };
                s.input.clear();
                return true;
            }
            _ => {}
        }
        if let Some(c) = crate::interrupts::scancode_to_char(sc) {
            if !c.is_control() {
                let s = unsafe { &mut *core::ptr::addr_of_mut!(SHELL) };
                s.input.push(c);
                return true;
            }
        }
        return false;
    }
    if let Some(c) = crate::interrupts::scancode_to_char(sc) {
        if let Some(n) = c.to_digit(10) {
            if (1..=7).contains(&n) {
                ACTIVE_TAB.store(u64::from(n - 1), Ordering::Relaxed);
                return true;
            }
        }
    }
    false
}

const MOUSE_PIXELS: [u8; 8] = [0x80, 0xC0, 0xE0, 0xF0, 0xF8, 0xE8, 0xC8, 0x8C];

static mut CURSOR_X: usize = 0;
static mut CURSOR_Y: usize = 0;
static mut CURSOR_X0: usize = 0;
static mut CURSOR_Y0: usize = 0;
static mut CURSOR_INIT: bool = false;
static mut PREV_BTNS: u8 = 0;

pub(crate) fn paint_cursor(fb: &Framebuffer, x: usize, y: usize, on: bool) {
    let color = if on { colors::FG } else { colors::BG };
    for (row, mask) in MOUSE_PIXELS.iter().enumerate() {
        for col in 0..8usize {
            if mask & (0x80 >> col) != 0 {
                unsafe {
                    fb.put_pixel(x + col, y + row, if on { color } else { colors::BG });
                }
            }
        }
    }
}

/// Applies a boot-mouse report: moves the arrow (clamped to the framebuffer),
/// repaints it and, on a left-click landing in the tab row, switches tabs.
pub fn on_mouse(dx: i32, dy: i32, buttons: u8) {
    let Some(fb) = console::framebuffer() else {
        return;
    };
    unsafe {
        let px = core::ptr::addr_of_mut!(CURSOR_X);
        let py = core::ptr::addr_of_mut!(CURSOR_Y);
        let init = core::ptr::addr_of_mut!(CURSOR_INIT);
        if !*init {
            *px = fb.width() / 2;
            *py = fb.height() / 4;
            *core::ptr::addr_of_mut!(CURSOR_X0) = *px;
            *core::ptr::addr_of_mut!(CURSOR_Y0) = *py;
            *init = true;
        }
        let max_x = (fb.width() as i32).saturating_sub(8);
        let max_y = (fb.height() as i32).saturating_sub(8);
        let nx = (*px as i32 + dx).clamp(0, max_x) as usize;
        let ny = (*py as i32 + dy).clamp(0, max_y) as usize;
        if nx != *px || ny != *py {
            paint_cursor(
                fb,
                *core::ptr::addr_of!(CURSOR_X0),
                *core::ptr::addr_of!(CURSOR_Y0),
                false,
            );
            paint_cursor(fb, nx, ny, true);
            *core::ptr::addr_of_mut!(CURSOR_X0) = nx;
            *core::ptr::addr_of_mut!(CURSOR_Y0) = ny;
            *px = nx;
            *py = ny;
        }
    }
    let prev_btns = unsafe { *core::ptr::addr_of!(PREV_BTNS) };
    let clicked = buttons & 0x01 != 0 && prev_btns & 0x01 == 0;
    unsafe {
        *core::ptr::addr_of_mut!(PREV_BTNS) = buttons;
    }
    if clicked {
        let (cx, cy) = unsafe {
            (
                *core::ptr::addr_of!(CURSOR_X),
                *core::ptr::addr_of!(CURSOR_Y),
            )
        };
        if let Some(idx) = tab_at_click(fb, cx, cy) {
            ACTIVE_TAB.store(idx as u64, Ordering::Relaxed);
        }
    }
}

/// Tick source label for the System tab / GUI window: `HW-LAPIC`, `HW-IRQ` or
/// `SOFT`.
pub(crate) fn tick_mode() -> &'static str {
    if crate::lapic::active() {
        "HW-LAPIC"
    } else if IRQ32_SEEN.load(Ordering::Relaxed) {
        "HW-IRQ"
    } else {
        "SOFT"
    }
}

/// Lid state for the status bar: `--` no EC, `?` scanning, `0`/`1` candidate
/// bit value once a transition was observed (polarity is board-specific).
pub(crate) fn lid_state() -> &'static str {
    if !crate::lid::active() {
        "--"
    } else if crate::lid::known() {
        if crate::lid::lid_open() {
            "1"
        } else {
            "0"
        }
    } else {
        "?"
    }
}

/// Verbose lid state for the System tab: appends the pinned EC RAM offset and
/// bit once a transition has been observed.
fn lid_detail() -> String {
    if !crate::lid::active() {
        String::from("-- (no EC)")
    } else if crate::lid::known() {
        format!(
            "{}@{:02x}.{}",
            if crate::lid::lid_open() { "1" } else { "0" },
            crate::lid::candidate_offset(),
            crate::lid::candidate_bit()
        )
    } else {
        String::from("? (scanning)")
    }
}

/// Row of the dashboard occupied by the tab bar (its first panel row).
fn tab_bar_y() -> usize {
    console::text_height()
}

fn content_origin() -> (usize, usize) {
    (0, tab_bar_y() + console::GLYPH_H)
}

fn content_height() -> usize {
    (panel_rows().saturating_sub(1)) * console::GLYPH_H
}

/// Maps a click pixel position onto a tab (`0..=6`), mirroring `draw_tabs`.
fn tab_at_click(fb: &Framebuffer, x: usize, y: usize) -> Option<usize> {
    let slot = fb.width().saturating_sub(16) / TABS.len();
    if slot == 0 {
        return None;
    }
    let idx = x / slot;
    if idx < TABS.len() && y >= tab_bar_y() && y < tab_bar_y() + console::GLYPH_H {
        Some(idx)
    } else {
        None
    }
}

fn draw_tabs(fb: &Framebuffer) {
    let y = tab_bar_y();
    let avail = fb.width().saturating_sub(16);
    let slot = avail / TABS.len();
    let active = ACTIVE_TAB.load(Ordering::Relaxed) as usize;
    for (i, name) in TABS.iter().enumerate() {
        let x = i * slot;
        let sel = i == active;
        let bg = if sel { PANEL_SEL } else { PANEL_BG };
        let fg = if sel { colors::FG } else { TEXT_DIM };
        unsafe {
            fb.fill_rect(x, y, slot, console::GLYPH_H, bg);
        }
        draw_text(fb, x, y, &format!("{}{}", i + 1, name), fg, bg, x + slot);
    }
}

fn draw_status(fb: &Framebuffer) {
    let h = fb.height();
    let y = h.saturating_sub(console::GLYPH_H);
    let midx = (fb.width().saturating_sub(console::GLYPH_W)) / 2;
    let ticks = TICKS.load(Ordering::Relaxed);
    unsafe {
        fb.fill_rect(0, y, fb.width(), console::GLYPH_H, STATUS_BG);
    }
    let helm = format!(
        "{}  tab {}/7  tick={}  mode={}  pit={:04x}",
        VERSION,
        ACTIVE_TAB.load(Ordering::Relaxed) + 1,
        ticks,
        tick_mode(),
        crate::interrupts::pit_count()
    );
    draw_text(fb, 0, y, &helm, TEXT_DIM, STATUS_BG, midx);
    let right = format!(
        "k=0x{:02x} uk={} pk={} m={} pm={} lid={} T={}c",
        LAST_SCANCODE.load(Ordering::Relaxed),
        crate::xhci::KEY_SEQ.load(Ordering::Relaxed),
        crate::ps2::key_seq(),
        crate::xhci::MOUSE_SEQ.load(Ordering::Relaxed),
        crate::ps2::mouse_seq(),
        lid_state(),
        crate::thermal::temp_c(),
    );
    draw_text(
        fb,
        midx + console::GLYPH_W,
        y,
        &right,
        TEXT_DIM,
        STATUS_BG,
        fb.width().saturating_sub(8),
    );
}

/// Paints the whole dashboard from live kernel state and redraws the mouse
/// arrow last so repaints cannot erase it.
pub fn render() {
    let Some(fb) = console::framebuffer() else {
        return;
    };
    ensure_font();
    let active = ACTIVE_TAB.load(Ordering::Relaxed) as usize;
    draw_tabs(fb);
    let (x, y0) = content_origin();
    let max_px = fb.width().saturating_sub(16);
    unsafe {
        fb.fill_rect(x, y0, fb.width(), content_height(), PANEL_BG);
    }
    let mut y = y0;
    match active {
        0 => render_system(fb, &mut y, max_px),
        1 => render_sched(fb, &mut y, max_px),
        2 => render_usb(fb, &mut y, max_px),
        3 => render_ipc(fb, &mut y, max_px),
        4 => render_storage(fb, &mut y, max_px),
        5 => render_shell(fb, &mut y, max_px),
        _ => render_about(fb, &mut y, max_px),
    }
    draw_status(fb);
    unsafe {
        if *core::ptr::addr_of!(CURSOR_INIT) {
            paint_cursor(
                fb,
                *core::ptr::addr_of!(CURSOR_X),
                *core::ptr::addr_of!(CURSOR_Y),
                true,
            );
        }
    }
}

/// Draws one text line and advances the row cursor; stops past the panel.
fn line(fb: &Framebuffer, y: &mut usize, x: usize, text: &str, color: Color, max_px: usize) {
    if *y + console::GLYPH_H <= tab_bar_y() + panel_rows() * console::GLYPH_H {
        draw_text(fb, x, *y, text, color, PANEL_BG, max_px);
    }
    *y += console::GLYPH_H;
}

fn render_system(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    let (sent, recv) = syscalls::stats();
    let ticks = TICKS.load(Ordering::Relaxed);
    line(
        fb,
        y,
        0,
        &format!("AIOS MICROKERNEL - {}", VERSION),
        ACCENT,
        max_px,
    );
    line(
        fb,
        y,
        0,
        &format!(
            "uptime {}s  ticks={}  clock={}",
            ticks / TIMER_HZ,
            ticks,
            tick_mode()
        ),
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        &format!(
            "lid={}  temp={}C tjmax={} sim={}  ps2key={} ps2mouse=id0x{:02x}",
            lid_detail(),
            crate::thermal::temp_c(),
            crate::thermal::tjmax_c(),
            crate::thermal::simulated() as u8,
            crate::ps2::key_seq(),
            crate::ps2::mouse_id()
        ),
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        &format!(
            "frames allocated={}  regions={}  ipc sent={} recv={}",
            crate::memory::frames_allocated(),
            crate::memory::frame_region_count(),
            sent,
            recv
        ),
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        "keys: 1-7 switch tabs | left-click tab | 6=Shell",
        TEXT_DIM,
        max_px,
    );
}

fn render_sched(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    line(fb, y, 0, "SCHEDULER", ACCENT, max_px);
    line(
        fb,
        y,
        0,
        &format!(
            "switches={}  current=pid{}",
            sched::switch_count(),
            sched::current_pid()
        ),
        colors::FG,
        max_px,
    );
    for pid in 1..=4u32 {
        let state = if !sched::task_present(pid) {
            "absent".to_string()
        } else if sched::task_asleep(pid) {
            "sleep".to_string()
        } else if sched::current_pid() == pid {
            "running".to_string()
        } else {
            "ready".to_string()
        };
        line(
            fb,
            y,
            0,
            &format!("pid{}: {}", pid, state),
            colors::FG,
            max_px,
        );
    }
}

fn render_usb(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    let ctl = crate::G_XHCI.load(Ordering::Relaxed);
    line(fb, y, 0, "USB (xHCI)", ACCENT, max_px);
    line(
        fb,
        y,
        0,
        &format!("controller={}", ctl_str(ctl)),
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        &format!(
            "key-seq={} last-sc=0x{:02x}",
            crate::xhci::KEY_SEQ.load(Ordering::Relaxed),
            crate::xhci::KEY_SCANCODE.load(Ordering::Relaxed)
        ),
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        &format!(
            "mouse-seq={} btns={:#x} dx={} dy={}",
            crate::xhci::MOUSE_SEQ.load(Ordering::Relaxed),
            crate::xhci::MOUSE_BUTTONS.load(Ordering::Relaxed),
            (crate::xhci::MOUSE_DX.load(Ordering::Relaxed) as i64) as i32,
            (crate::xhci::MOUSE_DY.load(Ordering::Relaxed) as i64) as i32
        ),
        colors::FG,
        max_px,
    );
}

fn render_ipc(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    let (sent, recv) = syscalls::stats();
    line(fb, y, 0, "IPC (int 0x80 gate)", ACCENT, max_px);
    line(
        fb,
        y,
        0,
        &format!("sent={}  recv={}", sent, recv),
        colors::FG,
        max_px,
    );
    for pid in 1..=4u32 {
        line(
            fb,
            y,
            0,
            &format!("mb{}={}/16", pid, syscalls::mailbox_len(pid)),
            colors::FG,
            max_px,
        );
    }
}

fn render_storage(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    let ahci = crate::G_AHCI.load(Ordering::Relaxed);
    let nvme = crate::G_NVME.load(Ordering::Relaxed);
    line(fb, y, 0, "STORAGE", ACCENT, max_px);
    line(
        fb,
        y,
        0,
        &format!(
            "ahci={}  nvme={}  mmio-window=0x{:x}",
            ctl_str(ahci),
            ctl_str(nvme),
            crate::memory::mmio_used()
        ),
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        "block I/O drivers (ahci/nvme) are wired to the console 0x10-mmio probe",
        TEXT_DIM,
        max_px,
    );
}

fn ctl_str(v: i32) -> &'static str {
    match v {
        2 => "ready+rw",
        1 => "ready",
        0 => "error",
        _ => "none",
    }
}

fn render_shell(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    let s = unsafe { &*core::ptr::addr_of!(SHELL) };
    let budget = panel_rows().saturating_sub(2);
    let start = s.history.len().saturating_sub(budget);
    for entry in s.history.iter().skip(start).take(budget) {
        line(fb, y, 0, entry, colors::FG, max_px);
    }
    line(fb, y, 0, &format!("> {}", s.input), ACCENT, max_px);
    line(
        fb,
        y,
        0,
        "Enter=run  Backspace=edit  Esc=clear",
        TEXT_DIM,
        max_px,
    );
}

fn render_about(fb: &Framebuffer, y: &mut usize, max_px: usize) {
    line(fb, y, 0, VERSION, ACCENT, max_px);
    line(
        fb,
        y,
        0,
        "microkernel TUI on PSF glyphs (synth PSF2, round-trip parsed)",
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        "tabs: 1 System / 2 Sched / 3 USB / 4 IPC / 5 Storage / 6 Shell / 7 About",
        colors::FG,
        max_px,
    );
    line(
        fb,
        y,
        0,
        "mouse: move arrow, left-click a tab to switch; keys also drive the panel",
        TEXT_DIM,
        max_px,
    );
}
