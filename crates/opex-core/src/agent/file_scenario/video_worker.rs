//! In-core durable worker for video_jobs. Lives in opex-core (not memory-worker)
//! because it needs LLM providers, ui_event_tx and session delivery.

use std::sync::Arc;

use anyhow::Context as _;
use opex_db::video_jobs::VideoJob;
use tokio_util::sync::CancellationToken;

use crate::agent::file_scenario::video_summary::{build_summary_messages, RawMaterial};
use crate::agent::providers::{CallOptions, LlmProvider};
use crate::gateway::AppState;

// ── URL rewrite ───────────────────────────────────────────────────────────────

/// Convert a public upload URL to a localhost URL for toolgate download
/// (same pattern used by document extraction).
/// For `source_type = "url"` jobs the ref is an external page link and passes
/// through unchanged.
fn source_payload(job: &VideoJob, gateway_listen: &str) -> serde_json::Value {
    if job.source_type == "url" {
        serde_json::json!({ "page_url": job.source_ref })
    } else {
        let local = crate::agent::url_tools::uploads_local_url(&job.source_ref, gateway_listen);
        serde_json::json!({ "video_url": local })
    }
}

// ── Core logic (unit-testable, no DB) ────────────────────────────────────────

/// Call toolgate `/summarize-video`, build the digest, run through the LLM,
/// and return the summary text. Delivery is intentionally separate so this
/// function is independently unit-testable with a mock toolgate and a fake
/// provider.
pub async fn process_one(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    provider: &dyn LlmProvider,
    job: &VideoJob,
) -> anyhow::Result<String> {
    // ── 1. Call toolgate ──────────────────────────────────────────────────────
    let url = format!("{}/summarize-video", toolgate_url.trim_end_matches('/'));
    let mut body = source_payload(job, gateway_listen);
    body["language"] = serde_json::json!("ru");

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;

    if !resp.status().is_success() {
        anyhow::bail!("toolgate /summarize-video HTTP {}", resp.status().as_u16());
    }

    let raw: RawMaterial = resp
        .json()
        .await
        .context("deserialise toolgate /summarize-video response")?;

    // ── 2. Build digest messages and call provider ────────────────────────────
    let messages = build_summary_messages(&raw);
    let opts = CallOptions {
        thinking_level: 0,
        claude_md_content: None,
    };
    let llm_resp = provider.chat(&messages, &[], opts).await?;

    Ok(llm_resp.content)
}

// ── Delivery (web-only, v1) ───────────────────────────────────────────────────

/// Insert the summary as an assistant message into the originating session and
/// push a live `ui_event` so open browser tabs render it without a reload.
///
/// v1 is web-only: `channel_id` stays NULL; the Telegram notify path is
/// deferred to a later release.
async fn deliver(
    db: &sqlx::PgPool,
    ui_tx: &tokio::sync::broadcast::Sender<String>,
    job: &VideoJob,
    text: &str,
) {
    // Insert straight into messages (session_id is known; is_mirror=true keeps
    // it off the normal branching / parent_message_id chain).
    if let Err(e) = sqlx::query(
        "INSERT INTO messages (session_id, agent_id, role, content, is_mirror) \
         VALUES ($1, $2, 'assistant', $3, true)",
    )
    .bind(job.session_id)
    .bind(&job.agent_name)
    .bind(text)
    .execute(db)
    .await
    {
        // CRITICAL: job already marked done but session delivery failed — summary
        // is stored in video_jobs.summary but not visible in the UI session.
        // Operator must manually re-inject or the job must be re-delivered.
        tracing::error!(
            error = %e,
            job_id = %job.id,
            session_id = %job.session_id,
            "video summary delivery failed after job marked done — summary lost from session"
        );
    }

    // Live push — open clients pick this up via their WebSocket event feed.
    let ev = serde_json::json!({
        "type": "video_summary_ready",
        "session_id": job.session_id.to_string(),
        "text": text,
    });
    let _ = ui_tx.send(ev.to_string());
}

// ── Poll loop ─────────────────────────────────────────────────────────────────

/// Spawn the background video-job worker (concurrency = 1 in v1).
///
/// The worker:
/// 1. Claims one job at a time from the durable `video_jobs` queue.
/// 2. Resolves the agent's registered engine to obtain its LLM provider.
/// 3. Calls `process_one` (toolgate → digest → LLM).
/// 4. Marks the job done/failed and delivers the summary to the web session.
pub fn spawn_video_worker(state: &AppState, shutdown: CancellationToken) {
    let db = state.infra.db.clone();
    let agents = state.agents.clone();
    let ui_tx = state.channels.ui_event_tx.clone();

    // Resolve toolgate URL at spawn time (config is stable after startup).
    let toolgate_url = state
        .config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());

    let gateway_listen = state.config.config.gateway.listen.clone();

    // The worker owns its own reqwest::Client — pooled, reused across polls.
    let http = reqwest::Client::new();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }

            // ── Claim ─────────────────────────────────────────────────────────
            let job = match opex_db::video_jobs::claim_next_video_job(&db).await {
                Ok(Some(j)) => j,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "video_worker: claim failed");
                    continue;
                }
            };

            tracing::info!(
                job_id = %job.id,
                agent = %job.agent_name,
                source_type = %job.source_type,
                "video_worker: processing job"
            );

            // ── Resolve provider ──────────────────────────────────────────────
            let engine: Arc<crate::agent::engine::AgentEngine> =
                match agents.get_engine(&job.agent_name).await {
                    Some(e) => e,
                    None => {
                        tracing::warn!(
                            job_id = %job.id,
                            agent = %job.agent_name,
                            "video_worker: agent engine not found — marking failed"
                        );
                        let _ = opex_db::video_jobs::mark_video_job_failed(
                            &db,
                            job.id,
                            "agent engine not found",
                        )
                        .await;
                        continue;
                    }
                };

            let provider: Arc<dyn LlmProvider> = engine.provider_arc();

            // ── Process ───────────────────────────────────────────────────────
            match process_one(&http, &toolgate_url, &gateway_listen, provider.as_ref(), &job).await {
                Ok(summary) => {
                    tracing::info!(job_id = %job.id, "video_worker: job succeeded");
                    let _ = opex_db::video_jobs::mark_video_job_done(&db, job.id, &summary).await;
                    deliver(&db, &ui_tx, &job, &summary).await;
                }
                Err(e) => {
                    let msg = format!("Не удалось обработать видео: {e}");
                    tracing::warn!(job_id = %job.id, error = %e, "video_worker: job failed");
                    let _ =
                        opex_db::video_jobs::mark_video_job_failed(&db, job.id, &e.to_string())
                            .await;
                    deliver(&db, &ui_tx, &job, &msg).await;
                }
            }
        }
        tracing::info!("video_worker: stopped");
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::Message;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Minimal fake LLM provider returning a fixed summary ──────────────────

    struct FakeLlm;

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for FakeLlm {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[opex_types::ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
            Ok(opex_types::LlmResponse {
                content: "СВОДКА: тест ок".to_string(),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
                model: None,
                provider: None,
                fallback_notice: None,
                tools_used: vec![],
                iterations: 1,
                thinking_blocks: vec![],
            })
        }

        fn name(&self) -> &str {
            "fake"
        }

        fn current_model(&self) -> String {
            "fake".into()
        }
    }

    // ── Test: process_one calls toolgate and returns LLM digest ─────────────

    #[tokio::test]
    async fn process_one_calls_toolgate_and_builds_digest() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "duration": 60.0,
                "transcript": "речь из видео",
                "frames": [{"timestamp": 5.0, "description": "слайд"}],
                "degraded": {"stt": false, "vision": false}
            })))
            .mount(&server)
            .await;

        let job = VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "file".into(),
            source_ref: "http://localhost/api/uploads/x?sig=1".into(),
            source_title: None,
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let client = reqwest::Client::new();
        let provider = FakeLlm;

        let summary =
            process_one(&client, &server.uri(), "0.0.0.0:18789", &provider, &job)
                .await
                .unwrap();

        assert!(summary.contains("СВОДКА"), "digest returned: {summary}");
    }

    // ── Test: process_one fails fast when toolgate returns 500 ───────────────

    #[tokio::test]
    async fn process_one_fails_on_toolgate_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let job = VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "file".into(),
            source_ref: "http://localhost/api/uploads/y?sig=2".into(),
            source_title: None,
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let client = reqwest::Client::new();
        let provider = FakeLlm;
        let result =
            process_one(&client, &server.uri(), "0.0.0.0:18789", &provider, &job).await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("500"), "expected HTTP 500 in error: {msg}");
    }

    // ── Test: url-type jobs pass source_ref unchanged ────────────────────────

    #[tokio::test]
    async fn process_one_url_source_passes_page_url() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "duration": 120.0,
                "transcript": "youtube видео",
                "frames": [],
                "degraded": {"stt": false, "vision": true}
            })))
            .mount(&server)
            .await;

        let job = VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "url".into(),
            source_ref: "https://youtube.com/watch?v=abc".into(),
            source_title: None,
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let client = reqwest::Client::new();
        let provider = FakeLlm;
        let summary =
            process_one(&client, &server.uri(), "0.0.0.0:18789", &provider, &job)
                .await
                .unwrap();

        assert!(summary.contains("СВОДКА"), "digest returned: {summary}");
    }
}
