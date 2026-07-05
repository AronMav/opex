//! Background loader/refresher for the model catalog.
//!
//! Phase 1a: in-memory only — fetch models.dev on startup and every
//! `refresh_hours`, install into the process-global catalog. No disk cache /
//! bundled snapshot yet (a Phase 1a follow-up for offline/first-run); a server
//! with egress simply re-fetches on restart. All errors are logged and ignored
//! so a catalog outage never blocks startup or resolution.

use std::time::Duration;

use super::{models_dev, openrouter, ModelCatalog};
use crate::config::ModelCatalogConfig;

/// Spawn the background catalog loader. No-op when disabled.
pub fn spawn(cfg: ModelCatalogConfig) {
    if !cfg.enabled {
        tracing::info!("model catalog disabled via config");
        return;
    }
    tokio::spawn(async move {
        // SSRF-guarded client: the URLs are admin-configured (trusted), but the
        // guarded client is harmless for a public host and safe if misconfigured.
        let client = crate::net::ssrf::ssrf_http_client(Duration::from_secs(20));
        let period = Duration::from_secs(cfg.refresh_hours.max(1) * 3600);
        loop {
            let cat = build(&client, &cfg).await;
            let n = cat.len();
            if n > 0 {
                super::install(cat);
                tracing::info!(models = n, "model catalog loaded");
            } else {
                tracing::warn!("model catalog empty after fetch (all sources failed) — falling back to native probe / heuristic");
            }
            tokio::time::sleep(period).await;
        }
    });
}

/// Build one catalog from all configured sources. models.dev is loaded FIRST
/// (priority 0), OpenRouter SECOND (priority 1), so on-conflict models.dev wins.
/// Each source is independent — a failure logs and is skipped.
async fn build(client: &reqwest::Client, cfg: &ModelCatalogConfig) -> ModelCatalog {
    let mut cat = ModelCatalog::new();

    if !cfg.models_dev_url.is_empty() {
        match fetch_json(client, &cfg.models_dev_url).await {
            Ok(json) => {
                let n = models_dev::load_into(&mut cat, &json);
                tracing::debug!(models = n, "loaded models.dev");
            }
            Err(e) => tracing::warn!(error = %e, url = %cfg.models_dev_url, "models.dev fetch failed"),
        }
    }
    if !cfg.openrouter_url.is_empty() {
        match fetch_json(client, &cfg.openrouter_url).await {
            Ok(json) => {
                let n = openrouter::load_into(&mut cat, &json);
                tracing::debug!(models = n, "loaded OpenRouter");
            }
            Err(e) => tracing::warn!(error = %e, url = %cfg.openrouter_url, "OpenRouter fetch failed"),
        }
    }
    cat
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> anyhow::Result<serde_json::Value> {
    let text = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(serde_json::from_str(&text)?)
}
