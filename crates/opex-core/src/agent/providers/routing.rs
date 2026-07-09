//! `RoutingProvider`: condition-based dispatch across multiple
//! `Arc<dyn LlmProvider>` routes with cooldown + failover handling.
//!
//! The router walks `routes` in order, picking the first whose
//! `condition` matches the latest user message (length-based or
//! keyword-based: `short`, `long`, `with_tools`, `financial`,
//! `analytical`, `code`, `default`/`always`). On primary failure it
//! consults `error_classify` / `LlmCallError::is_failover_worthy` to
//! decide whether to bubble up or try fallbacks.
//!
//! Design notes embedded inline:
//!
//! - `chat()` and `chat_stream()` have identical routing logic but
//!   cannot share a generic helper due to async-trait object lifetime
//!   issues. The duplication is intentional and kept in sync.
//! - Per-route `cooldown_duration` × `AuthError` 300s floor (spec §4.6).
//! - `max_failover_attempts` cap (issue #9, c55b039) prevents unbounded
//!   cascades on long fallback chains.
//! - Mid-stream failure cannot fail over: cooldown is applied but the
//!   error bubbles up so the user sees the partial result (issue #6).

use std::sync::Arc;

use anyhow::Result;

use super::factory::{ProviderOverrides, build_provider};
use super::timeouts;
use super::{CallOptions, LlmCallError, LlmProvider, UnconfiguredProvider};
use crate::secrets::SecretsManager;

struct RouteEntry {
    condition: String,
    /// Unique key for cooldown tracking: "{`condition}:{provider_name`}" to prevent
    /// two routes that use the same provider (but different models/configs) from
    /// sharing a cooldown bucket.
    key: String,
    provider: Arc<dyn LlmProvider>,
    cooldown_duration: std::time::Duration,
}

/// Routing provider: selects the appropriate backend based on message characteristics.
pub struct RoutingProvider {
    routes: Vec<RouteEntry>,
    /// Tracks providers on cooldown (provider name → cooldown expiry).
    cooldowns: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    /// Maximum number of *failover* attempts per request. Does NOT count the
    /// primary call itself — a value of 3 means "up to 3 fallbacks after
    /// primary failed". Re-added to prevent unbounded cascading failures
    /// through long fallback chains (see issue #9 / commit c55b039).
    max_failover_attempts: u32,
}

/// Create a routing provider from ordered route configs. Each route references a
/// named DB provider via `connection`; this function resolves each to a
/// `ProviderRow` and builds a provider via `build_provider`.
///
/// `agent_temperature` / `agent_max_tokens` / `agent_prompt_cache` provide
/// agent-level defaults that propagate to every route's provider build. A
/// route's `temperature` field (if present) takes precedence over
/// `agent_temperature`; `model` override comes from the route's `model` field.
/// `agent_prompt_cache` mirrors the same field on the direct (non-routing)
/// path: Anthropic providers honour it, others ignore it (CACHE-04).
///
/// `max_failover_attempts` caps the number of fallback attempts per request
/// (re-added post-c55b039 / 8d33376 — see issue #9).
///
/// Routes with a missing or invalid `connection` are skipped with a log entry.
#[allow(clippy::too_many_arguments)]
pub async fn create_routing_provider(
    db: &sqlx::PgPool,
    routes: &[crate::config::ProviderRouteConfig],
    agent_temperature: f64,
    agent_max_tokens: Option<u32>,
    agent_prompt_cache: bool,
    max_failover_attempts: u32,
    secrets: Arc<SecretsManager>,
) -> Arc<dyn LlmProvider> {
    let mut entries: Vec<RouteEntry> = Vec::with_capacity(routes.len());
    for r in routes {
        let Some(conn_name) = r.connection.as_deref().filter(|s| !s.is_empty()) else {
            tracing::warn!(condition = %r.condition, "routing rule has no `connection` — skipping");
            continue;
        };
        let row = match crate::db::providers::get_provider_by_name(db, conn_name).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::warn!(condition = %r.condition, connection = %conn_name,
                    "routing rule references missing provider — skipping");
                continue;
            }
            Err(e) => {
                tracing::error!(condition = %r.condition, connection = %conn_name, error = %e,
                    "DB error resolving route connection — skipping");
                continue;
            }
        };
        let opts: timeouts::ProviderOptions =
            serde_json::from_value(row.options.clone()).unwrap_or_default();
        let timeouts_cfg = opts.timeouts;
        let cancel = tokio_util::sync::CancellationToken::new();
        // Route temperature beats agent default; missing route temperature
        // falls through to `agent_temperature` (which is itself agent → global default).
        let effective_temperature = r.temperature.unwrap_or(agent_temperature);
        let model_override = r
            .model
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let overrides = ProviderOverrides {
            model: model_override,
            temperature: Some(effective_temperature),
            max_tokens: agent_max_tokens,
            // Agent-level prompt_cache flag propagates to every route's
            // provider build. Anthropic honours it; other providers ignore
            // (CACHE-04). Mirrors the non-routing build path in factory.rs.
            prompt_cache: Some(agent_prompt_cache),
        };
        let p = match build_provider(&row, secrets.clone(), &timeouts_cfg, cancel, overrides) {
            Ok(p) => {
                let arc: Arc<dyn LlmProvider> = Arc::from(p);
                arc
            }
            Err(e) => {
                tracing::error!(condition = %r.condition, connection = %conn_name, error = %e,
                    "failed to build provider from route — skipping");
                continue;
            }
        };
        let key = format!("{}:{}", r.condition, p.name());
        entries.push(RouteEntry {
            condition: r.condition.clone(),
            key,
            provider: p,
            cooldown_duration: std::time::Duration::from_secs(r.cooldown_secs.max(1)),
        });
    }

    // Issue #2: if every configured route was skipped (missing `connection`,
    // missing DB row, or `build_provider` failure), install a sentinel
    // `UnconfiguredProvider` entry so `select_route` always has something
    // to return. Without this, the first `chat()` would panic in
    // `select_route` via `.expect("RoutingProvider has no routes")`.
    //
    // The sentinel returns `LlmCallError::AuthError` (classified,
    // non-failover-worthy) on every call — matching the degraded-path
    // pattern used by `resolve_provider_for_agent`. Every call now surfaces
    // a consistent typed error instead of panicking.
    if entries.is_empty() {
        tracing::error!(
            attempted_routes = routes.len(),
            "RoutingProvider has no usable routes — installing \
             `unconfigured` sentinel; all LLM calls for this agent will \
             return a classified error until a working route is added"
        );
        let sentinel: Arc<dyn LlmProvider> = Arc::new(UnconfiguredProvider::new(
            "no usable routes",
        ));
        entries.push(RouteEntry {
            condition: "default".to_string(),
            key: "unconfigured:sentinel".to_string(),
            provider: sentinel,
            cooldown_duration: std::time::Duration::from_secs(1),
        });
    }

    Arc::new(RoutingProvider {
        routes: entries,
        cooldowns: std::sync::Mutex::new(std::collections::HashMap::new()),
        max_failover_attempts,
    })
}

impl RoutingProvider {
    /// Choose the best matching provider for the given messages and tools.
    /// Evaluates conditions in order; returns the first match.
    /// Falls back to the last route if nothing else matches.
    fn select_route(
        &self,
        messages: &[opex_types::Message],
        tools: &[opex_types::ToolDefinition],
    ) -> Result<&RouteEntry> {
        let last_user_msg = messages
            .iter()
            .rev()
            .find(|m| m.role == opex_types::MessageRole::User)
            .map_or("", |m| m.content.as_str());

        let last_user_len = last_user_msg.len();
        let lower = last_user_msg.to_lowercase();

        for entry in &self.routes {
            let matches = match entry.condition.as_str() {
                "short" => last_user_len < 300,
                "long" => last_user_len > 2000,
                "with_tools" => !tools.is_empty(),
                "financial" => contains_any(&lower, FINANCIAL_KEYWORDS),
                "analytical" => contains_any(&lower, ANALYTICAL_KEYWORDS),
                "code" => contains_any(&lower, CODE_KEYWORDS),
                "default" | "always" => true,
                "fallback" => false, // only used via explicit fallback logic below
                _ => false,
            };
            if matches {
                tracing::debug!(condition = %entry.condition, "routing condition matched");
                return Ok(entry);
            }
        }

        // Last resort: return last route (or first if routes is empty —
        // shouldn't happen because `create_routing_provider` installs a
        // sentinel `UnconfiguredProvider` when the route list would
        // otherwise be empty, so this branch is unreachable in prod.
        // Belt + suspenders: return an error instead of panicking.
        self.routes.last()
            .or_else(|| self.routes.first())
            .ok_or_else(|| anyhow::anyhow!(
                "RoutingProvider has no routes — this indicates a bug in \
                 create_routing_provider (sentinel was not installed)"
            ))
    }

    /// Check if a provider is on cooldown.
    fn is_on_cooldown(&self, name: &str) -> bool {
        let map = self.cooldowns.lock().unwrap_or_else(|e| {
            tracing::warn!("cooldowns Mutex poisoned, recovering");
            e.into_inner()
        });
        map.get(name).is_some_and(|exp| std::time::Instant::now() < *exp)
    }

    /// Put a provider on cooldown.
    fn set_cooldown(&self, name: &str, duration: std::time::Duration) {
        let mut map = self.cooldowns.lock().unwrap_or_else(|e| {
            tracing::warn!("cooldowns Mutex poisoned on write, recovering");
            e.into_inner()
        });
        map.insert(name.to_string(), std::time::Instant::now() + duration);
    }

    /// Classify error and apply appropriate cooldown.
    ///
    /// Returns `Some(reason)` if the error is failover-worthy (caller
    /// should try next route — the returned short string is the label
    /// suitable for `llm_failover_total{reason=…}`), or `None` if the
    /// error should bubble up immediately (preserving any `partial_state`
    /// carried by the typed `LlmCallError`).
    ///
    /// Resolution order:
    /// 1. Downcast to `LlmCallError` and honor `is_failover_worthy()`.
    ///    - `AuthError` failover is disabled per the typed predicate, but
    ///      when the error is classified via the legacy string path we still
    ///      apply the 300s cooldown floor documented in spec §4.6.
    /// 2. If the downcast fails, fall back to the legacy string-based
    ///    classification. Untyped errors are treated as failover-worthy to
    ///    preserve historical behavior.
    ///
    /// Side effect: bumps `metrics::MetricsRegistry::record_llm_timeout`
    /// when the error is one of the four `LlmCallError` timeout variants
    /// (connect / request / inactivity / max_duration). The failover
    /// counter itself is bumped by the caller once it knows the target
    /// route (`from → to`).
    fn handle_provider_error(
        &self,
        e: &anyhow::Error,
        provider_name: &str,
        route_cooldown: std::time::Duration,
    ) -> Option<&'static str> {
        if let Some(llm_err) = e.downcast_ref::<LlmCallError>() {
            // Bump the timeout counter for the four timeout variants,
            // regardless of failover-worthiness (max_duration is NOT
            // failover-worthy but is still a timeout we want to count).
            if let Some(metrics) = crate::metrics::global() {
                match llm_err {
                    LlmCallError::ConnectTimeout { provider, .. } => {
                        metrics.record_llm_timeout(provider, "connect");
                    }
                    LlmCallError::RequestTimeout { provider, .. } => {
                        metrics.record_llm_timeout(provider, "request");
                    }
                    LlmCallError::InactivityTimeout { provider, .. } => {
                        metrics.record_llm_timeout(provider, "inactivity");
                    }
                    LlmCallError::MaxDurationExceeded { provider, .. } => {
                        metrics.record_llm_timeout(provider, "max_duration");
                    }
                    _ => {}
                }
            }

            if !llm_err.is_failover_worthy() {
                // Non-failover-worthy errors bubble up to the caller, but some
                // typed variants (notably `AuthError`) still deserve a cooldown
                // to prevent re-hammering the primary with the same credentials.
                // Issue #8: the 300s floor documented in spec §4.6 was
                // previously only reachable on the legacy string-classified
                // path; the typed path short-circuited before `set_cooldown`.
                if matches!(llm_err, LlmCallError::AuthError { .. }) {
                    let cd = std::time::Duration::from_secs(route_cooldown.as_secs().max(300));
                    tracing::warn!(
                        provider = %provider_name,
                        error = %e,
                        cooldown_secs = cd.as_secs(),
                        "route failed with AuthError — applying 300s cooldown floor (not failing over)"
                    );
                    self.set_cooldown(provider_name, cd);
                } else {
                    tracing::warn!(
                        provider = %provider_name,
                        error = %e,
                        "route failed with non-failover-worthy error — bubbling up"
                    );
                }
                return None;
            }
            let cd = match llm_err {
                LlmCallError::AuthError { .. } => {
                    std::time::Duration::from_secs(route_cooldown.as_secs().max(300))
                }
                _ => route_cooldown.max(std::time::Duration::from_secs(1)),
            };
            let reason: &'static str = match llm_err {
                LlmCallError::ConnectTimeout { .. } => "connect_timeout",
                LlmCallError::RequestTimeout { .. } => "request_timeout",
                LlmCallError::InactivityTimeout { .. } => "inactivity",
                LlmCallError::Server5xx { .. } => "5xx",
                LlmCallError::Network(_) => "network",
                LlmCallError::SchemaError { .. } => "schema_pre_stream",
                // The remaining typed variants are NOT failover-worthy
                // (AuthError, MaxDurationExceeded, UserCancelled,
                // ShutdownDrain) and returned `None` above — unreachable
                // here, but we provide a stable token for defense.
                _ => "typed_other",
            };
            tracing::warn!(
                provider = %provider_name,
                error = %e,
                cooldown_secs = cd.as_secs(),
                reason = reason,
                "route failed (typed), attempting next"
            );
            self.set_cooldown(provider_name, cd);
            return Some(reason);
        }

        // Untyped error: legacy string-based classification.
        let class = crate::agent::error_classify::classify(e);
        let cd = crate::agent::error_classify::cooldown_duration(&class).min(route_cooldown);
        tracing::warn!(
            provider = %provider_name,
            error = %e,
            error_class = ?class,
            cooldown_secs = cd.as_secs(),
            "route failed (untyped), attempting next"
        );
        if !cd.is_zero() {
            self.set_cooldown(provider_name, cd);
        }
        Some("untyped")
    }

    /// Record a failover transition. Called at the point where the router
    /// has decided the current route failed with a failover-worthy error
    /// and is about to attempt `to_key`. Internally looks up the
    /// process-wide `MetricsRegistry` via `metrics::global()` and is a
    /// no-op if none has been installed (e.g. in unit tests).
    fn record_failover(from_key: &str, to_key: &str, reason: &str) {
        if let Some(metrics) = crate::metrics::global() {
            metrics.record_llm_failover(from_key, to_key, reason);
        }
    }

    /// Get all route entries that could serve as fallbacks (not on cooldown, not excluded).
    fn available_fallbacks(&self, exclude_key: &str) -> Vec<&RouteEntry> {
        self.routes
            .iter()
            .filter(|e| e.key != exclude_key && !self.is_on_cooldown(&e.key))
            .collect()
    }

    /// Test-only constructor for `RoutingProvider` — builds a routing chain from
    /// a list of `(key, provider, cooldown_secs)` tuples without going through
    /// `build_provider` / DB resolution. Used by unit tests for the failover
    /// predicate wiring. Defaults `max_failover_attempts` to a large value
    /// (`u32::MAX`) so existing tests exercise the full route list; use
    /// `new_for_test_with_cap` to verify the cap behavior explicitly.
    ///
    /// Every entry is installed with condition `"default"` so `select_route`
    /// matches on the first one (same behavior the production `always`
    /// condition would give for a single-route chain).
    #[cfg(test)]
    pub(crate) fn new_for_test(routes: Vec<(String, Arc<dyn LlmProvider>, u64)>) -> Self {
        Self::new_for_test_with_cap(routes, u32::MAX)
    }

    /// Test-only constructor that lets a test set an explicit
    /// `max_failover_attempts` cap (see `new_for_test` docs).
    #[cfg(test)]
    pub(crate) fn new_for_test_with_cap(
        routes: Vec<(String, Arc<dyn LlmProvider>, u64)>,
        max_failover_attempts: u32,
    ) -> Self {
        let entries = routes
            .into_iter()
            .map(|(key, provider, cooldown_secs)| RouteEntry {
                condition: "default".to_string(),
                key,
                provider,
                cooldown_duration: std::time::Duration::from_secs(cooldown_secs.max(1)),
            })
            .collect();
        Self {
            routes: entries,
            cooldowns: std::sync::Mutex::new(std::collections::HashMap::new()),
            max_failover_attempts,
        }
    }
}

// ── Keyword sets for semantic routing ─────────────────────────────────────────

const FINANCIAL_KEYWORDS: &[&str] = &[
    // Russian
    "портфель", "акции", "бумаги", "дивиденды", "доходность", "прибыль", "убыток",
    "imoex", "ртс", "мосбиржа", "moex", "облигации", "фонд", "etf", "паи",
    "котировки", "инвестиц", "брокер", "позиции", "активы", "тикер",
    // English
    "portfolio", "shares", "dividend", "yield", "return", "profit", "loss",
    "stock", "bond", "equity", "ticker", "market",
];

const ANALYTICAL_KEYWORDS: &[&str] = &[
    // Russian
    "анализируй", "подсчитай", "посчитай", "вычисли", "рассчитай", "сравни",
    "корреляция", "среднее", "медиана", "статистика", "динамика", "тренд",
    "процент", "прогноз", "агрегируй", "сгруппируй",
    // English
    "analyze", "calculate", "compute", "correlation", "average", "median",
    "statistics", "trend", "forecast", "aggregate",
];

const CODE_KEYWORDS: &[&str] = &[
    // Russian
    "скрипт", "код", "запусти", "выполни", "python", "bash",
    "напиши скрипт", "напиши код",
    // English
    "script", "code", "execute", "run script", "run code",
];

pub(super) fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|kw| text.contains(kw))
}

// ── RoutingProvider LlmProvider impl ─────────────────────────────────────────
// NOTE: chat() and chat_stream() have identical routing/fallback logic but
// cannot be unified into a generic helper due to async closure lifetime issues
// with trait objects. The duplication is intentional and kept in sync.

#[async_trait::async_trait]
impl LlmProvider for RoutingProvider {
    async fn chat(
        &self,
        messages: &[opex_types::Message],
        tools: &[opex_types::ToolDefinition],
        opts: CallOptions,
    ) -> Result<opex_types::LlmResponse> {
        let primary = self.select_route(messages, tools)?;
        let primary_key = primary.key.clone();
        let primary_display = primary.provider.name().to_string();
        let primary_cooldown = primary.cooldown_duration;

        let primary_skipped = self.is_on_cooldown(&primary_key);
        // Most recent failover reason carried from the previous failed
        // route to the next attempt — populated by `handle_provider_error`.
        // The key of the most recent failed route (source of the next
        // failover transition). Starts as `primary_key`; the first
        // transition is recorded as either "primary on cooldown → fallback"
        // or "primary_failed_error → fallback".
        let mut pending_reason: Option<&'static str>;
        let mut last_failed_key = primary_key.clone();

        if primary_skipped {
            tracing::debug!(provider = %primary_display, "primary on cooldown, skipping");
            pending_reason = Some("cooldown");
        } else {
            match primary.provider.chat(messages, tools, opts.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => match self.handle_provider_error(&e, &primary_key, primary_cooldown) {
                    None => {
                        // Non-failover-worthy: bubble up with `partial_state` intact.
                        return Err(e);
                    }
                    Some(reason) => {
                        pending_reason = Some(reason);
                    }
                },
            }
        }

        // `pending_reason` is consumed on the next loop iteration; the last
        // iteration's reassignment is by design (dead-store is cheap and
        // keeps the loop body uniform). Silence the unused_assignments lint
        // for the final-iteration dead store on error.
        //
        // Issue #9: enforce `max_failover_attempts` cap — stop iterating
        // once we've attempted N fallbacks, even if more routes remain.
        // `enumerate` is `usize`; we compare against the u32 cap by casting.
        #[allow(unused_assignments)]
        {
            let fallbacks = self.available_fallbacks(&primary_key);
            for (idx, fb) in fallbacks.into_iter().enumerate() {
                if idx as u32 >= self.max_failover_attempts {
                    tracing::warn!(
                        attempts = idx as u32,
                        cap = self.max_failover_attempts,
                        "failover cap reached — not trying further routes"
                    );
                    break;
                }
                // Record failover counter at the transition point.
                if let Some(reason) = pending_reason.take() {
                    Self::record_failover(&last_failed_key, &fb.key, reason);
                }
                tracing::info!(provider = %fb.provider.name(), "trying fallback provider");
                match fb.provider.chat(messages, tools, opts.clone()).await {
                    Ok(mut resp) => {
                        let reason = if primary_skipped { "cooldown" } else { "primary_failed" };
                        resp.fallback_notice = Some(format!("↪️ {} → {} ({})", primary_display, fb.provider.name(), reason));
                        return Ok(resp);
                    }
                    Err(e) => {
                        match self.handle_provider_error(&e, &fb.key, fb.cooldown_duration) {
                            None => return Err(e),
                            Some(reason) => {
                                pending_reason = Some(reason);
                                last_failed_key = fb.key.clone();
                            }
                        }
                    }
                }
            }
        }
        anyhow::bail!("all providers failed (including fallbacks)")
    }

    async fn chat_stream(
        &self,
        messages: &[opex_types::Message],
        tools: &[opex_types::ToolDefinition],
        chunk_tx: tokio::sync::mpsc::Sender<String>,
        opts: CallOptions,
    ) -> Result<opex_types::LlmResponse> {
        let primary = self.select_route(messages, tools)?;
        let primary_key = primary.key.clone();
        let primary_display = primary.provider.name().to_string();
        let primary_cooldown = primary.cooldown_duration;

        let primary_skipped = self.is_on_cooldown(&primary_key);
        let mut pending_reason: Option<&'static str>;
        let mut last_failed_key = primary_key.clone();

        if primary_skipped {
            tracing::debug!(provider = %primary_display, "primary on cooldown, skipping for streaming");
            pending_reason = Some("cooldown");
        } else {
            use std::sync::atomic::{AtomicBool, Ordering};
            let chunks_sent = Arc::new(AtomicBool::new(false));
            let (tracking_tx, mut tracking_rx) = tokio::sync::mpsc::channel::<String>(1024);
            let forwarder = {
                let sentinel = chunks_sent.clone();
                let forward_tx = chunk_tx.clone();
                tokio::spawn(async move {
                    while let Some(chunk) = tracking_rx.recv().await {
                        sentinel.store(true, Ordering::Relaxed);
                        forward_tx.send(chunk).await.ok();
                    }
                })
            };

            match primary.provider.chat_stream(messages, tools, tracking_tx, opts.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    // tracking_tx is now consumed/dropped by the call above.
                    // Wait for the forwarder to drain any buffered chunks before
                    // reading chunks_sent — this eliminates the race condition.
                    let _ = forwarder.await;

                    if chunks_sent.load(Ordering::Relaxed) {
                        // Issue #6: mid-stream failure cannot fail over (user
                        // already received partial content), but the primary
                        // still deserves a cooldown + metric bump so we don't
                        // re-hammer it on the next request. Swallow the
                        // returned "failover reason" — we're not actually
                        // failing over.
                        let _ = self.handle_provider_error(&e, &primary_key, primary_cooldown);
                        tracing::warn!(provider = %primary_display, error = %e,
                            "streaming: mid-stream failure, partial output already sent — cooldown applied, not failing over");
                        return Err(e);
                    }
                    // Downcast to typed error; non-failover-worthy errors bubble up
                    // (preserving `partial_state`); failover-worthy errors apply
                    // cooldown and fall through to the fallback chain.
                    match self.handle_provider_error(&e, &primary_key, primary_cooldown) {
                        None => return Err(e),
                        Some(reason) => {
                            pending_reason = Some(reason);
                        }
                    }
                    tracing::warn!(provider = %primary_display,
                        "streaming: primary failed before first chunk, trying fallback chain");
                }
            }
        }

        // See the `chat` impl for the rationale behind the
        // `unused_assignments` allow — the last iteration's reassignment
        // on error is dead-store by design.
        //
        // Issue #9: enforce `max_failover_attempts` cap, same as `chat()`.
        #[allow(unused_assignments)]
        {
            let fallbacks = self.available_fallbacks(&primary_key);
            for (idx, fb) in fallbacks.into_iter().enumerate() {
                if idx as u32 >= self.max_failover_attempts {
                    tracing::warn!(
                        attempts = idx as u32,
                        cap = self.max_failover_attempts,
                        "streaming failover cap reached — not trying further routes"
                    );
                    break;
                }
                if let Some(reason) = pending_reason.take() {
                    Self::record_failover(&last_failed_key, &fb.key, reason);
                }
                tracing::info!(provider = %fb.provider.name(), "trying streaming fallback provider");
                // F063: mirror the primary path's chunks_sent guard. Without it,
                // a fallback that streamed partial content then failed mid-stream
                // would fail over AGAIN to the next fallback and double-stream to
                // the same sink (the user sees A's partial output + B's full
                // output concatenated).
                use std::sync::atomic::{AtomicBool, Ordering};
                let chunks_sent = Arc::new(AtomicBool::new(false));
                let (tracking_tx, mut tracking_rx) = tokio::sync::mpsc::channel::<String>(1024);
                let forwarder = {
                    let sentinel = chunks_sent.clone();
                    let forward_tx = chunk_tx.clone();
                    tokio::spawn(async move {
                        while let Some(chunk) = tracking_rx.recv().await {
                            sentinel.store(true, Ordering::Relaxed);
                            forward_tx.send(chunk).await.ok();
                        }
                    })
                };
                match fb.provider.chat_stream(messages, tools, tracking_tx, opts.clone()).await {
                    Ok(mut resp) => {
                        let reason = if primary_skipped { "cooldown" } else { "primary_failed" };
                        resp.fallback_notice = Some(format!("↪️ {} → {} ({})", primary_display, fb.provider.name(), reason));
                        return Ok(resp);
                    }
                    Err(e) => {
                        // Drain the forwarder before reading the sentinel.
                        let _ = forwarder.await;
                        if chunks_sent.load(Ordering::Relaxed) {
                            let _ = self.handle_provider_error(&e, &fb.key, fb.cooldown_duration);
                            tracing::warn!(provider = %fb.provider.name(), error = %e,
                                "streaming fallback: mid-stream failure, partial output already sent — not failing over further");
                            return Err(e);
                        }
                        match self.handle_provider_error(&e, &fb.key, fb.cooldown_duration) {
                            None => return Err(e),
                            Some(reason) => {
                                pending_reason = Some(reason);
                                last_failed_key = fb.key.clone();
                            }
                        }
                    }
                }
            }
        }
        anyhow::bail!("all streaming providers failed (including fallbacks)")
    }

    fn name(&self) -> &'static str {
        "routing"
    }

    fn set_model_override(&self, model: Option<String>) {
        for entry in &self.routes {
            entry.provider.set_model_override(model.clone());
        }
    }

    fn current_model(&self) -> String {
        self.routes
            .first().map_or_else(|| "unknown".to_string(), |e| e.provider.current_model())
    }
}
