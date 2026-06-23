// This file is `#[path]`-included from BOTH `gateway::handlers::backup` (used
// by the API response handler) AND `dto_export::backup_dto` (used by
// `gen_ts_types` to register the TS type). Keeping the struct shape in one
// file guarantees no drift between API JSON output and the generated TS type.
//
// `register_ts_dto!()` is NOT called here because the file is compiled twice;
// registration happens explicitly in `dto_export::mod.rs` against
// `backup_dto::BackupEntryDto` (the dto_export path).

use serde::Serialize;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct BackupEntryDto {
    pub filename: String,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub size_bytes: u64,
    pub created_at: Option<String>,
}
