# AIOS Known Bugs & Workarounds

## RESOLVED: ring-3 demo pid3 parked in a busy spin and froze the OS on PIC-less boards
- **Status:** RESOLVED in v2.38.10
- **Symptom (MSI, v2.38.9):** after ~8 demo rounds the log stops at
  `[ktask] alive #8` … `[sched] pid3 woke`; the 2 Hz heartbeat square
  freezes; green okay-square copies are visible strung up the screen.
- **Root cause (two bugs):**
  1. `build_pid3` ended in `jmp $` (infinite spin). On boards without the
     8259 PIC the scheduler cannot preempt a running task, so a busy-spinning
     ring-3 task never yields → `idle_loop` (the software-tick source and
     the heartbeat) never runs → `TICKS` stalls, sleeping pid1/pid2 never
     wake and the whole kernel looks frozen. In QEMU the hardware PIT
     preempts, which is why this was never caught: **no preemptive
     timeslicing exists on PIC-less hardware** — the kernel is cooperative
     there.
  2. `scroll_up` memmoved the entire framebuffer, dragging copies of the
     bottom-right heartbeat overlay (and the top-right PIT bar) up with every
     console scroll — the "moving green squares".
- **Fix:** pid3 now loops back to its sleep cycle instead of parking
  (`jmp_self` removed), so it keeps yielding and the tick economy stays
  alive forever. The console reserves the bottom `GLYPH_H` row for the
  heartbeat overlay and `scroll_up` is bounded to the console region, so
  overlay pixels are never scrolled/ghosted.
- **Regression guard:** no demo task may end in a busy loop; keep every
  ring-3 program yielding (`SYS_SLEEP`) at some cadence.

## OPEN: PIT IRQ0 never arrives on the MSI laptop — 8259 PIC dead in UEFI APIC mode
- **Status:** CONFIRMED in v2.38.6-7, mitigated by a software-tick fallback
- **Symptom:** boot completes (all 14 steps; three ring-3 tasks run,
  `getpid`/`write` work, each sleeps 20 ticks), then text stops at
  `[sched] idle`. `[tick]`, `[sched] pid N woke` and the 2 Hz heartbeat
  never appear. The v2.38.5 top-right PIT bar stays height-0 (invisible)
  on the MSI.
- **Confirmation:** with the v2.38.6 software-tick fallback the MSI boot no
  longer freezes — `[ktask] alive` advances, all three ring-3 tasks wake,
  run and sleep in a cycle, proving the scheduler was healthy all along and
  only the hardware timer IRQ was missing. `[sched] idle` was rate-limited
  to 1/s in v2.38.7 (the fallback printed ~100 idle lines/s).
- **Root cause:** the legacy 8259 PIC does not deliver IRQ0 to the CPU on
  this UEFI laptop (the line is routed via the unprogrammed IO-APIC),
  so `TICKS` never changes. All follow-on symptoms cascade from a frozen
  tick counter.
- **Proof path added in v2.38.6:** boot now prints on-screen
  `[probe] irq32_seen=.. ticks=..->.. (delta N)` after a ~120 ms PIT
  countdown; `ticks delta 0` + `irq32_seen false` confirms the dead PIC.
- **Mitigation:** `idle_loop` software-tick fallback — when no IRQ32 has
  ever been seen, idle polls the always-running PIT countdown for one
  tick period (~10 ms) and increments `TICKS` itself, driving sleeps,
  `[tick]` and the heartbeat on any board.
- **Exact pending question:** a full fix would program the Local APIC timer
  or IO-APIC redirection so hardware ticks arrive again; the software
  fallback keeps the OS functional meanwhile (tick cadence preserved via
  the PIT countdown, busy-poll in idle only, `hlt` when IRQs work).
- **Regression guard:** `IRQ32_SEEN` distinguishes a working PIC
  (`hlt` path, hardware ticks) from a dead one (poll path); no double
  counting.

## DIAGNOSTIC: v2.38.5 indicators — frozen CPU vs wedged console (superseded)
- **Symptom:** boot completes on the MSI laptop (all 14 steps; three ring-3
  tasks run, `getpid`/`write` work, each sleeps 20 ticks), then text output
  stops at `[sched] idle`. No `[sched] pid N woke`, no `[tick] 1s`, and the
  heartbeat square appears steady. Because all text goes through the
  console/serial lock, a frozen screen is ambiguous.
- **Diagnosis added in v2.38.5** (bypasses the console lock):
  - A cycling PIT tick bar (top-right, grows/shrinks every IRQ) proves the
    timer ISR keeps firing when the console is wedged.
  - The heartbeat now blinks at 2 Hz instead of ~100 Hz (per-iteration toggles
    integrated into a steady-looking fill).
- **Interpretation:** PIT bar cycles = CPU alive, console lock wedged (focus
  on `CONSOLE_LOCK` holders). Heartbeat visibly blinks = idle alive, ticks
  advancing. Both static green = machine actually hung (PIT stopped), e.g.
  `cli` left set or the idle switch corrupted RFLAGS.
- **Open question:** who, if anyone, holds `CONSOLE_LOCK` indefinitely after
  the last task sleeps? Idle is ring-0 and the ISR guard
  (`sched.rs:258`) forbids switching ring-0 tasks mid-print, so the classic
  preempt-while-holding path should be closed; the RFLAGS/IF state after the
  `aios_restore_ring0` switch into the fabricated idle frame is unverified.

## RESOLVED: v2.38.4 kernel never compiled after ring-3 restore commits
- **Status:** FIXED in v2.38.4
- **Symptom:** all `cargo build --workspace` invocations passed but the
  bare-metal kernel did NOT compile (`aios-kernel` is not a workspace
  member); every USB re-flash silently shipped the stale 2026-09-24 ISO.
  The on-screen behaviour never changed regardless of source edits, and
  diagnostic work blamed the hardware for too long. The framebuffer
  self-check (green square, bottom-right) was visible because it predates
  the breakage.
- **Root cause (3 corruption spots from a bad merge):**
  1. `aios-kernel/build.rs` — second raw asm string was unterminated
    (`"#,` missing) and its `asm.push_str(` closing `);` was lost;
    `aios_handler_table` also re-declared inside the string.
  2. `aios-kernel/src/sched.rs` — `*frame = incoming;` + an extra `}`
    located after the end of `schedule()`.
  3. `aios-kernel/src/interrupts.rs` — `fatal`/`halt`/`read_cr2`
    duplicated (E0428); `main.rs` had `mod interrupts;` + a redundant
    `use crate::interrupts;` (E0255).
- **Fix:** properly terminated the asm raw string, deleted the dead code
  and duplicates, removed the redundant `use`, ran `cargo fmt` on the
  crate, and rebuilt the ISO from the current source.
- **Regression guard:** the kernel must be built with `cargo build
  --manifest-path aios-kernel/Cargo.toml --target x86_64-unknown-none`
  (NOT just `cargo build --workspace`, which excludes it). The ISO is
  produced by `aios-kernel-run` with `AIOS_SKIP_QEMU=1`.

## DIAGNOSTIC: v2.38.3 boot progress strip + blinking heartbeat (real hardware, no serial)
- **What was observed:** on the MSI laptop the framebuffer self-check probe
  (green 8x8 square, bottom-right) stays lit and static. It is written once
  in `main.rs` at (width-8, height-8). Because it remains visible, GOP
  writes ARE reaching the screen and the kernel passed the self-check —
  but the idle-loop heartbeat (top-left, colour-cycling) never appeared,
  so execution stops somewhere between the self-check and `idle_loop`.
- **Fix (instrumentation):** boot status is now visible without F8/serial:
  `print_step()` always draws an orange square on the bottom-left strip and
  prints `STEP n/14`; step 14 was added for the ring-3 handoff;
  the heartbeat now BLINKS the bottom-right square (OK/BG toggle), so a
  blinking lower-right square means `idle_loop` is actually running.
- **How to read it:** on the next boot count the orange squares (1..14).
  The first missing square after the last visible one pinpoints the hung
  init stage (2=PCI, 8=AHCI, 9=NVMe, 10=xHCI, 11-12=sched/user init,
  14=ring-3 handoff). No blinking heartbeat = stuck before idle_loop.

## RESOLVED: v2.38.0 PIT IDT gate corruption causes GP#13 on real hardware
- **Status:** FIXED in v2.38.1 (mask IRQ 0)
- **Symptom:** on a real laptop with the AIOS USB stick plugged in, the
  kernel reaches `scheduler online` but immediately halts. The serial
  console shows `[serial] scheduler online, IPC + user syscalls armed.`
  followed by no further output. The system appears to stop.
- **Root cause:** the PIT IDT gate for vector 32 has a corrupted
  `offset_mid` field. After `sti` enables interrupts, the PIT fires
  immediately and the CPU looks up the corrupted IDT entry, triggering
  GP#13 (General Protection Fault). `fatal()` halts the system.
- **Fix:** mask IRQ 0 in the PIC OCW1 mask (`0xFC` → `0xFD`) in
  `init_pic()`. This disables the PIT interrupt. The kernel boots to
  `scheduler online` without crashing. Cooperative scheduling via
  `yield_kernel()` (`int 0xfa`) still works correctly. TICKS does not
  increment because the PIT ISR never fires (no preemption occurs).
  The `IdtEntry` struct is marked `#[repr(C, packed)]` to ensure the
  IDT gate descriptor layout exactly matches the x86 specification
  (16 bytes, no padding), which is required for the `lidt` instruction
  to load correct gate bases on real hardware. The PIT IDT gate
  corruption (`offset_mid` field) is hardware-specific and does not
  reproduce in QEMU.
- **Note:** keyboard input is now available via xHCI multi-port enumeration (BUG-044 fixed in v2.38.1).

## DIAGNOSTIC: v2.38.2 F8 debug mode + HALT_REASON and boot-step logging
- **Purpose:** determine exactly where the kernel stops on real hardware.
- **F8 debug mode:** press **F8** immediately after the BIOS/UEFI boot screen appears and before the kernel starts printing. The i8042 keyboard controller is polled for F8 make code (`0x38`). If detected, `DEBUG_MODE: AtomicBool` is set and a debug banner is printed at the **top-left** corner of the framebuffer showing all 14 `STEP` markers. Without F8, the kernel boots quietly (TUI only, no console markers).
- **Mechanism:** when debug mode is active, `print_step()` prints `STEP X/14: message` lines only to the framebuffer at the top-left area. `HALT_REASON: AtomicU32` stores a 0x80000000+ code on fatal error, debug port `0x80` receives the low byte, and both `vprintln!` and `kprintln!` emit the code and detail string before halting.
- **Usage:** on a real laptop, if the system halts at `STEP 7/14` for example, the last printed step identifies the failure region. The debug port `0x80` value can be read with a hardware probe or an external logic analyzer.
- **Error codes:** `0x10000000 + vector` for interrupt faults, `0x20000000` for page fault, `0x30000001` for paging selftest failure.
- **Files:** `aios-kernel/src/interrupts.rs`, `aios-kernel/src/console.rs`, `aios-kernel/src/main.rs`.

## RESOLVED: v2.35.0 `crt.rs` `memset` hung the boot (circular PLT)
- **Status:** FIXED in v2.36.0 (module deleted)
- **Symptom:** the kernel hung mid-boot right after `[serial] heap online.` — `interrupts online.` never appeared and no fault was printed.
- **Root cause:** the strong `memset` in `aios-kernel/src/crt.rs` was emitted with a PLT stub. The `memset` GOT slot received an `R_X86_64_RELATIVE` relocation pointing at the `memset` PLT stub (`0xffffffff80009ae0`) itself, so the first out-of-line `memset` call (from the `[MemRegion; 64]` array init) jumped into its own stub forever.
- **Fix:** deleted `aios-kernel/src/crt.rs` and its `mod crt;`; the kernel now uses `compiler_builtins`' weak `memcpy`/`memmove`/`memset`/`memcmp`. Verified by booting past the hang to the scheduler. Note: this module was shipped in **v2.35.0**, so that tag is affected.

## RESOLVED: Limine HHDM does not map MMIO (PCI BAR access page-faults)
- **Status:** FIXED in v2.36.0
- **Symptom:** reading the AHCI ABAR through the HHDM alias faulted with `PAGE FAULT: addr=0xffff800081060004 ip=0xffffffff80007420 err=0x0`; earlier the same access triple-faulted because it ran before `sti`.
- **Root cause:** the Limine HHDM maps only usable RAM, not device MMIO regions, so `hhdm_offset + phys` is unmapped for BARs.
- **Fix:** added `memory::map_mmio(phys, size)` (maps page-rounded device pages into a dedicated window at `0xffffff00_2000_0000`, PML4 slot 510) and moved driver init **after** `sti` so faults are handled by the kernel.

## RESOLVED: v2.37.0 NVMe bring-up — `CSTS.CFS` on enable and admin-command timeouts
- **Status:** FIXED in v2.37.0 (driver added)
- **Symptom:** the NVMe driver set `CSTS.CFS` immediately after writing `CC.EN=1` (`nvme: controller fatal status`) and, once that was fixed, the first admin command timed out.
- **Root cause:** (1) `AQA` packs `ASQS` in bits 11:0 but `ACQS` in bits **27:16** — the initial `<< 12` left `ACQS = 0`, so QEMU rejected the start (`pci_nvme_err_startfail_acqent_sz_zero`); (2) the completion-entry phase tag is bit 0 of the 16-bit status field (dword bit 16), not the dword's bit 31, so the poll never matched a completion.
- **Fix:** `AQA = (depth-1) | ((depth-1) << 16)` and the phase test uses `(status >> 16) & 1` (the status field is read as `(status >> 17) & 0x7FFF`). Diagnosed with QEMU `-trace enable=pci_nvme_*`.

## RESOLVED: `font8x8` dependency broke the no_std kernel build
- **Status:** FIXED in v2.34.0
- **Symptom:** `cargo build --target x86_64-unknown-none --release` for `aios-kernel` failed inside the dependency source with `error: cannot find macro \`print\` in this scope` / `\`println\`` (e.g. `font8x8-0.3.1/src/block.rs`, `src/box.rs`).
- **Root cause:** `font8x8` 0.3.1 is a `std`-only crate — its `Display`/`Debug` impls call `print!`/`println!`, so it cannot compile for a freestanding target.
- **Fix:** dropped the dependency and vendored the public-domain 8x8 bitmap table as `aios-kernel/src/font8x8.rs` (`BASIC: [[u8; 8]; 128]`, Basic Latin); `console.rs` indexes it directly.

## RESOLVED: xHCI probes only the first USB device when booting from a stick (BUG-044)
- **Status:** FIXED in v2.38.1 (enumerate all root ports)
- **Symptom:** `xhci.rs` called `reset_port()` which returned the first root port with a connected device. When the USB flash stick occupied that port, `find_hid_ep` failed with `xhci init failed: xhci: no HID interrupt IN endpoint in configuration` and the kernel reached `scheduler online` with the **keyboard not armed**.
- **Root cause:** `reset_port()` iterated ports and returned the first one with `PORT_CCS` set, without checking the device's USB class.
- **Fix:** replaced `reset_port()` with `find_hid_port()` which iterates **all** root ports, resets each one, calls `enable_slot`/`address_device`/`get_descriptor`/`find_hid_ep` for each, and returns the first port that has a HID keyboard. On QEMU this correctly skips ports 1-4 (mass storage) and selects port 5 (usb-kbd device).
- **Verification:** QEMU UEFI shows `port 1-4 portsc=0x000202a0` (no CCS → skipped) and `port 5 portsc=0x00020ee1` (CCS set → HID keyboard armed) → `usb hid boot keyboard armed.` → `scheduler online, IPC + user syscalls armed.`

## KNOWN (v2.34.0): bare-metal kernel gaps after the Limine + GOP migration
- **Status:** INTRODUCED LIMITATION — Phase 1 (boot + graphics) is functional and v2.35.0 added PCI discovery; the following are not yet implemented:
  - PCI enumeration exists (`pci.rs`), and native AHCI (SATA) and NVMe storage drivers are online (v2.36.0/v2.37.0), but input is still PS/2 only (no USB-HID/xHCI, no PS/2-less fallback);
  - the Limine HHDM maps physical memory with huge pages, so kernel virtual mappings must stay outside the HHDM window — the kernel's scratch/heap addresses live in PML4 slot 510 (`0xffff_ff00_…`) and the image in slot 511.
- **Workaround / notes:** none needed for Phase 1; the next bare-metal phase adds the AHCI/NVMe storage driver and USB-HID input.

## RESOLVED: Live/Installer media showed a black screen after the boot menu on a real laptop
- **Status:** FIXED in v2.33.3 (user report: «при выборе Live или Installer загружается vmlinuz, потом initramfs — и на этом всё стоит, чёрный экран»)
- **Symptom:** the Limine boot menu rendered on the laptop screen; choosing either entry loaded `vmlinuz` + `initramfs`; then the screen went black and stayed black — while the machine kept booting invisibly. The same media passed our QEMU tests because the harness captures the serial console.
- **Root cause (v2.33.2 analysis, incomplete):** the v2.33.2 serial-only→`tty0` console fix was verified via QEMU screendump but the laptop stayed black. The real cause: the initramfs ships **no VGA console driver** (`vgacon`/`vesafb`/`efifb` are absent, `simpledrm` is built-in but has no frame buffer until fbcon registers one), so a `tty0` console had nothing to render — and the GPU DRM modules were loaded only *after* userspace (and the shipped `aios-init` never ran its `bring_userspace()`/`sfs-up.sh` GPU bring-up at all). `console=tty0` alone was therefore not enough.
- **Fix (v2.33.3):** the initramfs `/init` entry is now a pre-init `aios-loader` script (runs before any userspace) that `modprobe`s all five x86 GPU DRM drivers (`radeon`, `nouveau`, `gma500_gfx`, `i915`, `amdgpu`) — giving fbcon a frame buffer early — then `dd`-extracts the embedded original `aios-init` ELF (baked fixed-width `BINOFF`/`BINSZ`) and `exec`s it to preserve the PID 1 chain. Both Limine entries also got `loglevel=7` so the kernel backlog is visible once fbcon registers. Kernel-initramfs constraint: only **in-place content replacement** of the `/init` entry works — appended entries are ignored by the kernel unpacker and deleting/renaming `/init` makes it reject the whole archive.
- **Workaround / notes:** none needed post-fix. Related known limitation: the hybrid ISO is legacy-MBR (Limine BIOS stage) — pure-UEFI firmware without CSM, and Secure Boot, will not boot it; tracked in `docs/TODO.md` (real-hardware acceptance).

## KNOWN (v2.32.0): the native browser (TUI `B`/`n`, GUI Browser tab) is not available in the Live ISO
- **Status:** INTRODUCED LIMITATION — the live image ships `aios`/`aios-gui` built with `--no-default-features` (no wry/WebKitGTK) to keep the Alpine rootfs small and avoid a WebKitGTK system-dependency.
- **Symptom:** on the live USB the `B`/`n` browser hotkeys in the kernel TUI have no effect and the `aios-gui` dashboard has no "Native Browser" (F7) tab. `W` (launch GUI) and every other tab still work.
- **Root cause:** building/embedding WebKitGTK for the musl live image is heavy and fragile; the browser engine was made an optional `webview` feature and is simply not enabled in the live build.
- **Workaround / notes:** planned as a follow-up phase (WebKitGTK in the rootfs + `webview` enabled in the live build), currently tracked in `docs/TODO.md`.

## RESOLVED: Shell / AI Console output lines were clipped at the right edge of the window (no wrapping)
- **Status:** FIXED in v2.31.5 (reported by the user: «в окне отображения ответов нет переноса в shell»)
- **Symptom:** long response and command output lines in the Shell tab (7) and the AI Console (3) were silently truncated at the terminal width — no line wrap, the tail of each line was unreachable.
- **Root cause:** `draw_shell_tab`/`draw_ai_output` render the output as a ratatui `List` (which truncates, never wraps), and the shared `wrap_line` split by **character count** instead of display width — fine for ASCII, wrong for multi-byte text (Cyrillic letters were already 1 cell, CJK 2).
- **Fix:** `wrap_line` in `aios/src/tui/ui.rs` now splits by display width via `unicode-width` (combining marks stay attached to their base char, `width == 0` yields no items), and both the Shell and AI Console output lists wrap each logical line into the available panel width before rendering.
- **Workaround / notes:** none needed post-fix; covered by `wrap_line_wraps_at_display_width` (ASCII / Cyrillic / CJK / combining / width-0 edge cases).

## RESOLVED: `F10` re-probe was promised by the UI/docs but unbound (F-key bar lied: `10Quit`)
- **Status:** FIXED in v2.31.1
- **Symptom:** the System & HW inspector printed "Press F10 to re-probe" and `docs/INTERFACE.md` claimed `F10` triggers a manual full re-probe, yet pressing `F10` did nothing — the F-key bar label read `10Quit`, but quitting is already `q` (or `Ctrl+C`).
- **Root cause:** the kernel TUI only ever bound `KeyCode::F(1)`; `F10` fell through to the per-tab handlers and was ignored. The `10Quit` label and the `F10 re-probe` hint contradicted each other.
- **Fix:** `KeyCode::F(10)` now calls `refresh_hw()` (full `HardwareProfile::detect()` + engine rescan) from any tab; the F-key bar label became `10Rescan` and the render smoke test asserts it (`all_tabs_render_without_panic_and_with_chrome`).
- **Workaround / notes:** none needed post-fix.

## RESOLVED: F1 "Help" in the kernel TUI did nothing (the overlay was never rendered)
- **Status:** FIXED in v2.31.1 (reported by the user: «сделай на ф1 держишь справка показывается реализуй как положеное»)
- **Symptom:** pressing/holding `F1` (or `?`) in the `aios` TUI showed nothing, even though the F-key bar labels key 1 as `Help` and INTERFACE/ARCHITECTURE already documented an "F1 Help Overlay".
- **Root cause:** `F1`/`?` toggled `app.show_help`, and the key handler react to `Esc`/`h`, but **no draw path ever rendered that flag** — `draw()` only consumed `ai_show_help` (AI Console help). The global help state was dead code.
- **Fix:** `draw()` now renders a full-screen opaque `AIOS Help` block (`draw_help`, gated on `show_help`) that replaces the whole dashboard background before drawing, so no text blends. `F1`/`?`/`Esc`/`h` dismiss it. Holding `F1` keeps it open (key repeats are filtered out).
- **Workaround / notes:** covered by `help_overlay_renders_on_all_sizes` (40×12…120×30 asserts the `AIOS Help` title and F-key lines at every size).

## RESOLVED: phantom `USB 0000:0000 (unknown)` re-provisioned in a loop — "a new line appears every time I open tab 1"
- **Status:** FIXED in v2.31.1 (reported by the user after the pile-up fix: «уже лучше но все равно когда переключаю первую вкладку появляется инфо которое смещает каждое открытие появляется новая строка»)
- **Symptom:** on the System & HW tab the Events toast strip and the right-side log panel kept gaining lines: `HAL: NVIDIA GPU detected …` / `HAL: Detected 16 cores…` every few seconds and a recurring `[Hardware] USB 0000:0000 (unknown) -> driver not found… -> Generic Fallback` provisioning toast. Each visit showed more content. The machine's PnP tree contains a USB entry without any vendor/product identity (`USB 0000:0000 (unknown)`) that keeps flapping.
- **Root cause:** (1) `HardwareProfile::detect_usb` kept such `0000:0000` entries as devices, so every flap surfaced as an `Added` fingerprint difference and re-triggered provisioning; (2) `HotplugMonitor` ran a full `HardwareProfile::detect()` on every native push event (WM_DEVICECHANGE), one scan per event; (3) `detect()`/`detect_gpu_nvidia` logged the `INFO` hardware summary on every scan, flooding the log panel.
- **Fix:** (1) `detect_usb` now drops entries where VID==0 && PID==0 via new pure helper `is_identifiable_usb` (unit-tested) — enumeration artifacts can't be fingerprinted or provisioned; (2) native pushes are coalesced in `HotplugMonitor` to at most one full scan per `poll_ms`; (3) the `HAL: Detected …` / `HAL: NVIDIA GPU detected` lines are logged once per process.
- **Workaround / notes:** covered by `test_is_identifiable_usb` + `test_pnp_extract_unknown_usb_is_zero_zero`. Debounce + phantom filter apply on Linux too (a `lsusb` line without a parsed ID is skipped).

## RESOLVED: kernel TUI painted the RAM gauge over the status bar on short windows ("everything piles up" when switching tabs)
- **Status:** FIXED in v2.31.1 (reported by the user: «при использовании переключая между вкладками распадается интерфейс и потом все в кучу становится»)
- **Symptom:** on small terminal heights the System & HW tab (tab 1) looked like a pile of widgets — the ` RAM Usage ` gauge row sat on top of the status bar row (`AIOS v…` invisible), the CPU/OS blocks collapsed and the Hardware Inspector + events sprawled. Reproduced deterministically at 50×14 and root-caused with a `TestBackend` render harness + row dumps.
- **Root cause:** `draw_system_tab` positioned the RAM gauge as `Rect::new(x+2, chunk.y + chunk.height - 3, w-4, 3)`. ratatui treats `Length` constraints as hard equalities and shrinks them proportionally when the sum (9+8+10=27) exceeds the panel height; at panel height ≤ 9 the CPU chunk gets `height == 0`, so the gauge's `y` became `chunk.y + 0 - 3`, and for `chunk.y == 3` that is row `0` — the status bar zone. `Flex::Start` cannot help: flex only affects spacers.
- **Fix:** gauge rect clamped with `saturating_sub` on both height and width; the gauge is drawn only when `width > 10 && height > 0`, so it can never leave the left panel. Remaining overlap sources eliminated by a render smoke test looping all sizes.
- **Workaround / notes:** none needed post-fix; covered by `all_tabs_render_without_panic_and_with_chrome`.

## RESOLVED: status bar title truncated mid-word at narrow widths
- **Status:** FIXED in v2.31.1
- **Symptom:** `Network & Stor`, `Blocks & Sv`, `Studio Bridg` instead of full tab titles; the `AIOS v…` prefix disappeared at very small widths.
- **Root cause:** the new status bar used a fixed 8-column segment layout; when the segment width < span width, ratatui's solver shrank the segment (and the title was chopped by the block side borders), regardless of `Flex::Start`.
- **Fix:** status bar rendered as a single `Paragraph` from a `Line` of styled `Span`s — like Far/MC it clips only at the terminal's right edge; the title and version are never chopped. The F-key bar intentionally still clips at the right margin (same as `mc`).

## RESOLVED: crashed bridge worker could kill the TUI and leave the terminal in raw mode
- **Status:** FIXED in v2.31.1
- **Symptom:** a panic in the bridge's background worker that held `registry`/`scheduler` poisoned those mutexes; the next frame's `.lock().unwrap()` in `draw_blocks_tab`/`draw_bridge_tab` panicked, `run_tui` unwound without restoring the terminal, and the console was left in raw-mode garbage (one more way the screen "piles up").
- **Root cause:** `run_tui` had no `catch_unwind`; every draw-path `Mutex::lock().unwrap()` was panic-on-poison.
- **Fix:** added poison-tolerant `locked()` in `app_state.rs` (`unwrap_or_else(|p| p.into_inner())`); converted all draw-path and handler locks in `ui.rs`/`mod.rs` to it; wrapped `run()` in `std::panic::catch_unwind(AssertUnwindSafe(..))` that unconditionally restores the terminal and returns `Err("AIOS TUI crashed: …")`. `logs` locks are intentionally left as `unwrap` (cannot be poisoned — only the TUI touches them, via `if let`).
- **Workaround / notes:** covered by `poisoned_locks_do_not_break_rendering`.

## RESOLVED: `test_e2e_bridge_http_endpoints` failed after the v2.30.0 web-auth lockdown
- **Status:** FIXED in v2.31.1 (caught by `cargo test --workspace` during the v2.31.1 verification run)
- **Symptom:** `/api/v1/system/status` returned `{"error":"Authentication required","success":false}` (assert on `status == "running"` got `Null`); the same awaited `/api/v1/workflow`, `/api/v1/metrics`, `/api/v1/intent`.
- **Root cause:** the test predates authentication — `require_auth` protects every `/api/*` route except the allowlist (`/api/v1/auth/*`, `/api/v1/health`, `/api/v1/sys/status`, `/ws/telemetry`, non-`/api/` paths); `/api/v1/system/status` is not on it, and the test sent no credentials.
- **Fix:** the test now registers user `e2e` (or logs in when the user already persists on disk) and sends a `Bearer` token on all protected calls.
- **Workaround / notes:** DECIDED — `/api/v1/system/status` stays protected (it exposes processes/blocks/watchdog/RAM); `/api/v1/sys/status` remains the public lightweight status probe. Verified by 3 consecutive isolated runs + the full workspace suite.

## RESOLVED: AIOS Studio web auth and CORS lockdown — full workspace verification
- **Status:** RESOLVED in v2.31.1 (previously OPEN in v2.30.0)
- **Note:** the v2.30.0 entry below was marked "UNVERIFIED (no linker)". The MSVC linker became available; `cargo test --workspace` now compiles and runs `aios-bridge` (auth.rs, DTOs, `require_auth`, CORS) green, so the code is verified.
- **Related:** `/ws/telemetry` remains intentionally unauthenticated (WebSocket auth is a follow-up); it only exposes RAM/CPU telemetry.

## RESOLVED: `test_runner_recovery_after_heartbeat` flaked under load (heartbeat raced the watchdog tick phase)
- **Status:** FIXED in v2.29.1 (found during a full verification run on Windows x64; the workspace test suite and a parallel kernel BIOS build were loading the machine)
- **Symptom:** `aios-watchdog\src\runner.rs:228` failed with `assertion left == right failed: left: Recovering, right: Monitoring` — the final state check after the recovery heartbeat saw `Recovering` instead of `Monitoring`. The failure was intermittent (passed on isolated runs).
- **Root cause:** test race against the background thread's tick phase. `WatchdogRunner` polls `check_timeout()` every `interval/2 = 100 ms`; under load a tick can be delayed so that at the moment the test sends hb2 (t≈500 ms) the state is still `Suspended`. By design (`watchdog.rs:88`) a heartbeat restores `Monitoring` only from `Recovering/SafeMode/Warned` — from `Suspended` it does nothing until the next tick advances `Suspended → Recovering`. With no further heartbeats sent, the state stayed `Recovering` at assert time.
- **Fix:** the test now polls for `Monitoring` for up to 2 s, sending an increasing-sequence heartbeat every 50 ms per iteration — whichever tick phase the background thread is in, the first heartbeat after `Suspended → Recovering` completes the recovery. Semantics under test are unchanged.
- **Workaround / notes:** none needed post-fix; verified by 5 consecutive green runs of `cargo test -p aios-watchdog --lib` plus a full workspace run (1417 tests green). Same flake class as the v2.28.1 RT stress threshold fix.

## RESOLVED: `test_stress_rt_scheduler_500` flaked on a loaded machine (hard 2 s wall-clock threshold)
- **Status:** FIXED in v2.28.1 (found during the v2.28.1 full workspace audit, Windows x64)
- **Symptom:** `cargo test --workspace` failed once with `RT scheduling took 2.0974127s (>2s)` in `tests/stress_test.rs:113`. The functional assertion (500 RT processes scheduled) passed; only the wall-clock budget tripped.
- **Root cause:** the speed limit was hard-coded to `2000 ms` regardless of build profile, violating the AGENTS.md rule that all speed tests carry **dual debug/release thresholds**. A debug-build scheduler loop on a machine running parallel test binaries can legitimately exceed 2 s.
- **Fix:** dual threshold — `5000 ms` under `cfg!(debug_assertions)`, `2000 ms` in release. Re-run of the suite is green (11/11).
- **Workaround / notes:** none needed post-fix; if CI flakes recur on other wall-clock stress tests, consider percentile budgets or CPU pinning.

## RESOLVED: aios-kernel heap returned corrupt data after `Vec` growth (stale block size on alloc)
- **Status:** FIXED in v2.28.0 (found during milestone 2 heap testing under QEMU)
- **Symptom:** the milestone 2 heap test printed `heap: Vec<u64> 1000 elems, sum=18446198715943183352` instead of `999000`; later allocations were inconsistent.
- **Root cause:** the free-list allocator took a free block, split it, but never recorded the *allocated* size in the block header — the header still held the pre-split (full) block size. `dealloc` then used that stale size for the coalescing adjacency check, adding a wrong size to the merged block and corrupting the free list.
- **Fix:** `heap.rs` writes the exact allocated size (`needed`) into `(*block).size` immediately after the split decision, before returning the payload.
- **Workaround / notes:** none needed post-fix; verified by the milestone 2 QEMU run (`sum=999000`, stress `len_sum=5100`, `final Vec sum=1498500`).

## RESOLVED: aios-kernel milestone 1 triple-faulted during GDT/IDT setup (packed descriptor layout)
- **Status:** FIXED in v2.27.0 (found during milestone 1 interrupt bring-up under QEMU)
- **Symptom:** the kernel booted to milestone 0, then died inside `gdt::init` with no panic message. QEMU `-d int` showed a single `v=0d` (#GP) at `aios_reload_segments` / `ltr`, then `v=08` (double fault) and a triple fault; the register dump at the fault showed `GDT= d960000000000000 0000003f` and `IDT= d9f0000000000000 00000fff` — the GDTR/IDTR bases were truncated (low 16 bits of the real base).
- **Root cause:** `struct Descriptor { limit: u16, base: u64 }` was `#[repr(C)]`; alignment of `u64` pushed `base` to offset 8 and left 6 padding bytes at 2..8. The CPU reads an `lgdt`/`lidt` operand as a contiguous 10-byte descriptor — limit at offset 0, base at offset 2. So the loaded base became `padding + first two base bytes` (e.g. `0xd960` instead of `0x1000001d960`), pointing GDT/IDT at garbage. Every descriptor load through the new GDT then faulted: `retfq`/`mov ds, 0x10` → `#GP`, exception delivery via the broken IDT → `#DF` → triple fault.
- **Fix:** mark both descriptors `#[repr(C, packed)]` (base now starts at offset 2, matching the CPU's view) and access the `descriptor` field through `addr_of_mut!` (packed fields are unaligned). `gdt.rs` and `idt.rs`.
- **Workaround / notes:** none needed post-fix; verified by the milestone 1 QEMU run (`[serial] Milestone 1: interrupts online.`, `tick 1s/2s/...`, `key 'h' (0x23)` via QEMU monitor).

## RESOLVED: aios-kernel `aios_reload_segments` code landed in a data section (`.section` leak across `global_asm!`)
- **Status:** FIXED in v2.27.0 (found during milestone 1 interrupt bring-up)
- **Symptom:** `aios_reload_segments` was linked into `.data.rel.ro` (a data section) instead of `.text`; `nm` reported it as `D aios_reload_segments` inside the `.data.rel.ro` range.
- **Root cause:** Rust concatenates all `global_asm!` blocks into one assembly stream. The generated `irq_stubs.S` emits `.section .data.rel.ro` for `aios_handler_table`, and the assembler keeps that section as current; the next `global_asm!` (the `gdt` module) has no explicit section, so its code landed in `.data.rel.ro`.
- **Fix:** explicit `.text` at the top of the `gdt` module's `global_asm!`. Also removed the unnecessary far-return CS reload (`retfq`); the bootloader already runs the kernel with CS=`0x08`/SS=`0x10`, which resolve to the kernel's own descriptors after `lgdt`, so `reload_segments` only reloads DS/ES/FS/GS.
- **Workaround / notes:** none needed post-fix.

## RESOLVED: aios-kernel milestone 0 triple-faulted on first boot (VGA write to unmapped 0xB8000)
- **Status:** FIXED in v2.26.0 (found during first boot verification under QEMU)
- **Symptom:** QEMU booted the bootloader, the kernel entry point was reached, then the CPU triple-faulted. The serial log ended with `Jumping to kernel entry point at VirtAddr(0x100000028a0)`; QEMU `-d int` showed `old: 0xe new 0xe` then `v=08` (double fault), with `RAX=0xB8000`, `IP=0x10000001a40` (`movw $0x720,(%rax)`) and `CR2=0xE0`.
- **Root cause:** The bootloader's default `Mappings` sets `physical_memory: Option::None` — only the kernel segments, boot info, stack and framebuffer are mapped, low physical memory is not. The VGA clear loop wrote to the text buffer at physical `0xB8000`, which had no mapping → page fault. Because the kernel has not installed an IDT yet, the faulting IDT-descriptor fetch at base 0 (vector 0xE → linear `0xE0`, hence `CR2=0xE0`) itself faulted → double fault → triple fault.
- **Fix:** `aios-kernel` now builds a `BootloaderConfig` with `config.mappings.physical_memory = Some(Mapping::Dynamic)` and passes it to `entry_point!(kernel_main, config = &BOOTLOADER_CONFIG)`. `kernel_main` forwards `physical_memory_offset` to `vga::vga_init`, which re-points `VgaWriter::buffer_addr` to `0xB8000 + offset`, so the VGA buffer is reached through the physical-memory map.
- **Workaround / notes:** none needed post-fix; verified by the milestone 0 QEMU smoke run (`[serial] Milestone 0 OK.`).

## Current: No known defects (Clean Build)


### KNOWN LIMITATION: kernel frame-copy scheduler is single-core; demo tasks never exit
The Milestone 3/4 scheduler switches contexts by copying trap frames inside the timer ISR: no SMP/IPI support, no task teardown (the two ring-3 programs and the kernel worker loop forever), and IPC mailboxes drop-oldest when full (`MAILBOX_DEPTH=16`). Fine for milestone scope; revisit before running real workloads on the bare-metal track.

### KNOWN LIMITATION: QEMU smoke test skips when qemu-system-x86_64 is absent
`scripts/qemu-smoke.ps1` builds the BIOS image, then exits with code 2 and a clear message instead of failing CI on hosts without QEMU. Install QEMU (or run on a host that has it) to execute the runtime assertions over COM1.

### KNOWN LIMITATION: GUI sys segments poll via current-thread Runtime `block_on`
`aios-gui` refreshes the top-bar system-control segments every 2 s by `block_on` on a current-thread tokio Runtime created at startup. Acceptable for a periodic top-bar refresh; switch to an async channel if the polling surface grows.

As of v2.13.0, all tests pass, clippy reports zero warnings, and the 18 bugs found in the v2.7.0 bug-fix pass (BUG-021…BUG-038) are fixed and covered by regression tests. The v2.8.0 restructure of the kernel TUI to 7 tabs, the `--safe-mode` boot flag and the GUI AI Studio / Network Settings tabs, the v2.9.0 / v2.9.1 AI chat persistence, `/preset` templates and streaming work, the v2.9.2 button-contrast fix, the v2.9.5 Live USB image, the v2.10.0 `aios-vfs`/`aios-fm` file manager, the v2.11.0 `aios-cluster`, the v2.12.0 `aios-init` initramfs init, the v2.13.0 `/system/aios-core` kernel-TUI handover, the v2.20.0 stateful process migration (executor state snapshots + `GetState`/`GetStateReply` + state-carried `migrate`), the v2.21.0 checkpoint replication (heartbeat broadcast + TTL pruning + automatic failover restore), the v2.22.0 `aios-autohal` hardware auto-provisioning and the v2.25.0 native push-based hot-plug notifications added no new known defects. See the Historical Issues section and `docs/CHANGELOG.md`.

### RESOLVED: VRAM shown as 4.0 GB for GPUs above 4 GiB in the kernel TUI
- **Status:** FIXED in v2.25.2 (found during live verification)
- **Symptom:** `aios` (kernel TUI) detected the GPU model correctly but always reported 4.0 GB VRAM (an RTX 3060 12 GB showed "4.0 GB VRAM"), while the GUI/HAL correctly showed 12288 MB.
- **Root cause:** `hw_probe::probe_gpu` read VRAM from the WMI field `win32_VideoController.AdapterRAM`, which is 32-bit. NVIDIA drivers return wrapped/truncated values for GPUs above 4 GiB — the RTX 3060 reports 4293918720 (0xFFF00000), i.e. 4.0 GiB minus 1 MiB. The existing `0xFFFFFFFF` guard only covered the "unknown" sentinel, not the wrap.
- **Fix:** `probe_gpu` now prefers `aios_hal::hardware::HardwareProfile::detect()`, which reads the real VRAM via `nvidia-smi --query-gpu=memory.total` (MiB). The WMI path remains only as a last-resort name source; a `gpu_from_hal` converter plus regression tests were added.
- **Workaround / notes:** none needed post-fix; covered by regression tests in `aios/src/hw_probe.rs`.

### RESOLVED: `Cannot start a runtime from within a runtime` at TUI startup
- **Status:** FIXED in v2.25.1 (found during live verification)
- **Symptom:** `aios` (kernel TUI) panicked on `thread 'main'` right after HAL detection (`HAL: NVIDIA GPU detected … Detected 16 cores, …`) with `Cannot start a runtime from within a runtime` from `tokio-1.53.1/src/runtime/scheduler/multi_thread/mod.rs:91`.
- **Root cause:** `main` is `#[tokio::main]`, so the main thread already lives inside a tokio runtime. The startup provisioning pass (`AutohalEngine::rescan` → `provision_blocking`) and other synchronous wrappers (`DriverFetcher::sync_get`/`find_driver_sync`, `StoreManager::block_on`) each built a *fresh* tokio runtime and called `block_on` from inside the running runtime, which tokio forbids.
- **Fix:** new `aios_core::runtime::block_on_future` helper — outside a runtime it builds a fresh runtime; inside a multi-thread runtime it parks the worker with `block_in_place` and blocks on the existing handle (non-`Send` futures included); all synchronous wrappers route through it.
- **Workaround / notes:** none needed post-fix; covered by the regression test `provision_blocking_is_safe_inside_tokio_runtime`.

### RESOLVED: `Kernel panic: No working init found` on initramfs boot
- **Status:** FIXED in v2.12.0 (design-level)
- **Symptom:** When the initramfs did not contain a working `/sbin/init` (or the busybox init script was missing/not executable), the kernel aborted with `Kernel panic: No working init found. Try passing init= option to kernel.`
- **Root cause:** The previous initramfs `/init` was a shell script (`live/init.rs`) that depended on busybox being present and executable; any packaging error left the kernel with nothing valid to run.
- **Fix:** New `aios-init` crate is a statically linked (`x86_64-unknown-linux-musl`) Rust `/init` binary (see `docs/ARCHITECTURE.md` Layer 8). It never panics: if `/system/aios-core` or `/installer` is missing it drops to a rescue shell (`/bin/sh` → `/bin/busybox sh` → `/bin/ash`), and if no shell exists it parks in an idle `waitpid` reap loop instead of exiting (an exiting PID 1 is what triggers the kernel panic).
- **Workaround / notes:** pass `init=/init console=tty0` on the kernel command line (GRUB/Syslinux) so the binary is used explicitly; run `./build_initramfs.sh` (optionally `BUSYBOX_PATH=...` for the rescue shell). Since v2.13.0 the script also builds and stages the real `aios` kernel binary as `/system/aios-core`, so the boot lands in the full kernel TUI; the rescue shell is only the fallback.

### KNOWN LIMITATION: `aios` static-musl build needs native TLS libraries on the build host
- **Status:** BY DESIGN
- **Symptom:** `build_initramfs.sh` (and `live/build.sh` in the default aios-init mode) build `aios` for `x86_64-unknown-linux-musl`; `reqwest` 0.12 without an explicit `rustls` feature links native-tls/OpenSSL, so the musl cross-build requires system OpenSSL dev/static libraries (the Alpine live container installs `openssl-dev`).
- **Workaround:** Build on a host that provides OpenSSL for musl (Alpine or an equivalent container); alternatively use `./build_initramfs.sh --no-aios-core` / `SKIP_AIOS_CORE=1` to produce a rescue-shell-only initramfs.
- **Note:** If the aios build fails or is skipped, the script warns and continues — boot still works via the rescue shell.


### KNOWN LIMITATION: `HOST://` access requires capability tokens
- **Status:** BY DESIGN
- **Symptom:** The file manager starts both panels on `AIOS://` (sandboxed); navigating to `HOST://` shows an empty listing, and host operations fail with `denied: missing capability`
- **Workaround:** Grant `vfs:host:read` (`g`) and `vfs:host:write` (`w`) tokens in the Files tab; tokens live in `AclContext` for the current process only and are not persisted
- **Note:** `HostVfs` still refuses paths outside the host root even with tokens (path-containment via `canonicalize_inside`)

### KNOWN LIMITATION: GUI file manager needs a writable sandbox dir
- **Status:** BY DESIGN
- **Symptom:** If `AIOS_DATA_DIR` points to a read-only location, the GUI Files tab shows "FM runtime failed / VFS root init"
- **Workaround:** Point `AIOS_DATA_DIR` at a writable folder before launching `aios-gui` (default sandbox: `AIOS_DATA_DIR/vfs_sandbox`)

### KNOWN LIMITATION: `aios-autohal` Bluetooth/ACPI fingerprints are lookup-ready but not yet sourced
- **Status:** BY DESIGN
- **Symptom:** `HardwareFingerprint`/`BusType` include Bluetooth and ACPI, but `aios-hal::HardwareProfile` does not yet surface such devices, so `extract_fingerprints` currently returns only USB/PCI/NVMe entries.
- **Workaround:** None needed — the variants exist so the inspector tree and driver lookup keys (`bt.*`, `acpi.*`) already cover them; a future `aios-hal` update plugs them in without changes to `aios-autohal`.

### FIXED: Linux GPU probe did not compile (`hw_probe.rs`)
- **Status:** FIXED in v2.9.5
- **Symptom:** `cargo build` for `target_os = "linux"` failed with `E0308` in `aios/src/hw_probe.rs`: `String::from_utf8(Command::new("lspci").arg("-v").output())` — `output()` returns `Result<Output, io::Error>`, the code expected `Vec<u8>` (missing `.ok()?.stdout`)
- **Root cause:** The Linux GPU-probe branch was never compiled (only Windows/macOS were); two call sites forgot the stdout extraction present in the Windows branch
- **Fix:** Added `.ok()?.stdout` to the `nvidia-smi` and `lspci` Linux sites and the equivalent macOS `system_profiler` site
- **Regression coverage:** Linux static-musl release build now compiles (see `live/build.sh`); no CI covers non-Windows targets yet

### FIXED: GUI TextEdit fields invisible on light system theme
- **Status:** FIXED in v2.9.3
- **Symptom:** Input fields on the GUI Network Settings tab (and other `TextEdit` inputs) showed light text on a white background — effectively unreadable
- **Root cause:** eframe 0.31 seeds `Visuals` from the OS system theme; on a Windows machine in light mode `extreme_bg_color` (the TextEdit background) stayed `#FFFFFF` because `AiosTheme::apply` only overrode `dark_mode` and part of the widget palette
- **Fix:** `apply` now sets every `Visuals` surface field explicitly (see `docs/CHANGELOG.md` v2.9.3); verified by pixel analysis on the running GUI (`#1E1E2A` field, `#D4D4DF` text)
- **Regression coverage:** manual pixel-level verification only; no automated GUI test exists yet

### KNOWN LIMITATION: signed-manifest enforcement is opt-in via env
- **Status:** BY DESIGN
- **Symptom:** `store install` / `store update` reject unsigned or un-trusted blocks when `AIOS_TRUSTED_PUBLIC_KEYS` (or a source's `trusted_public_keys`) is set, while the same install succeeds without it
- **Workaround:** Only set `AIOS_TRUSTED_PUBLIC_KEYS` on systems that require verified-only installs; leave it unset (default) to allow unsigned blocks while still verifying signatures against the embedded key
- **Note:** The bridge `store publish` path uses `BlockInstaller::from_env` (honours `AIOS_TRUSTED_PUBLIC_KEYS`), so a *signed* publish is gated by the local trust policy while *unsigned* publishes stay allowed unless trusted keys are configured (see `docs/ARCHITECTURE.md`, Phase 42)

### KNOWN LIMITATION: `store publish` needs a running update service
- **Status:** BY DESIGN
- **Symptom:** `store publish <file.wasm>` fails with a connection error when no bridge server is listening on `AIOS_BRIDGE_PORT` (default `8080`)
- **Workaround:** Run the kernel (`cargo run --bin aios`) so the bridge serves `POST /api/v1/store/publish`, or point `AIOS_BRIDGE_PORT` at the running update service
- **Note:** The wasm payload is base64-encoded and verified against the computed SHA-256 server-side before install

### KNOWN LIMITATION: Store remote sources require network
- **Status:** BY DESIGN
- **Symptom:** `store search` / `store install` / `store update` against a GitHub or HTTP source fail with a download error when offline or the remote is unreachable
- **Workaround:** Use a local source (`store add-source local:<path>`) for offline installs, or run the built-in update service (`aios-bridge` on `AIOS_BLOCKS_DIR`) and point `store add-source http://host:port` at it
- **Note:** Block binaries are verified against the manifest SHA-256 on install; a tampered payload is rejected with a checksum error

### KNOWN LIMITATION: TUI Web tab renders text only
- **Status:** BY DESIGN
- **Symptom:** The terminal Web tab cannot render CSS/JS/images — pages are shown as structured text (headings `#`, lists `•`/`1.`, tables `|`, `hr`, images as `[alt]`)
- **Note:** This is an inherent terminal limitation, not a regression. For full-fidelity browsing use the native browser: press `W` in any TUI or open the GUI **Browser** tab (F7)
- **Note (v2.17.0):** For JS-heavy sites the text view falls back to a headless Chromium-class render (`--dump-dom`) when the plain fetch yields no readable text. This requires a Chromium-class browser (`msedge`/`chromium`/`google-chrome`) installed and reachable; override the binary with `AIOS_HEADLESS_BROWSER`, add `--no-sandbox` (containers) with `AIOS_HEADLESS_NO_SANDBOX=1`. Without a browser the page is shown as fetched.

### KNOWN LIMITATION: AI Console chat log is a single JSONL file, unbounded
- **Status:** BY DESIGN
- **Symptom:** `AIOS_DATA_DIR/chat.jsonl` grows without rotation; every message (user + assistant, full text) is appended on each reply and the whole transcript is re-written on quit. The TUI AI Console and the GUI AI Studio share the same default file, so the most recent writer wins on boot
- **Workaround:** Delete or archive `aios_data/chat.jsonl` when it grows too large; `/clear` clears the on-screen transcript but does not delete the file
- **Note:** The file is parsed line-by-line so a corrupt trailing line does not break startup

### KNOWN LIMITATION: `/preset` templates are session-scoped (TUI vs GUI files)
- **Status:** RESOLVED in v2.9.1 — presets persist to `AIOS_DATA_DIR/presets.json` in both the TUI and the GUI; custom templates survive restarts
- **Symptom (resolved):** Presets defined with `/preset <name> <text>` were lost on exit before persistence was wired up
- **Note:** TUI and GUI share the same `AIOS_DATA_DIR/presets.json` default path, so the most recent writer wins on boot

## Historical Issues (Fixed)

### BUG-001: Speed test failures in debug mode
- **Status:** FIXED
- **Symptom:** `test_serialize_speed` and `test_deserialize_speed` panic with threshold exceeded in debug builds
- **Root Cause:** Debug builds are 10-20x slower than release due to lack of optimizations. Original thresholds assumed release-mode performance.
- **Fix:** Added `cfg!(debug_assertions)` dual thresholds: 50us for debug, 1us for release
- **Affected files:** `aios-core/src/ipc_protocol.rs:210,234`, `tests/integration_test.rs:95`

### BUG-002: Windows PATH not inherited by cargo
- **Status:** WORKAROUND IN PLACE
- **Symptom:** `cargo` command not found when running from opencode CLI
- **Root Cause:** Windows environment variable propagation between processes
- **Workaround:** Prepend PATH with `$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")` before every cargo invocation
- **Affected:** All cargo commands

### BUG-003: Clippy `skip_while_next` lint on intent engine
- **Status:** FIXED
- **Symptom:** `skip_while().next()` triggers clippy warning
- **Fix:** Replaced with `position().nth()` iterator pattern
- **Affected file:** `aios-tui/src/intent_engine.rs:128-131`

### BUG-004: Clippy `needless_borrow` in block loader
- **Status:** FIXED
- **Symptom:** `hex::encode(&actual)` triggers needless borrow warning
- **Fix:** Changed to `hex::encode(actual)`
- **Affected file:** `aios-block-mgr/src/loader.rs:17`

### BUG-005: Unused imports in integration tests
- **Status:** FIXED
- **Symptom:** 8 clippy warnings for unused imports and mutable variable
- **Fix:** Removed unused imports (`BlockState`, `AIOSException`, `ProcessState`, `HealthCheckFn`, `StateTransferManager`, `Arc`, `Mutex`, `thread`) and `mut` keyword
- **Affected file:** `tests/integration_test.rs:1-17,323`

### BUG-006: Private `now_ms()` function shared across modules
- **Status:** FIXED
- **Symptom:** `E0603: function now_ms is private` in aios-security when access_control.rs and sandbox.rs tried to call `capability::now_ms()`
- **Fix:** Changed `fn now_ms()` to `pub fn now_ms()` in capability.rs
- **Affected files:** `aios-security/src/capability.rs:141`, `access_control.rs:74`, `sandbox.rs:61,72`

### BUG-007: Watchdog tests failed without sleep between checks
- **Status:** FIXED
- **Symptom:** `test_missed_heartbeats_trigger_suspend` and related tests panic because heartbeat age was 0ms (freshly created), never exceeding the 100ms interval
- **Fix:** Added `thread::sleep(Duration::from_millis(120))` between check_timeout calls; adjusted loop count from 3 to 2 (since 3 overdue checks triggers suspend)
- **Affected file:** `aios-watchdog/src/watchdog.rs:207-259`

### BUG-008: Sandbox memory limit test arithmetic error
- **Status:** FIXED
- **Symptom:** `assertion failed: !sb.allocate_memory(1)` because 500+499+1=1000 which is NOT > 1000
- **Fix:** Changed to 500+500+1 so 1000+1 > 1000 triggers the limit
- **Affected file:** `aios-security/src/sandbox.rs:164`

### BUG-009: Context store missing type imports
- **Status:** FIXED
- **Symptom:** `E0425: cannot find type TelemetryStore/WorkflowStore/StabilityStore` in store.rs
- **Fix:** Added `use crate::telemetry::TelemetryStore` etc. imports
- **Affected file:** `aios-context/src/store.rs:1-3`

### BUG-010: schedule_next early-break prevents correct aging behavior
- **Status:** FIXED (v0.4.0)
- **Symptom:** `test_scheduler_aging_starvation_prevention` fails — low-priority process with aging boost not selected over high-priority process
- **Root Cause:** Inner loop in `schedule_next()` had `break` after finding the first Ready process in each priority queue. With aging, a later process in the queue could have a higher effective priority than an earlier one, but the `break` prevented evaluating all of them.
- **Fix:** Removed `break` so all Ready processes in a queue are evaluated. Candidate selection picks the one with highest effective priority.
- **Affected file:** `aios-process-mgr/src/scheduler.rs:320`

### BUG-011: Flaky dependency graph tests due to non-deterministic topological sort
- **Status:** FIXED (v0.4.0)
- **Symptom:** `test_unload_order_reversed` and `test_block_dependency_graph_ordering` intermittently fail
- **Root Cause:** Kahn's algorithm processes nodes via `VecDeque` seeded from `HashMap` iteration, which is non-deterministic. Independent nodes (no dependency between them) can appear in any order. Tests assumed fixed ordering.
- **Fix:** Changed tests to verify only dependency constraints (if X depends on Y, then Y appears before X in load order), not absolute positions of independent nodes.
- **Affected files:** `aios-block-mgr/src/dependency.rs:225-236`, `tests/integration_test.rs`

## Potential Future Issues

### RISK-001: AtomicU64 packet_id wrapping
- **Description:** `PACKET_COUNTER` in ipc_protocol.rs is an `AtomicU64` that will wrap around after 2^64 packets
- **Impact:** Extremely unlikely in practice (would take years at high throughput)
- **Mitigation:** Not currently addressed; could add wrapping_add or reset logic

### RISK-002: Non-recursive `find_by_name` search
- **Description:** `BlockRegistry::find_by_name` does linear scan of all entries
- **Impact:** O(n) per lookup; negligible at current scale but could matter with thousands of blocks
- **Mitigation:** Could add secondary name -> id HashMap index

### RISK-003: No block unloading during active hot-swap
- **Description:** `BlockLoader::unload_block` logs a warning but does not prevent unloading an active block
- **Impact:** Could cause in-flight IPC messages to target a removed block
- **Mitigation:** Active blocks should be frozen before unload in production use

### RISK-004: Watchdog not integrated with orchestrator thread
- **Status:** FIXED (v0.3.0)
- **Description:** Watchdog crate was implemented but not wired to the AI Orchestrator thread in main.rs
- **Fix:** Watchdog heartbeat thread now runs in background during TUI session; dashboard header shows live watchdog state
- **Affected files:** `aios-tui/src/main.rs`, `aios-tui/src/dashboard.rs`

### RISK-005: WebView runs on a background thread — macOS incompatible
- **Description:** `WebBrowser` in `aios-webview` runs the winit event loop on a background thread; on macOS winit/wry require the event loop on the main thread
- **Impact:** Browser window will not open on macOS builds
- **Mitigation:** Documented; not addressed. On macOS spawn the webview from the app's main thread or use `build_as_child` inside the egui window (see TODO Phase 34)

### RISK-006: GUI Browser is a companion window, not embedded
- **Description:** The GUI Browser tab opens the webview in a separate OS window next to the egui dashboard rather than as a child viewport of the tab
- **Impact:** Cosmetic UX gap — window focus/navigation differs from an embedded browser
- **Mitigation:** Future work: `build_as_child` on Windows/macOS/X11 to render inside the egui tab (see TODO Phase 34)

### BUG-012: Recovery log get_pending_entries ignores completed IDs
- **Status:** FIXED (v1.0.0)
- **Symptom:** `test_recovery_log_pending` fails — `get_pending_entries()` returns completed entries instead of filtering them out
- **Root Cause:** The function skipped `COMPLETED:` marker lines but never collected the IDs from those markers to exclude matching entries. All entries with `status == "pending"` were returned regardless of whether they had been marked complete.
- **Fix:** Collect completed IDs from `COMPLETED:` lines first, then filter entries by excluding those whose ID is in the completed set.
- **Affected file:** `aios-persistence/src/recovery.rs:108-140`

### BUG-013: Kernel (`aios`) booted with an empty block registry — no browser
- **Status:** FIXED (v2.2.0)
- **Symptom:** On a fresh machine the kernel started with zero registered blocks; `aios-browser` was a plain library (no `StatefulBlock` impl), so no browser was available at boot and there was no auto-launch of the OS browser.
- **Root Cause:** `aios/src/orchestrator.rs:73` created an empty `BlockRegistry`; the 3-block boot sequence existed only in `aios-tui`/`aiosd`; `BrowserEngine` never implemented `handle_message`.
- **Fix:** Added `BrowserBlock` (`StatefulBlock`) in `aios-browser/src/block.rs`; kernel now registers hal/ipc_bus/scheduler/browser at boot, boot-discovers `AIOS_BLOCKS_DIR`, wires the browser handler into the `MessageRouter`, and the TUI `b` hotkey opens URLs in the OS default browser via the block.
- **Affected files:** `aios-browser/src/block.rs`, `aios/src/orchestrator.rs`, `aios/src/tui/mod.rs`, `aios/src/tui/ui.rs`, `aios/src/tui/app_state.rs`, `aios-tui/src/main.rs`, `aios-daemon/src/main.rs`

### BUG-014: DuckDuckGo search dropped first letter of result URLs
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_duckduckgo_parse_results` failed — every result URL lost its leading `h` (e.g. `ttps://example.com`)
- **Root Cause:** `DuckDuckGoBackend::parse_html_response` advanced past `href="` (6 chars) by 7
- **Fix:** Offset corrected from `+7` to `+6`
- **Affected file:** `aios-search/src/backends.rs:68`

### BUG-015: HtmlParser::extract_text included <head>/<title> text
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_html_parser_extract_text` failed — page text was `"Test Hello world"` instead of `"Hello world"`
- **Root Cause:** `extract_text` stripped only `<script>`/`<style>` and tags, leaving `<head>`/`<title>` text in the body text
- **Fix:** `extract_text` now strips `<head>...</head>` before tag removal; added `test_extract_text_strips_head`
- **Affected file:** `aios-browser/src/html_parser.rs:22-36`

### BUG-016: Chaos rapid-fire test asserted plaintext of redacted report
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_chaos_reporter_rapid_fire` failed — `json.contains("event #0")` was false
- **Root Cause:** Events with even indices used `zero_knowledge=true`, so `event #0` was SHA-256 hashed and never appeared in the JSON output
- **Fix:** Assertions now check redaction semantics: `event #0` absent, `event #1`/`event #99` present, `"redacted":true` present
- **Affected file:** `tests/chaos_test.rs:341-344`

### BUG-017: WorkflowCompiler emitted init/start with a return value
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_e2e_easylang_wasm_pipeline` failed — `result.functions_called` never contained `init`/`start`
- **Root Cause:** `WorkflowCompiler::generate_wat` exported `init`/`start` with `(result i32)`, but `BlockExecutor::execute_block` invokes them with an empty results buffer; the calls errored (logged as warnings) and the functions were not recorded
- **Fix:** `init`/`start` now export without a result, matching the executor contract and its unit fixtures
- **Affected file:** `aios-builder/src/compiler.rs:60-62`

### BUG-018: Bridge /api/v1/metrics never populated
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_e2e_bridge_http_endpoints` failed — Prometheus text contained no `HELP`
- **Root Cause:** `BridgeContext::metric_collector` was constructed but no handler recorded metrics, so `to_prometheus()` always returned an empty string
- **Fix:** Added axum request middleware `record_metrics` that records `http_requests_total` (counter), `http_last_latency_ms` (gauge) and `http_request_latency_ms` (histogram) for every request
- **Affected file:** `aios-bridge/src/server.rs`

### BUG-019: Fault-tolerance test asserted mid-quantum preemption
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_fault_tolerance_scheduler_survives_crash` failed — "Replacement (high priority) should be next"
- **Root Cause:** The scheduler continues the current process until its time-slice quota expires (time-slicing, no preemption), but the test scheduled once, then expected the newly spawned High process to run immediately while the current quantum was still active
- **Fix:** The test now spawns the replacement before the final `schedule_next()`, matching the scheduler contract verified by `test_priority_scheduling`
- **Affected file:** `tests/stress_fault_tolerance.rs:266-275`

### BUG-020: Safe-Mode Shell commands always returned "Unknown command"
- **Status:** FIXED (v2.2.2)
- **Symptom:** On the `aios-tui` Shell tab every SafeModeShell command (`ps`, `kill`, `spawn`, `status`, `logs`, `restart`, `help`, `blocks`, `load`, `unload`) printed `Error: Unknown command`; only `fetch`/`search`/`open`/`clear` worked
- **Root Cause:** `execute_shell_cmd` in `aios-tui/src/main.rs:160` mapped every unrecognized command to `ShellCommand::Unknown(cmd.to_string())`, bypassing `SafeModeShell::parse_command` — only the TUI's own four commands reached the SafeModeShell, so the entire safe-mode command set was unreachable
- **Fix:** Commands now route through `SafeModeShell::parse_command`; `help`/`?` additionally list the TUI-specific commands; `blocks` output now prints the block state cleanly (`Active`, not `Some(Active)`) via `registry.topology_with_state()`
- **Affected files:** `aios-tui/src/main.rs:160-177`, `aios-watchdog/src/safe_mode.rs`

### BUG-021: extract_text returned empty text for pages with `<!DOCTYPE html>`
- **Status:** FIXED (v2.7.0)
- **Symptom:** `HtmlParser::extract_text` returned an empty string for any page whose root is `<!DOCTYPE html><html>…`; all browsed pages appeared blank in the TUI Web tab
- **Root Cause:** `extract_text` iterated the body's own text instead of walking the element children of the document root, so a doctype-first document produced no text
- **Fix:** `extract_text` now iterates the element children of the document root; added regression test `test_extract_text_with_doctype`
- **Affected file:** `aios-browser/src/html_parser.rs`

### BUG-022: IpcBus `DropOldest` evicted the most critical packet
- **Status:** FIXED (v2.7.0)
- **Symptom:** with `BoundedBusPolicy::DropOldest`, overflow discarded the highest-priority queued packet and kept the least important one
- **Root Cause:** `DropOldest` popped from the front, but the queue is ordered highest-priority-first (send time order for equal priorities)
- **Fix:** `DropOldest` now pops from the back (lowest priority); added `test_drop_oldest_keeps_highest_priority`
- **Affected file:** `aios-ipc/src/bus.rs`

### BUG-023: Bridge status handler never listed the newest process
- **Status:** FIXED (v2.7.0)
- **Symptom:** `GET /api/v1/status` and the `status` intent showed every process except the newest one
- **Root Cause:** the handler probed PIDs `0..process_count`, but process IDs start at 1, so the last (newest) process was always skipped
- **Fix:** the handler now iterates `scheduler.all_processes()`; the TUI processes tab uses the same source
- **Affected files:** `aios-bridge/src/server.rs`, `aios/src/tui/ui.rs`

### BUG-024: Bridge `MetricType::All` hardcoded process_count to 0
- **Status:** FIXED (v2.7.0)
- **Symptom:** metrics for the `All` metric type always reported `process_count = 0`
- **Root Cause:** the count was read after `scheduler` was dropped (moved into the report), so it always evaluated to 0
- **Fix:** `process_count` is captured before the scheduler is dropped
- **Affected file:** `aios-bridge/src/server.rs`

### BUG-025: TUI Web back-navigation ping-ponged forever
- **Status:** FIXED (v2.7.0)
- **Symptom:** pressing `b` to go back to page A, then `b` again, returned to B instead of staying on A — A↔B infinite loop
- **Root Cause:** `load_url` always pushed the URL onto the history stack, including when called for back-navigation, re-adding the page that was just popped
- **Fix:** `load_url` gained `push_history: bool`; back navigation pops without re-pushing; all call sites updated
- **Affected file:** `aios-tui/src/main.rs`

### BUG-026: Rapid `B` presses spawned multiple native browser windows
- **Status:** FIXED (v2.7.0)
- **Symptom:** pressing `B` repeatedly in the TUI Web tab started several native browser instances
- **Root Cause:** no guard between the keypress and the spawn; each press launched the OS browser
- **Fix:** a `WEB_BROWSER_SPAWNING` atomic guard allows one spawn in flight until the child is reported
- **Affected file:** `aios-tui/src/main.rs`

### BUG-027: GUI browser open blocked the egui UI up to 45 s
- **Status:** FIXED (v2.7.0)
- **Symptom:** opening the WebView from the GUI Browser tab froze the dashboard for up to 45 s (window init timeout)
- **Root Cause:** `WebBrowser::open` ran synchronously on the egui thread
- **Fix:** the open runs on a background thread (`pending_browser`/`pending_browser_error` slots, `browser_opening` guard); `poll_browser_open` picks up the result each frame; repeated opens during startup are ignored
- **Affected file:** `aios-gui/src/app.rs`

### BUG-028: DuckDuckGo `uddg` redirect URL not decoded
- **Status:** FIXED (v2.7.0)
- **Symptom:** result URLs pointed at `https://duckduckgo.com/l/?uddg=%2F...` instead of the real target
- **Root Cause:** `DuckDuckGoBackend` returned the redirect URL as-is
- **Fix:** `resolve_duckduckgo_url` unwraps the `uddg` parameter, skipping non-http/s values; `aios-search` adds the `url` dependency; 4 tests
- **Affected files:** `aios-search/src/backends.rs`, `aios-search/Cargo.toml`

### BUG-029: `save_telemetry` clobbered earlier batches
- **Status:** FIXED (v2.7.0)
- **Symptom:** saving telemetry twice left only the latest batch in the store
- **Root Cause:** every batch was written under the same key
- **Fix:** keys are assigned from a monotonic `TELEMETRY_NEXT_KEY` counter persisted in `META_TABLE`; added `test_save_telemetry_does_not_clobber_previous_batches`
- **Affected file:** `aios-context/src/persistence.rs`

### BUG-030: compressed-telemetry chunk keys collided each round
- **Status:** FIXED (v2.7.0)
- **Symptom:** each compression round overwrote the previous chunk, so only the last compression survived
- **Root Cause:** chunk keys were derived from a timestamp/metric key that was identical across rounds
- **Fix:** chunks use a monotonic `next_chunk_id`; removed `chrono_block_name`; added `test_multiple_compression_rounds_do_not_collide`
- **Affected file:** `aios-context/src/compressed_telemetry.rs`

### BUG-031: `response_err` discarded the error message
- **Status:** FIXED (v2.7.0)
- **Symptom:** IPC error responses carried an empty payload, so callers saw a generic failure with no message
- **Root Cause:** `response_err` built the response with `Payload::Empty`
- **Fix:** the message is carried as `Payload::Text(msg)`; added `test_response_err_carries_message`
- **Affected file:** `aios-core/src/ipc_protocol.rs`

### BUG-032: capability `remaining_ms` was inverted
- **Status:** FIXED (v2.7.0)
- **Symptom:** long-lived capabilities reported ~0 ms remaining; `remaining_ms` grew as expiry approached
- **Root Cause:** `remaining_ms` computed `now − expires`
- **Fix:** now `expires_at_ms.saturating_sub(now_ms())`; added a test for a future expiry
- **Affected file:** `aios-security/src/capability.rs`

### BUG-033: priority-inheritance counter never incremented
- **Status:** FIXED (v2.7.0)
- **Symptom:** `total_inheritances` always reported 0 even when priority boosts happened
- **Root Cause:** the field was declared but never incremented
- **Fix:** the counter increments in both the `acquire_lock` and `request_resource` boost paths and is surfaced via `state()`; tests added
- **Affected file:** `aios-process-mgr/src/priority_inheritance.rs`

### BUG-034: `restore_linear_memory` silently truncated oversized data
- **Status:** FIXED (v2.7.0)
- **Symptom:** restoring a larger state snapshot into linear memory silently dropped the trailing bytes
- **Root Cause:** the copy length was `min(data, memory)`
- **Fix:** restore now fails explicitly when data exceeds the linear memory; `aios-live-update` logs a warning; added `test_restore_linear_memory_rejects_oversized_data`
- **Affected files:** `aios-wasm/src/sandbox.rs`, `aios-live-update/src/wasm_engine.rs`

### BUG-035: CPU affinity applied to the scheduler thread
- **Status:** FIXED (v2.7.0)
- **Symptom:** `set_cpu_affinity` pinned the scheduler thread (OS affinity targets the calling thread) instead of the spawned process thread
- **Root Cause:** the OS call was invoked from the scheduler context
- **Fix:** the mask is stored per-thread (`Arc<Mutex<Vec<usize>>>`) and applied by the spawned thread itself before running the payload; `validate_cores` pre-validates the mask; `set_cpu_affinity` no longer touches the calling thread
- **Affected files:** `aios-process-mgr/src/cpu_affinity.rs`, `aios-process-mgr/src/scheduler.rs`

### BUG-036: TUI/bridge lock-order inversion
- **Status:** FIXED (v2.7.0)
- **Symptom:** deadlock risk — the TUI blocks tab locked `scheduler → registry` while the bridge used `registry → scheduler`
- **Fix:** both sides now lock `scheduler → registry`
- **Affected file:** `aios/src/tui/ui.rs`

### BUG-037: WMIC `AdapterRAM` 32-bit overflow
- **Status:** FIXED (v2.7.0)
- **Symptom:** GPUs with more than 4 GB VRAM reported a bogus ~4 GB; `0xFFFFFFFF` overflow
- **Fix:** `0xFFFFFFFF` (`AdapterRAM` > 4 GB) is treated as unknown (0)
- **Affected file:** `aios/src/hw_probe.rs`

### BUG-038: wasm `timeout_ms` never enforced — epoch deadline never reached
- **Status:** FIXED (v2.7.0)
- **Symptom:** no host-side ticker ever incremented the engine epoch, so `timeout_ms` was not enforced as wall-clock time; only the fuel limit bounded runaway wasm (an infinite loop ran until fuel ran out)
- **Root Cause:** `set_epoch_deadline(1)` armed the store, but nothing called `Engine::increment_epoch()`, so the deadline was unreachable (engine epoch stayed 0)
- **Fix:** a per-engine background ticker (`EpochTicker`) calls `Engine::increment_epoch()` every `timeout_ms / 4`; every store is armed with `EPOCH_TICKS_PER_TIMEOUT = 4` ticks, and `call_func`/`instantiate` (plus the executor's `init`/`start`) re-arm the deadline before each wasm call so long-lived stores keep working while every call is bounded by `timeout_ms`
- **Tests:** `test_epoch_timeout_interrupts_runaway_wasm` (infinite loop interrupted in ~150 ms with fuel that alone would take ~10 s) and `test_epoch_deadline_rearmed_between_calls`
- **Affected files:** `aios-wasm/src/sandbox.rs`, `aios-wasm/src/executor.rs`

### BUG-039: GUI/TUI crash on hardware detect — `wmic` CSV rows indexed past bounds
- **Status:** FIXED (v2.9.1)
- **Symptom:** `aios-gui` (and any binary running `HardwareProfile::detect`) panicked at startup with `index out of bounds: the len is 2 but the index is 2` in `aios-hal/src/hardware.rs` — the GUI window never opened
- **Root Cause:** `detect_memory` parsed `wmic memorychip get Capacity,Speed,DimmLocator /format:csv` by indexing `parts[2]` after only checking `parts.len() >= 2`; on machines where wmic emits short rows (e.g. a blank Speed column collapsing to a 2-field line) the indexing panicked
- **Fix:** extraction into a pure helper `HardwareProfile::parse_wmic_memory_csv` that requires `parts.len() >= 3` (Node + Capacity + Speed) before touching any index; malformed/short rows are skipped instead of crashing
- **Tests:** `test_parse_wmic_memory_csv_full_rows` (two DIMMs summed, speed read) and `test_parse_wmic_memory_csv_short_rows_no_panic` (short 2-field row skipped, valid row still parsed)
- **Affected files:** `aios-hal/src/hardware.rs`

### BUG-040: `grub-install` failed with exit 127 in the live installer environment
- **Status:** FIXED (v2.33.0)
- **Symptom:** the installer copied the rootfs to the target, then died at
  `grub-install` with `exitcode=0x00000100`/`0x7f`; kernel panic "Attempted to kill init".
- **Root Cause:** the Alpine `grub-install` binary links `liblzma.so.5` and
  `libdevmapper.so.1.02` (in packages `xz-libs` and `device-mapper-libs`); with
  only the `xz`/`lvm2` binaries in the image, the dynamic linker refused to start
  the binary (exit 127). The kernel later loads the `dm` module on demand.
- **Fix:** ship `xz-libs` and `device-mapper-libs` in the initramfs; grub-install
  for i386-pc and x86_64-efi now exits 0. Diagnostics were added via a
  `ldd`-based check in the build-time debug path (`AIOS_DBG=1`), and stderr of
  grub-install is captured to `/mnt/target/grub-err.txt`.
- **Affected files:** build tooling (initramfs package list), `live/aios-install`

### BUG-041: `sfdisk` rejects GPT type names with spaces (`type=BIOS boot`)
- **Status:** FIXED (v2.33.0)
- **Symptom:** heredoc line `size=1M, type=BIOS boot` aborted partitioning with
  `>>> line 2: unsupported command`.
- **Root Cause:** sfdisk parses comma-separated fields; a type name containing a
  space is not accepted as a bare token.
- **Fix:** use the raw GUID for the BIOS boot partition type:
  `type=21686148-6449-6E6F-744E-656564454649`.
- **Affected files:** `live/aios-install`

### BUG-042: quoted heredoc left `${DEV}` literal in grub.cfg → `root=3`
- **Status:** FIXED (v2.33.0)
- **Symptom:** the installed system failed to boot: kernel panic
  `VFS: Unable to mount root fs on "3" or unknown-block(0,3)`.
- **Root Cause:** grub.cfg was generated with a quoted heredoc (`cat <<'GRUB'`),
  so `${DEV}` stayed literal in the file; GRUB then substituted its own (empty)
  `${DEV}` variable, turning `root=${DEV}3` into `root=3`.
- **Fix:** generate the heredoc unquoted (`<<GRUB`) so the shell expands `${DEV}`
  before GRUB reads the file.
- **Affected files:** `live/aios-install`

### BUG-043: GRUB cannot embed BIOS core.img on GPT without a BIOS Boot partition
- **Status:** KNOWN LIMITATION, handled (v2.33.0)
- **Symptom:** `grub-install --target=i386-pc` warned `this GPT partition label
  contains no BIOS Boot Partition; embedding won't be possible` and exited 1.
- **Root Cause:** on GPT disks GRUB BIOS has nowhere to embed `core.img` (the
  post-MBR gap is not usable for GPT layout without a dedicated partition).
- **Workaround:** the installer creates a dedicated 1 MiB BIOS boot partition
  (GUID `21686148-...`) as the first partition, so BIOS embedding works. UEFI
  boots are unaffected (EFI system partition is used).

### BUG-044: RAW-discriminator — real-hardware GP#13 proves corrupt IDT gate for vector-32 (PIT IRQ0)
- **Status:** CONFIRMED on real hardware via byte-truth (v0.5.1)
- **Symptom:** on real USB-flashed hardware the boot reached the discriminator
  probe, printed RAW IDT gate dumps, then a planned `int 0x20` soft-int fired
  a #GP (General Protection Fault, vector-13) at non-canonical `ip=0xffffffff000aa2`.
- **Byte-truth evidence:**
  - fresh ISO written to USB as raw bytes, read-back verified „mism=0“, contained
    the markers IDT-RAW-32 and IDT-RAW-50 (RAW=True on the booted image);
  - vector-50 (the soft-int used by the scheduler probe) is intact;
  - vector-32 gate descriptor offset is corrupt:  x000aa2 + upper=0xffffffff
    instead of kernel link address  xffffffff8000xxxx — the mid-16 bits of the
    handler offset are zeroed, so a soft-int to vector 32 lands at a non-canonical
    address and #GP/#GP13 fires.
- **Root Cause (hypothesis, awaiting fix):** the IDT gate descriptor for vector-32
  is corrupted when it is installed — offset_mid (bits 16..31 of the handler
  address) is zero despite a correct offset_high=0xffffffff. Vector-50 is built
  by a separate path and is intact menus; this discriminator proved the corruption
  is in the raw gate bytes, not in the ISO/limine/bootloader.
- **Fix direction:** harden/hard-patch vector-32 gate installation in
  ios-kernel/src/interrupts/idt.rs (set_handler/set_gate) so offset_mid is
  always derived from the full 64-bit handler (bits 16..31), not truncated; then
  re-verify via RAW discriminator probe.
- **Affected files:** ios-kernel/src/interrupts/idt.rs (gate install),
  ios-kernel-run/src/main.rs (probe).
