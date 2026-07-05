//! Background loader/refresher for the model catalog.
//!
//! Phase 1a: in-memory only — fetch models.dev on startup and every
//! `refresh_hours`, install into the process-global catalog. No disk cache /
//! bundled snapshot yet (a Phase 1a follow-up for offline/first-run); a server
//! with egress simply re-fetches on restart. All errors are logged and ignored
//! so a catalog outage never blocks startup or resolution.

use std::time::Duration;

use super::{models_dev, ModelCatalog};
use crate::config::ModelCatalogConfig;

/// Spawn the background catalog loader. No-op when disabled.
pub fn spawn(cfg: ModelCatalogConfig) {
    if !cfg.enabled {
        tracing::info!("model catalog disabled via config");
        return;
    }
    tokio::spawn(async move {
        // SSRF-guarded client: the URL is admin-configured (trusted), but the
        // guarded client is harmless for a public host and safe if misconfigured.
        let client = crate::net::ssrf::ssrf_http_client(Duration::from_secs(20));
        let period = Duration::from_secs(cfg.refresh_hours.max(1) * 3600);
        loop {
            match fetch_models_dev(&client, &cfg.models_dev_url).await {
                Ok(cat) => {
                    let n = cat.len();
                    super::install(cat);
                    tracing::info!(models = n, url = %cfg.models_dev_url, "model catalog loaded");
                }
                Err(e) => {
                    tracing::warn!(error = %e, url = %cfg.models_dev_url, "model catalog fetch failed (falling back to native probe / heuristic)");
                }
            }
            tokio::time::sleep(period).await;
        }
    });
}

async fn fetch_models_dev(client: &reqwest::Client, url: &str) -> anyhow::Result<ModelCatalog> {
    let text = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let mut cat = ModelCatalog::new();
    let n = models_dev::load_into(&mut cat, &json);
    if n == 0 {
        anyhow::bail!("models.dev payload parsed to zero models");
    }
    Ok(cat)
}
