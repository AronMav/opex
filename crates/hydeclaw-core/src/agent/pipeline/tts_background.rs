//! Background TTS task — synthesise audio and deliver it outside the
//! SSE session deadline so a slow Qwen3-TTS on Pi can't time out the agent.

use std::sync::Arc;

use base64::Engine as _;
use tokio::sync::broadcast;
use tokio_util::task::TaskTracker;

use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};
use crate::agent::engine::SecretsEnvResolver;
use crate::tools::yaml_tools::{ChannelActionConfig, OAuthContext, YamlToolDef};

/// Owns everything a background TTS job needs — no borrows, safe to `tokio::spawn`.
pub struct BackgroundTtsTask {
    pub(crate) tool:           YamlToolDef,
    pub(crate) args:           serde_json::Value,
    pub(crate) ca:             ChannelActionConfig,
    pub(crate) http_client:    reqwest::Client,
    /// None only in tests where the YAML tool has no env-var templates.
    pub(crate) resolver:       Option<SecretsEnvResolver>,
    pub(crate) oauth_ctx:      Option<OAuthContext>,
    pub(crate) channel_router: Option<ChannelActionRouter>,
    pub(crate) ui_event_tx:    Option<broadcast::Sender<String>>,
    pub(crate) bg_tasks:       Arc<TaskTracker>,
    pub(crate) workspace_dir:  String,
    pub(crate) db:             sqlx::PgPool,
    pub(crate) upload_key:     [u8; 32],
    pub(crate) ttl_secs:       u64,
    pub(crate) tool_headers:   Vec<(String, String)>,
    pub(crate) context:        serde_json::Value,
    pub(crate) agent_name:     String,
}
