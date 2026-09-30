//! Bare-metal windowed GUI for the AIOS kernel.
//!
//! A full-screen graphical desktop that the kernel shell can switch to with the
//! `gui` command (back to the console/TUI with `tui`, or `Esc` with no window
//! focused). It replaces the console + interactive TUI while active: the
//! console's `vprintln!` calls are skipped so stray log lines cannot garble the
//! desktop, and the heartbeat square is paused. The screen owns a desktop
//! background, a left icon column (one tile per app), a bottom task bar and a
//! z-ordered list of windows with a title bar, close button and draggable body.
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
//! type into the focused `Welcome` window) and from the mouse via `on_mouse`
//! (icon/close/task-bar clicks, title-bar drag); the loop reads these through
//! lock-free atomics and never blocks on `inb`/spin for input.

use crate::framebuffer::{colors, Color, Framebuffer};
use crate::interrupts::{TICKS, TIMER_HZ};
use crate::{console, tui};
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

/// Ceiling on open windows (one slot per app instance).
pub const MAX_WINS: usize = 8;

const WIN_W: usize = 300;
const WIN_H: usize = 170;
const TITLE_H: usize = 18;
const TITLE_BTN: usize = 16;
const ICON_W: usize = 84;
const ICON_H: usize = 56;
const TASKBAR_H: usize = 18;

const DESK_BG: Color = 0x00_08_0c_14;
const TITLE_ON: Color = 0x00_2e_5a_c2;
const TITLE_OFF: Color = 0x00_20_2c_48;
const WIN_BG: Color = 0x00_18_24_48;
const BAR_BG: Color = 0x00_10_16_2c;
const BAR_ON: Color = 0x00_2a_4a_92;
const ICON_ACC: Color = 0x00_38_8a_e8;
const CLOSE_BG: Color = 0x00_b0_40_40;
const TEXT: Color = 0x00_d0_d0_e0;
const TEXT_DIM: Color = 0x00_80_90_b0;

static ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum WinKind {
    Welcome,
    System,
    Clock,
    About,
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
    note: [u8; 32],
    note_len: usize,
}

static mut WINS: [Option<Window>; MAX_WINS] = [None; MAX_WINS];
static mut CUR_X: usize = 0;
static mut CUR_Y: usize = 0;
static mut CUR_VIS: bool = false;
static mut LAST_BTNS: u8 = 0;
static mut DRAG: Option<usize> = None;

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
/// pixels, `pitch = w * 4`) plus its copied title.
struct ClientWin {
    /// Scheduler slot of the owning task (validated on every `SYS_GUI` call).
    pid: u32,
    w: usize,
    h: usize,
    buf: *const u8,
    title: [u8; CLIENT_TITLE_CAP],
    title_len: usize,
}

static mut CLIENTS: [Option<ClientWin>; MAX_CLIENTS] = [None, None, None, None];

/// Base of the RAM backbuffer: a dense software frame the GUI renders into and
/// then publishes to VRAM with a damage-aware `blit_region`. Sits in the spare
/// PML4-slot gap between the kernel heap (ends at `HEAP_START + 2 MiB`) and the
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
/// direct-VRAM path (`render()` handles a missing buffer) — no panic, no OOM
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
/// not yet requested) — the caller then paints directly into VRAM.
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
        WinKind::Client(_) => "ring3 client",
    }
}

/// Client window record for `id` (bounds-checked against [`MAX_CLIENTS`]).
fn client_slot(id: u8) -> Option<&'static ClientWin> {
    let id = id as usize;
    if id >= MAX_CLIENTS {
        return None;
    }
    unsafe { #[allow(static_mut_refs)] (*core::ptr::addr_of!(CLIENTS))[id].as_ref() }
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

fn spawn(kind: WinKind) {
    let Some(slot) = open_slot() else {
        return;
    };
    dirty_all();
    let n = window_count();
    let fb_w = console::framebuffer().map(|fb| fb.width()).unwrap_or(800);
    let fb_h = console::framebuffer().map(|fb| fb.height()).unwrap_or(600);
    let cascade = n % 5;
    let x = (fb_w / 2 + cascade * 28).saturating_sub(WIN_W / 2);
    let y = (fb_h / 3 + cascade * 34).min(fb_h.saturating_sub(WIN_H + TASKBAR_H + 20));
    let w = WIN_W;
    let h = if kind == WinKind::Clock { 140 } else { WIN_H };
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        for win in wins.iter_mut().flatten() {
            win.focused = false;
        }
        wins[slot] = Some(Window {
            kind,
            title: win_title(kind),
            x,
            y,
            w,
            h,
            focused: true,
            note: [0; 32],
            note_len: 0,
        });
    }
    bring_to_front(slot);
}

fn focus_window(i: usize) {
    dirty_all();
    unsafe {
        let wins = &mut *core::ptr::addr_of_mut!(WINS);
        for win in wins.iter_mut().flatten() {
            win.focused = false;
        }
        if let Some(win) = wins[i].as_mut() {
            win.focused = true;
        }
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

fn close_window(i: usize) {
    dirty_all();
    unsafe {
        if let Some(w) = core::ptr::addr_of!(DRAG).read() {
            if w == i {
                DRAG = None;
            }
        }
        (*core::ptr::addr_of_mut!(WINS))[i] = None;
    }
}

fn cycle_focus() {
    let wins = (0..MAX_WINS)
        .filter(|&i| unsafe { (*core::ptr::addr_of!(WINS))[i].is_some() })
        .collect::<alloc::vec::Vec<usize>>();
    if wins.is_empty() {
        return;
    }
    let cur = focused_idx().and_then(|i| wins.iter().position(|&w| w == i));
    let next = match cur {
        Some(p) => wins[(p + 1) % wins.len()],
        None => wins[0],
    };
    focus_window(next);
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
        CUR_VIS = false;
        LAST_BTNS = 0;
    }
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

/// Wire entry for the `SYS_GUI` syscall from ring-3.
///
/// `rdi == 0` performs CREATE with `rsi` pointing at a [`CreateReq`]
/// (returns the client id, `u64::MAX` on any validation failure). Any other
/// `rdi` performs PRESENT: the client's window is made dirty (and re-spawned
/// if it was closed, e.g. by the GUI `leave()`), so the composite picks up the
/// new pixels on the next `render()` — this is the ring-3 client's "flip".
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
        let req =
            unsafe { core::ptr::read_volatile(req_va as *const CreateReq) };
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
    } else {
        // PRESENT
        let mut id_found = None;
        unsafe {
            #[allow(static_mut_refs)]
            {
                for (i, c) in (*core::ptr::addr_of_mut!(CLIENTS)).iter_mut().enumerate() {
                    if let Some(c) = c {
                        if c.pid == pid {
                            id_found = Some(i as u8);
                        }
                    }
                }
            }
        }
        match id_found {
            Some(id) => {
                present_client(id);
                frame.rax = 1;
            }
            None => {
                frame.rax = u64::MAX;
            }
        }
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

/// Owns a `Client(id)` window: takes a window slot, sizes it to the client
/// buffer plus the title bar, and raises it to z-front.
fn spawn_client(id: u8) {
    let Some(slot) = open_slot() else {
        return;
    };
    let Some(c) = client_slot(id) else {
        return;
    };
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
        wins[slot] = Some(Window {
            kind: WinKind::Client(id),
            title: win_title(WinKind::Client(id)),
            x,
            y,
            w,
            h,
            focused: true,
            note: [0; 32],
            note_len: 0,
        });
    }
    bring_to_front(slot);
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
        fb.blit_at(
            &client,
            (0, 0),
            (win.x, win.y + TITLE_H),
            (c.w.min(win.w), c.h.min(body)),
        );
    }
}

/// Feeds one make-code to the windowed GUI. Returns `true` when consumed.
pub fn handle_scancode(sc: u8) -> bool {
    if !active() {
        return false;
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
        _ => {}
    }
    let Some(c) = crate::interrupts::scancode_to_char(sc) else {
        return false;
    };
    if c.is_control() {
        return true;
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

/// Applies a mouse report: moves the arrow (clamped to the framebuffer),
/// drags the focused window off its title bar and dispatches clicks
/// (icons / close button / task bar / window body).
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
            if let Some(i) = *core::ptr::addr_of!(DRAG) {
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
            dirty_rect(ox, oy, 8, 8);
            dirty_rect(nx, ny, 8, 8);
        }
        let prev = *core::ptr::addr_of!(LAST_BTNS);
        let down = buttons & 0x01 != 0;
        if !down {
            *core::ptr::addr_of_mut!(DRAG) = None;
        }
        if down != (prev & 0x01 != 0) {
            *core::ptr::addr_of_mut!(LAST_BTNS) = buttons;
            if down {
                dispatch_click(fb, nx, ny);
            }
        }
    }
}

fn hit_title(win: &Window, x: usize, y: usize) -> bool {
    y >= win.y && y < win.y + TITLE_H && x >= win.x && x < win.x + win.w.saturating_sub(TITLE_BTN)
}

fn hit_close(win: &Window, x: usize, y: usize) -> bool {
    x >= win.x + win.w.saturating_sub(TITLE_BTN)
        && x < win.x + win.w
        && y >= win.y
        && y < win.y + TITLE_H
}

fn hit_body(win: &Window, x: usize, y: usize) -> bool {
    x >= win.x && x < win.x + win.w && y >= win.y + TITLE_H && y < win.y + win.h
}

fn hit_icon(x: usize, y: usize) -> Option<WinKind> {
    let icons = [
        (WinKind::System, 20, 40),
        (WinKind::Clock, 20, 108),
        (WinKind::About, 20, 176),
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
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    for (i, win) in wins.iter().enumerate() {
        if let Some(win) = win {
            let w = 8 + win.title.len() * console::GLYPH_W;
            if x >= bx && x < bx + w {
                return Some(i);
            }
            bx += w + 4;
        }
    }
    None
}

fn dispatch_click(fb: &Framebuffer, x: usize, y: usize) {
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
            if hit_close(win, x, y) {
                close_window(i);
                hit_any = true;
                break;
            }
            if hit_title(win, x, y) {
                unsafe {
                    *core::ptr::addr_of_mut!(DRAG) = Some(i);
                }
                focus_window(i);
                hit_any = true;
                break;
            }
            if hit_body(win, x, y) {
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
/// complete frame or the new complete one, which — together with the damage
/// culling — removes both the full-screen flicker and torn rows. If the
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
            if full || rect_overlaps(&d, win.x, win.y, win.w, win.h) {
                draw_window(target, win);
            }
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
                WinKind::System | WinKind::Clock => dirty_window(win),
                _ => {}
            }
        }
    }
}

fn icon_label(kind: WinKind) -> &'static str {
    match kind {
        WinKind::Welcome => "Welcome",
        WinKind::System => "System",
        WinKind::Clock => "Uptime",
        WinKind::About => "About",
        WinKind::Client(_) => "ring3",
    }
}

fn draw_icons(fb: &Framebuffer, clip: &Rect) {
    let icons = [
        (WinKind::System, 20, 40),
        (WinKind::Clock, 20, 108),
        (WinKind::About, 20, 176),
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
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    for win in wins.iter().flatten() {
        let title = win_live_title(win);
        let w = 8 + title.len() * console::GLYPH_W;
        let bg = if win.focused { BAR_ON } else { BAR_BG };
        unsafe {
            fb.fill_rect(bx, y, w, TASKBAR_H, bg);
        }
        tui::draw_text(fb, bx + 4, y, title, TEXT, bg, bx + w);
        bx += w + 4;
    }
}

fn draw_window(fb: &Framebuffer, win: &Window) {
    let title_bg = if win.focused { TITLE_ON } else { TITLE_OFF };
    let title = win_live_title(win);
    unsafe {
        fb.fill_rect(win.x, win.y, win.w, TITLE_H, title_bg);
        fb.fill_rect(win.x, win.y + TITLE_H, win.w, win.h - TITLE_H, WIN_BG);
    }
    let max_px = fb.width().saturating_sub(16);
    tui::draw_text(
        fb,
        win.x + 6,
        win.y + 1,
        title,
        TEXT,
        title_bg,
        (win.x + win.w).min(max_px),
    );
    let cx = win.x + win.w - TITLE_BTN;
    unsafe {
        fb.fill_rect(cx, win.y, TITLE_BTN, TITLE_H, CLOSE_BG);
    }
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
        WinKind::Client(_) => draw_client(fb, win),
    }
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
        "mouse: click icon to open, drag title to move",
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
