//! Shared Web3 provider assets for the embedded and native browsers.
//!
//! The same EIP-1193-shaped script is injected before page code both into the
//! Boa context (through [`crate::script::ScriptEngine`]) and into the native
//! WebView (through `wry::WebViewBuilder::with_initialization_script`). Rust
//! never evaluates untrusted script here — it only owns the transport text.

/// JavaScript source of the injected `window.ethereum` provider.
pub const PROVIDER_JS: &str = include_str!("provider.js");

/// Return the injected provider source (stable helper for other crates).
pub const fn provider_js() -> &'static str {
    PROVIDER_JS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defines_eip1193_surface() {
        assert!(PROVIDER_JS.contains("global.ethereum = provider"));
        assert!(PROVIDER_JS.contains("request: request"));
        assert!(PROVIDER_JS.contains("__aiosWeb3Resolve"));
        assert!(PROVIDER_JS.contains("global.ipc.postMessage"));
        assert!(PROVIDER_JS.contains("__aiosWeb3Invoke"));
    }
}
