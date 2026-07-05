//! Mapping between OPEX `provider_type`s and external-catalog provider ids, plus
//! model-id normalization and a small explicit alias table for versioned ids
//! whose bare form doesn't match the catalog slug.

/// Normalize a model id for catalog lookup: lowercase, trim, and strip an
/// ollama-style `:tag` suffix (`kimi-k2.6:cloud` -> `kimi-k2.6`). The `vendor/`
/// prefix (if any) is preserved — the catalog index handles suffix matching.
pub fn normalize_model(model: &str) -> String {
    let m = model.trim().to_ascii_lowercase();
    match m.split_once(':') {
        Some((head, _tag)) if !head.is_empty() => head.to_string(),
        _ => m,
    }
}

/// Catalog provider id(s) a given OPEX `provider_type` may appear under. First
/// match wins in the exact index. Unknown types map to themselves so a
/// same-named catalog provider still matches.
pub fn catalog_provider_ids(provider_type: &str) -> &'static [&'static str] {
    match provider_type.to_ascii_lowercase().as_str() {
        "openai" | "openai_compat" | "codex-cli" => &["openai"],
        "anthropic" | "claude-cli" => &["anthropic"],
        "google" | "gemini-cli" | "gemini-cloudcode" => &["google", "google-vertex"],
        "deepseek" => &["deepseek"],
        "moonshot" => &["moonshotai", "moonshot"],
        "qwen" => &["alibaba", "dashscope", "qwen"],
        "glm" => &["zhipuai", "zhipu", "z-ai", "glm"],
        "minimax" => &["minimax"],
        "xai" => &["xai"],
        "mistral" => &["mistral"],
        "groq" => &["groq"],
        "together" => &["togetherai", "together"],
        "perplexity" => &["perplexity"],
        "nvidia" => &["nvidia"],
        "openrouter" => &["openrouter"],
        "huggingface" => &["huggingface"],
        "xiaomi" => &["xiaomi"],
        "cloudflare" => &["cloudflare-workers-ai", "cloudflare"],
        // ollama / vllm / sglang / volcengine / qianfan / litellm / venice /
        // qianfan self-report or have no reliable catalog id → try the raw type.
        other => raw(other),
    }
}

/// Leak the raw type as a single-element static slice. Called only for
/// otherwise-unmapped provider types (rare, cached-ish via interning below).
fn raw(other: &str) -> &'static [&'static str] {
    // A tiny intern set for the handful of unmapped provider types so the
    // returned slice can be `'static` without leaking on every call.
    use std::sync::{Mutex, OnceLock};
    static INTERN: OnceLock<Mutex<Vec<&'static [&'static str]>>> = OnceLock::new();
    let intern = INTERN.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut guard) = intern.lock() {
        if let Some(slot) = guard.iter().find(|s| s[0] == other) {
            return slot;
        }
        let leaked: &'static str = Box::leak(other.to_string().into_boxed_str());
        let slice: &'static [&'static str] = Box::leak(Box::new([leaked]));
        guard.push(slice);
        return slice;
    }
    &[]
}

/// Explicit `(normalized_model_id) -> (catalog_provider_id, catalog_model_id)`
/// aliases for versioned/renamed ids whose bare form doesn't match a catalog
/// slug. Kept small and additive.
pub fn model_alias(normalized_model: &str) -> Option<(&'static str, &'static str)> {
    match normalized_model {
        "kimi-k2.6" | "kimi-k2" => Some(("moonshotai", "kimi-k2")),
        "kimi-k1.5" => Some(("moonshotai", "kimi-k1.5")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_tag_and_lowercases() {
        assert_eq!(normalize_model("Kimi-K2.6:cloud"), "kimi-k2.6");
        assert_eq!(normalize_model(" GPT-4o "), "gpt-4o");
        assert_eq!(normalize_model("moonshotai/kimi-k2"), "moonshotai/kimi-k2");
    }

    #[test]
    fn provider_type_maps_to_catalog_ids() {
        assert_eq!(catalog_provider_ids("moonshot"), &["moonshotai", "moonshot"]);
        assert_eq!(catalog_provider_ids("openai_compat"), &["openai"]);
        // unmapped type maps to itself (stable across calls)
        assert_eq!(catalog_provider_ids("ollama"), &["ollama"]);
        assert_eq!(catalog_provider_ids("ollama"), &["ollama"]);
    }
}
