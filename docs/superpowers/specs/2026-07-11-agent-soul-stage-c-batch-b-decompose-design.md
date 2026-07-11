# Этап C — Батч B «plan-decompose-react внутри одобренной цели» — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 1)
**База:** задеплоенные gated-инициатива v1 + harden-фаза 2A; research находка 4 (plan-decompose-react, Smallville). Спеки: `2026-07-11-agent-soul-stage-c-initiative-design.md` (§8), `...-phase2a-hardening-design.md` (§8).

---

## 1. Цель и не-цели

**Цель:** дать одобренной инициативной цели механику **plan-decompose-react**: декомпозировать цель в упорядоченные чанки (конкретные шаги), исполнять по чанкам, реактивно перепланировать остаток при расхождении. Обогащает существующий goal-driver, НЕ вводит новый автономный слой.

**Охват (решение брейншторма):** ТОЛЬКО цели `origin='initiative'`, opt-in через `[agent.initiative] decompose=true` (default false). Интерактивный `/goal` и cron-цели НЕ затронуты — их плоский loop без изменений.

**Инвариант gated сохраняется:** цель уже одобрена владельцем (v1/2A); декомпозиция и исполнение чанков происходят ВНУТРИ этого одобрения — новый гейт НЕ нужен.

**Не-цели (будущее, B-wide):** персистентный ДНЕВНОЙ план на несколько намерений, heartbeat-продвижение чанков в течение дня, авто-approve. Здесь — только внутри-цели декомпозиция.

---

## 2. Что переиспускается

- `agent/goal/driver.rs` — `run_goal_driver` loop (reload row → `continuation_prompt` → `run_goal_turn` → deliver → `judge` → `next_action` → loop). Ветвление добавляется только для initiative+decompose.
- `agent/goal/mod.rs` — `Verdict`, `next_action(row, verdict) -> DriverAction`, `continuation_prompt`, `parse_judge_verdict`, `budget_left()`.
- `session_goals.subgoals JSONB` (Vec<String>) — переиспользуется как **упорядоченный список чанков** для decompose-целей (у initiative-целей сейчас пусто; `set_subgoals` уже есть). `max_turns`/`turn_count` — бюджет.
- `agent/soul/reflection.rs::llm_text` (pub(crate), таймаут) + `json_repair::repair_json` — для decompose/replan LLM-вызовов.
- `InitiativeConfig` (`config/mod.rs`) — расширяется флагом.
- Goal-driver `run_goal_turn`, deliver, GoalTarget (2A) — доставка прогресса/результата без изменений.

---

## 3. Компоненты

### 3.1 Конфиг

`InitiativeConfig` += `pub decompose: bool` (`#[serde(default)]` → false). Без диапазон-валидации (bool). Флаг читается goal-driver'ом через `engine.cfg().agent.initiative.decompose`.

### 3.2 Схема — `session_goals.current_chunk` (миграция 079, additive)

```sql
ALTER TABLE session_goals ADD COLUMN IF NOT EXISTS current_chunk INT NOT NULL DEFAULT 0;
```
`current_chunk` = индекс текущего чанка в `subgoals`. `current_chunk >= len(subgoals)` → все чанки завершены. `GoalRow` += `pub current_chunk: i32` (декод в `get`/`list_*`). Новая DB-функция `set_current_chunk(db, session_id, n)`.

### 3.3 Чистая логика — `agent/goal/decompose.rs`

Юнит-тестируемые функции (без IO):
- `pub fn chunk_continuation_prompt(goal: &str, chunks: &[String], current: usize) -> String` — «Цель: {goal}. Текущий шаг {current+1}/{len}: {chunks[current]}. Сделано: {chunks[..current]}. Работай над ТЕКУЩИМ шагом; когда шаг выполнен — заяви явно.»
- `pub fn decompose_prompt(goal: &str, max_chunks: usize) -> String` — «Разбей цель на {≤max_chunks} упорядоченных конкретных шагов. Верни JSON {"chunks": ["...", ...]}».
- `pub fn replan_prompt(goal, done: &[String], remaining: &[String], last_outcome: &str) -> String` — «Дан goal, сделанные шаги, оставшиеся, последний результат. Пересмотри ОСТАВШИЕСЯ шаги (оставь/правь/добавь/убери). JSON {"remaining": [...]}».
- `pub struct ChunkVerdict { pub chunk_done: bool, pub replan: bool }`; `pub fn parse_chunk_verdict(raw: &str) -> ChunkVerdict` (толерантный парс, дефолт `{false,false}` при провале).
- `pub enum DecomposeAction { Continue, Advance, AdvanceAndReplan, Done, Pause(&'static str) }`
- `pub fn advance_decision(row: &GoalRow, verdict: ChunkVerdict, chunks_len: usize) -> DecomposeAction` — чистое решение: `!budget_left → Pause("budget")`; `chunk_done && current+1 >= chunks_len → Done`; `chunk_done && replan → AdvanceAndReplan`; `chunk_done → Advance`; иначе `Continue`. (`Done` проверяется до budget-паузы, как в `next_action`.)

Промпты `chunk_judge`: judge возвращает `{"chunk_done": bool, "replan": bool, "reason": "..."}`.

### 3.4 Интеграция в goal-driver

**Зависимость:** `GoalRow` (из `get()`) сейчас НЕ декодит `origin` — добавить `pub origin: String` в `GoalRow` + `g.origin` в SELECT функции `get` (и прочих, конструирующих GoalRow). Тогда драйвер видит origin цели.

В `run_goal_driver` (`driver.rs`) добавить ветку по `is_decompose = row.origin == "initiative" && engine.cfg().agent.initiative.decompose`:

- **Если НЕ decompose** — существующий плоский путь без изменений (continuation_prompt + judge + next_action).
- **Если decompose:**
  1. **Ленивая декомпозиция:** если `row.subgoals.is_empty()` — LLM `decompose_prompt` → `chunks` (cap `MAX_CHUNKS=8`) → `set_subgoals(chunks)` + `set_current_chunk(0)`; reload row. Провал/пустой результат → лог warn + fallback к плоскому пути на этот прогон (fail-soft).
  2. **Ход по чанку:** `chunk_continuation_prompt(goal, subgoals, current_chunk)` → `run_goal_turn` → deliver → chunk-judge (LLM) → `parse_chunk_verdict` → `advance_decision`:
     - `Continue` → следующий ход (тот же чанк);
     - `Advance` → `set_current_chunk(current+1)`;
     - `AdvanceAndReplan` → `replan_prompt` (LLM) → `set_subgoals([done.. + new_remaining])` → `set_current_chunk(current+1)` (реплан только когда judge флагнул расхождение — экономит LLM);
     - `Done` → `set_status('done')`, доставить финал, выход;
     - `Pause(reason)` → как существующий бюджет/judge-паузы.
  3. `bump_turn` каждый ход (существующий бюджет). Всё под тем же `max_turns`.

`is_decompose` вычисляется один раз; ветвление не трогает плоский путь. Реплан/decompose LLM-вызовы — с таймаутом (как reflection).

### 3.5 Безопасность/бюджет/fail-soft

- Цель одобрена (gated); декомпозиция внутренняя — гейт не добавляется.
- Чанки тратят существующий `max_turns` (`budget_left`); cap `MAX_CHUNKS=8` ограничивает объём декомпозиции.
- Fail-soft: провал decompose/replan/parse → откат к плоскому loop (цель всё равно исполняется). Никаких паник.
- Реплан только по флагу judge → ограниченная стоимость (не LLM-вызов на каждый чанк без нужды).
- Доставка прогресса/финала владельцу — существующим goal-driver путём (turn-доставка + GoalTarget из 2A). Чанки видны владельцу как ход диалога цели.

---

## 4. Поток данных (E2E)

```
approved initiative goal (2A) → goal-driver, is_decompose=true:
  subgoals пусто → decompose_prompt(LLM) → chunks[0..k] → set_subgoals + current_chunk=0
  loop:
    chunk_continuation_prompt(goal, chunks, current) → run_goal_turn → deliver
    chunk_judge(LLM) → {chunk_done, replan}
    advance_decision:
      Continue → тот же чанк
      Advance → current++
      AdvanceAndReplan → replan_prompt(LLM) → subgoals[current..]=new → current++
      Done (за последним чанком) → set_status('done') → финал в DM владельца
      Pause(budget/judge) → как сейчас
```

---

## 5. Обработка ошибок

- decompose LLM провал/пусто → fallback к плоскому loop (goal исполняется без чанков).
- replan LLM провал → сохранить текущий остаток чанков, продолжить (не падать).
- parse_chunk_verdict провал → `{false,false}` → Continue (как ParseFail в next_action; consecutive-failures-пауза наследуется через существующий счётчик, если применимо — см. §3.4).
- Краш посреди → durable re-drive (2A) воскрешает цель; `current_chunk`/`subgoals` персистентны → продолжит с текущего чанка.
- Пустой goal_text / cap превышен → декомпозиция обрезается до MAX_CHUNKS.

---

## 6. Тестирование

- **Юнит (чистые функции):** `chunk_continuation_prompt` (фокус на current, сделанные перечислены), `parse_chunk_verdict` (валид/мусор→дефолт), `advance_decision` (Continue/Advance/AdvanceAndReplan/Done-за-последним/Pause-budget — все ветки + Done-перед-budget), replan-merge (done-префикс сохранён + new remaining).
- **sqlx:** `set_current_chunk` + декод `current_chunk` в GoalRow; re-drive сохраняет current_chunk.
- **E2E на сервере (manual):** initiative-агент с `decompose=true`, одобрить цель → наблюдать в session_goals: subgoals заполнены чанками, current_chunk растёт по ходу; лог decompose/replan; финал в DM. Проверить fallback: если decompose LLM даёт мусор → плоский loop (цель всё равно доходит до done).

---

## 7. Файловая структура (для плана)

- `crates/opex-core/src/config/mod.rs` — `InitiativeConfig.decompose: bool`.
- `migrations/079_goal_current_chunk.sql` — `session_goals.current_chunk`.
- `crates/opex-core/src/db/session_goals.rs` — `GoalRow.current_chunk` + `GoalRow.origin` + декод (get/list_*, добавить `g.origin`/`g.current_chunk` в SELECT) + `set_current_chunk`.
- `crates/opex-core/src/agent/goal/decompose.rs` (новый) — чистые функции (промпты, ChunkVerdict, DecomposeAction, advance_decision).
- `crates/opex-core/src/agent/goal/mod.rs` — `pub mod decompose;`.
- `crates/opex-core/src/agent/goal/driver.rs` — ветка decompose в `run_goal_driver` + decompose/replan/chunk-judge LLM-вызовы.

---

## 8. Что дальше (вне Батча B)

- B-wide: персистентный дневной план (несколько намерений), heartbeat-продвижение чанков в течение дня, реактивное перепланирование на уровне дня.
- Сурфейсинг чанк-плана в UI (панель плана); авто-approve с бюджет-капом.
