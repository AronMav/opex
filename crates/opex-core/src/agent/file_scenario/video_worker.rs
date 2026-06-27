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
    /// `(filename, image_b64)` pairs — one per frame — to upload via `save_media`.
    pub media: Vec<(String, String)>,
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
///
/// `digest_mode` selects the digest strategy: `Some("mapreduce")` runs the
/// per-segment map-reduce path (topical segmentation → per-segment digest →
/// merge → final summary); anything else (`Some("single")` / `None`) runs the
/// legacy single-pass digest. The map-reduce path degrades gracefully — any
/// segmentation/JSON failure logs a warning and falls back to uniform segments
/// (and, if even that yields nothing, to single-pass) rather than failing the job.
pub async fn process_one(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    provider: &dyn LlmProvider,
    job: &VideoJob,
    digest_mode: Option<&str>,
    on_phase: &(dyn Fn(&str, &str) + Sync),
) -> anyhow::Result<NoteResult> {
    use crate::agent::file_scenario::video_summary::{
        build_summary_messages, extract_summary, slug, RawMaterial,
    };

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

    let frame_names: Vec<String> = (0..raw.frames.len())
        .map(|i| format!("frame-{:02}.jpg", i + 1))
        .collect();

    let media: Vec<(String, String)> = frame_names
        .iter()
        .cloned()
        .zip(raw.frames.iter().map(|f| f.image_b64.clone()))
        .collect();

    // ── 3. Build LLM body ─────────────────────────────────────────────────────
    let title_for_note = if title.is_empty() {
        note_slug.clone()
    } else {
        title
    };

    let note = if matches!(digest_mode, Some("mapreduce")) {
        build_note_mapreduce(provider, &raw, &title_for_note, &frame_names, on_phase).await?
    } else {
        on_phase("digest", "📝 Составляю конспект…");
        let messages = build_summary_messages(&raw, &frame_names);
        let llm_body = provider.chat(&messages, &[], digest_opts()).await?.content;
        crate::agent::file_scenario::video_summary::build_note(
            &raw,
            &title_for_note,
            &llm_body,
            &frame_names,
        )
    };

    // ── 4. Extract summary ───────────────────────────────────────────────────
    let summary = extract_summary(&note);

    Ok(NoteResult {
        slug: note_slug,
        note,
        summary,
        media,
    })
}

/// Shared `CallOptions` for every digest LLM call (no thinking, no CLAUDE.md).
fn digest_opts() -> CallOptions {
    CallOptions {
        thinking_level: 0,
        claude_md_content: None,
    }
}

/// Parse the segment-boundary JSON returned by the LLM into
/// `(start_frac, title)` pairs. Robust to surrounding prose / markdown fences:
/// we extract the outermost `[` … `]` and parse that. Returns `None` on any
/// failure so the caller can fall back to uniform segments.
fn parse_segment_boundaries(raw_json: &str) -> Option<Vec<(f64, String)>> {
    #[derive(serde::Deserialize)]
    struct Bound {
        start_frac: f64,
        #[serde(default)]
        title: String,
    }
    let start = raw_json.find('[')?;
    let end = raw_json.rfind(']')?;
    if end <= start {
        return None;
    }
    let slice = &raw_json[start..=end];
    let parsed: Vec<Bound> = serde_json::from_str(slice).ok()?;
    if parsed.is_empty() {
        return None;
    }
    let mut out: Vec<(f64, String)> = parsed
        .into_iter()
        .map(|b| {
            let title = if b.title.trim().is_empty() {
                "Сегмент".to_string()
            } else {
                b.title.trim().to_string()
            };
            (b.start_frac.clamp(0.0, 1.0), title)
        })
        .collect();
    // Sort ascending by start_frac and force the first segment to start at 0.0.
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(first) = out.first_mut() {
        first.0 = 0.0;
    }
    Some(out)
}

/// Uniform fallback boundaries: `n` evenly-spaced segments starting at 0.0.
fn uniform_boundaries(n: usize) -> Vec<(f64, String)> {
    let n = n.max(1);
    (0..n)
        .map(|i| (i as f64 / n as f64, format!("Часть {}", i + 1)))
        .collect()
}

/// Map-reduce digest: segment the transcript by topic, summarise each segment in
/// its own small-context LLM call, concatenate the segment notes in order, then
/// write one final `## Резюме`. Any segmentation/JSON error falls back to uniform
/// segments (and the per-segment map still runs), so this never hard-fails on a
/// flaky boundaries response.
async fn build_note_mapreduce(
    provider: &dyn LlmProvider,
    raw: &crate::agent::file_scenario::video_summary::RawMaterial,
    title: &str,
    frame_names: &[String],
    on_phase: &(dyn Fn(&str, &str) + Sync),
) -> anyhow::Result<String> {
    use crate::agent::file_scenario::video_summary::{
        build_note_from_parts, final_summary_messages, frames_for_segment,
        segment_boundaries_messages, segment_digest_messages, slice_segments,
    };

    // ── Step 1: topical segmentation ──────────────────────────────────────────
    on_phase("digest", "📝 Размечаю темы…");
    let boundaries = match provider
        .chat(&segment_boundaries_messages(raw), &[], digest_opts())
        .await
    {
        Ok(resp) => parse_segment_boundaries(&resp.content).unwrap_or_else(|| {
            tracing::warn!(
                "video_worker: segment-boundaries JSON unparseable — falling back to 6 uniform segments"
            );
            uniform_boundaries(6)
        }),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "video_worker: segment-boundaries LLM call failed — falling back to 6 uniform segments"
            );
            uniform_boundaries(6)
        }
    };

    // ── Step 2: slice transcript + assign frames per segment ──────────────────
    let segments = slice_segments(&raw.transcript, &boundaries);
    let total = segments.len();

    // ── Step 3 (map): per-segment digest ──────────────────────────────────────
    let mut segment_notes: Vec<String> = Vec::with_capacity(total);
    for (i, (_seg_title, seg_text)) in segments.iter().enumerate() {
        let seg_start = boundaries[i].0;
        let seg_end = boundaries.get(i + 1).map(|b| b.0).unwrap_or(1.0);
        let seg_frames = frames_for_segment(raw, frame_names, seg_start, seg_end, i + 1 == total);

        on_phase("digest", &format!("📝 Конспект сегмента {}/{}…", i + 1, total));
        match provider
            .chat(&segment_digest_messages(raw, seg_text, &seg_frames), &[], digest_opts())
            .await
        {
            Ok(resp) => segment_notes.push(resp.content.trim().to_string()),
            Err(e) => {
                // A single segment failing should not sink the whole job; keep a
                // visible placeholder so the transcript slice is still acknowledged.
                tracing::warn!(
                    error = %e,
                    segment = i + 1,
                    "video_worker: segment digest failed — inserting placeholder"
                );
                segment_notes.push(format!("### Часть {} (фрагмент не обработан)\n", i + 1));
            }
        }
    }

    let merged = segment_notes.join("\n\n");

    // ── Step 4 (reduce): final summary over the merged body ───────────────────
    on_phase("digest", "📝 Финальное резюме…");
    let summary = match provider
        .chat(&final_summary_messages(&merged), &[], digest_opts())
        .await
    {
        Ok(resp) => resp.content.trim().to_string(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "video_worker: final-summary LLM call failed — using empty summary"
            );
            String::new()
        }
    };

    Ok(build_note_from_parts(raw, title, &summary, &merged, frame_names))
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
    let digest_mode = state.config.config.video.digest_mode.clone();

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
                digest_mode.as_deref(),
                &on_phase,
            )
            .await
            {
                Ok(nr) => {
                    tracing::info!(job_id = %job.id, slug = %nr.slug, "video_worker: note assembled");
                    emit_video_progress(&ui_tx, job.session_id, "saving", "💾 Сохраняю в Obsidian…");

                    // Free folder name — collision avoidance
                    let mut folder = format!("Видео/{}", nr.slug);
                    for suffix in 2..=20 {
                        let exists = mcp
                            .call_tool(
                                "mcp-obsidian",
                                "note_exists",
                                &serde_json::json!({
                                    "folder": folder,
                                    "filename": "конспект.md"
                                }),
                            )
                            .await
                            .map(|s| s.trim() == "true")
                            .unwrap_or(false);
                        if !exists {
                            break;
                        }
                        folder = format!("Видео/{}-{}", nr.slug, suffix);
                    }

                    // Save media frames into Видео/<slug>/images/
                    let mut ok = true;
                    for (name, b64) in &nr.media {
                        if let Err(e) = mcp
                            .call_tool(
                                "mcp-obsidian",
                                "save_media",
                                &serde_json::json!({ "filename": name, "content_b64": b64, "folder": format!("{folder}/images") }),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, frame = %name, "video_worker: save_media failed");
                            ok = false;
                            break;
                        }
                    }

                    if !ok {
                        let _ = opex_db::video_jobs::mark_video_job_failed(
                            &db,
                            job.id,
                            "save_media failed",
                        )
                        .await;
                        deliver(&db, &ui_tx, &job, "Не удалось сохранить кадры конспекта").await;
                        emit_video_progress(&ui_tx, job.session_id, "failed", "");
                        continue;
                    }

                    // Create the note
                    if let Err(e) = mcp
                        .call_tool(
                            "mcp-obsidian",
                            "create_note",
                            &serde_json::json!({
                                "folder": folder,
                                "filename": "конспект.md",
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

                    let path = format!("{folder}/конспект.md");
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
            None,
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
        assert!(!note.media.is_empty(), "media collected for MCP save");
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
            None,
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
            None,
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
            None,
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
        let _ = process_one(&http, &server.uri(), "127.0.0.1:18789", &provider, &job, None, &on_phase)
            .await
            .expect("ok");

        assert_eq!(*phases.lock().unwrap(), vec!["fetch".to_string(), "digest".to_string()]);
    }

    // ── Map-reduce path ───────────────────────────────────────────────────────

    /// Sequenced fake provider: returns canned responses in call order, so we can
    /// drive the multi-call map-reduce pipeline (boundaries → N segments → summary).
    struct SeqLlm {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl SeqLlm {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for SeqLlm {
        async fn chat(
            &self,
            messages: &[Message],
            _tools: &[opex_types::ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
            // Record the user message of each call for assertions.
            if let Some(u) = messages.last() {
                self.calls.lock().unwrap().push(u.content.clone());
            }
            let content = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "## fallback".to_string());
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
        fn name(&self) -> &str { "seq" }
        fn current_model(&self) -> String { "seq".into() }
    }

    #[tokio::test]
    async fn process_one_mapreduce_segments_map_and_reduce() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "Урок", "duration": 100.0,
                "transcript": "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0123",
                "frames": [
                    {"timestamp": 10.0, "description": "к1", "image_b64": "/9j/A"},
                    {"timestamp": 80.0, "description": "к2", "image_b64": "/9j/B"}
                ],
                "degraded": {"stt": false, "vision": false}
            })))
            .mount(&server)
            .await;

        // Call order: boundaries (2 segments) → seg1 digest → seg2 digest → final summary.
        let provider = SeqLlm::new(vec![
            // boundaries with surrounding prose to exercise the [..] extractor
            "Вот сегменты: [{\"start_frac\":0.0,\"title\":\"Вступление\"},{\"start_frac\":0.5,\"title\":\"Практика\"}]",
            "### Вступление (0:00)\n- деталь один\n![](images/frame-01.jpg)",
            "### Практика (0:50)\n- деталь два\n![](images/frame-02.jpg)",
            "Это видео про вступление и практику.",
        ]);

        let job = VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "file".into(),
            source_ref: "http://localhost/api/uploads/x?sig=1".into(),
            source_title: Some("Урок".into()),
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let http = reqwest::Client::new();
        let nr = process_one(
            &http,
            &server.uri(),
            "0.0.0.0:18789",
            &provider,
            &job,
            Some("mapreduce"),
            &|_: &str, _: &str| {},
        )
        .await
        .unwrap();

        // 4 LLM calls fired: boundaries + 2 segments + final summary.
        assert_eq!(provider.calls.lock().unwrap().len(), 4, "boundaries + 2 maps + reduce");
        // Final summary came from the reduce call.
        assert!(nr.summary.contains("вступление и практику"), "summary: {}", nr.summary);
        // Both segment digests merged into ## Конспект, in order.
        let i1 = nr.note.find("деталь один").expect("seg1 present");
        let i2 = nr.note.find("деталь два").expect("seg2 present");
        assert!(i1 < i2, "segments concatenated in order");
        assert!(nr.note.contains("## Резюме"), "summary section");
        assert!(nr.note.contains("## Конспект"), "digest section");
        assert!(nr.note.contains("> [!note]- Полный транскрипт"), "transcript collapsed in");
        assert!(!nr.media.is_empty(), "media collected for MCP");
    }

    #[tokio::test]
    async fn process_one_mapreduce_falls_back_on_bad_boundaries_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "Урок", "duration": 60.0,
                "transcript": "речь без кадров целиком для нарезки",
                "frames": [],
                "degraded": {"stt": false, "vision": false}
            })))
            .mount(&server)
            .await;

        // First response is junk (no JSON array) → uniform fallback to 6 segments,
        // so the provider must then serve 6 segment digests + 1 final summary.
        let mut responses = vec!["извини, не смог разметить"];
        for i in 0..6 {
            responses.push(if i == 0 { "### Часть 1\n- a" } else { "### Часть\n- b" });
        }
        responses.push("краткое резюме целиком");
        let provider = SeqLlm::new(responses);

        let job = VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "file".into(),
            source_ref: "http://localhost/api/uploads/x?sig=1".into(),
            source_title: Some("Урок".into()),
            status: "processing".into(),
            summary: None,
            error: None,
            attempts: 1,
        };

        let http = reqwest::Client::new();
        let nr = process_one(
            &http,
            &server.uri(),
            "0.0.0.0:18789",
            &provider,
            &job,
            Some("mapreduce"),
            &|_: &str, _: &str| {},
        )
        .await
        .expect("mapreduce must not hard-fail on bad boundaries JSON");

        // boundaries(1) + 6 uniform segments + final summary(1) = 8 calls.
        assert_eq!(provider.calls.lock().unwrap().len(), 8, "fell back to 6 uniform segments");
        assert!(nr.summary.contains("резюме целиком"), "summary: {}", nr.summary);
        assert!(nr.note.contains("## Конспект"));
    }

    #[test]
    fn parse_segment_boundaries_extracts_array_and_sorts() {
        let raw = "prefix [{\"start_frac\":0.6,\"title\":\"B\"},{\"start_frac\":0.0,\"title\":\"A\"}] suffix";
        let b = parse_segment_boundaries(raw).expect("parses");
        assert_eq!(b.len(), 2);
        // Sorted ascending, first forced to 0.0.
        assert_eq!(b[0].0, 0.0);
        assert_eq!(b[0].1, "A");
        assert_eq!(b[1].1, "B");
        assert!((b[1].0 - 0.6).abs() < 1e-9);
    }

    #[test]
    fn parse_segment_boundaries_rejects_non_json() {
        assert!(parse_segment_boundaries("нет тут массива").is_none());
        assert!(parse_segment_boundaries("[]").is_none(), "empty array → None");
    }

    #[test]
    fn uniform_boundaries_evenly_spaced_starting_at_zero() {
        let b = uniform_boundaries(4);
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].0, 0.0);
        assert!((b[1].0 - 0.25).abs() < 1e-9);
        assert!((b[3].0 - 0.75).abs() < 1e-9);
    }
}
