# Agent Soul Stage B — Persona Drift Detector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Пассивный embedding-only детектор внутрисессионного персона-дрифта (self-baseline: агент vs его собственные ранние ходы сессии), пишущий метрику в `session_timeline`. Detect+log only — без инъекций, без LLM, без новых файлов/таблиц. Opt-in `[agent.drift]`.

**Architecture:** `drift = 1 − cos(последний_свой_ответ, центроид_ранних_своих_ответов)`. Чистые функции в `agent/drift/`, per-session baseline-кэш на AgentConfig, интеграция в `context_builder.build()` через deps-метод, метрика в существующий `session_timeline` (`event_type='drift_probe'`). Всё поверх задеплоенного этапа A.

**Tech Stack:** Rust (opex-core), toolgate embeddings (`Arc<dyn EmbeddingService>`), PostgreSQL session_timeline, dashmap.

**Спека:** [docs/superpowers/specs/2026-07-11-agent-soul-stage-b-persona-drift-design.md](../specs/2026-07-11-agent-soul-stage-b-persona-drift-design.md) (ревизия 2). При расхождении план ↔ спека — спека главнее.

## Global Constraints

- **Windows НЕ запускает Rust-тесты (crash).** Verification на каждом шаге = `cargo check --all-targets -p opex-core`. НЕ звать `cargo test`. Тесты гоняются на сервере (троттлированно, изолированный worktree, `CARGO_BUILD_JOBS=4 nice ionice`) в конце.
- **clippy::string_slice policy:** любой `s[a..b]`-срез требует `#[allow(clippy::string_slice)]` + one-line обоснование, иначе `make lint` (`-D warnings`) падает на сервере. В этом плане срезы маловероятны (только числовая математика).
- `session_timeline.event_type` — свободный TEXT (без CHECK); `'drift_probe'` валиден. Writer: `opex_db::session_timeline::log_event(db: &PgPool, session_id: Uuid, event_type: &str, payload: Option<&serde_json::Value>) -> Result<()>`.
- Эмбеддер: `AgentConfig.embedder: Arc<dyn EmbeddingService>` (поле, `agent_config.rs:39`), достижим как `self.cfg().embedder`. **НЕ** `MemoryStore::embedder()` (недоступен через `MemoryService`-трейт). `EmbeddingService::embed(&self, text: &str) -> Result<Vec<f32>>` и `embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>` (`memory/embedding.rs:35,38`).
- `MessageRow` (`opex-db/src/sessions.rs:682`): `role: String`, `content: String`, `agent_id: Option<String>`, `created_at`, `status`.
- Константы (не конфиг, YAGNI): дефолт baseline_turns 3.
- Коммиты: один на задачу, БЕЗ Co-Authored-By. `git push` — только с явного разрешения оператора.
- **Регрессионный инвариант:** при `[agent.drift].enabled=false` — ноль изменений поведения (нет embedding, нет timeline-записей, промпт байт-в-байт).
- v1 = **только detect+log.** Никакой инъекции в `messages`, никакого A-anchor, никакого нового файла. Коррекция — фаза 2 (не в этом плане).

---

### Task 1: DriftConfig `[agent.drift]`

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs`

**Interfaces:**
- Produces: `pub struct DriftConfig { pub enabled: bool, pub threshold: f32, pub min_history: usize, pub baseline_turns: usize }` с дефолтами `false / 0.15 / 6 / 3`; поле `pub drift: DriftConfig` на `AgentSettings` (`#[serde(default)]`); `DriftConfig::validate() -> Vec<String>`, вызываемый из `AgentConfig::load()`.

- [ ] **Step 1: Тесты** (в существующий `#[cfg(test)]` config-модуль):

```rust
    #[test]
    fn drift_config_defaults_when_section_absent() {
        let toml_src = r#"
[agent]
name = "T"
language = "ru"
provider = "openai"
model = "gpt-4o"
"#;
        let cfg: AgentConfig = toml::from_str(toml_src).unwrap();
        assert!(!cfg.agent.drift.enabled);
        assert_eq!(cfg.agent.drift.threshold, 0.15);
        assert_eq!(cfg.agent.drift.min_history, 6);
        assert_eq!(cfg.agent.drift.baseline_turns, 3);
    }

    #[test]
    fn drift_config_validate_rejects_out_of_range() {
        let bad = DriftConfig { enabled: true, threshold: 3.0, min_history: 1, baseline_turns: 20 };
        let errs = bad.validate();
        assert_eq!(errs.len(), 3, "each violated rule reports once: {errs:?}");
        assert!(DriftConfig::default().validate().is_empty());
    }
```

- [ ] **Step 2: Реализация** (рядом с `SoulConfig`, `config/mod.rs`):

```rust
/// Configuration for persona-drift detection (spec stage B, 2026-07-11).
/// Maps to `[agent.drift]`. All fields default — section can be omitted.
/// v1: detect + log only (no correction).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_drift_threshold")]
    pub threshold: f32,
    #[serde(default = "default_drift_min_history")]
    pub min_history: usize,
    #[serde(default = "default_drift_baseline_turns")]
    pub baseline_turns: usize,
}

fn default_drift_threshold() -> f32 { 0.15 }
fn default_drift_min_history() -> usize { 6 }
fn default_drift_baseline_turns() -> usize { 3 }

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: default_drift_threshold(),
            min_history: default_drift_min_history(),
            baseline_turns: default_drift_baseline_turns(),
        }
    }
}

impl DriftConfig {
    /// Validate drift settings. Called from `AgentConfig::load()` (like SoulConfig).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !(0.0..=2.0).contains(&self.threshold) {
            errors.push("drift.threshold must be in [0.0, 2.0]".to_string());
        }
        if !(2..=50).contains(&self.min_history) {
            errors.push("drift.min_history must be in [2, 50]".to_string());
        }
        if !(1..=10).contains(&self.baseline_turns) {
            errors.push("drift.baseline_turns must be in [1, 10]".to_string());
        }
        errors
    }
}
```

На `AgentSettings` (после `soul`):

```rust
    /// Persona-drift detection (spec stage B, 2026-07-11).
    #[serde(default)]
    pub drift: DriftConfig,
```

В `AgentConfig::load()` — рядом с `self.agent.soul.validate()` добавить идентичную обработку `self.agent.drift.validate()` (тот же формат ошибок/bail).

Ломающиеся struct-литералы `AgentSettings { ... }` (те же, что чинил этап A): `config/mod.rs` два тест-литерала + `gateway/handlers/agents/schema.rs` — добавить `drift: DriftConfig::default(),` (cargo check покажет каждый).

- [ ] **Step 3:** Run: `cargo check --all-targets -p opex-core` → чисто.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/gateway/handlers/agents/schema.rs
git commit -m "feat(drift): [agent.drift] DriftConfig with load-time validation"
```

---

### Task 2: Детектор (чистые функции)

**Files:**
- Create: `crates/opex-core/src/agent/drift/mod.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (объявить `pub(crate) mod drift;`)

**Interfaces:**
- Consumes: `opex_db::sessions::MessageRow` (поля role/content/agent_id).
- Produces:
  - `pub fn centroid(embeddings: &[Vec<f32>]) -> Option<Vec<f32>>` (центроид нормированных; None если пусто/вырождено)
  - `pub fn drift_score(baseline_centroid: &[f32], recent: &[f32]) -> f32` (`1 − cos`, ∈ [0,2])
  - `pub fn own_assistant_texts(history: &[opex_db::sessions::MessageRow], agent_name: &str) -> Vec<String>`

- [ ] **Step 1: Тесты** (в `drift/mod.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, agent: Option<&str>, content: &str) -> opex_db::sessions::MessageRow {
        opex_db::sessions::MessageRow {
            id: uuid::Uuid::new_v4(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            created_at: chrono::Utc::now(),
            agent_id: agent.map(String::from),
            feedback: None,
            edited_at: None,
            status: "done".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn drift_zero_when_recent_equals_baseline() {
        let base = centroid(&[vec![1.0, 0.0, 0.0]]).unwrap();
        let s = drift_score(&base, &[1.0, 0.0, 0.0]);
        assert!(s.abs() < 1e-5, "identical → ~0, got {s}");
    }

    #[test]
    fn drift_one_when_orthogonal() {
        let base = centroid(&[vec![1.0, 0.0, 0.0]]).unwrap();
        let s = drift_score(&base, &[0.0, 1.0, 0.0]);
        assert!((s - 1.0).abs() < 1e-5, "orthogonal → 1, got {s}");
    }

    #[test]
    fn centroid_normalizes_before_averaging() {
        // два ортогональных, разной магнитуды → центроид указывает в биссектрису
        let c = centroid(&[vec![10.0, 0.0], vec![0.0, 0.1]]).unwrap();
        // после нормализации оба единичные → среднее (0.5, 0.5)
        assert!((c[0] - 0.5).abs() < 1e-5 && (c[1] - 0.5).abs() < 1e-5, "got {c:?}");
    }

    #[test]
    fn centroid_empty_and_zero_vectors_are_none() {
        assert!(centroid(&[]).is_none());
        assert!(centroid(&[vec![0.0, 0.0, 0.0]]).is_none());
    }

    #[test]
    fn own_texts_filters_role_agent_and_empty() {
        let hist = vec![
            row("user", Some("A"), "привет"),
            row("assistant", Some("A"), "ответ A1"),
            row("assistant", Some("B"), "ответ чужого агента"),  // peer — исключить
            row("assistant", None, "ответ без тега"),            // None → считаем своим
            row("assistant", Some("A"), "   "),                  // пустой — исключить
            row("assistant", Some("A"), "ответ A2"),
        ];
        let texts = own_assistant_texts(&hist, "A");
        assert_eq!(texts, vec!["ответ A1", "ответ без тега", "ответ A2"]);
    }
}
```

- [ ] **Step 2: Реализация**

```rust
//! Self-baseline persona-drift detector (spec stage B §2): drift = 1 − cos(recent,
//! baseline_centroid), где baseline = центроид собственных ранних ответов агента в
//! этой сессии. Embedding-only, detect+log v1 (никаких инъекций). Чистые функции;
//! обвязка (embed через cfg().embedder, кэш, запись в session_timeline) — в
//! engine/context_builder.rs.

/// L2-нормализация. Вырожденный/нулевой вектор → None.
fn normalize(v: &[f32]) -> Option<Vec<f32>> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < f32::EPSILON {
        return None;
    }
    Some(v.iter().map(|x| x / norm).collect())
}

/// Центроид нормированных эмбеддингов (среднее единичных векторов).
/// None, если пусто или все вырождены.
pub fn centroid(embeddings: &[Vec<f32>]) -> Option<Vec<f32>> {
    let normed: Vec<Vec<f32>> = embeddings.iter().filter_map(|e| normalize(e)).collect();
    if normed.is_empty() {
        return None;
    }
    let dim = normed[0].len();
    let mut acc = vec![0.0f32; dim];
    for v in &normed {
        for (i, x) in v.iter().enumerate() {
            if i < dim {
                acc[i] += x;
            }
        }
    }
    let n = normed.len() as f32;
    Some(acc.iter().map(|x| x / n).collect())
}

/// Косинус (0 при вырожденном векторе).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < f32::EPSILON || nb < f32::EPSILON {
        return 0.0;
    }
    dot / (na * nb)
}

/// drift = 1 − cos(recent, baseline_centroid) ∈ [0, 2]. Выше = дальше от раннего себя.
pub fn drift_score(baseline_centroid: &[f32], recent: &[f32]) -> f32 {
    1.0 - cosine(recent, baseline_centroid)
}

/// Тексты СОБСТВЕННЫХ assistant-ответов агента с натуральным содержимым,
/// хронологически. Фильтр: role=assistant, agent_id == свой ИЛИ None (untagged —
/// считаем своим; чужие peer-агенты в пуле тегируются своим id и исключаются),
/// непустой trim. Пропускает tool-call-only / пустые (спека §2, ревью F10/F12).
pub fn own_assistant_texts(
    history: &[opex_db::sessions::MessageRow],
    agent_name: &str,
) -> Vec<String> {
    history
        .iter()
        .filter(|m| {
            let own = m.agent_id.as_deref();
            m.role == "assistant"
                && (own.is_none() || own == Some(agent_name))
                && !m.content.trim().is_empty()
        })
        .map(|m| m.content.clone())
        .collect()
}
```

**NB implementer:** тест-хелпер `row()` использует `..Default::default()` для незаполненных полей `MessageRow`. Сверить, что `MessageRow` реально `#[derive(Default)]` (открыть `opex-db/src/sessions.rs:682` — там есть поля после `status`, напр. `#[sqlx(default)]`-поле на :694). Если `Default` НЕ выведен — заполнить в `row()` ВСЕ поля структуры явно (не полагаться на `..Default::default()`), иначе тест не скомпилируется.

- [ ] **Step 3:** Run: `cargo check --all-targets -p opex-core` → чисто.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/drift/ crates/opex-core/src/agent/mod.rs
git commit -m "feat(drift): self-baseline detector core (centroid, drift_score, own-turn filter)"
```

---

### Task 3: Per-session baseline-кэш на AgentConfig

**Files:**
- Modify: `crates/opex-core/src/agent/agent_config.rs`
- Modify: `crates/opex-core/src/gateway/handlers/agents/lifecycle.rs:211` (единственная точка `soul_runtime: Arc::default()`)

**Interfaces:**
- Produces: поле `pub drift_baselines: std::sync::Arc<dashmap::DashMap<uuid::Uuid, std::sync::Arc<Vec<f32>>>>` на `AgentConfig` — session_id → baseline_centroid. `Arc::default()` при конструировании.

- [ ] **Step 1: Поле на AgentConfig** (рядом с `soul_runtime`, `agent_config.rs:80`):

```rust
    /// Per-session persona-drift baseline cache (spec stage B §3): session_id →
    /// centroid of the agent's early own assistant-turn embeddings. Established
    /// once per session, reused each turn. Process-local (survives across turns,
    /// resets on agent hot-reload — fail-soft re-establish). `Arc::default()` at
    /// construction. Soft-capped in the drift_probe writer to bound memory.
    pub drift_baselines: std::sync::Arc<dashmap::DashMap<uuid::Uuid, std::sync::Arc<Vec<f32>>>>,
```

- [ ] **Step 2: Конструирование** — в `gateway/handlers/agents/lifecycle.rs`, где `soul_runtime: std::sync::Arc::default(),` (строка ~211), добавить:

```rust
        drift_baselines: std::sync::Arc::default(),
```

Любые тест-литералы `AgentConfig { ... }` сломаются — cargo check покажет; добавить туда же `drift_baselines: std::sync::Arc::default(),`.

- [ ] **Step 3:** Run: `cargo check --all-targets -p opex-core` → чисто.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/agent_config.rs crates/opex-core/src/gateway/handlers/agents/lifecycle.rs
git commit -m "feat(drift): per-session baseline centroid cache on AgentConfig"
```

---

### Task 4: Интеграция `drift_probe` в context_builder

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (трейт `ContextBuilderDeps` + вызов в `build()`)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (`impl ContextBuilderDeps for AgentEngine` — реализация `drift_probe`)

**Interfaces:**
- Consumes: `centroid`/`drift_score`/`own_assistant_texts` (Task 2); `DriftConfig` (Task 1); `drift_baselines` (Task 3); `cfg().embedder`; `opex_db::session_timeline::log_event`.
- Produces: deps-метод `async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: uuid::Uuid)` — fire-and-forget метрика; `(())`, ничего не вставляет в промпт.

- [ ] **Step 1: Трейт-метод** (в `context_builder.rs`, `ContextBuilderDeps`, рядом с `soul_blocks`):

```rust
    /// Persona-drift probe (spec stage B): self-baseline drift score → session_timeline.
    /// Detect+log only, no prompt injection. No-op when `[agent.drift]` disabled or on error.
    async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: uuid::Uuid);
```

Тестовых impl'ов `ContextBuilderDeps` в дереве НЕТ (только `AgentEngine`, подтверждено этапом A) — mock не нужен.

- [ ] **Step 2: Реализация** (в `engine/context_builder.rs`, `impl ContextBuilderDeps for AgentEngine`):

```rust
    async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: uuid::Uuid) {
        let cfg = &self.cfg().agent.drift;
        if !cfg.enabled {
            return;
        }
        if history.len() < cfg.min_history {
            return;
        }
        let agent = self.agent_name();
        let texts = crate::agent::drift::own_assistant_texts(history, agent);
        // нужно ≥ baseline_turns эталонных + ≥1 свежий
        if texts.len() < cfg.baseline_turns + 1 {
            return;
        }
        let embedder = &self.cfg().embedder;

        // baseline: центроид первых baseline_turns собственных ответов (кэш пер-сессия)
        let baselines = &self.cfg().drift_baselines;
        let baseline = if let Some(b) = baselines.get(&session_id) {
            b.clone()
        } else {
            let base_texts: Vec<&str> = texts.iter().take(cfg.baseline_turns).map(|s| s.as_str()).collect();
            let embs = match embedder.embed_batch(&base_texts).await {
                Ok(e) => e,
                Err(e) => { tracing::warn!(agent, error = %e, "drift baseline embed failed"); return; }
            };
            let Some(c) = crate::agent::drift::centroid(&embs) else {
                tracing::warn!(agent, "drift baseline centroid degenerate"); return;
            };
            let arc = std::sync::Arc::new(c);
            // soft-cap: не даём кэшу расти безгранично (спека §3)
            const MAX_BASELINES: usize = 2000;
            if baselines.len() >= MAX_BASELINES {
                if let Some(k) = baselines.iter().next().map(|e| *e.key()) {
                    baselines.remove(&k);
                }
            }
            baselines.insert(session_id, arc.clone());
            arc
        };

        // recent: последний собственный ответ
        let Some(recent_text) = texts.last() else { return };
        let recent = match embedder.embed(recent_text).await {
            Ok(v) => v,
            Err(e) => { tracing::warn!(agent, error = %e, "drift recent embed failed"); return; }
        };
        let score = crate::agent::drift::drift_score(&baseline, &recent);
        let over = score > cfg.threshold;

        // cos напрямую для наблюдаемости (декомпозиция — ревью F13)
        let cos = 1.0 - score;
        let payload = serde_json::json!({
            "drift_score": score,
            "cos_recent_baseline": cos,
            "own_assistant_turns": texts.len(),
            "baseline_turns_used": cfg.baseline_turns,
            "history_len": history.len(),
            "over_threshold": over,
        });
        if let Err(e) = opex_db::session_timeline::log_event(
            &self.cfg().db, session_id, "drift_probe", Some(&payload),
        ).await {
            tracing::warn!(agent, error = %e, "drift timeline write failed");
        }
        if over {
            tracing::warn!(agent, drift_score = score, "persona drift over threshold");
        } else {
            tracing::debug!(agent, drift_score = score, "drift probe");
        }
    }
```

**NB implementer:** сверить точные accessor'ы в `impl ContextBuilderDeps for AgentEngine`: `self.cfg()` → `&AgentConfig` (поля `.agent.drift`, `.embedder`, `.drift_baselines`, `.db`), `self.agent_name() -> &str` — по образцу соседних методов (`soul_blocks`, `select_top_k_tools_semantic` для `cfg().embedder`). `embed_batch` принимает `&[&str]`.

- [ ] **Step 3: Вызов в `build()`** — в `context_builder.rs::build()`, ПОСЛЕ загрузки `history` (переменная `history`, доступна с ~:271-276) и наличия `session_id`, добавить fire-and-forget вызов (детекция читает прошлые ходы; НИЧЕГО не вставляет):

```rust
        // Stage B: persona-drift probe (detect+log only, fail-soft, no injection).
        deps.drift_probe(&history, session_id).await;
```

Разместить после того, как `history` загружена, но до/после сборки промпта — не важно (не влияет на messages). Разместить сразу после блока загрузки history для читаемости. НЕ در subagent/openai-compat путях (они минуют context_builder — спека §6).

- [ ] **Step 4: Тест breakdown-независимости** (unit, context_builder.rs) — drift не трогает ContextBreakdown, отдельного теста не требует; вместо этого добавить в E2E-чеклист (Task 5) проверку записи timeline. Здесь только `cargo check`.

Run: `cargo check --all-targets -p opex-core` → чисто.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(drift): drift_probe wiring — self-baseline score to session_timeline"
```

---

### Task 5: Серверные тесты, деплой, E2E

**Files:** нет новых.

- [ ] **Step 1: Серверный прогон** (троттлированно, изолированный worktree через git bundle, как этап A): `cargo test --bin opex-core` (unit drift: centroid/drift_score/own_texts/config) под `CARGO_BUILD_JOBS=4 nice ionice`, детачед. Плюс `cargo clippy -p opex-core --all-targets -- -D warnings` (make lint гейт). Всё зелено.

- [ ] **Step 2: Деплой** — спросить разрешение на push; `push` + `bash scripts/server-deploy.sh --skip-build` после ручной троттлированной `cargo build --release --features opex-core/gemini-cloudcode -p opex-core -p opex-watchdog -p opex-memory-worker`. UI не нужен (v1 без UI). Миграций нет.

- [ ] **Step 3: E2E** (на одном агенте):

1. `[agent.drift] enabled = true` в TOML агента → рестарт core → `curl /health` ok.
2. Короткая сессия (< min_history) → в `session_timeline` НЕТ `drift_probe` записей.
3. Длинная сессия, где собеседник навязывает чужой регистр → `SELECT payload FROM session_timeline WHERE event_type='drift_probe' AND session_id=... ORDER BY created_at` — drift_score растёт по ходам, при превышении порога `over_threshold=true` + warn-лог.
4. Мультиагент (если применимо): в pool-сессии baseline/recent считаются только по своим ответам.
5. Регрессия: `enabled=false` агент → нет `drift_probe` записей, промпт не меняется.
6. Fail-soft: остановить toolgate → ход работает, drift пропущен (warn в логах).

- [ ] **Step 4:** Наблюдение drift_score на живом трафике ~неделю → тюнинг `threshold` → решение о фазе 2 (коррекция A-anchor).

---

## Покрытие спеки (self-check)

| Спека | Таск |
| --- | --- |
| §1 архитектура (detect+log, no file/injection/LLM/table) | 2, 4 |
| §2 детектор (self-baseline, drift_score, own-turn filter, agent_id, tool-only skip, gates) | 2, 4 |
| §3 baseline-кэш (per-session, soft-cap) | 3, 4 |
| §4 наблюдаемость (session_timeline drift_probe, декомпозиция) | 4 |
| §5 конфиг `[agent.drift]` + валидация | 1 |
| §6 интеграция (drift_probe deps, cfg().embedder, absent on subagent/openai) | 4 |
| §7 стоимость (+1 embed/ход) | 4 (embed_batch baseline + embed recent) |
| §8 fail-soft | 4 (все ошибки → warn+return) |
| §9 тесты | по таскам + сервер (Task 5) |
| §10 фаза 2 | вне плана (документировано в спеке) |
