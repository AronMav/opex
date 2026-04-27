// Leaf file — zero crate::* imports. Included via include!() in cron.rs
// and re-exported in lib.rs dto_export for ts-gen.
use serde::Serialize;

// ── CronJob DTO (GET /api/cron) ──────────────────────────────────────────────
// Derived from scheduler::ScheduledJob with renamed fields + computed next_run.

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct CronJobDto {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub cron: String,
    pub timezone: String,
    pub task: String,
    pub enabled: bool,
    pub silent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    #[cfg_attr(feature = "ts-gen", ts(type = "{ channel: string; chat_id: number; channel_id?: string }"))]
    pub announce_to: Option<serde_json::Value>,
    pub jitter_secs: i32,
    pub run_once: bool,
    pub run_at: Option<String>,
    pub created_at: String,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    #[cfg_attr(feature = "ts-gen", ts(type = "{ allow: string[]; deny: string[] }"))]
    pub tool_policy: Option<serde_json::Value>,
}
crate::register_ts_dto!(CronJobDto);

// ── CronRun DTO (GET /api/cron/{id}/runs, GET /api/cron/runs) ────────────────

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct CronRunDto {
    pub id: String,
    pub job_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub job_name: Option<String>,
    pub agent_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    #[cfg_attr(feature = "ts-gen", ts(type = "\"running\" | \"success\" | \"error\""))]
    pub status: String,
    pub error: Option<String>,
    pub response_preview: Option<String>,
}
crate::register_ts_dto!(CronRunDto);
