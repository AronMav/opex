//! In-core dispatch table for built-in deterministic FSE actions.
//!
//! Fail-closed (security-load-bearing): an `executor=tool` `action_ref` not
//! present in this table resolves to `None` and the caller emits
//! `ScenarioOutcome{status: Unsupported}`. It NEVER falls through to a YAML
//! tool or a generic executor. A future contributor must not add a generic
//! fallthrough arm.

use crate::agent::file_scenario::outcome::{status_from_http, ScenarioOutcome};
use crate::agent::url_tools::uploads_local_url;

/// Context the async `summarize_video` built-in needs to enqueue a durable job.
/// Other built-ins ignore it (they pass `None`).
pub struct EnqueueCtx<'a> {
    pub db: &'a sqlx::PgPool,
    pub session_id: uuid::Uuid,
    pub agent_name: &'a str,
    pub source_type: &'a str, // "file" | "url"
}

/// Everything one built-in handler needs. Borrowed (no ownership) — the
/// dispatcher is called synchronously pre-LLM. `timeout` is the core-enforced
/// per-execution ceiling that maps to `ScenarioStatus::Timeout`.
pub struct DispatchInput<'a> {
    pub action_ref: &'a str,
    pub attachment: &'a opex_types::MediaAttachment,
    pub toolgate_url: &'a str,
    pub gateway_listen: &'a str,
    pub language: &'a str,
    pub http_client: &'a reqwest::Client,
    pub timeout: std::time::Duration,
    pub enqueue: Option<EnqueueCtx<'a>>,
}

/// Resolve `action_ref` against the in-core table and run the matching built-in,
/// producing a deterministic [`ScenarioOutcome`]. Fail-closed: an unknown
/// `action_ref` returns `Unsupported` and NEVER touches toolgate.
pub async fn dispatch_action(input: DispatchInput<'_>) -> ScenarioOutcome {
    let action = match resolve(input.action_ref) {
        Some(a) => a,
        None => {
            return ScenarioOutcome::unsupported(format!(
                "action_ref '{}' is not a built-in deterministic action",
                input.action_ref
            ));
        }
    };

    match action {
        BuiltinAction::Save => run_save(&input),
        BuiltinAction::Transcribe => run_transcribe(&input).await,
        BuiltinAction::Describe => run_describe(&input).await,
        BuiltinAction::ExtractDocument => run_extract_document(&input).await,
        BuiltinAction::SummarizeVideo => run_summarize_video(&input).await,
    }
}

/// Rowless universal fallback: persist nothing new (the upload already exists),
/// just surface its signed URL as an artifact — never a bare in-prompt hint.
fn run_save(input: &DispatchInput<'_>) -> ScenarioOutcome {
    let name = input.attachment.file_name.as_deref().unwrap_or("file");
    ScenarioOutcome::save(
        format!("File '{name}' saved; no automatic processing was applied."),
        vec![input.attachment.url.clone()],
    )
}

/// `transcribe` built-in — downloads the audio via localhost then POSTs to
/// toolgate `/transcribe` (multipart). Mirrors the former inline auto-transcribe
/// call (removed in Phase 3); returns a deterministic envelope instead of
/// mutating prompt text.
async fn run_transcribe(input: &DispatchInput<'_>) -> ScenarioOutcome {
    let local_url = uploads_local_url(&input.attachment.url, input.gateway_listen);
    let bytes = match input.http_client.get(&local_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return ScenarioOutcome::failed(format!("read audio bytes: {e}")),
        },
        Ok(resp) => {
            return ScenarioOutcome::failed(format!("download audio: HTTP {}", resp.status().as_u16()))
        }
        Err(e) => return ScenarioOutcome::failed(format!("download audio: {e}")),
    };

    let url = format!("{}/transcribe", input.toolgate_url.trim_end_matches('/'));
    let filename = input.attachment.url.split('/').next_back().unwrap_or("voice.ogg");
    let part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename.to_string())
        .mime_str("audio/ogg")
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.to_vec()));
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("language", input.language.to_string());

    let req = input.http_client.post(&url).multipart(form);
    let req = crate::trace_propagation::inject_trace_context(req);

    match tokio::time::timeout(input.timeout, req.send()).await {
        Ok(Ok(resp)) => {
            let code = resp.status().as_u16();
            if !resp.status().is_success() {
                return ScenarioOutcome {
                    status: status_from_http(code),
                    summary_text: String::new(),
                    artifact_urls: Vec::new(),
                    reason: Some(format!("transcribe: HTTP {code}")),
                    video_accepted: false,
                };
            }
            match resp.json::<serde_json::Value>().await {
                Ok(data) => {
                    let transcript = data["text"].as_str().unwrap_or("").to_string();
                    if transcript.is_empty() {
                        ScenarioOutcome::failed("transcribe: empty transcript".into())
                    } else {
                        ScenarioOutcome::ok(
                            format!("[Voice message (transcribed): {transcript}]"),
                            vec![input.attachment.url.clone()],
                        )
                    }
                }
                Err(e) => ScenarioOutcome::failed(format!("transcribe: bad JSON: {e}")),
            }
        }
        Ok(Err(e)) => ScenarioOutcome::failed(format!("transcribe request: {e}")),
        Err(_) => ScenarioOutcome::timeout(),
    }
}

/// `describe` built-in — downloads the image via localhost then POSTs to
/// toolgate `/describe` (multipart). On ok: appends `<vision>…</vision>` and
/// keeps the URL in `artifact_urls` so the UI can reconstruct the image
/// FilePart from history (image-ok exception, §4.4).
async fn run_describe(input: &DispatchInput<'_>) -> ScenarioOutcome {
    let local_url = uploads_local_url(&input.attachment.url, input.gateway_listen);
    let bytes = match input.http_client.get(&local_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return ScenarioOutcome::failed(format!("read image bytes: {e}")),
        },
        Ok(resp) => return ScenarioOutcome::failed(format!("download image: HTTP {}", resp.status().as_u16())),
        Err(e) => return ScenarioOutcome::failed(format!("download image: {e}")),
    };

    let url = format!("{}/describe", input.toolgate_url.trim_end_matches('/'));
    let filename = input.attachment.url.split('/').next_back().unwrap_or("image.jpg").to_string();
    let mime = input.attachment.mime_type.as_deref().unwrap_or("image/jpeg").to_string();
    let part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename)
        .mime_str(&mime)
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.to_vec()));
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("language", input.language.to_string());

    let req = input.http_client.post(&url).multipart(form);
    let req = crate::trace_propagation::inject_trace_context(req);

    match tokio::time::timeout(input.timeout, req.send()).await {
        Ok(Ok(resp)) => {
            let code = resp.status().as_u16();
            if !resp.status().is_success() {
                return ScenarioOutcome {
                    status: status_from_http(code),
                    summary_text: String::new(),
                    artifact_urls: Vec::new(),
                    reason: Some(format!("describe: HTTP {code}")),
                    video_accepted: false,
                };
            }
            match resp.json::<serde_json::Value>().await {
                Ok(data) => {
                    let desc = data["description"].as_str().unwrap_or("").to_string();
                    if desc.is_empty() {
                        ScenarioOutcome::failed("describe: empty description".into())
                    } else {
                        ScenarioOutcome::ok(
                            format!("[User attached an image: {}]\n<vision>{desc}</vision>", input.attachment.url),
                            vec![input.attachment.url.clone()],
                        )
                    }
                }
                Err(e) => ScenarioOutcome::failed(format!("describe: bad JSON: {e}")),
            }
        }
        Ok(Err(e)) => ScenarioOutcome::failed(format!("describe request: {e}")),
        Err(_) => ScenarioOutcome::timeout(),
    }
}

/// `extract_document` built-in — POSTs the LOCALHOST-REWRITTEN upload URL to
/// toolgate `/extract-text-url`. The signed `/api/uploads` URL is unreachable
/// from inside the deployment, so toolgate must download from localhost (same
/// reason the enrichers rewrite). Toolgate itself does the download.
async fn run_extract_document(input: &DispatchInput<'_>) -> ScenarioOutcome {
    let document_url = uploads_local_url(&input.attachment.url, input.gateway_listen);
    let url = format!("{}/extract-text-url", input.toolgate_url.trim_end_matches('/'));
    let body = serde_json::json!({ "document_url": document_url, "max_chars": 8000 });

    let req = input.http_client.post(&url).json(&body);
    let req = crate::trace_propagation::inject_trace_context(req);

    match tokio::time::timeout(input.timeout, req.send()).await {
        Ok(Ok(resp)) => {
            let code = resp.status().as_u16();
            if !resp.status().is_success() {
                return ScenarioOutcome {
                    status: status_from_http(code),
                    summary_text: String::new(),
                    artifact_urls: Vec::new(),
                    reason: Some(format!("extract_document: HTTP {code}")),
                    video_accepted: false,
                };
            }
            match resp.json::<serde_json::Value>().await {
                Ok(data) => {
                    let text = data["text"].as_str().unwrap_or("").to_string();
                    if text.is_empty() {
                        ScenarioOutcome::failed("extract_document: empty extraction".into())
                    } else {
                        let name = input.attachment.file_name.as_deref().unwrap_or("document");
                        ScenarioOutcome::ok(
                            format!("[Document '{name}' contents]:\n{text}"),
                            vec![input.attachment.url.clone()],
                        )
                    }
                }
                Err(e) => ScenarioOutcome::failed(format!("extract_document: bad JSON: {e}")),
            }
        }
        Ok(Err(e)) => ScenarioOutcome::failed(format!("extract_document request: {e}")),
        Err(_) => ScenarioOutcome::timeout(),
    }
}

/// Async built-in: enqueue a durable video_jobs row and return an instant ack.
/// The heavy pipeline runs out-of-band in the in-core video worker.
///
/// Dedup: if an active (pending/processing) job for the same `source_ref` already
/// exists in this session within a 2-minute window, we skip enqueue and return a
/// duplicate-ack instead — preventing double-jobs from mobile client reloads.
async fn run_summarize_video(input: &DispatchInput<'_>) -> ScenarioOutcome {
    let ctx = match &input.enqueue {
        Some(c) => c,
        None => {
            return ScenarioOutcome::unsupported(
                "summarize_video requires enqueue context (session/agent)".into(),
            )
        }
    };
    let source_ref = &input.attachment.url;

    // Dedup check: return early if a live job for this URL already exists.
    match opex_db::video_jobs::find_recent_active_video_job(ctx.db, ctx.session_id, source_ref)
        .await
    {
        Ok(Some(_)) => {
            // A live job already exists for this URL — short-circuit the agent
            // loop just like a fresh enqueue (the worker will deliver the summary).
            return ScenarioOutcome::video_accepted(
                "🎬 это видео уже в обработке — пришлю конспект, когда будет готов.".into(),
                vec![source_ref.clone()],
            );
        }
        Ok(None) => {}
        Err(e) => {
            // Non-fatal: log and fall through to enqueue normally rather than blocking.
            tracing::warn!(error = %e, "dedup check for video job failed; proceeding with enqueue");
        }
    }

    match opex_db::video_jobs::enqueue_video_job(
        ctx.db,
        ctx.session_id,
        ctx.agent_name,
        ctx.source_type,
        source_ref,
        input.attachment.file_name.as_deref(),
    )
    .await
    {
        Ok(_id) => ScenarioOutcome::video_accepted(
            "🎬 видео принято, готовлю сводку — пришлю, когда будет готова.".into(),
            vec![source_ref.clone()],
        ),
        Err(e) => ScenarioOutcome::failed(format!("could not enqueue video job: {e}")),
    }
}

/// The built-in deterministic action names that the dispatch table resolves.
/// 1:1 with [`crate::agent::file_scenario::outcome::FSE_DEFAULT_ALLOWLIST`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAction {
    Transcribe,
    Describe,
    ExtractDocument,
    Save,
    SummarizeVideo,
}

/// Fail-closed resolution of an `action_ref` to a built-in. Returns `None` for
/// anything outside the closed set — the caller turns `None` into
/// `ScenarioOutcome::unsupported(...)`. NO generic fallthrough.
pub fn resolve(action_ref: &str) -> Option<BuiltinAction> {
    match action_ref {
        "transcribe" => Some(BuiltinAction::Transcribe),
        "describe" => Some(BuiltinAction::Describe),
        "extract_document" => Some(BuiltinAction::ExtractDocument),
        "save" => Some(BuiltinAction::Save),
        "summarize_video" => Some(BuiltinAction::SummarizeVideo),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::file_scenario::outcome::ScenarioStatus;
    use opex_types::{MediaAttachment, MediaType};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn att(url: &str, mt: MediaType) -> MediaAttachment {
        MediaAttachment { url: url.into(), media_type: mt, file_name: None, mime_type: None, file_size: None }
    }

    fn input<'a>(
        action_ref: &'a str,
        attachment: &'a MediaAttachment,
        toolgate_url: &'a str,
        client: &'a reqwest::Client,
    ) -> DispatchInput<'a> {
        DispatchInput {
            action_ref,
            attachment,
            toolgate_url,
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: client,
            timeout: Duration::from_secs(10),
            enqueue: None,
        }
    }

    #[tokio::test]
    async fn unknown_action_is_unsupported_never_falls_through() {
        let client = reqwest::Client::new();
        let a = att("https://pub/api/uploads/1?sig=x", MediaType::Image);
        // toolgate_url points nowhere — proof it is never called for unknown action.
        let out = dispatch_action(input("code_exec", &a, "http://127.0.0.1:1", &client)).await;
        assert_eq!(out.status, ScenarioStatus::Unsupported);
        assert!(out.reason.unwrap().contains("code_exec"));
    }

    #[tokio::test]
    async fn save_produces_ok_with_artifact_url() {
        let client = reqwest::Client::new();
        let a = att("https://pub/api/uploads/abc?sig=x", MediaType::Document);
        let out = dispatch_action(input("save", &a, "http://127.0.0.1:1", &client)).await;
        assert_eq!(out.status, ScenarioStatus::Ok);
        // The signed URL lives ONLY in artifact_urls, never in summary_text.
        assert_eq!(out.artifact_urls, vec!["https://pub/api/uploads/abc?sig=x".to_string()]);
        assert!(!out.summary_text.contains("/api/uploads/"), "URL must not survive in summary_text");
    }

    #[tokio::test]
    async fn transcribe_success_returns_ok_with_transcript() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/transcribe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "привет мир"})))
            .mount(&server).await;
        // also mock the localhost download the handler performs for bytes
        Mock::given(method("GET")).and(path("/api/uploads/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OggSfakeaudio".to_vec()))
            .mount(&server).await;

        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att(&format!("{server_uri}/api/uploads/v1?sig=x"), MediaType::Audio);
        let port = server_uri.rsplit(':').next().unwrap().to_string();
        let gl = format!("0.0.0.0:{port}");
        let mut inp = input("transcribe", &a, &server_uri, &client);
        // make uploads_local_url resolve to the mock server's port instead of 18789
        inp.gateway_listen = &gl;
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::Ok);
        assert!(out.summary_text.contains("привет мир"), "summary must carry transcript: {}", out.summary_text);
    }

    #[tokio::test]
    async fn transcribe_non_2xx_is_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/uploads/v2"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OggSfake".to_vec()))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/transcribe"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad"))
            .mount(&server).await;
        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att(&format!("{server_uri}/api/uploads/v2?sig=x"), MediaType::Audio);
        let port = server_uri.rsplit(':').next().unwrap().to_string();
        let gl = format!("0.0.0.0:{port}");
        let mut inp = input("transcribe", &a, &server_uri, &client);
        inp.gateway_listen = &gl;
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::Failed);
    }

    #[test]
    fn resolve_known_builtins() {
        assert_eq!(resolve("transcribe"), Some(BuiltinAction::Transcribe));
        assert_eq!(resolve("describe"), Some(BuiltinAction::Describe));
        assert_eq!(resolve("extract_document"), Some(BuiltinAction::ExtractDocument));
        assert_eq!(resolve("save"), Some(BuiltinAction::Save));
    }

    #[test]
    fn resolve_unknown_is_none_fail_closed() {
        // A stray / forged allowlist member or binding row must be inert.
        assert_eq!(resolve("code_exec"), None);
        assert_eq!(resolve("analyze_image"), None); // YAML tool name, not an action name
        assert_eq!(resolve(""), None);
        assert_eq!(resolve("Transcribe"), None); // case-sensitive
    }

    #[tokio::test]
    async fn transcribe_timeout_returns_timeout_status() {
        let server = MockServer::start().await;
        // Download responds instantly — proves the timeout fires inside the toolgate POST, not here.
        Mock::given(method("GET")).and(path("/api/uploads/v3"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OggSfakeaudio".to_vec()))
            .mount(&server).await;
        // Toolgate POST is deliberately slow (30 s) — will be hit by the 100 ms ceiling.
        Mock::given(method("POST")).and(path("/transcribe"))
            .respond_with(ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_json(serde_json::json!({"text": "never reached"})))
            .mount(&server).await;

        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att(&format!("{server_uri}/api/uploads/v3?sig=x"), MediaType::Audio);
        let port = server_uri.rsplit(':').next().unwrap().to_string();
        let gl = format!("0.0.0.0:{port}");
        let mut inp = input("transcribe", &a, &server_uri, &client);
        inp.gateway_listen = &gl;
        inp.timeout = Duration::from_millis(100);
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::Timeout);
    }

    #[test]
    fn every_allowlist_member_resolves() {
        for name in crate::agent::file_scenario::outcome::FSE_DEFAULT_ALLOWLIST {
            assert!(resolve(name).is_some(), "allowlist member {name} must resolve to a builtin");
        }
    }

    #[test]
    fn resolve_summarize_video() {
        assert_eq!(resolve("summarize_video"), Some(BuiltinAction::SummarizeVideo));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn summarize_video_enqueues_and_acks(pool: sqlx::PgPool) {
        use opex_types::{MediaAttachment, MediaType};
        let sid = uuid::Uuid::new_v4();
        let att = MediaAttachment {
            url: "https://h/api/uploads/v1?sig=x".into(),
            media_type: MediaType::Video,
            file_name: Some("clip.mp4".into()),
            mime_type: Some("video/mp4".into()),
            file_size: None,
        };
        let client = reqwest::Client::new();
        let input = DispatchInput {
            action_ref: "summarize_video",
            attachment: &att,
            toolgate_url: "http://localhost:9011",
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: &client,
            timeout: std::time::Duration::from_secs(60),
            enqueue: Some(EnqueueCtx {
                db: &pool,
                session_id: sid,
                agent_name: "Atlas",
                source_type: "file",
            }),
        };
        let out = dispatch_action(input).await;
        assert_eq!(out.status, ScenarioStatus::Ok);
        assert!(out.summary_text.contains("видео"), "ack mentions video: {}", out.summary_text);
        assert!(
            out.video_accepted,
            "successful summarize_video enqueue must set video_accepted=true (drives LLM-loop short-circuit)"
        );

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE session_id=$1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1, "one video_jobs row enqueued");
    }

    /// Constructor invariants: only the async-video ack carries `video_accepted=true`.
    /// Synchronous scenario outcomes (ok/save/failed/transcribe-style) leave it false.
    #[test]
    fn video_accepted_flag_only_on_video_ack_constructor() {
        assert!(
            ScenarioOutcome::video_accepted("ack".into(), vec![]).video_accepted,
            "video_accepted constructor must set the flag"
        );
        assert!(!ScenarioOutcome::ok("transcript".into(), vec![]).video_accepted);
        assert!(!ScenarioOutcome::save("saved".into(), vec![]).video_accepted);
        assert!(!ScenarioOutcome::failed("boom".into()).video_accepted);
        assert!(!ScenarioOutcome::unsupported("nope".into()).video_accepted);
        assert!(!ScenarioOutcome::timeout().video_accepted);
    }

    /// Dedup ack (live job already exists) must ALSO short-circuit the agent loop:
    /// the worker already owns the summary, so the agent must not re-process.
    #[sqlx::test(migrations = "../../migrations")]
    async fn summarize_video_dedup_ack_sets_video_accepted(pool: sqlx::PgPool) {
        use opex_types::{MediaAttachment, MediaType};
        let sid = uuid::Uuid::new_v4();
        let att = MediaAttachment {
            url: "https://h/api/uploads/dedup-flag?sig=z".into(),
            media_type: MediaType::Video,
            file_name: Some("clip.mp4".into()),
            mime_type: Some("video/mp4".into()),
            file_size: None,
        };
        let client = reqwest::Client::new();
        let mk = || DispatchInput {
            action_ref: "summarize_video",
            attachment: &att,
            toolgate_url: "http://localhost:9011",
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: &client,
            timeout: std::time::Duration::from_secs(60),
            enqueue: Some(EnqueueCtx { db: &pool, session_id: sid, agent_name: "Atlas", source_type: "file" }),
        };
        let out1 = dispatch_action(mk()).await;
        assert!(out1.video_accepted, "first enqueue ack sets video_accepted");
        let out2 = dispatch_action(mk()).await;
        assert!(out2.summary_text.contains("уже в обработке"), "second call deduped");
        assert!(out2.video_accepted, "dedup ack must also short-circuit the agent loop");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn summarize_video_persists_source_title(pool: sqlx::PgPool) {
        use opex_types::{MediaAttachment, MediaType};
        let sid = uuid::Uuid::new_v4();
        let att = MediaAttachment {
            url: "https://h/api/uploads/v1?sig=x".into(),
            media_type: MediaType::Video,
            file_name: Some("Лекция.mp4".into()),
            mime_type: Some("video/mp4".into()),
            file_size: None,
        };
        let client = reqwest::Client::new();
        let input = DispatchInput {
            action_ref: "summarize_video",
            attachment: &att,
            toolgate_url: "http://localhost:9011",
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: &client,
            timeout: std::time::Duration::from_secs(60),
            enqueue: Some(EnqueueCtx { db: &pool, session_id: sid, agent_name: "Atlas", source_type: "file" }),
        };
        let _ = dispatch_action(input).await;
        let title: Option<String> = sqlx::query_scalar("SELECT source_title FROM video_jobs WHERE session_id=$1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(title.as_deref(), Some("Лекция.mp4"));
    }

    /// Second dispatch of the same attachment URL within 2 minutes must return the
    /// dedup ack WITHOUT inserting a second row in video_jobs.
    #[sqlx::test(migrations = "../../migrations")]
    async fn summarize_video_dedup_skips_second_enqueue(pool: sqlx::PgPool) {
        use opex_types::{MediaAttachment, MediaType};
        let sid = uuid::Uuid::new_v4();
        let att = MediaAttachment {
            url: "https://h/api/uploads/dup-clip?sig=y".into(),
            media_type: MediaType::Video,
            file_name: Some("dup.mp4".into()),
            mime_type: Some("video/mp4".into()),
            file_size: None,
        };
        let client = reqwest::Client::new();

        // First call: enqueues normally.
        let out1 = dispatch_action(DispatchInput {
            action_ref: "summarize_video",
            attachment: &att,
            toolgate_url: "http://localhost:9011",
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: &client,
            timeout: std::time::Duration::from_secs(60),
            enqueue: Some(EnqueueCtx { db: &pool, session_id: sid, agent_name: "Atlas", source_type: "file" }),
        })
        .await;
        assert_eq!(out1.status, ScenarioStatus::Ok);
        assert!(out1.summary_text.contains("принято"), "first ack: {}", out1.summary_text);

        // Second call (same session, same URL, job still pending): must dedup.
        let out2 = dispatch_action(DispatchInput {
            action_ref: "summarize_video",
            attachment: &att,
            toolgate_url: "http://localhost:9011",
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: &client,
            timeout: std::time::Duration::from_secs(60),
            enqueue: Some(EnqueueCtx { db: &pool, session_id: sid, agent_name: "Atlas", source_type: "file" }),
        })
        .await;
        assert_eq!(out2.status, ScenarioStatus::Ok);
        assert!(
            out2.summary_text.contains("уже в обработке"),
            "dedup ack expected, got: {}",
            out2.summary_text
        );

        // Exactly one row in video_jobs.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE session_id=$1")
                .bind(sid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "dedup must prevent second row from being inserted");
    }

    #[tokio::test]
    async fn describe_success_returns_ok_with_vision_and_keeps_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/uploads/i1"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake".to_vec()))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/describe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"description": "a red square"})))
            .mount(&server).await;
        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att(&format!("{}/api/uploads/i1?sig=x", server_uri), MediaType::Image);
        let port = server_uri.rsplit(':').next().unwrap().to_string();
        let mut inp = input("describe", &a, &server_uri, &client);
        let gl = format!("0.0.0.0:{port}");
        inp.gateway_listen = &gl;
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::Ok);
        assert!(out.summary_text.contains("a red square"));
        assert!(out.summary_text.contains("<vision>"), "describe must wrap in <vision> tags");
        // image-ok keeps the URL hint so the UI can reconstruct the FilePart
        assert_eq!(out.artifact_urls.first().map(|s| s.as_str()), Some(a.url.as_str()));
    }

    #[tokio::test]
    async fn describe_503_provider_inactive_is_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/uploads/i2"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake".to_vec()))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/describe"))
            .respond_with(ResponseTemplate::new(503).set_body_string("no vision provider"))
            .mount(&server).await;
        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att(&format!("{}/api/uploads/i2?sig=x", server_uri), MediaType::Image);
        let port = server_uri.rsplit(':').next().unwrap().to_string();
        let mut inp = input("describe", &a, &server_uri, &client);
        let gl = format!("0.0.0.0:{port}");
        inp.gateway_listen = &gl;
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::Failed);
    }

    #[tokio::test]
    async fn extract_document_localhost_rewrites_url_before_calling_toolgate() {
        let server = MockServer::start().await;
        // The handler must POST a document_url whose host is localhost:{port},
        // NOT the public host. We assert by matching the body.
        let port = server.uri().rsplit(':').next().unwrap().to_string();
        let expected_url = format!("http://localhost:{port}/api/uploads/d1?sig=x");
        Mock::given(method("POST")).and(path("/extract-text-url"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({ "document_url": expected_url })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "Hello PDF"})))
            .mount(&server).await;
        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att("https://public.example/api/uploads/d1?sig=x", MediaType::Document);
        let mut inp = input("extract_document", &a, &server_uri, &client);
        let gl = format!("0.0.0.0:{port}");
        inp.gateway_listen = &gl;
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::Ok, "reason: {:?}", out.reason);
        assert!(out.summary_text.contains("Hello PDF"));
    }

    #[tokio::test]
    async fn extract_document_413_is_too_large() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/extract-text-url"))
            .respond_with(ResponseTemplate::new(413).set_body_string("too big"))
            .mount(&server).await;
        let client = reqwest::Client::new();
        let server_uri = server.uri();
        let a = att("https://public.example/api/uploads/d2?sig=x", MediaType::Document);
        let port = server_uri.rsplit(':').next().unwrap().to_string();
        let gl = format!("0.0.0.0:{port}");
        let mut inp = input("extract_document", &a, &server_uri, &client);
        inp.gateway_listen = &gl;
        let out = dispatch_action(inp).await;
        assert_eq!(out.status, ScenarioStatus::TooLarge);
    }
}
