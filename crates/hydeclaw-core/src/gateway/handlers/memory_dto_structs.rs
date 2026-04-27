// Leaf file — zero crate::* imports. Included via include!() in memory.rs
// and re-exported in lib.rs dto_export for ts-gen.
use serde::Serialize;

// ── MemoryDocument DTO (GET /api/memory/documents) ───────────────────────────
// Two modes:
//   List mode: created_at/accessed_at/scope present; similarity absent.
//   Search mode: similarity present; created_at/accessed_at/scope absent.
// Absent-in-mode fields are None and skipped in serialization.

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct MemoryDocumentDto {
    pub id: String,
    pub source: Option<String>,
    pub pinned: bool,
    pub relevance_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub similarity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub accessed_at: Option<String>,
    pub preview: Option<String>,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub chunks_count: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number | null"))]
    pub total_chars: Option<i64>,
    pub category: Option<String>,
    pub topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub scope: Option<String>,
}
crate::register_ts_dto!(MemoryDocumentDto);

// ── MemoryStats DTO (GET /api/memory/stats) ───────────────────────────────────
// Drift fix: handler emits `tasks` sub-object but api.ts MemoryStats did not declare it.

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct MemoryTaskStatsDto {
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub pending: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub processing: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub done: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub failed: i64,
}
crate::register_ts_dto!(MemoryTaskStatsDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct MemoryStatsDto {
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub total: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub total_chunks: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub pinned: i64,
    pub avg_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub embed_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub embed_dim: Option<i32>,
    pub tasks: MemoryTaskStatsDto,
}
crate::register_ts_dto!(MemoryStatsDto);
