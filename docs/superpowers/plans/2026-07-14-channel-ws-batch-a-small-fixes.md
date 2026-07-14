# Channel WS Audit Fixes — Batch A (small fixes) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the five small, mutually-independent channel-WS audit fixes (#2 subscribe-leak, #4 writer-wedge, #5 clarify owner-gate, #6 handshake guard, #7 connected_channels dedup) with no concurrency redesign.

**Architecture:** Five surgical edits across four files in `crates/opex-core/src/gateway/handlers/channel_ws/`. Each fix is independently testable and carries no shared state with the others. #2 and #7 both key off the "first Ready on this connection" signal (`action_install_tx.is_some()`) and land together in one handshake task; #4/#5/#6 are one-file edits each.

**Tech Stack:** Rust 2024, tokio, axum WS, `futures_util::Sink`, `dashmap`. Tests are `#[tokio::test]` / `#[test]` inline modules. rustls-tls only — no new deps.

## Global Constraints

- Rust + rustls-tls only — never add OpenSSL or any new external dependency for these fixes.
- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/` (parallel uncommitted user work).
- Do NOT push; do NOT deploy — the controller runs the server test session + deploy after review, on explicit user approval.
- Windows dev host cannot reliably run the Rust test suite — authority for test results is the server (`~/opex-src`, throttled `CARGO_BUILD_JOBS=4 nice ionice`). Local `cargo check`/`clippy` only.
- The reader's "never awaits engine work" invariant must not regress: none of these edits may add an inline engine/DB `.await` to the reader hot path. (#6's guard is a sync string compare.)
- Preserve existing behaviour exactly outside the named defect: no drive-by refactors.
- Source spec: `docs/superpowers/specs/2026-07-14-channel-ws-audit-fixes-design.md` §4.

---

### Task 1: Handshake — subscribe under first-Ready guard (#2) + connected_channels dedup (#7)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/handshake.rs` (`handle_ready`, lines ~41–110)
- Test: same file, new `#[cfg(test)]` module at end

**Interfaces:**
- Consumes: `handle_ready(ctx, engine, agent_name, out_tx, adapter_type, version, formatting_prompt, state, action_install_tx: &mut Option<oneshot::Sender<ActionForwarderInit>>, outbound_ids)` — unchanged signature.
- Produces: no new public items. Behaviour change only: `router.subscribe()` + `channel_conn_id` assignment + `connected_channels` push all happen ONLY on the first `Ready`; a second `Ready` updates `last_activity` and logs, touching nothing else.

**Background (read before editing):**
Today `handle_ready` (1) always pushes a `connected_channels` row (lines 78–91), then (2) always calls `router.subscribe()` and assigns `state.channel_conn_id` (lines 99–101), and only THEN conditionally hands the receiver to the forwarder via `action_install_tx.take()` (line 102). On a duplicate `Ready` this registers a dead second subscription and overwrites `channel_conn_id`, orphaning the first (defect #2), and pushes a duplicate `connected_channels` row (defect #7).

The fix keys both on a single "first Ready" signal. `action_install_tx` is `Some` on entry and is `take()`n on the first Ready (the router always exists for a channel WS — `mod.rs:67` rejects agents without one), so `action_install_tx.is_some()` at the top of `handle_ready` is a reliable first-Ready predicate.

- [ ] **Step 1: Write the failing test**

Add at the very end of `handshake.rs`. The test drives the two behaviours structurally via a small helper that mimics the first-Ready decision, because `handle_ready` itself needs a full `CwsCtx`/DB. We assert the decision predicate and the dedup helper in isolation, matching how the other channel_ws unit tests stay DB-free.

First extract the dedup step into a testable free function (added in Step 3); the test targets it:

```rust
#[cfg(test)]
mod ready_guard_tests {
    use super::*;
    use crate::gateway::state::ConnectedChannel;

    fn chan(agent: &str, ctype: &str) -> ConnectedChannel {
        let now = chrono::Utc::now();
        ConnectedChannel {
            agent_name: agent.to_string(),
            channel_id: None,
            channel_type: ctype.to_string(),
            display_name: format!("{agent}/{ctype}"),
            adapter_version: "test".to_string(),
            connected_at: now,
            last_activity: now,
        }
    }

    #[test]
    fn first_ready_pushes_row() {
        let mut chans: Vec<ConnectedChannel> = vec![];
        upsert_connected_channel(&mut chans, /*is_first_ready=*/ true, chan("Arty", "telegram"));
        assert_eq!(chans.len(), 1, "first Ready must push a row");
    }

    #[test]
    fn repeat_ready_does_not_duplicate_row() {
        let mut chans: Vec<ConnectedChannel> = vec![chan("Arty", "telegram")];
        let before = chans[0].last_activity;
        std::thread::sleep(std::time::Duration::from_millis(2));
        upsert_connected_channel(&mut chans, /*is_first_ready=*/ false, chan("Arty", "telegram"));
        assert_eq!(chans.len(), 1, "repeat Ready must not push a duplicate row");
        assert!(chans[0].last_activity > before, "repeat Ready must bump last_activity");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core ready_guard -- --nocapture`
Expected: FAIL to compile — `upsert_connected_channel` not defined.

- [ ] **Step 3: Add the dedup helper**

Add this free function in `handshake.rs` (above `handle_ready`):

```rust
/// Push a `connected_channels` entry only on the first `Ready` of a
/// connection; on a repeated `Ready`, update the matching row's
/// `last_activity` instead of duplicating it (defect #7). Pure so it is
/// unit-testable without a DB.
pub(super) fn upsert_connected_channel(
    chans: &mut Vec<crate::gateway::state::ConnectedChannel>,
    is_first_ready: bool,
    entry: crate::gateway::state::ConnectedChannel,
) {
    if is_first_ready {
        chans.push(entry);
        return;
    }
    if let Some(existing) = chans
        .iter_mut()
        .find(|c| c.agent_name == entry.agent_name && c.channel_type == entry.channel_type)
    {
        existing.last_activity = entry.last_activity;
    } else {
        // No prior row despite not-first-Ready (e.g. evicted) — push to stay consistent.
        chans.push(entry);
    }
}
```

- [ ] **Step 4: Rewire `handle_ready` to use the first-Ready signal**

At the TOP of `handle_ready`, right after the `tracing::info!` block (after line ~46), capture the signal BEFORE anything consumes it:

```rust
    // First `Ready` on this connection? (`action_install_tx` is taken on the
    // first Ready; the router always exists for a channel WS.) Drives both the
    // subscribe-once guard (#2) and connected_channels dedup (#7).
    let is_first_ready = action_install_tx.is_some();
```

Replace the `connected_channels` push block (current lines 78–91) with:

```rust
    // Register / refresh in connected_channels (dedup on repeat Ready — #7).
    {
        let now = chrono::Utc::now();
        let entry = crate::gateway::state::ConnectedChannel {
            agent_name: agent_name.to_string(),
            channel_id: ch_id,
            channel_type: state.channel_type.clone(),
            display_name: ch_display,
            adapter_version: version,
            connected_at: now,
            last_activity: now,
        };
        upsert_connected_channel(&mut ctx.bus.connected_channels.write().await, is_first_ready, entry);
    }
```

Replace the subscribe block (current lines 97–110) with — `subscribe()` now happens INSIDE the `take()` guard so a second Ready subscribes nothing and leaves `channel_conn_id` untouched (#2):

```rust
    // Subscribe to the channel action router and hand off the receiver to the
    // action-forwarder — ONLY on the first Ready. A duplicate Ready must not
    // register a second (dead) subscription or overwrite channel_conn_id (#2).
    if let Some(ref router) = engine.state().channel_router {
        if let Some(tx) = action_install_tx.take() {
            let (id, rx) = router.subscribe(&state.channel_type).await;
            state.channel_conn_id = Some(id);
            let _ = tx.send(ActionForwarderInit {
                channel_type: state.channel_type.clone(),
                channel_action_rx: rx,
            });
        } else {
            tracing::warn!(%agent_name, "Ready received twice on same WS — subscribe skipped");
        }
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core ready_guard -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 6: Local check + clippy**

Run: `cargo check -p opex-core --bin opex-core` then `cargo clippy -p opex-core --bin opex-core -- -D warnings`
Expected: clean (0 warnings on touched code).

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/handshake.rs
git commit -m "fix(channel-ws): subscribe + connected_channels only on first Ready (#2,#7)"
```

---

### Task 2: Writer — bounded write-timeout (#4)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/writer.rs` (`run`, lines 21–43; tests module)
- Test: same file, `#[cfg(test)]` module

**Interfaces:**
- Consumes: `writer::run<S>(sink: S, rx: mpsc::Receiver<OutboundMsg>)` where `S: Sink<WsMessage> + Unpin, S::Error: Display` — signature unchanged.
- Produces: a module const `WRITE_TIMEOUT: Duration` (45s prod). On a write that does not complete within `WRITE_TIMEOUT`, the writer logs and returns (exits), which drops `rx` and triggers the existing reader-teardown path.

**Background:** `writer::run`'s `sink.send(...).await` has no timeout. A stuck-but-open adapter blocks it → the bounded 256-slot `out_tx` fills → the reader blocks on its own `out_tx.send()` → stops reading inbound → ActionResult/Cancel freeze; the wedge never self-heals (defect #4). Wrapping each send in `tokio::time::timeout` converts the indefinite wedge into a bounded "close after WRITE_TIMEOUT of no write progress." 45s is chosen above the existing 30s app-level ping and the 20s tool-action grace, so a healthy-but-idle adapter is never killed mid-cycle.

- [ ] **Step 1: Write the failing test**

The existing `CaptureSink` always returns `Poll::Ready` and cannot simulate a stuck adapter — add a NEW sink that never completes a send. Append to the `tests` module in `writer.rs`:

```rust
    /// A sink whose `poll_ready` never resolves — simulates a stuck-but-open
    /// adapter. `tokio::time::timeout`'s own timer fires regardless of whether
    /// this registers a waker, so the writer still exits.
    struct StuckSink;

    impl Sink<WsMessage> for StuckSink {
        type Error = std::convert::Infallible;
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
        fn start_send(self: std::pin::Pin<&mut Self>, _item: WsMessage) -> Result<(), Self::Error> {
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn writer_exits_on_stuck_sink_after_timeout() {
        let (tx, rx) = mpsc::channel::<OutboundMsg>(4);
        let h = tokio::spawn(run(StuckSink, rx));
        tx.send(OutboundMsg::Wire(ChannelOutbound::Chunk {
            request_id: "r".to_string(),
            text: "stuck".to_string(),
        }))
        .await
        .unwrap();
        // Advance virtual time past WRITE_TIMEOUT; the writer must return.
        tokio::time::advance(WRITE_TIMEOUT + std::time::Duration::from_secs(1)).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), h)
            .await
            .expect("writer must exit after WRITE_TIMEOUT")
            .unwrap();
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core writer_exits_on_stuck_sink -- --nocapture`
Expected: FAIL to compile — `WRITE_TIMEOUT` not defined.

- [ ] **Step 3: Add the const + wrap both sends**

At the top of `writer.rs` (after the `use` lines, before `run`):

```rust
/// Max time a single WS write may take before the writer gives up and exits,
/// tearing down the connection. Chosen above the 30s app-level ping and the
/// 20s tool-action grace so a healthy-but-idle adapter is never killed (#4).
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
```

Replace the `Wire` arm (lines 28–33) with:

```rust
            OutboundMsg::Wire(payload) => {
                match tokio::time::timeout(WRITE_TIMEOUT, sink.send(ws_json(&payload))).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "channel WS writer: sink send failed, exiting");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!("channel WS writer: send timed out ({WRITE_TIMEOUT:?}), exiting — adapter stuck");
                        return;
                    }
                }
            }
```

Replace the `Ping` arm (lines 34–39) with:

```rust
            OutboundMsg::Ping => {
                match tokio::time::timeout(
                    WRITE_TIMEOUT,
                    sink.send(WsMessage::Ping(vec![1, 2, 3, 4].into())),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "channel WS writer: ping send failed, exiting");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!("channel WS writer: ping timed out ({WRITE_TIMEOUT:?}), exiting — adapter stuck");
                        return;
                    }
                }
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core --  writer -- --nocapture`
(Runs all four writer tests: the three existing + the new stuck-sink test.)
Expected: PASS (4 tests) — the existing `CaptureSink` tests still pass because `timeout` around an immediately-ready send resolves instantly.

- [ ] **Step 5: Local check + clippy**

Run: `cargo check -p opex-core --bin opex-core` then `cargo clippy -p opex-core --bin opex-core -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/writer.rs
git commit -m "fix(channel-ws): bound writer sink.send with WRITE_TIMEOUT to break wedge (#4)"
```

---

### Task 3: clarify-text owner-gate (#5)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` (`handle_clarify_text`, lines 651–743)
- Test: same file, extend an existing or add a `#[cfg(test)]` module

**Interfaces:**
- Consumes: `handle_clarify_text(ctx, engine, agent_name, channel_type, request_id, msg, out_tx) -> bool` — signature unchanged.
- Produces: behaviour change only — a non-owner's plain text does NOT resolve a pending clarify. It returns `false` (falls through to a normal turn) instead of resolving the owner-directed clarify.

**Background:** `handle_clarify_callback` (inline.rs:576–588) reads `ctx.auth.access_guards`, computes `is_owner`, and rejects non-owners before resolving. `handle_clarify_text` has NO such gate (defect #5): under `per-chat`/`shared` dm_scope any allowed non-owner member's next message resolves a clarify the agent directed at the owner. We add the same owner check, but — per spec §4.3 — a non-owner falls through to a normal turn (`return false`), rather than consuming with an error frame (which is what the callback path does for its button-press case).

- [ ] **Step 1: Write the failing test**

`handle_clarify_text` needs a full `CwsCtx`, so a pure unit test can't drive it end-to-end. Instead assert the owner-gate decision via a small pure helper we extract (Step 3). Add:

```rust
#[cfg(test)]
mod clarify_text_owner_gate_tests {
    use super::clarify_text_is_owner_allowed;

    #[test]
    fn owner_allowed() {
        assert!(clarify_text_is_owner_allowed(true), "owner may resolve clarify text");
    }

    #[test]
    fn non_owner_falls_through() {
        assert!(!clarify_text_is_owner_allowed(false), "non-owner must not resolve — falls through to a turn");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core clarify_text_owner_gate -- --nocapture`
Expected: FAIL to compile — `clarify_text_is_owner_allowed` not defined.

- [ ] **Step 3: Add the helper + the gate**

Add the trivial pure helper near the top of `inline.rs` (it documents intent and gives the test a target):

```rust
/// Owner-gate decision for clarify text-intercept (#5): only the owner may
/// resolve a clarify the agent directed at the owner. A non-owner's message
/// falls through to a normal turn (caller returns `false`).
pub(super) fn clarify_text_is_owner_allowed(is_owner: bool) -> bool {
    is_owner
}
```

In `handle_clarify_text`, insert the owner check right after the `has_any_pending` fast-path (after line 673, before the text extraction). Placing it after the fast-path avoids a guard read on every message:

```rust
    // Owner gate (#5): only the owner may answer a clarify directed at the
    // owner. A non-owner's plain text falls through to a normal turn — unlike
    // the callback path, we do NOT consume with an error frame here.
    let is_owner = ctx
        .auth
        .access_guards
        .read()
        .await
        .get(agent_name)
        .is_some_and(|g| g.is_owner(&msg.user_id));
    if !clarify_text_is_owner_allowed(is_owner) {
        tracing::debug!(user_id = %msg.user_id, "clarify text-intercept: non-owner, falling through to turn");
        return false;
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core clarify_text_owner_gate -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Local check + clippy**

Run: `cargo check -p opex-core --bin opex-core` then `cargo clippy -p opex-core --bin opex-core -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/inline.rs
git commit -m "fix(channel-ws): owner-gate clarify text-intercept, non-owner falls through (#5)"
```

---

### Task 4: Reader — reject Message before Ready (#6)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` (`Message` arm, insert before the `connected_channels` bump at line ~102)
- Test: same file, extend `wire_guards` / add a small module

**Interfaces:**
- Consumes: reader state `state.channel_type: String` (default `"unknown"` until `handle_ready` sets it).
- Produces: behaviour change only — a `Message` arriving before the adapter's `Ready` was processed yields an `Error` frame and is NOT dispatched.

**Background:** A `Message` arriving before `Ready` is dispatched with `channel_type == "unknown"` and no formatting prompt (defect #6). `channel_type` is set at the top of `handle_ready`, so `state.channel_type == "unknown"` precisely means "no Ready processed yet." Reject such a Message with an error frame. This is a sync string compare — it does NOT violate the reader's "never awaits engine work" invariant (the `out_tx.send` is the same non-engine send the invalid-JSON path already uses).

- [ ] **Step 1: Write the failing test**

Add a `wire_guards`-style structural test asserting the guard is wired before dispatch (matching how this module tests reader routing without a live socket). Append to the `wire_guards` module in `reader.rs`:

```rust
    #[test]
    fn handshake_guard_before_dispatch() {
        let src = include_str!("reader.rs");
        let guard = src
            .find("channel_type == \"unknown\"")
            .expect("handshake-completion guard must be wired in the Message arm");
        let dispatch = src.find("dispatcher::dispatch_message(").expect("dispatcher present");
        assert!(guard < dispatch, "handshake guard must run before dispatch_message");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core handshake_guard_before_dispatch -- --nocapture`
Expected: FAIL — `channel_type == "unknown"` not found in source.

- [ ] **Step 3: Insert the guard**

In the `ChannelInbound::Message { request_id, msg }` arm, as the FIRST statement inside the arm (before the `last_activity` bump block at line ~102):

```rust
                        // Handshake-completion guard (#6): a Message before the
                        // adapter's Ready would create a bogus "unknown"-channel
                        // session with no formatting prompt. Reject it.
                        if state.channel_type == "unknown" {
                            let _ = out_tx
                                .send(OutboundMsg::Wire(ChannelOutbound::Error {
                                    request_id,
                                    message: "handshake not complete: send Ready before Message".to_string(),
                                }))
                                .await;
                            continue;
                        }
```

Note: `request_id` is moved into the error frame on the reject path; this is fine because `continue` exits the arm immediately. On the normal path `request_id` is still available further down (unchanged).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p opex-core --bin opex-core handshake_guard_before_dispatch -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Local check + clippy**

Run: `cargo check -p opex-core --bin opex-core` then `cargo clippy -p opex-core --bin opex-core -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/reader.rs
git commit -m "fix(channel-ws): reject Message before Ready handshake completes (#6)"
```

---

## Post-implementation (controller, after whole-branch review + user approval)

- Server test session in `~/opex-src` (throttled): `git pull`, `CARGO_BUILD_JOBS=4 nice ionice cargo test -p opex-core --bin opex-core -- channel_ws writer_exits ready_guard clarify_text_owner_gate handshake_guard` (or the full channel_ws module).
- `make remote-deploy` (build on server → atomic swap → restart) on explicit user approval.
- E2E smoke: reconnect an adapter (duplicate Ready → one `connected_channels` row, channel actions still delivered); confirm no regression in Telegram DM flow.
