//! Bare-metal windowed GUI for the AIOS kernel.
//!
//! The single AIOS Desktop: a full-screen graphical shell drawn straight onto
//! the kernel's linear framebuffer. It is the default screen after boot (the
//! idle loop calls [`enter()`]) and needs no host GUI process, X11 or eframe.
//! The console/TUI remains an escape hatch (`tui` command, or `Esc` with no
//! window focused). While the desktop owns the screen, `vprintln!` output is
//! suppressed and the heartbeat square paused so stray log lines cannot
//! garble the pixels. The screen owns a desktop background, a bottom task bar
//! with a `☰ Пуск` button on the left and the network tray on the right, a
//! sorted/searchable Start menu, and a z-ordered list of windows with title
//! bar, minimize/maximize/close buttons, draggable body and resize handles.
//! `F1`-`F9` open/focus the nine Start functions; window controls moved to
//! `F10`/`F11`/`F12` so no desktop shortcut is shadowed.
//!
//! Rendering is double-buffered and damage-based: every change records a dirty
//! rectangle and `render()` repaints only what intersects it (desktop
//! background, then the task bar/Start strip, then every window whose
//! rectangle overlaps, then the 8x8 arrow last) into a RAM backbuffer, then
//! publishes the damaged rectangle to VRAM with a single `copy_nonoverlapping`
//! per scanline. Idle frames therefore refresh only the live System/Uptime
//! windows, the panel is never drawn incrementally (no flicker, no torn rows),
//! and the static desktop stays untouched. The backbuffer is mapped once from
//! the frame allocator on `enter()` and its absence degrades gracefully to
//! direct-VRAM painting (never a panic). Text uses the same PSF path as the TUI
//! (`tui::draw_text`, now Unicode-aware: Basic Latin + generated Cyrillic, so
//! Russian Start labels render on the raw framebuffer). Input comes from both
//! key sources (PS/2 + USB-HID) via `handle_scancode` (Tab cycles focus, `Esc`
//! closes / leaves, `F1`-`F9` open apps, printable keys type into Start search
//! or the focused kernel window and queue events for ring-3 clients) and from
//! the mouse via `on_mouse` (Start/task-bar/window clicks, title drag, resize);
//! the loop reads these through lock-free atomics and never blocks on
//! `inb`/spin for input.
//!
//! Window geometry also has two explicit states: maximize grows a window to
//! the whole desktop above the task bar (geometry saved for restore, further
//! move/resize disabled) and minimize hides it until a task-bar/icon click
//! restores it (`focus_window` un-minimizes, `render` skips hidden windows).

use crate::framebuffer::{colors, Color, Framebuffer};
use crate::interrupts::{TICKS, TIMER_HZ};
use crate::{console, tui, vector};
use alloc::format;
use alloc::string::String;
use core::cmp::Ordering as CmpOrdering;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Ceiling on open windows (one slot per app instance).
pub const MAX_WINS: usize = 8;

const WIN_W: usize = 300;
const WIN_H: usize = 170;
const TITLE_H: usize = 18;
const TITLE_BTN: usize = 16;
/// Width of the three-button cluster (minimize / maximize-restore / close)
/// anchored to the right end of the title bar.
const TITLE_BTNS: usize = 3 * TITLE_BTN;
const TASKBAR_H: usize = 18;
/// Width reserved for the bottom-left `☰ Пуск` button; task-bar window buttons
/// start after it so the button never moves when windows open or close.
const START_BTN_W: usize = 88;
/// Width/height of the Start panel above the task bar.
const START_W: usize = 384;
const START_HEADER_H: usize = 42;
const START_ROW_H: usize = 34;
const START_VISIBLE: usize = 10;
const START_PAD: usize = 6;
/// Maximum number of Start entries after built-ins, catalog programs, actions
/// and live ring-3 clients have been merged and sorted.
const START_CAP: usize = 40;
/// Maximum characters accepted by the Start search field.
const START_QUERY_CAP: usize = 24;
/// Width of the hit zone along a window's outer edge that starts a resize
/// drag (edges/corners, checked before title/body).
const RESIZE_BORDER: usize = 5;
/// Smallest window width accepted by an edge/corner resize drag (equals the
/// narrowest default — the 96 px ring-3 client window).
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

/// Interactive crash dialog (fault recovery, Ring 0): fixed size, centered on
/// the desktop above every window; three plates resolve the frozen task.
const CDLG_W: usize = 464;
const CDLG_H: usize = 176;
/// Top of the recovery button row, measured from the dialog's top edge.
const CDLG_BTN_Y: usize = 136;
const CDLG_BTN_H: usize = 26;
/// `Rollback` / `Terminate` / `Resume` plates, 12 px gaps.
const CDLG_RB_X: usize = 16;
const CDLG_RB_W: usize = 136;
const CDLG_TM_X: usize = 164;
const CDLG_TM_W: usize = 136;
const CDLG_RS_X: usize = 312;
const CDLG_RS_W: usize = 136;

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

/// Desktop background wash: top and bottom stops of the vertical gradient;
/// their midpoint is the classic flat desktop `08 0C 14`, so the GUI-entry
/// pixel probe at (1100, 400) still resolves exactly.
const DESK_TOP: Color = 0x00_0c_10_18;
const DESK_BOT: Color = 0x00_04_08_10;
/// Corner radius shared by windows, dialogs and desktop icon tiles.
const WIN_R: i32 = 7;
/// Damage pad: soft shadows spill a few pixels past their shape, so every
/// damage rectangle is unioned wider than its content by this margin.
const SHADOW_PAD: usize = 7;
/// Taskbar top stop of the vertical wash (v2.38.39).
const BAR_TOP: Color = 0x00_1a_22_3a;
const TITLE_ON: Color = 0x00_2e_5a_c2;
const TITLE_OFF: Color = 0x00_20_2c_48;
const WIN_BG: Color = 0x00_18_24_48;
const BAR_BG: Color = 0x00_10_16_2c;
const BAR_ON: Color = 0x00_2a_4a_92;
const CLOSE_BG: Color = 0x00_b0_40_40;
const BTN_BG: Color = 0x00_28_30_50;
const BAR_MIN: Color = 0x00_0a_0e_1c;
const TEXT: Color = 0x00_d0_d0_e0;
const TEXT_DIM: Color = 0x00_80_90_b0;
/// Crash dialog (fault recovery) accents: error-red title bar and a hot
/// warning-color task line.
const TITLE_CRASH: Color = 0x00_b0_40_40;
const TEXT_HOT: Color = 0x00_ff_c0_80;

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
    /// Widget Lab (v2.47.0): a demo host for the generic widget set — editable
    /// text fields, a scrollable list with a scrollbar and a dropdown menu —
    /// proving keyboard/mouse routing into the focused window's widgets.
    Widgets,
    /// A window owned by a ring-3 task; the id indexes [`CLIENTS`], whose
    /// buffer the window composites on every repaint.
    Client(u8),
    /// A Start-catalog application window; the id indexes [`APPS`]. Used for
    /// the nine desktop functions and for external programs whose host-only
    /// runtime is unavailable in the native kernel.
    App(u8),
}

/// One application exposed in the kernel Start menu.
#[derive(Clone, Copy)]
struct AppInfo {
    /// Name shown in Start, the task bar and the window title.
    title: &'static str,
    /// Short hint shown under the name in Start.
    subtitle: &'static str,
    /// Body lines rendered in the application window.
    body: &'static [&'static str],
}

/// Start catalog in `F1`-`F9` order, followed by the shared external program
/// list. Dashboard, Network Settings, App Store, Files, Calculator, Terminal,
/// Network Settings (program) and Text Editor are real in-kernel apps; entries
/// whose runtime cannot exist without a host still open a capability note.
static APPS: &[AppInfo] = &[
    AppInfo {
        title: "System Dashboard",
        subtitle: "F1 · Система",
        body: &["Live kernel metrics, scheduler and IPC counters."],
    },
    AppInfo {
        title: "WASM Blocks",
        subtitle: "F2 · Блоки",
        body: &[
            "WASM blocks are loaded by the kernel block manager.",
            "Use the TUI block tab to inspect and hot-swap them.",
        ],
    },
    AppInfo {
        title: "AI Studio",
        subtitle: "F3 · ИИ",
        body: &[
            "Streaming LLM chat needs the host inference runtime.",
            "The native kernel exposes no model backend.",
        ],
    },
    AppInfo {
        title: "App Store",
        subtitle: "F4 · Магазин",
        body: &[
            "Built-in catalog of kernel applications.",
            "Enter or Run launches the selected program.",
        ],
    },
    AppInfo {
        title: "Network Settings",
        subtitle: "F5 · Сеть",
        body: &["Wired DHCP and software Wi-Fi settings."],
    },
    AppInfo {
        title: "Dependencies",
        subtitle: "F6 · Зависимости",
        body: &[
            "Block dependencies are tracked by aios-block-mgr.",
            "No graph view exists in the native kernel yet.",
        ],
    },
    AppInfo {
        title: "Native Browser",
        subtitle: "F7 · Браузер",
        body: &[
            "WebView needs a host windowing stack.",
            "The kernel desktop renders pixels only.",
        ],
    },
    AppInfo {
        title: "Files",
        subtitle: "F8 · Файлы",
        body: &[
            "Kernel information viewer: ↑/↓ selects a file.",
            "The preview below shows live kernel data.",
        ],
    },
    AppInfo {
        title: "Hardware & Drivers",
        subtitle: "F9 · Оборудование",
        body: &[
            "PCI, USB, NIC, storage and thermal drivers are probed at boot.",
            "Live state is shown in the System window.",
        ],
    },
    AppInfo {
        title: "Калькулятор",
        subtitle: "Программа · calculator",
        body: &[
            "Integer calculator: type digits, + - * /, Enter or =.",
            "Buttons accept mouse clicks; C clears, Backspace deletes.",
        ],
    },
    AppInfo {
        title: "Настройки",
        subtitle: "Программа · settings",
        body: &[
            "External program from the shared AIOS catalog.",
            "No host process launcher exists in the native kernel.",
        ],
    },
    AppInfo {
        title: "Браузер",
        subtitle: "Программа · browser",
        body: &[
            "External program from the shared AIOS catalog.",
            "No host process launcher exists in the native kernel.",
        ],
    },
    AppInfo {
        title: "Терминал",
        subtitle: "Программа · terminal",
        body: &["Opens the kernel console; Esc from there returns to the desktop."],
    },
    AppInfo {
        title: "Файловый менеджер",
        subtitle: "Программа · files",
        body: &[
            "Kernel information viewer: ↑/↓ selects a file.",
            "The preview below shows live kernel data.",
        ],
    },
    AppInfo {
        title: "Настройки звука",
        subtitle: "Программа · sound",
        body: &[
            "External program from the shared AIOS catalog.",
            "The kernel has no audio stack.",
        ],
    },
    AppInfo {
        title: "Сменить обои",
        subtitle: "Программа · wallpaper",
        body: &[
            "External program from the shared AIOS catalog.",
            "The kernel desktop uses a fixed dark gradient.",
        ],
    },
    AppInfo {
        title: "Настройки сети",
        subtitle: "Программа · network",
        body: &["Opens the kernel Network window (the same app as F5)."],
    },
    AppInfo {
        title: "Текстовый редактор",
        subtitle: "Программа · editor",
        body: &[
            "Six-line kernel editor: type, Enter for a new line,",
            "Backspace deletes, ↑/↓ or a click selects a line.",
        ],
    },
    AppInfo {
        title: "Скриншот",
        subtitle: "Программа · screenshot",
        body: &[
            "External program from the shared AIOS catalog.",
            "No host process launcher exists in the native kernel.",
        ],
    },
    AppInfo {
        title: "Перезагрузка",
        subtitle: "Программа · reboot",
        body: &[
            "Use the machine power button or the boot loader menu.",
            "The kernel desktop does not reset hardware.",
        ],
    },
    AppInfo {
        title: "Выключение",
        subtitle: "Программа · shutdown",
        body: &[
            "Use the machine power button or the boot loader menu.",
            "The kernel desktop does not power off hardware.",
        ],
    },
];

/// `APPS` index of the in-kernel App Store window.
const APP_STORE: u8 = 3;
/// `APPS` index of the in-kernel Files window.
const APP_FILES: u8 = 7;
/// `APPS` index of the in-kernel Calculator window.
const APP_CALC: u8 = 9;
/// `APPS` index of the kernel console launcher.
const APP_TERMINAL: u8 = 12;
/// `APPS` index of the Russian-named Files entry.
const APP_FILES_ALT: u8 = 13;
/// `APPS` index of the Russian-named Network Settings entry.
const APP_NETWORK_ALT: u8 = 16;
/// `APPS` index of the in-kernel Text Editor window.
const APP_EDITOR: u8 = 17;

/// Applications offered by the App Store window, as indexes into [`APPS`].
const STORE_IDS: [u8; 6] = [0, APP_STORE, APP_FILES, APP_CALC, APP_TERMINAL, APP_EDITOR];

/// Names of the live information pseudo-files shown by the Files viewer.
const FILE_ROWS: [&str; 6] = [
    "system/version",
    "system/uptime",
    "system/timer",
    "system/memory",
    "system/gui",
    "system/input",
];

/// Calculator display capacity in bytes (ASCII).
const CALC_CAP: usize = 20;
/// Number of calculator button columns.
const CALC_COLS: usize = 4;
/// Calculator buttons in row-major order; each char feeds [`calc_input`].
const CALC_KEYS: [char; 16] = [
    '7', '8', '9', '/', '4', '5', '6', '*', '1', '2', '3', '-', '0', 'c', '=', '+',
];
/// Width of one calculator button in pixels.
const CALC_BTN_W: usize = 56;
/// Height of one calculator button in pixels.
const CALC_BTN_H: usize = 22;
/// Horizontal step between calculator buttons.
const CALC_BTN_STEP_X: usize = 62;
/// Vertical step between calculator button rows.
const CALC_BTN_STEP_Y: usize = 26;
/// Window-local x of the calculator button grid.
const CALC_GRID_X: usize = 8;
/// Window-local y of the calculator button grid.
const CALC_GRID_Y: usize = 50;

/// Bytes per line of the kernel text editor.
const EDITOR_COL_CAP: usize = 40;
/// Number of lines in the kernel text editor.
const EDITOR_ROWS: usize = 6;
/// Window-local y of the first App Store / Files / Editor row.
const APP_ROW_Y: usize = 26;
/// Height of one selectable row in an in-kernel app window.
const APP_ROW_H: usize = 16;
/// Window-local geometry `(x, y, w, h)` of the App Store Run button.
const STORE_BTN: (usize, usize, usize, usize) = (8, 138, 120, 20);

/// Live state of the in-kernel integer calculator.
#[derive(Clone, Copy)]
struct CalcState {
    /// ASCII display text without a terminator.
    text: [u8; CALC_CAP],
    /// Used length of [`CalcState::text`].
    len: u8,
    /// Accumulated left operand of the pending operation.
    acc: i64,
    /// Pending operator byte (`+`, `-`, `*` or `/`), zero when none.
    op: u8,
    /// Whether the next digit replaces the display instead of appending.
    fresh: bool,
    /// Whether the display currently shows an error.
    error: bool,
}

/// Calculator state, owned by the single-threaded GUI.
static mut CALC: CalcState = CalcState {
    text: [0; CALC_CAP],
    len: 0,
    acc: 0,
    op: 0,
    fresh: true,
    error: false,
};
/// Selected row of the App Store window.
static mut STORE_SEL: u8 = 0;
/// Selected row of the Files viewer.
static mut FILES_SEL: u8 = 0;
/// Text buffers of the kernel editor lines.
static mut EDITOR_LINES: [[u8; EDITOR_COL_CAP]; EDITOR_ROWS] = [[0; EDITOR_COL_CAP]; EDITOR_ROWS];
/// Used byte lengths of [`EDITOR_LINES`].
static mut EDITOR_LENS: [u8; EDITOR_ROWS] = [0; EDITOR_ROWS];
/// Line currently being edited.
static mut EDITOR_ROW: u8 = 0;

/// Compact label for a ring-3 window in Start (titles there are dynamic).
static CLIENT_LABELS: [&str; MAX_CLIENTS] = [
    "ring3 client 0",
    "ring3 client 1",
    "ring3 client 2",
    "ring3 client 3",
];

/// What happens when a Start entry is activated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StartAction {
    /// Open or focus a kernel window.
    Open(WinKind),
    /// Open or focus a Start-catalog application window.
    App(u8),
    /// Repaint every live desktop surface.
    Refresh,
    /// Spawn the five dormant ring-3 demo tasks once.
    Ring3Demo,
    /// Leave the desktop for the kernel console/TUI.
    Console,
}

/// One sorted entry in the native Start menu.
#[derive(Clone, Copy)]
struct StartEntry {
    /// Display name.
    title: &'static str,
    /// Short hint shown under the name.
    subtitle: &'static str,
    /// Action executed on activation.
    action: StartAction,
}

/// Kernel windows that are not part of the nine desktop functions. Together
/// with [`APPS`], the live ring-3 clients and the two system actions they form
/// the complete, sorted Start catalog.
static START_BASE: &[StartEntry] = &[
    StartEntry {
        title: "About",
        subtitle: "AIOS · информация",
        action: StartAction::Open(WinKind::About),
    },
    StartEntry {
        title: "Task Manager",
        subtitle: "Система · задачи",
        action: StartAction::Open(WinKind::Tasks),
    },
    StartEntry {
        title: "Uptime",
        subtitle: "Система · время",
        action: StartAction::Open(WinKind::Clock),
    },
    StartEntry {
        title: "Welcome",
        subtitle: "AIOS · приветствие",
        action: StartAction::Open(WinKind::Welcome),
    },
    StartEntry {
        title: "Widget Lab",
        subtitle: "Система · виджеты",
        action: StartAction::Open(WinKind::Widgets),
    },
];

/// Whether the Start panel is currently visible.
static START_OPEN: AtomicBool = AtomicBool::new(false);
/// Selected row in the filtered Start list.
static mut START_SEL: usize = 0;
/// First visible row in the filtered Start list.
static mut START_SCROLL: usize = 0;
/// ASCII/UTF-8 bytes of the Start search query.
static mut START_QUERY: [u8; START_QUERY_CAP] = [0; START_QUERY_CAP];
/// Length of [`START_QUERY`] in bytes.
static mut START_QUERY_LEN: usize = 0;

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
/// `bring_to_front` slot shifts — an index would go stale the moment the
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

/// A widget hosted in a window body — the first increment of the widget-set
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
    /// Editable single-line text field (v2.47.0): click to focus, then type.
    /// `id` addresses the field in the host window's form store; the caption
    /// is drawn by the host body, not the widget.
    TextField {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        /// Form-store slot addressing the field's buffer.
        id: u8,
    },
    /// Scrollable single-column item list (v2.47.0). `id` addresses the list
    /// selection/scroll state; `items` are the static rows.
    List {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        id: u8,
        items: &'static [&'static str],
    },
    /// Vertical scrollbar bound to a list (v2.47.0): a track with a thumb
    /// positioned by the list's scroll offset; clicking jumps proportionally.
    Scrollbar {
        x: usize,
        y: usize,
        h: usize,
        /// The [`Widget::List`] whose scroll this bar drives.
        list_id: u8,
    },
    /// Dropdown menu (v2.47.0): a plate that opens [`Menu::items`] below it;
    /// picking a row runs [`WidgetAction::Menu`] with the picked index.
    Menu {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        id: u8,
        label: &'static str,
        items: &'static [&'static str],
    },
    /// Checkbox (v2.63.0): a square box with a caption that toggles its bit in
    /// the host form store; the checked bit flips [`WidgetAction::Check`].
    Checkbox {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        /// Form-store bit addressing this checkbox.
        id: u8,
        label: &'static str,
    },
    /// Radio button (v2.64.0): a round box that selects its `group` slot in the
    /// host form store; selecting one clears the others in the same group.
    Radio {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        /// Form-store group addressing this radio's selection.
        group: u8,
        /// The `group` value this radio selects.
        value: u8,
        label: &'static str,
    },
    /// Horizontal slider (v2.67.0): a track with a thumb whose fraction of the
    /// span maps to a value in `min..=max`, stored in the host form store.
    /// Clicking anywhere on the track jumps the thumb to that value.
    Slider {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        /// Form-store slot addressing this slider's value.
        id: u8,
        /// Value at the left end of the track.
        min: u8,
        /// Value at the right end of the track.
        max: u8,
        label: &'static str,
    },
}

/// Width of a [`Widget::Scrollbar`] track in pixels.
const SCROLLBAR_W: usize = 12;

/// Window-local bounding box `(x, y, w, h)` of any widget variant.
fn widget_rect(wg: &Widget) -> (usize, usize, usize, usize) {
    match *wg {
        Widget::Button { x, y, w, h, .. }
        | Widget::TextField { x, y, w, h, .. }
        | Widget::List { x, y, w, h, .. }
        | Widget::Menu { x, y, w, h, .. }
        | Widget::Checkbox { x, y, w, h, .. }
        | Widget::Radio { x, y, w, h, .. }
        | Widget::Slider { x, y, w, h, .. } => (x, y, w, h),
        Widget::Scrollbar { x, y, h, .. } => (x, y, SCROLLBAR_W, h),
    }
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
/// Raises a "shown/logged" edge marker for the crash dialog: falls when the
/// pending fault is resolved so the next fault opens it again.
static mut CRASH_SEEN: bool = false;
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
    /// Keep-first semantics — a second event arriving before the app polls is
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
/// exactly at `BACKBUF_BASE` — abutting, page-disjoint) and below the
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

/// Unions a pixel rectangle into the pending damage, padded by
/// [`SHADOW_PAD`] so shadow spill is repainted together with its shape.
fn dirty_rect(x: usize, y: usize, w: usize, h: usize) {
    let x0 = x.saturating_sub(SHADOW_PAD);
    let y0 = y.saturating_sub(SHADOW_PAD);
    unsafe {
        let d = &mut *core::ptr::addr_of_mut!(DIRTY);
        d.x0 = d.x0.min(x0);
        d.y0 = d.y0.min(y0);
        d.x1 = d.x1.max(x.saturating_add(w).saturating_add(SHADOW_PAD));
        d.y1 = d.y1.max(y.saturating_add(h).saturating_add(SHADOW_PAD));
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
        WinKind::Network => "Network",
        WinKind::Tasks => "Task Manager",
        WinKind::Widgets => "Widget Lab",
        WinKind::Client(_) => "ring3 client",
        WinKind::App(id) => APPS.get(id as usize).map(|a| a.title).unwrap_or("AIOS"),
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

/// Kind of the focused window, if any — the target of the keyboard hotkeys.
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
/// [`remember_closed_z`] — the caller then places the reopened window there.
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
        form_load(WinKind::Network);
    }
    if kind == WinKind::Tasks {
        w = 440;
        h = 248;
        x = 460.min(fb_w.saturating_sub(w + 8));
        y = 140.min(fb_h.saturating_sub(h + TASKBAR_H + 8));
    }
    if kind == WinKind::Widgets {
        w = 460;
        h = 332;
        x = 500.min(fb_w.saturating_sub(w + 8));
        y = 120.min(fb_h.saturating_sub(h + TASKBAR_H + 8));
        lab_form_load();
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

/// Closes window `i` — unless it holds a typed-text note, in which case the
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
/// focus — and ring-3 client input — cannot be stolen.
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

/// Window of the interactive crash dialog: center of the desktop, sized with
/// the CDLG_* block.
fn crash_pos(fb: &Framebuffer) -> (usize, usize) {
    ((fb.width() - CDLG_W) / 2, (fb.height() - CDLG_H) / 2)
}

/// Polls the fault interceptor for a pending recovery: opens (logs + dirty) on
/// the rising edge and repaints on the falling edge so the dialog and its
/// aftermath clear from the desktop. Called every idle-loop iteration.
pub fn poll_crash() {
    let pending = crate::fault::pending();
    let seen = unsafe {
        #[allow(static_mut_refs)]
        {
            let s = core::ptr::addr_of!(CRASH_SEEN).read();
            if pending != s {
                *core::ptr::addr_of_mut!(CRASH_SEEN) = pending;
            }
            s
        }
    };
    if pending && !seen {
        dirty_all();
        if let Some(record) = crate::fault::peek() {
            let snap = &record.snapshot;
            crate::vprintln!(
                "[gui] crash dialog: pid {} {} at rip={:#x} rsp={:#x}",
                snap.pid,
                record.kind.name(),
                snap.rip,
                snap.rsp
            );
            crate::kprintln!("[serial] [gui] crash dialog open pid {}", snap.pid);
        }
    } else if !pending && seen {
        dirty_all();
    }
}

/// Cursor position of the crash dialog buttons; used by both mouse routing and
/// unit-testable geometry.
fn crash_btn_rects(fb: &Framebuffer) -> ((usize, usize), (usize, usize), (usize, usize)) {
    let (dx, dy) = crash_pos(fb);
    let ry = dy + CDLG_BTN_Y;
    (
        (dx + CDLG_RB_X, ry),
        (dx + CDLG_TM_X, ry),
        (dx + CDLG_RS_X, ry),
    )
}

/// Mouse routing while the crash dialog is up: only the three recovery plates
/// act; every other press is blocked (the desktop and windows keep their
/// focus). Matches [`draw_crash_dialog`].
fn crash_click(fb: &Framebuffer, x: usize, y: usize) {
    if !crate::fault::pending() {
        return;
    }
    let budget_locked = crate::fault::over_budget_current();
    let ((rx, ry), (tx, ty), (sx, sy)) = crash_btn_rects(fb);
    let hit =
        |bx: usize, bw: usize, by: usize| x >= bx && x < bx + bw && y >= by && y < by + CDLG_BTN_H;
    if hit(rx, CDLG_RB_W, ry) {
        if budget_locked {
            crate::kprintln!("[gui] crash dialog blocks rollback click: over fault budget");
        } else {
            crate::fault::resolve(crate::fault::Recovery::Rollback);
            dirty_all();
        }
    } else if hit(tx, CDLG_TM_W, ty) {
        crate::fault::resolve(crate::fault::Recovery::Terminate);
        dirty_all();
    } else if hit(sx, CDLG_RS_W, sy) {
        if budget_locked {
            crate::kprintln!("[gui] crash dialog blocks resume click: over fault budget");
        } else {
            crate::fault::resolve(crate::fault::Recovery::Resume);
            dirty_all();
        }
    } else {
        crate::kprintln!("[gui] crash dialog blocks click ({},{})", x, y);
    }
}

/// Keyboard routing while the crash dialog is up: `Enter` resumes, `R` rolls
/// back, `T` terminates, everything else is consumed and logged. Over the
/// task's fault budget only `T`/`Enter`-on-Rollback-less actions resolve
/// (Resume and Rollback are blocked).
fn crash_key(sc: u8) -> bool {
    let resume = sc == 0x1C
        || crate::interrupts::scancode_to_char(sc).is_some_and(|c| c.eq_ignore_ascii_case(&'s'));
    if resume {
        if crate::fault::over_budget_current() {
            crate::kprintln!("[gui] crash dialog blocks resume: over fault budget");
        } else {
            crate::fault::resolve(crate::fault::Recovery::Resume);
            dirty_all();
        }
        return true;
    }
    if crate::interrupts::scancode_to_char(sc).is_some_and(|c| c.eq_ignore_ascii_case(&'r')) {
        if crate::fault::over_budget_current() {
            crate::kprintln!("[gui] crash dialog blocks rollback: over fault budget");
        } else {
            crate::fault::resolve(crate::fault::Recovery::Rollback);
            dirty_all();
        }
        return true;
    }
    if crate::interrupts::scancode_to_char(sc).is_some_and(|c| c.eq_ignore_ascii_case(&'t')) {
        crate::fault::resolve(crate::fault::Recovery::Terminate);
        dirty_all();
        return true;
    }
    crate::kprintln!("[gui] crash dialog blocks scancode {:#04x}", sc);
    true
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
        WinKind::Widgets => 6,
        WinKind::Client(id) => 7 + id,
        WinKind::App(id) => 7 + MAX_CLIENTS as u8 + id,
    }
}

/// All open window indices in [`kind_rank`] order (`usize::MAX` marks the
/// unused tail). Both the task-bar hit test and its drawing walk this order,
/// so buttons keep their place across focus/z-order changes — a minimized
/// window's button never moves under the pointer.
fn taskbar_slots() -> [usize; MAX_WINS] {
    let mut out = [usize::MAX; MAX_WINS];
    let mut n = 0usize;
    for rank in 0u8..(7 + MAX_CLIENTS as u8 + APPS.len() as u8) {
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

/// Lowercase key of one title scalar, so Latin and Cyrillic names sort
/// naturally without an allocation or a locale table.
fn sort_key(c: char) -> u32 {
    let cp = c as u32;
    if c.is_ascii_uppercase() || (0x0410..=0x042f).contains(&cp) {
        cp + 0x20
    } else if cp == 0x0401 {
        0x0451
    } else {
        cp
    }
}

/// Case-insensitive, locale-free ordering of two Start titles.
fn start_cmp(a: &str, b: &str) -> CmpOrdering {
    let mut ai = a.chars().map(sort_key);
    let mut bi = b.chars().map(sort_key);
    loop {
        match (ai.next(), bi.next()) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                CmpOrdering::Equal => {}
                order => return order,
            },
            (Some(_), None) => return CmpOrdering::Greater,
            (None, Some(_)) => return CmpOrdering::Less,
            (None, None) => return CmpOrdering::Equal,
        }
    }
}

/// Inserts `entry` into the sorted Start array with a linear insertion sort
/// (the catalog is at most [`START_CAP`] long).
fn start_push(out: &mut [Option<StartEntry>; START_CAP], n: &mut usize, entry: StartEntry) {
    if *n >= out.len() {
        return;
    }
    let mut j = *n;
    while j > 0 && start_cmp(entry.title, out[j - 1].unwrap().title) == CmpOrdering::Less {
        out[j] = out[j - 1];
        j -= 1;
    }
    out[j] = Some(entry);
    *n += 1;
}

/// Builds the full Start catalog: built-in windows, the nine functions, the
/// shared external program list, live ring-3 windows and system actions — all
/// sorted by title with [`start_cmp`].
fn start_entries() -> [Option<StartEntry>; START_CAP] {
    let mut out = [None; START_CAP];
    let mut n = 0usize;
    for entry in START_BASE {
        start_push(&mut out, &mut n, *entry);
    }
    for (i, app) in APPS.iter().enumerate() {
        let action = match i {
            0 => StartAction::Open(WinKind::System),
            4 => StartAction::Open(WinKind::Network),
            _ => StartAction::App(i as u8),
        };
        start_push(
            &mut out,
            &mut n,
            StartEntry {
                title: app.title,
                subtitle: app.subtitle,
                action,
            },
        );
    }
    for id in 0..MAX_CLIENTS as u8 {
        if client_slot(id).is_some() {
            start_push(
                &mut out,
                &mut n,
                StartEntry {
                    title: CLIENT_LABELS[id as usize],
                    subtitle: "ring-3 · клиент",
                    action: StartAction::Open(WinKind::Client(id)),
                },
            );
        }
    }
    if !crate::user::demos_started() {
        start_push(
            &mut out,
            &mut n,
            StartEntry {
                title: "Ring-3 демо",
                subtitle: "Система · запустить задачи A/B/C/D/E",
                action: StartAction::Ring3Demo,
            },
        );
    }
    start_push(
        &mut out,
        &mut n,
        StartEntry {
            title: "Обновить всё",
            subtitle: "Система · refresh",
            action: StartAction::Refresh,
        },
    );
    start_push(
        &mut out,
        &mut n,
        StartEntry {
            title: "Консоль ядра",
            subtitle: "Система · terminal",
            action: StartAction::Console,
        },
    );
    out
}

/// Current Start search text (only characters typed on the kernel key path).
fn start_query() -> &'static str {
    unsafe {
        let n = *core::ptr::addr_of!(START_QUERY_LEN);
        let p = core::ptr::addr_of!(START_QUERY) as *const u8;
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(p, n))
    }
}

/// Case-insensitive substring match on both the title and the hint. The same
/// case folding as [`start_cmp`] covers Latin and Cyrillic, so `net`, `F5` and
/// `систем` all narrow the sorted list.
fn start_matches(entry: &StartEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    [entry.title, entry.subtitle].iter().any(|haystack| {
        haystack.char_indices().any(|(i, _)| {
            let mut q = query.chars();
            let mut h = haystack[i..].chars();
            loop {
                match (q.next(), h.next()) {
                    (Some(a), Some(b)) => {
                        if sort_key(a) != sort_key(b) {
                            return false;
                        }
                    }
                    (None, _) => return true,
                    (Some(_), None) => return false,
                }
            }
        })
    })
}

/// Array index of the `n`-th entry that survives the current search filter.
fn start_visible_index(
    entries: &[Option<StartEntry>; START_CAP],
    query: &str,
    n: usize,
) -> Option<usize> {
    let mut seen = 0usize;
    for (i, entry) in entries.iter().enumerate() {
        if let Some(entry) = entry {
            if start_matches(entry, query) {
                if seen == n {
                    return Some(i);
                }
                seen += 1;
            }
        }
    }
    None
}

/// Number of entries surviving the current search filter.
fn start_count() -> usize {
    let entries = start_entries();
    let query = start_query();
    entries
        .iter()
        .flatten()
        .filter(|entry| start_matches(entry, query))
        .count()
}

/// Keeps the selected/scrolled rows inside the filtered list after edits.
fn clamp_start_sel() {
    let count = start_count();
    unsafe {
        if count == 0 {
            START_SEL = 0;
            START_SCROLL = 0;
        } else {
            if START_SEL >= count {
                START_SEL = count - 1;
            }
            if START_SEL < START_SCROLL {
                START_SCROLL = START_SEL;
            }
            if START_SEL >= START_SCROLL + START_VISIBLE {
                START_SCROLL = START_SEL + 1 - START_VISIBLE;
            }
        }
    }
}

/// Whether the Start panel is visible.
fn start_open() -> bool {
    START_OPEN.load(Ordering::Relaxed)
}

/// Opens or closes the Start panel: the task-bar button and the left `Win`
/// key both route here.
fn toggle_start() {
    let open = !start_open();
    START_OPEN.store(open, Ordering::Relaxed);
    unsafe {
        START_SEL = 0;
        START_SCROLL = 0;
        START_QUERY_LEN = 0;
        START_QUERY[0] = 0;
    }
    dirty_all();
}

fn close_start() {
    START_OPEN.store(false, Ordering::Relaxed);
    dirty_all();
}

/// Handles one key while the Start panel owns the keyboard.
fn start_key(sc: u8) -> bool {
    match sc {
        0x01 | 0x5B => {
            close_start();
            return true;
        }
        0x48 => unsafe {
            START_SEL = START_SEL.saturating_sub(1);
            clamp_start_sel();
            dirty_all();
            return true;
        },
        0x50 => unsafe {
            let count = start_count();
            if count > 0 && START_SEL + 1 < count {
                START_SEL += 1;
            }
            clamp_start_sel();
            dirty_all();
            return true;
        },
        0x1C => {
            let entries = start_entries();
            let query = start_query();
            let sel = unsafe { START_SEL };
            if let Some(i) = start_visible_index(&entries, query, sel) {
                let entry = entries[i].unwrap();
                close_start();
                activate_start(entry);
            }
            return true;
        }
        0x0E => unsafe {
            let mut n = START_QUERY_LEN;
            while n > 0 {
                n -= 1;
                if START_QUERY[n] & 0xc0 != 0x80 {
                    break;
                }
            }
            START_QUERY_LEN = n;
            START_QUERY[n] = 0;
            START_SEL = 0;
            START_SCROLL = 0;
            dirty_all();
            return true;
        },
        _ => {}
    }
    let Some(c) = crate::interrupts::scancode_to_char(sc) else {
        return true;
    };
    if !c.is_control() {
        unsafe {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                if START_QUERY_LEN + 1 >= START_QUERY_CAP {
                    break;
                }
                START_QUERY[START_QUERY_LEN] = b;
                START_QUERY_LEN += 1;
            }
            START_QUERY[START_QUERY_LEN] = 0;
            START_SEL = 0;
            START_SCROLL = 0;
        }
        dirty_all();
    }
    true
}

/// Executes one Start entry after the panel has closed.
fn activate_start(entry: StartEntry) {
    match entry.action {
        StartAction::Open(kind) => open_or_focus(kind),
        StartAction::App(id) => open_app(id),
        StartAction::Refresh => {
            dirty_all();
            crate::kprintln!("[gui] start: refresh all");
        }
        StartAction::Ring3Demo => {
            match crate::user::start_demos() {
                Ok(()) => crate::kprintln!("[gui] start: ring-3 demos launched"),
                Err(e) => crate::kprintln!("[gui] start: ring-3 demos failed: {}", e),
            }
            dirty_all();
        }
        StartAction::Console => leave(),
    }
}

/// Opens (or focuses) one Start-catalog program. Dashboard and Network Settings
/// reuse their native windows; Terminal and the network program alias dispatch
/// to the same kernel surfaces, and the rest open in-kernel application windows.
fn open_app(id: u8) {
    match id {
        0 => open_or_focus(WinKind::System),
        4 | APP_NETWORK_ALT => open_or_focus(WinKind::Network),
        APP_TERMINAL => leave(),
        _ if (id as usize) < APPS.len() => {
            if find_kind(WinKind::App(id)).is_none() {
                app_load(id);
            }
            open_or_focus(WinKind::App(id));
        }
        _ => {}
    }
}

/// Prepares the state of an in-kernel app when its window first opens.
fn app_load(id: u8) {
    match id {
        APP_CALC => unsafe {
            CALC = CalcState {
                text: [0; CALC_CAP],
                len: 0,
                acc: 0,
                op: 0,
                fresh: true,
                error: false,
            };
        },
        APP_STORE => unsafe { STORE_SEL = 0 },
        APP_FILES | APP_FILES_ALT => unsafe { FILES_SEL = 0 },
        _ => {}
    }
    let title = APPS.get(id as usize).map(|a| a.title).unwrap_or("app");
    crate::kprintln!("[gui] app launch {} id={}", title, id);
}

/// Writes `v` as decimal ASCII into the calculator display.
fn calc_set_number(v: i64) {
    let s = format!("{}", v);
    let b = s.as_bytes();
    let n = b.len().min(CALC_CAP);
    let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
    st.text[..n].copy_from_slice(&b[..n]);
    st.len = n as u8;
    st.error = false;
}

/// Parses the calculator display as an integer, or `None` on error/empty.
fn calc_value() -> Option<i64> {
    let st = unsafe { &*core::ptr::addr_of!(CALC) };
    if st.error || st.len == 0 {
        return None;
    }
    core::str::from_utf8(&st.text[..st.len as usize])
        .ok()?
        .parse::<i64>()
        .ok()
}

/// Puts the calculator into its error state.
fn calc_error() {
    let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
    st.text[..3].copy_from_slice(b"ERR");
    st.len = 3;
    st.error = true;
    st.fresh = true;
    st.op = 0;
}

/// Folds the pending operator into the display, if one is pending.
fn calc_apply() {
    let (op, acc) = {
        let st = unsafe { &*core::ptr::addr_of!(CALC) };
        (st.op, st.acc)
    };
    if op == 0 {
        return;
    }
    let Some(rhs) = calc_value() else {
        return;
    };
    let result = match op {
        b'+' => acc.saturating_add(rhs),
        b'-' => acc.saturating_sub(rhs),
        b'*' => acc.saturating_mul(rhs),
        b'/' if rhs != 0 => acc.checked_div(rhs).unwrap_or(0),
        b'/' => {
            calc_error();
            return;
        }
        _ => return,
    };
    calc_set_number(result);
    let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
    st.acc = result;
    st.op = 0;
    st.fresh = true;
    crate::kprintln!("[gui] calc result {}", result);
}

/// Feeds one character into the calculator. Returns `true` when consumed.
fn calc_input(c: char) -> bool {
    if c.is_ascii_digit() {
        let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
        if st.fresh || st.error {
            st.len = 0;
            st.error = false;
            st.fresh = false;
        }
        if (st.len as usize) < CALC_CAP {
            st.text[st.len as usize] = c as u8;
            st.len += 1;
        }
        return true;
    }
    if matches!(c, 'c' | 'C') {
        let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
        st.len = 0;
        st.acc = 0;
        st.op = 0;
        st.fresh = true;
        st.error = false;
        return true;
    }
    if matches!(c, '+' | '-' | '*' | '/' | 'x' | 'X') {
        calc_apply();
        let op = if matches!(c, 'x' | 'X') {
            b'*'
        } else {
            c as u8
        };
        let value = calc_value().unwrap_or(0);
        let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
        st.acc = value;
        st.op = op;
        st.fresh = true;
        return true;
    }
    if c == '=' {
        calc_apply();
        return true;
    }
    false
}

/// Deletes the last calculator display character.
fn calc_backspace() {
    let st = unsafe { &mut *core::ptr::addr_of_mut!(CALC) };
    if st.len > 0 {
        st.len -= 1;
        st.fresh = false;
        st.error = false;
    }
}

/// Current calculator display text.
fn calc_text() -> &'static str {
    let st = unsafe { &*core::ptr::addr_of!(CALC) };
    core::str::from_utf8(&st.text[..st.len as usize]).unwrap_or("ERR")
}

/// Moves the App Store selection by `delta` rows, clamped to the catalog.
fn store_move(delta: i8) {
    let n = STORE_IDS.len() as i8 - 1;
    let sel = unsafe { STORE_SEL as i8 };
    let next = (sel + delta).clamp(0, n) as u8;
    unsafe { STORE_SEL = next };
    let title = STORE_IDS
        .get(next as usize)
        .and_then(|&id| APPS.get(id as usize))
        .map(|app| app.title)
        .unwrap_or("app");
    crate::kprintln!("[gui] store select {}", title);
}

/// Launches the App Store's selected program.
fn store_launch() {
    let Some(&id) = STORE_IDS.get(unsafe { STORE_SEL as usize }) else {
        return;
    };
    let title = APPS.get(id as usize).map(|app| app.title).unwrap_or("app");
    crate::kprintln!("[gui] store launch {} id={}", title, id);
    open_app(id);
}

/// Moves the Files viewer selection by `delta` rows, clamped to the file list.
fn files_move(delta: i8) {
    let n = FILE_ROWS.len() as i8 - 1;
    let sel = unsafe { FILES_SEL as i8 };
    unsafe { FILES_SEL = (sel + delta).clamp(0, n) as u8 };
}

/// One line of the editor as ASCII text.
fn editor_line(row: usize) -> &'static str {
    let len = unsafe { EDITOR_LENS[row] as usize };
    unsafe { core::str::from_utf8_unchecked(&EDITOR_LINES[row][..len]) }
}

/// Types one ASCII character into the current editor line.
fn editor_type(c: char) {
    let row = unsafe { EDITOR_ROW as usize };
    let len = unsafe { EDITOR_LENS[row] as usize };
    if !c.is_ascii() || c.is_control() || len + 1 >= EDITOR_COL_CAP {
        return;
    }
    unsafe {
        EDITOR_LINES[row][len] = c as u8;
        EDITOR_LENS[row] = (len + 1) as u8;
    }
}

/// Deletes the last character of the current editor line.
fn editor_backspace() {
    let row = unsafe { EDITOR_ROW as usize };
    let len = unsafe { EDITOR_LENS[row] as usize };
    if len > 0 {
        unsafe {
            EDITOR_LINES[row][len - 1] = 0;
            EDITOR_LENS[row] = (len - 1) as u8;
        }
    }
}

/// Moves the editor cursor to the next line, if any.
fn editor_newline() {
    let row = unsafe { EDITOR_ROW as usize };
    if row + 1 < EDITOR_ROWS {
        unsafe { EDITOR_ROW = (row + 1) as u8 };
    }
}

/// Routes a key to the focused in-kernel app. Returns `true` when consumed.
fn app_key(sc: u8) -> bool {
    let Some(WinKind::App(id)) = focused_kind() else {
        return false;
    };
    let handled = match id {
        APP_CALC => {
            if sc == 0x0E {
                calc_backspace();
                true
            } else if let Some(c) = crate::interrupts::scancode_to_char(sc) {
                calc_input(c)
            } else {
                false
            }
        }
        APP_STORE => match sc {
            0x48 => {
                store_move(-1);
                true
            }
            0x50 => {
                store_move(1);
                true
            }
            0x1C => {
                store_launch();
                true
            }
            _ => false,
        },
        APP_FILES | APP_FILES_ALT => match sc {
            0x48 => {
                files_move(-1);
                true
            }
            0x50 => {
                files_move(1);
                true
            }
            _ => false,
        },
        APP_EDITOR => {
            if sc == 0x0E {
                editor_backspace();
                true
            } else if sc == 0x1C {
                editor_newline();
                true
            } else if sc == 0x48 {
                unsafe { EDITOR_ROW = EDITOR_ROW.saturating_sub(1) };
                true
            } else if sc == 0x50 {
                unsafe { EDITOR_ROW = (EDITOR_ROW as usize + 1).min(EDITOR_ROWS - 1) as u8 };
                true
            } else if let Some(c) = crate::interrupts::scancode_to_char(sc) {
                editor_type(c);
                true
            } else {
                false
            }
        }
        _ => false,
    };
    if handled {
        dirty_kind(WinKind::App(id));
    }
    handled
}

/// Row selected by a window-local `y` inside a list of `count` rows.
fn app_row_at(y: usize, count: usize) -> Option<usize> {
    if y < APP_ROW_Y || y >= APP_ROW_Y + count * APP_ROW_H {
        return None;
    }
    Some((y - APP_ROW_Y) / APP_ROW_H)
}

/// Routes a body click inside an in-kernel app window to its controls.
fn app_click(win: &Window, x: usize, y: usize) {
    let WinKind::App(id) = win.kind else {
        return;
    };
    let (lx, ly) = (x.saturating_sub(win.x), y.saturating_sub(win.y));
    let handled = match id {
        APP_CALC => {
            let mut hit = false;
            for (i, key) in CALC_KEYS.iter().enumerate() {
                let bx = CALC_GRID_X + (i % CALC_COLS) * CALC_BTN_STEP_X;
                let by = CALC_GRID_Y + (i / CALC_COLS) * CALC_BTN_STEP_Y;
                if lx >= bx && lx < bx + CALC_BTN_W && ly >= by && ly < by + CALC_BTN_H {
                    calc_input(*key);
                    hit = true;
                    break;
                }
            }
            hit
        }
        APP_STORE => {
            let (bx, by, bw, bh) = STORE_BTN;
            if lx >= bx && lx < bx + bw && ly >= by && ly < by + bh {
                store_launch();
                true
            } else if let Some(row) = app_row_at(ly, STORE_IDS.len()) {
                unsafe { STORE_SEL = row as u8 };
                true
            } else {
                false
            }
        }
        APP_FILES | APP_FILES_ALT => {
            if let Some(row) = app_row_at(ly, FILE_ROWS.len()) {
                unsafe { FILES_SEL = row as u8 };
                true
            } else {
                false
            }
        }
        APP_EDITOR => {
            if let Some(row) = app_row_at(ly, EDITOR_ROWS) {
                unsafe { EDITOR_ROW = row as u8 };
                true
            } else {
                false
            }
        }
        _ => false,
    };
    if handled {
        dirty_kind(win.kind);
    }
}

/// Geometry `(x, y, w, h)` of the Start panel above the task bar.
fn start_pos(fb: &Framebuffer) -> (usize, usize, usize, usize) {
    let w = START_W.min(fb.width().saturating_sub(4));
    let h = START_HEADER_H + START_VISIBLE * START_ROW_H + START_PAD;
    let y = fb.height().saturating_sub(TASKBAR_H + h);
    (2, y, w, h)
}

/// Paints the Start panel: title, search field and the sorted, filtered list
/// with the selected row highlighted. Cyrillic labels use the same glyph path
/// as every other kernel string.
fn draw_start(fb: &Framebuffer) {
    if !start_open() {
        return;
    }
    let (x, y, w, h) = start_pos(fb);
    unsafe {
        vector::shadow_round(
            fb,
            x as i32,
            y as i32,
            w as i32,
            h as i32,
            WIN_R,
            5,
            95,
            0x00_00_03_07,
        );
        vector::fill_round(
            fb,
            x as i32,
            y as i32,
            w as i32,
            h as i32,
            WIN_R,
            vector::ALL,
            WIN_BG,
        );
        vector::stroke_round(
            fb,
            x as i32,
            y as i32,
            w as i32,
            h as i32,
            WIN_R,
            vector::ALL,
            vector::mix(WIN_BG, TEXT_DIM, 110),
        );
        vector::fill_round(
            fb,
            (x + 6) as i32,
            (y + 22) as i32,
            (w - 12) as i32,
            16,
            3,
            vector::ALL,
            BAR_MIN,
        );
    }
    tui::draw_text(fb, x + 8, y + 3, "Пуск", TEXT, WIN_BG, x + w - 8);
    tui::draw_text(
        fb,
        x + 144,
        y + 3,
        "^v Enter Esc",
        TEXT_DIM,
        WIN_BG,
        x + w - 6,
    );
    let mut query = String::from(start_query());
    query.push('_');
    tui::draw_text(fb, x + 10, y + 22, &query, TEXT, BAR_MIN, x + w - 10);
    let entries = start_entries();
    let filter = start_query();
    let mut row = 0usize;
    let sel = unsafe { START_SEL };
    let scroll = unsafe { START_SCROLL };
    for entry in entries.iter().flatten() {
        if !start_matches(entry, filter) {
            continue;
        }
        if row < scroll {
            row += 1;
            continue;
        }
        let visible = row - scroll;
        if visible >= START_VISIBLE {
            break;
        }
        let ry = y + START_HEADER_H + visible * START_ROW_H;
        let bg = if visible == sel { BAR_ON } else { WIN_BG };
        unsafe {
            fb.fill_rect(x + 4, ry, w - 8, START_ROW_H, bg);
        }
        tui::draw_text(fb, x + 8, ry, entry.title, TEXT, bg, x + w - 8);
        tui::draw_text(fb, x + 8, ry + 16, entry.subtitle, TEXT_DIM, bg, x + w - 8);
        row += 1;
    }
}

/// Click on a Start row activates it; clicks in the header only keep the panel
/// open so the user can continue typing a search.
fn start_click(fb: &Framebuffer, _x: usize, y: usize) {
    let (_px, py, _pw, _ph) = start_pos(fb);
    let Some(rel) = y.checked_sub(py + START_HEADER_H) else {
        return;
    };
    let row = rel / START_ROW_H;
    if row >= START_VISIBLE {
        return;
    }
    let entries = start_entries();
    let filter = start_query();
    let target = unsafe { START_SCROLL } + row;
    if let Some(i) = start_visible_index(&entries, filter, target) {
        unsafe {
            START_SEL = target;
        }
        let entry = entries[i].unwrap();
        close_start();
        activate_start(entry);
    }
}

/// Enters GUI mode. The idle loop calls this once at boot, so the desktop is
/// the only user-facing GUI; the `gui` shell command can re-enter it later.
pub fn enter() {
    if active() {
        return;
    }
    if let Some(fb) = console::framebuffer() {
        ensure_backbuffer(fb);
    }
    START_OPEN.store(false, Ordering::Relaxed);
    unsafe {
        START_SEL = 0;
        START_SCROLL = 0;
        START_QUERY_LEN = 0;
        START_QUERY[0] = 0;
    }
    raise_clients();
    ACTIVE.store(true, Ordering::Relaxed);
    dirty_all();
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
    START_OPEN.store(false, Ordering::Relaxed);
    unsafe {
        START_SEL = 0;
        START_SCROLL = 0;
        START_QUERY_LEN = 0;
        START_QUERY[0] = 0;
    }
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
    PIN_SINCE.store(0, Ordering::Relaxed);
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
/// - `0` — CREATE with `rsi` pointing at a [`CreateReq`] (returns the client
///   id, `u64::MAX` on any validation failure);
/// - `1` — PRESENT: the client's window is made dirty (and re-spawned if it
///   was closed, e.g. by the GUI `leave()`), so the composite picks up the new
///   pixels on the next `render()` — this is the ring-3 client's "flip";
/// - `2` — GET_EVENT: drains the single pending input event (returns the
///   [`EV_KEY`]/[`EV_CLICK`] packed word, [`EV_NONE`] when idle, `u64::MAX`
///   if the task owns no client window);
/// - anything else — `u64::MAX`.
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
        // GET_EVENT — drain the single pending input event.
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
    // The interactive crash dialog owns the keyboard until resolved.
    if crate::fault::pending() {
        return crash_key(sc);
    }
    // Modal dialog owns the keyboard until it is resolved.
    if modal_active() {
        return modal_key(sc);
    }
    // The Start panel owns the keyboard while visible: arrows, Enter, Esc and
    // printable search characters must never reach a window behind it.
    if start_open() {
        return start_key(sc);
    }
    if sc == 0x5B {
        // Left Win (USB path; PS/2 arrives as an extended code) toggles Start.
        toggle_start();
        return true;
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
    // Real in-kernel apps own their keys before the generic widget form: the
    // calculator, store, viewer and editor each consume digits, arrows and
    // editing keys instead of feeding a hidden demo form.
    if app_key(sc) {
        return true;
    }
    // The focused Widget Lab window routes keys into its focused widget
    // (v2.47.0): Tab cycles focus, Backspace edits, Enter activates, printable
    // keys type, and Escape dismisses an open dropdown.
    if widget_key(sc) {
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
        // Desktop function keys on plain make codes: `idle_loop` forwards only
        // non-extended make bytes, so F-keys need no release tracking and can
        // never collide with typing. Window controls moved to F10/F11/F12 so
        // F1-F9 stay reserved for the nine Start functions.
        0x3B => {
            open_app(0);
            return true;
        }
        0x3C => {
            open_app(1);
            return true;
        }
        0x3D => {
            open_app(2);
            return true;
        }
        0x3E => {
            open_app(3);
            return true;
        }
        0x3F => {
            open_app(4);
            return true;
        }
        0x40 => {
            open_app(5);
            return true;
        }
        0x41 => {
            open_app(6);
            return true;
        }
        0x42 => {
            open_app(7);
            return true;
        }
        0x43 => {
            open_app(8);
            return true;
        }
        0x44 => {
            // F10 — maximize/restore the focused window.
            if let Some(kind) = focused_kind() {
                toggle_maximize(kind);
            }
            return true;
        }
        0x57 => {
            // F11 — close the focused window (same path as Esc / X button).
            if let Some(i) = focused_idx() {
                close_window(i);
            }
            return true;
        }
        0x58 => {
            // F12 — minimize the focused window.
            if let Some(kind) = focused_kind() {
                minimize_window(kind);
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
            crate::kprintln!("[serial] [gui] note '{}' note_len={}", c, win.note_len);
        }
    }
    true
}

/// How long an unbroken stream of edge-pushing reports may last before the
/// pointer is suppressed (one second at any `TIMER_HZ`). Packet counts would
/// trip this gate ten times sooner at 1000 Hz than at 100 Hz.
const RUNAWAY_TICKS: u64 = TIMER_HZ;
/// Runaway gate: tick of the first uninterrupted edge-pushing report, or zero.
static PIN_SINCE: AtomicU64 = AtomicU64::new(0);
/// Runaway gate: while true, pointer input is suppressed until a packet moves
/// the cursor away from the pinned edge.
static PIN_LOCK: AtomicBool = AtomicBool::new(false);

/// Runaway-pointer gate (v2.38.33): decides whether one pointer report may be
/// processed (logged, applied, rendered) by the active screen owner. Real
/// hardware that streams garbage deltas — a desynced PS/2 touchpad next to a
/// USB mouse, or a misbehaving xHCI device — used to pin the arrow in the
/// bottom-left corner forever while every packet cost a serial line, a
/// formatted log and a full repaint (the lag/storm seen on the laptop).
///
/// One second (v2.79.0, previously 60 packets) of reports pushing an
/// already-clamped cursor further into an edge switches the gate off; it
/// re-arms only when a moving packet pushes the cursor away from that edge
/// (button-only reports stay swallowed so the phantom stream cannot click the
/// task bar). Each state transition logs one line, so a serial capture shows
/// exactly when and why input was suppressed.
pub fn input_allowed(dx: i32, dy: i32) -> bool {
    let Some(fb) = console::framebuffer() else {
        return true;
    };
    // Before the first packet the arrow has no position yet (it centres on
    // the next report) — nothing can be pinned, so let input through.
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
    let now = TICKS.load(Ordering::Relaxed);
    if PIN_LOCK.load(Ordering::Relaxed) {
        if moving && !pin {
            PIN_LOCK.store(false, Ordering::Relaxed);
            PIN_SINCE.store(0, Ordering::Relaxed);
            crate::kprintln!("[gui] pointer runaway released");
            return true;
        }
        return false;
    }
    if !pin {
        if moving {
            PIN_SINCE.store(0, Ordering::Relaxed);
        }
        return true;
    }
    let since = PIN_SINCE.load(Ordering::Relaxed);
    if since == 0 {
        PIN_SINCE.store(now.max(1), Ordering::Relaxed);
        return true;
    }
    let elapsed_ms = now.saturating_sub(since) * 1000 / TIMER_HZ;
    if now.saturating_sub(since) >= RUNAWAY_TICKS {
        PIN_LOCK.store(true, Ordering::Relaxed);
        crate::kprintln!(
            "[gui] pointer runaway: input suppressed ({} ms pushing into the edge)",
            elapsed_ms
        );
        return false;
    }
    true
}

/// Applies a mouse report: moves the arrow (clamped to the framebuffer),
/// drives the active drag — title-bar move or edge/corner resize — and
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
                if crate::fault::pending() {
                    crash_click(fb, nx, ny);
                } else if modal_active() {
                    modal_click(fb, nx, ny);
                } else {
                    dispatch_click(fb, nx, ny);
                }
            }
        }
    }
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

/// Whether `(x, y)` hits the bottom-left `☰ Пуск` button. Checked before the
/// window buttons so its hit zone never drifts with the open-window list.
fn hit_start_button(fb: &Framebuffer, x: usize, y: usize) -> bool {
    y >= fb.height().saturating_sub(TASKBAR_H) && x < START_BTN_W
}

fn hit_taskbar(fb: &Framebuffer, x: usize, y: usize) -> Option<usize> {
    if y < fb.height().saturating_sub(TASKBAR_H) {
        return None;
    }
    let mut bx = START_BTN_W + 4;
    for i in taskbar_slots() {
        if i == usize::MAX {
            break;
        }
        if let Some(win) = unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref() } {
            let w = 8 + tui::text_width(win_live_title(win));
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
/// so row one starts at 102 and row two at 132 — the 170 px window still
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
/// window — mode row at 104, address fields at 128..192, apply/test at 208,
/// Wi-Fi fields at 296/312 and the scan/connect row at 328.
static NETWORK_WIDGETS: [Widget; 15] = [
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
    Widget::Checkbox {
        x: 252,
        y: 208,
        w: 160,
        h: 16,
        id: 0,
        label: "Remember settings",
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

/// Widget Lab list rows (v2.47.0): ten palette names the list scrolls through.
static LAB_ITEMS: [&str; 10] = [
    "Crimson", "Amber", "Lime", "Teal", "Azure", "Indigo", "Violet", "Magenta", "Slate", "Ivory",
];

/// Widget Lab menu rows (v2.47.0): picking one runs its index as a command.
static LAB_MENU: [&str; 4] = ["Clear name", "Clear note", "Reset list", "Close"];

/// Widget table of the Widget Lab window (v2.47.0): two editable text fields, a
/// scrollable list with a bound scrollbar, a dropdown menu and a close button —
/// the first host that exercises the non-button widget variants and routed
/// keyboard input. Laid out for the 460x300 window: captions at 30/54, fields
/// at 48/72, the list at 100 (5 rows) with its bar at x=312, and the action row
/// at 192.
static LAB_WIDGETS: [Widget; 11] = [
    Widget::TextField {
        x: 8,
        y: 48,
        w: 300,
        h: 16,
        id: 0,
    },
    Widget::TextField {
        x: 8,
        y: 72,
        w: 300,
        h: 16,
        id: 1,
    },
    Widget::List {
        x: 8,
        y: 100,
        w: 300,
        h: 82,
        id: 2,
        items: &LAB_ITEMS,
    },
    Widget::Scrollbar {
        x: 312,
        y: 100,
        h: 82,
        list_id: 2,
    },
    Widget::Menu {
        x: 8,
        y: 192,
        w: 110,
        h: 20,
        id: 3,
        label: "Edit",
        items: &LAB_MENU,
    },
    Widget::Button {
        x: 124,
        y: 192,
        w: 96,
        h: 20,
        label: "Close",
        action: WidgetAction::CloseHost,
    },
    Widget::Checkbox {
        x: 8,
        y: 224,
        w: 152,
        h: 16,
        id: 0,
        label: "Auto-connect",
    },
    Widget::Checkbox {
        x: 168,
        y: 224,
        w: 152,
        h: 16,
        id: 1,
        label: "Persist config",
    },
    Widget::Radio {
        x: 8,
        y: 248,
        w: 132,
        h: 16,
        group: 0,
        value: 0,
        label: "DHCP",
    },
    Widget::Radio {
        x: 148,
        y: 248,
        w: 132,
        h: 16,
        group: 0,
        value: 1,
        label: "Static",
    },
    Widget::Slider {
        x: 8,
        y: 272,
        w: 300,
        h: 16,
        id: 0,
        min: 0,
        max: 100,
        label: "Volume",
    },
];

/// Widget Lab text-field capacity in bytes (ASCII, no terminator).
const LAB_FIELD_CAP: usize = 48;
/// Widget Lab text-field count (name, note).
const LAB_FIELD_N: usize = 2;
/// Widget Lab "nothing" sentinel for `focus`/`menu`.
const LAB_NO: u8 = 0xFF;
/// Visible rows of the Widget Lab list.
const LAB_ROWS: usize = 5;

/// Editable state of one widget host (v2.65.0). Originally the Widget Lab's
/// `LAB_FORM` (v2.47.0), it is now a per-window store: [`FORMS`] holds one
/// instance per built-in host, keyed by [`form_slot`], so a checkbox or radio
/// in the Network window never aliases the Widget Lab's state. `focus`/`menu`
/// name the widget that owns the keyboard; the routing ([`widget_key`]) is
/// generic and dispatches to whichever host is focused.
struct WidgetForm {
    /// Text buffers for field ids `0..LAB_FIELD_N`.
    fields: [[u8; LAB_FIELD_CAP]; LAB_FIELD_N],
    /// Used length of each field buffer.
    lens: [u8; LAB_FIELD_N],
    /// Widget id owning the keyboard, or [`LAB_NO`].
    focus: u8,
    /// Open menu id, or [`LAB_NO`].
    menu: u8,
    /// Selected list row.
    sel: u8,
    /// First visible list row.
    scroll: u8,
    /// Status line shown beside the header.
    msg: [u8; 20],
    /// Used length of `msg`.
    msg_len: u8,
    /// Checkbox checked bits (v2.63.0), addressed by `Widget::Checkbox::id`.
    checks: [bool; FORM_CHECK_N],
    /// Radio group selection (v2.64.0): `radios[g]` = the `value` of the selected
    /// radio in group `g`, or `LAB_NO` for none.
    radios: [u8; FORM_RADIO_N],
    /// Slider values (v2.67.0), addressed by `Widget::Slider::id`.
    sliders: [u8; FORM_SLIDER_N],
}

/// Checkbox count of a widget form (v2.63.0).
const FORM_CHECK_N: usize = 2;
/// Radio group count of a widget form (v2.64.0).
const FORM_RADIO_N: usize = 1;
/// Slider count of a widget form (v2.67.0).
const FORM_SLIDER_N: usize = 1;

/// One form slot per built-in host kind (v2.65.0).
const FORM_SLOTS: usize = 8;

/// Slot of `kind` in [`FORMS`]: built-in windows own a slot each, ring-3
/// clients share the last one (their widget table is host-driven and today
/// empty).
const fn form_slot(kind: WinKind) -> usize {
    match kind {
        WinKind::Welcome => 0,
        WinKind::System => 1,
        WinKind::Clock => 2,
        WinKind::About => 3,
        WinKind::Network => 4,
        WinKind::Tasks => 5,
        WinKind::Widgets => 6,
        WinKind::Client(_) | WinKind::App(_) => 7,
    }
}

/// Default (empty) widget form; the initializer of every [`FORMS`] slot.
const EMPTY_FORM: WidgetForm = WidgetForm {
    fields: [[0; LAB_FIELD_CAP]; LAB_FIELD_N],
    lens: [0; LAB_FIELD_N],
    focus: LAB_NO,
    menu: LAB_NO,
    sel: 0,
    scroll: 0,
    msg: [0; 20],
    msg_len: 0,
    checks: [false; FORM_CHECK_N],
    radios: [LAB_NO; FORM_RADIO_N],
    sliders: [0; FORM_SLIDER_N],
};

/// Per-window widget forms, one per [`form_slot`] (single-threaded GUI global).
static mut FORMS: [WidgetForm; FORM_SLOTS] = [EMPTY_FORM; FORM_SLOTS];

/// Mutable handle to `kind`'s widget form (single-threaded GUI global).
fn form(kind: WinKind) -> &'static mut WidgetForm {
    let forms = unsafe { &mut *core::ptr::addr_of_mut!(FORMS) };
    &mut forms[form_slot(kind)]
}

/// Resets the form of `kind` to its defaults; called when the window opens.
fn form_load(kind: WinKind) {
    let f = form(kind);
    for b in f.fields.iter_mut() {
        for c in b.iter_mut() {
            *c = 0;
        }
    }
    f.lens = [0; LAB_FIELD_N];
    f.focus = 0;
    f.menu = LAB_NO;
    f.sel = 0;
    f.scroll = 0;
    f.checks = [false; FORM_CHECK_N];
    f.radios = [LAB_NO; FORM_RADIO_N];
    f.sliders = [0; FORM_SLIDER_N];
    form_msg(kind, "ready");
}

/// Resets the Widget Lab form; called when its window opens.
fn lab_form_load() {
    form_load(WinKind::Widgets);
}

/// Mutable handle to the Widget Lab form (single-threaded GUI global).
fn lab_form() -> &'static mut WidgetForm {
    form(WinKind::Widgets)
}

/// Sets the status line of `kind`'s form (ASCII, truncated to the buffer).
fn form_msg(kind: WinKind, s: &str) {
    let f = form(kind);
    let n = s.len().min(f.msg.len());
    f.msg[..n].copy_from_slice(&s.as_bytes()[..n]);
    f.msg_len = n as u8;
}

/// Sets the Widget Lab status line (ASCII, truncated to the buffer).
fn lab_msg(s: &str) {
    form_msg(WinKind::Widgets, s);
}

/// Open Widget Lab menu id, or [`LAB_NO`].
fn lab_menu() -> u8 {
    lab_form().menu
}

/// Runs a picked menu row (v2.47.0): indices edit the demo form, `3` closes
/// the host window.
fn lab_menu_pick(index: usize) {
    crate::kprintln!("[gui] widget Widget Lab/menu item {} click", index);
    match index {
        0 => {
            let f = lab_form();
            f.lens[0] = 0;
            f.fields[0] = [0; LAB_FIELD_CAP];
            lab_msg("name cleared");
        }
        1 => {
            let f = lab_form();
            f.lens[1] = 0;
            f.fields[1] = [0; LAB_FIELD_CAP];
            lab_msg("note cleared");
        }
        2 => {
            let f = lab_form();
            f.sel = 0;
            f.scroll = 0;
            lab_msg("list reset");
        }
        3 => {
            if let Some(i) = find_kind(WinKind::Widgets) {
                close_window(i);
            }
        }
        _ => {}
    }
}

/// Selects the list row under a click and focuses the list.
fn lab_list_click(win: &Window, wy: usize, wh: usize, id: u8, items: &[&str], y: usize) {
    let ay = win.y + wy;
    if y < ay || y >= ay + wh {
        return;
    }
    let row = {
        let f = lab_form();
        let row = f.scroll as usize + (y - ay) / MENU_ROW_H;
        if row < items.len() {
            f.sel = row as u8;
        }
        f.focus = id;
        row
    };
    lab_msg("list");
    crate::kprintln!(
        "[gui] widget {}/list row {} click",
        win_label(win.kind, win.title),
        row
    );
    dirty_kind(win.kind);
}

/// Jumps the list scroll proportionally to a click on its scrollbar.
fn lab_scroll_click(win: &Window, wy: usize, wh: usize, y: usize) {
    let count = LAB_ITEMS.len();
    let max_scroll = count.saturating_sub(LAB_ROWS);
    if max_scroll == 0 {
        return;
    }
    let ay = win.y + wy;
    let rel = y.saturating_sub(ay).min(wh.saturating_sub(1));
    let scroll = (rel * max_scroll + wh / 2) / wh.max(1);
    let end = (scroll + LAB_ROWS - 1).min(count - 1);
    let f = lab_form();
    f.scroll = scroll as u8;
    if (f.sel as usize) < scroll {
        f.sel = scroll as u8;
    } else if (f.sel as usize) > end {
        f.sel = end as u8;
    }
    lab_msg("scroll");
    dirty_kind(win.kind);
}

/// Handles a click while a Widget Lab menu is open. Returns `true` when the
/// click was consumed by the dropdown; a click on the menu plate returns
/// `false` so the normal widget path toggles it, and a click elsewhere dismisses
/// the menu and falls through.
fn lab_dropdown_click(x: usize, y: usize) -> bool {
    let menu_id = lab_menu();
    if menu_id == LAB_NO {
        return false;
    }
    let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
    let Some(win) = wins.iter().flatten().find(|w| w.kind == WinKind::Widgets) else {
        lab_form().menu = LAB_NO;
        return false;
    };
    let mut hit_item = None;
    let mut on_plate = false;
    for wg in LAB_WIDGETS.iter() {
        if let Widget::Menu {
            x: wx,
            y: wy,
            w: ww,
            h: wh,
            id,
            items,
            ..
        } = *wg
        {
            if id != menu_id {
                continue;
            }
            let ax = win.x + wx;
            let ay = win.y + wy;
            if y >= ay + wh {
                let dw = menu_dropdown_w(items).max(ww);
                if y < ay + wh + items.len() * MENU_ROW_H && x >= ax && x < ax + dw {
                    hit_item = Some((y - (ay + wh)) / MENU_ROW_H);
                }
            }
            if x >= ax && x < ax + ww && y >= ay && y < ay + wh {
                on_plate = true;
            }
        }
    }
    if let Some(k) = hit_item {
        lab_menu_pick(k);
        lab_form().menu = LAB_NO;
        dirty_kind(WinKind::Widgets);
        return true;
    }
    if on_plate {
        return false;
    }
    lab_form().menu = LAB_NO;
    dirty_kind(WinKind::Widgets);
    false
}

/// Routes a key to the focused host's form (v2.47.0, generic since v2.66.0):
/// menu dismissal, `Ctrl+C`/`Ctrl+X`/`Ctrl+V` clipboard shortcuts, Tab field
/// cycling, Backspace editing, Enter activation and printable typing. Returns
/// `true` when the key was consumed by the focused window.
fn widget_key(sc: u8) -> bool {
    let Some(i) = focused_idx() else {
        return false;
    };
    let Some(host) = (unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref().map(|w| w.kind) }) else {
        return false;
    };
    if host == WinKind::Network {
        return false;
    }
    let f_menu = form_menu(host);
    if f_menu != LAB_NO && sc == 0x01 {
        form_menu_set(host, LAB_NO);
        dirty_kind(host);
        return true;
    }
    if f_menu != LAB_NO {
        form_menu_set(host, LAB_NO);
    }
    if crate::interrupts::CTRL_DOWN.load(Ordering::Relaxed) {
        match sc {
            0x2E => {
                let _ = form_clip_copy(host);
                dirty_kind(host);
                return true;
            }
            0x2D => {
                let _ = form_clip_cut(host);
                dirty_kind(host);
                return true;
            }
            0x2F => {
                let _ = form_clip_paste(host);
                dirty_kind(host);
                return true;
            }
            _ => {}
        }
    }
    let consumed = match sc {
        0x0F => {
            form_focus_cycle(host);
            true
        }
        0x0E => {
            form_backspace(host);
            true
        }
        0x1C => {
            form_enter(host);
            true
        }
        _ => {
            let Some(c) = crate::interrupts::scancode_to_char(sc) else {
                return false;
            };
            if c.is_control() {
                return false;
            }
            if crate::interrupts::CTRL_DOWN.load(Ordering::Relaxed) {
                return true;
            }
            form_type_char(host, c);
            true
        }
    };
    if consumed {
        dirty_kind(host);
    }
    consumed
}

/// Dropdown pixel width of a menu: the widest row plus padding.
fn menu_dropdown_w(items: &[&str]) -> usize {
    let max = items.iter().map(|s| s.len()).max().unwrap_or(1);
    max * console::GLYPH_W + 12
}

/// Widget table of a window kind; clients and built-ins without a demo table
/// get an empty slice.
fn widgets_for(kind: WinKind) -> &'static [Widget] {
    if kind == WinKind::Welcome {
        &WELCOME_WIDGETS
    } else if kind == WinKind::Network {
        &NETWORK_WIDGETS
    } else if kind == WinKind::Tasks {
        &TASK_WIDGETS
    } else if kind == WinKind::Widgets {
        &LAB_WIDGETS
    } else {
        &[]
    }
}

/// Index of the widget of `win` under the absolute point `(x, y)`, if any.
fn widget_index_at(win: &Window, x: usize, y: usize) -> Option<usize> {
    widgets_for(win.kind).iter().position(|wg| {
        let (bx, by, bw, bh) = widget_rect(wg);
        x >= win.x + bx && x < win.x + bx + bw && y >= win.y + by && y < win.y + by + bh
    })
}

/// The widget that would take a click at `(x, y)`, honouring the same
/// topmost-wins rule as [`dispatch_click`]: the first window containing the
/// point either yields its body widget or — when the point falls on its
/// title/edges — nothing, never falling through to a window underneath.
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
    // A Widget Lab dropdown highlights the row under the cursor, but the
    // dropdown is not a widget rect; while a menu is open, refresh the host on
    // any cursor move inside it so the highlight tracks live (v2.47.0).
    if !modal_active() && lab_menu() != LAB_NO {
        let (x, y) = unsafe { (*core::ptr::addr_of!(CUR_X), *core::ptr::addr_of!(CUR_Y)) };
        let wins = unsafe { &(*core::ptr::addr_of!(WINS)) };
        if let Some(win) = wins.iter().flatten().find(|w| w.kind == WinKind::Widgets) {
            if x >= win.x && x < win.x + win.w && y >= win.y && y < win.y + win.h {
                dirty_kind(WinKind::Widgets);
            }
        }
    }
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
/// serial proof line, then runs the action — open/focus a built-in window, or
/// close the hosting window through [`close_window`] (modal gate included).
/// The host index may have gone stale after [`focus_window`], so the action
/// resolves the target window by its [`WinKind`] instead.
fn widget_click(win: &Window, host_label: &str, wg: Widget, click_x: usize, y: usize) {
    let host = win.kind;
    match wg {
        Widget::Button { label, action, .. } => {
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
        Widget::TextField { id, .. } => {
            crate::kprintln!("[gui] widget {}/field {} focus", host_label, id);
            let f = form(win.kind);
            f.focus = id;
            form_msg(win.kind, "field");
            dirty_kind(win.kind);
        }
        Widget::List {
            y: wy,
            h: wh,
            id,
            items,
            ..
        } => {
            if win.kind == WinKind::Widgets {
                lab_list_click(win, wy, wh, id, items, y);
            }
        }
        Widget::Scrollbar { y: wy, h: wh, .. } => {
            if win.kind == WinKind::Widgets {
                lab_scroll_click(win, wy, wh, y);
            }
        }
        Widget::Menu { id, .. } => {
            if win.kind == WinKind::Widgets {
                let open = form_menu(win.kind) == id;
                form_menu_set(win.kind, if open { LAB_NO } else { id });
                crate::kprintln!(
                    "[gui] widget {}/menu {}",
                    host_label,
                    if open { "close" } else { "open" }
                );
                dirty_kind(win.kind);
            }
        }
        Widget::Checkbox { id, label, .. } => {
            let on = form_check_toggle(win.kind, id as usize);
            crate::kprintln!(
                "[gui] widget {}/check {} = {}",
                host_label,
                label,
                if on { "on" } else { "off" }
            );
            form_msg(win.kind, if on { "checked" } else { "unchecked" });
            dirty_kind(win.kind);
        }
        Widget::Radio {
            group,
            value,
            label,
            ..
        } => {
            let g = group as usize;
            form_radio_select(win.kind, g, value);
            crate::kprintln!("[gui] widget {}/radio {} = {}", host_label, label, value);
            form_msg(win.kind, label);
            dirty_kind(win.kind);
        }
        Widget::Slider {
            x,
            w,
            id,
            min,
            max,
            label,
            ..
        } => {
            let v = slider_value_at(win.x + x, w, min, max, click_x);
            form_slider_set(win.kind, id, v);
            crate::kprintln!("[gui] widget {}/slider {} = {}", host_label, label, v);
            form_msg(win.kind, "slider");
            dirty_kind(win.kind);
        }
    }
}

/// Repaints the window(s) of `kind` after a form change.
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
        match *wg {
            Widget::TextField { x, y, w, h, id } => {
                let (ax, ay) = (win.x + x, win.y + y);
                let hostk = win.kind;
                let focused = form_focus(hostk) == id;
                draw_field_box(fb, ax, ay, w, h, focused, hover == Some((hostk, wi)));
                let (val, count) = {
                    let f = form(hostk);
                    let i = id as usize;
                    if i < LAB_FIELD_N {
                        let bytes = &f.fields[i][..f.lens[i] as usize];
                        (core::str::from_utf8(bytes).unwrap_or(""), utf8_chars(bytes))
                    } else {
                        ("", 0)
                    }
                };
                let ty = ay + (h - console::GLYPH_H) / 2;
                if val.is_empty() {
                    let hint = if id == 0 { "name" } else { "note" };
                    tui::draw_text(fb, ax + 4, ty, hint, TEXT_DIM, NET_BOX_BG, ax + w - 4);
                } else {
                    draw_field_text(fb, ax + 4, ty, val, ax + w - 4);
                }
                if focused {
                    let cx = ax + 4 + count * console::GLYPH_W;
                    if cx + 2 < ax + w {
                        unsafe {
                            fb.fill_rect(cx, ay + 2, 2, h.saturating_sub(4), BAR_ON);
                        }
                    }
                }
            }
            Widget::List {
                x,
                y,
                w,
                h,
                id,
                items,
            } => {
                let (ax, ay) = (win.x + x, win.y + y);
                if win.kind == WinKind::Widgets {
                    draw_lab_list(fb, ax, ay, w, h, id, items);
                }
            }
            Widget::Scrollbar { x, y, h, list_id } => {
                let (ax, ay) = (win.x + x, win.y + y);
                if win.kind == WinKind::Widgets {
                    draw_lab_scrollbar(fb, ax, ay, h, list_id);
                }
            }
            Widget::Menu {
                x,
                y,
                w,
                h,
                id,
                label,
                items,
            } => {
                let (ax, ay) = (win.x + x, win.y + y);
                let open = win.kind == WinKind::Widgets && form_menu(win.kind) == id;
                let plate = if open || hover == Some((win.kind, wi)) {
                    BAR_ON
                } else {
                    BTN_BG
                };
                draw_plate(fb, ax, ay, w, h, label, plate);
                // A small down triangle at the right end signals a dropdown.
                unsafe {
                    let tx = ax + w - 12;
                    let ty = ay + h / 2;
                    for row in 0..3 {
                        fb.fill_rect(tx + row, ty, 5 - 2 * row, 1, TEXT);
                    }
                }
                if open {
                    let dw = menu_dropdown_w(items).max(w);
                    let (cx, cy) =
                        unsafe { (*core::ptr::addr_of!(CUR_X), *core::ptr::addr_of!(CUR_Y)) };
                    for (k, item) in items.iter().enumerate() {
                        let iy = ay + h + k * MENU_ROW_H;
                        let ihover = cx >= ax && cx < ax + dw && cy >= iy && cy < iy + MENU_ROW_H;
                        let bg = if ihover { BAR_ON } else { BTN_BG };
                        unsafe {
                            fb.fill_rect(ax, iy, dw, MENU_ROW_H, bg);
                            fb.fill_rect(ax, iy, dw, 1, vector::mix(bg, TEXT_DIM, 120));
                        }
                        tui::draw_text(
                            fb,
                            ax + 4,
                            iy + (MENU_ROW_H - console::GLYPH_H) / 2,
                            item,
                            TEXT,
                            bg,
                            (ax + dw).min(fb.width()),
                        );
                    }
                }
            }
            Widget::Button {
                x,
                y,
                w,
                h,
                label,
                action,
            } => {
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
                    draw_edge_box(fb, ax, ay, w, h, border);
                    continue;
                }
                let plate = if hover == Some((win.kind, wi)) {
                    BAR_ON
                } else {
                    BTN_BG
                };
                draw_plate(fb, ax, ay, w, h, label, plate);
            }
            Widget::Checkbox {
                x, y, h, id, label, ..
            } => {
                let (ax, ay) = (win.x + x, win.y + y);
                let on = form_checked(win.kind, id);
                let hovered = hover == Some((win.kind, wi));
                draw_checkbox(fb, ax, ay, h, label, on, hovered);
            }
            Widget::Radio {
                x,
                y,
                h,
                group,
                value,
                label,
                ..
            } => {
                let (ax, ay) = (win.x + x, win.y + y);
                let selected = form_radio(win.kind, group as usize) == value;
                let hovered = hover == Some((win.kind, wi));
                draw_radio(fb, ax, ay, h, label, selected, hovered);
            }
            Widget::Slider {
                x,
                y,
                w,
                h,
                id,
                min,
                max,
                label,
            } => {
                let (ax, ay) = (win.x + x, win.y + y);
                let value = form_slider(win.kind, id).clamp(min, max);
                let hovered = hover == Some((win.kind, wi));
                draw_slider(
                    fb,
                    SliderView {
                        ax,
                        ay,
                        w,
                        h,
                        label,
                        min,
                        max,
                        value,
                        hovered,
                    },
                );
            }
        }
    }
}

/// Height of one dropdown-menu row in pixels.
const MENU_ROW_H: usize = 16;

/// Paints a rounded button/menu plate with centered label text.
fn draw_plate(
    fb: &Framebuffer,
    ax: usize,
    ay: usize,
    w: usize,
    h: usize,
    label: &str,
    plate: Color,
) {
    unsafe {
        vector::fill_round(
            fb,
            ax as i32,
            ay as i32,
            w as i32,
            h as i32,
            4,
            vector::ALL,
            plate,
        );
    }
    let tx = ax + w.saturating_sub(tui::text_width(label)) / 2;
    let ty = ay + h.saturating_sub(console::GLYPH_H) / 2;
    tui::draw_text(fb, tx, ty, label, TEXT, plate, (ax + w).min(fb.width()));
}

/// Paints a checkbox (v2.63.0): a square box whose caption sits to its right.
/// The box border is [`BAR_ON`] when hovered, the interior shows a check mark
/// when on, and the caption is [`TEXT`] (dimmed while off).
fn draw_checkbox(
    fb: &Framebuffer,
    ax: usize,
    ay: usize,
    h: usize,
    label: &str,
    on: bool,
    hovered: bool,
) {
    let box_sz = h.min(12);
    let by = ay + (h - box_sz) / 2;
    let border = if hovered { BAR_ON } else { TEXT_DIM };
    unsafe {
        fb.fill_rect(ax, by, box_sz, box_sz, NET_BOX_BG);
    }
    draw_edge_box(fb, ax, by, box_sz, box_sz, border);
    if on {
        unsafe {
            let mut k = 0;
            while k < box_sz.saturating_sub(4) {
                fb.fill_rect(ax + 3 + k, by + 3 + k, 2, 2, TEXT);
                fb.fill_rect(ax + box_sz - 4 - k, by + 3 + k, 2, 2, TEXT);
                k += 2;
            }
        }
    }
    let tx = ax + box_sz + 6;
    let ty = ay + h.saturating_sub(console::GLYPH_H) / 2;
    let color = if on { TEXT } else { TEXT_DIM };
    tui::draw_text(fb, tx, ty, label, color, BTN_BG, fb.width());
}

/// Paints a radio button (v2.64.0): a circle outline with a filled dot when
/// selected; the caption brightens when selected.
fn draw_radio(
    fb: &Framebuffer,
    ax: usize,
    ay: usize,
    h: usize,
    label: &str,
    selected: bool,
    hovered: bool,
) {
    let r = h.min(12) / 2;
    let cx = ax + r + 1;
    let cy = ay + h / 2;
    let border = if hovered { BAR_ON } else { TEXT_DIM };
    for dy in 0..(2 * r + 3) {
        for dx in 0..(2 * r + 3) {
            let px = cx as i32 + dx as i32 - (r as i32 + 1);
            let py = cy as i32 + dy as i32 - (r as i32 + 1);
            if px < 0 || py < 0 {
                continue;
            }
            let dx2 = (dx as i32 - (r as i32 + 1)).abs();
            let dy2 = (dy as i32 - (r as i32 + 1)).abs();
            let dist2 = dx2 * dx2 + dy2 * dy2;
            let r2 = (r as i32 + 1) * (r as i32 + 1);
            let r12 = (r as i32) * (r as i32);
            if dist2 <= r2 && dist2 >= r12 {
                unsafe {
                    fb.fill_rect(px as usize, py as usize, 1, 1, border);
                }
            }
        }
    }
    if selected {
        unsafe {
            let ir = if r >= 3 { r - 2 } else { 1 };
            for dy in 0..(2 * ir + 1) {
                for dx in 0..(2 * ir + 1) {
                    let dx2 = dx as i32 - ir as i32;
                    let dy2 = dy as i32 - ir as i32;
                    if dx2 * dx2 + dy2 * dy2 <= (ir as i32) * (ir as i32) {
                        fb.fill_rect(cx + dx - ir, cy + dy - ir, 1, 1, TEXT);
                    }
                }
            }
        }
    }
    let tx = ax + 2 * r + 8;
    let ty = ay + h.saturating_sub(console::GLYPH_H) / 2;
    let color = if selected { TEXT } else { TEXT_DIM };
    tui::draw_text(fb, tx, ty, label, color, BTN_BG, fb.width());
}

/// Left inset of a slider's track from its widget origin (leaves room for the
/// caption).
const SLIDER_LABEL_W: usize = 96;
/// Right inset of a slider's track from its widget width (leaves room for the
/// numeric value).
const SLIDER_VALUE_W: usize = 40;
/// Thickness of the slider track in pixels.
const SLIDER_TRACK_H: usize = 4;
/// Side length of the slider thumb in pixels.
const SLIDER_THUMB: usize = 10;

/// Track `(x, width)` of a slider widget at origin `ax` with total width `w`.
fn slider_track(ax: usize, w: usize) -> (usize, usize) {
    (
        ax + SLIDER_LABEL_W,
        w.saturating_sub(SLIDER_LABEL_W + SLIDER_VALUE_W),
    )
}

/// Value a click at absolute `x` maps to on the track of a slider spanning
/// `min..=max`.
fn slider_value_at(ax: usize, w: usize, min: u8, max: u8, x: usize) -> u8 {
    let (tx, tw) = slider_track(ax, w);
    if tw == 0 || max <= min {
        return min;
    }
    let rel = x.clamp(tx, tx + tw) - tx;
    let span = (max - min) as usize;
    (min as usize + rel * span / tw).min(max as usize) as u8
}

/// One slider paint request (v2.68.0): geometry, caption and value. Grouping
/// them keeps `draw_slider` readable and under clippy's argument limit.
struct SliderView<'a> {
    ax: usize,
    ay: usize,
    w: usize,
    h: usize,
    label: &'a str,
    min: u8,
    max: u8,
    value: u8,
    hovered: bool,
}

/// Paints a slider (v2.67.0): caption on the left, a sunken track with the
/// filled portion in [`BAR_ON`], a [`TEXT`] thumb at the value, and the numeric
/// value on the right.
fn draw_slider(fb: &Framebuffer, view: SliderView<'_>) {
    let SliderView {
        ax,
        ay,
        w,
        h,
        label,
        min,
        max,
        value,
        hovered,
    } = view;
    let ty = ay + h.saturating_sub(console::GLYPH_H) / 2;
    let label_fg = if hovered { TEXT } else { TEXT_DIM };
    tui::draw_text(fb, ax, ty, label, label_fg, WIN_BG, ax + SLIDER_LABEL_W);
    let (tx, tw) = slider_track(ax, w);
    if tw == 0 {
        return;
    }
    let cy = ay + h / 2;
    let span = (max.saturating_sub(min)).max(1) as usize;
    let off = ((value.saturating_sub(min)) as usize * tw / span).min(tw);
    let ty0 = cy.saturating_sub(SLIDER_TRACK_H / 2);
    unsafe {
        fb.fill_rect(tx, ty0, tw, SLIDER_TRACK_H, BTN_BG);
        fb.fill_rect(tx, ty0, off, SLIDER_TRACK_H, BAR_ON);
    }
    draw_edge_box(
        fb,
        tx,
        ty0,
        tw,
        SLIDER_TRACK_H,
        if hovered { BAR_ON } else { TEXT_DIM },
    );
    let thx = (tx + off).saturating_sub(SLIDER_THUMB / 2);
    unsafe {
        fb.fill_rect(
            thx,
            cy.saturating_sub(SLIDER_THUMB / 2),
            SLIDER_THUMB,
            SLIDER_THUMB,
            TEXT,
        );
    }
    draw_edge_box(
        fb,
        thx,
        cy.saturating_sub(SLIDER_THUMB / 2),
        SLIDER_THUMB,
        SLIDER_THUMB,
        TEXT_DIM,
    );
    let mut buf = [0u8; 3];
    let mut n = value as usize;
    let mut len = 0usize;
    if n == 0 {
        buf[0] = b'0';
        len = 1;
    } else {
        while n > 0 && len < buf.len() {
            buf[len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        buf[..len].reverse();
    }
    let val = core::str::from_utf8(&buf[..len]).unwrap_or("0");
    tui::draw_text(fb, tx + tw + 6, ty, val, TEXT, WIN_BG, ax + w);
}

/// Paints a 1px rectangular border in `border`.
fn draw_edge_box(fb: &Framebuffer, ax: usize, ay: usize, w: usize, h: usize, border: Color) {
    unsafe {
        fb.fill_rect(ax, ay, w, 1, border);
        fb.fill_rect(ax, ay + h - 1, w, 1, border);
        fb.fill_rect(ax, ay, 1, h, border);
        fb.fill_rect(ax + w - 1, ay, 1, h, border);
    }
}

/// Paints a text-field box: a filled [`NET_BOX_BG`] surface with a border that
/// is [`BAR_ON`] when focused, [`TEXT`] when hovered, [`TEXT_DIM`] otherwise.
fn draw_field_box(
    fb: &Framebuffer,
    ax: usize,
    ay: usize,
    w: usize,
    h: usize,
    focused: bool,
    hovered: bool,
) {
    unsafe {
        fb.fill_rect(ax, ay, w, h, NET_BOX_BG);
    }
    let border = if focused {
        BAR_ON
    } else if hovered {
        TEXT
    } else {
        TEXT_DIM
    };
    draw_edge_box(fb, ax, ay, w, h, border);
}

/// Number of UTF-8 scalars in `bytes`: every byte that is not a continuation
/// byte (`10xxxxxx`) starts one scalar.
fn utf8_chars(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| (**b & 0xC0) != 0x80).count()
}

/// Paints a text field's value one glyph cell per scalar. ASCII and Cyrillic
/// scalars use the kernel's shared Unicode glyph path; the string never crosses
/// `max_px`, so it stays inside the field frame.
fn draw_field_text(fb: &Framebuffer, x: usize, y: usize, s: &str, max_px: usize) {
    let mut px = x;
    for ch in s.chars() {
        if px + console::GLYPH_W > max_px {
            break;
        }
        tui::draw_char(fb, px, y, ch, TEXT, NET_BOX_BG);
        px += console::GLYPH_W;
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
    if hit_start_button(fb, x, y) {
        toggle_start();
        return;
    }
    if start_open() {
        let (px, py, pw, ph) = start_pos(fb);
        if x >= px && x < px + pw && y >= py && y < py + ph {
            start_click(fb, x, y);
        } else {
            close_start();
        }
        return;
    }
    if hit_globe(fb, x, y) {
        crate::kprintln!("[gui] tray globe click");
        open_or_focus(WinKind::Network);
        return;
    }
    if let Some(i) = hit_taskbar(fb, x, y) {
        focus_window(i);
        return;
    }
    // An open Widget Lab dropdown swallows its item clicks before ordinary
    // hit-testing; the menu plate itself falls through to toggle normally.
    if lab_dropdown_click(x, y) {
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
                // event. Captured before `focus_window` moves slots around —
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
                    widget_click(win, &label, wg, x, y);
                    hit_any = true;
                    break;
                }
                // In-kernel apps hit-test their own buttons and list rows
                // before the generic Task Manager row logic.
                app_click(win, x, y);
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
/// complete frame or the new complete one, which — together with the damage
/// culling — removes both the full-screen flicker and torn rows. If the
/// backbuffer is unavailable the renderer falls back to painting directly into
/// VRAM (identical damage logic, v2.38.23 behaviour).
///
/// The partial path repaints desktop, then the task-bar strip (redrawn whole
/// if touched), then every window whose rectangle overlaps the damage (drawn
/// bottom-up so the z-order stays correct), then Start, then the arrow last.
/// Idle repaints are therefore limited to the live telemetry windows instead
/// of the whole screen.
/// Desktop background colour of scanline `row`: the slow vertical wash from
/// [`DESK_TOP`] to [`DESK_BOT`]. Channel steps truncate toward zero, which
/// makes the screen midpoint land exactly on the classic flat desktop tone.
fn desk_color(row: usize, h: usize) -> Color {
    let span = (h.max(2) - 1) as i32;
    let k = row.min(h.saturating_sub(1)) as i32;
    let ch = |shift: u32| -> u32 {
        let t = ((DESK_TOP >> shift) & 0xff) as i32;
        let b = ((DESK_BOT >> shift) & 0xff) as i32;
        (t + ((b - t) * k) / span) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

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
    let screen_h = vram.height();
    for row in ry0..(ry0 + rh) {
        unsafe {
            target.fill_rect(rx0, row, rw, 1, desk_color(row, screen_h));
        }
    }
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
    // Start is the topmost desktop surface; it stays above open windows and
    // below the modal/crash dialogs.
    if start_open() {
        let (sx, sy, sw, sh) = start_pos(target);
        if full || rect_overlaps(&d, sx, sy, sw, sh) {
            draw_start(target);
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
    // The interactive crash dialog paints above everything, modal included.
    if crate::fault::pending() {
        let (cx, cy) = crash_pos(target);
        if full || rect_overlaps(&d, cx, cy, CDLG_W, CDLG_H) {
            draw_crash_dialog(target);
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

fn draw_taskbar(fb: &Framebuffer) {
    let y = fb.height().saturating_sub(TASKBAR_H);
    unsafe {
        fb.fill_rect_vgrad(0, y, fb.width(), TASKBAR_H, BAR_TOP, BAR_BG);
        fb.fill_rect(0, y, fb.width(), 1, vector::mix(BAR_TOP, TEXT_DIM, 110));
    }
    // Bottom-left Start button: the three bars are vector-painted because the
    // glyph font has no U+2630 box, the label uses the Cyrillic glyph table.
    let start_bg = if start_open() { BAR_ON } else { TITLE_ON };
    unsafe {
        vector::fill_round(
            fb,
            2,
            y as i32,
            (START_BTN_W - 4) as i32,
            (TASKBAR_H - 1) as i32,
            3,
            vector::ALL,
            start_bg,
        );
        for row in 0..3 {
            fb.fill_rect(8, y + 4 + row * 4, 12, 2, TEXT);
        }
    }
    tui::draw_text(fb, 26, y, "Пуск", TEXT, start_bg, START_BTN_W);
    let mut bx = START_BTN_W + 4;
    for i in taskbar_slots() {
        if i == usize::MAX {
            break;
        }
        if let Some(win) = unsafe { (*core::ptr::addr_of!(WINS))[i].as_ref() } {
            let title = win_live_title(win);
            let w = 8 + tui::text_width(title);
            let bg = if win.focused {
                BAR_ON
            } else if win.minimized {
                BAR_MIN
            } else {
                BAR_BG
            };
            unsafe {
                vector::fill_round(
                    fb,
                    bx as i32,
                    y as i32,
                    w as i32,
                    (TASKBAR_H - 1) as i32,
                    3,
                    vector::ALL,
                    bg,
                );
            }
            tui::draw_text(fb, bx + 4, y, title, TEXT, bg, bx + w);
            bx += w + 4;
        }
    }
    // Tray: a recessed pill holding the state globe (circle + ring +
    // meridian + equator), painted inside [width-20, width-4).
    let gx = fb.width().saturating_sub(20);
    let cx = fb.width().saturating_sub(12);
    let cy = (y + TASKBAR_H / 2) as i32;
    let color = globe_color();
    unsafe {
        vector::fill_round(
            fb,
            gx as i32,
            (y + 1) as i32,
            16,
            (TASKBAR_H - 2) as i32,
            4,
            vector::ALL,
            BAR_MIN,
        );
        vector::fill_circle(fb, cx as i32, cy, 5, color);
        vector::stroke_ellipse(fb, cx as i32, cy, 5, 5, vector::mix(color, TEXT, 80));
        vector::stroke_ellipse(fb, cx as i32, cy, 2, 5, vector::mix(color, TEXT, 45));
        vector::line(
            fb,
            cx as i32 - 5,
            cy,
            cx as i32 + 5,
            cy,
            vector::mix(color, TEXT, 45),
        );
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
            let t = TICKS.load(Ordering::Relaxed) / (TIMER_HZ / 20);
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
    let (vx, vy) = (win.x as i32, win.y as i32);
    let (vw, vh) = (win.w as i32, win.h as i32);
    let title_i = TITLE_H as i32;
    unsafe {
        vector::shadow_round(fb, vx, vy, vw, vh, WIN_R, 5, 95, 0x00_00_03_07);
        vector::fill_round(fb, vx, vy, vw, vh, WIN_R, vector::ALL, WIN_BG);
        vector::fill_round(fb, vx, vy, vw, title_i, WIN_R, vector::TOP, title_bg);
        vector::fill_round(
            fb,
            vx,
            vy + title_i - 1,
            vw,
            1,
            0,
            0,
            vector::mix(title_bg, TEXT, 40),
        );
    }
    // Body text clips at the window's own right edge (resize-safe); the
    // title text additionally stops before the three-button cluster.
    let edge = (win.x + win.w).min(fb.width());
    let max_px = edge.saturating_sub(6);
    let title_max = edge.min(win.x + win.w.saturating_sub(TITLE_BTNS + 2));
    tui::draw_text(fb, win.x + 6, win.y + 1, title, TEXT, title_bg, title_max);
    // Right-anchored cluster: minimize [w-48,w-32), maximize [w-32,w-16),
    // close [w-16,w). The close plate carries the window's top-right corner
    // radius so it stays flush with the rounded frame.
    let bx = win.x + win.w.saturating_sub(TITLE_BTNS);
    unsafe {
        vector::fill_round(
            fb,
            bx as i32,
            vy,
            (TITLE_BTNS - TITLE_BTN) as i32,
            title_i,
            0,
            0,
            BTN_BG,
        );
        vector::fill_round(
            fb,
            (bx + TITLE_BTN) as i32,
            vy,
            TITLE_BTN as i32,
            title_i,
            WIN_R,
            vector::TR,
            CLOSE_BG,
        );
    }
    let mx = bx + TITLE_BTN;
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
    // Edge definition: a soft 1px outline straddling the rounded frame.
    unsafe {
        vector::stroke_round(
            fb,
            vx,
            vy,
            vw,
            vh,
            WIN_R,
            vector::ALL,
            vector::mix(WIN_BG, TEXT_DIM, 110),
        );
    }
    let mut y = win.y + TITLE_H + 8;
    match win.kind {
        WinKind::Welcome => draw_welcome(fb, win, &mut y, max_px),
        WinKind::System => draw_system(fb, win.x + 8, &mut y, max_px),
        WinKind::Clock => draw_clock(fb, win.x + 8, &mut y, max_px),
        WinKind::About => draw_about(fb, win.x + 8, &mut y, max_px),
        WinKind::Network => draw_network(fb, win, max_px),
        WinKind::Tasks => draw_tasks(fb, win, max_px),
        WinKind::Widgets => draw_lab(fb, win, max_px),
        WinKind::Client(_) => draw_client(fb, win),
        WinKind::App(id) => draw_app(fb, win, &mut y, max_px, id),
    }
    draw_widgets(fb, win);
}

/// Paints the modal confirm dialog: title bar, the close question, the hint
/// lines and the centered `OK` / `Cancel` pair — [`modal_click`] hits exactly
/// these two rectangles.
fn draw_modal(fb: &Framebuffer, kind: WinKind, title: &'static str) {
    let (x, y) = modal_pos(fb);
    let max = x + DLG_W - 6;
    let (vx, vy) = (x as i32, y as i32);
    unsafe {
        vector::shadow_round(
            fb,
            vx,
            vy,
            DLG_W as i32,
            DLG_H as i32,
            WIN_R,
            6,
            110,
            0x00_00_03_07,
        );
        vector::fill_round(
            fb,
            vx,
            vy,
            DLG_W as i32,
            DLG_H as i32,
            WIN_R,
            vector::ALL,
            WIN_BG,
        );
        vector::fill_round(
            fb,
            vx,
            vy,
            DLG_W as i32,
            TITLE_H as i32,
            WIN_R,
            vector::TOP,
            TITLE_ON,
        );
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
        vector::fill_round(
            fb,
            (x + DLG_OK_X) as i32,
            ry as i32,
            DLG_OK_W as i32,
            DLG_BTN_H as i32,
            4,
            vector::ALL,
            BAR_ON,
        );
        vector::fill_round(
            fb,
            (x + DLG_CANCEL_X) as i32,
            ry as i32,
            DLG_CANCEL_W as i32,
            DLG_BTN_H as i32,
            4,
            vector::ALL,
            BTN_BG,
        );
    }
    modal_btn_label(fb, "OK", x + DLG_OK_X, ry, DLG_OK_W, BAR_ON);
    modal_btn_label(fb, "Cancel", x + DLG_CANCEL_X, ry, DLG_CANCEL_W, BTN_BG);
}

/// Draws the interactive crash dialog over the desktop (above every window and
/// the confirm modal). Three recovery plates mirror [`crash_key`] /
/// [`crash_click`]; the faulting task stays frozen until one is chosen.
fn draw_crash_dialog(fb: &Framebuffer) {
    let (x, y) = crash_pos(fb);
    let max = x + CDLG_W - 6;
    let (vx, vy) = (x as i32, y as i32);
    unsafe {
        vector::shadow_round(
            fb,
            vx,
            vy,
            CDLG_W as i32,
            CDLG_H as i32,
            WIN_R,
            6,
            110,
            0x00_00_03_07,
        );
        vector::fill_round(
            fb,
            vx,
            vy,
            CDLG_W as i32,
            CDLG_H as i32,
            WIN_R,
            vector::ALL,
            WIN_BG,
        );
        vector::fill_round(
            fb,
            vx,
            vy,
            CDLG_W as i32,
            TITLE_H as i32,
            WIN_R,
            vector::TOP,
            TITLE_CRASH,
        );
    }
    let hang = matches!(crate::fault::peek(), Some(r) if r.kind == crate::fault::FaultCode::Hang);
    tui::draw_text(
        fb,
        x + 6,
        y + 1,
        if hang { "Task Hung" } else { "Task Error" },
        TEXT,
        TITLE_CRASH,
        max,
    );
    let mut ly = y + TITLE_H + 8;
    let budget_locked = crate::fault::over_budget_current();
    let (task_line, snap_line, hint_line) = match crate::fault::peek() {
        Some(record) => {
            let snap = &record.snapshot;
            (
                format!(
                    "pid {} {}: {} (crash #{}/{})",
                    snap.pid,
                    if hang { "hung" } else { "faulted" },
                    record.kind.name(),
                    record.count,
                    crate::fault::MAX_FAULTS_PER_TASK
                ),
                format!(
                    "snap rip={:#x} rsp={:#x} hash={:#x}",
                    snap.rip, snap.rsp, snap.state_hash
                ),
                if budget_locked {
                    "fault budget exceeded: only T = Terminate"
                } else {
                    "R = Rollback   T = Terminate   Enter/S = Resume"
                },
            )
        }
        None => (
            String::from("fault in an unknown task"),
            String::from("waiting for record..."),
            if budget_locked {
                "fault budget exceeded: only T = Terminate"
            } else {
                "R = Rollback   T = Terminate   Enter/S = Resume"
            },
        ),
    };
    let task_line = {
        let q = crate::fault::queued();
        if q > 1 {
            format!("{}   (+{} more queued)", task_line, q - 1)
        } else {
            task_line
        }
    };
    tui::draw_text(fb, x + 12, ly, &task_line, TEXT_HOT, WIN_BG, max);
    ly += console::GLYPH_H + 6;
    tui::draw_text(fb, x + 12, ly, &snap_line, TEXT, WIN_BG, max);
    ly += console::GLYPH_H + 6;
    tui::draw_text(fb, x + 12, ly, hint_line, TEXT_DIM, WIN_BG, max);
    let ((rx, ry), (tx, ty), (sx, sy)) = crash_btn_rects(fb);
    // Over the fault budget only the Terminate plate stays armed; Rollback and
    // Resume are drawn dim so the only actionable path is visually obvious.
    let (rb_bg, rs_bg, tm_bg) = if budget_locked {
        (BTN_BG, BTN_BG, BAR_ON)
    } else {
        (BAR_ON, BAR_ON, BTN_BG)
    };
    unsafe {
        vector::fill_round(
            fb,
            rx as i32,
            ry as i32,
            CDLG_RB_W as i32,
            CDLG_BTN_H as i32,
            4,
            vector::ALL,
            rb_bg,
        );
        vector::fill_round(
            fb,
            tx as i32,
            ty as i32,
            CDLG_TM_W as i32,
            CDLG_BTN_H as i32,
            4,
            vector::ALL,
            tm_bg,
        );
        vector::fill_round(
            fb,
            sx as i32,
            sy as i32,
            CDLG_RS_W as i32,
            CDLG_BTN_H as i32,
            4,
            vector::ALL,
            BAR_ON,
        );
    }
    modal_btn_label(fb, "Rollback", rx, ry, CDLG_RB_W, rb_bg);
    modal_btn_label(fb, "Terminate", tx, ty, CDLG_TM_W, tm_bg);
    modal_btn_label(fb, "Resume", sx, sy, CDLG_RS_W, rs_bg);
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
    let tx = rx + rw.saturating_sub(tui::text_width(label)) / 2;
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
    let temp = if crate::thermal::simulated() {
        format!(
            "temp {}c~  lid {} (simulated)",
            crate::thermal::temp_c(),
            tui::lid_state()
        )
    } else {
        format!(
            "temp {}c  lid {}",
            crate::thermal::temp_c(),
            tui::lid_state()
        )
    };
    text_line(fb, x, y, &temp, TEXT, WIN_BG, max_px);
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

/// Body of a Start-catalog application window. Implemented programs render
/// live interactive content; entries whose runtime needs a host keep the
/// honest capability note from [`APPS`].
fn draw_app(fb: &Framebuffer, win: &Window, y: &mut usize, max_px: usize, id: u8) {
    match id {
        APP_CALC => draw_calc(fb, win, max_px),
        APP_STORE => draw_store(fb, win, max_px),
        APP_FILES | APP_FILES_ALT => draw_files(fb, win, max_px),
        APP_EDITOR => draw_editor(fb, win, max_px),
        _ => {
            let x = win.x + 8;
            let Some(app) = APPS.get(id as usize) else {
                return;
            };
            text_line(fb, x, y, app.title, colors::OK, WIN_BG, max_px);
            *y += 4;
            for line in app.body {
                text_line(fb, x, y, line, TEXT, WIN_BG, max_px);
            }
        }
    }
}

/// Paints the calculator display and its clickable button grid.
fn draw_calc(fb: &Framebuffer, win: &Window, max_px: usize) {
    let x = win.x + 8;
    let y = win.y + TITLE_H + 8;
    let w = win.w.saturating_sub(16);
    unsafe {
        vector::fill_round(
            fb,
            x as i32,
            y as i32,
            w as i32,
            20,
            3,
            vector::ALL,
            NET_BOX_BG,
        );
    }
    let value = calc_text();
    let tw = tui::text_width(value).min(w.saturating_sub(8));
    tui::draw_text(
        fb,
        x + w.saturating_sub(tw + 4),
        y + 6,
        value,
        colors::OK,
        NET_BOX_BG,
        x + w,
    );
    for (i, key) in CALC_KEYS.iter().enumerate() {
        let bx = win.x + CALC_GRID_X + (i % CALC_COLS) * CALC_BTN_STEP_X;
        let by = win.y + CALC_GRID_Y + (i / CALC_COLS) * CALC_BTN_STEP_Y;
        unsafe {
            vector::fill_round(
                fb,
                bx as i32,
                by as i32,
                CALC_BTN_W as i32,
                CALC_BTN_H as i32,
                3,
                vector::ALL,
                BTN_BG,
            );
        }
        let mut buf = [0u8; 4];
        let label = key.encode_utf8(&mut buf);
        let lw = tui::text_width(label);
        tui::draw_text(
            fb,
            bx + CALC_BTN_W.saturating_sub(lw) / 2,
            by + 6,
            label,
            TEXT,
            BTN_BG,
            bx + CALC_BTN_W,
        );
    }
    tui::draw_text(
        fb,
        x,
        win.y + 154,
        "C = clear  Backspace = delete  integer math",
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}

/// Paints the App Store catalog with the selected row and Run button.
fn draw_store(fb: &Framebuffer, win: &Window, max_px: usize) {
    let sel = unsafe { STORE_SEL as usize };
    for (i, &id) in STORE_IDS.iter().enumerate() {
        let row_y = win.y + APP_ROW_Y + i * APP_ROW_H;
        let (bg, fg) = if i == sel {
            (BAR_ON, TEXT)
        } else {
            (WIN_BG, TEXT_DIM)
        };
        unsafe {
            fb.fill_rect(win.x + 4, row_y, win.w.saturating_sub(8), APP_ROW_H - 1, bg);
        }
        let title = APPS.get(id as usize).map(|a| a.title).unwrap_or("app");
        tui::draw_text(fb, win.x + 10, row_y + 3, title, fg, bg, max_px);
    }
    let (bx, by, bw, bh) = STORE_BTN;
    unsafe {
        vector::fill_round(
            fb,
            (win.x + bx) as i32,
            (win.y + by) as i32,
            bw as i32,
            bh as i32,
            3,
            vector::ALL,
            BTN_BG,
        );
    }
    tui::draw_text(
        fb,
        win.x + bx + 10,
        win.y + by + 6,
        "Запуск (Enter)",
        TEXT,
        BTN_BG,
        win.x + bx + bw,
    );
    tui::draw_text(
        fb,
        win.x + 8,
        win.y + 162,
        "↑/↓ выбор · Enter запуск",
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
}

/// Content of one Files pseudo-file, generated from live kernel state.
fn files_preview(row: usize) -> String {
    match row {
        0 => String::from(tui::VERSION),
        1 => uptime_hms(),
        2 => format!("ticks {} at {} Hz", TICKS.load(Ordering::Relaxed), TIMER_HZ),
        3 => format!("frames {}", crate::memory::frames_allocated()),
        4 => {
            let clients = (0..MAX_CLIENTS as u8)
                .filter(|&id| client_slot(id).is_some())
                .count();
            format!("windows {} clients {}", window_count(), clients)
        }
        5 => format!(
            "ps2 {} usb {}",
            crate::ps2::key_seq(),
            crate::xhci::KEY_SEQ.load(Ordering::Relaxed)
        ),
        _ => String::new(),
    }
}

/// Paints the Files list and the live preview of the selected pseudo-file.
fn draw_files(fb: &Framebuffer, win: &Window, max_px: usize) {
    let sel = unsafe { FILES_SEL as usize };
    for (i, name) in FILE_ROWS.iter().enumerate() {
        let row_y = win.y + APP_ROW_Y + i * APP_ROW_H;
        let (bg, fg) = if i == sel {
            (BAR_ON, TEXT)
        } else {
            (WIN_BG, TEXT_DIM)
        };
        unsafe {
            fb.fill_rect(win.x + 4, row_y, win.w.saturating_sub(8), APP_ROW_H - 1, bg);
        }
        tui::draw_text(fb, win.x + 10, row_y + 3, name, fg, bg, max_px);
    }
    tui::draw_text(
        fb,
        win.x + 8,
        win.y + 130,
        "--- preview ---",
        TEXT_DIM,
        WIN_BG,
        max_px,
    );
    tui::draw_text(
        fb,
        win.x + 8,
        win.y + 146,
        &files_preview(sel),
        colors::OK,
        WIN_BG,
        max_px,
    );
}

/// Paints the six editable editor lines with a cursor on the active line.
fn draw_editor(fb: &Framebuffer, win: &Window, max_px: usize) {
    let active = unsafe { EDITOR_ROW as usize };
    for row in 0..EDITOR_ROWS {
        let row_y = win.y + APP_ROW_Y + row * APP_ROW_H;
        let bg = if row == active { BAR_ON } else { WIN_BG };
        unsafe {
            fb.fill_rect(win.x + 4, row_y, win.w.saturating_sub(8), APP_ROW_H - 1, bg);
        }
        let line = editor_line(row);
        tui::draw_text(fb, win.x + 8, row_y + 3, line, TEXT, bg, max_px);
        if row == active {
            let cx = win.x + 8 + tui::text_width(line);
            tui::draw_text(fb, cx, row_y + 3, "_", colors::OK, bg, max_px);
        }
    }
    tui::draw_text(
        fb,
        win.x + 8,
        win.y + 134,
        "↑/↓ строка · Enter новая · Backspace удалить",
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
    /// Last button-feedback word shown right of the mode buttons (16 bytes
    /// since v2.38.36, sized for `no wifi card`).
    msg: [u8; 16],
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
    msg: [0; 16],
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
        msg: [0; 16],
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
    crate::kprintln!("[serial] [gui] backspace focus={} len={}", d.focus, *len);
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
        NetBtn::Scan => match crate::wifi::start_scan() {
            Ok(()) => net_set_msg("scanning..."),
            Err(e) => net_set_msg(e),
        },
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
            unsafe {
                let d = &*core::ptr::addr_of!(NET_DRAFT);
                crate::kprintln!(
                    "[serial] [gui] type '{}' focus={} ssid_len={} pass_len={}",
                    c,
                    d.focus,
                    d.ssid_len,
                    d.pass_len
                );
            }
            dirty_kind(WinKind::Network);
            true
        }
    }
}

/// Body painter of the Widget Lab (v2.47.0): the header, the hint/status lines.
/// The field frames, list, scrollbar, menu and slider are painted afterwards by
/// [`draw_widgets`], and each field draws its own value and caret.
fn draw_lab(fb: &Framebuffer, win: &Window, max_px: usize) {
    let x = win.x + 8;
    let at = |y: usize, s: &str, fg: Color| {
        tui::draw_text(fb, x, win.y + y, s, fg, WIN_BG, max_px);
    };
    at(TITLE_H + 8, "Widget Lab", TEXT);
    at(300, "Tab focus  n/p list  enter pick", TEXT_DIM);
    at(314, "ctrl+c/x/v copy/cut/paste", TEXT_DIM);
    if lab_menu() == LAB_NO {
        let f = lab_form();
        if f.msg_len > 0 {
            let m = core::str::from_utf8(&f.msg[..f.msg_len as usize]).unwrap_or("");
            tui::draw_text(
                fb,
                win.x + 200,
                win.y + TITLE_H + 8,
                m,
                TEXT,
                WIN_BG,
                max_px,
            );
        }
    }
}

/// Paints the Widget Lab list: a sunken box with the visible rows, the selected
/// row highlighted, at most `LAB_ROWS` at a time from the scroll offset.
fn draw_lab_list(
    fb: &Framebuffer,
    ax: usize,
    ay: usize,
    w: usize,
    h: usize,
    id: u8,
    items: &[&str],
) {
    unsafe {
        fb.fill_rect(ax, ay, w, h, NET_BOX_BG);
    }
    draw_edge_box(fb, ax, ay, w, h, TEXT_DIM);
    let (focused, scroll, sel) = {
        let f = lab_form();
        (f.focus == id, f.scroll as usize, f.sel as usize)
    };
    let rows = h.saturating_sub(2) / MENU_ROW_H;
    for r in 0..rows {
        let idx = scroll + r;
        let Some(item) = items.get(idx) else {
            break;
        };
        let ry = ay + 1 + r * MENU_ROW_H;
        let bg = if idx == sel {
            if focused {
                BAR_ON
            } else {
                vector::mix(NET_BOX_BG, BAR_ON, 128)
            }
        } else {
            NET_BOX_BG
        };
        unsafe {
            fb.fill_rect(ax + 1, ry, w.saturating_sub(2), MENU_ROW_H, bg);
        }
        let fg = if idx == sel { TEXT } else { TEXT_DIM };
        tui::draw_text(
            fb,
            ax + 4,
            ry + (MENU_ROW_H - console::GLYPH_H) / 2,
            item,
            fg,
            bg,
            (ax + w).saturating_sub(2),
        );
    }
}

/// Paints the Widget Lab scrollbar: a track with a thumb sized by the visible
/// fraction and positioned by the bound list's scroll offset.
fn draw_lab_scrollbar(fb: &Framebuffer, ax: usize, ay: usize, h: usize, list_id: u8) {
    unsafe {
        fb.fill_rect(ax, ay, SCROLLBAR_W, h, NET_BOX_BG);
    }
    draw_edge_box(fb, ax, ay, SCROLLBAR_W, h, TEXT_DIM);
    let _ = list_id;
    let count = LAB_ITEMS.len();
    let max_scroll = count.saturating_sub(LAB_ROWS);
    let track = h.saturating_sub(2);
    if track == 0 {
        return;
    }
    let thumb = (track * LAB_ROWS / count.max(1)).clamp(4, track);
    let pos = (track - thumb)
        .saturating_mul(lab_form().scroll as usize)
        .checked_div(max_scroll)
        .unwrap_or(0);
    unsafe {
        fb.fill_rect(ax + 2, ay + 1 + pos, SCROLLBAR_W - 4, thumb, BAR_ON);
    }
}

/// Paints the Network window body (v2.38.34): five status lines on the 16 px
/// grid from `y+24`, the mode row with its feedback word, the seven field
/// captions and values (the widget layer draws their borders), the Wi-Fi status
/// line and up to three scan rows. Everything clips at `max_px`, i.e. the
/// window's own right edge.
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

    let mut card_b = [0u8; 32];
    let cn = crate::wifi::card_desc(&mut card_b);
    let card = core::str::from_utf8(&card_b[..cn]).unwrap_or("?");
    at(
        240,
        &format!("wifi card {}", card),
        if crate::wifi::has_card() {
            TEXT
        } else {
            TEXT_DIM
        },
    );

    let (results, count) = crate::wifi::scan_results();
    if count == 0 {
        let hint = if crate::wifi::scanning() {
            "scanning..."
        } else if crate::wifi::scan_done() {
            "no networks found"
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
    bring_to_front(i);
    dirty_all();
}

/// The focused Task Manager window takes Enter to switch to the selected row.
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
            .is_some_and(|w| w.kind == WinKind::Tasks)
    };
    if !is_tasks {
        return false;
    }
    task_switch();
    true
}

/// Toggles checkbox `i` in `host`'s form store and returns the new state.
fn form_check_toggle(host: WinKind, i: usize) -> bool {
    let f = form(host);
    if i >= f.checks.len() {
        return false;
    }
    f.checks[i] = !f.checks[i];
    f.checks[i]
}

/// Checked state of checkbox `id` in `host`'s form store, `false` out of range.
fn form_checked(host: WinKind, id: u8) -> bool {
    form(host).checks.get(id as usize).copied().unwrap_or(false)
}

/// Selected value of radio group `g` in `host`'s form store, or [`LAB_NO`] if
/// none/out of range.
fn form_radio(host: WinKind, g: usize) -> u8 {
    form(host).radios.get(g).copied().unwrap_or(LAB_NO)
}

/// Selects radio `value` in group `g` of `host`'s form store.
fn form_radio_select(host: WinKind, g: usize, value: u8) {
    let f = form(host);
    if g < f.radios.len() {
        f.radios[g] = value;
    }
}

/// Value of slider `id` in `host`'s form store, `0` out of range.
fn form_slider(host: WinKind, id: u8) -> u8 {
    form(host).sliders.get(id as usize).copied().unwrap_or(0)
}

/// Sets slider `id` in `host`'s form store to `value`.
fn form_slider_set(host: WinKind, id: u8, value: u8) {
    let f = form(host);
    if let Some(slot) = f.sliders.get_mut(id as usize) {
        *slot = value;
    }
}

/// Capacity of the GUI clipboard in bytes (v2.68.0).
const CLIP_CAP: usize = 128;

/// GUI clipboard (v2.68.0): UTF-8 bytes shared by every widget host, filled by
/// `Ctrl+C`/`Ctrl+X` and drained by `Ctrl+V`. Kernel-internal only — it is not
/// the firmware/desktop clipboard of any host OS.
static mut CLIP: [u8; CLIP_CAP] = [0; CLIP_CAP];
/// Used byte length of [`CLIP`].
static mut CLIP_LEN: usize = 0;

/// Used byte length of the GUI clipboard.
fn clip_len() -> usize {
    unsafe { *core::ptr::addr_of!(CLIP_LEN) }
}

/// Copies `bytes` into the GUI clipboard, truncating to [`CLIP_CAP`], and
/// returns the stored length.
fn clip_store(bytes: &[u8]) -> usize {
    let n = bytes.len().min(CLIP_CAP);
    unsafe {
        let clip = &mut *core::ptr::addr_of_mut!(CLIP);
        clip[..n].copy_from_slice(&bytes[..n]);
        *core::ptr::addr_of_mut!(CLIP_LEN) = n;
    }
    n
}

/// Bytes currently in the GUI clipboard.
fn clip_bytes() -> &'static [u8] {
    let n = clip_len();
    unsafe { &(&*core::ptr::addr_of!(CLIP))[..n] }
}

/// Copies the focused text field of `host` into the clipboard (v2.68.0).
/// Returns the copied byte count, or `None` when no text field has focus (the
/// status line then says `no field`).
fn form_clip_copy(host: WinKind) -> Option<usize> {
    let (id, n) = {
        let f = form(host);
        let id = f.focus as usize;
        if id >= LAB_FIELD_N {
            (id, 0)
        } else {
            (id, clip_store(&f.fields[id][..f.lens[id] as usize]))
        }
    };
    if id >= LAB_FIELD_N {
        form_msg(host, "no field");
        return None;
    }
    form_msg(host, "copied");
    crate::kprintln!("[gui] clipboard copy {} bytes", n);
    Some(n)
}

/// Copies the focused text field into the clipboard and clears it (`Ctrl+X`).
fn form_clip_cut(host: WinKind) -> Option<usize> {
    let n = form_clip_copy(host)?;
    let f = form(host);
    let id = f.focus as usize;
    f.lens[id] = 0;
    f.fields[id] = [0; LAB_FIELD_CAP];
    form_msg(host, "cut");
    crate::kprintln!("[gui] clipboard cut {} bytes", n);
    Some(n)
}

/// Appends the clipboard to the focused text field (`Ctrl+V`, v2.68.0),
/// honouring [`LAB_FIELD_CAP`] and never splitting a UTF-8 scalar. Returns the
/// pasted byte count (`0` when the clipboard is empty or the field is full).
fn form_clip_paste(host: WinKind) -> Option<usize> {
    let id = {
        let f = form(host);
        let id = f.focus as usize;
        if id >= LAB_FIELD_N {
            form_msg(host, "no field");
            return None;
        }
        id
    };
    let clip = clip_bytes();
    if clip.is_empty() {
        form_msg(host, "empty");
        return Some(0);
    }
    let n = {
        let f = form(host);
        let len = f.lens[id] as usize;
        let mut n = clip.len().min(LAB_FIELD_CAP.saturating_sub(len));
        while n > 0 && n < clip.len() && (clip[n] & 0xC0) == 0x80 {
            n -= 1;
        }
        f.fields[id][len..len + n].copy_from_slice(&clip[..n]);
        f.lens[id] = (len + n) as u8;
        n
    };
    form_msg(host, if n == 0 { "full" } else { "pasted" });
    crate::kprintln!("[gui] clipboard paste {} bytes", n);
    Some(n)
}

/// Advances the focus across text fields of `host`'s form.
fn form_focus_cycle(host: WinKind) {
    let f = form(host);
    if f.focus < LAB_NO {
        f.focus = f.focus.saturating_add(1) % (LAB_FIELD_N as u8);
    } else {
        f.focus = 0;
    }
}

/// Deletes the last UTF-8 scalar of the focused text field, if any (v2.68.0:
/// whole scalar, not one byte, so a multi-byte character never half-disappears).
fn form_backspace(host: WinKind) {
    let f = form(host);
    let id = f.focus as usize;
    if id < LAB_FIELD_N && f.lens[id] > 0 {
        let end = f.lens[id] as usize;
        let mut start = end - 1;
        while start > 0 && (f.fields[id][start] & 0xC0) == 0x80 {
            start -= 1;
        }
        for b in &mut f.fields[id][start..end] {
            *b = 0;
        }
        f.lens[id] = start as u8;
    }
}

/// Types `c` into the focused text field as UTF-8 bytes (v2.68.0). A scalar is
/// appended only when it fits whole in [`LAB_FIELD_CAP`]; otherwise the field is
/// left untouched rather than truncated mid-character.
fn form_type_char(host: WinKind, c: char) {
    let f = form(host);
    let id = f.focus as usize;
    if id < LAB_FIELD_N {
        let mut buf = [0u8; 4];
        let enc = c.encode_utf8(&mut buf).as_bytes();
        let len = f.lens[id] as usize;
        if len + enc.len() <= LAB_FIELD_CAP {
            f.fields[id][len..len + enc.len()].copy_from_slice(enc);
            f.lens[id] += enc.len() as u8;
        }
        return;
    }
    if host == WinKind::Widgets && id == 2 {
        let count = LAB_ITEMS.len();
        let sel = match c {
            'n' => (f.sel as usize + 1).min(count.saturating_sub(1)),
            'p' => (f.sel as usize).saturating_sub(1),
            _ => return,
        };
        f.sel = sel as u8;
        if sel < f.scroll as usize {
            f.scroll = sel as u8;
        } else if sel >= f.scroll as usize + LAB_ROWS {
            f.scroll = sel as u8 + 1 - LAB_ROWS as u8;
        }
    }
}

/// Activates the focused widget of `host`'s form.
fn form_enter(host: WinKind) {
    if host != WinKind::Widgets {
        return;
    }
    let row = {
        let f = form(host);
        if (f.focus as usize) == 2 {
            Some(f.sel as usize)
        } else if (f.focus as usize) < LAB_FIELD_N {
            form_msg(host, "ok");
            None
        } else {
            None
        }
    };
    if let Some(row) = row {
        if row < LAB_ITEMS.len() {
            form_msg(host, LAB_ITEMS[row]);
        }
    }
}

fn form_menu(host: WinKind) -> u8 {
    form(host).menu
}

fn form_menu_set(host: WinKind, m: u8) {
    form(host).menu = m;
}

fn form_focus(host: WinKind) -> u8 {
    form(host).focus
}
