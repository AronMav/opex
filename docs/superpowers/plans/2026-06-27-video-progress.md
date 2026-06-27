# Video Processing Progress — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a live, self-updating status line in the chat while a video is processed into an Obsidian note (fetch → digest → saving → done), and make the final note appear without a reload.

**Architecture:** The in-core `video_worker` broadcasts a new `video_progress` WS event at four phase boundaries through the existing `ui_event_tx`. `process_one` gains a progress callback for the two phases inside it; the worker loop emits the rest. The UI subscribes, stores per-session progress, renders one indicator line, and on the terminal phase clears it and refetches the session's messages.

**Tech Stack:** Rust (opex-core, tokio broadcast), TypeScript/React 19, Zustand, TanStack Query, existing `/ws` WebSocket.

**Spec:** docs/superpowers/specs/2026-06-27-video-progress-design.md

## Global Constraints

- rustls only — never add OpenSSL. No new `.env` keys (only the 3 allowed).
- TDD — failing test first. Work on master; commit per task; NO push; no Co-Authored-By trailer.
- `process_one` must stay deterministic in unit tests — the new callback is a no-op (`&|_, _| {}`) in tests; no `Utc::now()`/IO added.
- Progress emits are best-effort (`let _ = ui_tx.send(...)`) — never affect job processing.
- WS event shape is exact: `{"type":"video_progress","session_id":<uuid-str>,"phase":"fetch|digest|saving|done|failed","text":<string>}`.
- Phase strings exactly: `fetch`, `digest`, `saving`, `done`, `failed`. Active phases carry Russian `text`; terminal phases (`done`/`failed`) carry `""`.
- v1 web-only (no Telegram progress).

---

### Task 1: Backend — emit `video_progress` at four phase boundaries

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/video_worker.rs`
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `state.channels.ui_event_tx: broadcast::Sender<String>` (already cloned as `ui_tx` in `spawn_video_worker`); `VideoJob.session_id: Uuid`.
- Produces: `process_one(..., on_phase: &(dyn Fn(&str, &str) + Sync))` — new trailing param. Worker loop emits `saving`/`done`/`failed`.

- [ ] **Step 1: Write the failing test** — assert `process_one` invokes the callback `fetch` then `digest`, in order, around the toolgate + LLM steps. Add to the existing tests module (reuse the `MockServer` + fake provider pattern from `process_one_builds_note_with_image_and_summary`).

```rust
#[tokio::test]
async fn process_one_emits_fetch_then_digest() {
    use std::sync::{Arc, Mutex};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/summarize-video"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "title": "Тест", "duration": 12.0, "transcript": "речь",
            "frames": [], "degraded": {"stt": false, "vision": false}
        })))
        .mount(&server).await;

    let provider = FakeLlm::new("## Резюме\nкоротко\n\n## Конспект\nтело");
    let job = VideoJob {
        id: uuid::Uuid::new_v4(), session_id: uuid::Uuid::new_v4(),
        agent_name: "Atlas".into(), channel_id: None,
        source_type: "url".into(), source_ref: "https://youtu.be/x".into(),
        source_title: None, status: "processing".into(),
        summary: None, error: None, attempts: 1,
    };

    let phases: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let p2 = phases.clone();
    let on_phase = move |phase: &str, _text: &str| {
        p2.lock().unwrap().push(phase.to_string());
    };

    let http = reqwest::Client::new();
    let _ = process_one(&http, &server.uri(), "127.0.0.1:18789", &provider, &job, &on_phase)
        .await
        .expect("ok");

    assert_eq!(*phases.lock().unwrap(), vec!["fetch".to_string(), "digest".to_string()]);
}
```

- [ ] **Step 2: Run it, verify it fails** — `cargo test -p opex-core video_worker::process_one_emits_fetch_then_digest` → fails to compile (`process_one` takes no `on_phase`).

- [ ] **Step 3: Add the `on_phase` param + two emits to `process_one`**

Change the signature (append the param):
```rust
pub async fn process_one(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    provider: &dyn LlmProvider,
    job: &VideoJob,
    on_phase: &(dyn Fn(&str, &str) + Sync),
) -> anyhow::Result<NoteResult> {
```

- As the **first** statement of the body:
  `on_phase("fetch", "🎬 Скачиваю и расшифровываю видео…");`
- Immediately **before** the `provider.chat(...)` / `build_summary_messages` LLM call (after the toolgate response is parsed into `RawMaterial`):
  `on_phase("digest", "📝 Составляю конспект…");`

- [ ] **Step 4: Add the `emit_video_progress` helper + wire worker-loop emits**

Add near `deliver`:
```rust
/// Best-effort broadcast of a video processing phase to open UI clients.
/// `text` is the status line for active phases; terminal phases pass "".
fn emit_video_progress(
    ui_tx: &tokio::sync::broadcast::Sender<String>,
    session_id: uuid::Uuid,
    phase: &str,
    text: &str,
) {
    let ev = serde_json::json!({
        "type": "video_progress",
        "session_id": session_id.to_string(),
        "phase": phase,
        "text": text,
    });
    let _ = ui_tx.send(ev.to_string());
}
```

In `spawn_video_worker`, build the callback closure capturing `ui_tx` + `job.session_id` and pass it to `process_one`:
```rust
let pj_ui = ui_tx.clone();
let pj_sid = job.session_id;
let on_phase = move |phase: &str, text: &str| {
    emit_video_progress(&pj_ui, pj_sid, phase, text);
};
match process_one(&http, &toolgate_url, &gateway_listen, provider.as_ref(), &job, &on_phase).await {
```

Then add the remaining emits in the loop:
- **saving** — right after `Ok(nr) =>` and the `note assembled` log, before the `note_exists` collision loop:
  `emit_video_progress(&ui_tx, job.session_id, "saving", "💾 Сохраняю в Obsidian…");`
- **done** — immediately after the success-path `deliver(&db, &ui_tx, &job, &chat).await;`:
  `emit_video_progress(&ui_tx, job.session_id, "done", "");`
- **failed** — immediately after EACH error-path `deliver(...)` (MCP-disabled, save_media fail, create_note fail, and the outer `Err(e)` arm):
  `emit_video_progress(&ui_tx, job.session_id, "failed", "");`

- [ ] **Step 5: Update the other `process_one` call sites in tests** — the three existing `process_one` tests (`process_one_builds_note_with_image_and_summary`, `process_one_calls_toolgate_and_builds_digest`, `process_one_fails_on_toolgate_error`, `process_one_url_source_passes_page_url`) must pass a no-op callback as the new last arg: `&|_: &str, _: &str| {}`.

- [ ] **Step 6: Run tests + check** — `cargo test -p opex-core video_worker::` (all green incl. new) and `cargo check -p opex-core` clean.

- [ ] **Step 7: Commit** — `git add -A && git commit -m "feat(video-progress): worker emits video_progress at fetch/digest/saving/done/failed"`

---

### Task 2: Frontend — WS type, store state, subscription

**Files:**
- Modify: `ui/src/types/ws.ts`
- Modify: `ui/src/stores/chat-types.ts` (state shape + initial) and the chat store actions file that defines `markSessionActive` (find via `grep -rl markSessionActive ui/src/stores`)
- Modify: `ui/src/app/(authenticated)/chat/page.tsx` (add subscription)
- Test: `ui/src/stores/__tests__/video-progress.test.ts` (new)

**Interfaces:**
- Consumes: `useWsSubscription`, `qk.sessionMessages(id)`, `queryClient`.
- Produces: store `videoProgress: Record<string, {phase: string; text: string}>` keyed by sessionId; actions `setVideoProgress(sessionId, phase, text)` and `clearVideoProgress(sessionId)`; selector for the current session's entry.

- [ ] **Step 1: Add the WS type.** In `ui/src/types/ws.ts` add the interface and union member:
```ts
export interface WsVideoProgress {
  type: "video_progress";
  session_id: string;
  phase: "fetch" | "digest" | "saving" | "done" | "failed";
  text: string;
}
```
Add `| WsVideoProgress` to the `WsEvent` union.

- [ ] **Step 2: Write the failing store test** — `ui/src/stores/__tests__/video-progress.test.ts`:
```ts
import { describe, it, expect, beforeEach } from "vitest";
import { useChatStore } from "@/stores/chat-store";

describe("videoProgress store", () => {
  beforeEach(() => {
    useChatStore.setState({ videoProgress: {} } as never);
  });
  it("set then clear", () => {
    useChatStore.getState().setVideoProgress("s1", "fetch", "качаю");
    expect(useChatStore.getState().videoProgress["s1"]).toEqual({ phase: "fetch", text: "качаю" });
    useChatStore.getState().setVideoProgress("s1", "saving", "сохраняю");
    expect(useChatStore.getState().videoProgress["s1"].phase).toBe("saving");
    useChatStore.getState().clearVideoProgress("s1");
    expect(useChatStore.getState().videoProgress["s1"]).toBeUndefined();
  });
});
```

- [ ] **Step 3: Run it, verify it fails** — `cd ui && npm test -- video-progress` → fails (`setVideoProgress` undefined).

- [ ] **Step 4: Add state + actions.** In `chat-types.ts` add to the store state interface `videoProgress: Record<string, { phase: string; text: string }>;` and to the initial state `videoProgress: {},`. In the actions file that holds `markSessionActive`, add (Immer-style, matching the existing actions):
```ts
setVideoProgress: (sessionId, phase, text) =>
  set((s) => { s.videoProgress[sessionId] = { phase, text }; }),
clearVideoProgress: (sessionId) =>
  set((s) => { delete s.videoProgress[sessionId]; }),
```
Add their signatures to the actions type interface alongside `markSessionActive`.

- [ ] **Step 5: Run the store test** — `cd ui && npm test -- video-progress` → PASS.

- [ ] **Step 6: Add the subscription** in `chat/page.tsx`, mirroring the `agent_processing` handler (≈line 303):
```ts
useWsSubscription("video_progress", useCallback((data: {
  session_id: string; phase: string; text: string;
}) => {
  const store = useChatStore.getState();
  if (data.phase === "done" || data.phase === "failed") {
    store.clearVideoProgress(data.session_id);
    queryClient.invalidateQueries({ queryKey: qk.sessionMessages(data.session_id) });
  } else {
    store.setVideoProgress(data.session_id, data.phase, data.text);
  }
}, []));
```
(`qk.sessionMessages` invalidation uses the 3-element prefix → matches the agent-suffixed query.)

- [ ] **Step 7: Build + test** — `cd ui && npm test -- video-progress` green; `cd ui && npx tsc --noEmit` clean for the touched files.

- [ ] **Step 8: Commit** — `git add -A && git commit -m "feat(video-progress): WS type + chat-store progress state + subscription"`

---

### Task 3: Frontend — the live indicator component

**Files:**
- Create: `ui/src/components/chat/VideoProgressIndicator.tsx`
- Modify: the active-thread render (find the component that renders the message list / thread bottom for the current session — e.g. `ui/src/app/(authenticated)/chat/ChatThread.tsx` or `MessageList.tsx`)
- Test: `ui/src/components/chat/__tests__/VideoProgressIndicator.test.tsx` (new)

**Interfaces:**
- Consumes: the store's `videoProgress` keyed by the current session id.
- Produces: a presentational indicator (spinner + text) shown only when an entry exists.

- [ ] **Step 1: Write the failing render test:**
```tsx
import { render, screen } from "@testing-library/react";
import { describe, it, expect, beforeEach } from "vitest";
import { useChatStore } from "@/stores/chat-store";
import { VideoProgressIndicator } from "@/components/chat/VideoProgressIndicator";

describe("VideoProgressIndicator", () => {
  beforeEach(() => useChatStore.setState({ videoProgress: {} } as never));
  it("renders text for an active session, nothing otherwise", () => {
    const { rerender } = render(<VideoProgressIndicator sessionId="s1" />);
    expect(screen.queryByText(/Сохраняю/)).toBeNull();
    useChatStore.getState().setVideoProgress("s1", "saving", "💾 Сохраняю в Obsidian…");
    rerender(<VideoProgressIndicator sessionId="s1" />);
    expect(screen.getByText(/Сохраняю/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run it, verify it fails** — component does not exist.

- [ ] **Step 3: Implement the component:**
```tsx
"use client";
import { useChatStore } from "@/stores/chat-store";
import { Loader2 } from "lucide-react";

export function VideoProgressIndicator({ sessionId }: { sessionId: string | null }) {
  const entry = useChatStore((s) => (sessionId ? s.videoProgress[sessionId] : undefined));
  if (!entry) return null;
  return (
    <div className="flex items-center gap-2 px-4 py-2 text-sm text-muted-foreground">
      <Loader2 className="h-4 w-4 animate-spin" />
      <span>{entry.text}</span>
    </div>
  );
}
```

- [ ] **Step 4: Run the test** — PASS.

- [ ] **Step 5: Mount it** at the bottom of the active thread, passing the current session id (the same `activeSessionId` the thread already uses). Place it just below the last message, above the composer.

- [ ] **Step 6: Build + test** — `cd ui && npm test -- VideoProgressIndicator` green; `cd ui && npm run build` succeeds.

- [ ] **Step 7: Commit** — `git add -A && git commit -m "feat(video-progress): live indicator component in chat thread"`

---

## Final verification (controller, after all tasks)

- `cargo clippy -p opex-core --all-targets -- -D warnings` clean.
- `cargo test -p opex-core video_worker::` green.
- `cd ui && npm test` green; `cd ui && npm run build` succeeds.
- Whole-branch review, then deploy (core + UI static) and E2E: send a video, watch the line walk fetch → digest → saving, then the note appears live.
