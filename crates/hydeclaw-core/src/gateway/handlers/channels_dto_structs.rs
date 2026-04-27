// Leaf file — zero crate::* imports. Included via include!() in channels.rs
// and re-exported in lib.rs dto_export for ts-gen.
use serde::Serialize;

// ── ChannelRow DTO (GET /api/{agent}/channels, GET /api/channels) ───────────

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct ChannelRowDto {
    pub id: String,
    pub agent_name: String,
    pub channel_type: String,
    pub display_name: String,
    #[cfg_attr(feature = "ts-gen", ts(type = "Record<string, unknown>"))]
    pub config: serde_json::Value,
    pub status: String,
    pub error_msg: Option<String>,
}
crate::register_ts_dto!(ChannelRowDto);

// ── ActiveChannel DTO (GET /api/channels/active) ─────────────────────────────
// Mirrors ConnectedChannel in gateway/state.rs (which cannot get ts-gen attrs
// because state.rs has crate-internal imports). Must stay in sync with ConnectedChannel.

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct ActiveChannelDto {
    pub agent_name: String,
    pub channel_id: Option<String>,
    pub channel_type: String,
    pub display_name: String,
    pub adapter_version: String,
    pub connected_at: String,
    pub last_activity: String,
}
crate::register_ts_dto!(ActiveChannelDto);
