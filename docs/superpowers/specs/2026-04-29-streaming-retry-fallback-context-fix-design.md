# Streaming Retry, Fallback Cleanup & Context Warning — Design

**Date:** 2026-04-29  
**Scope:** Three targeted fixes in the LLM provider layer  
**Motivation:** Session `e8680a68` failed with `ollama: server returned 503` because streaming requests have no retry logic; the fallback provider `gemini-cli` referenced a non-existent entry; 46 KB of tool results were sent with no diagnostics.

---

## Background

Investigation of a failed session revealed three distinct issues:

1. `chat_stream()` in `providers_openai.rs` performs a single `req.send().await` with no retry on 5xx. `chat()` uses `retry_http_post()` which retries 503 three times. The asymmetry means any transient Ollama Cloud outage permanently fails a streaming session instead of recovering.

2. `Arty` agent config has `fallback_provider = "gemini-cli"` but no such provider exists in the DB. `create_fallback_provider()` logs a `warn` and returns `None`, so after `max_consecutive_failures` retries the session fails with no fallback.

3. No pre-call context size measurement. The 46 KB tool result is sent blindly; a resulting 503 is indistinguishable from a transient server error.

---

## Bug #1 — Streaming Retry

### Problem

`chat_stream()` (`providers_openai.rs`):

```rust
let resp = match req.send().await {
    Ok(r) => r,
    Err(e) => return Err(...),
};
if !resp.status().is_success() {
    // → immediate Server5xx, no retry
}
```

`chat()` retries via `retry_http_post()` → `retry_http_post_custom()`. `chat_stream()` has no equivalent.

### Fix

**New primitive in `providers_http.rs`:**

```rust
pub async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    provider_name: &str,
    retryable_codes: &[u16],
    mut customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<reqwest::Response>
```

Implements the same backoff loop as `retry_http_post_custom()`:
- `BackoffPolicy::default()` — 3 attempts, base 1 s, factor 3.0, max 30 s, jitter 500 ms
- 400 → log body dump, no retry (same as today)
- Code in `retryable_codes` → sleep + retry
- Network error → retry
- Final attempt fails → `bail!`
- Returns `reqwest::Response` on success (body not consumed)

**`retry_http_post_custom()` becomes a thin wrapper:**

```rust
pub async fn retry_http_post_custom(...) -> Result<String> {
    let resp = send_with_retry(client, url, body, provider_name, retryable_codes, customize).await?;
    Ok(resp.text().await?)
}
```

`retry_http_post()` is unchanged (calls `_custom`).

**`send_with_retry()` — typed error variant**

`send_with_retry` returns `Result<reqwest::Response, SendError>` where:

```rust
pub enum SendError {
    Http { status: u16, body: String },  // non-2xx after all retries
    Network(anyhow::Error),              // reqwest/connection error
}
```

`retry_http_post_custom` maps `SendError` → `anyhow::Error` (backward compat unchanged). `chat_stream` maps `SendError` → the correct `LlmCallError` variant, preserving the existing 401/403 → `AuthError` classification:

```rust
let resp = crate::agent::providers_http::send_with_retry(
    &self.streaming_client, &effective_url, &body, &self.provider_name,
    crate::agent::providers_http::RETRYABLE_OPENAI,
    move |req| if api_key_clone.is_empty() { req } else { req.bearer_auth(&api_key_clone) },
).await
.map_err(|e| match e {
    SendError::Http { status, .. } if status == 401 || status == 403 =>
        anyhow::Error::new(LlmCallError::AuthError { provider: self.provider_name.clone(), status }),
    SendError::Http { status, .. } if status >= 500 =>
        anyhow::Error::new(LlmCallError::Server5xx { provider: self.provider_name.clone(), status }),
    SendError::Http { status, body } =>
        anyhow::anyhow!("{} API error {}: {}", self.provider_name, status, body),
    SendError::Network(e) =>
        anyhow::Error::new(super::classify_reqwest_err_from(e, &self.provider_name, ...)),
})?;
```

The existing error-classification block in `chat_stream` is removed — `send_with_retry` + the match above replaces it.

### Files Changed

- `crates/hydeclaw-core/src/agent/providers_http.rs` — add `send_with_retry()`; slim down `retry_http_post_custom()` to delegate to it
- `crates/hydeclaw-core/src/agent/providers_openai.rs` — replace send block in `chat_stream()`

---

## Bug #2 — Remove Stale Fallback Provider

### Problem

`config/agents/Arty.toml` (on Pi) has `fallback_provider = "gemini-cli"`. No CLI providers are used in this deployment; `gemini-cli` is not registered in the `providers` table. The engine silently falls through to session failure.

### Fix

**Config change only — no code changes needed.**

PATCH the provider field to null via the API after deploying:

```
PATCH /api/agents/Arty
{ "fallback_provider": null }
```

No code change needed: the existing `tracing::warn!` at `create_fallback_provider()` L100–105 already covers the `Ok(None)` case.

---

## Bug #3 — Large Context Warning

### Problem

No measurement of total context size before an LLM call. A provider 503 caused by an oversized request is indistinguishable from a transient outage.

### Fix

Add a warning at the top of both `chat()` and `chat_stream()` in `providers_openai.rs`, after the body is built but before sending:

```rust
const LARGE_CONTEXT_CHARS: usize = 200_000; // ~50K tokens

let ctx_chars: usize = messages.iter().map(|m| m.content.len()).sum();
if ctx_chars > LARGE_CONTEXT_CHARS {
    tracing::warn!(
        provider = %self.provider_name,
        model = %self.model,
        context_chars = ctx_chars,
        threshold = LARGE_CONTEXT_CHARS,
        "large context being sent to LLM — provider may reject with 5xx or truncate silently"
    );
}
```

No truncation. No user-visible error. Purely diagnostic — surfaces the root cause in logs when a provider rejects with an opaque 5xx.

---

## Testing

### Bug #1
- Unit test in `providers_http.rs`: mock server returns 503 on first two attempts, 200 on third → `send_with_retry` returns Ok with the response
- Unit test: mock returns 503 three times → returns `Err`
- Existing `providers_openai.rs` and provider integration tests must continue to pass

### Bug #2
- Post-deploy: `GET /api/agents/Arty` → `fallback_provider` field is null

### Bug #3
- Manual: send a message with >200K chars of context → `tracing::warn!` appears in logs

---

## Rollout

1. Implement and test locally (`make check`, `make test`)
2. Deploy binary to Pi (`make deploy-binary`)
3. PATCH Arty config via API (`fallback_provider: null`)
4. Verify via `make doctor`
