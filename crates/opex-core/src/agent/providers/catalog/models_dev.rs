//! Loader for the models.dev `api.json` catalog.
//!
//! Shape: a top-level object **keyed by provider id**, each with a nested
//! `models{}` map; every model has `limit: { context, output }`. See
//! `docs/architecture/2026-07-05-model-catalog-multicatalog.md`.

use super::{Caps, CatalogSource, CostMeta, ModelCatalog, ModelMeta, ProviderMeta};
use serde_json::Value;

const OPENAI_COMPAT_NPM: &str = "@ai-sdk/openai-compatible";

/// Merge a parsed models.dev payload into `cat`. Lenient — malformed providers
/// or models are skipped. Returns the number of models added. Also records
/// provider-level metadata (name/api/env/models) for the preset picker.
pub fn load_into(cat: &mut ModelCatalog, json: &Value) -> usize {
    let Some(providers) = json.as_object() else {
        return 0;
    };
    let mut n = 0;
    for (provider_id, pv) in providers {
        let Some(models) = pv.get("models").and_then(Value::as_object) else {
            continue;
        };
        let mut model_ids: Vec<String> = Vec::new();
        for (model_id, mv) in models {
            // List every model for the preset picker (incl. TTS/embedding models
            // with no context window).
            model_ids.push(model_id.clone());
            let Some(limit) = mv.get("limit") else { continue };
            let Some(context) = limit.get("context").and_then(as_u32) else {
                continue;
            };
            // Guard against 0/bogus windows (models.dev uses 0 for "unknown").
            if context < 1000 {
                continue;
            }
            let output = limit.get("output").and_then(as_u32);
            let cost = parse_cost(mv.get("cost"));
            let caps = Some(Caps {
                attachment: mv.get("attachment").and_then(Value::as_bool).unwrap_or(false),
                reasoning: mv.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
                tool_call: mv.get("tool_call").and_then(Value::as_bool).unwrap_or(false),
                // Default true: absence must not spuriously disable temperature.
                temperature: mv.get("temperature").and_then(Value::as_bool).unwrap_or(true),
            });
            cat.insert(
                provider_id,
                model_id,
                ModelMeta { context, output, cost, caps, source: CatalogSource::ModelsDev },
            );
            model_ids.push(model_id.clone());
            n += 1;
        }

        // Provider metadata for the "add provider" preset picker.
        model_ids.sort();
        let name = pv
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id)
            .to_string();
        let api = pv.get("api").and_then(Value::as_str).map(str::to_string);
        let npm = pv.get("npm").and_then(Value::as_str).unwrap_or_default();
        let env = pv
            .get("env")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        cat.insert_provider(ProviderMeta {
            id: provider_id.clone(),
            name,
            api,
            env,
            openai_compatible: npm == OPENAI_COMPAT_NPM,
            provider_type: super::aliases::opex_provider_type(provider_id).to_string(),
            models: model_ids,
        });
    }
    n
}

fn as_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| v.as_f64().filter(|f| *f > 0.0).map(|f| f as u32))
}

/// Parse a `cost` object (`{input, output}` USD per 1M tokens). Requires both.
pub(super) fn parse_cost(v: Option<&Value>) -> Option<CostMeta> {
    let c = v?;
    let input = c.get("input").and_then(Value::as_f64)?;
    let output = c.get("output").and_then(Value::as_f64)?;
    Some(CostMeta { input, output })
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
