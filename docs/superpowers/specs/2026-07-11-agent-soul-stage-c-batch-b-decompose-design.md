# Этап C — Батч B «plan-decompose-react внутри одобренной цели» — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 2 — после тройного ревью: сверка-с-кодом / безопасность / полнота)
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

Константы: `pub const MAX_CHUNKS: usize = 8;` `pub const CHUNK_MAX_CHARS: usize = 300;` (per-chunk длина, как `EVENT_MAX_CHARS`).

Юнит-тестируемые функции (без IO):
- `pub fn chunk_continuation_prompt(goal: &str, chunks: &[String], current: usize) -> String` — «Цель: {goal}. Текущий шаг {current+1}/{len}: {chunks[current]}. Сделано ранее: {chunks[..current]}. Работай над ТЕКУЩИМ шагом; когда шаг выполнен — заяви явно.»
- `pub fn decompose_prompt(goal: &str) -> String` — «Разбей цель на не более {MAX_CHUNKS} упорядоченных конкретных шагов. Верни строго JSON: {"chunks": ["...", ...]}. Цель: {goal}».
- `pub fn replan_prompt(goal, done: &[String], remaining: &[String], last_outcome: &str) -> String` — «Неизменная цель (одобрена владельцем): {goal}. Сделанные шаги: {done}. Оставшиеся: {remaining}. Последний результат: {last_outcome}. Пересмотри ОСТАВШИЕСЯ шаги — они ДОЛЖНЫ выводиться из неизменной цели, не расширять её. Верни строго JSON: {"remaining": ["...", ...]}».
- `pub fn chunk_judge_prompt(goal: &str, current_chunk: &str, last: &str) -> String` (Gap#1 — chunk-judge как явная функция) — «Цель: {goal}. Текущий шаг: {current_chunk}. Последний ответ агента (обрезан до 4000 симв., как существующий judge): {last}. Выполнен ли ТЕКУЩИЙ шаг? Изменил ли результат план так, что оставшиеся шаги нужно пересмотреть? Верни строго JSON: {"chunk_done": bool, "replan": bool}».
- `pub struct ChunkVerdict { pub chunk_done: bool, pub replan: bool, pub parse_ok: bool }`; `pub fn parse_chunk_verdict(raw: &str) -> ChunkVerdict` (толерантный парс через `json_repair::repair_json`; при провале `{false,false, parse_ok:false}`).
- `pub enum DecomposeAction { Continue, Advance, AdvanceAndReplan, Done, Pause(&'static str) }`
- `pub fn advance_decision(row: &GoalRow, verdict: ChunkVerdict, chunks_len: usize) -> DecomposeAction` — чистое решение. `current = row.current_chunk as usize`. **Порядок веток строго как в `next_action` (Ambiguity#6):**
  1. `verdict.chunk_done && current + 1 >= chunks_len` → `Done` (проверяется ПЕРВЫМ, до budget — как `Verdict::Done` в `next_action`);
  2. `!row.budget_left()` → `Pause("budget")`;
  3. `verdict.chunk_done && verdict.replan` → `AdvanceAndReplan`;
  4. `verdict.chunk_done` → `Advance`;
  5. `!verdict.parse_ok && row.consecutive_judge_failures + 1 >= 3` → `Pause("judge")` (M2/Gap#3 — chunk-путь НАСЛЕДУЕТ judge-fail-паузу плоского пути через существующий счётчик);
  6. иначе → `Continue`.

**Санитайз (H1/M3 — обязательно):** `chunks` из decompose и `remaining` из replan — вывод LLM, реинжектится каждый ход. КАЖДЫЙ элемент прогоняется через `sanitize_soul_text(chunk, CHUNK_MAX_CHARS)` перед `set_subgoals`; элемент, тронувший `scan_for_block` (→ None), ОТБРАСЫВАЕТСЯ. Если после санитайза список пуст → трактуется как провал decompose/replan (§5 fallback). Санитайз применяется в impure-обёртке (driver), не в чистой функции.

### 3.4 Интеграция в goal-driver

**Зависимость:** `GoalRow` (из `get()`) сейчас НЕ декодит `origin`/`current_chunk` — добавить `pub origin: String` + `pub current_chunk: i32` в `GoalRow` + `g.origin, g.current_chunk` в SELECT ОБОИХ конструкторов (`get()` — алиас `GoalRowTuple`; `list_active_by_agent_and_origin()` — локальный `type Row`) + починить 2 тест-фикстуры `row()` (`session_goals.rs` тесты и `mod.rs` тесты). Тогда драйвер видит origin/current_chunk.

В `run_goal_driver` (`driver.rs`), СРАЗУ ПОСЛЕ reload+budget-guard в каждой итерации цикла (Ambiguity#7 — не «один раз», а per-iteration после reload), вычислить `is_decompose = row.origin == "initiative" && engine.cfg().agent.initiative.decompose && !decompose_failed`, где `decompose_failed` — **in-memory bool** (объявлен вне цикла, Gap#4/#5): при провале decompose ставится `true` → на весь прогон драйвера откат к плоскому пути (без retry-декомпозиции каждый ход).

- **Если НЕ is_decompose** — существующий плоский путь БЕЗ изменений (`continuation_prompt` + `judge` + `next_action`).
- **Если is_decompose:**
  1. **Ленивая декомпозиция:** если `row.subgoals.is_empty()` — `llm_text(&provider, decompose_prompt(goal))` → парс `{"chunks":[...]}` → **санитайз каждого** (§3.3) → cap `MAX_CHUNKS` → если пусто/провал: `decompose_failed=true` + `continue` (следующая итерация пойдёт плоским путём); иначе `set_subgoals(chunks)` + `set_current_chunk(0)` + `continue` (reload на следующей итерации).
  2. **Ход по чанку** (под тем же `goal_lock`, что плоский путь — §Ambiguity#10): `chunk_continuation_prompt(goal, &row.subgoals, row.current_chunk as usize)` → `run_goal_turn(session_id, &prompt, cancel.clone())` → `bump_turn` → `deliver` → reload `fresh` → chunk-judge: `llm_text(&provider, chunk_judge_prompt(goal, current_chunk_text, &turn_text))` → `parse_chunk_verdict` → `record_verdict` (для наследования счётчика) → `advance_decision(&fresh, verdict, chunks_len)`:
     - `Continue` → следующая итерация (тот же чанк);
     - `Advance` → `set_current_chunk(current+1)`;
     - `AdvanceAndReplan` → `replan`: `done = subgoals[0..=current]` (включительно, Ambiguity#9), `llm_text(replan_prompt(goal, done, &subgoals[current+1..], &turn_text))` → парс `{"remaining":[...]}` → санитайз каждого → `set_subgoals([done + new_remaining])` + `set_current_chunk(current+1)`; при провале replan — сохранить текущий остаток, `set_current_chunk(current+1)` (не падать, §5);
     - `Done` → `set_status('done')`, финал доставлен (существующий deliver), выход из цикла;
     - `Pause(reason)` → как существующие бюджет/judge-паузы (`set_status('paused')`).

`provider = engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc())` (тот же, что существующий `judge`). LLM-вызовы под таймаутом внутри `llm_text`. `last_outcome` для replan/judge = текст хода из `run_goal_turn` (обрезать до 4000 симв. как существующий judge).

### 3.5 Безопасность/бюджет/fail-soft

- **Гейт держится на execution-слое (CONFIRMED-SAFE):** chunk-ходы идут через `run_goal_turn → run_isolated_pipeline` — та же tool-policy/deny-list + approval-workflow, что любой ход; approval-гейченные тулы в CRON-сессии без человека → таймаут → reject (fail-closed). Декомпозиция — механизм промптинга/расписания, НЕ новая привилегия. Цель одобрена (gated) — новый гейт не нужен.
- **Injection-барьер (H1):** чанки санитизируются `sanitize_soul_text` перед записью (§3.3) — закрывает второго-порядка инъекцию через `last_outcome` (недоверенный tool-результат → replan → персистентный реинжект). `scan_for_block`-триггер → отбрасывание/fallback.
- **Anchor к цели (M1):** replan-промпт фиксирует неизменную одобренную `goal_text`, оставшиеся шаги обязаны выводиться из неё; реплан-события логируются (`tracing::info`) для аудита.
- **Бюджет:** чанки тратят существующий `max_turns` (`budget_left`); `MAX_CHUNKS`+`CHUNK_MAX_CHARS` cap; judge-fail-пауза наследуется (§3.3 ветка 5). Реплан только при `chunk_done && replan` → ≤MAX_CHUNKS вызовов (не per-turn).
- **Fail-soft:** провал decompose/replan/parse → `decompose_failed`/сохранение остатка → плоский loop / продолжение. Никаких паник.
- **Config-drift (L1):** если `decompose` выключен посреди активной decompose-цели — плоский путь `continuation_prompt` должен резать `subgoals` с `current_chunk` (не выдавать сделанные чанки как критерии). Плоский `continuation_prompt` для origin='initiative' с `current_chunk>0` берёт `&subgoals[current_chunk..]`.

### 3.6 Re-drive совместимость

`current_chunk`/`subgoals` персистентны → durable re-drive (2A) продолжит с текущего чанка. Крайний случай (краш между decompose-LLM и `set_subgoals`): `subgoals` пусто → re-decompose с нуля (`current_chunk=0`, потерь нет).

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

- decompose LLM провал/пусто/весь список отброшен санитайзом → `decompose_failed=true` (in-memory) → плоский loop на весь прогон (goal исполняется без чанков).
- replan LLM провал/пусто → сохранить текущий остаток чанков, `current_chunk++`, продолжить (не падать).
- parse_chunk_verdict провал → `parse_ok=false` → `record_verdict` инкрементит `consecutive_judge_failures`; `advance_decision` ветка 5: 3 подряд → `Pause("judge")` (наследует safety-valve плоского пути, M2/Gap#3).
- Санитайз-триггер (`scan_for_block`) на чанке → чанк отброшен; если это опустошает список → как provало decompose/replan.
- Краш посреди → durable re-drive (2A) воскрешает цель; `current_chunk`/`subgoals` персистентны → продолжит с текущего чанка.
- Cap превышен → декомпозиция обрезается до `MAX_CHUNKS`; чанк длиннее `CHUNK_MAX_CHARS` → усечён санитайзом.

---

## 6. Тестирование

- **Юнит (чистые функции):** `chunk_continuation_prompt` (фокус на current, сделанные перечислены), `parse_chunk_verdict` (валид/мусор→`parse_ok:false`), `advance_decision` (ВСЕ ветки: Done-за-последним-ПЕРЕД-budget, Pause-budget, AdvanceAndReplan, Advance, Pause-judge-после-3-parse-fail, Continue), `chunk_judge_prompt`/`decompose_prompt`/`replan_prompt` (содержат goal/current/JSON-контракт).
- **Санитайз-тест:** чанк с инъекцией (`<|im_start|>`) → отброшен/усечён (как soul-тесты); проверить, что санитайз применяется к decompose И replan выводу.
- **sqlx:** `set_current_chunk` + декод `current_chunk`/`origin` в GoalRow; re-drive сохраняет current_chunk.
- **E2E на сервере (manual):** initiative-агент с `decompose=true`, одобрить цель → наблюдать в session_goals: subgoals заполнены чанками, current_chunk растёт по ходу; лог decompose/replan; финал в DM. Проверить fallback: если decompose LLM даёт мусор → плоский loop (цель всё равно доходит до done).

---

## 7. Файловая структура (для плана)

- `crates/opex-core/src/config/mod.rs` — `InitiativeConfig.decompose: bool`.
- `migrations/079_goal_current_chunk.sql` — `session_goals.current_chunk`.
- `crates/opex-core/src/db/session_goals.rs` — `GoalRow.origin` + `GoalRow.current_chunk` + декод в ОБОИХ конструкторах (`get()` → `GoalRowTuple`; `list_active_by_agent_and_origin()` → локальный `Row`; добавить `g.origin, g.current_chunk` в оба SELECT + расширить оба tuple + маппинги) + `set_current_chunk` + починка 2 тест-фикстур `row()` (session_goals.rs тесты, mod.rs тесты).
- `crates/opex-core/src/agent/goal/decompose.rs` (новый) — чистые функции (`MAX_CHUNKS`/`CHUNK_MAX_CHARS`, все промпты вкл. `chunk_judge_prompt`, `ChunkVerdict{parse_ok}`, `parse_chunk_verdict`, `DecomposeAction`, `advance_decision`).
- `crates/opex-core/src/agent/goal/mod.rs` — `pub mod decompose;` + плоский `continuation_prompt` режет `subgoals[current_chunk..]` для origin='initiative' (L1 config-drift).
- `crates/opex-core/src/agent/goal/driver.rs` — ветка `is_decompose` + `decompose_failed` in-memory флаг + ленивая декомпозиция + chunk-loop + decompose/replan/chunk-judge `llm_text`-вызовы + санитайз чанков перед `set_subgoals`.

**Декомпозиция (~7 задач):** (1) миграция 079; (2) GoalRow.origin+current_chunk декод оба сайта + set_current_chunk + фикстуры; (3) InitiativeConfig.decompose; (4) decompose.rs чистые функции + промпты + advance_decision + санитайз-тесты; (5) driver.rs интеграция (ветка+флаг+LLM+санитайз); (6) L1 плоский-путь slice; (7) sqlx + E2E. 1/3 независимы; 2→5, 4→5.

---

## 8. Что дальше (вне Батча B)

- B-wide: персистентный дневной план (несколько намерений), heartbeat-продвижение чанков в течение дня, реактивное перепланирование на уровне дня.
- Сурфейсинг чанк-плана в UI (панель плана); авто-approve с бюджет-капом.
