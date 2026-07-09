//! Universal durable worker for the File Handler Hub async queue (handler_jobs).
//! Generalization of video_worker.rs: claims a job, and (R12) for upload-based
//! jobs DOWNLOADS the upload bytes over loopback in Rust and POSTs them to
//! toolgate /handlers/{id}/run as multipart (field "file" + job_id), mirroring
//! dispatch.rs::run_transcribe. url-based jobs send the source_url form field
//! and no "file". The 202 means the runner was spawned; results come back via
//! the core callback endpoints (files.rs), not this worker.

use anyhow::Context as _;
use tokio_util::sync::CancellationToken;

use crate::gateway::AppState;
use opex_db::handler_jobs::{self, HandlerJob};

/// POST toolgate /handlers/{id}/run as multipart with job_id (R12). Treats any
/// 2xx (incl. 202 Accepted) as success.
///
/// Per-job IDOR fix: mints a `callback_token` bound to `job.id` and includes it
/// in the form so the runner can forward it to the /progress and /complete
/// callback endpoints, which verify it (closes the IDOR from Task 3).
///
/// Mime + filename: for upload-based jobs the real mime is fetched from the DB
/// (mirroring the sync path in files.rs) and `filename` is set to the
/// upload_id string — the uploads table has no original filename, so the UUID
/// is the correct stable identifier. For url-based jobs mime stays empty and
/// filename is derived from the last path segment of the source_ref.
pub async fn dispatch_async_job(
    db: &sqlx::PgPool,
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    signed_url_base: &str,
    key: &[u8; 32],
    job: &HandlerJob,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/handlers/{}/run",
        toolgate_url.trim_end_matches('/'),
        job.handler_id
    );
    let language = job
        .params
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("ru")
        .to_string();
    let params_str = serde_json::to_string(&job.params).unwrap_or_else(|_| "{}".to_string());

    // Mint the per-job callback token using the dedicated TTL constant so the
    // callback window is sized for long async jobs and is independent of the
    // upload-URL cache knob (fixes the TTL coupling issue).
    let callback_token = crate::uploads::mint_job_callback_token(
        key,
        job.id,
        crate::uploads::JOB_CALLBACK_TTL_SECS,
    );

    // Determine mime and filename. For upload-based jobs we fetch the real mime
    // from the DB (mirrors the sync path in files.rs which sends mime + upload_id).
    // For url-based jobs we leave mime empty and derive the filename from the URL.
    let (mime, filename) = if let Some(upload_id) = job.upload_id {
        let real_mime = crate::db::uploads::get_by_id(db, upload_id)
            .await
            .unwrap_or(None)
            .map(|row| row.mime)
            .unwrap_or_default();
        (real_mime, upload_id.to_string())
    } else {
        let fname = job
            .source_ref
            .as_deref()
            .and_then(|s| s.split('/').next_back())
            .and_then(|s| s.split('?').next())
            .unwrap_or("")
            .to_string();
        (String::new(), fname)
    };

    // Operator-set per-agent settings ("valves") for this handler, injected as
    // ctx.config in the runner. Best-effort: an empty object on any DB miss.
    let config_str = crate::db::handler_config::get_config(db, &job.handler_id, &job.agent_name)
        .await
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok())
        .unwrap_or_else(|| "{}".to_string());

    let mut form = reqwest::multipart::Form::new()
        .text("mime", if mime.is_empty() { "application/octet-stream".to_string() } else { mime.clone() })
        .text("filename", filename)
        .text("params", params_str)
        .text("config", config_str)
        .text("language", language)
        .text("job_id", job.id.to_string())
        .text("callback_token", callback_token);

    if let Some(upload_id) = job.upload_id {
        // R12: download the upload bytes over loopback in Rust (mirror run_transcribe),
        // then attach as the "file" part — toolgate never fetches a loopback URL.
        // Use the dedicated callback TTL for the signed URL too (consistent with
        // the token TTL above so the URL doesn't expire before the job completes).
        let public = crate::uploads::mint_uploads_url(
            signed_url_base,
            upload_id,
            key,
            crate::uploads::JOB_CALLBACK_TTL_SECS,
        );
        let local = crate::agent::url_tools::uploads_local_url(&public, gateway_listen);
        let resp = http
            .get(&local)
            .send()
            .await
            .with_context(|| format!("loopback GET {local} failed"))?;
        if !resp.status().is_success() {
            anyhow::bail!("loopback upload fetch HTTP {}", resp.status().as_u16());
        }
        let bytes = resp.bytes().await.context("read upload bytes")?;
        let part = reqwest::multipart::Part::bytes(bytes.to_vec())
            .file_name(upload_id.to_string())
            .mime_str(&mime)
            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.to_vec()));
        form = form.part("file", part);
    } else if let Some(source_ref) = &job.source_ref {
        // url-based job (e.g. YouTube): pass the external URL, no "file" part.
        form = form.text("source_url", source_ref.clone());
    }

    let resp = http
        .post(&url)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "toolgate /handlers/{}/run HTTP {}",
            job.handler_id,
            resp.status().as_u16()
        );
    }
    Ok(())
}

/// Spawn the background async-handler worker (concurrency = 1 in v1).
///
/// The worker:
/// 1. Recovers stale 'processing' rows from a previous crash on startup.
/// 2. Claims one job at a time from the durable `handler_jobs` queue.
/// 3. For upload-based jobs: downloads the upload bytes over loopback and
///    POSTs them to toolgate /handlers/{id}/run as multipart (R12).
/// 4. For url-based jobs: sends the source_url form field instead.
/// 5. A 202 response means the runner was spawned; the job stays 'processing'
///    until the runner's /complete callback marks it done.
/// Runtime stale-processing deadline (F014). A healthy job bumps `updated_at`
/// on every claim / progress post, so a 'processing' row untouched for this long
/// means its runner died without posting `/complete`. Sized well above the
/// runner's own wall-clock cap (F016) plus the largest gap between progress
/// posts of a long-video job, so legitimate jobs are never falsely reaped.
const STALE_PROCESSING_DEADLINE_SECS: i64 = 4 * 3600; // 4h
/// Minimum spacing between stale-sweeps (the poll loop ticks every 5s).
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
/// Max concurrent in-flight ('processing') runners (F088). Each runner spawns a
/// multi-minute out-of-process pipeline (yt-dlp + ffmpeg + STT + LLM), so the
/// worker must not keep dispatching new jobs while old ones are still running.
const MAX_CONCURRENT_RUNNERS: i64 = 3;

pub fn spawn_file_handler_worker(state: &AppState, shutdown: CancellationToken) {
    let state = state.clone();
    let db = state.infra.db.clone();
    let toolgate_url = state
        .config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let gateway_listen = state.config.config.gateway.listen.clone();
    let signed_url_base = crate::uploads::web_uploads_base().to_string();
    // R6: real accessor, NOT master_key — derives the per-domain HMAC key
    let key = state.infra.secrets.get_upload_hmac_key();
    // The worker owns its own reqwest::Client — pooled, reused across polls.
    // Explicit timeouts: a hung toolgate must fail the dispatch (job → failed,
    // recoverable via stale-job recovery) instead of wedging this loop forever.
    // 120 s covers the loopback download of a 50 MB upload plus the multipart
    // POST; toolgate answers 202 as soon as the runner is spawned.
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    tokio::spawn(async move {
        // Crash recovery: reset rows stuck in 'processing' from a previous run.
        match handler_jobs::recover_stale_handler_jobs(&db, STALE_PROCESSING_DEADLINE_SECS).await {
            Ok(n) if n > 0 => {
                tracing::info!(recovered = n, "file_handler_worker: recovered stale jobs")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "file_handler_worker: stale recovery failed"),
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        // F014: throttle the stale-processing sweep so it runs ~once a minute,
        // not on every 5s poll tick. `None` forces a sweep on the first tick.
        let mut last_sweep: Option<std::time::Instant> = None;
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }

            // F014: runtime sweep for jobs whose out-of-process runner died
            // without posting /complete — the once-at-startup recovery above
            // never revisits in-flight rows, so without this a stuck row (and
            // the chat's "…готовлю сводку") hangs until a full core restart.
            if last_sweep.is_none_or(|t| t.elapsed() >= SWEEP_INTERVAL) {
                last_sweep = Some(std::time::Instant::now());
                match handler_jobs::list_stale_processing_jobs(&db, STALE_PROCESSING_DEADLINE_SECS)
                    .await
                {
                    Ok(stale) => {
                        for job in &stale {
                            tracing::warn!(
                                job_id = %job.id,
                                handler = %job.handler_id,
                                "file_handler_worker: reaping stale 'processing' job (runner never posted /complete)"
                            );
                            crate::gateway::handlers::files::fail_stuck_job_and_notify(
                                &state,
                                job,
                                "handler timed out — the background job stopped responding",
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "file_handler_worker: stale sweep failed")
                    }
                }
            }

            // F088: cap concurrent in-flight runners. Skip claiming while at the
            // cap — the queued job stays put and is picked up once a running one
            // finishes (posts /complete) or is reaped by the stale sweep above.
            match handler_jobs::count_processing_handler_jobs(&db).await {
                Ok(n) if n >= MAX_CONCURRENT_RUNNERS => continue,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "file_handler_worker: processing-count failed");
                    continue;
                }
            }

            let job = match handler_jobs::claim_next_handler_job(&db).await {
                Ok(Some(j)) => j,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "file_handler_worker: claim failed");
                    continue;
                }
            };

            tracing::info!(
                job_id = %job.id,
                handler = %job.handler_id,
                agent = %job.agent_name,
                "file_handler_worker: dispatching"
            );
            // claim already set status=processing; the explicit mark is a no-op
            // but documents the intent and is kept for symmetry with video_worker.
            let _ = handler_jobs::mark_handler_job_processing(&db, job.id).await;

            if let Err(e) = dispatch_async_job(
                &db,
                &http,
                &toolgate_url,
                &gateway_listen,
                &signed_url_base,
                &key,
                &job,
            )
            .await
            {
                tracing::warn!(
                    job_id = %job.id,
                    handler = %job.handler_id,
                    error = %e,
                    "file_handler_worker: dispatch failed — marking job failed"
                );
                let _ = handler_jobs::mark_handler_job_failed(&db, job.id, &e.to_string()).await;
            }
            // Success path is terminal-by-callback: the runner posts /complete
            // which marks the job done. This worker does not wait for it.
        }
        tracing::info!("file_handler_worker: stopped");
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn upload_job() -> HandlerJob {
        HandlerJob {
            id: uuid::Uuid::new_v4(),
            upload_id: Some(uuid::Uuid::new_v4()),
            source_ref: None,
            handler_id: "summarize_video".into(),
            agent_name: "Atlas".into(),
            session_id: uuid::Uuid::new_v4(),
            params: serde_json::json!({"language": "ru"}),
            status: "processing".into(),
            phase: None,
            pct: None,
            result: None,
            attempts: 1,
        }
    }

    fn url_job() -> HandlerJob {
        let mut j = upload_job();
        j.upload_id = None;
        j.source_ref = Some("https://www.youtube.com/watch?v=abc".into());
        j
    }

    /// A lazy pool that never actually connects — used in non-DB unit tests
    /// where dispatch_async_job needs a &PgPool but the DB call is non-fatal
    /// (upload mime lookup falls back to empty string on any error).
    fn lazy_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("connect_lazy must not fail — no actual connection is made here")
    }

    #[tokio::test]
    async fn dispatch_upload_job_posts_multipart_with_loopback_bytes_and_accepts_202() {
        // The upload server returns bytes that core fetches over loopback (R12),
        // then re-POSTs as multipart to toolgate.
        let uploads = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"VIDEOBYTES".to_vec()))
            .mount(&uploads)
            .await;

        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/summarize_video/run"))
            .and(header_exists("content-type")) // multipart/form-data; boundary=...
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "accepted": true, "job_id": "ignored"
            })))
            .mount(&toolgate)
            .await;

        // gateway_listen points at the uploads mock so uploads_local_url resolves there.
        let listen = uploads.uri().trim_start_matches("http://").to_string();
        let http = reqwest::Client::new();
        let key = [7u8; 32];
        // Lazy pool: the mime DB lookup will fail gracefully (no real DB),
        // falling back to empty string — the dispatch still succeeds.
        let pool = lazy_pool();
        let res = dispatch_async_job(
            &pool,
            &http,
            &toolgate.uri(),
            &listen,
            "",          // signed_url_base = root-relative (web_uploads_base)
            &key,
            &upload_job(),
        )
        .await;

        assert!(res.is_ok(), "202 must be treated as success: {res:?}");
    }

    #[tokio::test]
    async fn dispatch_url_job_posts_source_url_without_file() {
        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/summarize_video/run"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&toolgate)
            .await;

        let http = reqwest::Client::new();
        let key = [7u8; 32];
        let pool = lazy_pool();
        // url job: no upload, no loopback download — source_ref drives it.
        let res = dispatch_async_job(
            &pool,
            &http,
            &toolgate.uri(),
            "127.0.0.1:18789",
            "",
            &key,
            &url_job(),
        )
        .await;
        assert!(res.is_ok(), "202 must be treated as success: {res:?}");
    }

    #[tokio::test]
    async fn dispatch_errors_on_non_2xx() {
        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/summarize_video/run"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&toolgate)
            .await;

        let http = reqwest::Client::new();
        let key = [7u8; 32];
        let pool = lazy_pool();
        let res = dispatch_async_job(
            &pool,
            &http,
            &toolgate.uri(),
            "127.0.0.1:18789",
            "",
            &key,
            &url_job(),
        )
        .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("500"));
    }

    #[test]
    fn url_job_filename_derived_from_last_path_segment() {
        // Verify that the filename for a url-based job is derived from the
        // last non-query segment of source_ref (not the literal "upload").
        let job = url_job(); // source_ref = "https://www.youtube.com/watch?v=abc"
        // The last path segment before '?' is "watch", query stripped.
        // We test the helper logic directly via the public dispatch path indirectly;
        // here we just verify the extraction logic matches expectations.
        let source = job.source_ref.as_deref().unwrap();
        let fname = source
            .split('/')
            .next_back()
            .and_then(|s| s.split('?').next())
            .unwrap_or("");
        assert_eq!(fname, "watch", "filename derived from last path segment, query stripped");
    }
}
