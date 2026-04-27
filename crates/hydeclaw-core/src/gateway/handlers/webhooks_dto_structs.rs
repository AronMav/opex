use serde::Serialize;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct WebhookEntryDto {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub secret: Option<String>,
    pub prompt_prefix: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub last_triggered_at: Option<String>,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub trigger_count: i32,
    #[cfg_attr(feature = "ts-gen", ts(type = "\"generic\" | \"github\""))]
    pub webhook_type: String,
    pub event_filter: Option<Vec<String>>,
}
crate::register_ts_dto!(WebhookEntryDto);
