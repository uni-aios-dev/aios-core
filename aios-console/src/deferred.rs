//! Commands intentionally excluded from the text-only console.
//!
//! The declarations below are kept as a commented design sketch. They are not
//! registry entries, so `help` never advertises them and `resolve` reports them
//! as deferred with an explicit reason instead of silently accepting them.
//! Re-enable a variant only together with a real text backend; none of these
//! may be implemented through a GUI, WebView or LLM call.

// ── DEFERRED / ЗАКОМЕНТИРОВАНО ─────────────────────────────────────────────
// Причина: графический, веб-, LLM-, кластерный или чисто визуальный функционал.
// Раскомментировать при появлении текстового backend'а с реальной семантикой.
//
// Command::GuiEnter              // ring-0 windowed GUI (kernel tui.rs:217, gui.rs)
// Command::GuiLeave              // kernel tui.rs:224
// Command::GuiLaunchHost         // aios_webview::launch_gui, hotkey `W`
// Command::WebOpen { url }       // WebView; not a text console operation
// Command::WebSearch { query }   // DuckDuckGo fetch/search/open
// Command::WebNavigate { .. }    // page banner, links, omnibox
// Command::AiQuery { prompt }    // LLM streaming in aios/tui/mod.rs:319
// Command::AiSlash { cmd }       // /model /backend /key /temp /preset ...
// Command::IntentNatural { text }// RU/EN natural language -> intent plan
// Command::ClusterStatus         // cluster status/nodes
// Command::ClusterSpawn { .. }   // cluster spawn/migrate/kill
// Command::StorePublish { .. }   // POST /api/v1/store/publish + signing key
// Command::StoreSign { .. }      // Ed25519 sidecar, developer workflow
// Command::PerfTuneVideo         // intent "optimize for video editing"
// Command::MemoryCompactionFake  // currently only {"compaction":"triggered"}
//
// Phase 2 (syntax extensions, not OS commands):
// Command::Pipe { .. }           // `|`
// Command::Redirect { .. }       // `>`, `>>`
// Command::Background { .. }     // partially present via trailing `&`
// Command::ShellTheme { .. }     // colours/ANSI; text console stays monochrome
// ───────────────────────────────────────────────────────────────────────────
