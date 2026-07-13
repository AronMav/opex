# Убрать client-gone hard-abort веб-сессий — Design Spec

**Дата:** 2026-07-13
**Статус:** проектирование (ревизия 1)
**Контекст:** веб-сессия Opex (channel=ui) прервалась в проде с `run_status='interrupted'`, reason `client_gone_runaway` — агент активно исполнял тулы (code_exec) >10 мин после того, как браузер отключился, и упёрся в 600с-backstop. Владелец: отключение браузера (мобилка/сеть/закрытая вкладка) — не отмена; работа должна доводиться до конца.

---

## 1. Цель и не-цели

**Цель:** отключение SSE-клиента (браузера) больше НЕ прерывает работающий движок. Ход доводится до естественного завершения, результат сохраняется в БД, виден при перезагрузке. Прервать можно только явно (`POST /api/chat/{id}/abort`) или через собственные ограничители движка.

**Не-цели:**
- Новый абсолютный wall-clock потолок хода (max_iterations + таймауты тулов уже ограничивают; отдельный cap — вне scope).
- Изменение явной отмены (`/abort` + `CANCEL_GRACE` 30с) — остаётся как есть.
- Изменение cron/goal-путей (они идут через `NoopSink`/`handle_isolated_via_pipeline`, НЕ через `sse_converter`; client_gone к ним неприменим).

---

## 2. Что меняется

Единственный источник обрыва по client-gone — блок в `crates/opex-core/src/gateway/handlers/chat/sse_converter.rs` (~стр. 210-240):
```rust
// Safety net: abort if client gone for 10+ minutes (runaway engine protection)
if client_gone_since.is_some_and(|t| t.elapsed().as_secs() > 600) {
    ... cleanup_session_terminated(..., "interrupted", "client_gone_runaway") ...
    engine_handle.abort();
    break;
}
```
Этот backstop мис-таргетирован: проверка стоит на верху цикла, который крутится только при получении события из `event_rx.recv()` — то есть срабатывает лишь пока движок ЭМИТИТ события (активно работает). Реально зависший движок (без событий) его обходит (признано в комментарии стр. ~135). Итог: он рубит ЛЕГИТИМНУЮ долгую работу, а настоящие зависания не ловит.

---

## 3. Компоненты

### 3.1 Удаление обрыва (`sse_converter.rs`)

- Удалить весь блок `if client_gone_since.is_some_and(|t| t.elapsed().as_secs() > 600) { … engine_handle.abort(); break; }` (стр. ~210-240), включая предшествующий комментарий AUDIT:SSE-03.
- `client_gone_since: Option<std::time::Instant>` (стр. 76) — заменить на `client_gone_logged: bool = false`. Единственное оставшееся назначение — однократный info-лог факта отключения.
  - Точка успешной отправки (клиент на связи, стр. ~88-89): `client_gone_logged = false;` (при reconnect лог перевзведётся).
  - Точка disconnect (стр. ~103-106): `if !client_gone_logged { client_gone_logged = true; tracing::info!("SSE client disconnected, continuing engine to completion (result saved to DB)"); }`.
- Поведение движка при disconnect — как сейчас (продолжает работать, буферизует в `registry.push_event` для БД + resume), но БЕЗ таймаут-обрыва. Цикл завершается естественно по `Finish`/закрытию `event_rx` (уже существующий путь, `finished_sent`).

### 3.2 Статус сессии

При disconnect→естественном завершении `SessionLifecycleGuard` финализирует сессию как обычно (`done`), а не `interrupted`. Reason `client_gone_runaway` больше не появляется. Explicit `/abort` продолжает давать `interrupted`/aborted как раньше.

### 3.3 Обновление устаревших комментариев

Привести в соответствие (описывают удалённое 10-мин окно):
- `sse_converter.rs` модульный doc (~стр. 16-17): убрать упоминание «600 s … client-gone runaway-protection window».
- `sse_converter.rs` ~стр. 135: комментарий про «pre-existing client_gone_since > 600 s check» — переписать/удалить (теперь нет 600с-обрыва; explicit-cancel CANCEL_GRACE остаётся).
- `gateway/stream_registry.rs` (~стр. 43-44): пункт «10-minute timeout … aborts engine» — заменить на «client disconnect: engine continues to completion, events buffered for DB + resume; no timeout-abort».
- `gateway/handlers/chat/sse.rs` (~стр. 210-216): упоминание «600s runaway-protection window» — привести в соответствие.

---

## 4. Обработка ошибок / инварианты

- **Runaway-защита (сохраняется):** `tool_loop.max_iterations` (потолок тулов на ход) + loop-detection (`break_threshold`) + таймауты тулов (sandbox, HTTP) + явный `/abort` (cancel token + 30с CANCEL_GRACE hard-abort). Web-ход — один ход, завершается сам.
- **Registry/resume:** буферизация событий в registry при disconnect не меняется (нужна для БД-сохранения + reconnect-replay). Удаляется только таймаут-обрыв.
- **Explicit cancel:** путь `POST /api/chat/{id}/abort` (cancel token, CANCEL_GRACE, hard-abort при зависании) — НЕ трогаем.
- **Принятый риск:** патологически долгий ход без наблюдателя крутится до естественного завершения (ограничено max_iterations × таймаут-тула, не бесконечно). Принято.

---

## 5. Тестирование

- **Компиляция:** `cargo check --all-targets -p opex-core` + `cargo clippy -p opex-core --all-targets -- -D warnings` — чисто (в т.ч. нет unused `client_gone_*`).
- **Регресс:** существующие SSE-тесты остаются зелёными (юнит-теста именно на 600с-обрыв НЕТ — проверено; удаление не ломает тестов). Полный `cargo test --bin opex-core` без регрессий.
- **E2E на сервере (manual):** запустить долгую (>10 мин работы) веб-задачу агенту, закрыть браузер/вкладку; убедиться: (1) движок доработал до конца (лог «continuing engine to completion»), (2) сессия финализирована `run_status='done'` (не `interrupted`/`client_gone_runaway`), (3) результат сохранён и виден при перезагрузке страницы.

---

## 6. Файловая структура (для плана)

- `crates/opex-core/src/gateway/handlers/chat/sse_converter.rs` — удалить 600с-блок; `client_gone_since` → `client_gone_logged: bool`; обновить doc/inline-комментарии.
- `crates/opex-core/src/gateway/stream_registry.rs` — обновить комментарий про 10-мин timeout.
- `crates/opex-core/src/gateway/handlers/chat/sse.rs` — обновить комментарий про 600с окно.

**Декомпозиция (~1 задача):** одно точечное изменение в одном файле + правка комментариев в двух смежных. Тестируется компиляцией/clippy + серверным E2E.

---

## 7. Что дальше (вне v1)

- Если когда-нибудь понадобится защита от реально зависшего (не эмитящего события) движка — отдельный wall-clock watchdog по времени последнего события, независимый от присутствия клиента (не то же, что удаляемый backstop). Пока не нужно (max_iterations + tool-таймауты покрывают).
