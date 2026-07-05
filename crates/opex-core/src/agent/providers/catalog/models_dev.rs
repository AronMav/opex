//! Loader for the models.dev `api.json` catalog.
//!
//! Shape: a top-level object **keyed by provider id**, each with a nested
//! `models{}` map; every model has `limit: { context, output }`. See
//! `docs/architecture/2026-07-05-model-catalog-multicatalog.md`.

use super::{CatalogSource, ModelCatalog, ModelMeta};
use serde_json::Value;

/// Merge a parsed models.dev payload into `cat`. Lenient — malformed providers
/// or models are skipped. Returns the number of models added.
pub fn load_into(cat: &mut ModelCatalog, json: &Value) -> usize {
    let Some(providers) = json.as_object() else {
        return 0;
    };
    let mut n = 0;
    for (provider_id, pv) in providers {
        let Some(models) = pv.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_id, mv) in models {
            let Some(limit) = mv.get("limit") else { continue };
            let Some(context) = limit.get("context").and_then(as_u32) else {
                continue;
            };
            // Guard against 0/bogus windows (models.dev uses 0 for "unknown").
            if context < 1000 {
                continue;
            }
            let output = limit.get("output").and_then(as_u32);
            cat.insert(
                provider_id,
                model_id,
                ModelMeta { context, output, source: CatalogSource::ModelsDev },
            );
            n += 1;
        }
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
      "moonshotai": {
        "id": "moonshotai",
        "name": "Moonshot AI",
        "models": {
          "kimi-k2": { "id": "kimi-k2", "limit": { "context": 262144, "output": 64000 } }
        }
      },
      "openai": {
        "id": "openai",
        "name": "OpenAI",
        "models": {
          "gpt-4o": { "id": "gpt-4o", "limit": { "context": 128000, "output": 16384 } },
          "broken": { "id": "broken", "limit": { "context": 0 } }
        }
      },
      "malformed": { "id": "malformed" }
    }"#;

    #[test]
    fn parses_fixture_into_index() {
        let json: Value = serde_json::from_str(FIXTURE).unwrap();
        let mut cat = ModelCatalog::new();
        let added = load_into(&mut cat, &json);
        assert_eq!(added, 2, "kimi-k2 + gpt-4o; broken(0) and malformed skipped");
        assert_eq!(cat.context("moonshot", "kimi-k2"), Some(262144));
        assert_eq!(cat.context("openai", "gpt-4o"), Some(128000));
        assert_eq!(cat.context("openai", "broken"), None);
    }

    #[test]
    fn non_object_json_is_zero() {
        let json: Value = serde_json::from_str("[]").unwrap();
        let mut cat = ModelCatalog::new();
        assert_eq!(load_into(&mut cat, &json), 0);
    }
}
