//! Model metadata catalog (context windows, output limits) merged from external
//! aggregators (models.dev, OpenRouter, …). See
//! `docs/architecture/2026-07-05-model-catalog-multicatalog.md`.
//!
//! The catalog is a **best-effort** source that slots into the context-window
//! resolution chain BELOW the native provider probe and ABOVE the name
//! heuristic:
//!
//! ```text
//! manual override > native self-report > CATALOG > name heuristic
//! ```
//!
//! A model absent from every catalog simply falls through — the catalog never
//! regresses a currently-working resolution.

mod aliases;
pub mod models_dev;
pub mod openrouter;
pub mod service;

pub use aliases::{catalog_provider_ids, normalize_model};

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

// ── Process-global catalog ────────────────────────────────────────────────────
//
// Mirrors the `CONTEXT_LIMIT_CACHE` pattern in `llm_call.rs`: a lazily-init'd
// global so `resolve_context_limit` / `context_limit_tokens` can consult it
// without threading a handle through every call site. Populated at startup +
// refreshed in the background by `service`.

static GLOBAL: OnceLock<RwLock<ModelCatalog>> = OnceLock::new();

fn global() -> &'static RwLock<ModelCatalog> {
    GLOBAL.get_or_init(|| RwLock::new(ModelCatalog::new()))
}

/// Replace the process-global catalog (atomic swap under a write lock).
pub fn install(catalog: ModelCatalog) {
    if let Ok(mut g) = global().write() {
        *g = catalog;
    }
}

/// Look up a model's context window in the process-global catalog.
/// `None` when the catalog is empty or the model isn't found.
pub fn global_context(provider_type: &str, model: &str) -> Option<u32> {
    global().read().ok().and_then(|c| c.context(provider_type, model))
}

/// All catalog providers (sorted) for the preset picker. Empty when the catalog
/// hasn't loaded yet.
pub fn global_providers() -> Vec<ProviderMeta> {
    global().read().map(|c| c.providers_sorted()).unwrap_or_default()
}

/// Look up a model's max-output-tokens in the process-global catalog.
pub fn global_output(provider_type: &str, model: &str) -> Option<u32> {
    global().read().ok().and_then(|c| c.output(provider_type, model))
}

/// Look up a model's token cost (USD per 1M tokens) in the process-global catalog.
pub fn global_cost(provider_type: &str, model: &str) -> Option<CostMeta> {
    global().read().ok().and_then(|c| c.cost(provider_type, model))
}

/// Look up a model's capability flags in the process-global catalog.
pub fn global_caps(provider_type: &str, model: &str) -> Option<Caps> {
    global().read().ok().and_then(|c| c.caps(provider_type, model))
}

/// Which aggregator a `ModelMeta` came from. Lower `priority()` wins on conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    ModelsDev,
    // Constructed by the Phase 1b/1c loaders; the priority ladder already
    // accounts for them so merge order is fixed from the start.
    #[allow(dead_code)]
    OpenRouter,
    #[allow(dead_code)]
    LiteLlm,
}

impl CatalogSource {
    fn priority(self) -> u8 {
        match self {
            CatalogSource::ModelsDev => 0,
            CatalogSource::OpenRouter => 1,
            CatalogSource::LiteLlm => 2,
        }
    }
}

/// USD cost per **1M tokens** (models.dev `cost` units).
#[derive(Debug, Clone, Copy)]
pub struct CostMeta {
    pub input: f64,
    pub output: f64,
}

/// Model capability flags (models.dev booleans). Absent fields default to the
/// permissive value so the catalog never *disables* something spuriously.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Accepts file/image attachments (vision). Carried for the deferred
    /// attachment/vision gate.
    #[allow(dead_code)]
    pub attachment: bool,
    /// Extended reasoning / chain-of-thought. Carried for the deferred
    /// reasoning-content gate.
    #[allow(dead_code)]
    pub reasoning: bool,
    /// Function calling. Carried for future tool gating.
    #[allow(dead_code)]
    pub tool_call: bool,
    /// Accepts a `temperature` parameter (false for e.g. o1-style models).
    pub temperature: bool,
}

/// Metadata for one model.
#[derive(Debug, Clone)]
pub struct ModelMeta {
    /// Total context window in tokens (matches `compressor.context_limit`).
    pub context: u32,
    /// Max output tokens, when the source reports it (Phase 3 max_tokens cap).
    pub output: Option<u32>,
    /// USD/1M-token cost, when the source reports it (Phase 3 $ usage).
    pub cost: Option<CostMeta>,
    /// Capability flags, when the source reports them (Phase 3c gating).
    pub caps: Option<Caps>,
    pub source: CatalogSource,
}

/// Provider-level metadata from the catalog — powers the "add provider" preset
/// picker (Phase 2). Only sources that carry provider info (models.dev) populate
/// this; model-only sources (OpenRouter) don't.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderMeta {
    pub id: String,
    pub name: String,
    /// Base API URL (models.dev `api`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// API-key env var name(s) (models.dev `env`).
    pub env: Vec<String>,
    /// Whether this provider is OpenAI-compatible (`npm` = `@ai-sdk/openai-compatible`).
    pub openai_compatible: bool,
    /// Known model ids (sorted).
    pub models: Vec<String>,
}

/// In-memory model catalog: an exact `(catalog_provider_id, model_id)` index
/// plus a loose `model_id`-only index for provider-agnostic fallback matches,
/// and a provider-metadata map for the preset picker.
#[derive(Debug, Default)]
pub struct ModelCatalog {
    exact: HashMap<(String, String), ModelMeta>,
    loose: HashMap<String, ModelMeta>,
    providers: HashMap<String, ProviderMeta>,
}

impl ModelCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of exact entries — for diagnostics / logging after a load.
    pub fn len(&self) -> usize {
        self.exact.len()
    }

    #[allow(dead_code)] // paired with len() (clippy::len_without_is_empty)
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty()
    }

    /// Record provider-level metadata (preset picker source).
    pub fn insert_provider(&mut self, p: ProviderMeta) {
        if !p.id.is_empty() {
            self.providers.insert(p.id.clone(), p);
        }
    }

    /// All providers, sorted by display name — for the preset picker.
    pub fn providers_sorted(&self) -> Vec<ProviderMeta> {
        let mut v: Vec<ProviderMeta> = self.providers.values().cloned().collect();
        v.sort_by_key(|p| p.name.to_ascii_lowercase());
        v
    }

    /// Insert one model. `provider_id` is the catalog's own id (e.g. `moonshotai`),
    /// NOT our `provider_type`. Higher-priority sources overwrite lower ones; a
    /// same-or-lower priority source never clobbers an existing entry.
    pub fn insert(&mut self, provider_id: &str, model_id: &str, meta: ModelMeta) {
        let pid = provider_id.trim().to_ascii_lowercase();
        let mid = normalize_model(model_id);
        if pid.is_empty() || mid.is_empty() {
            return;
        }
        upsert(&mut self.exact, (pid, mid.clone()), meta.clone());

        // Loose index by the bare model id, plus the suffix after a `vendor/`
        // prefix (catalog aggregators key some models as `vendor/model`).
        if let Some((_vendor, suffix)) = mid.split_once('/')
            && !suffix.is_empty()
        {
            upsert_loose(&mut self.loose, suffix.to_string(), meta.clone());
        }
        upsert_loose(&mut self.loose, mid, meta);
    }

    /// Resolve a model's context window for one of OUR `provider_type`s.
    /// Returns `None` when the model isn't in the catalog (caller falls back).
    pub fn context(&self, provider_type: &str, model: &str) -> Option<u32> {
        self.lookup(provider_type, model).map(|m| m.context)
    }

    /// Resolve a model's max-output-tokens, when the catalog reports it.
    pub fn output(&self, provider_type: &str, model: &str) -> Option<u32> {
        self.lookup(provider_type, model).and_then(|m| m.output)
    }

    /// Resolve a model's token cost (USD/1M), when the catalog reports it.
    pub fn cost(&self, provider_type: &str, model: &str) -> Option<CostMeta> {
        self.lookup(provider_type, model).and_then(|m| m.cost)
    }

    /// Resolve a model's capability flags, when the catalog reports them.
    pub fn caps(&self, provider_type: &str, model: &str) -> Option<Caps> {
        self.lookup(provider_type, model).and_then(|m| m.caps)
    }

    fn lookup(&self, provider_type: &str, model: &str) -> Option<&ModelMeta> {
        let mid = normalize_model(model);

        // 1. Exact match under any catalog provider id mapped from provider_type.
        for cid in catalog_provider_ids(provider_type) {
            if let Some(m) = self.exact.get(&(cid.to_string(), mid.clone())) {
                return Some(m);
            }
        }
        // 2. Explicit model alias (e.g. `kimi-k2.6` -> `moonshotai/kimi-k2`).
        if let Some((vendor, vmodel)) = aliases::model_alias(&mid)
            && let Some(m) = self.exact.get(&(vendor.to_string(), vmodel.to_string()))
        {
            return Some(m);
        }
        // 3. Loose match by model id (any provider).
        self.loose.get(&mid)
    }
}

fn upsert(map: &mut HashMap<(String, String), ModelMeta>, key: (String, String), meta: ModelMeta) {
    match map.get(&key) {
        Some(existing) if existing.source.priority() <= meta.source.priority() => {}
        _ => {
            map.insert(key, meta);
        }
    }
}

fn upsert_loose(map: &mut HashMap<String, ModelMeta>, key: String, meta: ModelMeta) {
    match map.get(&key) {
        Some(existing) if existing.source.priority() <= meta.source.priority() => {}
        _ => {
            map.insert(key, meta);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(context: u32, source: CatalogSource) -> ModelMeta {
        ModelMeta { context, output: None, cost: None, caps: None, source }
    }

    #[test]
    fn exact_lookup_via_provider_type_alias() {
        let mut c = ModelCatalog::new();
        // catalog provider id `moonshotai`; our provider_type is `moonshot`.
        c.insert("moonshotai", "kimi-k2", meta(262_144, CatalogSource::ModelsDev));
        assert_eq!(c.context("moonshot", "kimi-k2"), Some(262_144));
        // unknown model -> None (caller falls back)
        assert_eq!(c.context("moonshot", "no-such-model"), None);
    }

    #[test]
    fn model_alias_resolves_versioned_id() {
        let mut c = ModelCatalog::new();
        c.insert("moonshotai", "kimi-k2", meta(262_144, CatalogSource::ModelsDev));
        // ollama sends `kimi-k2.6:cloud` — normalized + aliased to moonshotai/kimi-k2
        assert_eq!(c.context("ollama", "kimi-k2.6:cloud"), Some(262_144));
    }

    #[test]
    fn loose_match_by_model_id_when_provider_unknown() {
        let mut c = ModelCatalog::new();
        c.insert("openrouter-vendor", "some-model", meta(200_000, CatalogSource::OpenRouter));
        // provider_type has no alias to `openrouter-vendor`, but the bare model
        // id matches loosely.
        assert_eq!(c.context("custom-thing", "some-model"), Some(200_000));
    }

    #[test]
    fn vendor_slash_model_is_loose_indexed_by_suffix() {
        let mut c = ModelCatalog::new();
        c.insert("requesty", "xai/grok-4", meta(256_000, CatalogSource::ModelsDev));
        // our provider sends the bare `grok-4`
        assert_eq!(c.context("xai", "grok-4"), Some(256_000));
    }

    #[test]
    fn higher_priority_source_wins_on_conflict() {
        let mut c = ModelCatalog::new();
        c.insert("openai", "gpt-x", meta(100_000, CatalogSource::OpenRouter));
        c.insert("openai", "gpt-x", meta(128_000, CatalogSource::ModelsDev)); // higher prio
        assert_eq!(c.context("openai", "gpt-x"), Some(128_000));
        // lower-priority source must NOT clobber the models.dev value
        c.insert("openai", "gpt-x", meta(999, CatalogSource::LiteLlm));
        assert_eq!(c.context("openai", "gpt-x"), Some(128_000));
    }
}
