# Этап C — Батч B «plan-decompose-react» — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Одобренная initiative-цель (opt-in) декомпозируется goal-driver'ом в упорядоченные чанки, исполняется по чанкам с chunk-judge, реактивно перепланирует остаток при флаге judge; всё внутри существующего gated goal-loop.

**Architecture:** Новый `agent/goal/decompose.rs` (чистые функции: промпты, `advance_decision`). `run_goal_driver` получает ветку `is_decompose` (origin='initiative' + `[agent.initiative] decompose`), не трогая плоский путь. Чанки хранятся в существующем `subgoals`, позиция — новая колонка `current_chunk`. LLM-вызовы через `reflection::llm_text`; чанки санитизируются `sanitize_soul_text` перед записью.

**Tech Stack:** Rust 2024 (sqlx, tokio), PostgreSQL. Переиспользуются goal-driver, `judge`-паттерн, `set_subgoals`/`bump_turn`/`record_verdict`, `reflection::llm_text`, `json_repair`, `sanitize_soul_text`.

## Global Constraints

- **rustls-tls only.**
- **Охват: ТОЛЬКО `origin='initiative'` + `[agent.initiative] decompose=true`** (default false). Интерактивный `/goal` и cron — плоский путь БЕЗ изменений.
- **Gated держится на execution-слое** (chunk-ходы через `run_goal_turn` → та же deny-list/approval) — новый гейт НЕ добавляется.
- **H1 санитайз:** каждый чанк из decompose/replan → `sanitize_soul_text(chunk, CHUNK_MAX_CHARS)` перед `set_subgoals`; `scan_for_block`-триггер (→None) → чанк отброшен; пустой список → как провал (fallback).
- **advance_decision порядок:** Done (за последним чанком) проверяется ПЕРВЫМ, до budget-паузы (как `next_action`).
- **judge-fail-пауза наследуется:** parse-fail → `record_verdict(judge_failed=true)` → 3 подряд → `Pause("judge")`.
- **Fallback:** провал decompose → in-memory `decompose_failed=true` → плоский путь на прогон (без retry-декомпозиции).
- `MAX_CHUNKS=8`, `CHUNK_MAX_CHARS=300`.
- **Тесты:** opex-core в bin-таргете (Windows НЕ гоняет). Implementer: `cargo check --all-targets -p opex-core` + `cargo clippy -p opex-core --all-targets -- -D warnings`. sqlx/unit — на сервере.
- Никаких Co-Authored-By; работа в master.

---

### Task 1: Миграция 079 — `session_goals.current_chunk`

**Files:** Create `migrations/079_goal_current_chunk.sql`

- [ ] **Step 1: Написать миграцию**

```sql
-- Stage C batch B: current chunk index for plan-decompose-react on initiative goals.
ALTER TABLE session_goals ADD COLUMN IF NOT EXISTS current_chunk INT NOT NULL DEFAULT 0;
```

- [ ] **Step 2: Проверка** — визуально корректный SQL (применится на сервере; локально PG нет).

- [ ] **Step 3: Commit**

```bash
git add migrations/079_goal_current_chunk.sql
git commit -m "feat(decompose): migration 079 session_goals.current_chunk"
```

---

### Task 2: `GoalRow.origin`/`current_chunk` декод + `set_current_chunk`

**Files:** Modify `crates/opex-core/src/db/session_goals.rs`

**Interfaces — Produces:** `GoalRow` += `pub origin: String, pub current_chunk: i32`; `pub async fn set_current_chunk(db, session_id, n: i32) -> Result<()>`.

- [ ] **Step 1: Расширить `GoalRow` + оба декод-сайта**

В `GoalRow` (после `consecutive_judge_failures`):
```rust
    pub origin: String,
    pub current_chunk: i32,
```
`GoalRowTuple` (алиас) → добавить два хвостовых поля: `(String, String, i32, i32, serde_json::Value, Option<String>, i32, String, i32)`. В `get()` SELECT → `SELECT goal_text, status, turn_count, max_turns, subgoals, last_verdict, consecutive_judge_failures, origin, current_chunk FROM ...`; маппинг добавить `origin, current_chunk` в конце. В `list_active_by_agent_and_origin()` локальный `type Row` → добавить `g.origin, g.current_chunk` в SELECT + два хвостовых поля в tuple + маппинг.

- [ ] **Step 2: `set_current_chunk`**

Рядом с `bump_turn`:
```rust
pub async fn set_current_chunk(db: &PgPool, session_id: Uuid, n: i32) -> Result<()> {
    sqlx::query("UPDATE session_goals SET current_chunk = $2, updated_at = now() WHERE session_id = $1")
        .bind(session_id)
        .bind(n)
        .execute(db)
        .await?;
    Ok(())
}
```

- [ ] **Step 3: Починить тест-фикстуры `row()`**

Найти литералы `GoalRow {` в тестах (`grep -rn "GoalRow {" crates/opex-core/src`): в `session_goals.rs` тестах и `agent/goal/mod.rs` тестах (`fn row(...)`). В каждый добавить `origin: "goal".into(), current_chunk: 0,` (или `origin: "initiative".into()` где тесту нужно). Компиляция тестов не должна падать.

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core` = 0.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/db/session_goals.rs
git commit -m "feat(decompose): GoalRow.origin+current_chunk decode + set_current_chunk"
```

---

### Task 3: `InitiativeConfig.decompose`

**Files:** Modify `crates/opex-core/src/config/mod.rs`

**Interfaces — Produces:** поле `pub decompose: bool` на `InitiativeConfig`.

- [ ] **Step 1: Провальный тест**

Рядом с `initiative_config_defaults_and_validation`:
```rust
#[test]
fn initiative_decompose_defaults_false() {
    assert!(!InitiativeConfig::default().decompose);
}
```

- [ ] **Step 2: FAIL** — `cargo test --bin opex-core initiative_decompose_defaults_false` → нет поля.

- [ ] **Step 3: Реализовать** — в `InitiativeConfig` (после `daily_proposal_cap`):
```rust
    #[serde(default)]
    pub decompose: bool,
```
В `Default for InitiativeConfig` добавить `decompose: false`. Починить breaking-литералы `InitiativeConfig {` если есть (grep — вероятно только Default; если тесты/schema строят литералом — добавить `decompose: false`).

- [ ] **Step 4: Проверка** — `cargo test --bin opex-core initiative_decompose_defaults_false && cargo check --all-targets -p opex-core` = PASS + 0.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(decompose): InitiativeConfig.decompose flag (opt-in)"
```

---

### Task 4: `agent/goal/decompose.rs` — чистые функции

**Files:** Create `crates/opex-core/src/agent/goal/decompose.rs`; Modify `crates/opex-core/src/agent/goal/mod.rs` (`pub mod decompose;`)

**Interfaces — Consumes:** `db::session_goals::GoalRow` (Task 2). **Produces:** `MAX_CHUNKS`, `CHUNK_MAX_CHARS`, `chunk_continuation_prompt`, `decompose_prompt`, `replan_prompt`, `chunk_judge_prompt`, `ChunkVerdict{chunk_done,replan,parse_ok}`, `parse_chunk_verdict`, `DecomposeAction`, `advance_decision`.

- [ ] **Step 1: Провальные тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::session_goals::GoalRow;

    fn row(current_chunk: i32, turn_count: i32, max_turns: i32, cjf: i32) -> GoalRow {
        GoalRow { session_id: uuid::Uuid::nil(), goal_text: "G".into(), status: "active".into(),
            turn_count, max_turns, subgoals: vec![], last_verdict: None,
            consecutive_judge_failures: cjf, origin: "initiative".into(), current_chunk }
    }

    #[test]
    fn advance_done_before_budget() {
        // last chunk done + budget exhausted → Done (not Pause budget)
        let r = row(2, 20, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:true,replan:false,parse_ok:true}, 3), DecomposeAction::Done));
    }
    #[test]
    fn advance_pause_budget_midway() {
        let r = row(0, 20, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:false,replan:false,parse_ok:true}, 3), DecomposeAction::Pause("budget")));
    }
    #[test]
    fn advance_and_replan() {
        let r = row(0, 1, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:true,replan:true,parse_ok:true}, 3), DecomposeAction::AdvanceAndReplan));
    }
    #[test]
    fn advance_plain() {
        let r = row(0, 1, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:true,replan:false,parse_ok:true}, 3), DecomposeAction::Advance));
    }
    #[test]
    fn pause_judge_after_three_parse_fails() {
        let r = row(0, 1, 20, 2); // cjf=2, +1 = 3 → Pause judge
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:false,replan:false,parse_ok:false}, 3), DecomposeAction::Pause("judge")));
    }
    #[test]
    fn continue_default() {
        let r = row(0, 1, 20, 0);
        assert!(matches!(advance_decision(&r, ChunkVerdict{chunk_done:false,replan:false,parse_ok:true}, 3), DecomposeAction::Continue));
    }
    #[test]
    fn parse_verdict_tolerant() {
        let v = parse_chunk_verdict("```json\n{\"chunk_done\": true, \"replan\": false}\n```");
        assert!(v.chunk_done && !v.replan && v.parse_ok);
        let bad = parse_chunk_verdict("garbage");
        assert!(!bad.chunk_done && !bad.parse_ok);
    }
    #[test]
    fn chunk_prompt_focuses_current() {
        let p = chunk_continuation_prompt("goalX", &["a".into(),"b".into(),"c".into()], 1);
        assert!(p.contains("goalX") && p.contains("b")); // current chunk
        assert!(p.contains("2/3") || p.contains("2 / 3") || p.to_lowercase().contains("шаг 2"));
    }
}
```

- [ ] **Step 2: FAIL** — `cargo test --bin opex-core -- goal::decompose::tests` → нет модуля.

- [ ] **Step 3: Реализовать**

```rust
//! Stage C batch B: pure logic for plan-decompose-react within an approved
//! initiative goal. Prompts + advance decision. IO (LLM, DB, sanitize) lives in
//! the driver.
use crate::db::session_goals::GoalRow;

pub const MAX_CHUNKS: usize = 8;
pub const CHUNK_MAX_CHARS: usize = 300;

pub fn decompose_prompt(goal: &str) -> String {
    format!(
        "Разбей цель на не более {MAX_CHUNKS} упорядоченных конкретных шагов. \
         Верни строго JSON: {{\"chunks\": [\"...\", ...]}}.\n\nЦель: {goal}"
    )
}

pub fn chunk_continuation_prompt(goal: &str, chunks: &[String], current: usize) -> String {
    let len = chunks.len();
    let cur_text = chunks.get(current).map(String::as_str).unwrap_or("");
    let done: Vec<String> = chunks.iter().take(current).enumerate()
        .map(|(i, c)| format!("{}. {c}", i + 1)).collect();
    let done_block = if done.is_empty() { String::new() } else { format!("\nСделано ранее:\n{}", done.join("\n")) };
    format!(
        "Цель: {goal}.\nТекущий шаг {}/{len}: {cur_text}.{done_block}\n\
         Работай над ТЕКУЩИМ шагом; когда шаг выполнен — заяви явно.",
        current + 1
    )
}

pub fn replan_prompt(goal: &str, done: &[String], remaining: &[String], last_outcome: &str) -> String {
    let last: String = last_outcome.chars().take(4000).collect();
    format!(
        "Неизменная цель (одобрена владельцем): {goal}.\nСделанные шаги: {done:?}.\n\
         Оставшиеся: {remaining:?}.\nПоследний результат: {last}.\n\
         Пересмотри ОСТАВШИЕСЯ шаги — они ДОЛЖНЫ выводиться из неизменной цели, не расширять её. \
         Верни строго JSON: {{\"remaining\": [\"...\", ...]}}."
    )
}

pub fn chunk_judge_prompt(goal: &str, current_chunk: &str, last: &str) -> String {
    let last_slice: String = last.chars().take(4000).collect();
    format!(
        "Цель: {goal}.\nТекущий шаг: {current_chunk}.\nПоследний ответ агента:\n{last_slice}\n\n\
         Выполнен ли ТЕКУЩИЙ шаг? Изменил ли результат план так, что оставшиеся шаги нужно пересмотреть? \
         Верни строго JSON: {{\"chunk_done\": <true|false>, \"replan\": <true|false>}}."
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkVerdict { pub chunk_done: bool, pub replan: bool, pub parse_ok: bool }

pub fn parse_chunk_verdict(raw: &str) -> ChunkVerdict {
    match crate::agent::json_repair::repair_json(raw) {
        Ok(v) => ChunkVerdict {
            chunk_done: v.get("chunk_done").and_then(|x| x.as_bool()).unwrap_or(false),
            replan: v.get("replan").and_then(|x| x.as_bool()).unwrap_or(false),
            parse_ok: true,
        },
        Err(_) => ChunkVerdict { chunk_done: false, replan: false, parse_ok: false },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecomposeAction { Continue, Advance, AdvanceAndReplan, Done, Pause(&'static str) }

/// Order mirrors `next_action`: Done checked FIRST (before budget). `current` = row.current_chunk.
pub fn advance_decision(row: &GoalRow, verdict: ChunkVerdict, chunks_len: usize) -> DecomposeAction {
    let current = row.current_chunk.max(0) as usize;
    if verdict.chunk_done && current + 1 >= chunks_len {
        return DecomposeAction::Done;
    }
    if !row.budget_left() {
        return DecomposeAction::Pause("budget");
    }
    if verdict.chunk_done && verdict.replan {
        return DecomposeAction::AdvanceAndReplan;
    }
    if verdict.chunk_done {
        return DecomposeAction::Advance;
    }
    if !verdict.parse_ok && row.consecutive_judge_failures + 1 >= 3 {
        return DecomposeAction::Pause("judge");
    }
    DecomposeAction::Continue
}
```

В `agent/goal/mod.rs` добавить `pub mod decompose;` (рядом с `pub mod driver; pub mod pool;`).

- [ ] **Step 4: Проверка** — `cargo test --bin opex-core -- goal::decompose::tests && cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` = PASS + 0/0.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/goal/decompose.rs crates/opex-core/src/agent/goal/mod.rs
git commit -m "feat(decompose): pure fns (prompts, ChunkVerdict, advance_decision)"
```

---

### Task 5: driver.rs интеграция — ветка decompose

**Files:** Modify `crates/opex-core/src/agent/goal/driver.rs`

**Interfaces — Consumes:** decompose.rs (T4), `GoalRow.origin/current_chunk` + `set_current_chunk` (T2), `InitiativeConfig.decompose` (T3), `reflection::llm_text`, `sanitize_soul_text`.

- [ ] **Step 1: Хелперы LLM (decompose/replan/chunk-judge) + санитайз**

В `driver.rs` добавить (рядом с `judge`):
```rust
use super::decompose::{self, ChunkVerdict, DecomposeAction, MAX_CHUNKS, CHUNK_MAX_CHARS};

/// Sanitize + cap LLM-produced chunk strings before persistence (H1). Drops
/// injection-tripping entries; empty result signals decompose/replan failure.
fn clean_chunks(raw: Vec<String>) -> Vec<String> {
    raw.into_iter()
        .filter_map(|c| crate::agent::soul::sanitize::sanitize_soul_text(&c, CHUNK_MAX_CHARS))
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .take(MAX_CHUNKS)
        .collect()
}

async fn llm_json_list(engine: &AgentEngine, prompt: String, key: &str) -> Vec<String> {
    let provider = engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc());
    let Ok(raw) = crate::agent::soul::reflection::llm_text(&provider, prompt).await else { return vec![] };
    let Ok(v) = crate::agent::json_repair::repair_json(&raw) else { return vec![] };
    v.get(key).and_then(|a| a.as_array()).map(|a| {
        a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
    }).unwrap_or_default()
}

async fn chunk_judge(engine: &AgentEngine, goal: &str, current_chunk: &str, last: &str) -> ChunkVerdict {
    let provider = engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc());
    match crate::agent::soul::reflection::llm_text(&provider, decompose::chunk_judge_prompt(goal, current_chunk, last)).await {
        Ok(raw) => decompose::parse_chunk_verdict(&raw),
        Err(_) => ChunkVerdict { chunk_done: false, replan: false, parse_ok: false },
    }
}
```
*(Свериться: `reflection::llm_text(&provider, String)` — `pub(crate)`; `AgentEngine` тип; `sanitize_soul_text(&str, usize) -> Option<String>`.)*

- [ ] **Step 2: Вставить ветку в `run_goal_driver` loop**

Объявить `let mut decompose_failed = false;` ПЕРЕД `loop {`. Внутри цикла, ПОСЛЕ budget-guard (после блока `if !row.budget_left() {...}`), добавить:
```rust
        let is_decompose = row.origin == "initiative"
            && engine.cfg().agent.initiative.decompose
            && !decompose_failed;
        if is_decompose {
            // Lazy decompose on first entry.
            if row.subgoals.is_empty() {
                let chunks = clean_chunks(llm_json_list(&engine, decompose::decompose_prompt(&row.goal_text), "chunks").await);
                if chunks.is_empty() {
                    tracing::warn!(session = %session_id, "decompose failed/empty; falling back to flat loop");
                    decompose_failed = true;
                    continue;
                }
                let _ = crate::db::session_goals::set_subgoals(&db, session_id, &chunks).await;
                let _ = crate::db::session_goals::set_current_chunk(&db, session_id, 0).await;
                continue; // reload on next iteration
            }
            let current = row.current_chunk.max(0) as usize;
            let cur_text = row.subgoals.get(current).cloned().unwrap_or_default();
            let lock = super::pool::goal_lock(&locks, session_id);
            let text = {
                let _guard = lock.lock().await;
                if cancel.is_cancelled() { break; }
                let prompt = decompose::chunk_continuation_prompt(&row.goal_text, &row.subgoals, current);
                match engine.run_goal_turn(session_id, &prompt, cancel.clone()).await {
                    Ok(t) => t, Err(e) => { tracing::warn!(session=%session_id, error=%e, "chunk turn failed; continue"); String::new() }
                }
            };
            if cancel.is_cancelled() { break; }
            let _ = crate::db::session_goals::bump_turn(&db, session_id).await;
            if !text.trim().is_empty() { deliver(&engine, &target, session_id, &text).await; }
            let verdict = chunk_judge(&engine, &row.goal_text, &cur_text, &text).await;
            let _ = crate::db::session_goals::record_verdict(&db, session_id,
                if verdict.chunk_done { "chunk_done" } else { "continue" }, !verdict.parse_ok).await;
            let fresh = crate::db::session_goals::get(&db, session_id).await.ok().flatten().unwrap_or_else(|| row.clone());
            match decompose::advance_decision(&fresh, verdict, fresh.subgoals.len()) {
                DecomposeAction::Continue => {}
                DecomposeAction::Advance => { let _ = crate::db::session_goals::set_current_chunk(&db, session_id, fresh.current_chunk + 1).await; }
                DecomposeAction::AdvanceAndReplan => {
                    let done: Vec<String> = fresh.subgoals.iter().take(current + 1).cloned().collect();
                    let remaining: Vec<String> = fresh.subgoals.iter().skip(current + 1).cloned().collect();
                    let new_remaining = clean_chunks(llm_json_list(&engine,
                        decompose::replan_prompt(&fresh.goal_text, &done, &remaining, &text), "remaining").await);
                    if !new_remaining.is_empty() {
                        let mut merged = done.clone(); merged.extend(new_remaining);
                        let _ = crate::db::session_goals::set_subgoals(&db, session_id, &merged).await;
                        tracing::info!(session=%session_id, "initiative goal replanned remaining chunks");
                    }
                    let _ = crate::db::session_goals::set_current_chunk(&db, session_id, fresh.current_chunk + 1).await;
                }
                DecomposeAction::Done => {
                    let _ = crate::db::session_goals::set_status(&db, session_id, "done").await;
                    deliver(&engine, &target, session_id, "✅ Goal complete.").await;
                    break;
                }
                DecomposeAction::Pause(reason) => {
                    let _ = crate::db::session_goals::set_status(&db, session_id, "paused").await;
                    let m = if reason == "judge" { "⏸ Goal paused (judge unreliable). /goal resume to retry." }
                        else { "⏸ Goal paused (turn budget). /goal resume to continue." };
                    deliver(&engine, &target, session_id, m).await;
                    break;
                }
            }
            continue; // decompose branch handled this iteration
        }
        // ---- flat path (unchanged) below ----
```

- [ ] **Step 3: L1 config-drift — плоский путь режет subgoals**

В плоском пути (`let prompt = continuation_prompt(&row.goal_text, &row.subgoals);`) заменить на срез при initiative+current_chunk>0 (чтобы отключение decompose посреди цели не выдавало сделанные чанки как критерии):
```rust
            let flat_subgoals: Vec<String> = if row.origin == "initiative" && row.current_chunk > 0 {
                row.subgoals.iter().skip(row.current_chunk as usize).cloned().collect()
            } else { row.subgoals.clone() };
            let prompt = continuation_prompt(&row.goal_text, &flat_subgoals);
```

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` = 0/0.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/goal/driver.rs
git commit -m "feat(decompose): driver branch — chunk loop + replan + sanitize + fallback + L1 slice"
```

---

### Task 6: sqlx-тест `current_chunk` + `set_current_chunk`

**Files:** Modify `crates/opex-core/src/db/session_goals.rs` (sqlx-тест)

- [ ] **Step 1: Написать sqlx-тест**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn set_and_read_current_chunk(pool: sqlx::PgPool) -> sqlx::Result<()> {
    // seed a session + initiative goal (reuse existing seed helper if present)
    let sid = crate::db::sessions::create_new_session(&pool, "Dec", "system", "cron").await.unwrap();
    upsert(&pool, sid, "goal", 20).await.unwrap();
    sqlx::query("UPDATE session_goals SET origin='initiative' WHERE session_id=$1").bind(sid).execute(&pool).await?;
    set_current_chunk(&pool, sid, 3).await.unwrap();
    let g = get(&pool, sid).await.unwrap().unwrap();
    assert_eq!(g.current_chunk, 3);
    assert_eq!(g.origin, "initiative");
    Ok(())
}
```
*(Свериться с точным путём `migrations` и `create_new_session`/`upsert` сигнатурами — как в существующих sqlx-тестах session_goals.rs.)*

- [ ] **Step 2: Проверка** — `cargo check --all-targets -p opex-core` = 0 (тест гоняется на сервере).

- [ ] **Step 3: Commit**

```bash
git add crates/opex-core/src/db/session_goals.rs
git commit -m "test(decompose): sqlx set/read current_chunk + origin decode"
```

---

## Замечания по исполнению

- **Порядок:** 1→2→4→5, 3 независим, 6 после 2. Реком: 1,2,3,4,5,6.
- **Тесты Rust — на сервере** (bin + sqlx). Windows: `cargo check` + `clippy -D`.
- **E2E (сервер, manual, после всех):** агент с `[agent.initiative] decompose=true`, одобрить initiative-цель → наблюдать в session_goals: `subgoals` заполнены чанками, `current_chunk` растёт; лог decompose/replan; финал в DM. Fallback: если decompose LLM даёт мусор/санитайз опустошает → `decompose_failed` → плоский loop доходит до done.
- **Свериться при реализации:** `reflection::llm_text` pub(crate)+сигнатура; `sanitize_soul_text(&str,usize)->Option`; `run_goal_turn`/`deliver`/`goal_lock` в driver scope; путь `migrations` в sqlx; тест-фикстуры `row()` (2 места).
