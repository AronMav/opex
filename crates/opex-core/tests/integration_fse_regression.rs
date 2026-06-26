//! FSE Phase 9 regression + retirement guards.
//!
//! Includes e2e wiremock-backed regression guards (Task 9.4) proving the FSE
//! deterministic dispatch still auto-transcribes audio and auto-describes images
//! via the seeded defaults (`audio/* → transcribe`, `image/* → describe`), even
//! after retiring the old inline enrichment arms.
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// No agent config or scaffold may still instruct an agent to honor the retired
/// image/audio/document arms. The video arm lives only in media-processing.md,
/// which is a skill, not an agent TOML.
#[test]
fn no_config_references_retired_media_skill() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for dir in ["crates/opex-core/scaffold", "crates/opex-core/tests/fixtures/agents"] {
        let p = root.join(dir);
        if !p.exists() {
            continue;
        }
        for entry in walk(&p) {
            let txt = std::fs::read_to_string(&entry).unwrap_or_default();
            // The skill name may appear; the retired *YAML tools* must not be
            // mandated by an agent config/scaffold.
            if txt.contains("transcribe_audio") || txt.contains("auto-describe") {
                offenders.push(entry.display().to_string());
            }
        }
    }
    assert!(offenders.is_empty(), "retired media arms referenced in: {offenders:?}");
}

fn walk(p: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ── Task 9.4: e2e regression guards (wiremock-backed) ───────────────────────
//
// These two tests prove the FSE built-in dispatch path still works end-to-end
// for the two default actions — no regression from retiring the old inline
// enrichment arms:
//
//   "transcribe" built-in → POST /transcribe (toolgate) → transcript in outcome
//   "describe"   built-in → POST /describe   (toolgate) → <vision> in outcome
//
// Entry point: `dispatch_action(DispatchInput { action_ref, ... })` — the
// exact dispatch-table function called by `run_builtin` inside the seam.
// The seam itself (dispatch_attachments) is exercised by `#[sqlx::test]`
// tests inline in `dispatch_seam.rs`; here we test the built-in handlers
// directly so the guards run without a live Postgres instance.
//
// Toolgate HTTP calls are intercepted by a wiremock server. The upload
// download (performed by `run_transcribe`/`run_describe` before calling
// toolgate) is also served by the SAME wiremock instance on a distinct
// `/api/uploads/*` path. `gateway_listen` is set to `"0.0.0.0:{port}"` so
// `uploads_local_url(att.url, gateway_listen)` rewrites to
// `http://localhost:{port}/api/uploads/...` — resolved by the mock server.
//
// After obtaining the outcome, `rewrite_enriched_text` is called with the
// synthetic enriched-text string (as `enrich_with_attachments` would produce),
// to verify the §4.4 URL-survival contract.

use opex_core::agent::file_scenario::dispatch::{dispatch_action, DispatchInput};
use opex_core::agent::file_scenario::rewrite::rewrite_enriched_text;
use opex_core::agent::file_scenario::ScenarioStatus;
use opex_types::{MediaAttachment, MediaType};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn audio_att(url: &str) -> MediaAttachment {
    MediaAttachment {
        url: url.into(),
        media_type: MediaType::Audio,
        file_name: Some("voice.ogg".into()),
        mime_type: Some("audio/ogg".into()),
        file_size: None,
    }
}

fn image_att(url: &str) -> MediaAttachment {
    MediaAttachment {
        url: url.into(),
        media_type: MediaType::Image,
        file_name: Some("photo.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        file_size: None,
    }
}

// ── regression_first_voice_transcribed ──────────────────────────────────────

/// Regression guard: `dispatch_action("transcribe", ...)` still calls toolgate
/// `POST /transcribe` and returns `Ok` with the transcript in `summary_text`.
///
/// Also verifies the §4.4 rewrite contract: after `rewrite_enriched_text` the
/// bare audio upload URL does NOT survive in the enriched text.
#[tokio::test]
async fn regression_first_voice_transcribed() {
    // 1. Stand up a wiremock server:
    //    - GET /api/uploads/v1 → OGG bytes (for the internal download in run_transcribe)
    //    - POST /transcribe    → {"text":"hello there"}
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/uploads/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"OggSfakeaudiobytes".to_vec()),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/transcribe"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"text": "hello there"})),
        )
        .mount(&server)
        .await;

    // 2. Build an attachment whose URL contains /api/uploads/ so that
    //    uploads_local_url rewrites to http://localhost:{port}/api/uploads/v1?sig=x.
    let port = server.address().port();
    let gateway_listen = format!("0.0.0.0:{port}");
    // The host in the attachment URL is the public host (ignored by uploads_local_url).
    let upload_url = "https://pub.example/api/uploads/v1?sig=x&exp=1".to_string();
    let att = audio_att(&upload_url);
    let client = reqwest::Client::new();

    // 3. Run dispatch_action("transcribe") — the exact call made by run_builtin
    //    in the seam for an audio/* attachment.
    let outcome = dispatch_action(DispatchInput {
        action_ref: "transcribe",
        attachment: &att,
        toolgate_url: &server.uri(),
        gateway_listen: &gateway_listen,
        language: "en",
        http_client: &client,
        timeout: std::time::Duration::from_secs(10),
        enqueue: None,
    })
    .await;

    // ── Assertions ──────────────────────────────────────────────────────────
    assert_eq!(
        outcome.status,
        ScenarioStatus::Ok,
        "transcribe must succeed: {:?}",
        outcome.reason
    );
    // The transcript lands in summary_text (surfaced to the LLM via EnrichResult).
    assert!(
        outcome.summary_text.contains("hello there"),
        "transcript must be in summary_text: {:?}",
        outcome.summary_text
    );
    // summary_text must NOT contain the bare signed URL (§4.4 / outcome contract).
    assert!(
        !outcome.summary_text.contains("/api/uploads/v1"),
        "bare upload URL must not appear in summary_text: {:?}",
        outcome.summary_text
    );

    // Also verify the rewrite contract: enrich_with_attachments produces
    // "[User sent a voice message: {url}]"; after rewrite on audio-ok the
    // hint is stripped (URL removed from the enriched text).
    let mut enriched = format!("[User sent a voice message: {upload_url}]");
    rewrite_enriched_text(&mut enriched, &[att], &[outcome]);
    assert!(
        !enriched.contains(&upload_url),
        "§4.4: audio-ok must strip the bare URL from enriched text: {enriched:?}"
    );
}

// ── regression_document_auto_extracted ──────────────────────────────────────

/// Regression guard: `dispatch_action("extract_document", ...)` still calls
/// toolgate `POST /extract-text-url` (with the localhost-rewritten URL in the
/// JSON body, NOT a file-bytes multipart) and returns `Ok` with the extracted
/// text in `summary_text`.
///
/// The `extract_document` built-in POSTs `{"document_url": "<local_url>",
/// "max_chars": 8000}` to toolgate — toolgate does the download, not core.
/// This differs from `transcribe`/`describe` which download the bytes
/// themselves and send them as multipart.
///
/// Also verifies the §4.4 rewrite contract: after `rewrite_enriched_text` the
/// bare document upload URL does NOT survive in the enriched text.
#[tokio::test]
async fn regression_document_auto_extracted() {
    // 1. Stand up a wiremock server:
    //    - POST /extract-text-url → {"text":"Quarterly report body"}
    //    (No GET /api/uploads/* mock needed: extract_document sends the URL
    //    to toolgate, so core itself never downloads the bytes for this built-in.)
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/extract-text-url"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"text": "Quarterly report body"})),
        )
        .mount(&server)
        .await;

    // 2. Build a document attachment. The URL must contain /api/uploads/ so
    //    uploads_local_url rewrites to http://localhost:{port}/api/uploads/doc1.
    let port = server.address().port();
    let gateway_listen = format!("0.0.0.0:{port}");
    let upload_url = "https://pub.example/api/uploads/doc1?sig=x&exp=1".to_string();
    let att = MediaAttachment {
        url: upload_url.clone(),
        media_type: MediaType::Document,
        file_name: Some("q3.pdf".into()),
        mime_type: Some("application/pdf".into()),
        file_size: None,
    };
    let client = reqwest::Client::new();

    // 3. Run dispatch_action("extract_document") — the exact call made by run_builtin
    //    in the seam for an application/pdf attachment with the seeded default.
    let outcome = dispatch_action(DispatchInput {
        action_ref: "extract_document",
        attachment: &att,
        toolgate_url: &server.uri(),
        gateway_listen: &gateway_listen,
        language: "en",
        http_client: &client,
        timeout: std::time::Duration::from_secs(10),
        enqueue: None,
    })
    .await;

    // ── Assertions ──────────────────────────────────────────────────────────
    assert_eq!(
        outcome.status,
        ScenarioStatus::Ok,
        "extract_document must succeed: {:?}",
        outcome.reason
    );
    // The extracted text lands in summary_text.
    assert!(
        outcome.summary_text.contains("Quarterly report body"),
        "extracted text must be in summary_text: {:?}",
        outcome.summary_text
    );
    // summary_text must NOT contain the bare signed URL (§4.4 / outcome contract).
    assert!(
        !outcome.summary_text.contains("/api/uploads/doc1"),
        "bare upload URL must not appear in summary_text: {:?}",
        outcome.summary_text
    );

    // Also verify the rewrite contract: after rewrite on document-ok the
    // hint is stripped (URL removed from the enriched text).
    let mut enriched = format!("[User attached a document \"q3.pdf\": {upload_url}]");
    rewrite_enriched_text(&mut enriched, &[att], &[outcome]);
    assert!(
        !enriched.contains(&upload_url),
        "§4.4: document-ok must strip the bare URL from enriched text: {enriched:?}"
    );
}

// ── regression_first_photo_described ────────────────────────────────────────

/// Regression guard: `dispatch_action("describe", ...)` still calls toolgate
/// `POST /describe` and returns `Ok` with `<vision>…</vision>` in `summary_text`.
///
/// Also verifies the §4.4 image-ok exception: after `rewrite_enriched_text` the
/// image URL hint IS KEPT in the enriched text (UI needs it for FilePart reconstruct).
#[tokio::test]
async fn regression_first_photo_described() {
    // 1. Stand up a wiremock server:
    //    - GET /api/uploads/img1 → JPEG bytes (for the internal download in run_describe)
    //    - POST /describe        → {"description":"a red cat"}
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/uploads/"))
        .respond_with(
            ResponseTemplate::new(200)
                // JPEG SOI marker bytes
                .set_body_bytes(b"\xFF\xD8\xFFfakeimagebytes".to_vec()),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/describe"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"description": "a red cat"})),
        )
        .mount(&server)
        .await;

    // 2. Build an image attachment. The path must contain /api/uploads/ so
    //    uploads_local_url can rewrite to http://localhost:{port}/api/uploads/img1.
    let port = server.address().port();
    let gateway_listen = format!("0.0.0.0:{port}");
    let upload_url = "https://pub.example/api/uploads/img1?sig=x&exp=1";
    let att = image_att(upload_url);
    let client = reqwest::Client::new();

    // 3. Run dispatch_action("describe") — the exact call made by run_builtin
    //    in the seam for an image/* attachment.
    let outcome = dispatch_action(DispatchInput {
        action_ref: "describe",
        attachment: &att,
        toolgate_url: &server.uri(),
        gateway_listen: &gateway_listen,
        language: "en",
        http_client: &client,
        timeout: std::time::Duration::from_secs(10),
        enqueue: None,
    })
    .await;

    // ── Assertions ──────────────────────────────────────────────────────────
    assert_eq!(
        outcome.status,
        ScenarioStatus::Ok,
        "describe must succeed: {:?}",
        outcome.reason
    );
    // The description with <vision> tags lands in summary_text.
    assert!(
        outcome.summary_text.contains("<vision>a red cat</vision>"),
        "vision tag must be in summary_text: {:?}",
        outcome.summary_text
    );

    // Also verify the §4.4 image-ok exception: the image URL hint is KEPT in
    // the enriched text so the UI can reconstruct the FilePart from history.
    let mut enriched = format!("[User attached an image: {upload_url}]");
    rewrite_enriched_text(&mut enriched, &[att], &[outcome]);
    assert!(
        enriched.contains(upload_url),
        "§4.4 image-ok: URL hint must be KEPT in enriched text: {enriched:?}"
    );
}
