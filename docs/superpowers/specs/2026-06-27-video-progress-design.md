# Video Processing Progress — Design Spec

**Date:** 2026-06-27
**Status:** approved (design)
**Extends:** [[2026-06-26-video-zettelkasten-notes-design]] (the video → Zettelkasten note pipeline)

## Goal

Show the user a live, in-chat progress indicator while a video is being
processed into an Obsidian note, instead of silence between the
"🎬 видео принято…" ack and the final note. Coarse worker-level phases; a
single self-updating status line (not a checklist card, not a message stream).

## Background

Today the in-core video worker (`video_worker.rs`) runs fully in the
background: claim → `process_one` (toolgate `/summarize-video` for
download+STT+frames, then the LLM digest) → MCP save → `deliver` (final note
message). The only user-visible signals are the instant ack and the final
message. The heaviest stretch (download + transcription + frames, minutes for
a long video) is hidden inside one toolgate HTTP call, so the worker cannot see
sub-steps of it — only coarse phase boundaries it controls itself.

There is also a latent gap: `deliver()` emits a `video_summary_ready` WS event
that the UI does **not** subscribe to (it is absent from `ui/src/types/ws.ts`),
so even the final note does not appear live — it shows up only after the user
reloads/reopens the session. This spec closes that gap as a near-free side
effect of the `done` phase.

## Decisions (locked)

- **Granularity:** coarse, worker-level phases (≈4), NOT toolgate sub-streaming.
- **UI:** one self-updating status line under the thread, replaced by the note
  on completion. NOT a checklist card, NOT one message per phase.

## Architecture

### WS event (backend → UI)

A new `ui_event` JSON shape, broadcast through the existing
`state.channels.ui_event_tx` (`broadcast::Sender<String>`), same channel that
already carries `session_updated`, `agent_processing`, `notification`, etc.:

```json
{
  "type": "video_progress",
  "session_id": "<uuid>",
  "phase": "fetch | digest | saving | done | failed",
  "text": "🎬 Скачиваю и расшифровываю видео…"
}
```

- `session_id` — the originating session (the indicator is scoped per session).
- `phase` — one of five values. `fetch|digest|saving` are *active* phases (show
  the line); `done|failed` are *terminal* (hide the line).
- `text` — human-readable Russian status for active phases. For terminal phases
  `text` is empty/ignored (the final note/error arrives as a real message via
  the existing `deliver`).

Backend has no typed struct requirement — events are `serde_json::json!` →
`ui_tx.send(string)`, matching the existing `deliver` emit. A small helper
`emit_video_progress(ui_tx, session_id, phase, text)` centralizes the shape.

### Emit points

Four emits across two functions. `process_one` gains a progress callback so it
can emit the two phases that bracket its internal steps without the worker
needing to split it:

```rust
// new param, last position:
on_phase: &(dyn Fn(&str, &str) + Sync)   // (phase, text)
```

- **fetch** — first line of `process_one`, before the toolgate POST:
  `on_phase("fetch", "🎬 Скачиваю и расшифровываю видео…")`
- **digest** — inside `process_one`, after toolgate returns and before the LLM
  `provider.chat(...)` call: `on_phase("digest", "📝 Составляю конспект…")`
- **saving** — worker loop, after `process_one` returns `Ok`, before the
  `note_exists`/`save_media`/`create_note` MCP sequence:
  `emit_video_progress(... "saving", "💾 Сохраняю в Obsidian…")`
- **done / failed** — worker loop, immediately after each `deliver(...)` call
  (both the success path and every failure path), emit the matching terminal
  phase so the UI clears the indicator. `done` on the success `deliver`;
  `failed` on every error `deliver` (toolgate error, MCP-disabled, save_media
  fail, create_note fail).

The worker constructs the callback as a closure capturing `ui_tx` + `session_id`
and passes it to `process_one`. In unit tests `process_one` receives a no-op
closure (`&|_, _| {}`), so the existing `process_one` tests stay deterministic
and unchanged in behaviour (only the new trailing arg is added at call sites).

### Frontend

1. **Type** — add to `ui/src/types/ws.ts`:
   ```ts
   export interface WsVideoProgress {
     type: "video_progress";
     session_id: string;
     phase: "fetch" | "digest" | "saving" | "done" | "failed";
     text: string;
   }
   ```
   and add it to the `WsEvent` union.

2. **State** — store the current active progress per session. A `Map<sessionId,
   {phase, text}>` in `chat-store` (mirrors the existing `activeSessionIds`
   pattern). On `fetch|digest|saving` → set/replace the entry; on `done|failed`
   → delete the entry.

3. **Subscription** — in the chat page, `useWsSubscription("video_progress", …)`:
   - active phase → update store;
   - terminal phase → clear store entry **and** invalidate the session's
     messages query (`queryClient.invalidateQueries` for the messages key) so
     the final note (already persisted by `deliver`) is refetched and appears
     live. This is the fix for the latent `video_summary_ready` gap.

4. **Indicator** — a small component (`VideoProgressIndicator`) rendered at the
   bottom of the active thread when the current session has an entry in the
   store. A spinner + `text`. No persistence; it is ephemeral UI state.

## Data flow

```
worker: claim job
  → emit fetch        ─ws→ UI: show "🎬 Скачиваю…"
  → toolgate (dl+STT+frames)
  → emit digest       ─ws→ UI: show "📝 Составляю конспект…"
  → LLM digest
  → emit saving       ─ws→ UI: show "💾 Сохраняю…"
  → MCP save + commit
  → deliver(note)     (persists assistant message)
  → emit done         ─ws→ UI: hide line + refetch messages → note appears
```

Failure on any phase: `deliver(error message)` then `emit failed` → UI hides the
line and refetches (error message appears).

## Error handling

- Progress emits are best-effort: `let _ = ui_tx.send(...)`. A dropped event
  never affects job processing (the durable `video_jobs` row + `deliver` remain
  the source of truth). If the UI misses a terminal event, the indicator would
  linger — mitigated by also clearing it whenever the session's messages
  refetch returns a newer assistant message, and it is purely cosmetic.
- No new failure modes in the worker: the callback only sends to a broadcast
  channel.

## Testing

- **Backend:** a `process_one` test asserting the callback is invoked with
  `fetch` then `digest` in order (pass a capturing closure into the existing
  toolgate-mock test). Worker-loop emits (`saving`/`done`/`failed`) are
  integration-level (covered by the existing loop structure + manual E2E); no
  new DB test required.
- **Frontend:** a store/reducer test that `video_progress` active phases set the
  entry and terminal phases clear it; a render test that the indicator shows for
  a session with an active entry and hides on terminal.

## Out of scope (follow-up)

- Toolgate sub-progress (download %, frames N/M) — would require streaming
  `/summarize-video`; explicitly deferred (decision: coarse phases).
- Telegram/channel progress mirroring — v1 is web-only, same as the parent
  feature.
- Percentage / ETA.
