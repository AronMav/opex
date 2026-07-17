pub mod client;
pub mod framing;
pub mod jsonrpc;
pub mod manager;
pub mod servers;
pub mod transport;

#[allow(unused_imports)]
pub use manager::LspManager;

use std::sync::OnceLock;

/// Process-wide `[lsp] enabled` flag, set once at startup from config.
static LSP_ENABLED: OnceLock<bool> = OnceLock::new();

/// Record whether LSP is enabled (called once at startup, mirrors the
/// `lsp_manager` construction in `main.rs`).
pub fn set_lsp_enabled(enabled: bool) {
    let _ = LSP_ENABLED.set(enabled);
}

/// Whether the `lsp` tool should be advertised in agents' tool schemas.
/// Defaults to `false` until [`set_lsp_enabled`] runs (e.g. in tests).
/// Execution is separately gated by the presence of the `LspManager`.
pub fn lsp_enabled() -> bool {
    *LSP_ENABLED.get().unwrap_or(&false)
}
