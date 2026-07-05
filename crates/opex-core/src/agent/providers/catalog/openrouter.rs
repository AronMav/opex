//! Loader for the OpenRouter `/api/v1/models` catalog.
//!
//! Shape: a flat `{ "data": [ { "id": "vendor/model", "context_length": N,
//! "top_provider": { "context_length": N, "max_completion_tokens": M } } ] }`.
//! Public, no auth. Best Chinese/frontier coverage (deepseek/qwen/glm/kimi/
//! minimax/xiaomi/tencent). Lower priority than models.dev on conflict.

use super::{CatalogSource, CostMeta, ModelCatalog, ModelMeta};
use serde_json::Value;

/// Merge a parsed OpenRouter models payload into `cat`. Returns models added.
pub fn load_into(cat: &mut ModelCatalog, json: &Value) -> usize {
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return 0;
    };
    let mut n = 0;
    for m in data {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            continue;
        };
        let context = m
            .get("context_length")
            .and_then(as_u32)
            .or_else(|| {
                m.get("top_provider")
                    .and_then(|tp| tp.get("context_length"))
                    .and_then(as_u32)
            });
        let Some(context) = context else { continue };
        if context < 1000 {
            continue;
        }
        let output = m
            .get("top_provider")
            .and_then(|tp| tp.get("max_completion_tokens"))
            .and_then(as_u32);

        // OpenRouter `pricing` is USD per TOKEN (strings) — convert to per-1M to
        // match models.dev units.
        let cost = m.get("pricing").and_then(|p| {
            let per_tok = |k: &str| p.get(k).and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
            match (per_tok("prompt"), per_tok("completion")) {
                (Some(i), Some(o)) => Some(CostMeta { input: i * 1e6, output: o * 1e6 }),
                _ => None,
            }
        });

        // OpenRouter ids are `vendor/model`; the catalog derives provider from
        // the slug prefix (and also loose-indexes the bare model id).
        let (provider_id, model_id) = id.split_once('/').unwrap_or(("openrouter", id));
        cat.insert(
            provider_id,
            model_id,
            ModelMeta { context, output, cost, source: CatalogSource::OpenRouter },
        );
        n += 1;
    }
    n
}

fn as_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| v.as_f64().filter(|f| *f > 0.0).map(|f| f as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "data": [
        { "id": "moonshotai/kimi-k2", "context_length": 262144,
          "top_provider": { "context_length": 262144, "max_completion_tokens": 64000 } },
        { "id": "deepseek/deepseek-chat",
          "top_provider": { "context_length": 128000 } },
        { "id": "bad/zero", "context_length": 0 },
        { "id": "no-context-model" }
      ]
    }"#;

    #[test]
    fn parses_flat_data_and_splits_vendor() {
        let json: Value = serde_json::from_str(FIXTURE).unwrap();
        let mut cat = ModelCatalog::new();
        let added = load_into(&mut cat, &json);
        assert_eq!(added, 2, "kimi + deepseek; zero and no-context skipped");
        // provider_type moonshot -> catalog vendor moonshotai (via aliases)
        assert_eq!(cat.context("moonshot", "kimi-k2"), Some(262144));
        // top_provider.context_length fallback when top-level absent
        assert_eq!(cat.context("deepseek", "deepseek-chat"), Some(128000));
    }

    #[test]
    fn models_dev_wins_over_openrouter_on_conflict() {
        use super::super::models_dev;
        let mut cat = ModelCatalog::new();
        // models.dev first (priority 0)
        let md: Value = serde_json::from_str(
            r#"{"openai":{"models":{"gpt-x":{"limit":{"context":128000}}}}}"#,
        ).unwrap();
        models_dev::load_into(&mut cat, &md);
        // OpenRouter second (priority 1) with a different value
        let or: Value = serde_json::from_str(
            r#"{"data":[{"id":"openai/gpt-x","context_length":99999}]}"#,
        ).unwrap();
        load_into(&mut cat, &or);
        assert_eq!(cat.context("openai", "gpt-x"), Some(128000), "models.dev wins");
    }
}
