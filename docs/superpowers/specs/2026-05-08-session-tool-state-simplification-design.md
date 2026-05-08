# SessionToolState Simplification — Design Spec

**Date:** 2026-05-08
**Status:** Approved

## Problem

`SessionToolState` (в `crates/hydeclaw-core/src/agent/dispatcher/state.rs`) содержит
три независимых `tokio::sync::RwLock`-поля: `describe_cache`, `call_counts`, `promoted`.

Две независимые проблемы:

1. **Утечка памяти.** `SessionToolStateMap` (`Arc<DashMap<Uuid, Arc<SessionToolState>>>`)
   в `AppState.agents.session_tool_state` никогда не очищается: ни в обработчиках
   удаления сессий (`handlers/sessions.rs`), ни в периодических задачах `main.rs`.
   `SessionPoolsMap` рядом очищается в обоих местах — это явная несостыковка.

2. **Лишняя сложность.** `call_counts` и `promoted` реализуют auto-promotion:
   system extension tool, вызванный через dispatcher 2+ раз, переезжает из
   extension-каталога в per-session core (`tools[]` LLM-запроса). Dispatcher
   включён по умолчанию `enabled: false`, promotion работает только для system
   extension tools (не для YAML/MCP), а данных о реальном эффекте нет. При этом
   код добавляет два отдельных `write()` на горячем пути `parallel.rs` и несёт
   трекинг `via_dispatcher_map` только ради promotion.

## Goal

1. Починить lifecycle: `session_tool_state` очищается при удалении сессий и
   периодически — с теми же инвариантами, что `session_pools`.
2. Упростить `SessionToolState` до единственной реальной ценности: `describe_cache`.
3. Удалить auto-promotion. При наличии данных о реальном эффекте — вернуть
   (30–40 строк изменений).

---

## Architecture

### `dispatcher/state.rs` — новая структура

```rust
/// Per-session describe cache for the tool dispatcher.
/// Avoids repeated filesystem reads (load_yaml_tools) within one session.
pub struct SessionToolState {
    describe_cache: RwLock<HashMap<String, String>>,
}

impl SessionToolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { describe_cache: RwLock::new(HashMap::new()) })
    }

    pub async fn get_describe(&self, name: &str) -> Option<String> {
        self.describe_cache.read().await.get(name).cloned()
    }

    pub async fn set_describe(&self, name: String, value: String) {
        self.describe_cache.write().await.insert(name, value);
    }
}
```

- `call_counts: RwLock<HashMap<String, u32>>` — удалено.
- `promoted: RwLock<HashSet<String>>` — удалено.
- Публичный API стал явным (`get_describe`/`set_describe`) вместо прямого доступа
  к полям.
- Один `RwLock` вместо трёх.

---

### `pipeline/parallel.rs` — удаление promotion-блока

Удалить целиком:

- `via_dispatcher_map: HashMap<ToolCallId, bool>` — существовал только для
  отслеживания dispatcher-originated вызовов ради promotion.
- Блок `if via_dispatcher && success && is_system_extension_tool(...)` (~35 строк).
- Функцию `is_system_extension_tool` (больше нигде не используется).
- Параметр `promotion_max` в `execute_tool_calls_partitioned`.

---

### `tool_handlers/tool_use.rs` — обновление describe

`deps.session_tool_state: Option<Arc<SessionToolState>>` остаётся — поле нужно
для describe_cache. Меняется только то, как оно используется.

`promoted_set()` — удалить. В call sites (`handle_search`, `handle_describe`)
передавать `&HashSet::new()` напрямую.

`handle_describe` использует новый API:

```rust
// кеш-читать:
if let Some(state) = deps.session_tool_state.as_ref() {
    if let Some(cached) = state.get_describe(name).await {
        return cached;
    }
}

// кеш-писать:
if let Some(state) = deps.session_tool_state.as_ref() {
    state.set_describe(name.to_string(), result.clone()).await;
}
```

---

### `agent/context_builder.rs` — убрать promoted-ветку

Два места, где `state.promoted.read().await.clone()` используется при partition
фильтрации инструментов:

```rust
// было:
let promoted_set = if let Some(state) = deps.session_tool_state(session_id) {
    state.promoted.read().await.clone()
} else { HashSet::new() };

// станет:
let promoted_set = HashSet::new();
```

Строки `promoted_count` в `tracing::info!` — удалить (поле исчезло).

Фильтр `|| promoted.contains(&t.name)` в `all_tools.retain(...)` — удалить.

Сигнатуры `build_extension_tool_list` и `find_extension_tool` в `lookup.rs`
**не меняются** — `promoted: &HashSet<String>` остаётся параметром, просто всегда
принимает `&HashSet::new()`. Это сохраняет будущую точку расширения.

---

### `gateway/handlers/sessions.rs` — lifecycle fix

**Одиночное удаление** (рядом с `session_pools.remove`):

```rust
agents.session_tool_state.remove(&id);
```

**Массовое удаление** (рядом с `session_pools.retain`):

```rust
agents.session_tool_state.retain(|sid, _| !session_ids.contains(sid));
```

---

### `main.rs` — периодическая эвикция не нужна

Periodic cleanup через `session_pools` как источник истины некорректен:
`session_tool_state` — per-session, не per-agent. Сессия может иметь
describe_cache без единого живого агента, и такая запись была бы ошибочно
выкинута.

После добавления explicit cleanup в `sessions.rs` lifecycle корректен:
entries удаляются точно при удалении сессии. Дополнительная периодическая
задача в `main.rs` не требуется.

---

## Что не меняется

- `SessionToolStateMap` type alias — `Arc<DashMap<Uuid, Arc<SessionToolState>>>`.
- `describe_cache` поведение — идентично текущему.
- Сигнатуры `build_extension_tool_list`, `find_extension_tool` в `lookup.rs`.
- `core_extra` конфиг в `ToolDispatcherConfig` — по-прежнему единственный способ
  добавить инструмент в per-session core (явно, через TOML).
- Все тесты, не связанные с promotion.

## Что заморожено (не удалено концептуально)

Auto-promotion можно вернуть когда появятся данные о реальном эффекте:

- Добавить `promoted: RwLock<HashSet<String>>` обратно в `SessionToolState`.
- Восстановить write-путь в `parallel.rs`.
- Раскомментировать `|| promoted.contains(&t.name)` в `context_builder.rs`.

Это 30-40 строк изменений при наличии данных о пользе.

---

## Изменяемые файлы

| Файл | Изменения |
| --- | --- |
| `agent/dispatcher/state.rs` | Удалить `call_counts`, `promoted`; добавить `get_describe`/`set_describe` |
| `agent/pipeline/parallel.rs` | Удалить promotion-блок, `via_dispatcher_map`, `is_system_extension_tool` |
| `agent/tool_handlers/tool_use.rs` | `promoted_set()` → `HashSet::new()`; новый describe API |
| `agent/context_builder.rs` | Удалить `promoted.read()` и `promoted.contains(...)` |
| `gateway/handlers/sessions.rs` | Добавить cleanup в оба места удаления |

---

## Тестирование

- Существующие unit-тесты в `dispatcher/` — должны пройти без изменений.
- Существующие тесты `tool_use.rs` на search/describe — должны пройти.
- Новые тесты:
  - `session_tool_state` очищается при `DELETE /api/sessions/{id}` (проверить
    что DashMap не содержит запись после удаления).
  - `session_tool_state` очищается при bulk delete.
  - `handle_describe` возвращает кеш-хит на второй вызов с тем же именем.
