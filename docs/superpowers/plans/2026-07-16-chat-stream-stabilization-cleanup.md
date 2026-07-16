# Chat Stream Stabilization + Cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Исправить 3 бага чат-стрима (visibility-stale, replay-переполнение, edit-layout), убрать мёртвый код эпохи legacy-транспорта, закрыть дрейф WS-типов через codegen, удалить мёртвые endpoint'ы и разбить переросшие page.tsx/ChatComposer.tsx.

**Architecture:** 4 независимых батча (B1 слой стрима → B2 WS-codegen → B3 малая уборка → B4 UI), каждый со своим тест-циклом и деплоем. Спека: `docs/superpowers/specs/2026-07-16-chat-stream-stabilization-cleanup-design.md`.

**Tech Stack:** Rust (axum, sqlx, ts-rs), Next.js 16 / React 19 / Zustand / vitest.

## Global Constraints

- Работа в **master** (без feature-веток), коммиты частые, push ТОЛЬКО с явного подтверждения владельца.
- Никакой Claude-атрибуции в коммитах (без Co-Authored-By).
- vitest запускать ТОЛЬКО из `ui/` (`cd ui && npx vitest run <path>`); из корня сканирует фантомы.
- Rust-тесты НЕ гонять на Windows (крашатся). Авторитет — сервер: `git bundle` → scp → clone на сервере → `CARGO_BUILD_JOBS=4 nice -n 15 ionice -c3 cargo test ...`. Тесты opex-core живут в BIN-таргете (`cargo test -p opex-core --bin opex-core`, НЕ `--lib`). sqlx-тесты: PG `opex_test` на `:5434` на сервере (поднимается `make test-db-up`; точный `DATABASE_URL` — в серверном тест-контуре, см. `Makefile` цель `test-db`).
- `cargo clippy --all-targets -- -D warnings` обязателен (clippy-ошибки не ловятся cargo check).
- Wire-формат SSE/WS менять НЕЛЬЗЯ — только рефакторинг сериализации. Fixtures без финального перевода строки (last bytes `0a7d` недопустимы).
- `#[sqlx::test]` — `migrations = "../../migrations"` + seed FK-родителей (`INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1,'A','u','ui')`).
- Перед push: `make gen-types` без диффа, `cd ui && npx tsc --noEmit`, vitest, серверный cargo test.
- Деплой: B1/B2 — ПАРОЙ (`make remote-deploy` + `./scripts/deploy-ui.sh`); B3 — по затронутому слою; B4 — только UI.

---

## Батч B1 — слой стрима

### Task 1: Прокинуть onEventActivity в новый транспорт (фикс visibility-stale)

**Files:**

- Modify: `ui/src/stores/stream/chat-stream.ts` (интерфейс `TurnStreamCallbacks` + проброс в `processSSEStream`)
- Modify: `ui/src/stores/streaming-renderer.ts:206-229` (передать колбэк из `connect`)
- Test: `ui/src/stores/stream/__tests__/stream-activity.test.ts` (создать)

**Interfaces:**

- Consumes: `processSSEStream` уже вызывает `callbacks.onEventActivity?.()` на каждом событии (`stream-processor.ts:163`) — трогать не надо.
- Produces: `TurnStreamCallbacks.onEventActivity?: () => void`; `connect()` передаёт `onEventActivity: () => recordEventActivity(agent)`.

- [ ] **Step 1: Написать падающий тест**

```ts
// ui/src/stores/stream/__tests__/stream-activity.test.ts
import { describe, it, expect, vi } from "vitest";
import { openTurnStream } from "../chat-stream";
import { streamSessionManager } from "../../stream-session";

function sseBody(lines: string[]): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  return new ReadableStream({
    start(c) {
      for (const l of lines) c.enqueue(enc.encode(`data: ${l}\n\n`));
      c.close();
    },
  });
}

describe("onEventActivity wiring (B1.2)", () => {
  it("fires per SSE event, not only on connect", async () => {
    const onEventActivity = vi.fn();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true, status: 200,
      body: sseBody([
        JSON.stringify({ type: "sync_begin", runStatus: "running", truncated: false }),
        JSON.stringify({ type: "text-delta", id: "t1", delta: "hi" }),
        JSON.stringify({ type: "sync_end", lastSeq: 2 }),
      ]),
    } as unknown as Response);
    localStorage.setItem("opex_token", "test-token");

    const session = streamSessionManager.start("A");
    await new Promise<void>((resolve) => {
      openTurnStream("A", "sid-1", session, {
        onEnvelopeApplied: () => {},
        onFinished: () => resolve(),
        onConnectionLost: () => resolve(),
        onEventActivity,
      });
    });
    expect(onEventActivity.mock.calls.length).toBeGreaterThanOrEqual(3);
  });
});
```

Примечание: если авторизация в тесте берётся иначе (см. как мокается token в существующих тестах `ui/src/stores/stream/__tests__/`), скопировать паттерн оттуда.

- [ ] **Step 2: Убедиться, что тест падает**

Run: `cd ui && npx vitest run src/stores/stream/__tests__/stream-activity.test.ts`
Expected: FAIL — `onEventActivity` не является полем `TurnStreamCallbacks` (tsc) либо 0 вызовов.

- [ ] **Step 3: Реализация**

В `chat-stream.ts` добавить в интерфейс и проброс:

```ts
export interface TurnStreamCallbacks {
  onEnvelopeApplied(): void;
  onFinished(): void;
  onConnectionLost(): void;
  /** Fired on every parsed SSE event — drives the renderer's visibility-stale detector. */
  onEventActivity?(): void;
}
```

и в объект `callbacks` внутри `processSSEStream(...)`:

```ts
          onEventActivity: () => cb.onEventActivity?.(),
```

В `streaming-renderer.ts` в вызов `openTurnStream(agent, sessionId, session, {...})` добавить:

```ts
      onEventActivity: () => recordEventActivity(agent),
```

Обновить комментарий над `_lastEventTime` (строки 46-56): семантика снова «время последнего события».

- [ ] **Step 4: Тест зелёный + существующие**

Run: `cd ui && npx vitest run src/stores/stream/ && npx tsc --noEmit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/stream/chat-stream.ts ui/src/stores/streaming-renderer.ts ui/src/stores/stream/__tests__/stream-activity.test.ts
git commit -m "fix(ui): wire onEventActivity into envelope transport — visibility-stale measures socket silence again"
```

### Task 2: Удалить мёртвый non-batch путь и мёртвые колбэки

**Files:**

- Modify: `ui/src/stores/stream/stream-processor.ts` (удаление `batchMode`, `onStreamDone`; ~-80 строк)
- Modify: `ui/src/stores/stream/chat-stream.ts` (убрать `batchMode: true` и устаревший заголовок T6/T7)
- Modify: `ui/src/stores/sse-events.ts` (удалить `extractSseEventId`)
- Test: существующие `ui/src/stores/stream/__tests__/*`, `ui/src/stores/__tests__/sse-parsing.test.ts`, `ui/src/__tests__/sse-events.test.ts`

**Interfaces:**

- Produces: `StreamProcessorOpts` без поля `batchMode`; `StreamProcessorCallbacks` без `onStreamDone`. Поведение = прежний batch-режим, безусловно.

- [ ] **Step 1: Инвентаризация вызывателей**

Run: `cd ui && npx tsc --noEmit` (базовая линия) и `rg -n "batchMode|onStreamDone|extractSseEventId" src/`
Expected: список всех мест (продовые: `stream-processor.ts`, `chat-stream.ts`, `sse-events.ts`; остальное — тесты).

- [ ] **Step 2: Правка stream-processor.ts**

Удалить: поле `batchMode` из `StreamProcessorOpts` (строки 78-90) и деструктуризации (строка 99); поле `onStreamDone` (строки 44-47) и вызов `callbacks.onStreamDone?.()` (строка 664); doc-блок «T6: batch-apply transport…» (строки 55-61) переписать без слов про legacy; в `case "sync_begin"` и `case "sync_end"` убрать `if (!batchMode) break;` (строки 522, 534); в `finally` заменить `if (batchMode) { ... }` на безусловный блок и удалить комментарий «Non-batchMode (legacy transport)…» (строки 654-656); удалить мёртвый комментарий про `step-finish` (строки 391-393) — заменить одной строкой «`step-finish` не существует на wire (снят в S6.5)».

- [ ] **Step 3: Правка chat-stream.ts и sse-events.ts**

В `chat-stream.ts`: убрать `batchMode: true` из вызова `processSSEStream`; заголовочный комментарий строк 1-21 сократить до актуального («клиентский транспорт server-authoritative стрима: startTurn = POST 202, openTurnStream = GET envelope; реконнект-политика у вызывателя»). В `sse-events.ts`: удалить `extractSseEventId` (строка ~66) и его тесты в `ui/src/__tests__/sse-events.test.ts`.

- [ ] **Step 4: Прогнать и починить тесты**

Run: `cd ui && npx tsc --noEmit && npx vitest run`
Expected: tsc укажет тестовые вызовы с `batchMode` — удалить флаг из них. Тесты, эмулировавшие legacy-поток БЕЗ конверта, привести к продовой форме (добавить `sync_begin`/`sync_end`/`finish` события в фикстуры), НЕ ослаблять processor. Итог: все зелёные, `rg "batchMode" ui/src` пуст.

- [ ] **Step 5: Commit**

```bash
git add -A ui/src
git commit -m "refactor(ui): drop dead non-batch transport path, onStreamDone, extractSseEventId"
```

### Task 3: Консолидация finishing→history

**Files:**

- Modify: `ui/src/stores/streaming-renderer.ts:214-227` (убрать дублирующие инвалидации из `onFinished`)
- Modify: `ui/src/app/(authenticated)/chat/ChatThread.tsx:104-121` (обновить комментарий backstop-эффекта)
- Test: `ui/src/stores/stream/__tests__/on-finished-no-dup-invalidate.test.ts` (создать)

**Interfaces:**

- Consumes: post-finally в `stream-processor.ts:671-731` — единственный владелец invalidate(sessions) + refetch(messages) + settle-с-id-guard.
- Produces: `onFinished` в renderer = только `connectionPhase: "idle"` + сброс reconnect-бюджета.

**ОТКЛОНЕНИЕ ОТ СПЕКИ (обосновано):** спека предлагала удалить `finalizeHandoff`-effect в ChatThread. Чтение кода показало: это НЕ дубликат, а backstop late-persist кейса — post-finally оставляет `mode:"finishing"`, когда assistant-row ещё не в refetch (`assistantPersisted=false`, `stream-processor.ts:719-727`), и завершить переход при позднем появлении row может только data-driven эффект. Он не создаёт query-трафика (только читает кэш + флипает mode). Реальный дубликат — инвалидации в `onFinished` renderer'а. Удаляем их; effect остаётся с уточнённым комментарием.

- [ ] **Step 1: Написать падающий тест**

```ts
// ui/src/stores/stream/__tests__/on-finished-no-dup-invalidate.test.ts
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../chat-stream", () => ({
  startTurn: vi.fn(),
  openTurnStream: vi.fn(),
}));
import { openTurnStream } from "../chat-stream";
import { queryClient } from "@/lib/query-client";
import { createStreamingRenderer } from "../../streaming-renderer";
import { useChatStore } from "../../chat-store";

describe("onFinished does not duplicate post-finally invalidations (B1.4)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("connect().onFinished only idles the phase", () => {
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");
    const renderer = createStreamingRenderer({
      get: () => useChatStore.getState(),
      set: (fn) => useChatStore.setState((s) => { fn(s as never); }),
    });
    renderer.connect("A", "sid-1");
    const cb = vi.mocked(openTurnStream).mock.calls[0][3];
    invalidateSpy.mockClear();
    cb.onFinished();
    expect(invalidateSpy).not.toHaveBeenCalled();
    expect(useChatStore.getState().agents["A"]?.connectionPhase).toBe("idle");
  });
});
```

- [ ] **Step 2: Убедиться, что падает** — `cd ui && npx vitest run src/stores/stream/__tests__/on-finished-no-dup-invalidate.test.ts` → FAIL (2 вызова invalidateQueries).

- [ ] **Step 3: Реализация**

В `streaming-renderer.ts` `onFinished` (строки 214-227): удалить обе строки `queryClient.invalidateQueries(...)`; комментарий заменить на: «Turn is over. Query invalidation + refetch + history settle are owned EXCLUSIVELY by stream-processor's post-finally; here we only idle the phase and reset the reconnect budget.» Проверить, что импорты `queryClient`/`qk` в renderer ещё нужны (saveUiState их не использует; если больше не нужны — удалить импорт).

В `ChatThread.tsx` (строки 104-111) обновить комментарий: «Late-persist backstop: post-finally settles to history only when the assistant row is already in the refetched cache; when the row lands LATER, this data-driven effect completes the switch. Not a duplicate — no query traffic here.»

- [ ] **Step 4: Тесты** — `cd ui && npx vitest run && npx tsc --noEmit` → PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/streaming-renderer.ts ui/src/app/\(authenticated\)/chat/ChatThread.tsx ui/src/stores/stream/__tests__/on-finished-no-dup-invalidate.test.ts
git commit -m "refactor(ui): single owner for finishing->history — drop duplicate invalidations from onFinished"
```

### Task 4: Серверная компакция replay-буфера

**Files:**

- Modify: `crates/opex-core/src/gateway/stream_registry.rs` (`push_event` + 2 новые fn + тесты)
- Modify: `crates/opex-types/src/sse.rs` (переписать комментарий про клиентское восстановление у `truncated`)

**Interfaces:**

- Produces: `fn compact_events(events: &mut Vec<(u64, String)>)`, `fn try_merge_text_delta(prev: &str, next: &str) -> Option<String>` (приватные). Семантика `truncated`: ставится ТОЛЬКО когда компакция не смогла ужать буфер ниже `MAX_BUFFER_SIZE` (некомпактируемые события); sticky.

- [ ] **Step 1: Написать падающий sqlx-тест** (в `mod tests` файла `stream_registry.rs`)

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn overflow_compacts_text_deltas(pool: sqlx::PgPool) {
        let registry = StreamRegistry::new(pool);
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'A', 'u', 'ui')")
            .bind(sid)
            .execute(registry.db())
            .await
            .expect("seed session");
        registry
            .register_with_token(sid, "A", CancellationToken::new(), Uuid::new_v4())
            .await
            .unwrap();
        let key = sid.to_string();
        let total: u64 = 10_000 + 500;
        for i in 0..total {
            registry
                .push_event(&key, &format!("{{\"type\":\"text-delta\",\"id\":\"t1\",\"delta\":\"x{i};\"}}"))
                .await;
        }
        let sub = registry.subscribe(&key).await.unwrap();
        // Компакция должна была ужать буфер — replay полный, без truncated.
        assert!(!sub.truncated, "delta stream must compact, not truncate");
        assert!(sub.events.len() < MAX_BUFFER_SIZE);
        // seq строго монотонен и последний seq сохранён.
        assert!(sub.events.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(sub.events.last().unwrap().0, total - 1);
        // Семантическая полнота: конкатенация всех delta == исходный текст.
        let mut concat = String::new();
        for (_, json) in &sub.events {
            let v: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(v["type"], "text-delta");
            concat.push_str(v["delta"].as_str().unwrap());
        }
        let expected: String = (0..total).map(|i| format!("x{i};")).collect();
        assert_eq!(concat, expected);
    }
```

Существующий `overflow_sets_truncated` (не-delta события `{"i":N}`) остаётся как есть — он теперь проверяет патологический fallback и должен остаться зелёным.

- [ ] **Step 2: Убедиться, что падает (на сервере)**

```bash
git add crates/opex-core/src/gateway/stream_registry.rs
git commit -m "test(core): failing compaction test for replay buffer overflow"
git bundle create /tmp/wip.bundle HEAD
scp /tmp/wip.bundle aronmav@188.246.224.118:/tmp/
ssh aronmav@188.246.224.118 'rm -rf ~/wip-test && git clone -q /tmp/wip.bundle ~/wip-test && cd ~/wip-test && make test-db-up && CARGO_BUILD_JOBS=4 nice -n 15 ionice -c3 DATABASE_URL=<из цели test-db в Makefile, PG opex_test:5434> cargo test -p opex-core --bin opex-core overflow_compacts -- --nocapture'
```

(Перед первым запуском открыть `Makefile`, цель `test-db`, и подставить оттуда точную строку `DATABASE_URL` — она указывает на изолированный PG `opex_test` на `:5434`.) Expected: FAIL на `assert!(!sub.truncated)`.

- [ ] **Step 3: Реализация в stream_registry.rs**

Заменить тело `push_event` (строки 168-188) на:

```rust
    pub async fn push_event(&self, session_id: &str, event_json: &str) -> u64 {
        let streams = self.streams.read().await;
        if let Some(stream) = streams.get(session_id) {
            let mut inner = stream.inner.lock().await;
            let id = inner.next_event_id;
            inner.next_event_id += 1;
            let owned = event_json.to_owned();
            // Buffer full: compact adjacent text-deltas in place (replay stays
            // semantically complete). `truncated` is the sticky fallback for a
            // buffer that would not shrink (non-delta flood) — once set we stop
            // re-attempting compaction on every push.
            if inner.events.len() >= MAX_BUFFER_SIZE && !inner.truncated {
                compact_events(&mut inner.events);
                if inner.events.len() >= MAX_BUFFER_SIZE {
                    inner.truncated = true;
                }
            }
            if inner.events.len() < MAX_BUFFER_SIZE {
                inner.events.push((id, owned.clone()));
                let _ = stream.broadcast_tx.send((id, owned));
            } else {
                // Pathological (uncompactable) overflow: broadcast only.
                let _ = stream.broadcast_tx.send((id, owned));
            }
            id
        } else {
            0
        }
    }
```

Добавить перед `mod tests`:

```rust
/// Compact the replay buffer in place: adjacent `text-delta` events of the
/// same block `id` merge into one event with the concatenated delta. The
/// merged event keeps the seq of its LAST constituent — seq stays monotonic,
/// so subscriber seq-cutoff (stream.rs) keeps working unchanged.
fn compact_events(events: &mut Vec<(u64, String)>) {
    let mut out: Vec<(u64, String)> = Vec::with_capacity(events.len() / 4);
    for (seq, json) in events.drain(..) {
        if let Some(last) = out.last_mut() {
            if let Some(merged) = try_merge_text_delta(&last.1, &json) {
                last.0 = seq;
                last.1 = merged;
                continue;
            }
        }
        out.push((seq, json));
    }
    *events = out;
}

/// Merge two adjacent SSE JSON strings when both are `text-delta` of the
/// same block id. Returns the merged JSON, or None when not mergeable.
fn try_merge_text_delta(prev: &str, next: &str) -> Option<String> {
    let p: serde_json::Value = serde_json::from_str(prev).ok()?;
    if p.get("type")?.as_str()? != "text-delta" {
        return None;
    }
    let n: serde_json::Value = serde_json::from_str(next).ok()?;
    if n.get("type")?.as_str()? != "text-delta" {
        return None;
    }
    if p.get("id") != n.get("id") {
        return None;
    }
    let combined = format!("{}{}", p.get("delta")?.as_str()?, n.get("delta")?.as_str()?);
    let mut merged = p;
    merged["delta"] = serde_json::Value::String(combined);
    serde_json::to_string(&merged).ok()
}
```

Обновить doc-комментарий у `truncated` в `ActiveStreamInner` (строки 27-29): «Set when the buffer overflowed AND compaction could not shrink it (non-delta flood). Late subscribers get an incomplete replay; the client shows a banner and relies on the final history refetch.» Аналогично переписать комментарий у поля `truncated` в `crates/opex-types/src/sse.rs` (у `SyncBegin`) — убрать упоминание «partial text из REST + хвост буфера», вписать новую семантику.

- [ ] **Step 4: Зелёный прогон на сервере**

Та же команда, что в Step 2, плюс полный контур: `cargo test -p opex-core --bin opex-core stream_registry` и `cargo clippy --all-targets -- -D warnings`. Expected: оба теста overflow_* зелёные, clippy чистый.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/stream_registry.rs crates/opex-types/src/sse.rs
git commit -m "feat(core): compact replay buffer on overflow — adjacent text-deltas merge, truncated becomes pathological-only fallback"
```

### Task 5: Клиентский баннер truncated

**Files:**

- Modify: `ui/src/stores/chat-types.ts` (поле `replayTruncated: boolean` в `AgentState` + `emptyAgentState()`)
- Modify: `ui/src/stores/stream/stream-processor.ts` (`case "sync_begin"`)
- Modify: `ui/src/stores/streaming-renderer.ts` (`connect`: сброс поля)
- Modify: `ui/src/app/(authenticated)/chat/ChatThread.tsx` (баннер)
- Modify: `ui/src/i18n/en.ts`, `ui/src/i18n/ru.ts` (ключ `chat.replay_truncated`)
- Test: `ui/src/stores/stream/__tests__/replay-truncated.test.ts` (создать)

**Interfaces:**

- Produces: `AgentState.replayTruncated: boolean` (default `false`); баннер виден только при `replayTruncated && isActivePhase(connectionPhase)`.

- [ ] **Step 1: Падающий тест** — по образцу Task 1 (тот же harness): скормить конверт с `{"type":"sync_begin","runStatus":"running","truncated":true}` и заассертить `useChatStore.getState().agents["A"]?.replayTruncated === true`; вторым кейсом — `truncated:false` → поле `false`.

- [ ] **Step 2: FAIL** — `cd ui && npx vitest run src/stores/stream/__tests__/replay-truncated.test.ts` (tsc: нет поля).

- [ ] **Step 3: Реализация**

`chat-types.ts`: в `AgentState` добавить `/** sync_begin.truncated — replay неполон (патологическое переполнение буфера); показываем баннер до конца хода. */ replayTruncated: boolean;`, в `emptyAgentState()` — `replayTruncated: false,`.

`stream-processor.ts`, `case "sync_begin"` — добавить перед `session.buffer.reset()`:

```ts
            if (event.truncated) session.write({ replayTruncated: true });
```

`streaming-renderer.ts`, в `update(agent, {...})` внутри `connect()` (строки 198-203) добавить `replayTruncated: false,` (каждое новое подключение решает заново); в аналогичный `update` внутри `sendTurn` (строки 317-324) — тоже.

`ChatThread.tsx`: рядом с `<ReconnectingIndicator>` (строка ~318):

```tsx
      {replayTruncated && isStreaming && (
        <div className="mx-auto my-2 rounded-md bg-muted/50 px-3 py-1.5 text-xs text-muted-foreground">
          {t("chat.replay_truncated")}
        </div>
      )}
```

с селектором рядом с `isLlmReconnecting`: `const replayTruncated = useChatStore((s) => s.agents[currentAgent]?.replayTruncated ?? false);`

i18n: `en.ts` → `replay_truncated: "Long response — the full text will appear when the turn completes."`; `ru.ts` → `replay_truncated: "Длинный ответ — полный текст появится после завершения хода."` (в объект `chat`).

Если упадёт grep-guard `stream-state-mutation-grep.test.ts` — мы пишем поле только через `session.write`/`update` renderer'а, добавить поле в его список по образцу соседних.

- [ ] **Step 4: PASS** — `cd ui && npx vitest run && npx tsc --noEmit`.

- [ ] **Step 5: Commit**

```bash
git add -A ui/src
git commit -m "feat(ui): replay-truncated banner for pathological buffer overflow"
```

### Task 6: Гейт и деплой B1

- [ ] **Step 1: Полный локальный UI-гейт** — `cd ui && npx tsc --noEmit && npx vitest run && npm run build`.
- [ ] **Step 2: Серверный Rust-гейт** — bundle→clone (команды из Task 4 Step 2), на сервере: `cargo clippy --all-targets -- -D warnings && cargo test -p opex-core --bin opex-core && cargo test -p opex-types`.
- [ ] **Step 3: Push (СПРОСИТЬ подтверждение владельца) + деплой парой** — `make remote-deploy && ./scripts/deploy-ui.sh` (ui-скрипт билдит локально — см. deploy-gaps).
- [ ] **Step 4: E2E на проде** — (а) длинный ход → скрыть вкладку >20 с при активном стриме → вернуть: НЕТ лишнего reconnect (в devtools Network один GET stream); (б) refresh мид-стрим → конверт восстанавливает текст; (в) Stop работает; (г) обычный короткий ход без регрессий; (д) `make logs` — без новых ошибок.
- [ ] **Step 5: Commit чек-листа не требуется; отметить батч в плане.**

---

## Батч B2 — WS-codegen

### Task 7: enum WsEvent в opex-types + ts-rs + фикстуры

**Files:**

- Create: `crates/opex-types/src/ws.rs`
- Modify: `crates/opex-types/src/lib.rs` (`pub mod ws;`)
- Create: `crates/opex-types/tests/ws_wire.rs` (по образцу `tests/sse_wire.rs`)
- Modify: `crates/opex-core/src/bin/gen_ts_types.rs` (или где регистрируются exports — найти по `rg "sse.generated" crates/`) — добавить экспорт `WsEvent` → `ui/src/types/ws.generated.ts`
- Modify: `.github/workflows/ci.yml:190-193` — добавить `ui/src/types/ws.generated.ts` в drift-проверку

**Interfaces:**

- Produces: `opex_types::ws::WsEvent` с методом `pub fn to_json(&self) -> String`. Wire-формат 1:1 с текущими json!-сайтами (инвентаризация в Step 1).

- [ ] **Step 1: Инвентаризация wire-форм (ОБЯЗАТЕЛЬНО до кода)**

Run: `rg -n 'ui_event_tx' crates/ --type rust` и `rg -n '"type": "' crates/opex-core/src --type rust | rg -v 'providers|mcp|yaml_tools|openai_compat|sse_writer|anthropic|scheduler/mod.rs:36|scheduler/mod.rs:48'`
Известные сайты (сверить дословно): `notifications.rs:48,55,61,307` (notification_read / notifications_read_all / notifications_cleared / notification), `approval_manager.rs:116` (approval_requested), `pipeline/approval.rs:53` (approval_resolved), `engine/stream.rs:75` + `pipeline/bootstrap.rs:197` (agent_processing), `scheduler/mod.rs:1696` + `channel_ws/reader.rs:197` + `chat/sse.rs:318` (session_updated; у reader-варианта есть поле `channel`), `sessions.rs:578` (agent_joined), `files.rs:787` (file, поле `mediaType`), `files.rs:818` (file_job_progress), `canvas.rs:36,51` (canvas_update), `channel_ws/handshake.rs:101` + `channel_ws/mod.rs:106` (channels_changed), `main.rs:103` (log), `goal/driver.rs:352` (goal-turn, поле `sessionId`), плюс найти сайт `pong` (`rg '"pong"' crates/`). Записать точный набор полей каждого.

- [ ] **Step 2: Написать enum (поля — ДОСЛОВНО с инвентаризации)**

```rust
// crates/opex-types/src/ws.rs
//! Global UI WebSocket event bus — single typed source of truth.
//! Mirrors the historical `json!({"type": ...})` wire format 1:1.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    Notification {
        #[ts(type = "import(\"./api\").NotificationRow")]
        data: serde_json::Value,
    },
    NotificationRead { data: NotificationReadData },
    NotificationsReadAll { data: NotificationsReadAllData },
    NotificationsCleared,
    AgentProcessing {
        agent: String,
        status: String, // "start" | "end"
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    ApprovalRequested {
        approval_id: String,
        agent: String,
        tool: String,
        #[ts(type = "Record<string, unknown>")]
        arguments: serde_json::Value,
    },
    ApprovalResolved { approval_id: String, agent: String, status: String },
    SessionUpdated {
        session_id: String,
        agent: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    AgentJoined { session_id: String, participants: Vec<String> },
    FileJobProgress {
        job_id: String,
        handler_id: String,
        session_id: String,
        phase: String,
        pct: u8,
        status: String,
    },
    File {
        url: String,
        #[serde(rename = "mediaType")]
        media_type: String,
    },
    CanvasUpdate {
        action: String,
        agent: String,
        content_type: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    ChannelsChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    Log { level: String, target: String, message: String, timestamp: String },
    AuditEvent {
        event_type: String,
        agent: String,
        #[ts(type = "Record<string, unknown>")]
        details: serde_json::Value,
    },
    #[serde(rename = "goal-turn")]
    GoalTurn {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct NotificationReadData {
    pub id: String,
    pub unread_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct NotificationsReadAllData {
    pub unread_count: i64,
}

impl WsEvent {
    /// Serialize for the broadcast bus (`ui_event_tx: Sender<String>`).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
```

ВАЖНО: если инвентаризация Step 1 показала другие/дополнительные поля у какого-то сайта — enum правится под фактический wire, НЕ наоборот. Поля `agent_joined` сверить с `sessions.rs:575-584` дословно (есть ли `agent`).

- [ ] **Step 3: Фикстур-тест `tests/ws_wire.rs`** — скопировать механику `sse_wire.rs` (helper `write_fixture` c round-trip-ассертом), директория `../../ui/src/__tests__/fixtures/ws/`, ОДИН тест на КАЖДЫЙ вариант enum (17 фикстур: notification, notification_read, notifications_read_all, notifications_cleared, agent_processing, approval_requested, approval_resolved, session_updated, agent_joined, file_job_progress, file, canvas_update, channels_changed, log, audit_event, goal-turn, pong). Файлы без финального перевода строки (write_fixture из sse_wire уже так пишет).

- [ ] **Step 4: Регистрация в gen_ts_types + CI**

Найти регистрацию SSE-экспорта: `rg -n "sse.generated|export_all|SseEvent" crates/opex-core/src/bin/` и добавить рядом экспорт `opex_types::ws::WsEvent` (плюс `NotificationReadData`, `NotificationsReadAllData`) в `ui/src/types/ws.generated.ts` тем же механизмом. В `.github/workflows/ci.yml` в список drift-файлов (строки 190-193) добавить `ui/src/types/ws.generated.ts \`.

- [ ] **Step 5: Прогон на сервере + codegen локально**

`make gen-types` (локально; создаст `ws.generated.ts`), затем bundle→сервер: `cargo test -p opex-types` (ws_wire пишет фикстуры в дерево клона — фикстуры сгенерировать ЛОКАЛЬНО нельзя из-за Windows-запрета? Нет: opex-types тесты не крашатся на Windows только у opex-core с БД; ws_wire — чистый serde. Прогнать локально: `cargo test -p opex-types --test ws_wire`. Если локальный cargo test всё же нестабилен на этой машине — сгенерировать на сервере и забрать `scp -r`).
Expected: 17 фикстур в `ui/src/__tests__/fixtures/ws/`, `ws.generated.ts` существует.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-types ui/src/types/ws.generated.ts ui/src/__tests__/fixtures/ws .github/workflows/ci.yml crates/opex-core/src/bin
git commit -m "feat(types): WsEvent enum + ts-rs codegen + wire fixtures for the UI WebSocket bus"
```

### Task 8: Перевести все send-сайты на WsEvent

**Files:**

- Modify: все сайты из инвентаризации Task 7 Step 1 (≈12 файлов opex-core)

**Interfaces:**

- Consumes: `opex_types::ws::WsEvent::to_json()`.
- Produces: `rg 'ui_event_tx' crates/ | rg 'json!'` — пусто.

- [ ] **Step 1: Механическая замена**

Образец (sessions.rs:575-584):

```rust
            // БЫЛО:
            // let event = serde_json::json!({
            //     "type": "agent_joined",
            //     "session_id": id.to_string(),
            //     "participants": participants,
            // });
            // bus.ui_event_tx.send(event.to_string()).ok();
            // СТАЛО:
            use opex_types::ws::WsEvent;
            bus.ui_event_tx
                .send(WsEvent::AgentJoined {
                    session_id: id.to_string(),
                    participants: participants.clone(),
                }.to_json())
                .ok();
```

Так по каждому сайту. `files.rs:818` — helper, конструирующий `ev`, переводится на возврат `WsEvent::FileJobProgress {...}`; три вызова `.send(ev.to_string())` → `.send(ev.to_json())`. `main.rs:103` (log-layer) — то же. Wire-формат обязан совпасть с фикстурами Task 7 бит-в-бит: где старый json! НЕ имел опционального поля — передавать `None` (skip_serializing_if сохранит форму).

- [ ] **Step 2: Гейт** — `cargo check --all-targets` локально; bundle→сервер `cargo clippy --all-targets -- -D warnings && cargo test -p opex-core --bin opex-core && cargo test -p opex-types`. Expected: чисто; grep `rg "ui_event_tx" crates/ -A2 | rg "json!"` пуст.

- [ ] **Step 3: Commit** — `git add -A crates && git commit -m "refactor(core): route all UI WS sends through typed WsEvent"`

### Task 9: UI — generated-типы, фикстур-тест, подписка agent_joined

**Files:**

- Modify: `ui/src/types/ws.ts` → тонкий реэкспорт
- Create: `ui/src/__tests__/ws-events.fixtures.test.ts`
- Modify: `ui/src/types/__tests__/ws-types.test.ts` (актуализировать под реэкспорт или заменить фикстур-тестом)
- Modify: `ui/src/app/(authenticated)/chat/page.tsx` (~строка 330, рядом с `file_job_progress`) — подписка `agent_joined`

**Interfaces:**

- Produces: `ws.ts` экспортирует `WsEvent`, `WsEventType`, `WsEventOf<T>` и алиасы старых имён (`WsSessionUpdated` и т.д.) — потребители не меняются.

- [ ] **Step 1: Переписать ws.ts**

```ts
// ui/src/types/ws.ts — thin re-export over the ts-rs codegen (see crates/opex-types/src/ws.rs).
import type { WsEvent } from "./ws.generated";

export type { WsEvent } from "./ws.generated";
export type WsEventType = WsEvent["type"];
export type WsEventOf<T extends WsEventType> = Extract<WsEvent, { type: T }>;

// Back-compat aliases (historical hand-written interface names).
export type WsSessionUpdated = WsEventOf<"session_updated">;
export type WsAgentProcessing = WsEventOf<"agent_processing">;
export type WsApprovalRequested = WsEventOf<"approval_requested">;
export type WsApprovalResolved = WsEventOf<"approval_resolved">;
export type WsLog = WsEventOf<"log">;
export type WsCanvasUpdate = WsEventOf<"canvas_update">;
export type WsChannelsChanged = WsEventOf<"channels_changed">;
export type WsAuditEvent = WsEventOf<"audit_event">;
export type WsNotification = WsEventOf<"notification">;
export type WsNotificationRead = WsEventOf<"notification_read">;
export type WsNotificationsReadAll = WsEventOf<"notifications_read_all">;
export type WsNotificationsCleared = WsEventOf<"notifications_cleared">;
export type WsFileJobProgress = WsEventOf<"file_job_progress">;
export type WsPong = WsEventOf<"pong">;
```

- [ ] **Step 2: Фикстур-тест с жёстким счётчиком**

```ts
// ui/src/__tests__/ws-events.fixtures.test.ts
import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";
import type { WsEvent, WsEventType } from "@/types/ws";

const FIXTURES = path.join(__dirname, "fixtures/ws");
const EXPECTED_COUNT = 17; // один к одному с вариантами WsEvent — бампить при добавлении

describe("WS wire fixtures (Rust serde ↔ ts-rs)", () => {
  const files = fs.readdirSync(FIXTURES).filter((f) => f.endsWith(".json"));

  it(`covers all ${EXPECTED_COUNT} variants`, () => {
    expect(files.length).toBe(EXPECTED_COUNT);
  });

  it("no trailing newline in fixtures", () => {
    for (const f of files) {
      const raw = fs.readFileSync(path.join(FIXTURES, f));
      expect(raw[raw.length - 1]).not.toBe(0x0a);
    }
  });

  it.each(files)("%s parses into the WsEvent union", (f) => {
    const ev = JSON.parse(fs.readFileSync(path.join(FIXTURES, f), "utf8")) as WsEvent;
    const t: WsEventType = ev.type; // compile-time: type must be in the union
    expect(typeof t).toBe("string");
  });
});
```

- [ ] **Step 3: Подписка agent_joined** — в `page.tsx` рядом с существующими подписками (после блока `file_job_progress`, ~строка 333):

```tsx
  useWsSubscription("agent_joined", useCallback((data: { session_id: string; participants: string[] }) => {
    useChatStore.getState().updateSessionParticipants(data.session_id, data.participants);
  }, []));
```

(Если поля отличаются от фикстуры `agent_joined.json` — привести к фикстуре.)

- [ ] **Step 4: Гейт** — `cd ui && npx tsc --noEmit && npx vitest run`; tsc покажет потребителей, чьи поля разошлись с generated (например, `pct: number` vs `u8` — одинаково `number`; чинить потребителя, не тип). `make gen-types` → git diff пуст.

- [ ] **Step 5: Commit** — `git add -A ui/src && git commit -m "feat(ui): WS types from codegen + fixture contract test + agent_joined subscription"`

### Task 10: Гейт и деплой B2

- [ ] **Step 1:** Полный гейт: `make gen-types` (diff пуст) → `cd ui && npx tsc --noEmit && npx vitest run && npm run build` → серверный `cargo clippy -- -D warnings` + `cargo test -p opex-core --bin opex-core` + `cargo test -p opex-types`.
- [ ] **Step 2:** Push (подтверждение владельца) + `make remote-deploy && ./scripts/deploy-ui.sh`.
- [ ] **Step 3:** E2E: (а) approval-запрос из чата → toast на другой странице + bell; (б) уведомление любое → звон/бейдж; (в) invite агента в сессию → участники обновились без reload (новая подписка); (г) канвас-обновление; (д) monitor: логи и аудит текут.

---

## Батч B3 — малая уборка

### Task 11: Удаление мёртвых endpoint'ов

**Files:**

- Modify: `crates/opex-core/src/gateway/handlers/sessions.rs` (routes: строки 20, 24, 27, 30 `.patch(...)`, 33; хендлеры `api_latest_session` (~40-69), `api_compact_session` (~599+), `api_patch_message` (~802+), `api_export_session` (~969+), `api_active_path`)
- Modify: `crates/opex-core/src/db/sessions.rs` — осиротевшие fn (например `get_latest_ui_session`) удалить, если clippy пометит dead_code

- [ ] **Step 1: Предохранитель — grep на проде**

```bash
ssh aronmav@188.246.224.118 "grep -rn -E 'sessions/latest|active-path|sessions/[^/\"]+/export|sessions/[^/\"]+/compact|/api/messages/' ~/opex/workspace/tools ~/opex/workspace/skills ~/opex/config/skills 2>/dev/null; echo '--- exit:' \$?"
```

Expected: только `/api/messages/{id}/feedback` допустим (он остаётся). Любое другое совпадение → СТОП, доложить владельцу, не удалять этот endpoint.

- [ ] **Step 2: Grep в репо** — `rg -n "sessions/latest|active-path|/export|/compact|api/messages" ui/src channels/src docs/runbooks` — убедиться, что UI не вызывает (допустимы: `/compact` как SLASH-команда в ChatComposer — это не REST; `feedback`).

- [ ] **Step 3: Удаление** — убрать 5 маршрутов из `routes()`: строку 20 (`latest`), 24 (`compact`), 27 (`export`), 33 (`active-path`) и `.patch(api_patch_message)` из строки 30 (delete + feedback остаются). Удалить тела хендлеров и их вспомогательные структуры запросов. `cargo check` → удалить осиротевшие db-fn, на которые укажет `-D warnings` (dead_code).

- [ ] **Step 4: Гейт на сервере** — `cargo clippy --all-targets -- -D warnings && cargo test -p opex-core --bin opex-core`. Expected: чисто (если существуют тесты на удалённые хендлеры — удалить вместе с ними).

- [ ] **Step 5: Commit** — `git add -A crates && git commit -m "refactor(gateway): drop dead session endpoints (latest/export/compact/active-path, PATCH message)"`

### Task 12: Дедуп update()/ensure()

**Files:**

- Create: `ui/src/stores/chat/actions/_shared.ts`
- Modify: `ui/src/stores/chat/actions/navigation.ts:21-39`, `composer.ts:13-18`, `session-crud.ts:47-52`, `ui/src/stores/streaming-renderer.ts:84-89`

- [ ] **Step 1: Создать _shared.ts**

```ts
// ui/src/stores/chat/actions/_shared.ts
// Общие фабрики update/ensure — единственная копия (ранее 4 дубля).
import { emptyAgentState } from "../../chat-types";
import type { AgentState, ChatStore } from "../../chat-types";

type SetFn = (fn: (draft: ChatStore) => void) => void;
type GetFn = () => ChatStore;

export function makeUpdate(set: SetFn) {
  return function update(agent: string, patch: Partial<AgentState>): void {
    set((draft) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  };
}

export function makeEnsure(get: GetFn, set: SetFn) {
  return function ensure(agent: string): AgentState {
    const s = get().agents[agent];
    if (s) return s;
    const fresh = emptyAgentState();
    // Restore persisted context limit so ContextBar is correct before first SSE.
    try {
      const stored = localStorage.getItem(`ctx_limit:${agent}`);
      if (stored) fresh.modelContextLimit = Number(stored) || null;
    } catch {}
    set((draft) => { draft.agents[agent] = fresh; });
    return fresh;
  };
}
```

- [ ] **Step 2: Заменить 4 копии** — в каждом файле: `const update = makeUpdate(set);` (в renderer: `makeUpdate(store.set)`), в navigation дополнительно `const ensure = makeEnsure(get, set);`; локальные определения удалить. Сигнатуры `set` совпадают (`StoreAccess.set` в renderer имеет ту же форму).

- [ ] **Step 3: Гейт** — `cd ui && npx tsc --noEmit && npx vitest run`. Expected: без изменений поведения, всё зелёное.

- [ ] **Step 4: Commit** — `git add -A ui/src && git commit -m "refactor(ui): dedupe update()/ensure() store helpers into actions/_shared"`

### Task 13: Комментарии-призраки

**Files:**

- Modify: `ui/src/stores/chat-types.ts:159-171` (описание FSM фаз — убрать удалённые `reconnecting`/`complete`, описать текущие: idle/submitted/streaming/error + `isLlmReconnecting` как ортогональный флаг)
- Modify: по результатам `rg -n "cutover|T6|T7|legacy transport|step-finish" ui/src --type ts` — актуализировать/удалить остатки (то, что не убрано Task 2)

- [ ] **Step 1:** Grep → правки. Только комментарии, ноль изменений кода.
- [ ] **Step 2:** `cd ui && npx tsc --noEmit && npx vitest run` → PASS.
- [ ] **Step 3:** `git add -A ui/src && git commit -m "docs(ui): retire ghost comments from the pre-envelope transport era"`

### Task 14: Гейт и деплой B3

- [ ] **Step 1:** UI-гейт + серверный Rust-гейт (команды как в Task 6).
- [ ] **Step 2:** Push (подтверждение) + `make remote-deploy` (+ `./scripts/deploy-ui.sh`, т.к. Task 12/13 трогали UI).
- [ ] **Step 3:** E2E-smoke: чат шлёт/стримит, rename/delete сессии работают, `make doctor` зелёный.

---

## Батч B4 — UI

### Task 15: Фикс layout inline-редактирования

**Files:**

- Create: `ui/src/app/(authenticated)/chat/MessageEditForm.tsx`
- Modify: `ui/src/app/(authenticated)/chat/MessageActions.tsx` (EditButton → чистый триггер с пропом `onEdit`)
- Modify: `ui/src/app/(authenticated)/chat/MessageItem.tsx` (`UserMessage`: состояние `editing`, форма в теле)
- Test: `ui/src/app/(authenticated)/chat/__tests__/message-edit-form.test.tsx` (создать)

**Interfaces:**

- Produces: `MessageActions` получает новый проп `onEdit?: () => void`; `<MessageEditForm initialText onSubmit onCancel />`.

- [ ] **Step 1: Падающий тест**

```tsx
// ui/src/app/(authenticated)/chat/__tests__/message-edit-form.test.tsx
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MessageEditForm } from "../MessageEditForm";

describe("MessageEditForm (B4.1)", () => {
  it("renders full-width textarea with initial text", () => {
    render(<MessageEditForm initialText="hello" onSubmit={() => {}} onCancel={() => {}} />);
    const ta = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(ta.value).toBe("hello");
  });

  it("Enter submits, Shift+Enter does not, Escape cancels", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(<MessageEditForm initialText="hi" onSubmit={onSubmit} onCancel={onCancel} />);
    const ta = screen.getByRole("textbox");
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: true });
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledWith("hi");
    fireEvent.keyDown(ta, { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: FAIL** — `cd ui && npx vitest run "src/app/(authenticated)/chat/__tests__/message-edit-form.test.tsx"` (модуль не существует).

- [ ] **Step 3: Компонент**

```tsx
// ui/src/app/(authenticated)/chat/MessageEditForm.tsx
"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { X, Send } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";

export function MessageEditForm({
  initialText,
  onSubmit,
  onCancel,
}: {
  initialText: string;
  onSubmit: (text: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState(initialText);

  return (
    <div className="flex w-full flex-col gap-2">
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") { e.preventDefault(); onCancel(); }
          if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); onSubmit(text); }
        }}
        className="min-h-20 w-full resize-none rounded-lg border border-border bg-background px-3 py-2 text-sm text-foreground outline-none focus:border-primary/50"
        autoFocus
      />
      <div className="flex items-center justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          <X className="h-4 w-4 mr-1" />
          {t("common.cancel")}
        </Button>
        <Button variant="ghost" size="sm" onClick={() => onSubmit(text)} className="text-primary">
          <Send className="h-4 w-4 mr-1" />
          {t("common.save")}
        </Button>
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Поднять состояние**

`MessageActions.tsx`: у `EditButton` удалить `editing`/`editText`/ветку `if (editing)` (строки 272-314) — остаётся кнопка-триггер с `onClick={onEdit}` и прежним `data-action="edit"`; `MessageActions` принимает и пробрасывает `onEdit?: () => void` (рендер EditButton только при `!showReload && onEdit`).

`MessageItem.tsx` (`UserMessage`): добавить `const [editing, setEditing] = useState(false);`. В header: `<MessageActions message={message} showReload={false} onEdit={() => setEditing(true)} />`; при `editing` скрыть header-экшены (обернуть блок экшенов в `{!editing && (...)}`). Тело (строка 213-215) заменить на:

```tsx
        {editing ? (
          <MessageEditForm
            initialText={message.parts.filter((p): p is TextPart => p.type === "text").map((p) => p.text).join("\n")}
            onSubmit={(text) => { setEditing(false); useChatStore.getState().forkAndRegenerate(message.id, text); }}
            onCancel={() => setEditing(false)}
          />
        ) : (
          <div className={cn("min-w-0 space-y-3", isSending && "opacity-70")}>
            {message.parts.map((part, i) => renderPart(part, i))}
          </div>
        )}
```

(импорты `MessageEditForm`, `TextPart` добавить; свайп-жест `[data-action="edit"]` продолжает работать — кнопка осталась.)

- [ ] **Step 5: PASS + commit**

`cd ui && npx vitest run && npx tsc --noEmit` → PASS.

```bash
git add "ui/src/app/(authenticated)/chat/MessageEditForm.tsx" "ui/src/app/(authenticated)/chat/MessageActions.tsx" "ui/src/app/(authenticated)/chat/MessageItem.tsx" "ui/src/app/(authenticated)/chat/__tests__/message-edit-form.test.tsx"
git commit -m "fix(ui): inline message edit renders as full-width form in the message body"
```

### Task 16: Декомпозиция page.tsx (971 → ~400)

**Files:**

- Create: `ui/src/app/(authenticated)/chat/hooks/use-session-restore.ts`
- Create: `ui/src/app/(authenticated)/chat/hooks/use-chat-ws.ts`
- Create: `ui/src/app/(authenticated)/chat/SessionSidebar.tsx`
- Modify: `ui/src/app/(authenticated)/chat/page.tsx`

**Interfaces:**

- Produces: `useSessionRestore({ currentAgent, sessions, sessionsReady, agents }) → { effectiveUrlSessionId, setOverrideUrlSession }`; `useChatWs(currentAgent: string): void`; `<SessionSidebar sessions sessionsTotal currentAgent activeSessionId activeSessionIds onSelectSession onNewChat />` (точный список пропов финализируется по tsc при переносе).

**Правило: ЧИСТЫЙ ПЕРЕНОС.** Ни одной поведенческой правки в restore-машине — это подтверждённо самый ломкий участок UI. Порядок хуков/эффектов сохраняется: вызовы `useSessionRestore` и `useChatWs` ставятся ровно там, где стояли их эффекты.

- [ ] **Step 1: use-session-restore.ts** — перенести из page.tsx: состояние `overrideUrlSession` (строка 78-80), `restoredAgents` ref (116), эффекты инициализации агента (119, 127), эффект sessionsReady-restore (148), cross-agent URL-resolver с `urlResolveFetched` ref (166-187), главную restore-машину (189-265), URL-sync эффект (267-283). Хук возвращает `{ effectiveUrlSessionId, setOverrideUrlSession }`. Все `useSearchParams`/`useRouter` вызовы уходят внутрь хука.
- [ ] **Step 2:** `cd ui && npx tsc --noEmit` — по ошибкам довести пропы/возвраты. `npx vitest run` — зелёный. Commit: `git add -A ui/src && git commit -m "refactor(ui): extract session restore machine into use-session-restore"`
- [ ] **Step 3: use-chat-ws.ts** — перенести 3 подписки (`session_updated` 286, `agent_processing` 309, `file_job_progress` 321) + `agent_joined` из Task 9. Вызов `useChatWs(currentAgent)` в page.tsx на прежнем месте. tsc+vitest → commit `refactor(ui): extract chat WS subscriptions into use-chat-ws`.
- [ ] **Step 4: SessionSidebar.tsx** — перенести sidebar-состояние (строки 335-346), хендлеры (349-443: toggleSelection, delete/deleteAll/share/rename), `sessionFilter` + `filteredSessions` (527-541) и JSX `sessionList` (542-780). Пропы — по фактическим использованиям (tsc подскажет). page.tsx рендерит `<SessionSidebar ... />` в обоих местах (desktop + Sheet). tsc+vitest+`npm run build` → commit `refactor(ui): extract SessionSidebar from chat page`.
- [ ] **Step 5: Контроль размера** — `wc -l "ui/src/app/(authenticated)/chat/page.tsx"` ≈ ≤450 строк.

### Task 17: Декомпозиция ChatComposer.tsx (1232 → ~600)

**Files:**

- Create: `ui/src/app/(authenticated)/chat/hooks/use-voice-input.ts` (запись/VAD/continuous: строки ~143-292)
- Create: `ui/src/app/(authenticated)/chat/hooks/use-voice-reply.ts` (TTS-ответ: SpeakerQueue, rising/falling-edge эффекты: строки ~294-558)
- Modify: `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx`

**Правило: ЧИСТЫЙ ПЕРЕНОС, эффекты бит-в-бит.** Относительный порядок ВСЕХ useEffect сохраняется: `useVoiceInput` вызывается там, где был блок 143-292, `useVoiceReply` — где 294-558. Ничего не переписывать «по пути» (память проекта: голосовой контур ловил 2 Important-бага на ровно таких правках).

- [ ] **Step 1: use-voice-reply.ts** — перенести SpeakerQueue-создание (388-…), voiceTurnPending-эффекты (508-558 rising/falling edge), stopTts/feedFrom/handleTakeover. Возврат: `{ voiceReplyActive, stopTts }` + всё, что JSX композера реально использует (по tsc). Существующие тесты `hooks/__tests__/vad.test.ts`, `use-voice-recorder.test.tsx` НЕ трогать — они тестируют нижележащие хуки.
- [ ] **Step 2:** tsc+vitest → commit `refactor(ui): extract TTS voice-reply loop into use-voice-reply`.
- [ ] **Step 3: use-voice-input.ts** — перенести `useVoiceRecorder`-обвязку, VAD-конфиг, `handleAutoResult` (143-292). Возврат: `{ voice, ... }` по использованиям.
- [ ] **Step 4:** tsc + vitest + `npm run build`; `wc -l .../ChatComposer.tsx` ≤ ~700. Commit `refactor(ui): extract voice input wiring into use-voice-input`.

### Task 18: Гейт и деплой B4

- [ ] **Step 1:** `cd ui && npx tsc --noEmit && npx vitest run && npm run build`.
- [ ] **Step 2:** Push (подтверждение) + `./scripts/deploy-ui.sh`.
- [ ] **Step 3: Ручной E2E на проде (ОБЯЗАТЕЛЬНО, ломкие зоны):**
  - Edit: отредактировать своё сообщение на десктопе и на мобильной ширине (≤400px) — форма во всю ширину тела, Enter/Esc работают, ветка создаётся, BranchNavigator показывает 2 ветки; swipe-right на мобиле открывает edit.
  - Restore: deep-link `?s=<id>`, refresh мид-стрим, переключение агента туда-обратно, «New chat» — поведение прежнее.
  - Voice: микрофон → транскрипт → отправка → стриминговая озвучка ответа; Stop обрывает озвучку; очередь голосового сообщения во время стрима.
  - WS: agent_processing бейдж активной сессии, file_job_progress индикатор.

---

## Порядок и зависимости

```text
Task 1 → 2 → 3 → 4 → 5 → 6 (B1, строго по порядку: 2 упрощает 3 и 5)
Task 7 → 8 → 9 → 10 (B2)
Task 11..14 (B3; 11 и 12/13 независимы)
Task 15 → 16 → 17 → 18 (B4; 15 независим от 16/17)
B4 не зависит от B1-B3 и может идти параллельно, кроме Task 16 Step 3 (нужен agent_joined из Task 9).
```
