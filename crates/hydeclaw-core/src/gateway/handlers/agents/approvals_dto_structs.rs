use serde::Serialize;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct ApprovalEntryDto {
    pub id: String,
    pub agent_id: String,
    pub tool: String,
    #[cfg_attr(feature = "ts-gen", ts(type = "Record<string, unknown>"))]
    pub arguments: serde_json::Value,
    #[cfg_attr(feature = "ts-gen", ts(type = "\"pending\" | \"approved\" | \"rejected\""))]
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolved_by: Option<String>,
}
crate::register_ts_dto!(ApprovalEntryDto);
