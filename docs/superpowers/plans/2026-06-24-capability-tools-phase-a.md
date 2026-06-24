# Capability-инструменты (Фаза A) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Активной capability в реестре провайдеров достаточно, чтобы агент получил встроенный инструмент (`generate_image`, `synthesize_speech`, `search_web`, `transcribe_audio`, `analyze_image`) — без отдельных YAML-файлов; описание инструмента содержит имя топ-приоритетного провайдера.

**Architecture:** Capability-инструменты — это `YamlToolDef`, построенные в рантайме из статического реестра (текущие 5 YAML переезжают в Rust-const) с инъекцией топ-провайдера в `description`. Единая функция `resolve_tool()` (YAML-файл ИЛИ capability) подставляется в существующие точки `find_yaml_tool`, что переиспользует весь execute-блок (`to_tool_definition`, `execute_yaml_channel_action`, `execute_binary`) без дублирования. Без failover/sticky (это Фаза B).

**Tech Stack:** Rust 2024, sqlx (PostgreSQL), serde_yaml, axum. Тесты: `cargo test`, DB-тесты через `#[sqlx::test]`.

## Global Constraints

- rustls-tls only, никакого OpenSSL (копировать из CLAUDE.md verbatim).
- Точные имена параметров инструментов = 1:1 из текущих YAML (LLM-контракт): `generate_image{prompt,size,quality}`, `synthesize_speech{text}`, `search_web{query,max_results}` (БЕЗ `provider`), `transcribe_audio{audio_url,language}`, `analyze_image{image_url,question,language}`.
- Hard cutover: обратной совместимости со старыми YAML НЕ требуется.
- Без failover/sticky/каскада/правок toolgate — это Фаза B.
- Коммиты без `Co-Authored-By`. Работа в `master`. Не пушить без явного разрешения.
- `make check` (`cargo check --all-targets`) и `make lint` (`clippy -D warnings`) должны быть зелёными.

---

## File Structure

- **Create** `crates/opex-core/src/agent/capability_tools.rs` — реестр const-спецификаций (5 YAML как `&str`), парсинг в `YamlToolDef`, инъекция провайдера, `capability_tool_defs(db)`, `find_capability_tool(db,name)`, `resolve_tool(...)`, `CAPABILITY_TOOL_NAMES`.
- **Modify** `crates/opex-core/src/agent/mod.rs` — `pub mod capability_tools;`
- **Modify** `crates/opex-core/src/agent/context_builder.rs` — влить capability defs в tool_list; удалить `augment_search_web_description`.
- **Modify** `crates/opex-core/src/agent/engine/context_builder.rs` — реализация нового deps-метода; `available_tool_names` + capability имена.
- **Modify** `crates/opex-core/src/agent/engine_dispatch.rs` — `find_yaml_tool` → `resolve_tool` (YAML ∪ capability).
- **Modify** `crates/opex-core/src/agent/engine/run.rs` — `maybe_auto_tts` через `resolve_tool`.
- **Modify** `crates/opex-core/src/agent/pipeline/handlers.rs` — `handle_tool_test` через `resolve_tool`.
- **Modify** `crates/opex-core/src/agent/engine/mod.rs` — `has_tool`/`has_search` учитывают capability.
- **Modify** `crates/opex-core/src/agent/pipeline/subagent.rs` — `SUBAGENT_DENIED_TOOLS` += 5 имён.
- **Modify** `crates/opex-core/src/agent/workspace.rs` — текст про `search_web(provider=…)`.
- **Modify** `crates/opex-core/src/skills/mod.rs` — `audit_all_skills_required_tools_exist` known-set; `media_processing_is_video_only`.
- **Modify** `crates/opex-core/src/gateway/handlers/yaml_tools.rs` — `/api/yaml-tools` включает capability как read-only.
- **Delete** `workspace/tools/{generate_image,synthesize_speech,search_web,transcribe_audio,analyze_image}.yaml`.
- **Modify** скиллы `workspace/skills/{web-search,daily-briefing}.md` — убрать `provider=` из `search_web`.

---

### Task 1: Модуль `capability_tools` — реестр спецификаций и парсинг

**Files:**
- Create: `crates/opex-core/src/agent/capability_tools.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (добавить `pub mod capability_tools;`)
- Test: внутри `capability_tools.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub struct CapabilitySpec { pub capability: &'static str, pub tool_name: &'static str, pub yaml: &'static str }`
  - `pub fn capability_specs() -> &'static [CapabilitySpec]`
  - `pub const CAPABILITY_TOOL_NAMES: [&str; 5]`
  - `pub fn parse_spec(spec: &CapabilitySpec) -> anyhow::Result<crate::tools::yaml_tools::YamlToolDef>`

- [ ] **Step 1: Перенести 5 YAML в const-строки.** Скопировать ТЕКУЩЕЕ содержимое каждого из `workspace/tools/{generate_image,synthesize_speech,search_web,transcribe_audio,analyze_image}.yaml` в `const`-строки. Для `search_web` — удалить из `parameters` блок `provider` и из `body_template` убрать `{{#if provider}}…{{/if}}` (оставить `{"query": "{{query}}"{{#if max_results}}, "max_results": {{max_results}}{{/if}}}`).

```rust
//! Built-in capability tools: один инструмент на активную media-capability.
//! Спецификации = бывшие workspace/tools/*.yaml, перенесённые в код.
//! Описание дополняется именем топ-приоритетного активного провайдера.

use crate::tools::yaml_tools::YamlToolDef;

pub struct CapabilitySpec {
    pub capability: &'static str,
    pub tool_name: &'static str,
    pub yaml: &'static str,
}

pub const CAPABILITY_TOOL_NAMES: [&str; 5] = [
    "generate_image",
    "synthesize_speech",
    "search_web",
    "transcribe_audio",
    "analyze_image",
];

const GENERATE_IMAGE: &str = r#"
name: generate_image
description: "Generate images from a text description. Use for illustrations, diagrams, and art. The prompt must be in English. NOTE: the image is displayed in chat automatically — do NOT use canvas or other tools to show it."
endpoint: "http://localhost:9011/generate-image"
method: POST
parameters:
  prompt: { type: string, required: true, location: body, description: "Image description in English" }
  size: { type: string, required: false, location: body, description: "Size: 1024x1024, 1792x1024, 1024x1792, 512x512", default: "1024x1024" }
  quality: { type: string, required: false, location: body, description: "standard (fast) or high (slower, better)", default: "standard" }
channel_action: { action: send_photo, data_field: "_binary" }
status: verified
"#;

const SYNTHESIZE_SPEECH: &str = r#"
name: synthesize_speech
description: "Convert text to a spoken voice message. The audio is delivered to the chat automatically."
endpoint: "http://localhost:9011/v1/audio/speech"
method: POST
parameters:
  text: { type: string, required: true, location: body, description: "Text to speak" }
channel_action: { action: send_voice, data_field: "_binary" }
status: verified
"#;

const SEARCH_WEB: &str = r#"
name: search_web
description: "Web search. Returns results with page-content snippets."
endpoint: "http://localhost:9011/v1/search"
method: POST
parameters:
  query: { type: string, required: true, location: body, description: "Search query" }
  max_results: { type: integer, required: false, location: body, description: "Maximum number of results (default 5)", default: 5 }
body_template: |
  {"query": "{{query}}"{{#if max_results}}, "max_results": {{max_results}}{{/if}}}
response_transform: "$.results"
status: verified
"#;

const TRANSCRIBE_AUDIO: &str = r#"
name: transcribe_audio
description: "Transcribe speech from an audio file URL to text."
endpoint: "http://localhost:9011/transcribe-url"
method: POST
parameters:
  audio_url: { type: string, required: true, location: body, description: "URL of the audio file" }
  language: { type: string, required: false, location: body, description: "Language hint (ISO code)" }
response_transform: "$.text"
status: verified
"#;

const ANALYZE_IMAGE: &str = r#"
name: analyze_image
description: "Analyze/describe an image from a URL or an internal /uploads/ path."
endpoint: "http://localhost:18789/api/vision/analyze"
method: POST
parameters:
  image_url: { type: string, required: true, location: body, description: "Image URL or /uploads/ path" }
  question: { type: string, required: false, location: body, description: "Question about the image" }
  language: { type: string, required: false, location: body, description: "Answer language (ISO code)", default: "ru" }
response_transform: "$.description"
status: verified
"#;

pub fn capability_specs() -> &'static [CapabilitySpec] {
    &[
        CapabilitySpec { capability: "imagegen",  tool_name: "generate_image",    yaml: GENERATE_IMAGE },
        CapabilitySpec { capability: "tts",       tool_name: "synthesize_speech", yaml: SYNTHESIZE_SPEECH },
        CapabilitySpec { capability: "websearch", tool_name: "search_web",        yaml: SEARCH_WEB },
        CapabilitySpec { capability: "stt",       tool_name: "transcribe_audio",  yaml: TRANSCRIBE_AUDIO },
        CapabilitySpec { capability: "vision",    tool_name: "analyze_image",     yaml: ANALYZE_IMAGE },
    ]
}

pub fn parse_spec(spec: &CapabilitySpec) -> anyhow::Result<YamlToolDef> {
    let def: YamlToolDef = serde_yaml::from_str(spec.yaml)?;
    Ok(def)
}
```

> NB по точным значениям: перед коммитом сверить `endpoint`/`response_transform`/`channel_action`/имена параметров каждого блока с реальным файлом в `workspace/tools/` (особенно `analyze_image`: `image_url`/`question`/`language` + `$.description`; `transcribe_audio`: `audio_url` + `$.text`). Значения выше взяты из спеки; файл — источник истины на момент написания.

- [ ] **Step 2: Зарегистрировать модуль.** В `crates/opex-core/src/agent/mod.rs` добавить строку `pub mod capability_tools;` рядом с другими `pub mod` (например после `pub mod workspace;`).

- [ ] **Step 3: Написать тест парсинга.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_specs_parse_with_correct_names() {
        for spec in capability_specs() {
            let def = parse_spec(spec).unwrap_or_else(|e| panic!("{} failed: {e}", spec.tool_name));
            assert_eq!(def.name, spec.tool_name, "name mismatch for {}", spec.capability);
            assert!(!def.description.is_empty());
        }
        assert_eq!(capability_specs().len(), CAPABILITY_TOOL_NAMES.len());
    }

    #[test]
    fn search_web_has_no_provider_param() {
        let spec = capability_specs().iter().find(|s| s.tool_name == "search_web").unwrap();
        let def = parse_spec(spec).unwrap();
        assert!(!def.parameters.contains_key("provider"), "search_web must not expose LLM provider param");
    }

    #[test]
    fn binary_tools_have_channel_action() {
        for name in ["generate_image", "synthesize_speech"] {
            let spec = capability_specs().iter().find(|s| s.tool_name == name).unwrap();
            let def = parse_spec(spec).unwrap();
            assert!(def.channel_action.is_some(), "{name} must have channel_action");
        }
    }
}
```

- [ ] **Step 4: Запустить тесты.**

Run: `cargo test -p opex-core capability_tools::tests -- --nocapture`
Expected: PASS (3 теста).

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/capability_tools.rs crates/opex-core/src/agent/mod.rs
git commit -m "feat(capability-tools): статический реестр спецификаций + парсинг"
```

---

### Task 2: Фабрика с провайдером — `capability_tool_defs` и `find_capability_tool`

**Files:**
- Modify: `crates/opex-core/src/agent/capability_tools.rs`
- Test: `crates/opex-core/src/agent/capability_tools.rs` (`#[sqlx::test]`)

**Interfaces:**
- Consumes: `crate::db::providers::get_active_providers(db, capability) -> sqlx::Result<Vec<(String, i32)>>` (порядок по приоритету, топ — первый).
- Produces:
  - `pub async fn capability_tool_defs(db: &sqlx::PgPool) -> Vec<YamlToolDef>` — по одному `YamlToolDef` на capability с ≥1 активным провайдером, описание дополнено топ-провайдером.
  - `pub async fn find_capability_tool(db: &sqlx::PgPool, name: &str) -> Option<YamlToolDef>`
  - `pub fn is_capability_tool(name: &str) -> bool`

- [ ] **Step 1: Написать failing-тесты (DB).**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn defs_include_active_provider_in_description(pool: sqlx::PgPool) {
    crate::db::providers::set_provider_active_list(&pool, "imagegen", &[("flux-fal".into(), 1)])
        .await.unwrap();
    let defs = capability_tool_defs(&pool).await;
    let gi = defs.iter().find(|d| d.name == "generate_image").expect("generate_image present");
    assert!(gi.description.contains("flux-fal"), "desc must name provider: {}", gi.description);
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_def_when_capability_has_no_active_provider(pool: sqlx::PgPool) {
    let defs = capability_tool_defs(&pool).await;
    assert!(defs.iter().all(|d| d.name != "generate_image"),
        "no imagegen provider → no generate_image tool");
}

#[sqlx::test(migrations = "../../migrations")]
async fn find_returns_top_priority_provider(pool: sqlx::PgPool) {
    crate::db::providers::set_provider_active_list(
        &pool, "tts", &[("low".into(), 10), ("top".into(), 1)]).await.unwrap();
    let def = find_capability_tool(&pool, "synthesize_speech").await.expect("found");
    assert!(def.description.contains("top"));
    assert!(!def.description.contains("low"));
}
```

- [ ] **Step 2: Запустить — убедиться, что не компилируется/падает.**

Run: `cargo test -p opex-core capability_tools -- --nocapture` (требует `DATABASE_URL`; иначе `make test-db`)
Expected: FAIL — `capability_tool_defs`/`find_capability_tool` не существуют.

- [ ] **Step 3: Реализовать фабрику.**

```rust
fn with_provider(mut def: YamlToolDef, provider: &str) -> YamlToolDef {
    def.description = format!("{} (provider: {provider})", def.description.trim());
    def
}

pub fn is_capability_tool(name: &str) -> bool {
    CAPABILITY_TOOL_NAMES.contains(&name)
}

pub async fn capability_tool_defs(db: &sqlx::PgPool) -> Vec<YamlToolDef> {
    let mut out = Vec::new();
    for spec in capability_specs() {
        let top = match crate::db::providers::get_active_providers(db, spec.capability).await {
            Ok(list) => list.into_iter().next().map(|(name, _)| name),
            Err(e) => {
                tracing::warn!(capability = spec.capability, error = %e, "active providers query failed");
                None
            }
        };
        let Some(provider) = top else { continue };
        match parse_spec(spec) {
            Ok(def) => out.push(with_provider(def, &provider)),
            Err(e) => tracing::error!(tool = spec.tool_name, error = %e, "capability spec parse failed"),
        }
    }
    out
}

pub async fn find_capability_tool(db: &sqlx::PgPool, name: &str) -> Option<YamlToolDef> {
    let spec = capability_specs().iter().find(|s| s.tool_name == name)?;
    let top = crate::db::providers::get_active_providers(db, spec.capability)
        .await.ok()?.into_iter().next().map(|(n, _)| n)?;
    parse_spec(spec).ok().map(|d| with_provider(d, &top))
}
```

- [ ] **Step 4: Запустить тесты.**

Run: `make test-db` (или `cargo test -p opex-core capability_tools` с `DATABASE_URL`)
Expected: PASS (3 DB-теста + 3 unit из Task 1).

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/capability_tools.rs
git commit -m "feat(capability-tools): фабрика defs/find с топ-провайдером в описании"
```

---

### Task 3: Влить capability-инструменты в список инструментов LLM

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (trait `ContextBuilderDeps` + сборка `tool_list` ~539-560)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (реализация метода ~362)
- Test: `crates/opex-core/src/agent/capability_tools.rs` (assert на `to_tool_definition`)

**Interfaces:**
- Consumes: `capability_tool_defs(db)`, `YamlToolDef::to_tool_definition()`.
- Produces: новый метод трейта `async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef>;`

- [ ] **Step 1: Объявить метод в трейте.** В `context_builder.rs` рядом с `fn internal_tool_definitions(&self)` (~139) добавить:

```rust
    async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef>;
```

- [ ] **Step 2: Реализовать в engine.** В `engine/context_builder.rs` рядом с `internal_tool_definitions` (~362) добавить:

```rust
    async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef> {
        crate::agent::capability_tools::capability_tool_defs(&self.cfg().db).await
    }
```

- [ ] **Step 3: Влить в `tool_list`.** В `context_builder.rs` в блоке сборки (после строки `tool_list.extend(yaml_filtered.into_iter().map(|t| t.to_tool_definition()));`, ~551) добавить:

```rust
            // Built-in capability tools (one per active media capability).
            tool_list.extend(
                deps.capability_tool_defs().await.into_iter().map(|t| t.to_tool_definition()),
            );
```

- [ ] **Step 4: Тест — `to_tool_definition` несёт провайдера.** В `capability_tools.rs`:

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn tool_definition_description_carries_provider(pool: sqlx::PgPool) {
    crate::db::providers::set_provider_active_list(&pool, "websearch", &[("searxng".into(), 1)])
        .await.unwrap();
    let def = find_capability_tool(&pool, "search_web").await.unwrap();
    let td = def.to_tool_definition();
    assert_eq!(td.name, "search_web");
    assert!(td.description.contains("searxng"));
}
```

- [ ] **Step 5: Проверить компиляцию и тест.**

Run: `make check && make test-db`
Expected: компиляция зелёная; новый тест PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/capability_tools.rs
git commit -m "feat(capability-tools): показывать инструменты в списке LLM"
```

---

### Task 4: `available_tool_names` и `has_tool`/`has_search` учитывают capability

**Files:**
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (`available_tool_names` ~403)
- Modify: `crates/opex-core/src/agent/engine/mod.rs` (`has_tool` ~267)
- Test: `crates/opex-core/src/agent/capability_tools.rs`

**Interfaces:**
- Consumes: `capability_tool_defs(db)`, `is_capability_tool(name)`, `get_active_providers`.

- [ ] **Step 1: Добавить capability в `available_tool_names`.** В `engine/context_builder.rs` внутри `available_tool_names` (после цикла по YAML, ~412) добавить:

```rust
        for def in crate::agent::capability_tools::capability_tool_defs(&self.cfg().db).await {
            tools.push(opex_types::ToolDefinition {
                name: def.name.clone(),
                description: def.description.clone(),
                input_schema: serde_json::json!({}),
            });
        }
```

- [ ] **Step 2: Прочитать текущий `has_tool`/`has_search`.** Открыть `engine/mod.rs:267`. Подтвердить, как сейчас определяется `has_search` (файловая проверка `search_web.yaml`). Заменить файловую проверку для capability-имён на проверку активного провайдера.

```rust
    /// True, если инструмент доступен агенту (system / YAML / capability).
    pub(crate) async fn has_tool(&self, name: &str) -> bool {
        if crate::agent::capability_tools::is_capability_tool(name) {
            return crate::agent::capability_tools::find_capability_tool(&self.cfg().db, name)
                .await.is_some();
        }
        // … существующая логика (system registry / YAML-файл) …
        crate::tools::yaml_tools::find_yaml_tool(&self.cfg().workspace_dir, name).await.is_some()
    }
```

> Сверить точную текущую сигнатуру/тело `has_tool` при открытии файла и встроить ветку capability ПЕРВОЙ. Потребители `has_search` (`context_builder.rs:265`, `subagent_runner.rs:65`, `openai_compat.rs:69`) менять не нужно — они зовут `has_tool("search_web")`.

- [ ] **Step 3: Тест has_tool.** В `capability_tools.rs` через `find_capability_tool` уже покрыто наличие; добавить негатив:

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn find_is_none_without_provider(pool: sqlx::PgPool) {
    assert!(find_capability_tool(&pool, "search_web").await.is_none());
    assert!(find_capability_tool(&pool, "not_a_capability").await.is_none());
}
```

- [ ] **Step 4: Проверка.**

Run: `make check && make test-db`
Expected: зелёная компиляция; тест PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/engine/mod.rs crates/opex-core/src/agent/capability_tools.rs
git commit -m "feat(capability-tools): visibility (available_tool_names + has_tool)"
```

---

### Task 5: Диспетч — `resolve_tool` (YAML-файл ∪ capability)

**Files:**
- Modify: `crates/opex-core/src/agent/capability_tools.rs` (добавить `resolve_tool`)
- Modify: `crates/opex-core/src/agent/engine_dispatch.rs` (~169)
- Test: `crates/opex-core/src/agent/capability_tools.rs`

**Interfaces:**
- Produces: `pub async fn resolve_tool(workspace_dir: &str, db: &sqlx::PgPool, name: &str) -> Option<YamlToolDef>` — сначала capability (приоритет над файлом, hard cutover), затем YAML-файл.

- [ ] **Step 1: Реализовать `resolve_tool`.**

```rust
/// Разрешить имя инструмента в YamlToolDef: capability имеет приоритет над
/// YAML-файлом (hard cutover — capability-имена зарезервированы).
pub async fn resolve_tool(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    name: &str,
) -> Option<YamlToolDef> {
    if is_capability_tool(name) {
        return find_capability_tool(db, name).await;
    }
    crate::tools::yaml_tools::find_yaml_tool(workspace_dir, name).await
}
```

- [ ] **Step 2: Подставить в dispatch.** В `engine_dispatch.rs` заменить вызов `find_yaml_tool` (~169) на:

```rust
            if let Some(yaml_tool) = crate::agent::capability_tools::resolve_tool(
                &self.cfg().workspace_dir,
                &self.cfg().db,
                name,
            ).await {
```

(остальной блок исполнения — `channel_action` / cache / `execute_oauth` — без изменений; capability-tool имеет `status: Verified`, поэтому draft-гейт не сработает).

- [ ] **Step 3: Тест resolve.**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn resolve_prefers_capability(pool: sqlx::PgPool) {
    crate::db::providers::set_provider_active_list(&pool, "imagegen", &[("p".into(), 1)])
        .await.unwrap();
    let def = resolve_tool("/nonexistent-workspace", &pool, "generate_image").await.unwrap();
    assert_eq!(def.name, "generate_image");
    assert!(def.description.contains("(provider: p)"));
}
```

- [ ] **Step 4: Проверка.**

Run: `make check && make test-db`
Expected: зелёная; тест PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/capability_tools.rs crates/opex-core/src/agent/engine_dispatch.rs
git commit -m "feat(capability-tools): dispatch через resolve_tool (capability ∪ yaml)"
```

---

### Task 6: Системные обращения по имени — `maybe_auto_tts` и `handle_tool_test`

**Files:**
- Modify: `crates/opex-core/src/agent/engine/run.rs` (~248)
- Modify: `crates/opex-core/src/agent/pipeline/handlers.rs` (~547)

**Interfaces:**
- Consumes: `resolve_tool(workspace_dir, db, name)`.

- [ ] **Step 1: Переключить `maybe_auto_tts`.** В `engine/run.rs:248` заменить:

```rust
        let tool = match crate::agent::capability_tools::resolve_tool(
            &self.cfg().workspace_dir, &self.cfg().db, "synthesize_speech",
        ).await {
            Some(t) => t,
            None => {
                tracing::warn!("auto-tts: synthesize_speech tool not found (no tts provider active?)");
                return;
            }
        };
```

- [ ] **Step 2: Переключить `handle_tool_test`.** В `handlers.rs:547` заменить `find_yaml_tool(workspace_dir, tool_name)` на `resolve_tool(workspace_dir, db, tool_name)`. Проверить, что `db` доступен в сигнатуре `handle_tool_test`; если нет — пробросить `&sqlx::PgPool` параметром от вызывающего (искать вызов `handle_tool_test(` в `tool_handlers/`).

```rust
    let tool = match crate::agent::capability_tools::resolve_tool(
        workspace_dir, db, tool_name,
    ).await {
        Some(t) => t,
        None => return format!("Tool '{}' not found. Use tool_list() to see available tools.", tool_name),
    };
```

- [ ] **Step 3: Проверка компиляции.**

Run: `make check`
Expected: зелёная (если `db` не был в сигнатуре — добавить параметр и обновить call-site).

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/agent/engine/run.rs crates/opex-core/src/agent/pipeline/handlers.rs
git commit -m "fix(capability-tools): auto-tts и tool_test через resolve_tool"
```

---

### Task 7: Убрать `augment_search_web_description` и параметр `provider` из промптов

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (~558-560 вызов + ~668 определение + тесты ~1122-1158)
- Modify: `crates/opex-core/src/agent/workspace.rs` (~495)
- Modify: `workspace/skills/web-search.md`, `workspace/skills/daily-briefing.md`

**Interfaces:**
- Удаляет: `augment_search_web_description`, `active_websearch_providers` (если больше не используется — проверить).

- [ ] **Step 1: Убрать вызов augment.** В `context_builder.rs` удалить строки (~558-560):

```rust
            // Augment search_web description with live active-provider list.
            let ws_providers = deps.active_websearch_providers().await;
            augment_search_web_description(&mut all_tools, &ws_providers);
```

- [ ] **Step 2: Удалить функцию и её тесты.** Удалить `fn augment_search_web_description(...)` (~668) и тесты `augments_search_web_*` (~1122-1158). Если `active_websearch_providers` (trait ~111 + impl `engine/context_builder.rs:315`) больше нигде не зовётся — удалить и его (проверить `grep active_websearch_providers`). Если зовётся ещё где-то — оставить.

- [ ] **Step 3: Поправить промпт workspace.rs.** В `workspace.rs:495` убрать упоминание `provider` из описания `search_web` (заменить «`search_web` (optionally pass `provider`…)» на «`search_web` for web search»).

- [ ] **Step 4: Поправить скиллы.** В `workspace/skills/web-search.md` и `daily-briefing.md` убрать все `search_web(provider="…")` → `search_web(query="…")`.

- [ ] **Step 5: Проверка.**

Run: `make check && cargo test -p opex-core context_builder -- --nocapture`
Expected: зелёная; удалённые тесты отсутствуют, остальные PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/workspace.rs workspace/skills/web-search.md workspace/skills/daily-briefing.md
git commit -m "refactor(capability-tools): описание search_web = единый провайдер, убран augment"
```

---

### Task 8: Subagent denylist

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs` (~17 `SUBAGENT_DENIED_TOOLS`)

- [ ] **Step 1: Прочитать текущий `SUBAGENT_DENIED_TOOLS`** (subagent.rs:17) и добавить 5 имён.

```rust
pub const SUBAGENT_DENIED_TOOLS: &[&str] = &[
    // … существующие …
    "generate_image",
    "synthesize_speech",
    "analyze_image",
    "transcribe_audio",
    "search_web",
];
```

> Проверить: возможно `search_web` стоит ОСТАВИТЬ доступным субагентам (полезен для research-субагентов). Минимально-безопасный дефолт из спеки — деним все 5 ради инварианта `integration_fse_security.rs`. Если решено разрешить `search_web` — убрать его из списка и убедиться, что security-тест не проверяет именно `search_web`.

- [ ] **Step 2: Запустить security-регресс.**

Run: `cargo test -p opex-core --test integration_fse_security`
Expected: PASS (субагент не имеет `analyze_image`).

- [ ] **Step 3: Commit.**

```bash
git add crates/opex-core/src/agent/pipeline/subagent.rs
git commit -m "security(capability-tools): capability-инструменты в SUBAGENT_DENIED_TOOLS"
```

---

### Task 9: Hard cutover — удалить 5 YAML, починить skill-audit

**Files:**
- Delete: `workspace/tools/{generate_image,synthesize_speech,search_web,transcribe_audio,analyze_image}.yaml`
- Modify: `crates/opex-core/src/skills/mod.rs` (`audit_all_skills_required_tools_exist` ~766; `media_processing_is_video_only` ~762)

- [ ] **Step 1: Удалить файлы.**

```bash
git rm workspace/tools/generate_image.yaml workspace/tools/synthesize_speech.yaml workspace/tools/search_web.yaml workspace/tools/transcribe_audio.yaml workspace/tools/analyze_image.yaml
```

- [ ] **Step 2: Запустить skill-audit — убедиться, что падает.**

Run: `cargo test -p opex-core skills::tests::audit_all_skills_required_tools_exist -- --nocapture`
Expected: FAIL — скиллы ссылаются на `search_web`/`generate_image`/`analyze_image`, которых больше нет в known-set.

- [ ] **Step 3: Добавить capability-имена в known-set.** В `skills/mod.rs` в тесте `audit_all_skills_required_tools_exist` (~766), там где строится множество известных инструментов (`all_system_tool_names()` + YAML с диска), добавить:

```rust
        for name in crate::agent::capability_tools::CAPABILITY_TOOL_NAMES {
            known.insert(name.to_string());
        }
```

- [ ] **Step 4: Проверить `media_processing_is_video_only`.** Тест `media_processing_is_video_only` (~762) ассертит `tools_required == ["analyze_image"]`. Файл `media-processing.md` не меняем → тест остаётся валидным. Запустить, убедиться.

- [ ] **Step 5: Запустить тесты.**

Run: `cargo test -p opex-core skills -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/skills/mod.rs workspace/tools/
git commit -m "refactor(capability-tools): hard cutover — удалить 5 YAML, починить skill-audit"
```

---

### Task 10: UI — capability-инструменты видны в `/api/yaml-tools` (read-only)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/yaml_tools.rs` (`/api/yaml-tools` listing)

**Interfaces:**
- Consumes: `capability_tool_defs(db)`.

- [ ] **Step 1: Прочитать listing-хендлер** `api_yaml_tools_list` в `yaml_tools.rs` — понять форму ответа (DTO записи инструмента: name/description/status/endpoint/read_only?).

- [ ] **Step 2: Добавить capability-инструменты в ответ как read-only.** В конце сборки списка добавить по записи на каждый `capability_tool_defs(&db)`, помеченной read-only (например `status: "builtin"` или флаг `builtin: true`), чтобы UI не показывал кнопки verify/disable/delete. Конкретные поля DTO — по образцу существующих записей в этом хендлере.

```rust
    for def in crate::agent::capability_tools::capability_tool_defs(&db).await {
        out.push(/* YamlToolDto по образцу: name=def.name, description=def.description,
                    status="builtin", endpoint=def.endpoint, builtin=true */);
    }
```

- [ ] **Step 3: Проверка.**

Run: `make check`; затем вручную `make doctor` не нужен — проверить через `cargo test -p opex-core yaml_tools` если есть тесты хендлера; иначе — ручной GET `/api/yaml-tools` после `make remote-deploy` (или локального запуска) и убедиться, что 5 capability-инструментов в списке, без verify/disable.

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/gateway/handlers/yaml_tools.rs
git commit -m "feat(capability-tools): показывать capability-инструменты в UI (read-only)"
```

---

### Task 11: Полная проверка + регрессии

**Files:** (без изменений кода — верификация)

- [ ] **Step 1: Полная компиляция и линт.**

Run: `make check && make lint`
Expected: оба зелёные (clippy `-D warnings`).

- [ ] **Step 2: Регресс media_background.**

Run: `cargo test -p opex-core media_background -- --nocapture`
Expected: PASS (image_ready / send_photo / voice — путь не изменился, capability-tool = тот же YamlToolDef-движок).

- [ ] **Step 3: Полный DB-прогон.**

Run: `make test-db`
Expected: PASS (включая capability_tools, skills, fse_security).

- [ ] **Step 4: Финальный коммит (если остались правки) и сводка.**

```bash
git add -A && git commit -m "test(capability-tools): зелёный прогон Фазы A" || echo "nothing to commit"
git log --oneline -12
```

---

## Self-Review (выполнено автором плана)

- **Покрытие спеки (Фаза A):** реестр+парсинг (T1) ✓; фабрика с топ-провайдером (T2) ✓; описание в LLM-списке (T3) ✓; visibility/has_search (T4) ✓; dispatch (T5) ✓; maybe_auto_tts + handle_tool_test (T6) ✓; augment-замена + provider-промпты (T7) ✓; subagent denylist (T8) ✓; удаление YAML + skill-audit (T9) ✓; UI (T10) ✓; регрессии (T11) ✓. Деплой-чистка `~/opex/workspace/tools/` — операционный шаг при `make remote-deploy` (не код), отмечен в спеке §миграция п.9.
- **Плейсхолдеры:** там, где точная текущая структура файла не читалась построчно (T2 `get_active_providers` — подтверждён; T4 `has_tool`, T6 `handle_tool_test` сигнатура, T10 DTO) — дана инструкция «сверить/встроить» с точным путём и образцом, а не «TODO». Код-шаги несут реальный код.
- **Типы:** `resolve_tool`/`find_capability_tool`/`capability_tool_defs` — единые сигнатуры во всех задачах; `YamlToolDef`/`ToolDefinition` — из существующего кода; `get_active_providers(db,cap)->Vec<(String,i32)>` подтверждён.
