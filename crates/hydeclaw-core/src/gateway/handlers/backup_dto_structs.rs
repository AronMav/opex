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
