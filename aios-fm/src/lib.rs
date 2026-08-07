//! Two-panel file manager (Volkov/Far style) with async VFS operations,
//! background copy/move/delete jobs with live progress and cancellation,
//! smart AI previews (`F3`) and capability-gated access to the host
//! filesystem (`HOST://`).
//!
//! The engine (`engine`) is UI-agnostic: it owns two `PanelState`s and
//! processes `Command`s arriving over a `tokio::mpsc` channel, returning
//! `Ack`s over a second channel. Both `ui_tui` (ratatui) and `ui_gui`
//! (egui) render the same engine snapshot, which guarantees UI parity.

pub mod commands;
pub mod engine;
pub mod state;
pub mod ui_gui;
pub mod ui_tui;
