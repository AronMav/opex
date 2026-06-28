//! In-core durable worker for video_jobs. Lives in opex-core (not memory-worker)
//! because it needs LLM providers, ui_event_tx and session delivery.

use std::sync::Arc;

use anyhow::Context as _;
use opex_db::video_jobs::VideoJob;
use tokio_util::sync::CancellationToken;

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

// ── NoteResult ────────────────────────────────────────────────────────────────

/// Everything produced by `process_one`; consumed by the worker loop for MCP
/// writes and session delivery.
#[derive(Debug)]
pub struct NoteResult {
    pub slug: String,
    pub note: String,
    pub summary: String,
}

// ── Core logic (unit-testable, no DB) ────────────────────────────────────────

/// Call toolgate `/summarize-video`, assemble the full Obsidian note, run
/// through the LLM, and return a `NoteResult`. Delivery and MCP writes are
/// intentionally separate so this function is independently unit-testable with
/// a mock toolgate and a fake provider.
///
/// `on_phase` is called at two points: `"fetch"` (before the toolgate HTTP
/// call) and `"digest"` (before the LLM call). It is a best-effort hook — the
/// caller may pass a no-op. The callback must be `Sync` so the async fn can
/// hold it across await points.
pub async fn process_one(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    provider: &dyn LlmProvider,
    job: &VideoJob,
    on_phase: &(dyn Fn(&str, &str) + Sync),
) -> anyhow::Result<NoteResult> {
    use crate::agent::file_scenario::video_summary::{
        build_chunk_messages, build_note, build_reduce_messages, build_summary_messages,
        extract_summary, should_chunk, slug, split_transcript_by_time, RawMaterial,
        DIGEST_CHUNK_MINUTES,
    };
    use futures_util::stream::{StreamExt, TryStreamExt};

    on_phase("fetch", "🎬 Скачиваю и расшифровываю видео…");

    // ── 1. Call toolgate ──────────────────────────────────────────────────────
    let url = format!("{}/summarize-video", toolgate_url.trim_end_matches('/'));
    let mut body = source_payload(job, gateway_listen);
    body["language"] = serde_json::json!("ru");
    if let Some(t) = &job.source_title {
        body["title"] = serde_json::json!(t);
    }

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

    // ── 2. Derive slug + frame names ──────────────────────────────────────────
    let title = job
        .source_title
        .clone()
        .or_else(|| raw.title.clone())
        .unwrap_or_default();
    let id8 = {
        let full = job.id.simple().to_string();
        full[..8].to_string()
    };
    let note_slug = slug(&title, &id8);

    // ── 3. Build LLM body ─────────────────────────────────────────────────────
    // Screenshots are no longer embedded — frame DESCRIPTIONS still feed the
    // digest as on-screen context, but no images are uploaded to the vault.
    //
    // Long videos (transcript > threshold) use a map-reduce digest: a detailed
    // partial conspect per time-window (run concurrently), then a merge call that
    // preserves detail. Short videos keep the single-pass path. See video_summary.
    const MAP_CONCURRENCY: usize = 4;
    let opts = || CallOptions {
        thinking_level: 0,
        claude_md_content: None,
    };

    let chunks = if should_chunk(&raw.transcript) {
        split_transcript_by_time(&raw.transcript, DIGEST_CHUNK_MINUTES)
    } else {
        Vec::new()
    };

    let llm_body = if chunks.len() >= 2 {
        let total = chunks.len();
        on_phase(
            "digest",
            &format!("📝 Длинное видео: конспектирую по частям ({total})…"),
        );
        // Map: one detailed partial conspect per window, run concurrently (ordered).
        let chunk_msgs: Vec<Vec<opex_types::Message>> = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| build_chunk_messages(c, i, total, &raw.frames))
            .collect();
        // Build the per-chunk futures in an explicit loop (a `.map` closure that
        // returns a future borrowing its argument trips HRTB inference here).
        let mut map_futs = Vec::with_capacity(total);
        for m in &chunk_msgs {
            map_futs.push(provider.chat(m, &[], opts()));
        }
        let responses = futures_util::stream::iter(map_futs)
            .buffered(MAP_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;
        let partials: Vec<String> = responses.into_iter().map(|r| r.content).collect();
        // Reduce: merge the partials into one coherent note (detail-preserving).
        on_phase("digest", "📝 Свожу части в единый конспект…");
        let reduce_msgs = build_reduce_messages(&partials);
        provider.chat(&reduce_msgs, &[], opts()).await?.content
    } else {
        on_phase("digest", "📝 Составляю конспект…");
        let messages = build_summary_messages(&raw);
        provider.chat(&messages, &[], opts()).await?.content
    };

    // ── 4. Build note + extract summary ──────────────────────────────────────
    let title_for_note = if title.is_empty() {
        note_slug.clone()
    } else {
        title
    };
    let note = build_note(&raw, &title_for_note, &llm_body);
    let summary = extract_summary(&note);

    Ok(NoteResult {
        slug: note_slug,
        note,
        summary,
    })
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

/// Best-effort broadcast of a video processing phase to open UI clients.
/// `text` is the status line for active phases; terminal phases pass `""`.
fn emit_video_progress(
    ui_tx: &tokio::sync::broadcast::Sender<String>,
    session_id: uuid::Uuid,
    phase: &str,
    text: &str,
) {
    let ev = serde_json::json!({
        "type": "video_progress",
        "session_id": session_id.to_string(),
        "phase": phase,
        "text": text,
    });
    let _ = ui_tx.send(ev.to_string());
}

// ── Digest-provider resolution ────────────────────────────────────────────────

/// Resolve the provider used for the LLM digest step.
///
/// If `[video] digest_provider` is configured, we look up the named provider
/// row in the DB and build an HTTP provider from it (CLI providers are not
/// supported here — they need a sandbox + agent context).  On any failure we
/// warn and fall back to the agent's own provider.  When `digest_provider` is
/// not set we return `None` and the caller uses the engine provider as before.
async fn resolve_digest_provider(
    state: &AppState,
    provider_name: &str,
    model_override: Option<&str>,
) -> Option<Arc<dyn LlmProvider>> {
    use crate::agent::providers::{build_provider, ProviderOverrides};
    use crate::agent::providers::timeouts::ProviderOptions;
    use tokio_util::sync::CancellationToken;

    let row = match crate::db::providers::get_provider_by_name(&state.infra.db, provider_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            tracing::warn!(
                digest_provider = %provider_name,
                "video_worker: digest_provider not found in DB — falling back to agent provider"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                digest_provider = %provider_name,
                "video_worker: DB error resolving digest_provider — falling back to agent provider"
            );
            return None;
        }
    };

    if row.category != "text" && row.category != "llm" {
        tracing::warn!(
            digest_provider = %provider_name,
            category = %row.category,
            "video_worker: digest_provider is not a text/llm provider — falling back to agent provider"
        );
        return None;
    }

    if matches!(row.provider_type.as_str(), "claude-cli" | "gemini-cli" | "codex-cli") {
        tracing::warn!(
            digest_provider = %provider_name,
            provider_type = %row.provider_type,
            "video_worker: CLI providers are not supported as digest_provider — falling back to agent provider"
        );
        return None;
    }

    let opts: ProviderOptions = serde_json::from_value(row.options.clone()).unwrap_or_default();
    let timeouts = opts.timeouts;
    let overrides = ProviderOverrides {
        model: model_override.map(str::to_string),
        temperature: None,
        max_tokens: None,
        prompt_cache: None,
    };

    match build_provider(&row, state.auth.secrets.clone(), &timeouts, CancellationToken::new(), overrides) {
        Ok(provider) => {
            tracing::info!(
                digest_provider = %provider_name,
                model = ?model_override,
                "video_worker: using override digest provider"
            );
            Some(Arc::from(provider))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                digest_provider = %provider_name,
                "video_worker: failed to build digest_provider — falling back to agent provider"
            );
            None
        }
    }
}

// ── Poll loop ─────────────────────────────────────────────────────────────────

/// Spawn the background video-job worker (concurrency = 1 in v1).
///
/// The worker:
/// 1. Claims one job at a time from the durable `video_jobs` queue.
/// 2. Resolves the agent's registered engine to obtain its LLM provider and
///    MCP registry.
/// 3. Calls `process_one` (toolgate → digest → LLM → note assembly).
/// 4. Writes frames + note to the Obsidian vault via MCP, then marks the job
///    done/failed and delivers the summary + vault link to the web session.
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

    // Capture video config overrides at spawn time.
    let digest_provider_name = state.config.config.video.digest_provider.clone();
    let digest_model = state.config.config.video.digest_model.clone();

    // Clone the state for async use inside the spawned task.
    let state_for_worker = state.clone();

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

            // ── Resolve provider + MCP ────────────────────────────────────────
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

            // Resolve digest provider: use config override if set, else fall back to engine's own.
            let provider: Arc<dyn LlmProvider> = if let Some(ref name) = digest_provider_name {
                let override_p = resolve_digest_provider(
                    &state_for_worker,
                    name,
                    digest_model.as_deref(),
                )
                .await;
                override_p.unwrap_or_else(|| engine.provider_arc())
            } else {
                engine.provider_arc()
            };

            let mcp = match engine.mcp() {
                Some(m) => m.clone(),
                None => {
                    tracing::warn!(
                        job_id = %job.id,
                        "video_worker: MCP disabled — cannot save note"
                    );
                    let _ = opex_db::video_jobs::mark_video_job_failed(
                        &db,
                        job.id,
                        "MCP disabled — cannot save note",
                    )
                    .await;
                    deliver(
                        &db,
                        &ui_tx,
                        &job,
                        "Не удалось сохранить конспект: MCP не настроен",
                    )
                    .await;
                    emit_video_progress(&ui_tx, job.session_id, "failed", "");
                    continue;
                }
            };

            // ── Process ───────────────────────────────────────────────────────
            let pj_ui = ui_tx.clone();
            let pj_sid = job.session_id;
            let on_phase = move |phase: &str, text: &str| {
                emit_video_progress(&pj_ui, pj_sid, phase, text);
            };
            match process_one(
                &http,
                &toolgate_url,
                &gateway_listen,
                provider.as_ref(),
                &job,
                &on_phase,
            )
            .await
            {
                Ok(nr) => {
                    tracing::info!(job_id = %job.id, slug = %nr.slug, "video_worker: note assembled");
                    emit_video_progress(&ui_tx, job.session_id, "saving", "💾 Сохраняю в Obsidian…");

                    // Note goes directly into `Summary/` as `<slug>.md` — no
                    // per-note subfolder and no images (screenshots were dropped).
                    let folder = "Summary".to_string();
                    let mut filename = format!("{}.md", nr.slug);
                    for suffix in 2..=20 {
                        let exists = mcp
                            .call_tool(
                                "mcp-obsidian",
                                "note_exists",
                                &serde_json::json!({
                                    "folder": folder,
                                    "filename": filename
                                }),
                            )
                            .await
                            .map(|s| s.trim() == "true")
                            .unwrap_or(false);
                        if !exists {
                            break;
                        }
                        filename = format!("{}-{}.md", nr.slug, suffix);
                    }

                    // Create the note
                    if let Err(e) = mcp
                        .call_tool(
                            "mcp-obsidian",
                            "create_note",
                            &serde_json::json!({
                                "folder": folder,
                                "filename": filename,
                                "content": nr.note
                            }),
                        )
                        .await
                    {
                        tracing::warn!(job_id = %job.id, error = %e, "video_worker: create_note failed");
                        let _ = opex_db::video_jobs::mark_video_job_failed(
                            &db,
                            job.id,
                            &format!("create_note: {e}"),
                        )
                        .await;
                        deliver(
                            &db,
                            &ui_tx,
                            &job,
                            &format!("Не удалось сохранить конспект: {e}"),
                        )
                        .await;
                        emit_video_progress(&ui_tx, job.session_id, "failed", "");
                        continue;
                    }

                    // Commit vault — best-effort
                    let _ = mcp
                        .call_tool(
                            "mcp-obsidian",
                            "commit_vault",
                            &serde_json::json!({
                                "message": format!("видео-конспект: {}", nr.slug)
                            }),
                        )
                        .await;

                    let path = format!("{folder}/{filename}");
                    let chat = format!("{}\n\n📓 Конспект: {}", nr.summary, path);
                    let _ = opex_db::video_jobs::mark_video_job_done(&db, job.id, &nr.summary)
                        .await;
                    deliver(&db, &ui_tx, &job, &chat).await;
                    emit_video_progress(&ui_tx, job.session_id, "done", "");
                }
                Err(e) => {
                    let msg = format!("Не удалось обработать видео: {e}");
                    tracing::warn!(job_id = %job.id, error = %e, "video_worker: job failed");
                    let _ =
                        opex_db::video_jobs::mark_video_job_failed(&db, job.id, &e.to_string())
                            .await;
                    deliver(&db, &ui_tx, &job, &msg).await;
                    emit_video_progress(&ui_tx, job.session_id, "failed", "");
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

    struct FakeLlm {
        content: String,
    }

    impl FakeLlm {
        fn new(content: &str) -> Self {
            Self { content: content.to_string() }
        }
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for FakeLlm {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[opex_types::ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
            Ok(opex_types::LlmResponse {
                content: self.content.clone(),
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

    // ── Test: process_one assembles note from toolgate + LLM ─────────────────

    #[tokio::test]
    async fn process_one_builds_note_with_image_and_summary() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "Тест", "duration": 30.0, "transcript": "речь",
                "frames": [{"timestamp": 5.0, "description": "слайд", "image_b64": "/9j/AA=="}],
                "degraded": {"stt": false, "vision": false}
            })))
            .mount(&server)
            .await;

        let job = opex_db::video_jobs::VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "file".into(),
            source_ref: "http://localhost/api/uploads/x?sig=1".into(),
            source_title: Some("Тест".into()),
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let client = reqwest::Client::new();
        let provider = FakeLlm::new(
            "## Резюме\nкоротко\n\n## Конспект\n![](images/frame-01.jpg)",
        );

        let note = process_one(
            &client,
            &server.uri(),
            "0.0.0.0:18789",
            &provider,
            &job,
            &|_: &str, _: &str| {},
        )
        .await
        .unwrap();

        assert!(note.note.contains("title: Тест"), "frontmatter title");
        assert!(
            note.note.contains("> [!note]- Полный транскрипт"),
            "collapsed transcript"
        );
        assert!(note.summary.contains("коротко"), "summary extracted");
        // Screenshots removed: the LLM's image embed is stripped from the note.
        assert!(!note.note.contains("![](images/"), "image embed stripped from note");
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
        let provider = FakeLlm::new("## Резюме\nкоротко\n\n## Конспект\nтело");

        let nr = process_one(
            &client,
            &server.uri(),
            "0.0.0.0:18789",
            &provider,
            &job,
            &|_: &str, _: &str| {},
        )
        .await
        .unwrap();

        assert!(nr.summary.contains("коротко"), "digest returned: {}", nr.summary);
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
        let provider = FakeLlm::new("## Резюме\nкоротко\n\n## Конспект\nтело");
        let result = process_one(
            &client,
            &server.uri(),
            "0.0.0.0:18789",
            &provider,
            &job,
            &|_: &str, _: &str| {},
        )
        .await;

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
        let provider = FakeLlm::new("## Резюме\nкоротко\n\n## Конспект\nтело");
        let nr = process_one(
            &client,
            &server.uri(),
            "0.0.0.0:18789",
            &provider,
            &job,
            &|_: &str, _: &str| {},
        )
        .await
        .unwrap();

        assert!(nr.summary.contains("коротко"), "digest returned: {}", nr.summary);
    }

    // ── Test: process_one invokes on_phase in fetch then digest order ─────────

    #[tokio::test]
    async fn process_one_emits_fetch_then_digest() {
        use std::sync::{Arc, Mutex};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "Тест", "duration": 12.0, "transcript": "речь",
                "frames": [], "degraded": {"stt": false, "vision": false}
            })))
            .mount(&server)
            .await;

        let provider = FakeLlm::new("## Резюме\nкоротко\n\n## Конспект\nтело");
        let job = VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "url".into(),
            source_ref: "https://youtu.be/x".into(),
            source_title: None,
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let phases: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let p2 = phases.clone();
        let on_phase = move |phase: &str, _text: &str| {
            p2.lock().unwrap().push(phase.to_string());
        };

        let http = reqwest::Client::new();
        let _ = process_one(&http, &server.uri(), "127.0.0.1:18789", &provider, &job, &on_phase)
            .await
            .expect("ok");

        assert_eq!(*phases.lock().unwrap(), vec!["fetch".to_string(), "digest".to_string()]);
    }

    // ── Chunked map-reduce for long videos ───────────────────────────────────

    /// Counts calls and returns map- vs reduce-specific content (detected by the
    /// reduce system prompt) so we can assert the map-reduce shape.
    struct CountingLlm {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for CountingLlm {
        async fn chat(
            &self,
            messages: &[Message],
            _tools: &[opex_types::ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let is_reduce = messages
                .first()
                .map(|m| m.content.contains("Объедини их в ОДИН цельный конспект"))
                .unwrap_or(false);
            let content = if is_reduce {
                "## Резюме\nсводное резюме\n\n## Конспект\nсклеено из частей".to_string()
            } else {
                "### Раздел фрагмента\nдетали фрагмента".to_string()
            };
            Ok(opex_types::LlmResponse {
                content,
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
            "counting"
        }

        fn current_model(&self) -> String {
            "counting".into()
        }
    }

    fn mock_summarize(server: &MockServer, transcript: &str) -> impl std::future::Future<Output = ()> {
        let body = serde_json::json!({
            "duration": 12000.0,
            "transcript": transcript,
            "frames": [],
            "degraded": {"stt": false, "vision": false}
        });
        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
    }

    fn url_job() -> VideoJob {
        VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "url".into(),
            source_ref: "https://youtu.be/x".into(),
            source_title: None,
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        }
    }

    #[tokio::test]
    async fn process_one_long_video_uses_map_reduce() {
        let server = MockServer::start().await;
        // 199-min transcript -> 45-min windows at 0/50/100/150/199 -> 5 chunks.
        mock_summarize(
            &server,
            "[00:00] вступление\n[50:00] часть А\n[100:00] часть Б\n[150:00] часть В\n[199:00] финал",
        )
        .await;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = CountingLlm { calls: calls.clone() };
        let nr = process_one(
            &reqwest::Client::new(),
            &server.uri(),
            "127.0.0.1:18789",
            &provider,
            &url_job(),
            &|_: &str, _: &str| {},
        )
        .await
        .unwrap();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            6,
            "5 map calls + 1 reduce"
        );
        assert!(nr.summary.contains("сводное резюме"), "reduce summary used: {}", nr.summary);
        assert!(nr.note.contains("склеено из частей"), "reduce body in note");
    }

    #[tokio::test]
    async fn process_one_short_video_stays_single_pass() {
        let server = MockServer::start().await;
        mock_summarize(&server, "[00:10] короткая речь\n[09:00] конец").await;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = CountingLlm { calls: calls.clone() };
        let _ = process_one(
            &reqwest::Client::new(),
            &server.uri(),
            "127.0.0.1:18789",
            &provider,
            &url_job(),
            &|_: &str, _: &str| {},
        )
        .await
        .unwrap();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "short video = single-pass = 1 call"
        );
    }
}
