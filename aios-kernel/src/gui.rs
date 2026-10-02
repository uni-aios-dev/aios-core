//! Bare-metal windowed GUI for the AIOS kernel.
//!
//! A full-screen graphical desktop that the kernel shell can switch to with the
//! `gui` command (back to the console/TUI with `tui`, or `Esc` with no window
//! focused). It replaces the console + interactive TUI while active: the
//! console's `vprintln!` calls are skipped so stray log lines cannot garble the
//! desktop, and the heartbeat square is paused. The screen owns a desktop
//! background, a left icon column (one tile per app), a bottom task bar and a
//! z-ordered list of windows with a title bar, minimize/maximize/close
//! buttons, draggable body and resize handles on every edge/corner.
//!
//! Rendering is double-buffered and damage-based: every change records a dirty
//! rectangle and `render()` repaints only what intersects it (desktop
//! background, then icons and the task bar strip, then every window whose
//! rectangle overlaps, then the 8x8 arrow last) into a RAM backbuffer, then
//! publishes the damaged rectangle to VRAM with a single `copy_nonoverlapping`
//! per scanline. Idle frames therefore refresh only the live System/Uptime
//! windows, the panel is never drawn incrementally (no flicker, no torn rows),
//! and the static desktop stays untouched. The backbuffer is mapped once from
//! the frame allocator on `enter()` and its absence degrades gracefully to
//! direct-VRAM painting (never a panic). Text uses the same PSF path as the TUI
//! (`tui::draw_text`). Input comes from both key sources (PS/2 + USB-HID) via
//! `handle_scancode` (Tab cycles focus, `Esc` closes / leaves, printable keys
//! type into the focused kernel window or queue an event for a focused
//! ring-3 client) and from the mouse via `on_mouse` (icon/close/task-bar
//! clicks, title-bar drag, edge/corner resize); the loop reads these through
//! lock-free atomics and never blocks on `inb`/spin for input.
//!
//! Window geometry also has two explicit states: maximize grows a window to
//! the whole desktop above the task bar (geometry saved for restore, further
//! move/resize disabled) and minimize hides it until a task-bar/icon click
//! restores it (`focus_window` un-minimizes, `render` skips hidden windows).

use crate::framebuffer::{colors, Color, Framebuffer};
use crate::interrupts::{TICKS, TIMER_HZ};
use crate::{console, tui};
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Ceiling on open windows (one slot per app instance).
pub const MAX_WINS: usize = 8;

const WIN_W: usize = 300;
const WIN_H: usize = 170;
const TITLE_H: usize = 18;
const TITLE_BTN: usize = 16;
/// Width of the three-button cluster (minimize / maximize-restore / close)
/// anchored to the right end of the title bar.
const TITLE_BTNS: usize = 3 * TITLE_BTN;
const ICON_W: usize = 84;
const ICON_H: usize = 56;
const TASKBAR_H: usize = 18;
/// Width of the hit zone along a window's outer edge that starts a resize
/// drag (edges/corners, checked before title/body).
const RESIZE_BORDER: usize = 5;
/// Smallest window width accepted by an edge/corner resize drag (equals the
/// narrowest default вЂ” the 96 px ring-3 client window).
const RESIZE_MIN_W: usize = 96;
/// Smallest window height (title bar included): the 64 px client body plus
/// `TITLE_H`, i.e. 8 glyph rows of kernel text content.
const RESIZE_MIN_H: usize = TITLE_H + 64;

/// Modal confirm dialog (v2.38.30): fixed size, centered on the desktop.
const DLG_W: usize = 464;
const DLG_H: usize = 140;
/// Top of the button row inside the dialog, measured from its top edge.
const DLG_BTN_Y: usize = 96;
const DLG_BTN_H: usize = 24;
/// Button plates centered as a pair (32 px gap): `OK` then `Cancel`.
const DLG_OK_X: usize = 104;
const DLG_OK_W: usize = 96;
const DLG_CANCEL_X: usize = 232;
const DLG_CANCEL_W: usize = 128;

/// Task Manager body layout (window-local, v2.38.35): the column-header strip
/// sits right under the title bar, rows follow on the 16 px grid, and the
/// count/selection lines fill the gap above the End Task / Switch To plates
/// (at y 216 in [`TASK_WIDGETS`]).
const TASK_HDR_Y: usize = TITLE_H;
/// First task row (window-local).
const TASK_ROWS_Y: usize = 36;
const TASK_ROW_H: usize = 16;
/// x of the Status column (window-local); the task label clips here.
const TASK_STATUS_X: usize = 240;
/// y of the `N tasks` line (window-local); the selection line sits 16 px below.
const TASK_FOOT_Y: usize = 172;

/// Resize-drag edge mask: left edge.
const EDGE_L: u8 = 1;
/// Resize-drag edge mask: right edge.
const EDGE_R: u8 = 2;
/// Resize-drag edge mask: top edge.
const EDGE_T: u8 = 4;
/// Resize-drag edge mask: bottom edge.
const EDGE_B: u8 = 8;

const DESK_BG: Color = 0x00_08_0c_14;
const TITLE_ON: Color = 0x00_2e_5a_c2;
const TITLE_OFF: Color = 0x00_20_2c_48;
const WIN_BG: Color = 0x00_18_24_48;
const BAR_BG: Color = 0x00_10_16_2c;
const BAR_ON: Color = 0x00_2a_4a_92;
const ICON_ACC: Color = 0x00_38_8a_e8;
const CLOSE_BG: Color = 0x00_b0_40_40;
const BTN_BG: Color = 0x00_28_30_50;
const BAR_MIN: Color = 0x00_0a_0e_1c;
const TEXT: Color = 0x00_d0_d0_e0;
const TEXT_DIM: Color = 0x00_80_90_b0;

static ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum WinKind {
    Welcome,
    System,
    Clock,
    About,
    /// Network status and settings: wired NIC/DHCP state plus the software
    /// Wi-Fi stack (scan/connect against the simulated radio).
    Network,
    /// Task Manager (v2.38.35): the Windows-style list of open windows with
    /// an End Task / Switch To button pair acting on the selected row.
    Tasks,
    /// A window owned by a ring-3 task; the id indexes [`CLIENTS`], whose
    /// buffer the window composites on every repaint.
    Client(u8),
}

#[derive(Clone, Copy)]
struct Window {
    kind: WinKind,
    title: &'static str,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    focused: bool,
    /// Hidden by the minimize button; the task-bar/icon buttons restore it.
    minimized: bool,
    /// Expanded to the whole desktop above the task bar.
    maximized: bool,
    /// Geometry saved by the maximize action, restored on toggle-back.
    restore: Option<(usize, usize, usize, usize)>,
    note: [u8; 32],
    note_len: usize,
}

/// One of the three buttons anchored to the right end of a title bar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TitleBtn {
    /// Hide the window (task bar restores it).
    Minimize,
    /// Toggle full-desktop maximize / restore saved geometry.
    Maximize,
    /// Close the window.
    Close,
}

/// Active pointer drag, keyed by the window's [`WinKind`] (stable across
/// `bring_to_front` slot shifts вЂ” an index would go stale the moment the
/// drag starts and the window is focused/brought to the front).
#[derive(Clone, Copy)]
enum Drag {
    /// Move the window by its title bar.
    Move,
    /// Resize from the pressed edge/corner mask; the geometry at press time.
    Resize {
        edges: u8,
        x0: i32,
        y0: i32,
        w0: usize,
        h0: usize,
        wx0: i32,
        wy0: i32,
    },
}

/// A widget hosted in a window body вЂ” the first increment of the widget-set
/// roadmap (v2.38.32, buttons). Coordinates are window-local, so the widget
/// follows its host through move, resize and maximize, and it is painted on
/// top of the body content by [`draw_widgets`] and hit-tested before any
/// client event in [`dispatch_click`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Widget {
    /// A labeled plate that runs its [`WidgetAction`] on click.
    Button {
        /// x offset from the host window's left edge.
        x: usize,
        /// y offset from the host window's top edge.
        y: usize,
        /// Plate width.
        w: usize,
        /// Plate height.
        h: usize,
        /// Glyph text, centered on the plate.
        label: &'static str,
        /// Click behaviour (after the serial proof line).
        action: WidgetAction,
    },
}

/// What a widget click does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetAction {
    /// Open the built-in window of that kind, or focus it if already open.
    Open(WinKind),
    /// Close the hosting window through [`close_window`], so a typed note
    /// still raises the modal confirm dialog (the v2.38.30 gate).
    CloseHost,
    /// Run one of the Network window's command buttons (v2.38.34).
    Net(NetBtn),
    /// Network window text field: move the keyboard focus to field `0..=6`
    /// (`ip`, `mask`, `gw`, `dns1`, `dns2`, `ssid`, `pass`).
    NetField(u8),
    /// Run one of the Task Manager's command buttons (v2.38.35).
    Task(TaskBtn),
}

/// Command buttons of the Network window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NetBtn {
    /// Switch the draft to DHCP addressing.
    Dhcp,
    /// Switch the draft to the static fields.
    Static,
    /// Parse the five address fields and push them through `net::apply`.
    Apply,
    /// Kick an immediate reachability re-probe (`net::recheck`).
    Test,
    /// Ask the software Wi-Fi stack to scan for beacons.
    Scan,
    /// Connect to the drafted SSID/passphrase.
    Connect,
    /// Drop the current Wi-Fi association.
    Disc,
}

/// Command buttons of the Task Manager window (v2.38.35).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskBtn {
    /// Close the selected window through [`close_window`] (the typed-note
    /// confirm dialog gate included - the Windows "end this program?" flow).
    End,
    /// Focus and raise the selected window (Windows "Switch To").
    Switch,
}

static mut WINS: [Option<Window>; MAX_WINS] = [None; MAX_WINS];
static mut CUR_X: usize = 0;
static mut CUR_Y: usize = 0;
static mut CUR_VIS: bool = false;
static mut LAST_BTNS: u8 = 0;
static mut DRAG: Option<(WinKind, Drag)> = None;
/// Active modal confirm dialog: the window it closes on confirm, paired with
/// its serial label. While `Some` every focus/spawn/keyboard/mouse dispatch
/// path is gated (v2.38.30 focus-steal prevention) until the dialog is
/// resolved through [`modal_confirm`] / [`modal_cancel`].
static mut MODAL: Option<(WinKind, &'static str)> = None;
/// Remembered z-ranks of closed windows: `(kind, position bottomв†’top among
/// the open windows at close time)`. A later reopen of the same kind puts the
/// window back at that place in the stack instead of on top (v2.38.31 z-order
/// restore); an empty table just means "open on top" as before. Cleared with
/// the rest of the session state in [`leave`].
static mut CLOSED_Z: [Option<(WinKind, u8)>; MAX_WINS] = [None; MAX_WINS];
/// Currently hovered widget as `(host kind, index into the host's widget
/// table)`; drives the plate highlight in [`draw_widgets`]. Recomputed on
/// every cursor move by [`update_widget_hover`] (frozen while a modal dialog
/// is up) and cleared with the session state in [`leave`].
static mut WIDGET_HOVER: Option<(WinKind, usize)> = None;
/// Task Manager row selected by click (v2.38.35): keyed by [`WinKind`], so it
/// stays valid across z-order/slot shifts and only goes stale when the window
/// closes (End/Switch To then report the missing target). Cleared in [`leave`].
static mut TASK_SEL: Option<WinKind> = None;

/// Ceiling on ring-3 client window slots (one per `SYS_GUI`-registered task).
const MAX_CLIENTS: usize = 4;
/// Max title bytes copied from the client's user string.
const CLIENT_TITLE_CAP: usize = 24;
/// Bounds-safe window/buffer sizes accepted from a ring-3 client.
const CLIENT_MAX_W: usize = 512;
const CLIENT_MAX_H: usize = 512;
/// Hard cap on the client pixel buffer in bytes (bounds check against the
/// app-provided `w * h * 4` at registration).
const CLIENT_BUF_CAP: usize = 512 * 512 * 4;

/// A ring-3 window client: the task's dense pixel buffer (packed same-format
/// pixels, `pitch = w * 4`), its copied title and the single pending input
/// event slot (filled by the GUI input path, drained by `SYS_GUI` GET_EVENT).
struct ClientWin {
    /// Scheduler slot of the owning task (validated on every `SYS_GUI` call).
    pid: u32,
    w: usize,
    h: usize,
    buf: *const u8,
    title: [u8; CLIENT_TITLE_CAP],
    title_len: usize,
    /// One pending event (`0` = none): [`EV_KEY`] or [`EV_CLICK`] encoding.
    /// Keep-first semantics вЂ” a second event arriving before the app polls is
    /// dropped rather than overwriting the queued one.
    event: u64,
}

/// No pending client event (also returned by GET_EVENT when idle).
const EV_NONE: u64 = 0;
/// Event type in the low byte: a key was typed into the focused client.
const EV_KEY: u64 = 1;
/// Event type in the low byte: the client window body was clicked; x in bits
/// 8..24, y in bits 24..40 (body-relative pixels).
const EV_CLICK: u64 = 2;

/// Packs a key event: `EV_KEY | (ascii << 8)`.
fn ev_key(c: char) -> u64 {
    EV_KEY | ((c as u64) << 8)
}

/// Packs a click event: `EV_CLICK | (x << 8) | (y << 24)`, body-relative.
fn ev_click(x: usize, y: usize) -> u64 {
    EV_CLICK | ((x as u64) << 8) | ((y as u64) << 24)
}

/// Queues one input event for client `id` (single pending slot, keep-first
/// until the app drains it via `SYS_GUI` GET_EVENT). Returns whether it was
/// stored; logs a serial proof line for the delivered event.
fn post_client_event(id: u8, event: u64) -> bool {
    let stored = unsafe {
        #[allow(static_mut_refs)]
        {
            match (*core::ptr::addr_of_mut!(CLIENTS))[id as usize].as_mut() {
                Some(c) if c.event == EV_NONE => {
                    c.event = event;
                    true
                }
                _ => false,
            }
        }
    };
    if stored {
        let kind = event & 0xFF;
        if kind == EV_KEY {
            let ch = ((event >> 8) & 0xFF) as u8;
            crate::kprintln!("[serial] [gui] client {} key '{}'", id, ch as char);
        } else if kind == EV_CLICK {
            let x = (event >> 8) & 0xFFFF;
            let y = (event >> 24) & 0xFFFF;
            crate::kprintln!("[serial] [gui] client {} click ({}, {})", id, x, y);
        }
    }
    stored
}

static mut CLIENTS: [Option<ClientWin>; MAX_CLIENTS] = [None, None, None, None];

/// Base of the RAM backbuffer: a dense software frame the GUI renders into and
/// then publishes to VRAM with a damage-aware `blit_region`. Sits in the spare
/// PML4-slot gap directly above the kernel heap (v2.38.33: the 4 MiB heap ends
/// exactly at `BACKBUF_BASE` вЂ” abutting, page-disjoint) and below the
/// paging self-test page (`0xFFFF_FF00_1000_0000`), so it shares the paging
/// hierarchy already built for the heap and never collides with it.
const BACKBUF_BASE: u64 = 0xFFFF_FF00_0040_0000;
/// Marks a failed/absent backbuffer (disables the double buffer for the boot).
const BACKBUF_FAIL: u64 = u64::MAX;

static mut BACK_BASE: u64 = 0;

/// Axis-aligned damage rectangle (`x1`/`y1` exclusive) that the next
/// `render()` has to repaint. Starts full-screen so the first frame paints
/// everything; every mutation unions its affected region into it.
#[derive(Clone, Copy)]
struct Rect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

static mut DIRTY: Rect = Rect {
    x0: 0,
    y0: 0,
    x1: usize::MAX,
    y1: usize::MAX,
};

/// Marks the whole screen dirty (used by discrete structural changes: entering
/// / leaving, opening / closing / focusing windows).
fn dirty_all() {
    unsafe {
        *core::ptr::addr_of_mut!(DIRTY) = Rect {
            x0: 0,
            y0: 0,
            x1: usize::MAX,
            y1: usize::MAX,
        };
    }
}

/// Unions a pixel rectangle into the pending damage.
fn dirty_rect(x: usize, y: usize, w: usize, h: usize) {
    unsafe {
        let d = &mut *core::ptr::addr_of_mut!(DIRTY);
        d.x0 = d.x0.min(x);
        d.y0 = d.y0.min(y);
        d.x1 = d.x1.max(x.saturating_add(w));
        d.y1 = d.y1.max(y.saturating_add(h));
    }
}

/// Unions a whole window rectangle into the pending damage.
fn dirty_window(win: &Window) {
    dirty_rect(win.x, win.y, win.w, win.h);
}

/// Whether rectangle `(x, y, w, h)` overlaps the damage region.
fn rect_overlaps(d: &Rect, x: usize, y: usize, w: usize, h: usize) -> bool {
    x.saturating_add(w) > d.x0 && y.saturating_add(h) > d.y0 && x < d.x1 && y < d.y1
}

/// Maps the RAM backbuffer once (frames are never freed), so painting happens
/// off-screen and `render()` pushes only the dirty rectangle to VRAM. Uses the
/// frame allocator directly instead of the kernel heap: the buffer for a
/// 1920x1080 screen is ~8 MiB and growing the 2 MiB heap for it would starve
/// every other allocation. On any failure the GUI silently falls back to the
/// direct-VRAM path (`render()` handles a missing buffer) вЂ” no panic, no OOM
/// risk during the whole uptime.
fn ensure_backbuffer(fb: &Framebuffer) {
    unsafe {
        if *core::ptr::addr_of!(BACK_BASE) != 0 {
            return;
        }
    }
    let Some(bytes) = fb
        .width()
        .checked_mul(fb.height())
        .and_then(|n| n.checked_mul(4))
    else {
        unsafe {
            *core::ptr::addr_of_mut!(BACK_BASE) = BACKBUF_FAIL;
        }
        return;
    };
    let pages = bytes.div_ceil(crate::memory::PAGE_SIZE as usize);
    for i in 0..pages {
        let Some(frame) = crate::memory::alloc_frame() else {
            unsafe {
                *core::ptr::addr_of_mut!(BACK_BASE) = BACKBUF_FAIL;
            }
            return;
        };
        if crate::memory::map_page(
            BACKBUF_BASE + i as u64 * crate::memory::PAGE_SIZE,
            frame,
            false,
        )
        .is_err()
        {
            unsafe {
                *core::ptr::addr_of_mut!(BACK_BASE) = BACKBUF_FAIL;
            }
            return;
        }
    }
    unsafe {
        *core::ptr::addr_of_mut!(BACK_BASE) = BACKBUF_BASE;
    }
    crate::kprintln!(
        "[serial] gui backbuffer {} KiB at {:#x}",
        bytes / 1024,
        BACKBUF_BASE
    );
}

/// Wraps the mapped RAM backbuffer as a dense painting surface with the same
/// pixel format as VRAM. `None` when the buffer is absent (allocation failed or
/// not yet requested) вЂ” the caller then paints directly into VRAM.
fn back_fb(vram: &Framebuffer) -> Option<Framebuffer> {
    let base = unsafe { *core::ptr::addr_of!(BACK_BASE) };
    if base == 0 || base == BACKBUF_FAIL {
        return None;
    }
    Some(unsafe { Framebuffer::from_ram(base as *mut u8, vram.width(), vram.height(), vram) })
}

/// Whether the windowed GUI currently owns the screen.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn win_title(kind: WinKind) -> &'static str {
    match kind {
        WinKind::Welcome => "Welcome to AIOS GUI",
        WinKind::System => "System",
        WinKind::Clock => "Uptime",
        WinKind::About => "About",
        WinKind::Network => "Network",
        WinKind::Tasks => "Task Manager",
        WinKind::Client(_) => "ring3 client",
    }
}

/// Client window record for `id` (bounds-checked against [`MAX_CLIENTS`]).
fn client_slot(id: u8) -> Option<&'static ClientWin> {
    let id = id as usize;
    if id >= MAX_CLIENTS {
        return None;
    }
    unsafe {
        #[allow(static_mut_refs)]
        (*core::ptr::addr_of!(CLIENTS))[id].as_ref()
    }
}

/// The title shown in the client window's title bar / task bar: the live
/// per-window title borrowed from its client record, falling back to the
/// built-in one when the record is gone (e.g. closed while the window lingers).
fn win_live_title(win: &Window) -> &str {
    if let WinKind::Client(id) = win.kind {
        if let Some(c) = client_slot(id) {
            let n = c.title_len.min(c.title.len());
            return core::str::from_utf8(&c.title[..n]).unwrap_or("ring3");
        }
    }
    win.title
}

fn open_slot() -> Option<usize> {
    (0..MAX_WINS).find(|&i| unsafe { (*core::ptr::addr_of!(WINS))[i].is_none() })
}

fn window_count() -> usize {
    (0..MAX_WINS)
        .filter(|&i| unsafe { (*core::ptr::addr_of!(WINS))[i].is_some() })
        .count()
}

/// Brings window `i` to the front of the z-order (last drawn).
fn bring_to_front(i: usize) {
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        let Some(win) = wins[i].take() else {
            return;
        };
        for j in i..MAX_WINS - 1 {
            wins[j] = wins[j + 1].take();
        }
        wins[MAX_WINS - 1] = Some(win);
    }
}

fn focused_idx() -> Option<usize> {
    (0..MAX_WINS)
        .filter(|&i| unsafe {
            (*core::ptr::addr_of!(WINS))[i]
                .as_ref()
                .map(|w| w.focused)
                .unwrap_or(false)
        })
        .max()
}

/// Kind of the focused window, if any вЂ” the target of the keyboard hotkeys.
fn focused_kind() -> Option<WinKind> {
    let i = focused_idx()?;
    unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref().map(|w| w.kind) }
}

/// Records `kind`'s z-rank (0 = bottom of the stack) for a future reopen.
/// Re-closing the same kind overwrites the older rank; a full table just
/// drops the new one (the table is at most [`MAX_WINS`] deep anyway).
fn remember_closed_z(kind: WinKind, rank: u8) {
    unsafe {
        let mem = &mut *core::ptr::addr_of_mut!(CLOSED_Z);
        let mut free: Option<usize> = None;
        let mut hit: Option<usize> = None;
        for (i, slot) in mem.iter().enumerate() {
            match *slot {
                Some((k, _)) if k == kind => {
                    hit = Some(i);
                    break;
                }
                None if free.is_none() => free = Some(i),
                _ => {}
            }
        }
        if let Some(i) = hit.or(free) {
            mem[i] = Some((kind, rank));
        }
    }
}

/// Takes (and forgets) the rank remembered for `kind` by
/// [`remember_closed_z`] вЂ” the caller then places the reopened window there.
fn take_closed_z(kind: WinKind) -> Option<u8> {
    let found = unsafe {
        let mem = &mut *core::ptr::addr_of_mut!(CLOSED_Z);
        let mut found = None;
        for (i, slot) in mem.iter().enumerate() {
            if let Some((k, r)) = *slot {
                if k == kind {
                    found = Some((i, r));
                    break;
                }
            }
        }
        if let Some((i, _)) = found {
            mem[i] = None;
        }
        found
    };
    found.map(|(_, r)| r)
}

/// Inserts `win` into the z-order: at the rank remembered by
/// [`remember_closed_z`] when its kind was closed before, otherwise on top
/// (append). The slot array is compacted on the way, so holes left by earlier
/// closes disappear and slot order stays a dense bottomв†’top sequence.
fn place_window(win: Window) {
    let label = win_label(win.kind, win.title);
    let restored = take_closed_z(win.kind);
    let open = window_count();
    let rank = restored.map(|r| r as usize).unwrap_or(open).min(open);
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        let mut seq: [Option<Window>; MAX_WINS] = [None; MAX_WINS];
        let mut n = 0usize;
        for w in wins.iter().flatten() {
            seq[n] = Some(*w);
            n += 1;
        }
        for j in (rank..n).rev() {
            seq[j + 1] = seq[j];
        }
        seq[rank] = Some(win);
        *wins = seq;
    }
    if restored.is_some() {
        crate::kprintln!("[gui] reopen {} at z {}", label, rank);
    }
}

/// Opens a built-in window of `kind` focused, at its remembered z-rank or on
/// top (see [`place_window`]); refused while a modal dialog is up or the
/// window table is full.
fn spawn(kind: WinKind) {
    // Modal dialog up: no window may open underneath it.
    if modal_active() {
        return;
    }
    if open_slot().is_none() {
        return;
    }
    dirty_all();
    let n = window_count();
    let fb_w = console::framebuffer().map(|fb| fb.width()).unwrap_or(800);
    let fb_h = console::framebuffer().map(|fb| fb.height()).unwrap_or(600);
    let cascade = n % 5;
    let mut x = (fb_w / 2 + cascade * 28).saturating_sub(WIN_W / 2);
    let mut y = (fb_h / 3 + cascade * 34).min(fb_h.saturating_sub(WIN_H + TASKBAR_H + 20));
    let mut w = WIN_W;
    let mut h = if kind == WinKind::Clock { 140 } else { WIN_H };
    if kind == WinKind::Network {
        w = 420;
        h = 356;
        x = 420.min(fb_w.saturating_sub(w + 8));
        y = 356.min(fb_h.saturating_sub(h + TASKBAR_H + 8));
        net_draft_load();
    }
    if kind == WinKind::Tasks {
        w = 440;
        h = 248;
        x = 460.min(fb_w.saturating_sub(w + 8));
        y = 140.min(fb_h.saturating_sub(h + TASKBAR_H + 8));
    }
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        for win in wins.iter_mut().flatten() {
            win.focused = false;
        }
    }
    place_window(Window {
        kind,
        title: win_title(kind),
        x,
        y,
        w,
        h,
        focused: true,
        minimized: false,
        maximized: false,
        restore: None,
        note: [0; 32],
        note_len: 0,
    });
    crate::kprintln!("[gui] open {}", win_label(kind, win_title(kind)));
}

fn focus_window(i: usize) {
    // Modal dialog up: focus changes are refused (focus-steal prevention).
    if modal_active() {
        return;
    }
    dirty_all();
    let mut restored = None;
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        for win in wins.iter_mut().flatten() {
            win.focused = false;
        }
        if let Some(win) = wins[i].as_mut() {
            win.focused = true;
            // Focusing a hidden window (task bar / icon) restores it.
            if win.minimized {
                win.minimized = false;
                restored = Some((win.kind, win.title, win.w, win.h));
            }
        }
    }
    if let Some((kind, title, w, h)) = restored {
        crate::kprintln!("[gui] restore {} -> {}x{}", win_label(kind, title), w, h);
    }
    bring_to_front(i);
}

fn unfocus_all() {
    dirty_all();
    unsafe {
        for win in (*core::ptr::addr_of_mut!(WINS)).iter_mut().flatten() {
            win.focused = false;
        }
    }
}

/// Closes window `i` вЂ” unless it holds a typed-text note, in which case the
/// modal confirm dialog opens instead. All three close paths (Esc, the `X`
/// button, `F4`) funnel here, so none of them can silently drop the note.
fn close_window(i: usize) {
    let gate = unsafe {
        (*core::ptr::addr_of!(WINS))[i]
            .as_ref()
            .map(|w| (w.note_len > 0, w.kind, w.title))
    };
    if let Some((true, kind, title)) = gate {
        modal_open(kind, title);
        return;
    }
    close_window_raw(i);
}

/// Teardown half of [`close_window`]: records the window's z-rank for a
/// future reopen (v2.38.31), clears the slot (and any drag that pointed at
/// it) and logs the `[gui] close` proof line.
fn close_window_raw(i: usize) {
    dirty_all();
    unsafe {
        if let Some((kind, _)) = core::ptr::addr_of!(DRAG).read() {
            if (*core::ptr::addr_of!(WINS))[i].map(|w| w.kind) == Some(kind) {
                DRAG = None;
            }
        }
        if let Some(win) = (*core::ptr::addr_of!(WINS))[i].as_ref() {
            let rank = (0..i)
                .filter(|&j| (*core::ptr::addr_of!(WINS))[j].is_some())
                .count();
            remember_closed_z(win.kind, rank as u8);
            crate::kprintln!(
                "[gui] close {} (z {})",
                win_label(win.kind, win.title),
                rank
            );
        }
        (*core::ptr::addr_of_mut!(WINS))[i] = None;
    }
}

/// Whether the modal confirm dialog is up (the focus-steal input gate).
fn modal_active() -> bool {
    unsafe { core::ptr::addr_of!(MODAL).read().is_some() }
}

/// Center of the fixed-size dialog on the given framebuffer.
fn modal_pos(fb: &Framebuffer) -> (usize, usize) {
    ((fb.width() - DLG_W) / 2, (fb.height() - DLG_H) / 2)
}

/// Raises the confirm dialog over `kind`'s window and parks input: an active
/// title-bar drag is dropped so no window can move while the dialog is modal.
fn modal_open(kind: WinKind, title: &'static str) {
    unsafe {
        *core::ptr::addr_of_mut!(MODAL) = Some((kind, title));
        *core::ptr::addr_of_mut!(DRAG) = None;
    }
    dirty_all();
    crate::kprintln!("[gui] modal open {}", win_label(kind, title));
}

/// Dismisses the dialog, logging how it was resolved (`confirmed` /
/// `canceled`).
fn modal_close(confirmed: bool) {
    let m = unsafe {
        let m = core::ptr::addr_of!(MODAL).read();
        *core::ptr::addr_of_mut!(MODAL) = None;
        m
    };
    let Some((kind, title)) = m else {
        return;
    };
    dirty_all();
    crate::kprintln!(
        "[gui] modal close {} {}",
        win_label(kind, title),
        if confirmed { "confirmed" } else { "canceled" }
    );
}

/// Enter on the dialog: closes the target window for real and dismisses.
/// Teardown runs through [`close_window_raw`] so the note gate cannot re-open
/// the dialog it is resolving.
fn modal_confirm() {
    let target = unsafe { core::ptr::addr_of!(MODAL).read().map(|(kind, _)| kind) };
    if let Some(kind) = target {
        if let Some(i) = find_kind(kind) {
            close_window_raw(i);
        }
    }
    modal_close(true);
}

/// Esc on the dialog: keep the window and its typed note untouched.
fn modal_cancel() {
    modal_close(false);
}

/// Keyboard routing while the dialog is up: Enter confirms, Esc cancels,
/// every other make-code is consumed and logged so the block is provable
/// in the serial stream.
fn modal_key(sc: u8) -> bool {
    match sc {
        0x1C => modal_confirm(),
        0x01 => modal_cancel(),
        other => crate::kprintln!("[gui] modal blocks scancode {:#04x}", other),
    }
    true
}

/// Mouse routing while the dialog is up: only the two button plates act;
/// any other press (window body/title, icon, task bar) is blocked, so the
/// focus вЂ” and ring-3 client input вЂ” cannot be stolen.
fn modal_click(fb: &Framebuffer, x: usize, y: usize) {
    if !modal_active() {
        return;
    }
    let (dx, dy) = modal_pos(fb);
    let ry = dy + DLG_BTN_Y;
    let in_rect =
        |rx: usize, rw: usize| x >= dx + rx && x < dx + rx + rw && y >= ry && y < ry + DLG_BTN_H;
    if in_rect(DLG_OK_X, DLG_OK_W) {
        modal_confirm();
    } else if in_rect(DLG_CANCEL_X, DLG_CANCEL_W) {
        modal_cancel();
    } else {
        crate::kprintln!("[gui] modal blocks click ({},{})", x, y);
    }
}

fn find_kind(kind: WinKind) -> Option<usize> {
    (0..MAX_WINS)
        .find(|&i| unsafe { (*core::ptr::addr_of!(WINS))[i].map(|w| w.kind == kind) == Some(true) })
}

/// Toggles the window between its saved geometry and a full-desktop maximize
/// (the desktop minus the task bar). Log line carries the resulting size.
fn toggle_maximize(kind: WinKind) {
    let Some(i) = find_kind(kind) else {
        return;
    };
    let line;
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        let Some(win) = wins[i].as_mut() else {
            return;
        };
        let action = if win.maximized { "restore" } else { "maximize" };
        if win.maximized {
            if let Some((x, y, w, h)) = win.restore.take() {
                win.x = x;
                win.y = y;
                win.w = w;
                win.h = h;
            }
            win.maximized = false;
        } else {
            win.restore = Some((win.x, win.y, win.w, win.h));
            let fb_w = console::framebuffer().map(|fb| fb.width()).unwrap_or(800);
            let fb_h = console::framebuffer().map(|fb| fb.height()).unwrap_or(600);
            win.x = 0;
            win.y = 0;
            win.w = fb_w;
            win.h = fb_h.saturating_sub(TASKBAR_H);
            win.maximized = true;
        }
        line = Some((win_label(win.kind, win.title), action, win.w, win.h));
    }
    if let Some((label, action, w, h)) = line {
        crate::kprintln!("[gui] {} {} -> {}x{}", action, label, w, h);
    }
    dirty_all();
    focus_window(i);
}

/// Hides the window behind its task-bar button; when it was focused, focus
/// falls to the topmost visible window (or nothing at all).
fn minimize_window(kind: WinKind) {
    let Some(i) = find_kind(kind) else {
        return;
    };
    let line;
    unsafe {
        if let Some((k, _)) = *core::ptr::addr_of!(DRAG) {
            if k == kind {
                DRAG = None;
            }
        }
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        let Some(win) = wins[i].as_mut() else {
            return;
        };
        win.minimized = true;
        line = Some((win_label(win.kind, win.title), win.focused));
    }
    if let Some((label, was_focused)) = line {
        crate::kprintln!("[gui] minimize {}", label);
        dirty_all();
        if was_focused {
            if let Some(j) = (0..MAX_WINS).rev().find(|&j| unsafe {
                (*core::ptr::addr_of!(WINS))[j].map(|w| !w.minimized) == Some(true)
            }) {
                focus_window(j);
            } else {
                unfocus_all();
            }
        }
    }
}

fn cycle_focus() {
    // Belt-and-suspenders: the key gate already routes F9/Tab to `modal_key`.
    if modal_active() {
        return;
    }
    let wins = (0..MAX_WINS)
        .filter(|&i| unsafe { (*core::ptr::addr_of!(WINS))[i].map(|w| !w.minimized) == Some(true) })
        .collect::<alloc::vec::Vec<usize>>();
    if wins.is_empty() {
        return;
    }
    let cur = focused_idx().and_then(|i| wins.iter().position(|&w| w == i));
    let next = match cur {
        Some(p) => wins[(p + 1) % wins.len()],
        None => wins[0],
    };
    // Capture the label first: `focus_window` в†’ `bring_to_front` moves the
    // window out of slot `next` by the time we would read it.
    let label = unsafe {
        (*core::ptr::addr_of!(WINS))[next]
            .as_ref()
            .map(|w| win_label(w.kind, w.title))
    };
    focus_window(next);
    if let Some(label) = label {
        crate::kprintln!("[gui] focus {}", label);
    }
}

fn open_or_focus(kind: WinKind) {
    for i in 0..MAX_WINS {
        if let Some(win) = unsafe { &(*core::ptr::addr_of!(WINS))[i] } {
            if win.kind == kind {
                focus_window(i);
                return;
            }
        }
    }
    spawn(kind);
}

/// Rank of a window kind in the stable task-bar order: built-in apps in
/// their canonical order, ring-3 clients last by id.
fn kind_rank(kind: WinKind) -> u8 {
    match kind {
        WinKind::Welcome => 0,
        WinKind::System => 1,
        WinKind::Clock => 2,
        WinKind::About => 3,
        WinKind::Network => 4,
        WinKind::Tasks => 5,
        WinKind::Client(id) => 6 + id,
    }
}

/// All open window indices in [`kind_rank`] order (`usize::MAX` marks the
/// unused tail). Both the task-bar hit test and its drawing walk this order,
/// so buttons keep their place across focus/z-order changes вЂ” a minimized
/// window's button never moves under the pointer.
fn taskbar_slots() -> [usize; MAX_WINS] {
    let mut out = [usize::MAX; MAX_WINS];
    let mut n = 0usize;
    for rank in 0u8..(6 + MAX_CLIENTS as u8) {
        for i in 0..MAX_WINS {
            let hit = unsafe {
                (*core::ptr::addr_of!(WINS))[i].map(|w| kind_rank(w.kind) == rank) == Some(true)
            };
            if hit {
                out[n] = i;
                n += 1;
                break;
            }
        }
    }
    out
}

/// Enters GUI mode from the kernel shell (`gui` command).
pub fn enter() {
    if active() {
        return;
    }
    if let Some(fb) = console::framebuffer() {
        ensure_backbuffer(fb);
    }
    open_or_focus(WinKind::Welcome);
    if window_count() == 1 {
        open_or_focus(WinKind::System);
    }
    if window_count() == 2 {
        open_or_focus(WinKind::Clock);
    }
    raise_clients();
    ACTIVE.store(true, Ordering::Relaxed);
    render();
}

/// Raises every registered ring-3 client window above the built-in stack.
/// Client windows are registered while the console still owns the screen, so
/// by the time `enter()` opens Welcome/System/Clock they sit underneath them.
/// Processed in descending slot order so each `bring_to_front` shift cannot
/// invalidate an index not yet moved.
fn raise_clients() {
    let mut clients = [usize::MAX; MAX_WINS];
    let mut n = 0;
    for i in 0..MAX_WINS {
        let is_client = unsafe { &(*core::ptr::addr_of!(WINS))[i] }
            .map(|w| matches!(w.kind, WinKind::Client(_)))
            .unwrap_or(false);
        if is_client {
            clients[n] = i;
            n += 1;
        }
    }
    for k in (0..n).rev() {
        bring_to_front(clients[k]);
    }
    // Focus the frontmost client so its application receives input right away.
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    for i in (0..MAX_WINS).rev() {
        if let Some(win) = wins[i].as_ref() {
            if matches!(win.kind, WinKind::Client(_)) {
                focus_window(i);
                break;
            }
        }
    }
}

/// Leaves GUI mode and restores the console + TUI screen (`tui` command, or
/// `Esc` with no focused window). The in-memory console log was never
/// populated while the GUI owned the screen, so `console::clear()` just
/// repaints the framebuffer to its base state.
pub fn leave() {
    if !active() {
        return;
    }
    ACTIVE.store(false, Ordering::Relaxed);
    unsafe {
        for i in 0..MAX_WINS {
            (*core::ptr::addr_of_mut!(WINS))[i] = None;
        }
        DRAG = None;
        *core::ptr::addr_of_mut!(MODAL) = None;
        *core::ptr::addr_of_mut!(CLOSED_Z) = [None; MAX_WINS];
        WIDGET_HOVER = None;
        TASK_SEL = None;
        CUR_VIS = false;
        LAST_BTNS = 0;
    }
    PIN_STREAK.store(0, Ordering::Relaxed);
    PIN_LOCK.store(false, Ordering::Relaxed);
    crate::console::clear();
    tui::render();
}

/// Layout of the `SYS_GUI` CREATE request (user memory, fixed 16 bytes).
///
/// `buf` and `title` are user virtual addresses of the client's dense pixel
/// buffer and its NUL-terminated title string; both live below 4 GiB in the
/// ring-3 demo layout, so a single `u32` per pointer is enough.
#[repr(C)]
struct CreateReq {
    w: u32,
    h: u32,
    buf: u32,
    title: u32,
}

/// Wire entry for the `SYS_GUI` syscall from ring-3, dispatched on `rdi`:
///
/// - `0` вЂ” CREATE with `rsi` pointing at a [`CreateReq`] (returns the client
///   id, `u64::MAX` on any validation failure);
/// - `1` вЂ” PRESENT: the client's window is made dirty (and re-spawned if it
///   was closed, e.g. by the GUI `leave()`), so the composite picks up the new
///   pixels on the next `render()` вЂ” this is the ring-3 client's "flip";
/// - `2` вЂ” GET_EVENT: drains the single pending input event (returns the
///   [`EV_KEY`]/[`EV_CLICK`] packed word, [`EV_NONE`] when idle, `u64::MAX`
///   if the task owns no client window);
/// - anything else вЂ” `u64::MAX`.
pub fn client_syscall(pid: u32, frame: &mut crate::interrupts::InterruptFrame) {
    if frame.rdi == 0 {
        let req_va = frame.rsi;
        // Request struct must be fully mapped and its fields within sensible
        // window bounds before anything is copied.
        let valid = crate::memory::translate(req_va).is_some()
            && crate::memory::translate(req_va + 15).is_some();
        if !valid {
            frame.rax = u64::MAX;
            return;
        }
        let req = unsafe { core::ptr::read_volatile(req_va as *const CreateReq) };
        let w = req.w as usize;
        let h = req.h as usize;
        let buf_va = req.buf as u64;
        let mut title_va = req.title as u64;
        if w < 8 || h < 8 || w > CLIENT_MAX_W || h > CLIENT_MAX_H {
            frame.rax = u64::MAX;
            return;
        }
        let Some(area) = w.checked_mul(h).and_then(|n| n.checked_mul(4)) else {
            frame.rax = u64::MAX;
            return;
        };
        if area > CLIENT_BUF_CAP
            || crate::memory::translate(buf_va).is_none()
            || crate::memory::translate(buf_va + area as u64 - 1).is_none()
        {
            frame.rax = u64::MAX;
            return;
        }
        // Copy the NUL-terminated title byte-by-byte, validating each address.
        let mut title = [0u8; CLIENT_TITLE_CAP];
        let mut title_len = 0usize;
        loop {
            if title_len == CLIENT_TITLE_CAP {
                break;
            }
            if crate::memory::translate(title_va).is_none() {
                break;
            }
            let byte = unsafe { core::ptr::read_volatile(title_va as *const u8) };
            title_va += 1;
            if byte == 0 {
                break;
            }
            if (0x20..=0x7E).contains(&byte) {
                title[title_len] = byte;
                title_len += 1;
            }
        }
        create_client(pid, w, h, buf_va as *const u8, title, title_len, frame);
    } else if frame.rdi == 1 {
        // PRESENT
        match client_idx_of(pid) {
            Some(id) => {
                present_client(id);
                frame.rax = 1;
            }
            None => {
                frame.rax = u64::MAX;
            }
        }
    } else if frame.rdi == 2 {
        // GET_EVENT вЂ” drain the single pending input event.
        frame.rax = take_client_event(pid).unwrap_or(u64::MAX);
    } else {
        frame.rax = u64::MAX;
    }
}

/// Index in `CLIENTS` of the client owned by `pid`, if any.
fn client_idx_of(pid: u32) -> Option<u8> {
    unsafe {
        #[allow(static_mut_refs)]
        {
            for (i, c) in (*core::ptr::addr_of!(CLIENTS)).iter().enumerate() {
                if let Some(c) = c {
                    if c.pid == pid {
                        return Some(i as u8);
                    }
                }
            }
        }
        None
    }
}

/// Removes and returns the pending event of the client owned by `pid`
/// (`Some(0)` when there is simply nothing queued, `None` when the task owns
/// no client window at all).
fn take_client_event(pid: u32) -> Option<u64> {
    unsafe {
        #[allow(static_mut_refs)]
        {
            for c in (*core::ptr::addr_of_mut!(CLIENTS)).iter_mut().flatten() {
                if c.pid == pid {
                    let event = c.event;
                    c.event = EV_NONE;
                    return Some(event);
                }
            }
        }
        None
    }
}

/// Registers a validated client and spawns its window; returns the client id.
fn create_client(
    pid: u32,
    w: usize,
    h: usize,
    buf: *const u8,
    title: [u8; CLIENT_TITLE_CAP],
    title_len: usize,
    frame: &mut crate::interrupts::InterruptFrame,
) {
    // Reuse (overwrite) a slot already owned by this pid, else take a free one.
    let id = unsafe {
        #[allow(static_mut_refs)]
        {
            let mut owned: Option<usize> = None;
            let mut free: Option<usize> = None;
            for (i, c) in (*core::ptr::addr_of_mut!(CLIENTS)).iter_mut().enumerate() {
                match c {
                    Some(existing) if existing.pid == pid => {
                        owned = Some(i);
                        break;
                    }
                    None if free.is_none() => free = Some(i),
                    _ => {}
                }
            }
            owned.or(free)
        }
    };
    let Some(id) = id else {
        frame.rax = u64::MAX;
        return;
    };
    let id = id as u8;
    unsafe {
        #[allow(static_mut_refs)]
        {
            *core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(CLIENTS))[id as usize]) =
                Some(ClientWin {
                    pid,
                    w,
                    h,
                    buf,
                    title,
                    title_len,
                    event: EV_NONE,
                });
        }
    }
    crate::kprintln!(
        "[serial] [gui] ring3 pid {} registered client window {} ({}x{})",
        pid,
        id,
        w,
        h
    );
    spawn_client(id);
    frame.rax = id as u64;
}

/// Owns a `Client(id)` window: sizes it to the client buffer plus the title
/// bar and places it in the z-order via [`place_window`] (remembered rank
/// after a close, otherwise on top).
fn spawn_client(id: u8) {
    // Modal dialog up: no window may open underneath it.
    if modal_active() {
        return;
    }
    let Some(c) = client_slot(id) else {
        return;
    };
    if open_slot().is_none() {
        return;
    }
    dirty_all();
    let n = window_count();
    let fb_w = console::framebuffer().map(|fb| fb.width()).unwrap_or(800);
    let fb_h = console::framebuffer().map(|fb| fb.height()).unwrap_or(600);
    let cascade = n % 5;
    let w = c.w;
    let h = c.h + TITLE_H;
    let x = (fb_w / 2 + cascade * 28).saturating_sub(w / 2);
    let y = (fb_h / 3 + cascade * 34).min(fb_h.saturating_sub(h + TASKBAR_H + 20));
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        for win in wins.iter_mut().flatten() {
            win.focused = false;
        }
    }
    place_window(Window {
        kind: WinKind::Client(id),
        title: win_title(WinKind::Client(id)),
        x,
        y,
        w,
        h,
        focused: true,
        minimized: false,
        maximized: false,
        restore: None,
        note: [0; 32],
        note_len: 0,
    });
}

/// Marks the client's window damage so the next `render()` recomposites it.
/// If the window was closed (GUI `leave()` / `Esc`) it is re-spawned first, so
/// a live ring-3 client always gets its surface back on the next present.
fn present_client(id: u8) {
    let exists_and_dirty = (0..MAX_WINS).any(|i| unsafe {
        #[allow(static_mut_refs)]
        {
            let w = &(*core::ptr::addr_of!(WINS))[i];
            if let Some(win) = w {
                if win.kind == WinKind::Client(id) {
                    dirty_window(win);
                    return true;
                }
            }
        }
        false
    });
    if !exists_and_dirty {
        spawn_client(id);
    }
}

/// Composites the client's dense buffer into the window body (below its title
/// bar). The copy honours the user buffer's own pitch and is bounds/clip-safe
/// on both sides.
fn draw_client(fb: &Framebuffer, win: &Window) {
    let Some(c) = client_slot(match win.kind {
        WinKind::Client(id) => id,
        _ => return,
    }) else {
        return;
    };
    let body = win.h.saturating_sub(TITLE_H);
    if body == 0 || c.w == 0 || c.buf.is_null() {
        return;
    }
    unsafe {
        let client = Framebuffer::from_ram(c.buf as *mut u8, c.w, c.h, fb);
        if win.w == c.w && body == c.h {
            fb.blit_at(
                &client,
                (0, 0),
                (win.x, win.y + TITLE_H),
                (c.w.min(win.w), c.h.min(body)),
            );
        } else {
            // The user resized the window: scale the client's native buffer
            // (nearest neighbour) to the body, keeping its own resolution.
            fb.blit_scaled(
                &client,
                (0, 0),
                (win.x, win.y + TITLE_H),
                (c.w, c.h),
                (win.w, body),
            );
        }
    }
}

/// Feeds one make-code to the windowed GUI. Returns `true` when consumed.
pub fn handle_scancode(sc: u8) -> bool {
    if !active() {
        return false;
    }
    // Modal dialog owns the keyboard until it is resolved.
    if modal_active() {
        return modal_key(sc);
    }
    // The focused Task Manager window takes Enter first: Switch To on the
    // current selection (v2.38.35).
    if task_key(sc) {
        return true;
    }
    // The focused Network window edits its draft form first (v2.38.34):
    // Tab/Backspace move the field focus, Enter applies, printable keys type.
    if net_key(sc) {
        return true;
    }
    match sc {
        0x01 => {
            if let Some(i) = focused_idx() {
                close_window(i);
            } else if window_count() == 0 {
                leave();
            }
            return true;
        }
        0x0F => {
            cycle_focus();
            return true;
        }
        0x0E => {
            if let Some(i) = focused_idx() {
                unsafe {
                    if let Some(win) = (*core::ptr::addr_of!(WINS))[i].as_ref() {
                        dirty_window(win);
                    }
                }
                unsafe {
                    let wins = &mut *core::ptr::addr_of_mut!(WINS);
                    if let Some(win) = wins[i].as_mut() {
                        if win.note_len > 0 {
                            win.note_len -= 1;
                        }
                    }
                }
            }
            return true;
        }
        // Window hotkeys on plain F-key make codes: `idle_loop` forwards only
        // non-extended make bytes (break codes are filtered out by the
        // `sc & 0x80` gate), so F-keys need no modifier/release tracking and
        // can never collide with typing (they are not printable).
        0x3C => {
            // F2 вЂ” maximize/restore the focused window.
            if let Some(kind) = focused_kind() {
                toggle_maximize(kind);
            }
            return true;
        }
        0x3E => {
            // F4 вЂ” close the focused window (same path as Esc and the X button).
            if let Some(i) = focused_idx() {
                close_window(i);
            }
            return true;
        }
        0x3F => {
            // F5 вЂ” minimize the focused window.
            if let Some(kind) = focused_kind() {
                minimize_window(kind);
            }
            return true;
        }
        0x43 => {
            // F9 вЂ” cycle focus (Tab twin).
            cycle_focus();
            return true;
        }
        _ => {}
    }
    let Some(c) = crate::interrupts::scancode_to_char(sc) else {
        return false;
    };
    if c.is_control() {
        return true;
    }
    // A focused ring-3 client window consumes the key as an input event for
    // its application instead of the kernel-side typed-text note.
    if let Some(i) = focused_idx() {
        let client_id = unsafe {
            #[allow(static_mut_refs)]
            (*core::ptr::addr_of!(WINS))[i]
                .as_ref()
                .and_then(|w| match w.kind {
                    WinKind::Client(id) => Some(id),
                    _ => None,
                })
        };
        if let Some(id) = client_id {
            post_client_event(id, ev_key(c));
            return true;
        }
    }
    if let Some(i) = focused_idx() {
        unsafe {
            if let Some(win) = (*core::ptr::addr_of!(WINS))[i].as_ref() {
                dirty_window(win);
            }
        }
    }
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        if let Some(win) = wins.iter_mut().flatten().find(|w| w.focused) {
            if win.note_len < win.note.len() {
                win.note[win.note_len] = c as u8;
                win.note_len += 1;
            }
        }
    }
    true
}

/// Packets that keep pushing an already-clamped cursor further into the same
/// screen edge before the pointer is suppressed (roughly a second of a
/// runaway stream).
const RUNAWAY_STREAK: u32 = 60;
/// Runaway gate: consecutive edge-pushing packets (see [`input_allowed`]).
static PIN_STREAK: AtomicU32 = AtomicU32::new(0);
/// Runaway gate: while true, pointer input is suppressed until a packet moves
/// the cursor away from the pinned edge.
static PIN_LOCK: AtomicBool = AtomicBool::new(false);

/// Runaway-pointer gate (v2.38.33): decides whether one pointer report may be
/// processed (logged, applied, rendered) by the active screen owner. Real
/// hardware that streams garbage deltas вЂ” a desynced PS/2 touchpad next to a
/// USB mouse, or a misbehaving xHCI device вЂ” used to pin the arrow in the
/// bottom-left corner forever while every packet cost a serial line, a
/// formatted log and a full repaint (the lag/storm seen on the laptop).
///
/// [`RUNAWAY_STREAK`] consecutive packets that push an already-clamped cursor
/// further into an edge switch the gate off; it re-arms only when a moving
/// packet pushes the cursor away from that edge (button-only reports stay
/// swallowed so the phantom stream cannot click the task bar). Each state
/// transition logs one line, so a serial capture shows exactly when and why
/// input was suppressed.
pub fn input_allowed(dx: i32, dy: i32) -> bool {
    let Some(fb) = console::framebuffer() else {
        return true;
    };
    // Before the first packet the arrow has no position yet (it centres on
    // the next report) вЂ” nothing can be pinned, so let input through.
    if !unsafe { *core::ptr::addr_of!(CUR_VIS) } {
        return true;
    }
    let (x, y) = unsafe { (*core::ptr::addr_of!(CUR_X), *core::ptr::addr_of!(CUR_Y)) };
    let max_x = (fb.width() as i32).saturating_sub(8);
    let max_y = (fb.height() as i32).saturating_sub(8);
    let pin = (x == 0 && dx < 0)
        || (x as i32 >= max_x && dx > 0)
        || (y == 0 && dy < 0)
        || (y as i32 >= max_y && dy > 0);
    let moving = dx != 0 || dy != 0;
    if PIN_LOCK.load(Ordering::Relaxed) {
        if moving && !pin {
            PIN_LOCK.store(false, Ordering::Relaxed);
            PIN_STREAK.store(0, Ordering::Relaxed);
            crate::kprintln!("[gui] pointer runaway released");
            return true;
        }
        return false;
    }
    if !pin {
        if moving {
            PIN_STREAK.store(0, Ordering::Relaxed);
        }
        return true;
    }
    let streak = PIN_STREAK.fetch_add(1, Ordering::Relaxed) + 1;
    if streak >= RUNAWAY_STREAK {
        PIN_LOCK.store(true, Ordering::Relaxed);
        crate::kprintln!(
            "[gui] pointer runaway: input suppressed ({} packets pushing into the edge)",
            streak
        );
        return false;
    }
    true
}

/// Applies a mouse report: moves the arrow (clamped to the framebuffer),
/// drives the active drag вЂ” title-bar move or edge/corner resize вЂ” and
/// dispatches clicks (icons / title-bar buttons / task bar / window body).
/// A released button ends the drag, logging the final size after a resize;
/// minimize/maximize clicks log their action and resulting geometry.
pub fn on_mouse(dx: i32, dy: i32, buttons: u8) {
    let Some(fb) = console::framebuffer() else {
        return;
    };
    unsafe {
        if !*core::ptr::addr_of!(CUR_VIS) {
            *core::ptr::addr_of_mut!(CUR_X) = fb.width() / 2;
            *core::ptr::addr_of_mut!(CUR_Y) = fb.height() / 2;
            *core::ptr::addr_of_mut!(CUR_VIS) = true;
        }
        let ox = *core::ptr::addr_of!(CUR_X);
        let oy = *core::ptr::addr_of!(CUR_Y);
        let max_x = (fb.width() as i32).saturating_sub(8);
        let max_y = (fb.height() as i32).saturating_sub(8);
        let nx = (ox as i32 + dx).clamp(0, max_x) as usize;
        let ny = (oy as i32 + dy).clamp(0, max_y) as usize;
        let moved = nx != ox || ny != oy;
        *core::ptr::addr_of_mut!(CUR_X) = nx;
        *core::ptr::addr_of_mut!(CUR_Y) = ny;
        if moved {
            if let Some((kind, mode)) = *core::ptr::addr_of!(DRAG) {
                // Keyed by kind: `focus_window` at drag start may shift slots.
                let idx = (0..MAX_WINS).find(|&i| {
                    (*core::ptr::addr_of!(WINS))[i]
                        .map(|w| w.kind == kind)
                        .unwrap_or(false)
                });
                if let Some(i) = idx {
                    match mode {
                        Drag::Move => {
                            let wins = &mut *core::ptr::addr_of_mut!(WINS);
                            if let Some(win) = wins[i].as_mut() {
                                let max_x = fb.width().saturating_sub(win.w.min(fb.width()));
                                let max_y = fb.height().saturating_sub(win.h + TASKBAR_H);
                                let old = Rect {
                                    x0: win.x,
                                    y0: win.y,
                                    x1: win.x + win.w,
                                    y1: win.y + win.h,
                                };
                                win.x = (win.x as i32 + dx).clamp(0, max_x as i32) as usize;
                                win.y = (win.y as i32 + dy).clamp(0, max_y as i32) as usize;
                                dirty_rect(old.x0, old.y0, old.x1 - old.x0, old.y1 - old.y0);
                                dirty_rect(win.x, win.y, win.w, win.h);
                            }
                        }
                        Drag::Resize {
                            edges,
                            x0,
                            y0,
                            w0,
                            h0,
                            wx0,
                            wy0,
                        } => {
                            let wins = &mut *core::ptr::addr_of_mut!(WINS);
                            if let Some(win) = wins[i].as_mut() {
                                let ddx = nx as i32 - x0;
                                let ddy = ny as i32 - y0;
                                let fb_w = fb.width() as i32;
                                let fb_h = fb.height() as i32;
                                let lo_w = RESIZE_MIN_W as i32;
                                let lo_h = RESIZE_MIN_H as i32;
                                let mut w = w0 as i32;
                                let mut h = h0 as i32;
                                let mut x = wx0;
                                let mut y = wy0;
                                if edges & EDGE_L != 0 {
                                    let hi = (wx0 + w0 as i32).max(lo_w);
                                    w = (w0 as i32 - ddx).clamp(lo_w, hi);
                                    x = wx0 + w0 as i32 - w;
                                } else if edges & EDGE_R != 0 {
                                    let hi = (fb_w - wx0).max(lo_w);
                                    w = (w0 as i32 + ddx).clamp(lo_w, hi);
                                }
                                if edges & EDGE_T != 0 {
                                    let hi = (wy0 + h0 as i32).max(lo_h);
                                    h = (h0 as i32 - ddy).clamp(lo_h, hi);
                                    y = wy0 + h0 as i32 - h;
                                } else if edges & EDGE_B != 0 {
                                    let hi = (fb_h - TASKBAR_H as i32 - wy0).max(lo_h);
                                    h = (h0 as i32 + ddy).clamp(lo_h, hi);
                                }
                                let old = Rect {
                                    x0: win.x,
                                    y0: win.y,
                                    x1: win.x + win.w,
                                    y1: win.y + win.h,
                                };
                                win.x = x as usize;
                                win.y = y as usize;
                                win.w = w as usize;
                                win.h = h as usize;
                                dirty_rect(old.x0, old.y0, old.x1 - old.x0, old.y1 - old.y0);
                                dirty_rect(win.x, win.y, win.w, win.h);
                            }
                        }
                    }
                } else {
                    *core::ptr::addr_of_mut!(DRAG) = None;
                }
            }
            dirty_rect(ox, oy, 8, 8);
            dirty_rect(nx, ny, 8, 8);
            update_widget_hover();
        }
        let prev = *core::ptr::addr_of!(LAST_BTNS);
        let down = buttons & 0x01 != 0;
        if !down {
            if let Some((kind, mode)) = *core::ptr::addr_of!(DRAG) {
                if matches!(mode, Drag::Resize { .. }) {
                    log_resize(kind);
                }
            }
            *core::ptr::addr_of_mut!(DRAG) = None;
        }
        if down != (prev & 0x01 != 0) {
            *core::ptr::addr_of_mut!(LAST_BTNS) = buttons;
            if down {
                if modal_active() {
                    modal_click(fb, nx, ny);
                } else {
                    dispatch_click(fb, nx, ny);
                }
            }
        }
    }
}

fn hit_title(win: &Window, x: usize, y: usize) -> bool {
    y >= win.y && y < win.y + TITLE_H && x >= win.x && x < win.x + win.w.saturating_sub(TITLE_BTNS)
}

/// Title-bar button under `(x, y)`: the three-button cluster (minimize,
/// maximize/restore, close) glued to the window's right edge.
fn title_btn(win: &Window, x: usize, y: usize) -> Option<TitleBtn> {
    if y < win.y || y >= win.y + TITLE_H {
        return None;
    }
    let right = win.x + win.w;
    if x >= right || x < right.saturating_sub(TITLE_BTNS) {
        return None;
    }
    let off = x - (right - TITLE_BTNS);
    Some(if off >= 32 {
        TitleBtn::Close
    } else if off >= 16 {
        TitleBtn::Maximize
    } else {
        TitleBtn::Minimize
    })
}

fn hit_body(win: &Window, x: usize, y: usize) -> bool {
    x >= win.x && x < win.x + win.w && y >= win.y + TITLE_H && y < win.y + win.h
}

/// Edge/corner hit zone for a resize drag: the [`RESIZE_BORDER`]-wide strip
/// along the window's outer edges (title bar top included; the close button
/// wins over the top-right corner because it is tested first).
fn hit_resize(win: &Window, x: usize, y: usize) -> Option<u8> {
    // A maximized window is pinned to the desktop; a minimized one is hidden.
    if win.maximized || win.minimized {
        return None;
    }
    if x < win.x || x >= win.x + win.w || y < win.y || y >= win.y + win.h {
        return None;
    }
    let mut edges = 0u8;
    if x - win.x < RESIZE_BORDER {
        edges |= EDGE_L;
    }
    if win.x + win.w - x <= RESIZE_BORDER {
        edges |= EDGE_R;
    }
    if y - win.y < RESIZE_BORDER {
        edges |= EDGE_T;
    }
    if win.y + win.h - y <= RESIZE_BORDER {
        edges |= EDGE_B;
    }
    if edges == 0 {
        None
    } else {
        Some(edges)
    }
}

fn hit_icon(x: usize, y: usize) -> Option<WinKind> {
    let icons = [
        (WinKind::System, 20, 40),
        (WinKind::Clock, 20, 108),
        (WinKind::About, 20, 176),
        (WinKind::Network, 20, 244),
        (WinKind::Tasks, 20, 312),
    ];
    for (kind, ix, iy) in icons {
        if x >= ix && x < ix + ICON_W && y >= iy && y < iy + ICON_H {
            return Some(kind);
        }
    }
    None
}

fn hit_taskbar(fb: &Framebuffer, x: usize, y: usize) -> Option<usize> {
    if y < fb.height().saturating_sub(TASKBAR_H) {
        return None;
    }
    let mut bx = 4;
    for i in taskbar_slots() {
        if i == usize::MAX {
            break;
        }
        if let Some(win) = unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref() } {
            let w = 8 + win.title.len() * console::GLYPH_W;
            if x >= bx && x < bx + w {
                return Some(i);
            }
            bx += w + 4;
        }
    }
    None
}

/// Whether `(x, y)` falls on the tray link globe at the task bar's right end
/// (a 10 px dot inside the last 18 px of the strip); it opens the Network
/// window on click.
fn hit_globe(fb: &Framebuffer, x: usize, y: usize) -> bool {
    y >= fb.height().saturating_sub(TASKBAR_H) && x + 18 >= fb.width()
}

/// Serial label of a window: clients are addressed by id, everything else
/// carries its title.
fn win_label(kind: WinKind, title: &'static str) -> String {
    match kind {
        WinKind::Client(id) => format!("client {}", id),
        _ => String::from(title),
    }
}

/// Buttons of the Welcome window (v2.38.32 widget demo): a quick-launch row
/// for the three built-in apps plus a note-safe close button. The body text
/// block ends at `TITLE_H + 8 + 4 * GLYPH_H = 90` px below the window top,
/// so row one starts at 102 and row two at 132 вЂ” the 170 px window still
/// keeps a 14 px bottom margin.
static WELCOME_WIDGETS: [Widget; 4] = [
    Widget::Button {
        x: 8,
        y: 102,
        w: 112,
        h: 24,
        label: "System",
        action: WidgetAction::Open(WinKind::System),
    },
    Widget::Button {
        x: 126,
        y: 102,
        w: 112,
        h: 24,
        label: "Uptime",
        action: WidgetAction::Open(WinKind::Clock),
    },
    Widget::Button {
        x: 8,
        y: 132,
        w: 96,
        h: 24,
        label: "About",
        action: WidgetAction::Open(WinKind::About),
    },
    Widget::Button {
        x: 110,
        y: 132,
        w: 96,
        h: 24,
        label: "Close",
        action: WidgetAction::CloseHost,
    },
];

/// Widget table of the Network window (v2.38.34): seven input fields and
/// seven command buttons, laid out on the 16 px body grid of the 420x356
/// window вЂ” mode row at 104, address fields at 128..192, apply/test at 208,
/// Wi-Fi fields at 296/312 and the scan/connect row at 328.
static NETWORK_WIDGETS: [Widget; 14] = [
    Widget::Button {
        x: 80,
        y: 104,
        w: 72,
        h: 16,
        label: "DHCP",
        action: WidgetAction::Net(NetBtn::Dhcp),
    },
    Widget::Button {
        x: 160,
        y: 104,
        w: 104,
        h: 16,
        label: "Static",
        action: WidgetAction::Net(NetBtn::Static),
    },
    Widget::Button {
        x: 80,
        y: 128,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(0),
    },
    Widget::Button {
        x: 80,
        y: 144,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(1),
    },
    Widget::Button {
        x: 80,
        y: 160,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(2),
    },
    Widget::Button {
        x: 80,
        y: 176,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(3),
    },
    Widget::Button {
        x: 80,
        y: 192,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(4),
    },
    Widget::Button {
        x: 80,
        y: 208,
        w: 88,
        h: 16,
        label: "Apply",
        action: WidgetAction::Net(NetBtn::Apply),
    },
    Widget::Button {
        x: 176,
        y: 208,
        w: 72,
        h: 16,
        label: "Test",
        action: WidgetAction::Net(NetBtn::Test),
    },
    Widget::Button {
        x: 80,
        y: 296,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(5),
    },
    Widget::Button {
        x: 80,
        y: 312,
        w: 252,
        h: 16,
        label: "",
        action: WidgetAction::NetField(6),
    },
    Widget::Button {
        x: 80,
        y: 328,
        w: 72,
        h: 16,
        label: "Scan",
        action: WidgetAction::Net(NetBtn::Scan),
    },
    Widget::Button {
        x: 160,
        y: 328,
        w: 120,
        h: 16,
        label: "Connect",
        action: WidgetAction::Net(NetBtn::Connect),
    },
    Widget::Button {
        x: 288,
        y: 328,
        w: 72,
        h: 16,
        label: "Disc",
        action: WidgetAction::Net(NetBtn::Disc),
    },
];

/// Widget table of the Task Manager window (v2.38.35): the classic Windows
/// pair anchored bottom-left of the 440x248 window, below the task list and
/// the count/selection lines (`End Task` closes the selected row through
/// [`close_window`], `Switch To` raises it).
static TASK_WIDGETS: [Widget; 2] = [
    Widget::Button {
        x: 8,
        y: 216,
        w: 96,
        h: 24,
        label: "End Task",
        action: WidgetAction::Task(TaskBtn::End),
    },
    Widget::Button {
        x: 112,
        y: 216,
        w: 112,
        h: 24,
        label: "Switch To",
        action: WidgetAction::Task(TaskBtn::Switch),
    },
];

/// Widget table of a window kind; clients and built-ins without a demo table
/// get an empty slice.
fn widgets_for(kind: WinKind) -> &'static [Widget] {
    if kind == WinKind::Welcome {
        &WELCOME_WIDGETS
    } else if kind == WinKind::Network {
        &NETWORK_WIDGETS
    } else if kind == WinKind::Tasks {
        &TASK_WIDGETS
    } else {
        &[]
    }
}

/// Index of the widget of `win` under the absolute point `(x, y)`, if any.
fn widget_index_at(win: &Window, x: usize, y: usize) -> Option<usize> {
    widgets_for(win.kind).iter().position(|wg| {
        let Widget::Button {
            x: bx,
            y: by,
            w: bw,
            h: bh,
            ..
        } = *wg;
        x >= win.x + bx && x < win.x + bx + bw && y >= win.y + by && y < win.y + by + bh
    })
}

/// The widget that would take a click at `(x, y)`, honouring the same
/// topmost-wins rule as [`dispatch_click`]: the first window containing the
/// point either yields its body widget or вЂ” when the point falls on its
/// title/edges вЂ” nothing, never falling through to a window underneath.
fn hover_widget(x: usize, y: usize) -> Option<(WinKind, usize)> {
    for i in (0..MAX_WINS).rev() {
        let Some(win) = (unsafe { &(*core::ptr::addr_of!(WINS))[i] }) else {
            continue;
        };
        if win.minimized {
            continue;
        }
        if x < win.x || x >= win.x + win.w || y < win.y || y >= win.y + win.h {
            continue;
        }
        if !hit_body(win, x, y) {
            return None;
        }
        return widget_index_at(win, x, y).map(|wi| (win.kind, wi));
    }
    None
}

/// Recomputes [`WIDGET_HOVER`] after a cursor move and dirties the host
/// windows whose highlight state changed, so hovering a plate repaints it.
/// While a modal dialog is up no body widget may light up.
fn update_widget_hover() {
    let new = if modal_active() {
        None
    } else {
        let (x, y) = unsafe { (*core::ptr::addr_of!(CUR_X), *core::ptr::addr_of!(CUR_Y)) };
        hover_widget(x, y)
    };
    let old = unsafe { *core::ptr::addr_of!(WIDGET_HOVER) };
    if old == new {
        return;
    }
    for (kind, _) in [old, new].into_iter().flatten() {
        let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
        for win in wins.iter().flatten() {
            if win.kind == kind {
                dirty_window(win);
                break;
            }
        }
    }
    unsafe {
        *core::ptr::addr_of_mut!(WIDGET_HOVER) = new;
    }
}

/// Executes a clicked widget: logs the `[gui] widget <host>/<label> click`
/// serial proof line, then runs the action вЂ” open/focus a built-in window, or
/// close the hosting window through [`close_window`] (modal gate included).
/// The host index may have gone stale after [`focus_window`], so the action
/// resolves the target window by its [`WinKind`] instead.
fn widget_click(host: WinKind, host_label: &str, wg: Widget) {
    let Widget::Button { label, action, .. } = wg;
    crate::kprintln!("[gui] widget {}/{} click", host_label, label);
    match action {
        WidgetAction::Open(kind) => open_or_focus(kind),
        WidgetAction::CloseHost => {
            if let Some(i) = find_kind(host) {
                close_window(i);
            }
        }
        WidgetAction::Net(btn) => {
            net_button(btn);
            dirty_kind(host);
        }
        WidgetAction::NetField(f) => {
            unsafe {
                (*core::ptr::addr_of_mut!(NET_DRAFT)).focus = f;
            }
            dirty_kind(host);
        }
        WidgetAction::Task(TaskBtn::End) => task_end(),
        WidgetAction::Task(TaskBtn::Switch) => task_switch(),
    }
}

/// Marks every open window of `kind` for repaint.
fn dirty_kind(kind: WinKind) {
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    for win in wins.iter().flatten() {
        if win.kind == kind {
            dirty_window(win);
        }
    }
}

/// Paints the host window's widget table over its body content: each plate
/// uses [`BTN_BG`], lifted to [`BAR_ON`] while [`WIDGET_HOVER`] points at it,
/// with the label centered and clipped to the plate's right edge.
fn draw_widgets(fb: &Framebuffer, win: &Window) {
    let hover = unsafe { *core::ptr::addr_of!(WIDGET_HOVER) };
    for (wi, wg) in widgets_for(win.kind).iter().enumerate() {
        let Widget::Button {
            x,
            y,
            w,
            h,
            label,
            action,
        } = *wg;
        let (ax, ay) = (win.x + x, win.y + y);
        if let WidgetAction::NetField(field) = action {
            let focused = win.kind == WinKind::Network
                && unsafe { (*core::ptr::addr_of!(NET_DRAFT)).focus } == field;
            let border = if focused {
                BAR_ON
            } else if hover == Some((win.kind, wi)) {
                TEXT
            } else {
                TEXT_DIM
            };
            unsafe {
                fb.fill_rect(ax, ay, w, 1, border);
                fb.fill_rect(ax, ay + h - 1, w, 1, border);
                fb.fill_rect(ax, ay, 1, h, border);
                fb.fill_rect(ax + w - 1, ay, 1, h, border);
            }
            continue;
        }
        let plate = if hover == Some((win.kind, wi)) {
            BAR_ON
        } else {
            BTN_BG
        };
        unsafe {
            fb.fill_rect(ax, ay, w, h, plate);
        }
        let tx = ax + w.saturating_sub(label.len() * console::GLYPH_W) / 2;
        let ty = ay + h.saturating_sub(console::GLYPH_H) / 2;
        tui::draw_text(fb, tx, ty, label, TEXT, plate, (ax + w).min(fb.width()));
    }
}

/// Serial proof line for a finished resize drag: final geometry of the
/// resized window, addressed by its stable [`WinKind`].
fn log_resize(kind: WinKind) {
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    if let Some(win) = wins.iter().flatten().find(|w| w.kind == kind) {
        crate::kprintln!(
            "[gui] resize {} -> {}x{}",
            win_label(kind, win.title),
            win.w,
            win.h
        );
    }
}

fn dispatch_click(fb: &Framebuffer, x: usize, y: usize) {
    if hit_globe(fb, x, y) {
        crate::kprintln!("[gui] tray globe click");
        open_or_focus(WinKind::Network);
        return;
    }
    if let Some(i) = hit_taskbar(fb, x, y) {
        focus_window(i);
        return;
    }
    if let Some(kind) = hit_icon(x, y) {
        let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
        for (i, win) in wins.iter().enumerate() {
            if let Some(win) = win {
                if win.kind == kind {
                    focus_window(i);
                    return;
                }
            }
        }
        spawn(kind);
        return;
    }
    let mut hit_any = false;
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    for (i, win) in wins.iter().enumerate().rev() {
        if let Some(win) = win {
            if win.minimized {
                continue;
            }
            if let Some(btn) = title_btn(win, x, y) {
                match btn {
                    TitleBtn::Close => close_window(i),
                    TitleBtn::Maximize => toggle_maximize(win.kind),
                    TitleBtn::Minimize => minimize_window(win.kind),
                }
                hit_any = true;
                break;
            }
            if let Some(edges) = hit_resize(win, x, y) {
                unsafe {
                    *core::ptr::addr_of_mut!(DRAG) = Some((
                        win.kind,
                        Drag::Resize {
                            edges,
                            x0: x as i32,
                            y0: y as i32,
                            w0: win.w,
                            h0: win.h,
                            wx0: win.x as i32,
                            wy0: win.y as i32,
                        },
                    ));
                }
                focus_window(i);
                hit_any = true;
                break;
            }
            if hit_title(win, x, y) {
                // A maximized window is pinned: the title click only focuses.
                if !win.maximized {
                    unsafe {
                        *core::ptr::addr_of_mut!(DRAG) = Some((win.kind, Drag::Move));
                    }
                }
                focus_window(i);
                hit_any = true;
                break;
            }
            if hit_body(win, x, y) {
                // Widget plates swallow the click first (v2.38.32), no client
                // event. Captured before `focus_window` moves slots around вЂ”
                // see `cycle_focus`.
                if let Some(wi) = widget_index_at(win, x, y) {
                    let kind = win.kind;
                    let wg = widgets_for(kind)[wi];
                    let label = win_label(kind, win.title);
                    // An Open action focuses its target through
                    // `open_or_focus`; focusing the host first would lift it
                    // above the reopened window and mirror the restored
                    // z-rank (see `place_window`). Everything else behaves
                    // like an ordinary body click and focuses the host.
                    if !matches!(
                        wg,
                        Widget::Button {
                            action: WidgetAction::Open(_),
                            ..
                        }
                    ) {
                        focus_window(i);
                    }
                    widget_click(kind, &label, wg);
                    hit_any = true;
                    break;
                }
                // Task Manager rows select on click (v2.38.35); the button
                // plates above already swallowed their own clicks.
                if win.kind == WinKind::Tasks {
                    task_row_click(win, x, y);
                }
                if let WinKind::Client(id) = win.kind {
                    let rel_x = x - win.x;
                    let rel_y = y - (win.y + TITLE_H);
                    post_client_event(id, ev_click(rel_x, rel_y));
                }
                focus_window(i);
                hit_any = true;
                break;
            }
        }
    }
    if !hit_any {
        unfocus_all();
    }
}

/// Repaints only the regions dirtied since the last call and then publishes
/// them to the screen in one atomic push per dirty row.
///
/// Every draw primitive runs into the RAM backbuffer when it exists (a dense
/// software frame with the same pixel format as VRAM); once the frame is
/// painted, the damaged rectangle is blitted into the physical framebuffer with
/// `blit_region` (a single `copy_nonoverlapping` per scanline). VRAM is
/// therefore never drawn incrementally: the panel either sees the previous
/// complete frame or the new complete one, which вЂ” together with the damage
/// culling вЂ” removes both the full-screen flicker and torn rows. If the
/// backbuffer is unavailable the renderer falls back to painting directly into
/// VRAM (identical damage logic, v2.38.23 behaviour).
///
/// The partial path repaints desktop, then intersecting icons, the task-bar
/// strip (redrawn whole if touched), then every window whose rectangle overlaps
/// the damage (drawn bottom-up so the z-order stays correct), then the arrow
/// last. Idle repaints are therefore limited to the live telemetry windows
/// instead of the whole screen.
pub fn render() {
    let Some(vram) = console::framebuffer() else {
        return;
    };
    let d = unsafe { *core::ptr::addr_of!(DIRTY) };
    let full = d.x0 == 0 && d.y0 == 0 && d.x1 >= vram.width() && d.y1 >= vram.height();
    let (rx0, ry0, rw) = if full {
        (0usize, 0usize, vram.width())
    } else {
        let w = d.x1.min(vram.width()).saturating_sub(d.x0);
        let h = d.y1.min(vram.height()).saturating_sub(d.y0);
        if w == 0 || h == 0 {
            mark_live_dirty();
            return;
        }
        (d.x0, d.y0, w)
    };
    let rh = if full {
        vram.height()
    } else {
        d.y1.min(vram.height()).saturating_sub(d.y0)
    };
    let back = back_fb(vram);
    let target: &Framebuffer = back.as_ref().unwrap_or(vram);
    unsafe {
        target.fill_rect(rx0, ry0, rw, rh, DESK_BG);
    }
    draw_icons(target, &d);
    let task_y = vram.height().saturating_sub(TASKBAR_H);
    if full || rect_overlaps(&d, 0, task_y, vram.width(), TASKBAR_H) {
        draw_taskbar(target);
    }
    for i in 0..MAX_WINS {
        let win = unsafe { &(*core::ptr::addr_of!(WINS))[i] };
        if let Some(win) = win {
            if win.minimized {
                continue;
            }
            if full || rect_overlaps(&d, win.x, win.y, win.w, win.h) {
                draw_window(target, win);
            }
        }
    }
    // The modal dialog paints above windows and the task bar; it is
    // repainted on any partial damage that overlaps its rectangle (the
    // desktop fill would otherwise erase it under the live-window refresh).
    if let Some((kind, title)) = unsafe { core::ptr::addr_of!(MODAL).read() } {
        let (mx, my) = modal_pos(target);
        if full || rect_overlaps(&d, mx, my, DLG_W, DLG_H) {
            draw_modal(target, kind, title);
        }
    }
    unsafe {
        if *core::ptr::addr_of!(CUR_VIS)
            && (full
                || rect_overlaps(
                    &d,
                    *core::ptr::addr_of!(CUR_X),
                    *core::ptr::addr_of!(CUR_Y),
                    8,
                    8,
                ))
        {
            tui::paint_cursor(
                target,
                *core::ptr::addr_of!(CUR_X),
                *core::ptr::addr_of!(CUR_Y),
                true,
            );
        }
    }
    if let Some(back) = back.as_ref() {
        unsafe {
            vram.blit_region(back, rx0, ry0, rx0 + rw, ry0 + rh);
        }
    }
    unsafe {
        *core::ptr::addr_of_mut!(DIRTY) = Rect {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        };
    }
    mark_live_dirty();
}

/// Re-dirties the live telemetry windows (System / Uptime) after every repaint,
/// so the idle ~20 Hz refresh keeps updating them without touching the rest of
/// the desktop.
fn mark_live_dirty() {
    for i in 0..MAX_WINS {
        if let Some(win) = unsafe { &(*core::ptr::addr_of!(WINS))[i] } {
            match win.kind {
                WinKind::System | WinKind::Clock | WinKind::Network => dirty_window(win),
                _ => {}
            }
        }
    }
    if let Some(fb) = console::framebuffer() {
        dirty_rect(
            0,
            fb.height().saturating_sub(TASKBAR_H),
            fb.width(),
            TASKBAR_H,
        );
    }
}

fn icon_label(kind: WinKind) -> &'static str {
    match kind {
        WinKind::Welcome => "Welcome",
        WinKind::System => "System",
        WinKind::Clock => "Uptime",
        WinKind::About => "About",
        WinKind::Network => "Net",
        WinKind::Tasks => "Tasks",
        WinKind::Client(_) => "ring3",
    }
}

fn draw_icons(fb: &Framebuffer, clip: &Rect) {
    let icons = [
        (WinKind::System, 20, 40),
        (WinKind::Clock, 20, 108),
        (WinKind::About, 20, 176),
        (WinKind::Network, 20, 244),
        (WinKind::Tasks, 20, 312),
    ];
    for (kind, ix, iy) in icons {
        if !rect_overlaps(clip, ix, iy, ICON_W, ICON_H) {
            continue;
        }
        unsafe {
            fb.fill_rect(ix, iy, ICON_W, ICON_H, 0x00_0e_16_28);
            fb.fill_rect(ix, iy, ICON_W, 5, ICON_ACC);
        }
        tui::draw_text(
            fb,
            ix + 8,
            iy + 24,
            icon_label(kind),
            TEXT,
            0x00_0e_16_28,
            ix + ICON_W,
        );
        tui::draw_text(
            fb,
            ix + 8,
            iy + 40,
            "open",
            TEXT_DIM,
            0x00_0e_16_28,
            ix + ICON_W,
        );
    }
}

fn draw_taskbar(fb: &Framebuffer) {
    let y = fb.height().saturating_sub(TASKBAR_H);
    unsafe {
        fb.fill_rect(0, y, fb.width(), TASKBAR_H, BAR_BG);
    }
    let mut bx = 4;
    for i in taskbar_slots() {
        if i == usize::MAX {
            break;
        }
        if let Some(win) = unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref() } {
            let title = win_live_title(win);
            let w = 8 + title.len() * console::GLYPH_W;
            let bg = if win.focused {
                BAR_ON
            } else if win.minimized {
                BAR_MIN
            } else {
                BAR_BG
            };
            unsafe {
                fb.fill_rect(bx, y, w, TASKBAR_H, bg);
            }
            tui::draw_text(fb, bx + 4, y, title, TEXT, bg, bx + w);
            bx += w + 4;
        }
    }
    let gx = fb.width().saturating_sub(14);
    let color = globe_color();
    unsafe {
        fb.fill_rect(gx, y + (TASKBAR_H - 10) / 2, 10, 10, color);
    }
}

/// Colour of the tray link globe for the current [`crate::net::State`]:
/// grey (no NIC), red (no link), orange (link only), yellow (local),
/// blinking cyan (probing) or green (internet reachable).
fn globe_color() -> Color {
    use crate::net::State;
    match crate::net::state() {
        State::NoNic => 0x00_60_68_70,
        State::NoLink => 0x00_d0_30_30,
        State::LinkOnly => 0x00_e0_80_00,
        State::Local => 0x00_e0_d0_20,
        State::Checking => {
            let t = TICKS.load(Ordering::Relaxed) / 5;
            if t.is_multiple_of(2) {
                0x00_40_c0_e0
            } else {
                0x00_18_40_50
            }
        }
        State::Internet => 0x00_30_c0_60,
    }
}

fn draw_window(fb: &Framebuffer, win: &Window) {
    let title_bg = if win.focused { TITLE_ON } else { TITLE_OFF };
    let title = win_live_title(win);
    unsafe {
        fb.fill_rect(win.x, win.y, win.w, TITLE_H, title_bg);
        fb.fill_rect(win.x, win.y + TITLE_H, win.w, win.h - TITLE_H, WIN_BG);
    }
    // Body text clips at the window's own right edge (resize-safe); the
    // title text additionally stops before the three-button cluster.
    let edge = (win.x + win.w).min(fb.width());
    let max_px = edge.saturating_sub(6);
    let title_max = edge.min(win.x + win.w.saturating_sub(TITLE_BTNS + 2));
    tui::draw_text(fb, win.x + 6, win.y + 1, title, TEXT, title_bg, title_max);
    // Right-anchored cluster: minimize [w-48,w-32), maximize [w-32,w-16),
    // close [w-16,w).
    let bx = win.x + win.w.saturating_sub(TITLE_BTNS);
    unsafe {
        fb.fill_rect(bx, win.y, TITLE_BTNS, TITLE_H, BTN_BG);
    }
    let mx = bx + TITLE_BTN;
    unsafe {
        fb.fill_rect(mx, win.y, TITLE_BTN, TITLE_H, CLOSE_BG);
    }
    // Minimize: a low bar.
    unsafe {
        fb.fill_rect(bx + 3, win.y + 11, 10, 2, TEXT);
    }
    // Maximize: a plain square outline; restore: two overlapping squares.
    if win.maximized {
        unsafe {
            fb.fill_rect(mx + 3, win.y + 8, 7, 1, TEXT);
            fb.fill_rect(mx + 3, win.y + 14, 7, 1, TEXT);
            fb.fill_rect(mx + 3, win.y + 8, 1, 7, TEXT);
            fb.fill_rect(mx + 9, win.y + 8, 1, 7, TEXT);
            fb.fill_rect(mx + 6, win.y + 3, 8, 1, TEXT_DIM);
            fb.fill_rect(mx + 6, win.y + 9, 8, 1, TEXT_DIM);
            fb.fill_rect(mx + 6, win.y + 3, 1, 7, TEXT_DIM);
            fb.fill_rect(mx + 13, win.y + 3, 1, 7, TEXT_DIM);
        }
    } else {
        unsafe {
            fb.fill_rect(mx + 3, win.y + 4, 10, 1, TEXT);
            fb.fill_rect(mx + 3, win.y + 13, 10, 1, TEXT);
            fb.fill_rect(mx + 3, win.y + 4, 1, 10, TEXT);
            fb.fill_rect(mx + 12, win.y + 4, 1, 10, TEXT);
        }
    }
    // Close: the 'X' glyph on its red plate.
    let cx = win.x + win.w.saturating_sub(TITLE_BTN);
    let mut px = cx;
    for byte in b"X" {
        tui::draw_glyph(fb, px, win.y + 1, *byte, TEXT, CLOSE_BG);
        px += console::GLYPH_W;
    }
    let mut y = win.y + TITLE_H + 8;
    match win.kind {
        WinKind::Welcome => draw_welcome(fb, win, &mut y, max_px),
        WinKind::System => draw_system(fb, win.x + 8, &mut y, max_px),
        WinKind::Clock => draw_clock(fb, win.x + 8, &mut y, max_px),
        WinKind::About => draw_about(fb, win.x + 8, &mut y, max_px),
        WinKind::Network => draw_network(fb, win, max_px),
        WinKind::Tasks => draw_tasks(fb, win, max_px),
        WinKind::Client(_) => draw_client(fb, win),
    }
    draw_widgets(fb, win);
}

/// Paints the modal confirm dialog: title bar, the close question, the hint
/// lines and the centered `OK` / `Cancel` pair вЂ” [`modal_click`] hits exactly
/// these two rectangles.
fn draw_modal(fb: &Framebuffer, kind: WinKind, title: &'static str) {
    let (x, y) = modal_pos(fb);
    let max = x + DLG_W - 6;
    unsafe {
        fb.fill_rect(x, y, DLG_W, DLG_H, WIN_BG);
        fb.fill_rect(x, y, DLG_W, TITLE_H, TITLE_ON);
    }
    tui::draw_text(fb, x + 6, y + 1, "Confirm", TEXT, TITLE_ON, max);
    let question = format!("Close {}?", win_label(kind, title));
    let mut ly = y + TITLE_H + 8;
    tui::draw_text(fb, x + 12, ly, &question, TEXT, WIN_BG, max);
    ly += console::GLYPH_H + 6;
    tui::draw_text(
        fb,
        x + 12,
        ly,
        "Typed text will be lost.",
        TEXT_DIM,
        WIN_BG,
        max,
    );
    ly += console::GLYPH_H + 6;
    tui::draw_text(
        fb,
        x + 12,
        ly,
        "Enter = close, Esc = cancel",
        TEXT_DIM,
        WIN_BG,
        max,
    );
    let ry = y + DLG_BTN_Y;
    unsafe {
        fb.fill_rect(x + DLG_OK_X, ry, DLG_OK_W, DLG_BTN_H, BAR_ON);
        fb.fill_rect(x + DLG_CANCEL_X, ry, DLG_CANCEL_W, DLG_BTN_H, BTN_BG);
    }
    modal_btn_label(fb, "OK", x + DLG_OK_X, ry, DLG_OK_W, BAR_ON);
    modal_btn_label(fb, "Cancel", x + DLG_CANCEL_X, ry, DLG_CANCEL_W, BTN_BG);
}

/// Centers a button label inside its plate (v2.38.30 dialog buttons).
fn modal_btn_label(
    fb: &Framebuffer,
    label: &'static str,
    rx: usize,
    ry: usize,
    rw: usize,
    bg: Color,
) {
    let tx = rx + (rw - label.len() * console::GLYPH_W) / 2;
    tui::draw_text(
        fb,
        tx,
        ry + (DLG_BTN_H - console::GLYPH_H) / 2,
        label,
        TEXT,
        bg,
        rx + rw,
    );
}

fn text_line(
    fb: &Framebuffer,
    x: usize,
    y: &mut usize,
    s: &str,
    fg: Color,
    bg: Color,
    max_px: usize,
) {
    tui::draw_text(fb, x, *y, s, fg, bg, max_px);
    *y += console::GLYPH_H;
}

fn draw_welcome(fb: &Framebuffer, win: &Window, y: &mut usize, max_px: usize) {
    let x = win.x + 8;
    text_line(
        fb,
        x,
        y,
        "This is the microkernel GUI.",
        TEXT,
        WIN_BG,
        max_px,
    );
    let msg = core::str::from_utf8(&win.note[..win.note_len]).unwrap_or("?");
    text_line(fb, x, y, &format!("message: {}", msg), TEXT, WIN_BG, max_px);
    text_line(
        fb,
        x,
        y,
        "mouse: buttons open windows, drag title to move",
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
    text_line(
        fb,
        x,
        y,
        "keys: Tab focus, Esc close/leave, text types here",
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}

fn uptime_hms() -> String {
    let secs = TICKS.load(Ordering::Relaxed) / TIMER_HZ;
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let ss = secs % 60;
    format!("{:02}:{:02}:{:02}", hh, mm, ss)
}

fn draw_system(fb: &Framebuffer, x: usize, y: &mut usize, max_px: usize) {
    let (sent, recv) = crate::syscalls::stats();
    text_line(
        fb,
        x,
        y,
        &format!(
            "tick {}  mode {}",
            TICKS.load(Ordering::Relaxed),
            tui::tick_mode()
        ),
        TEXT,
        WIN_BG,
        max_px,
    );
    text_line(
        fb,
        x,
        y,
        &format!(
            "switches {}  ipc s/r {}/{}",
            crate::sched::switch_count(),
            sent,
            recv
        ),
        TEXT,
        WIN_BG,
        max_px,
    );
    text_line(
        fb,
        x,
        y,
        &format!(
            "temp {}c  lid {}",
            crate::thermal::temp_c(),
            tui::lid_state()
        ),
        TEXT,
        WIN_BG,
        max_px,
    );
    text_line(
        fb,
        x,
        y,
        &format!(
            "key ps2 {}  usb {}  mouse id 0x{:02x}",
            crate::ps2::key_seq(),
            crate::xhci::KEY_SEQ.load(Ordering::Relaxed),
            crate::ps2::mouse_id()
        ),
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
    text_line(
        fb,
        x,
        y,
        &format!("frames {}", crate::memory::frames_allocated()),
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}

fn draw_clock(fb: &Framebuffer, x: usize, y: &mut usize, max_px: usize) {
    text_line(fb, x, y, &uptime_hms(), TEXT, WIN_BG, max_px);
    text_line(
        fb,
        x,
        y,
        &format!("ticks {}", TICKS.load(Ordering::Relaxed)),
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}

fn draw_about(fb: &Framebuffer, x: usize, y: &mut usize, max_px: usize) {
    text_line(fb, x, y, tui::VERSION, colors::OK, WIN_BG, max_px);
    text_line(
        fb,
        x,
        y,
        "windowed GUI on the raw framebuffer",
        TEXT,
        WIN_BG,
        max_px,
    );
    text_line(
        fb,
        x,
        y,
        "RAM backbuffer + dirty-rect blit to VRAM",
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}

/// Background of the Network window's input boxes (darker than [`WIN_BG`]).
const NET_BOX_BG: Color = 0x00_10_18_28;

/// Editable draft behind the Network window: addressing mode, the five
/// dotted-quad strings, the Wi-Fi credentials, the active field focus and
/// the last short button feedback word. Reloaded from the persisted settings
/// every time the window opens; the passphrase lives only here.
#[derive(Clone, Copy)]
struct NetDraft {
    /// `true` while the DHCP mode button is selected.
    dhcp: bool,
    ip: [u8; 16],
    ip_len: u8,
    mask: [u8; 16],
    mask_len: u8,
    gw: [u8; 16],
    gw_len: u8,
    dns1: [u8; 16],
    dns1_len: u8,
    dns2: [u8; 16],
    dns2_len: u8,
    ssid: [u8; 32],
    ssid_len: u8,
    pass: [u8; 64],
    pass_len: u8,
    /// Active field `0..=6` (`ip`, `mask`, `gw`, `dns1`, `dns2`, `ssid`,
    /// `pass`); [`NET_F_NONE`] means no field takes keys.
    focus: u8,
    /// Last button-feedback word shown right of the mode buttons.
    msg: [u8; 8],
    msg_len: u8,
}

/// Focus value for "no field selected".
const NET_F_NONE: u8 = 7;
/// Number of editable fields in [`NetDraft`].
const NET_FIELDS: u8 = 7;

static mut NET_DRAFT: NetDraft = NetDraft {
    dhcp: true,
    ip: [0; 16],
    ip_len: 0,
    mask: [0; 16],
    mask_len: 0,
    gw: [0; 16],
    gw_len: 0,
    dns1: [0; 16],
    dns1_len: 0,
    dns2: [0; 16],
    dns2_len: 0,
    ssid: [0; 32],
    ssid_len: 0,
    pass: [0; 64],
    pass_len: 0,
    focus: NET_F_NONE,
    msg: [0; 8],
    msg_len: 0,
};

/// Rebuilds the draft from the persisted settings (fresh SSID/pass and an
/// empty feedback word); called whenever the Network window opens.
fn net_draft_load() {
    let s = crate::net::settings();
    let mut d = NetDraft {
        dhcp: s.dhcp,
        ip: [0; 16],
        ip_len: 0,
        mask: [0; 16],
        mask_len: 0,
        gw: [0; 16],
        gw_len: 0,
        dns1: [0; 16],
        dns1_len: 0,
        dns2: [0; 16],
        dns2_len: 0,
        ssid: [0; 32],
        ssid_len: 0,
        pass: [0; 64],
        pass_len: 0,
        focus: NET_F_NONE,
        msg: [0; 8],
        msg_len: 0,
    };
    d.ip_len = crate::net::fmt_ip(s.ip, &mut d.ip) as u8;
    d.mask_len = crate::net::fmt_ip(s.mask, &mut d.mask) as u8;
    d.gw_len = crate::net::fmt_ip(s.gw, &mut d.gw) as u8;
    d.dns1_len = crate::net::fmt_ip(s.dns1, &mut d.dns1) as u8;
    d.dns2_len = crate::net::fmt_ip(s.dns2, &mut d.dns2) as u8;
    unsafe {
        *core::ptr::addr_of_mut!(NET_DRAFT) = d;
    }
}

/// Parses a dotted quad (`"192.168.1.10"`) into four octets.
fn parse_quad(buf: &[u8], len: u8) -> Option<[u8; 4]> {
    let s = &buf[..len as usize];
    let mut out = [0u8; 4];
    let mut idx = 0usize;
    let mut val = 0u32;
    let mut digits = false;
    for &b in s {
        if b == b'.' {
            if !digits || idx >= 3 {
                return None;
            }
            out[idx] = val as u8;
            idx += 1;
            val = 0;
            digits = false;
        } else if b.is_ascii_digit() {
            val = val * 10 + u32::from(b - b'0');
            if val > 255 {
                return None;
            }
            digits = true;
        } else {
            return None;
        }
    }
    if !digits || idx != 3 {
        return None;
    }
    out[3] = val as u8;
    Some(out)
}

/// Stores the short feedback word shown right of the mode buttons.
fn net_set_msg(s: &str) {
    let d = unsafe { &mut *core::ptr::addr_of_mut!(NET_DRAFT) };
    let n = s.len().min(d.msg.len());
    d.msg[..n].copy_from_slice(&s.as_bytes()[..n]);
    d.msg_len = n as u8;
}

/// Appends one typed byte to a draft field (`quad` caps dotted quads at the
/// 15 characters of `"255.255.255.255"`).
fn net_append(buf: &mut [u8], len: &mut u8, b: u8, quad: bool) {
    let cap = if quad { 15 } else { buf.len() };
    let n = *len as usize;
    if n < cap {
        buf[n] = b;
        *len += 1;
    }
}

/// Types `c` into the focused draft field; dotted-quad fields accept digits
/// and dots only, credentials take any printable byte.
fn net_type(c: char) {
    let u = c as u32;
    if !(0x20..0x7F).contains(&u) {
        return;
    }
    let b = u as u8;
    let d = unsafe { &mut *core::ptr::addr_of_mut!(NET_DRAFT) };
    let focus = d.focus;
    let quad = focus < 5;
    if quad && !b.is_ascii_digit() && b != b'.' {
        return;
    }
    match focus {
        0 => net_append(&mut d.ip, &mut d.ip_len, b, quad),
        1 => net_append(&mut d.mask, &mut d.mask_len, b, quad),
        2 => net_append(&mut d.gw, &mut d.gw_len, b, quad),
        3 => net_append(&mut d.dns1, &mut d.dns1_len, b, quad),
        4 => net_append(&mut d.dns2, &mut d.dns2_len, b, quad),
        5 => net_append(&mut d.ssid, &mut d.ssid_len, b, false),
        6 => net_append(&mut d.pass, &mut d.pass_len, b, false),
        _ => {}
    }
}

/// Deletes the last byte of the focused draft field (Backspace).
fn net_backspace() {
    let d = unsafe { &mut *core::ptr::addr_of_mut!(NET_DRAFT) };
    let len = match d.focus {
        0 => &mut d.ip_len,
        1 => &mut d.mask_len,
        2 => &mut d.gw_len,
        3 => &mut d.dns1_len,
        4 => &mut d.dns2_len,
        5 => &mut d.ssid_len,
        6 => &mut d.pass_len,
        _ => return,
    };
    if *len > 0 {
        *len -= 1;
    }
}

/// Moves the field focus forward (Tab): `0 -> 1 -> .. -> 6 -> 0`, and wraps
/// in from [`NET_F_NONE`].
fn net_focus_next() {
    let d = unsafe { &mut *core::ptr::addr_of_mut!(NET_DRAFT) };
    d.focus = if d.focus >= NET_FIELDS {
        0
    } else {
        (d.focus + 1) % NET_FIELDS
    };
}

/// Runs a Network window command button against the draft.
fn net_button(btn: NetBtn) {
    match btn {
        NetBtn::Dhcp => {
            unsafe {
                (*core::ptr::addr_of_mut!(NET_DRAFT)).dhcp = true;
            }
            net_set_msg("dhcp");
        }
        NetBtn::Static => {
            unsafe {
                (*core::ptr::addr_of_mut!(NET_DRAFT)).dhcp = false;
            }
            net_set_msg("static");
        }
        NetBtn::Apply => net_apply(),
        NetBtn::Test => {
            crate::net::recheck();
            net_set_msg("check!");
        }
        NetBtn::Scan => {
            crate::wifi::start_scan();
            net_set_msg("scan ok");
        }
        NetBtn::Connect => net_connect(),
        NetBtn::Disc => {
            crate::wifi::disconnect();
            net_set_msg("disc ok");
        }
    }
}

/// Parses the five address fields and pushes them through `net::apply`
/// (which also persists them to CMOS). Feedback: `applied` or the failing
/// field name; a static mode demands a non-zero `ip`.
fn net_apply() {
    let d = unsafe { *core::ptr::addr_of!(NET_DRAFT) };
    let Some(ip) = parse_quad(&d.ip, d.ip_len) else {
        net_set_msg("bad ip");
        return;
    };
    let Some(mask) = parse_quad(&d.mask, d.mask_len) else {
        net_set_msg("bad mask");
        return;
    };
    let Some(gw) = parse_quad(&d.gw, d.gw_len) else {
        net_set_msg("bad gw");
        return;
    };
    let Some(dns1) = parse_quad(&d.dns1, d.dns1_len) else {
        net_set_msg("bad dns1");
        return;
    };
    let Some(dns2) = parse_quad(&d.dns2, d.dns2_len) else {
        net_set_msg("bad dns2");
        return;
    };
    if !d.dhcp && ip == [0; 4] {
        net_set_msg("need ip");
        return;
    }
    crate::net::apply(&crate::net::Settings {
        dhcp: d.dhcp,
        ip,
        mask,
        gw,
        dns1,
        dns2,
    });
    net_set_msg("applied");
}

/// Starts a Wi-Fi connection to the drafted SSID/passphrase (`wait` on
/// accept, the radio's error text on refusal).
fn net_connect() {
    let d = unsafe { *core::ptr::addr_of!(NET_DRAFT) };
    if d.ssid_len == 0 {
        crate::kprintln!("[serial] [gui] net connect: no ssid");
        net_set_msg("no ssid");
        return;
    }
    match crate::wifi::connect(
        &d.ssid[..d.ssid_len as usize],
        &d.pass[..d.pass_len as usize],
    ) {
        Ok(()) => net_set_msg("wait"),
        Err(e) => {
            crate::kprintln!(
                "[serial] [gui] net connect err: {} (ssid len {})",
                e,
                d.ssid_len
            );
            let m = String::from(e);
            net_set_msg(&m);
        }
    }
}

/// Routes a make-code to the focused Network window's draft (v2.38.34):
/// Tab advances the field focus, Backspace deletes, Enter applies and
/// printable characters type into the active field. Returns `false` for
/// every key the normal window handling owns (Esc, F-keys, plain notes).
fn net_key(sc: u8) -> bool {
    let Some(i) = focused_idx() else {
        return false;
    };
    let is_net = unsafe {
        (*core::ptr::addr_of!(WINS))[i]
            .as_ref()
            .map(|w| w.kind == WinKind::Network)
            .unwrap_or(false)
    };
    if !is_net {
        return false;
    }
    match sc {
        0x0F => {
            net_focus_next();
            dirty_kind(WinKind::Network);
            true
        }
        0x0E => {
            net_backspace();
            dirty_kind(WinKind::Network);
            true
        }
        0x1C => {
            net_button(NetBtn::Apply);
            dirty_kind(WinKind::Network);
            true
        }
        _ => {
            let Some(c) = crate::interrupts::scancode_to_char(sc) else {
                return false;
            };
            if c.is_control() {
                return false;
            }
            net_type(c);
            dirty_kind(WinKind::Network);
            true
        }
    }
}

/// Paints the Network window body (v2.38.34): five status lines on the
/// 16 px grid from `y+24`, the mode row with its feedback word, the seven
/// field captions and values (the widget layer draws their borders), the
/// Wi-Fi status line and up to three scan rows. Everything clips at
/// `max_px`, i.e. the window's own right edge.
fn draw_network(fb: &Framebuffer, win: &Window, max_px: usize) {
    let x = win.x + 8;
    let d = unsafe { *core::ptr::addr_of!(NET_DRAFT) };
    let st = crate::net::state();
    let lease = crate::net::lease();
    let dhcp = crate::net::settings().dhcp;
    let mut buf = [0u8; 48];
    let at = |y: usize, s: &str, fg: Color| {
        tui::draw_text(fb, x, win.y + y, s, fg, WIN_BG, max_px);
    };

    at(
        24,
        &format!("nic {} {}", crate::net::nic_kind(), st.name()),
        TEXT,
    );
    let mac_s: &str = match crate::net::nic_mac() {
        Some(m) => {
            let n = crate::net::fmt_mac(m, &mut buf);
            core::str::from_utf8(&buf[..n]).unwrap_or("?")
        }
        None => "-",
    };
    at(40, &format!("mac {}", mac_s), TEXT_DIM);

    let mode = if dhcp { "dhcp" } else { "static" };
    let ip_line = match lease {
        Some(l) => {
            let n = crate::net::fmt_ip(l.ip, &mut buf);
            let dotted = core::str::from_utf8(&buf[..n]).unwrap_or("?");
            let mut bits = 0u8;
            for b in l.mask {
                bits += b.count_ones() as u8;
            }
            format!("ip {}/{} {}", dotted, bits, mode)
        }
        None => format!("ip - {}", mode),
    };
    at(56, &ip_line, TEXT);

    let gw_line = match lease {
        Some(l) => {
            let gn = crate::net::fmt_ip(l.gw, &mut buf);
            let gw = String::from_utf8_lossy(&buf[..gn]).into_owned();
            let dn = crate::net::fmt_ip(l.dns1, &mut buf);
            let dns = String::from_utf8_lossy(&buf[..dn]).into_owned();
            format!("gw {} dns {}", gw, dns)
        }
        None => String::from("gw - dns -"),
    };
    at(72, &gw_line, TEXT_DIM);

    use crate::net::State;
    let probe = match st {
        State::NoNic => "no controller",
        State::NoLink => "link down",
        State::LinkOnly => "no address",
        State::Local => "local only",
        State::Checking => "probing...",
        State::Internet => "internet ok",
    };
    at(88, &format!("check: {}", probe), TEXT_DIM);

    at(104, "mode", TEXT_DIM);
    if d.msg_len > 0 {
        let m = core::str::from_utf8(&d.msg[..d.msg_len as usize]).unwrap_or("");
        tui::draw_text(fb, win.x + 272, win.y + 104, m, TEXT, WIN_BG, max_px);
    }

    const CAPTIONS: [&str; 7] = ["ip", "mask", "gw", "dns1", "dns2", "ssid", "pass"];
    const ROWS: [usize; 7] = [128, 144, 160, 176, 192, 296, 312];
    let bx = win.x + 80;
    for i in 0..NET_FIELDS as usize {
        let focused = d.focus == i as u8;
        let cap_fg = if focused { TEXT } else { TEXT_DIM };
        tui::draw_text(fb, x, win.y + ROWS[i], CAPTIONS[i], cap_fg, WIN_BG, bx);
        unsafe {
            fb.fill_rect(bx, win.y + ROWS[i], 252, 16, NET_BOX_BG);
        }
        let (field, len): (&[u8], usize) = match i {
            0 => (&d.ip[..], d.ip_len as usize),
            1 => (&d.mask[..], d.mask_len as usize),
            2 => (&d.gw[..], d.gw_len as usize),
            3 => (&d.dns1[..], d.dns1_len as usize),
            4 => (&d.dns2[..], d.dns2_len as usize),
            5 => (&d.ssid[..], d.ssid_len as usize),
            _ => (&d.pass[..], d.pass_len as usize),
        };
        let val: String = if i == 6 {
            "*".repeat(len)
        } else {
            String::from_utf8_lossy(&field[..len]).into_owned()
        };
        let max_chars = 244 / console::GLYPH_W;
        let shown = if val.len() > max_chars {
            &val[val.len() - max_chars..]
        } else {
            &val
        };
        tui::draw_text(
            fb,
            bx + 4,
            win.y + ROWS[i],
            shown,
            if focused { TEXT } else { TEXT_DIM },
            NET_BOX_BG,
            bx + 252,
        );
    }

    let wn = crate::wifi::status_line(&mut buf);
    let ws = core::str::from_utf8(&buf[..wn]).unwrap_or("?");
    at(232, &format!("wifi {}", ws), TEXT);

    let (results, count) = crate::wifi::scan_results();
    if count == 0 {
        let hint = if crate::wifi::scanning() {
            "scanning..."
        } else {
            "no scan yet"
        };
        at(248, hint, TEXT_DIM);
    } else {
        let mut cbuf = [0u8; 32];
        let cn = crate::wifi::connected_ssid(&mut cbuf);
        for (k, r) in results.iter().enumerate().take(count.min(3)) {
            let ssid = core::str::from_utf8(&r.ssid[..r.ssid_len as usize]).unwrap_or("?");
            let assoc = crate::wifi::associated() && r.ssid[..r.ssid_len as usize] == cbuf[..cn];
            let ssid = &ssid[..ssid.len().min(10)];
            let sec = match r.security {
                crate::wifi::Security::Open => "open",
                crate::wifi::Security::Wpa2 => "wpa2",
            };
            at(
                248 + k * 16,
                &format!(
                    "{}{} {} {} {}",
                    if assoc { "*" } else { " " },
                    k + 1,
                    ssid,
                    sec,
                    r.rssi
                ),
                TEXT,
            );
        }
    }
}

/// Selects the task row under the click (v2.38.35): rows follow the task-bar
/// rank order and the selection is keyed by [`WinKind`], so it survives the
/// slot shifts of any later focus/z change and only goes stale when the
/// window closes. Clicks in the gaps (header, foot, past the last row) do
/// nothing.
fn task_row_click(win: &Window, x: usize, y: usize) {
    if x < win.x + 8 || x >= win.x + win.w.saturating_sub(8) {
        return;
    }
    if y < win.y + TASK_ROWS_Y {
        return;
    }
    let ry = y - (win.y + TASK_ROWS_Y);
    if ry >= TASK_ROW_H * MAX_WINS {
        return;
    }
    let row = ry / TASK_ROW_H;
    let slots = taskbar_slots();
    let Some(&i) = slots.get(row).filter(|&&i| i != usize::MAX) else {
        return;
    };
    let Some(w) = (unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref() }) else {
        return;
    };
    unsafe {
        *core::ptr::addr_of_mut!(TASK_SEL) = Some(w.kind);
    }
    crate::kprintln!("[gui] tasks select {}", win_label(w.kind, w.title));
    dirty_kind(WinKind::Tasks);
}

/// End Task button: closes the selected window through [`close_window`], so a
/// typed note still raises the modal confirm dialog first (the Windows "end
/// this program?" flow). Logs `no selection` / `<label> gone` when the action
/// cannot run, so a stale selection is provable instead of silent.
fn task_end() {
    let sel = unsafe { *core::ptr::addr_of!(TASK_SEL) };
    let Some(kind) = sel else {
        crate::kprintln!("[gui] tasks end: no selection");
        return;
    };
    let Some(i) = find_kind(kind) else {
        crate::kprintln!("[gui] tasks end: {} gone", win_label(kind, win_title(kind)));
        return;
    };
    crate::kprintln!("[gui] tasks end {}", win_label(kind, win_title(kind)));
    close_window(i);
}

/// Switch To button (and the Enter shortcut): focuses and raises the selected
/// window, logging `[gui] tasks switch <label>` as the serial proof.
fn task_switch() {
    let sel = unsafe { *core::ptr::addr_of!(TASK_SEL) };
    let Some(kind) = sel else {
        crate::kprintln!("[gui] tasks switch: no selection");
        return;
    };
    let Some(i) = find_kind(kind) else {
        crate::kprintln!(
            "[gui] tasks switch: {} gone",
            win_label(kind, win_title(kind))
        );
        return;
    };
    crate::kprintln!("[gui] tasks switch {}", win_label(kind, win_title(kind)));
    focus_window(i);
}

/// Enter (make-code 0x1C) on the focused Task Manager window runs
/// [`task_switch`]; every other scancode and every other focused window falls
/// through to the normal key routing untouched.
fn task_key(sc: u8) -> bool {
    if sc != 0x1C {
        return false;
    }
    let Some(i) = focused_idx() else {
        return false;
    };
    let is_tasks = unsafe {
        (*core::ptr::addr_of!(WINS))[i]
            .as_ref()
            .map(|w| w.kind == WinKind::Tasks)
            .unwrap_or(false)
    };
    if !is_tasks {
        return false;
    }
    task_switch();
    true
}

/// Paints the Task Manager body (v2.38.35): the Task/Status column header on
/// its own strip, one 16 px row per open window in task-bar rank order (the
/// selected row highlighted with [`BAR_ON`], status Running/Background/
/// Minimized), then the `N tasks` count and the current selection line. Row
/// clicks are resolved by [`task_row_click`]; the button plates come from
/// [`draw_widgets`].
fn draw_tasks(fb: &Framebuffer, win: &Window, max_px: usize) {
    let slots = taskbar_slots();
    let count = slots.iter().filter(|&&i| i != usize::MAX).count();
    let hx = win.x + 8;
    let hw = win.w.saturating_sub(16);
    unsafe {
        fb.fill_rect(hx, win.y + TASK_HDR_Y, hw, TASK_ROW_H, BAR_BG);
    }
    let col2 = (win.x + TASK_STATUS_X).min(max_px);
    tui::draw_text(
        fb,
        hx + 4,
        win.y + TASK_HDR_Y,
        "Task",
        TEXT_DIM,
        BAR_BG,
        col2,
    );
    tui::draw_text(
        fb,
        win.x + TASK_STATUS_X + 4,
        win.y + TASK_HDR_Y,
        "Status",
        TEXT_DIM,
        BAR_BG,
        max_px,
    );
    let sel = unsafe { *core::ptr::addr_of!(TASK_SEL) };
    for (row, &i) in slots.iter().enumerate() {
        if i == usize::MAX {
            break;
        }
        let Some(w) = (unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref() }) else {
            continue;
        };
        let y = win.y + TASK_ROWS_Y + row * TASK_ROW_H;
        let selected = sel == Some(w.kind);
        if selected {
            unsafe {
                fb.fill_rect(hx, y, hw, TASK_ROW_H, BAR_ON);
            }
        }
        let bg = if selected { BAR_ON } else { WIN_BG };
        tui::draw_text(fb, hx + 4, y, win_live_title(w), TEXT, bg, col2);
        let status = if w.focused {
            "Running"
        } else if w.minimized {
            "Minimized"
        } else {
            "Background"
        };
        tui::draw_text(
            fb,
            win.x + TASK_STATUS_X + 4,
            y,
            status,
            TEXT_DIM,
            bg,
            max_px,
        );
    }
    let foot = format!("{} tasks", count);
    tui::draw_text(fb, hx, win.y + TASK_FOOT_Y, &foot, TEXT_DIM, WIN_BG, max_px);
    let sel_line = match sel {
        Some(kind) => format!("sel {}", win_label(kind, win_title(kind))),
        None => String::from("sel -"),
    };
    tui::draw_text(
        fb,
        hx,
        win.y + TASK_FOOT_Y + TASK_ROW_H,
        &sel_line,
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}
