use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct ToolEntryDto {
    pub name: String,
    pub url: String,
    pub tool_type: String,
    pub concurrency_limit: u32,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub healthcheck: Option<String>,
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub ui_path: Option<String>,
    pub managed: bool,
}
crate::register_ts_dto!(ToolEntryDto);

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct McpEntryDto {
    pub name: String,
    pub url: Option<String>,
    pub container: Option<String>,
    pub port: Option<u16>,
    pub mode: String,
    pub protocol: String,
    pub enabled: bool,
    pub status: Option<String>,
    #[cfg_attr(feature = "ts-gen", ts(type = "number | null"))]
    pub tool_count: Option<usize>,
}
crate::register_ts_dto!(McpEntryDto);
