//! i8042 (PS/2) support for the native laptop keyboard and touchpad/mouse,
//! driven by polling the controller status port.
//!
//! The classic IRQ1/IRQ12 lines are often inert on UEFI laptops (the 8259 PIC
//! is left unprogrammed), so everything is polled from the idle loop: bytes are
//! classified by the controller status flags (bit 5 = AUX/mouse output buffer,
//! bit 0 = keyboard output buffer). Keyboard bytes are treated exactly like the
//! IRQ path's scancodes; AUX bytes are reassembled into standard three-byte
//! PS/2 packets (buttons + 9-bit signed X/Y deltas). The aux device is enabled
//! via the controller's "-A8" port-raise command and identified through 0xF2.
//! All device waits are bounded, so hosts without a controller or pointer
//! (some SMBus touchpads) simply report `ok = false`.

use crate::kprintln;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

/// Keyboard/aux data port.
const DATA: u16 = 0x60;
/// Controller status/command port.
const CMD: u16 = 0x64;
/// Input buffer full flag in the status port.
const IBF: u8 = 0x02;
/// Keyboard output buffer full flag.
const OBF: u8 = 0x01;
/// Mouse (AUX) output buffer full flag.
const MOBF: u8 = 0x20;
/// Command: write the controller command byte.
const CMD_READ_CCB: u8 = 0x20;
/// Command: enable the AUX (mouse) port.
const CMD_ENABLE_AUX: u8 = 0xA8;
/// Byte prefix routing the next DATA byte to the AUX device.
const AUX_PREFIX: u8 = 0xD4;
/// Device command: enable data reporting.
const SET_REPORTING: u8 = 0xF4;
/// Device command: get device ID.
const GET_ID: u8 = 0xF2;

/// True once the controller accepted the keyboard/mouse bring-up.
static PS2_OK: AtomicBool = AtomicBool::new(false);
/// Keyboard: monotonically increasing make-code sequence.
static KEY_SEQ: AtomicU32 = AtomicU32::new(0);
/// Keyboard: last make-code seen (scancode set 1).
static KEY_SCANCODE: AtomicU32 = AtomicU32::new(0);
/// Mouse: monotonically increasing packet sequence.
static MOUSE_SEQ: AtomicU32 = AtomicU32::new(0);
/// Mouse: horizontal delta accumulated since the last read (the reader swaps
/// it to zero, so packets that arrive faster than the main-loop poll cannot
/// overwrite each other — v2.38.30 burst-loss fix).
static MOUSE_DX: AtomicI32 = AtomicI32::new(0);
/// Mouse: vertical delta accumulated since the last read (screen Y grows
/// downwards), drained together with [`MOUSE_DX`].
static MOUSE_DY: AtomicI32 = AtomicI32::new(0);
/// Mouse: button bits of the newest packet (bit0 left, bit1 right, bit2 mid).
static MOUSE_BUTTONS: AtomicU32 = AtomicU32::new(0);
/// Identified AUX device ID (0 = standard mouse, 3 = IntelliMouse, 4 = 5-button).
static MOUSE_ID: AtomicU32 = AtomicU32::new(0);

static mut MOUSE_PKT: [u8; 3] = [0; 3];
static mut MOUSE_IDX: usize = 0;

/// Keyboard make-code sequence (increments per processed byte).
pub fn key_seq() -> u32 {
    KEY_SEQ.load(Ordering::Relaxed)
}

/// Latest keyboard make-code.
pub fn key_scancode() -> u32 {
    KEY_SCANCODE.load(Ordering::Relaxed)
}

/// Mouse packet sequence (increments per decoded packet).
pub fn mouse_seq() -> u32 {
    MOUSE_SEQ.load(Ordering::Relaxed)
}

/// Largest pointer step one drain may hand back (v2.38.35 pointer-jump fix):
/// one full PS/2 packet (9-bit signed range) passes through untouched, while
/// a bigger accumulated backlog is split across polls so a stale burst can
/// never teleport the cursor across the screen (the "jumps to the task bar"
/// bug). The remainder stays in the accumulator and rides out next drain.
const MOUSE_MAX_STEP: i32 = 256;

/// Drains one axis: swaps the accumulator, clamps the step to
/// [`MOUSE_MAX_STEP`] and parks the unapplied remainder back, so the caller
/// never sees a larger jump than one poll's worth of motion.
fn drain_axis(acc: &AtomicI32) -> i32 {
    let taken = acc.swap(0, Ordering::Relaxed);
    let clamped = taken.clamp(-MOUSE_MAX_STEP, MOUSE_MAX_STEP);
    acc.fetch_add(taken - clamped, Ordering::Relaxed);
    clamped
}

/// Horizontal delta accumulated since the last call (drains the accumulator;
/// the step is clamped per [`MOUSE_MAX_STEP`], the remainder stays behind).
pub fn mouse_dx() -> i32 {
    drain_axis(&MOUSE_DX)
}

/// Vertical delta accumulated since the last call (screen-space, already
/// Y-inverted; clamped like [`mouse_dx`]).
pub fn mouse_dy() -> i32 {
    drain_axis(&MOUSE_DY)
}

/// Latest mouse button state.
pub fn mouse_buttons() -> u32 {
    MOUSE_BUTTONS.load(Ordering::Relaxed)
}

/// Identified AUX device ID (0 when unknown).
pub fn mouse_id() -> u32 {
    MOUSE_ID.load(Ordering::Relaxed)
}

fn status() -> u8 {
    unsafe { crate::port::inb(CMD) }
}

fn wait_ibf_clear() -> bool {
    for _ in 0..10_000 {
        if status() & IBF == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn wait_obf() -> Option<u8> {
    for _ in 0..100_000 {
        if status() & OBF != 0 {
            return Some(unsafe { crate::port::inb(DATA) });
        }
        core::hint::spin_loop();
    }
    None
}

fn write_cmd(b: u8) -> bool {
    if !wait_ibf_clear() {
        return false;
    }
    unsafe {
        crate::port::outb(CMD, b);
    }
    true
}

fn write_data(b: u8) -> bool {
    if !wait_ibf_clear() {
        return false;
    }
    unsafe {
        crate::port::outb(DATA, b);
    }
    true
}

/// Sends one command byte to the AUX device (prefix + byte) and returns the
/// device ACK, or `None` when the device does not answer.
fn aux_cmd(b: u8) -> Option<u8> {
    if write_cmd(AUX_PREFIX) && write_data(b) {
        wait_obf()
    } else {
        None
    }
}

/// Reads the AUX device ID: expects ACK then one identifier byte. The ACK is
/// drained separately so a silent device yields 0 without pinning a stray ACK.
fn read_aux_id() -> u32 {
    if aux_cmd(GET_ID).is_none() {
        return 0;
    }
    match (wait_obf(), wait_obf()) {
        (Some(_ack), Some(id)) => u32::from(id),
        _ => 0,
    }
}

/// Brings up the PS/2 port pair. The keyboard keeps its existing reporting;
/// the AUX port is raised and, when a device answers, moved into report mode
/// and identified. Each wait is bounded so boards without a PS/2 pointer
/// (SMBus touchpads) degrade to "keyboard only".
pub fn init() {
    if !write_cmd(CMD_READ_CCB) {
        kprintln!("[serial] [ps2] controller unresponsive");
        return;
    }
    // Bake touchpad/mouse on: port enable, then a report-mode handshake.
    let mouse_ok = write_cmd(CMD_ENABLE_AUX) && aux_cmd(SET_REPORTING).is_some();
    let id = if mouse_ok { read_aux_id() } else { 0 };
    MOUSE_ID.store(id, Ordering::Relaxed);
    if mouse_ok {
        kprintln!(
            "[serial] [ps2] aux/mouse up id=0x{:02x} (0x00=PS/2 0x03=Intelli 0x04=5btn)",
            id
        );
    }
    // Keyboard reporting is usually already on; ask anyway and accept silence.
    write_data(SET_REPORTING);
    PS2_OK.store(true, Ordering::Relaxed);
    kprintln!(
        "[serial] [ps2] i8042 ready (kbd+{}), IRQ-free polled path",
        if mouse_ok { "mouse" } else { "no-mouse" }
    );
}

/// Drains pending i8042 output. Keyboard bytes bump `KEY_SEQ`; AUX bytes feed
/// the three-byte packet decoder. Bounded to 32 reads so a wedged controller
/// cannot stall the idle loop.
pub fn drain() {
    if !PS2_OK.load(Ordering::Relaxed) {
        return;
    }
    for _ in 0..32 {
        let st = status();
        if st & OBF == 0 {
            break;
        }
        let byte = unsafe { crate::port::inb(DATA) };
        if st & MOBF != 0 {
            feed_mouse(byte);
        } else {
            feed_key(byte);
        }
    }
}

fn feed_key(byte: u8) {
    // Extended keys (0xE0/0xE1 prefix + the byte that follows: Win/arrow/Fn
    // scancodes) are dropped entirely — they must never reach the key consumer
    // (TUI/GUI/LAST_SCANCODE), on hardware where the byte-follows rule is the
    // only thing preventing an out-of-range entry.
    static mut EXT: bool = false;
    if unsafe { *core::ptr::addr_of!(EXT) } {
        unsafe {
            *core::ptr::addr_of_mut!(EXT) = false;
        }
        return;
    }
    if byte == 0xE0 || byte == 0xE1 {
        unsafe {
            *core::ptr::addr_of_mut!(EXT) = true;
        }
        return;
    }
    match byte {
        0x2A | 0x36 => {
            crate::interrupts::SHIFT_DOWN.store(true, Ordering::Relaxed);
            return;
        }
        0xAA | 0xB6 => {
            crate::interrupts::SHIFT_DOWN.store(false, Ordering::Relaxed);
            return;
        }
        _ => {}
    }
    if byte & 0x80 == 0 {
        crate::interrupts::SHIFTED.store(
            crate::interrupts::SHIFT_DOWN.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        KEY_SCANCODE.store(u32::from(byte), Ordering::Relaxed);
        KEY_SEQ.fetch_add(1, Ordering::Relaxed);
    }
}

fn feed_mouse(byte: u8) {
    #[allow(static_mut_refs)]
    unsafe {
        let pkt = &mut *core::ptr::addr_of_mut!(MOUSE_PKT);
        let idx = &mut *core::ptr::addr_of_mut!(MOUSE_IDX);
        if *idx == 0 {
            if byte & 0x08 == 0 {
                return;
            }
            pkt[*idx] = byte;
            *idx += 1;
        } else {
            pkt[*idx] = byte;
            if *idx + 1 < 3 {
                *idx += 1;
                return;
            }
            decode_packet(pkt);
            *idx = 0;
        }
    }
}

fn sign9(negative: bool, data: u8) -> i32 {
    if negative {
        i32::from(data) - 256
    } else {
        i32::from(data)
    }
}

fn decode_packet(pkt: &[u8; 3]) {
    let b0 = pkt[0];
    // Byte 0 bits 6/7 are the X/Y overflow flags: the controller lost motion
    // data and the wrapped value decodes into a garbage delta that teleports
    // the cursor (pointer-jump bug, v2.38.35). Drop the whole packet - the
    // sequence counter stays put, so the input band never even sees it.
    if b0 & 0xC0 != 0 {
        return;
    }
    let dx = sign9(b0 & 0x10 != 0, pkt[1]);
    let dy = -sign9(b0 & 0x20 != 0, pkt[2]);
    // Accumulate instead of store: a burst of packets between two main-loop
    // polls must sum up, not overwrite (the controller splits deltas larger
    // than one packet, so an overwrite silently drops the earlier chunk).
    MOUSE_DX.fetch_add(dx, Ordering::Relaxed);
    MOUSE_DY.fetch_add(dy, Ordering::Relaxed);
    MOUSE_BUTTONS.store(u32::from(b0 & 0x07), Ordering::Relaxed);
    MOUSE_SEQ.fetch_add(1, Ordering::Relaxed);
}
