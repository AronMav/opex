# Closing reliability gaps from the audit — design

**Date:** 2026-05-14
**Status:** approved (brainstorming)
**Scope:** two features bundled because both close phantom-feature gaps surfaced by the 2026-05-14 reliability audit. They live in different subsystems (watchdog binary vs. YAML-tool runtime) but share the same theme: deliver behaviour that the docs / data structures already advertise.

## Summary

**Part A — Watchdog agent inactivity alerts.** CLAUDE.md and `docs/ARCHITECTURE.md` both claim the watchdog "monitors agent inactivity". Reality: it only monitors managed-service health (HTTP `/health` checks), system resources (disk/RAM/CPU), and stuck sessions (sessions in `running` past a stale threshold). Nothing watches an *agent* (the Hyde / Alma / Arty config) for "has this agent done anything in the last N hours?" Add two new alert types — `stale_activity` (no recent activity at all) and `missed_heartbeat` (heartbeat-configured agent whose cron didn't fire) — bolted on as a sibling check inside the existing `hydeclaw-watchdog` binary, kept HTTP-only (no DB driver added to the watchdog crate).

**Part B — YAML tool response cache.** `YamlToolDef.cache: Option<YamlCacheConfig>` parses from YAML (with `ttl` and `key_params`) and is exposed through the operator-facing schema. The whole runtime — `ToolExecutionContext`, `get_cached`, `set_cached`, `build_cache_key`, plus the `CachedResponse` struct — exists but is gated by `#[cfg(test)]`, so it's compiled only into test binaries. Operators who set `cache: { ttl: 300 }` on a YAML tool get nothing. Promote the cache runtime to production, wire it into the YAML-tool dispatch path in `agent/engine_dispatch.rs`, place a shared `ToolExecutionContext` in `InfraServices`.

Acceptance: an agent that goes silent past the configured threshold produces exactly one Telegram alert and exactly one recovery alert when it comes back; a YAML tool with `cache: { ttl: 60 }` calling a wiremock endpoint twice with identical params fires the mock once.

## Background

### The audit findings these features close

The 2026-05-14 reliability audit (see `docs/superpowers/specs/2026-05-14-...` from earlier in the day) flagged 14 phantom features. Twelve were resolved by deletions; **#7 (prompt_cache routing)** was implemented in commit `fcdfda76`. Two remained:

- **#13 watchdog "agent inactivity":** documented as a feature, no implementation.
- **YAML cache:** documented (in the YAML schema operators write) and partially implemented (test-only), no production runtime.

The user explicitly authorised both during the 2026-05-14 brainstorming session.

### Why one spec instead of two

The two features are *conceptually* independent but the user grouped them under one design pass. Both are small enough to fit one spec without scope drift. Each Part is self-contained — the implementation plan can ship them as two PRs in any order.

## Part A — Watchdog agent inactivity alerts

### A.1 Architecture: watchdog stays HTTP-only

The watchdog binary has **no database driver** (`crates/hydeclaw-watchdog/Cargo.toml` carries only `tokio`, `reqwest`, `serde`, `chrono`, `tracing`, `sd-notify`). Every health signal it reads today comes via HTTP from core (`/health` of services, `/api/watchdog/settings`, `/api/sessions/stuck`). Episode state for existing alerts (`was_down`, `was_resource_warning`, `was_container_unhealthy`) lives in `HashMap<String, bool>` in `main.rs`.

This spec preserves that pattern. We add:

- One new HTTP endpoint in core: `GET /api/watchdog/agent-activity`
- One new module in the watchdog: `crates/hydeclaw-watchdog/src/inactivity.rs`
- One new `HashMap<EpisodeKey, AlertState>` in watchdog `main.rs` for episode dedup

**No new DB table.** Episode state stays in-memory; on watchdog restart, currently-inactive agents will re-fire a single alert. Watchdog rarely restarts (managed by systemd), so the cost is one false-positive per restart — acceptable.

### A.2 New core endpoint: `GET /api/watchdog/agent-activity`

Returns the data the watchdog needs to compute inactivity without touching the DB itself.

```json
[
  {
    "agent_id": "Hyde",
    "enabled": true,
    "latest_activity_at": "2026-05-14T10:14:00Z",
    "heartbeat_cron": "0 * * * *",
    "heartbeat_timezone": "Europe/Samara",
    "last_heartbeat_at": "2026-05-14T10:00:00Z"
  },
  {
    "agent_id": "Alma",
    "enabled": true,
    "latest_activity_at": "2026-05-13T18:42:00Z",
    "heartbeat_cron": null,
    "heartbeat_timezone": null,
    "last_heartbeat_at": null
  }
]
```

Where:

- `latest_activity_at` = `SELECT MAX(GREATEST(activity_at, last_message_at)) FROM sessions WHERE agent_id = $1`. Covers any session activity (user messages, channel messages, heartbeats, cron-triggered runs). Heartbeat is a session like any other (`channel = 'heartbeat'`, see `agent/channel_kind.rs:4`), so its activity naturally bumps this value.
- `heartbeat_cron` / `heartbeat_timezone` = mirror the agent's `[agent.heartbeat] cron / timezone` TOML fields. The schedule comes from the agent's config (in-memory after startup), not from the `scheduled_jobs` DB table.
- `last_heartbeat_at` = `SELECT MAX(started_at) FROM sessions WHERE agent_id = $1 AND channel = 'heartbeat'`. NULL if heartbeat never fired (agent freshly added).

The endpoint is added under `gateway/handlers/monitoring/` (next to the existing `/api/doctor` and `/api/dashboard` handlers) and authenticated by the same Bearer-token middleware. Returns 200 on success, 500 on DB error.

**Why this split:** core owns the data (agent registry, sessions table); watchdog owns the *policy* (thresholds + episode state). Keeps the watchdog dumb and offline-from-DB.

### A.3 Watchdog config

Two new fields in `WatchdogSettings` in `crates/hydeclaw-watchdog/src/config.rs`:

```rust
#[serde(default = "default_stale_activity_timeout_hours")]
pub stale_activity_timeout_hours: u64,   // default 6
#[serde(default = "default_missed_heartbeat_grace_minutes")]
pub missed_heartbeat_grace_minutes: u64, // default 10
```

Operator-visible only when they want to override:

```toml
[watchdog]
stale_activity_timeout_hours = 6
missed_heartbeat_grace_minutes = 10
```

### A.4 Watchdog `inactivity.rs` module

```rust
//! Per-agent inactivity checks (stale activity, missed heartbeat).
//! Polls GET /api/watchdog/agent-activity, applies thresholds, manages
//! in-memory episode state, fires alerts on transitions.

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub(crate) enum AlertType {
    StaleActivity,
    MissedHeartbeat,
}

#[derive(Debug, Clone)]
pub(crate) struct AlertState {
    pub fired_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) type EpisodeKey = (String /* agent_id */, AlertType);

#[derive(Debug, serde::Deserialize)]
pub(crate) struct AgentActivity {
    pub agent_id: String,
    pub enabled: bool,
    pub latest_activity_at: Option<chrono::DateTime<chrono::Utc>>,
    pub heartbeat_cron: Option<String>,
    pub heartbeat_timezone: Option<String>,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) async fn fetch_agent_activity(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
) -> Result<Vec<AgentActivity>>;

pub(crate) fn next_cron_after(
    cron_expr: &str,
    timezone: &str,           // e.g. "Europe/Samara"
    after: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>>;

/// Pure transition function — easy to unit-test.
pub(crate) fn classify(
    agent: &AgentActivity,
    now: chrono::DateTime<chrono::Utc>,
    stale_threshold: chrono::Duration,
    heartbeat_grace: chrono::Duration,
) -> Vec<AlertType>;

/// Mutate episode state based on classify() output, emit alert/recover actions.
pub(crate) fn reconcile(
    classified: HashMap<String, Vec<AlertType>>,
    state: &mut HashMap<EpisodeKey, AlertState>,
    now: chrono::DateTime<chrono::Utc>,
) -> (Vec<Fire>, Vec<Recover>);

pub(crate) async fn tick(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
    cfg: &WatchdogSettings,
    state: &mut HashMap<EpisodeKey, AlertState>,
    alerter: &Alerter,
) -> Result<()>;
```

### A.5 Classification logic

Given `agent` (from the endpoint) and `now`:

- **`StaleActivity`**: fires when `agent.enabled && agent.latest_activity_at < now - stale_threshold`. Special case: `latest_activity_at == None` (agent never had any session) — *do not fire*. Fresh agents are not stale.
- **`MissedHeartbeat`**: fires when all of:
  - `agent.enabled == true`
  - `agent.heartbeat_cron.is_some()`
  - The next expected fire time, computed as `next_cron_after(cron_expr, timezone, last_heartbeat_at.unwrap_or(epoch))`, is past `now - heartbeat_grace`.

  Cron parsing uses the `cron = "0.13"` workspace dep (already pulled in by the scheduler). Timezone handling: parse via `chrono-tz` (already in tree via `chrono`'s tz feature path — verify during impl, add if needed).

Both checks are *independent*. An agent can fire one, the other, both, or neither. Both go through the same dedup machinery — `reconcile` keys by `(agent_id, AlertType)`.

### A.6 Episode dedup & recovery

`HashMap<EpisodeKey, AlertState>` in `main.rs`, allocated alongside the existing `was_down` / `was_resource_warning` maps. `reconcile`:

- For each `(agent, alert_type)` returned by `classify` as currently-firing:
  - If `state.get(&key).is_none()` → emit `Fire` event, insert `AlertState { fired_at: now }`.
  - Otherwise → no-op (already alerted, episode ongoing).
- For each existing key in `state` whose agent did **not** return that `alert_type` from `classify`:
  - Emit `Recover` event, remove from `state`.

`Fire` and `Recover` are translated into channel-notify HTTP calls by the existing `alerter.rs`. Message format:

- Fire: `"agent {name} inactive (no activity for {hours}h, last seen {iso})"` for `StaleActivity`; `"agent {name} missed heartbeat (expected at {iso}, {minutes}m overdue)"` for `MissedHeartbeat`.
- Recover: `"agent {name} recovered ({alert_type})"`.

### A.7 Integration into `main.rs`

Add to the existing watchdog loop after `resources::tick(...)`:

```rust
let mut inactivity_state: HashMap<inactivity::EpisodeKey, inactivity::AlertState> = HashMap::new();

loop {
    // ... existing checks ...

    if let Err(e) = inactivity::tick(
        &http_client,
        &cfg.core_url,
        &cfg.auth_token,
        &cfg.watchdog,
        &mut inactivity_state,
        &alerter,
    ).await {
        tracing::warn!(error = %e, "inactivity tick failed");
    }

    tokio::time::sleep(Duration::from_secs(cfg.watchdog.interval_secs)).await;
}
```

Failure inside `tick` (e.g. core unreachable) is logged and skipped — it must not crash the watchdog process. Subsequent ticks retry.

## Part B — YAML tool response cache

### B.1 Activation strategy

Existing test-only code becomes production code:

- `CachedResponse` struct — drop `#[cfg(test)]`
- `ToolExecutionContext` struct — drop `#[cfg(test)]`, swap internal `tokio::sync::Mutex<HashMap<...>>` for `dashmap::DashMap<...>` so reads are lock-free under concurrency
- `get_cached`, `set_cached`, `build_cache_key` — drop `#[cfg(test)]`

The two existing `#[cfg(test)]` cache tests (`execution_context_cache_basic` plus any companions) move into the regular `mod tests` block — they verify the same code path as the new production behaviour.

### B.2 Shared placement

Add `pub tool_exec_ctx: Arc<ToolExecutionContext>` to `InfraServices` (`crates/hydeclaw-core/src/gateway/clusters/infra_services.rs`), constructed once at startup with the operator-configured `max_entries`. `engine_dispatch.rs` reaches it through the engine's `app_state.infra.tool_exec_ctx`.

`InfraServices` is the right home — it already holds shared, process-wide infrastructure (`db: PgPool`, `memory_store`, `embedder`, `sandbox`, `process_manager`).

### B.3 Config

New optional section in `hydeclaw.toml`:

```toml
[tools.cache]
max_entries = 1000   # soft cap before eviction
```

If absent, defaults apply. Per-tool TTL stays in the YAML file (`cache: { ttl: 300, key_params: ["query"] }`).

Rust struct: nest inside an existing `ToolsConfig` if one exists, otherwise add a new top-level field on `AppConfig`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ToolCacheConfig {
    #[serde(default = "default_tool_cache_max_entries")]
    pub max_entries: usize,
}
fn default_tool_cache_max_entries() -> usize { 1000 }
```

### B.4 Cache key shape

`build_cache_key(tool_name, method, endpoint, params, key_params) -> String` produces:

```text
{tool_name}|{method}|{endpoint}|k1=v1&k2=v2…
```

- `tool_name` first so collisions across renamed tools are visible in `tracing::debug!`.
- `method` + `endpoint` included so two YAML tools with the same name but different URLs never share a key.
- Params: if `key_params` is empty, all params from the LLM call sorted alphabetically; otherwise only the listed ones. Values serialised as `serde_json::Value::to_string()` to handle nested objects/arrays deterministically (note: `to_string()` already produces sorted-object form for `Map` since JSON doesn't guarantee object key order — document this caveat, accept rare false-positive misses for nested arguments).

### B.5 Dispatch integration

In `crates/hydeclaw-core/src/agent/engine_dispatch.rs::execute_tool_call_inner` (~line 213, immediately before the `client = ...` selection):

```rust
// YAML tool cache — pre-execution lookup.
let cache_key = match &yaml_tool.cache {
    Some(cfg) if yaml_tool.channel_action.is_none() && yaml_tool.pagination.is_none() => {
        Some(build_cache_key(
            &yaml_tool.name,
            &yaml_tool.method,
            &yaml_tool.endpoint,
            &arguments,
            &cfg.key_params,
        ))
    }
    _ => None,
};

if let Some(ref key) = cache_key {
    if let Some(body) = self.app_state.infra.tool_exec_ctx.get_cached(key).await {
        tracing::debug!(tool = %yaml_tool.name, "yaml tool cache hit");
        return Ok(body);
    }
}
```

After the HTTP execution returns successfully (and only on 2xx status):

```rust
if let (Some(ref key), Some(ref cfg)) = (cache_key.as_ref(), yaml_tool.cache.as_ref()) {
    self.app_state.infra.tool_exec_ctx.set_cached(key, &body, cfg.ttl).await;
}
```

Three explicit non-caching paths (encoded in the `Some(cfg) if ...` guard above):

1. `yaml_tool.channel_action.is_some()` — binary responses routed to channels, not LLM-context; cache is useless.
2. `yaml_tool.pagination.is_some()` — paginated tools auto-fetch additional pages mid-execution; responses are not idempotent without full pagination state.
3. HTTP non-2xx response — never cached. Error bodies should retry on next call. (Implemented by gating the `set_cached` call on the success branch of the existing HTTP-result match in `engine_dispatch.rs`.)

### B.6 Eviction policy

`set_cached` enforces the soft cap:

```rust
pub async fn set_cached(&self, key: &str, body: &str, ttl_secs: u64) {
    if self.cache.len() >= self.max_entries {
        // Soft eviction: remove the N oldest by expires_at.
        // Simple O(n) walk — acceptable for max_entries ≤ 10_000.
        let target_remove = self.max_entries / 10;          // 10 % of cap
        let mut victims: Vec<(String, std::time::Instant)> = self.cache
            .iter()
            .map(|e| (e.key().clone(), e.value().expires_at))
            .collect();
        victims.sort_by_key(|(_, exp)| *exp);
        for (k, _) in victims.into_iter().take(target_remove) {
            self.cache.remove(&k);
        }
    }
    self.cache.insert(key.to_string(), CachedResponse {
        body: body.to_string(),
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(ttl_secs),
    });
}
```

Why "remove 10 % when at cap" instead of "remove one":

- One-at-a-time eviction triggers the O(n) sort on every write once full — death by amortisation.
- Removing a batch (10 %) gives breathing room — next 100 writes are O(1) inserts before sort happens again.

No LRU bookkeeping (no per-access timestamp updates). The proxy "oldest expiration time" approximates LRU well enough for our access patterns (uniform TTLs across same-tool calls).

### B.7 Lazy TTL

`get_cached` removes expired entries on the read path:

```rust
pub async fn get_cached(&self, key: &str) -> Option<String> {
    let entry = self.cache.get(key)?;
    if std::time::Instant::now() >= entry.expires_at {
        drop(entry);                  // release the ref before remove
        self.cache.remove(key);
        return None;
    }
    Some(entry.body.clone())
}
```

No background sweeper. Untouched expired entries linger until eviction kicks in or the process restarts. Memory cost: per-entry size ≈ `body.len() + key.len() + ~64 bytes overhead`. With 1 000-entry cap and typical body 2 KB, worst case ≈ 2 MB — negligible on Pi RAM budget.

## Tests

### Watchdog tests (Part A)

**Unit (`crates/hydeclaw-watchdog/src/inactivity.rs`)**

- `next_cron_after_basic`: cron `0 * * * *`, tz `Europe/Samara`, last_fire `10:00 UTC` → `11:00 UTC` (with tz shift verified).
- `classify_stale_activity_triggers`: agent with `latest_activity_at < now - 6h` → returns `[StaleActivity]`.
- `classify_stale_activity_respects_enabled_false`: disabled agent → empty.
- `classify_stale_activity_skips_never_active`: `latest_activity_at = None` → empty (fresh agent).
- `classify_missed_heartbeat_triggers`: `heartbeat_cron = "0 * * * *"`, `last_heartbeat_at = now - 90min`, grace 10min → returns `[MissedHeartbeat]`.
- `classify_missed_heartbeat_respects_grace`: same setup with `last_heartbeat_at = now - 30min` → empty (next fire not yet expected).
- `classify_no_heartbeat_cron_no_alert`: `heartbeat_cron = None` → empty for that alert type.
- `reconcile_fires_once`: first call with `[StaleActivity]` → 1 Fire emitted, state populated. Second call same input → 0 emits.
- `reconcile_recovers_on_resolution`: state has `StaleActivity` open, classified returns empty → 1 Recover emitted, state cleared.
- `reconcile_independent_alert_types`: state has only `StaleActivity` open, classified returns `[StaleActivity, MissedHeartbeat]` → 0 Fire for stale (already open), 1 Fire for missed.

**Integration (`crates/hydeclaw-watchdog/tests/integration_inactivity.rs`)**

- Use `wiremock` to mock `GET /api/watchdog/agent-activity` and `POST /api/channels/notify`. Verify a full tick → expected number of notifies for various agent shapes.
- `tick_skips_disabled_agents`: mock returns 3 agents, 2 enabled-and-stale, 1 disabled-but-stale → 2 fire notifies.
- `tick_handles_endpoint_down`: mock returns 500 → tick returns Ok(()), no notifies, error logged.

**Endpoint test (`crates/hydeclaw-core/tests/integration_watchdog_agent_activity.rs`)**

- `#[sqlx::test]` — insert agents, sessions (some heartbeat, some not), call the endpoint, assert correct `latest_activity_at` and `last_heartbeat_at` aggregation.
- `endpoint_requires_auth`: no Bearer → 401.

### YAML cache tests (Part B)

**Unit (in `tools/yaml_tools.rs` `mod tests`)**

- Migrate `execution_context_cache_basic` from `#[cfg(test)]`-only path to verify post-promotion path still works.
- `cache_key_includes_method_and_endpoint`: same tool name, different endpoint → distinct keys.
- `cache_key_respects_key_params`: `key_params=["query"]`, change `irrelevant` param → same key.
- `cache_key_empty_key_params_uses_all`: same key only if all params equal.
- `eviction_at_soft_cap`: fill to `max_entries`, write one more → ~10 % of oldest entries evicted, total len < `max_entries`.
- `lazy_ttl_returns_none_on_expired`: insert with `ttl_secs = 0`, sleep 1 ms (or use mock clock), `get_cached` returns `None` and removes entry.

**Integration (`crates/hydeclaw-core/tests/integration_yaml_cache.rs` with `wiremock`)**

- `cache_hit_skips_http_call`: `wiremock::MockServer` expecting `1` call, YAML tool with `cache: { ttl: 60 }`, invoke twice with identical args → mock received exactly 1 request, second invocation logs `"yaml tool cache hit"`.
- `cache_miss_on_distinct_args`: same tool, different `query` param both times → mock receives 2 calls.
- `channel_action_bypasses_cache`: tool with `channel_action: {...}` and `cache: {...}` — both invocations hit the mock.
- `pagination_bypasses_cache`: tool with `pagination: {...}` and `cache: {...}` — both invocations hit the mock.
- `non_2xx_not_cached`: mock returns 500 once then 200 — second invocation gets the 200 (not the cached 500).

## Acceptance criteria

### Part A

1. With `[watchdog].stale_activity_timeout_hours = 1` and an agent whose newest session activity is older than 1 h, the watchdog fires exactly one alert via `POST /api/channels/notify` per episode.
2. When that agent's `activity_at` is bumped (any new session or message), the watchdog fires exactly one recovery alert and clears the episode.
3. An agent with `enabled = false` (config TOML) is never alerted on, regardless of activity timestamps.
4. An agent with `[agent.heartbeat] cron = "0 * * * *"` whose `last_heartbeat_at` is older than (next_cron_after + grace) produces a `MissedHeartbeat` alert independent of whether `StaleActivity` is also firing.
5. `GET /api/watchdog/agent-activity` returns 200 + valid JSON with the documented shape, given a valid Bearer token; returns 401 without one.
6. Watchdog tolerates core being unreachable (returns 500 / connection refused on the endpoint) — logs a warn, continues looping, does not crash.
7. `cargo test --workspace --lib` and `make test-db` are green; `cargo test -p hydeclaw-watchdog --tests` runs the integration tests above.

### Part B

1. YAML tool with `cache: { ttl: 60 }` called twice with identical arguments within 60 s — second call returns from cache (no HTTP request, log line `"yaml tool cache hit"` emitted).
2. Same tool with `cache: { ttl: 60 }`, two calls 70 s apart — second call hits HTTP again (entry expired and lazily removed).
3. YAML tool **without** `cache` field — behaves byte-identically to today. No cache lookup attempt visible in logs.
4. Tool with `channel_action: {...}` and `cache: {...}` — cache logic skipped, every invocation hits HTTP.
5. Tool whose HTTP response is non-2xx — never cached. The next call with same args hits HTTP again.
6. With `[tools.cache] max_entries = 100` and 200 distinct cache keys written within 60 s, the cache size stays ≤ 100 (modulo the batch-eviction overshoot of ~10 %).
7. `cargo build --workspace --all-targets` is green; the migrated and new tests pass.

## Implementation order

A natural breakdown into independent commits, each shippable:

1. **Core endpoint** (Part A foundation): add `GET /api/watchdog/agent-activity` handler + `#[sqlx::test]` integration test. Watchdog still doesn't use it yet.
2. **Watchdog inactivity module**: add `inactivity.rs` with the pure-logic functions + unit tests. Not yet wired into `main.rs`.
3. **Watchdog integration**: add the two config fields, wire `inactivity::tick(...)` into `main.rs` loop, add the wiremock integration test.
4. **YAML cache promotion**: drop `#[cfg(test)]`, add `ToolExecutionContext` to `InfraServices`, add `[tools.cache]` config section. No dispatch integration yet — verify everything still compiles.
5. **YAML cache dispatch**: integrate `get_cached` / `set_cached` calls into `engine_dispatch.rs::execute_tool_call_inner`. Wire wiremock integration test.

Steps 1–3 are Part A end-to-end. Steps 4–5 are Part B end-to-end. They can ship in either order.

## Out of scope (deferred)

| Not in this spec | Future home |
|---|---|
| Per-agent inactivity thresholds (e.g., Hyde 1 h, Alma 24 h) | Optional `[agent.watchdog] stale_activity_timeout_hours` field added later if the global threshold ever proves wrong for one specific agent |
| `scheduled_jobs` monitoring (user-created dynamic crons that stop firing) | Separate spec — different data source, different operator expectations |
| Heartbeat-grace derived automatically from `cron_expr` cadence | Only if the global 10-min grace turns out to be too loose for fast-cadence heartbeats |
| Watchdog DB pool (direct access instead of polling core) | Considered and rejected: drift from current HTTP-only model, no real value when the endpoint is one extra hop on a local socket |
| YAML cache: per-agent isolation | Wait until a real incident shows Hyde's cache fooling Alma |
| YAML cache: manual invalidation API (`POST /api/tools/{name}/cache/clear`) | Add when operators ask |
| YAML cache: Prometheus / OTEL hit/miss metrics | After the project gains a general metrics-exporter story |
| YAML cache: background sweeper | Add only on observed memory-pressure incidents |
| YAML cache: negative caching (cache 4xx with short TTL) | Add when external API rate-limiting forces our hand |

## Known risks and mitigations

1. **Cron parser edge cases.** `chrono-tz` and the `cron` crate occasionally disagree on DST transitions. Mitigation: the unit tests pin a known cron + tz combination across a DST boundary; if mismatch surfaces, document the behaviour rather than fight the libraries.
2. **Watchdog ↔ core auth.** The endpoint needs a Bearer token. The watchdog already reads `HYDECLAW_AUTH_TOKEN` from its env (via `cfg.auth_token`); confirm and reuse.
3. **Cache-key collision on nested params.** `Value::to_string()` on objects gives a JSON repr, but key order inside nested objects is technically not stable in `serde_json` (it depends on `Map` impl, which is `BTreeMap` by default in this crate — verify at impl time; if the dependency switched to `IndexMap` for preserve-order, sort manually).
4. **Episode state lost on watchdog restart.** Acceptable: one false-positive re-fire per restart. Watchdog restarts are rare and human-triggered.
5. **Cache poisoning across agents.** Shared cache means agent A's cached response is served to agent B for the same `(tool, method, endpoint, params)` key. This is *intentional* — same HTTP call should return the same body regardless of which agent asked. The cache is keyed by API call shape, not by agent identity. If an external API ever returns per-caller-different bodies based on something other than the request URL/params (e.g., session cookies), we'd need to extend the key, but no such tool exists today.
