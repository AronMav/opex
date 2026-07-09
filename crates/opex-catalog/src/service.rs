//! Background loader/refresher for the model catalog.
//!
//! Phase 1a: in-memory only — fetch models.dev on startup and every
//! `refresh_hours`, install into the process-global catalog. No disk cache /
//! bundled snapshot yet (a Phase 1a follow-up for offline/first-run); a server
//! with egress simply re-fetches on restart. All errors are logged and ignored
//! so a catalog outage never blocks startup or resolution.

use std::time::Duration;

use super::{models_dev, openrouter, ModelCatalog};

/// Runtime config for the catalog loader. The host crate maps its own config
/// (e.g. `opex.toml [model_catalog]`) onto this plain struct so `opex-catalog`
/// stays free of any host dependency.
#[derive(Debug, Clone)]
pub struct CatalogConfig {
    pub enabled: bool,
    pub refresh_hours: u64,
    pub models_dev_url: String,
    pub openrouter_url: String,
}

/// Spawn the background catalog loader. No-op when disabled. The host passes a
/// pre-built `reqwest::Client` (e.g. an SSRF-guarded one) — the crate never
/// builds its own, so URL/network policy stays with the host.
pub fn spawn(cfg: CatalogConfig, client: reqwest::Client) {
    if !cfg.enabled {
        tracing::info!("model catalog disabled via config");
        return;
    }
    tokio::spawn(async move {
        let period = Duration::from_secs(cfg.refresh_hours.max(1) * 3600);
        loop {
            let (cat, all_ok) = build(&client, &cfg).await;
            let n = cat.len();
            // F047: never overwrite a working global catalog with a PARTIAL
            // fetch (one source down) — that silently drops every model
            // exclusive to the failed source and regresses context-window
            // resolution. Install only when all sources succeeded, OR when the
            // global is still empty (first population, nothing to regress).
            let global_empty = super::global()
                .read()
                .map(|g| g.is_empty())
                .unwrap_or(true);
            if n > 0 && (all_ok || global_empty) {
                super::install(cat);
                tracing::info!(models = n, all_sources_ok = all_ok, "model catalog loaded");
            } else if n > 0 && !all_ok {
                tracing::warn!(models = n, "model catalog fetch was partial (a source failed) — keeping last-good catalog to avoid regression");
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
/// Returns the built catalog AND whether EVERY enabled source succeeded (F047).
/// The caller must NOT overwrite a working global catalog with a partial one.
async fn build(client: &reqwest::Client, cfg: &CatalogConfig) -> (ModelCatalog, bool) {
    let mut cat = ModelCatalog::new();
    let mut all_ok = true;

    if !cfg.models_dev_url.is_empty() {
        match fetch_json(client, &cfg.models_dev_url).await {
            Ok(json) => {
                let n = models_dev::load_into(&mut cat, &json);
                tracing::debug!(models = n, "loaded models.dev");
            }
            Err(e) => {
                tracing::warn!(error = %e, url = %cfg.models_dev_url, "models.dev fetch failed");
                all_ok = false;
            }
        }
    }
    if !cfg.openrouter_url.is_empty() {
        match fetch_json(client, &cfg.openrouter_url).await {
            Ok(json) => {
                let n = openrouter::load_into(&mut cat, &json);
                tracing::debug!(models = n, "loaded OpenRouter");
            }
            Err(e) => {
                tracing::warn!(error = %e, url = %cfg.openrouter_url, "OpenRouter fetch failed");
                all_ok = false;
            }
        }
    }
    (cat, all_ok)
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
