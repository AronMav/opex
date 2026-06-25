// Leaf module — only serde + ts_rs, no crate::* imports.
// `#[path]`-included from both `gateway::handlers::agents::checkpoints` (handler)
// and `dto_export::checkpoints_dto` (gen_ts_types). `register_ts_dto!()` calls
// live here so they are registered exactly once (from the dto_export path).
//
// Do NOT add `register_ts_dto!()` here; registration happens in dto_export/mod.rs
// to avoid double-registration (the file is compiled twice via two #[path] hosts).

use serde::Serialize;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct CheckpointMetaDto {
    pub n: usize,
    pub commit: String,
    pub created: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct CheckpointListDto {
    pub enabled: bool,
    pub items: Vec<CheckpointMetaDto>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct RestoreReportDto {
    pub n: usize,
    pub files: Vec<String>,
    pub new_checkpoint: Option<usize>,
}
