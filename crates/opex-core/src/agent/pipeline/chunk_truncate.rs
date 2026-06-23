//! Pure-string helpers for memory chunk truncation.
//!
//! Leaf module: depends only on `std` (`floor_char_boundary`). Safe to re-mount
//! into `lib.rs` for the integration-test bridge without cascading the rest
//! of the pipeline subtree. Both `agent::pipeline::memory` (production caller)
//! and `__memory_pipeline_bridge` (test facade) re-export from here, so there
//! is exactly one canonical implementation.

pub(crate) const MEMORY_CHUNK_MAX_CHARS: usize = 6_000;

/// Truncate a memory chunk to fit context budget.
///
/// Excalidraw docs are replaced with a short placeholder; other content
/// is hard-capped at `MEMORY_CHUNK_MAX_CHARS` by Unicode scalar boundary.
pub(crate) fn truncate_chunk_content(content: &str) -> &str {
    if content.contains("excalidraw-plugin: parsed")
        || content.contains("== EXCALIDRAW VIEW ==")
    {
        return "[Excalidraw diagram — binary content, skipped]";
    }
    let limit = content.floor_char_boundary(MEMORY_CHUNK_MAX_CHARS.min(content.len()));
    &content[..limit]
}
