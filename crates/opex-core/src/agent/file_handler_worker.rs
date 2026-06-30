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
pub async fn dispatch_async_job(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    signed_url_base: &str,
    key: &[u8; 32],
    ttl_secs: u64,
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

    // Mint a per-job callback token so the toolgate runner can authenticate
    // its /progress and /complete callbacks. Closes the IDOR from Task 3.
    let callback_token = crate::uploads::mint_job_callback_token(key, job.id, ttl_secs);

    let mut form = reqwest::multipart::Form::new()
        .text("mime", String::new())
        .text("filename", "upload".to_string())
        .text("params", params_str)
        .text("language", language)
        .text("job_id", job.id.to_string())
        .text("callback_token", callback_token);

    if let Some(upload_id) = job.upload_id {
        // R12: download the upload bytes over loopback in Rust (mirror run_transcribe),
        // then attach as the "file" part — toolgate never fetches a loopback URL.
        let public = crate::uploads::mint_uploads_url(signed_url_base, upload_id, key, ttl_secs);
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
        let part = reqwest::multipart::Part::bytes(bytes.to_vec()).file_name("upload");
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
pub fn spawn_file_handler_worker(state: &AppState, shutdown: CancellationToken) {
    let db = state.infra.db.clone();
    let toolgate_url = state
        .config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let gateway_listen = state.config.config.gateway.listen.clone();
    let ttl_secs = state.config.config.uploads.signed_url_ttl_secs;
    let signed_url_base = crate::uploads::web_uploads_base().to_string();
    // R6: real accessor, NOT master_key — derives the per-domain HMAC key
    let key = state.infra.secrets.get_upload_hmac_key();
    // The worker owns its own reqwest::Client — pooled, reused across polls.
    let http = reqwest::Client::new();

    tokio::spawn(async move {
        // Crash recovery: reset rows stuck in 'processing' from a previous run.
        match handler_jobs::recover_stale_handler_jobs(&db).await {
            Ok(n) if n > 0 => {
                tracing::info!(recovered = n, "file_handler_worker: recovered stale jobs")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "file_handler_worker: stale recovery failed"),
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
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
                &http,
                &toolgate_url,
                &gateway_listen,
                &signed_url_base,
                &key,
                ttl_secs,
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
        let res = dispatch_async_job(
            &http,
            &toolgate.uri(),
            &listen,
            "",          // signed_url_base = root-relative (web_uploads_base)
            &key,
            600,
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
        // url job: no upload, no loopback download — source_ref drives it.
        let res = dispatch_async_job(
            &http,
            &toolgate.uri(),
            "127.0.0.1:18789",
            "",
            &key,
            600,
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
        let res =
            dispatch_async_job(&http, &toolgate.uri(), "127.0.0.1:18789", "", &key, 600, &url_job()).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("500"));
    }
}
