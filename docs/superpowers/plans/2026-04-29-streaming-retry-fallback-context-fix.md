# Streaming Retry, Fallback Cleanup & Context Warning — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three bugs in the LLM provider layer: add retry to `chat_stream()`, remove a stale fallback provider from Arty's config, add a large-context diagnostic warning.

**Architecture:** Add `SendError` typed enum + `send_with_retry()` primitive to `providers_http.rs`; refactor `retry_http_post_custom()` to delegate to it; replace the bare `req.send().await` block in `chat_stream()` with `send_with_retry()`; add context-size `tracing::warn!` in both `chat()` and `chat_stream()`; PATCH the Pi API to clear Arty's stale fallback.

**Tech Stack:** Rust, `reqwest`, `wiremock 0.6.5` (already in dev-dependencies), `tokio::time::pause()` for fast retry tests.

---

## File Map

| File | Change |
|---|---|
| `crates/hydeclaw-core/src/agent/providers_http.rs` | Add `SendError`, `send_with_retry()`; refactor `retry_http_post_custom()`; add `#[cfg(test)]` module |
| `crates/hydeclaw-core/src/agent/providers_openai.rs` | Replace send block in `chat_stream()`; add context warning in `chat()` and `chat_stream()` |
| Pi API (runtime) | PATCH `/api/agents/Arty` → `fallback_provider: null` |

---

## Task 1 — Add `SendError` and write failing tests for `send_with_retry`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers_http.rs`

- [ ] **Step 1: Add `SendError` enum at the bottom of `providers_http.rs` (before the `#[allow(dead_code)]` section)**

  Open `crates/hydeclaw-core/src/agent/providers_http.rs` and append after line 155 (after `RETRYABLE_ANTHROPIC`):

  ```rust
  /// Typed error returned by [`send_with_retry`].
  #[derive(Debug)]
  pub enum SendError {
      /// Non-2xx HTTP response after all retry attempts, or a 400 (no retry).
      Http { status: u16, body: String },
      /// Network / connection failure after all retry attempts.
      Network(reqwest::Error),
  }

  impl std::fmt::Display for SendError {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
          match self {
              SendError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
              SendError::Network(e) => write!(f, "network error: {e}"),
          }
      }
  }

  impl std::error::Error for SendError {
      fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
          match self {
              SendError::Network(e) => Some(e),
              SendError::Http { .. } => None,
          }
      }
  }
  ```

- [ ] **Step 2: Add a failing stub for `send_with_retry`** (tests will call this)

  Append immediately after the `SendError` impl block:

  ```rust
  /// Low-level HTTP POST with exponential backoff retry.
  ///
  /// Returns the raw `reqwest::Response` on success (body not yet consumed).
  /// Callers decide how to read the body (text for non-streaming, stream for SSE).
  ///
  /// Error classification:
  /// - 400 → logged + `SendError::Http { status: 400 }` immediately (no retry)
  /// - retryable codes (e.g. 429/500/502/503) → retried up to `max_retries - 1` times
  /// - other non-2xx → `SendError::Http` immediately
  /// - network error → retried; final attempt → `SendError::Network`
  pub async fn send_with_retry(
      _client: &reqwest::Client,
      _url: &str,
      _body: &serde_json::Value,
      _provider_name: &str,
      _retryable_codes: &[u16],
      _customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
  ) -> Result<reqwest::Response, SendError> {
      unimplemented!("send_with_retry not yet implemented")
  }
  ```

- [ ] **Step 3: Add test module with three failing tests**

  Append at the end of `providers_http.rs`:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use wiremock::matchers::{method, path};
      use wiremock::{Mock, MockServer, ResponseTemplate};

      /// 503 twice → 200: send_with_retry should succeed on the third attempt.
      /// Uses tokio time-pause so backoff sleeps are instant.
      #[tokio::test(start_paused = true)]
      async fn send_with_retry_retries_503_and_succeeds() {
          let server = MockServer::start().await;
          Mock::given(method("POST"))
              .and(path("/v1/chat/completions"))
              .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
              .up_to_n_times(2)
              .mount(&server)
              .await;
          Mock::given(method("POST"))
              .and(path("/v1/chat/completions"))
              .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
              .mount(&server)
              .await;

          let client = reqwest::Client::new();
          let url = format!("{}/v1/chat/completions", server.uri());
          let result = send_with_retry(
              &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, |r| r,
          ).await;
          assert!(result.is_ok(), "expected Ok after retry, got {:?}", result.err());
          assert_eq!(server.received_requests().await.unwrap().len(), 3);
      }

      /// 503 three times (all retries exhausted): should return SendError::Http { status: 503 }.
      #[tokio::test(start_paused = true)]
      async fn send_with_retry_fails_after_all_503s() {
          let server = MockServer::start().await;
          Mock::given(method("POST"))
              .and(path("/v1/chat/completions"))
              .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
              .mount(&server)
              .await;

          let client = reqwest::Client::new();
          let url = format!("{}/v1/chat/completions", server.uri());
          let result = send_with_retry(
              &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, |r| r,
          ).await;
          assert!(
              matches!(result, Err(SendError::Http { status: 503, .. })),
              "expected Http(503), got {:?}", result
          );
          assert_eq!(server.received_requests().await.unwrap().len(), 3);
      }

      /// 400 should not be retried and returns immediately.
      #[tokio::test]
      async fn send_with_retry_no_retry_on_400() {
          let server = MockServer::start().await;
          Mock::given(method("POST"))
              .and(path("/v1/chat/completions"))
              .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
              .mount(&server)
              .await;

          let client = reqwest::Client::new();
          let url = format!("{}/v1/chat/completions", server.uri());
          let result = send_with_retry(
              &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, |r| r,
          ).await;
          assert!(
              matches!(result, Err(SendError::Http { status: 400, .. })),
              "expected Http(400), got {:?}", result
          );
          // exactly 1 request — no retry on 400
          assert_eq!(server.received_requests().await.unwrap().len(), 1);
      }
  }
  ```

- [ ] **Step 4: Verify tests fail (stub panics)**

  ```
  cargo test -p hydeclaw-core send_with_retry -- --nocapture
  ```

  Expected: all three tests FAIL with `not yet implemented`.

- [ ] **Step 5: Commit the failing tests + stub**

  ```bash
  git add crates/hydeclaw-core/src/agent/providers_http.rs
  git commit -m "test(providers_http): add failing tests for send_with_retry"
  ```

---

## Task 2 — Implement `send_with_retry`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers_http.rs`

- [ ] **Step 1: Replace the stub with the real implementation**

  Find and replace the entire `send_with_retry` stub function body:

  ```rust
  pub async fn send_with_retry(
      client: &reqwest::Client,
      url: &str,
      body: &serde_json::Value,
      provider_name: &str,
      retryable_codes: &[u16],
      mut customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
  ) -> Result<reqwest::Response, SendError> {
      let policy = BackoffPolicy::default();

      for attempt in 0..policy.max_retries {
          let start = std::time::Instant::now();
          let req = customize(client.post(url).json(body));

          match req.send().await {
              Ok(resp) => {
                  let status = resp.status();
                  tracing::info!(
                      provider = %provider_name,
                      status = %status,
                      elapsed_ms = start.elapsed().as_millis() as u64,
                      attempt,
                      "LLM API responded"
                  );

                  if status.is_success() {
                      return Ok(resp);
                  }

                  let code = status.as_u16();
                  let err_text = resp.text().await.unwrap_or_default();

                  if code == 400 {
                      let body_preview = serde_json::to_string(body).unwrap_or_default();
                      let mut end = body_preview.len().min(4000);
                      while end > 0 && !body_preview.is_char_boundary(end) { end -= 1; }
                      tracing::error!(
                          provider = %provider_name,
                          request_body = %&body_preview[..end],
                          "400 Bad Request — dumping request body for diagnosis"
                      );
                      return Err(SendError::Http { status: code, body: err_text });
                  }

                  if !retryable_codes.contains(&code) || attempt == policy.max_retries - 1 {
                      return Err(SendError::Http { status: code, body: err_text });
                  }

                  let backoff = policy.delay(attempt);
                  tracing::warn!(
                      provider = %provider_name,
                      status = %status,
                      attempt,
                      backoff_ms = backoff.as_millis() as u64,
                      "retrying LLM request"
                  );
                  tokio::time::sleep(backoff).await;
              }
              Err(e) => {
                  tracing::warn!(
                      provider = %provider_name,
                      error = %e,
                      attempt,
                      "LLM request failed"
                  );

                  if attempt == policy.max_retries - 1 {
                      return Err(SendError::Network(e));
                  }

                  tokio::time::sleep(policy.delay(attempt)).await;
              }
          }
      }

      unreachable!("loop always returns on the final attempt")
  }
  ```

- [ ] **Step 2: Run tests — all three must pass**

  ```
  cargo test -p hydeclaw-core send_with_retry -- --nocapture
  ```

  Expected output:
  ```
  test tests::send_with_retry_retries_503_and_succeeds ... ok
  test tests::send_with_retry_fails_after_all_503s ... ok
  test tests::send_with_retry_no_retry_on_400 ... ok
  ```

- [ ] **Step 3: Run full check**

  ```
  make check
  ```

  Expected: no errors.

- [ ] **Step 4: Commit**

  ```bash
  git add crates/hydeclaw-core/src/agent/providers_http.rs
  git commit -m "feat(providers_http): add SendError + send_with_retry with exponential backoff"
  ```

---

## Task 3 — Refactor `retry_http_post_custom` to delegate to `send_with_retry`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers_http.rs` lines 63–149

- [ ] **Step 1: Replace the body of `retry_http_post_custom`**

  The function signature stays identical — only the body changes.
  Replace everything inside `retry_http_post_custom` (the loop and the error handling) with:

  ```rust
  pub async fn retry_http_post_custom(
      client: &reqwest::Client,
      url: &str,
      body: &serde_json::Value,
      provider_name: &str,
      retryable_codes: &[u16],
      customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
  ) -> Result<String> {
      let resp = send_with_retry(client, url, body, provider_name, retryable_codes, customize)
          .await
          .map_err(|e| match e {
              SendError::Http { status, body: b } =>
                  anyhow::anyhow!("{provider_name} API error {status}: {b}"),
              SendError::Network(e) =>
                  anyhow::anyhow!("{provider_name} request error: {e}"),
          })?;
      Ok(resp.text().await?)
  }
  ```

- [ ] **Step 2: Run `make check` — no compilation errors**

  ```
  make check
  ```

- [ ] **Step 3: Run all tests — nothing regressed**

  ```
  make test
  ```

  Expected: all tests pass.

- [ ] **Step 4: Commit**

  ```bash
  git add crates/hydeclaw-core/src/agent/providers_http.rs
  git commit -m "refactor(providers_http): retry_http_post_custom delegates to send_with_retry"
  ```

---

## Task 4 — Update `chat_stream()` to use `send_with_retry`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers_openai.rs` lines 430–486

- [ ] **Step 1: Add `SendError` to the import in `providers_openai.rs`**

  Line 4 currently reads:
  ```rust
  use super::{async_trait, Deserialize, Arc, SecretsManager, ModelOverride, LlmProvider, Message, ToolDefinition, Result, LlmResponse, messages_to_openai_format, mpsc};
  ```

  Add `LlmCallError` import. Find the import of `LlmCallError` — it is referenced in `chat_stream` via `LlmCallError::AuthError` etc. Search for where it's imported:

  ```bash
  grep -n "LlmCallError\|use super::" crates/hydeclaw-core/src/agent/providers_openai.rs | head -10
  ```

  Then add `use crate::agent::providers_http::SendError;` near the top of the file (after existing `use` statements, before the struct definition).

- [ ] **Step 2: Replace the send block in `chat_stream()` (lines 430–486)**

  Find this block in `chat_stream()`:

  ```rust
  let start = std::time::Instant::now();
  let api_key = self.resolve_api_key().await;
  let effective_url = self.resolve_url().await;
  let mut req = self.streaming_client.post(&effective_url).json(&body);
  if !api_key.is_empty() {
      req = req.bearer_auth(&api_key);
  }
  let resp = match req.send().await {
      Ok(r) => r,
      Err(e) => {
          return Err(anyhow::Error::new(super::classify_reqwest_err(
              e,
              &self.provider_name,
              self.timeouts.connect_secs,
              self.timeouts.request_secs,
          )));
      }
  };

  if !resp.status().is_success() {
      let status = resp.status();
      let code = status.as_u16();
      let retry_after = resp.headers()
          .get("retry-after")
          .and_then(|v| v.to_str().ok())
          .map(std::string::ToString::to_string);
      let err_text = resp.text().await.unwrap_or_default();
      if code == 400 {
          let body_preview = serde_json::to_string(&body).unwrap_or_default();
          let mut end = body_preview.len().min(4000);
          while end > 0 && !body_preview.is_char_boundary(end) { end -= 1; }
          let truncated = &body_preview[..end];
          tracing::error!(
              provider = %self.provider_name,
              request_body = %truncated,
              "400 Bad Request (stream) — dumping request body for diagnosis"
          );
      }
      // Typed classification for response status errors: feeds
      // RoutingProvider failover + AuthError cooldown floor.
      if code == 401 || code == 403 {
          return Err(anyhow::Error::new(LlmCallError::AuthError {
              provider: self.provider_name.clone(),
              status: code,
          }));
      }
      if code >= 500 {
          return Err(anyhow::Error::new(LlmCallError::Server5xx {
              provider: self.provider_name.clone(),
              status: code,
          }));
      }
      if let Some(ra) = retry_after {
          anyhow::bail!("{} API error (retry-after: {}): {}", self.provider_name, ra, err_text);
      }
      anyhow::bail!("{} API error: {}", self.provider_name, err_text);
  }
  ```

  Replace with:

  ```rust
  let start = std::time::Instant::now();
  let api_key = self.resolve_api_key().await;
  let effective_url = self.resolve_url().await;
  let api_key_clone = api_key.clone();
  let resp = crate::agent::providers_http::send_with_retry(
      &self.streaming_client,
      &effective_url,
      &body,
      &self.provider_name,
      crate::agent::providers_http::RETRYABLE_OPENAI,
      move |req| if api_key_clone.is_empty() { req } else { req.bearer_auth(&api_key_clone) },
  )
  .await
  .map_err(|e| match e {
      SendError::Http { status, .. } if status == 401 || status == 403 =>
          anyhow::Error::new(LlmCallError::AuthError {
              provider: self.provider_name.clone(),
              status,
          }),
      SendError::Http { status, .. } =>
          anyhow::Error::new(LlmCallError::Server5xx {
              provider: self.provider_name.clone(),
              status,
          }),
      SendError::Network(e) =>
          anyhow::Error::new(super::classify_reqwest_err(
              e,
              &self.provider_name,
              self.timeouts.connect_secs,
              self.timeouts.request_secs,
          )),
  })?;
  ```

  Note: the `start` variable stays — it's used later in `chat_stream` for elapsed time logging.

- [ ] **Step 3: Run `make check`**

  ```
  make check
  ```

  Expected: no errors. If `LlmCallError` is not in scope, add the missing import.

- [ ] **Step 4: Run all tests**

  ```
  make test
  ```

  Expected: all tests pass.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/hydeclaw-core/src/agent/providers_openai.rs
  git commit -m "fix(providers_openai): chat_stream retries 5xx via send_with_retry"
  ```

---

## Task 5 — Add context size warning

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers_openai.rs`

The warning goes in **two places**: after the `tracing::info!` log and before key resolution in both `chat()` and `chat_stream()`.

- [ ] **Step 1: Add warning to `chat()` (non-streaming)**

  In `chat()`, after the `tracing::info!` block (line ~244, ending with `"calling LLM API"`), insert before `let api_key = self.resolve_api_key().await;`:

  ```rust
  const LARGE_CONTEXT_CHARS: usize = 200_000;
  let ctx_chars: usize = messages.iter().map(|m| {
      m.content.len()
          + m.tool_calls.as_deref().unwrap_or(&[]).iter()
              .map(|tc| tc.arguments.to_string().len())
              .sum::<usize>()
  }).sum();
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

- [ ] **Step 2: Add warning to `chat_stream()` (streaming)**

  In `chat_stream()`, after the `tracing::info!` block (line ~428, ending with `"calling LLM API (streaming)"`), insert before `let start = std::time::Instant::now();`:

  ```rust
  const LARGE_CONTEXT_CHARS: usize = 200_000;
  let ctx_chars: usize = messages.iter().map(|m| {
      m.content.len()
          + m.tool_calls.as_deref().unwrap_or(&[]).iter()
              .map(|tc| tc.arguments.to_string().len())
              .sum::<usize>()
  }).sum();
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

  Note: `LARGE_CONTEXT_CHARS` is defined in both function bodies — Rust allows local `const` items inside functions.

- [ ] **Step 3: Run `make check`**

  ```
  make check
  ```

  Expected: no errors.

- [ ] **Step 4: Run all tests**

  ```
  make test
  ```

  Expected: all tests pass.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/hydeclaw-core/src/agent/providers_openai.rs
  git commit -m "feat(providers_openai): warn on large LLM context (>200K chars)"
  ```

---

## Task 6 — Deploy and clear stale fallback config

- [ ] **Step 1: Build for ARM64 and deploy**

  ```
  make deploy-binary
  ```

  Expected: binary uploaded to Pi, service restarted.

- [ ] **Step 2: Verify service is up**

  ```
  make doctor
  ```

  Expected: `{"status":"ok"}` or equivalent health response.

- [ ] **Step 3: PATCH Arty's config to remove stale fallback**

  ```bash
  TOKEN=$(grep HYDECLAW_AUTH_TOKEN .deploy.env 2>/dev/null || ssh aronmav@192.168.1.85 'grep HYDECLAW_AUTH_TOKEN ~/hydeclaw/.env | cut -d= -f2 | tr -d "\" "')
  curl -s -X PATCH http://192.168.1.85:18789/api/agents/Arty \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"fallback_provider": null}'
  ```

  Expected: JSON response with `"fallback_provider": null`.

- [ ] **Step 4: Verify the config was cleared**

  ```bash
  curl -s http://192.168.1.85:18789/api/agents/Arty \
    -H "Authorization: Bearer $TOKEN" | python3 -c "import json,sys; d=json.load(sys.stdin); print('fallback_provider:', d.get('fallback_provider'))"
  ```

  Expected: `fallback_provider: None`

---

## Self-Review Notes

- **Spec coverage:** All three bugs covered. Bug #1 → Tasks 1–4. Bug #2 → Task 6. Bug #3 → Task 5. ✅
- **TDD:** Tests written before implementation (Task 1 tests, Task 2 implements). ✅
- **`LARGE_CONTEXT_CHARS` duplication:** Defined inside each function body as a local `const` — valid Rust. No module-level constant needed since it's used in exactly two local blocks. ✅
- **`start` variable in `chat_stream`:** Preserved from original position — used later in the SSE parsing loop for elapsed-time logging. ✅
- **`retry_after` header removed:** The `retry-after` header from 429 responses was previously surfaced in the error message but never used to delay retries. After refactor, 429 is retried automatically by `send_with_retry`; the header info is no longer in the error message but this is acceptable. ✅
- **`SendError` import in `providers_openai.rs`:** Task 4 Step 1 explicitly handles finding and adding the import. ✅
