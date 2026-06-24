# Capability-инструменты (Фаза A) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Активной capability в реестре провайдеров достаточно, чтобы агент получил встроенный инструмент (`generate_image`, `synthesize_speech`, `search_web`, `transcribe_audio`, `analyze_image`) — без отдельных YAML-файлов; описание инструмента содержит имя топ-приоритетного провайдера.

**Architecture:** Capability-инструменты — это `YamlToolDef`, построенные в рантайме из статического реестра (текущие 5 YAML переезжают в Rust-const) с инъекцией топ-провайдера в `description`. Единая функция `resolve_tool()` (YAML-файл ИЛИ capability) подставляется в существующие точки `find_yaml_tool`, что переиспользует весь execute-блок (`to_tool_definition`, `execute_yaml_channel_action`, `execute_binary`). Все точки обнаружения инструментов (прямой список LLM, `available_tool_names`, dispatcher `lookup.rs`) вливают capability-defs и фильтруют YAML-дубли по имени. Без failover/sticky (это Фаза B).

**Tech Stack:** Rust 2024, sqlx (PostgreSQL), serde_yaml, axum. Тесты: `cargo test`, DB-тесты через `#[sqlx::test]`.

## Global Constraints

- rustls-tls only, никакого OpenSSL.
- Точные имена параметров = 1:1 из текущих YAML (LLM-контракт): `generate_image{prompt,size,quality}`, `synthesize_speech{text}`, `search_web{query,max_results}` (БЕЗ `provider`), `transcribe_audio{audio_url,language}`, `analyze_image{image_url,question,language}`.
- Hard cutover: обратной совместимости со старыми YAML не требуется.
- Без failover/sticky/каскада/правок toolgate — это Фаза B.
- **FK:** `provider_active.provider_name REFERENCES providers(name)` (migration 053). Любой DB-тест, активирующий провайдера, ОБЯЗАН сначала вставить его в `providers` (иначе нарушение FK).
- Коммиты без `Co-Authored-By`. Работа в `master`. Не пушить без явного разрешения.
- `make check` и `make lint` (clippy `-D warnings`) должны быть зелёными; DB-тесты — через `make test-db`.

---

## File Structure

- **Create** `crates/opex-core/src/agent/capability_tools.rs` — реестр const-спецификаций (5 YAML как `&str`), парсинг в `YamlToolDef`, инъекция провайдера, `capability_tool_defs(db)`, `find_capability_tool(db,name)`, `resolve_tool(...)`, `is_capability_tool`, `CAPABILITY_TOOL_NAMES`.
- **Modify** `crates/opex-core/src/agent/mod.rs` — `pub mod capability_tools;`
- **Modify** `crates/opex-core/src/agent/context_builder.rs` — влить capability defs + фильтр YAML-дублей; удалить `augment_search_web_description` и trait-метод `active_websearch_providers`.
- **Modify** `crates/opex-core/src/agent/engine/context_builder.rs` — реализация `capability_tool_defs`; `available_tool_names` + capability; удалить impl `active_websearch_providers`.
- **Modify** `crates/opex-core/src/agent/engine_dispatch.rs` — `find_yaml_tool` → `resolve_tool`.
- **Modify** `crates/opex-core/src/agent/dispatcher/lookup.rs` — влить capability-defs (проброс `db`).
- **Modify** `crates/opex-core/src/agent/tool_handlers/tool_use.rs` — обновить call-site `build_extension_tool_list`/`find_extension_tool` (передать `db`).
- **Modify** `crates/opex-core/src/agent/engine/run.rs` — `maybe_auto_tts` через `resolve_tool`.
- **Modify** `crates/opex-core/src/agent/pipeline/handlers.rs` — `handle_tool_test` через `resolve_tool` (+ параметр `db`).
- **Modify** `crates/opex-core/src/agent/tool_handlers/tools_mgmt.rs` — call-site `handle_tool_test` (передать `deps.db`).
- **Modify** `crates/opex-core/src/agent/engine/mod.rs` — `has_tool` учитывает capability.
- **Modify** `crates/opex-core/src/agent/pipeline/subagent.rs` — `SUBAGENT_DENIED_TOOLS` += 5 имён.
- **Modify** `crates/opex-core/src/agent/workspace.rs` — текст про `search_web(provider=…)`.
- **Modify** `crates/opex-core/src/skills/mod.rs` — `audit_all_skills_required_tools_exist` known-set.
- **Modify** `crates/opex-core/src/gateway/handlers/yaml_tools.rs` — `api_yaml_tools_list_global` включает capability read-only.
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
  - `pub fn is_capability_tool(name: &str) -> bool`
  - `pub fn parse_spec(spec: &CapabilitySpec) -> anyhow::Result<crate::tools::yaml_tools::YamlToolDef>`

- [ ] **Step 1: Создать модуль с const-спецификациями.** Значения сверены с текущими `workspace/tools/*.yaml` (synthesize_speech несёт `timeout: 600` и `body_template` с ключом `input`; search_web — `parallel: true` + `body_template` + `response_transform`).

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

pub fn is_capability_tool(name: &str) -> bool {
    CAPABILITY_TOOL_NAMES.contains(&name)
}

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
description: "Send a voice message to the user via the channel. Use when the user asks to read aloud, send voice, or respond by voice. The voice/timbre is determined by the agent's TTS-provider configuration. IMPORTANT: this tool dispatches the voice in the background and the audio itself IS your reply. After calling it, end your turn — do NOT write acknowledgement text."
endpoint: "http://localhost:9011/v1/audio/speech"
method: POST
timeout: 600
parameters:
  text: { type: string, required: true, location: body, description: "Text to synthesize" }
body_template: |
  {"input": "{{text}}", "response_format": "opus"}
channel_action: { action: send_voice, data_field: "_binary" }
status: verified
"#;

const SEARCH_WEB: &str = r#"
name: search_web
description: "Web search. Returns results with page-content snippets."
endpoint: "http://localhost:9011/v1/search"
method: POST
parallel: true
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
description: "Transcribe audio or a voice message from a URL. Accepts audio_url and optional language. Returns text. Use when receiving a voice message from the user."
endpoint: "http://localhost:9011/transcribe-url"
method: POST
parameters:
  audio_url: { type: string, required: true, location: body, description: "Audio file URL to transcribe" }
  language: { type: string, required: false, location: body, description: "Language code (ru, en, etc.)", default: "ru" }
response_transform: "$.text"
status: verified
"#;

const ANALYZE_IMAGE: &str = r#"
name: analyze_image
description: "Analyze an image from a URL or /uploads/ path. Accepts image_url and an optional question. Returns a text description. Works with both external URLs (https://...) and internal /uploads/ paths."
endpoint: "http://localhost:18789/api/vision/analyze"
method: POST
parameters:
  image_url: { type: string, required: true, location: body, description: "Image URL to analyze (external https:// or internal /uploads/ path)" }
  question: { type: string, required: false, location: body, description: "Question about the image (optional)", default: "Describe what is in the image" }
  language: { type: string, required: false, location: body, description: "Response language (default: ru)", default: "ru" }
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
    Ok(serde_yaml::from_str(spec.yaml)?)
}
```

- [ ] **Step 2: Зарегистрировать модуль.** В `crates/opex-core/src/agent/mod.rs` добавить `pub mod capability_tools;` рядом с другими `pub mod`.

- [ ] **Step 3: Написать тесты парсинга.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_specs_parse_with_correct_names() {
        for spec in capability_specs() {
            let def = parse_spec(spec).unwrap_or_else(|e| panic!("{} failed: {e}", spec.tool_name));
            assert_eq!(def.name, spec.tool_name);
            assert!(!def.description.is_empty());
        }
        assert_eq!(capability_specs().len(), CAPABILITY_TOOL_NAMES.len());
    }

    #[test]
    fn search_web_has_no_provider_param() {
        let spec = capability_specs().iter().find(|s| s.tool_name == "search_web").unwrap();
        assert!(!parse_spec(spec).unwrap().parameters.contains_key("provider"));
    }

    #[test]
    fn tts_keeps_body_template_and_timeout() {
        let spec = capability_specs().iter().find(|s| s.tool_name == "synthesize_speech").unwrap();
        let def = parse_spec(spec).unwrap();
        assert_eq!(def.timeout, 600);
        let bt = def.body_template.as_deref().unwrap_or("");
        assert!(bt.contains("\"input\""), "TTS body must use 'input' key: {bt}");
    }

    #[test]
    fn binary_tools_have_channel_action() {
        for name in ["generate_image", "synthesize_speech"] {
            let spec = capability_specs().iter().find(|s| s.tool_name == name).unwrap();
            assert!(parse_spec(spec).unwrap().channel_action.is_some());
        }
    }
}
```

- [ ] **Step 4: Запустить тесты.**

Run: `cargo test -p opex-core capability_tools::tests -- --nocapture`
Expected: PASS (4 теста).

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
- Consumes: `crate::db::providers::get_active_providers(db, capability) -> sqlx::Result<Vec<(String, i32)>>` (топ — первый).
- Produces:
  - `pub async fn capability_tool_defs(db: &sqlx::PgPool) -> Vec<YamlToolDef>`
  - `pub async fn find_capability_tool(db: &sqlx::PgPool, name: &str) -> Option<YamlToolDef>`

- [ ] **Step 1: Реализовать фабрику.**

```rust
fn with_provider(mut def: YamlToolDef, provider: &str) -> YamlToolDef {
    def.description = format!("{} (provider: {provider})", def.description.trim());
    def
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

- [ ] **Step 2: Добавить сид-хелпер и DB-тесты.** Хелпер удовлетворяет FK `provider_active → providers(name)` (см. образец в `db/providers.rs:220-222`).

```rust
#[cfg(test)]
async fn seed_provider(pool: &sqlx::PgPool, name: &str, capability: &str, driver: &str) {
    sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES ($1,$2,$3,true)")
        .bind(name).bind(capability).bind(driver)
        .execute(pool).await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn defs_include_active_provider_in_description(pool: sqlx::PgPool) {
    seed_provider(&pool, "flux-fal", "imagegen", "fal").await;
    crate::db::providers::set_provider_active_list(&pool, "imagegen", &[("flux-fal".into(), 1)])
        .await.unwrap();
    let defs = capability_tool_defs(&pool).await;
    let gi = defs.iter().find(|d| d.name == "generate_image").expect("generate_image present");
    assert!(gi.description.contains("flux-fal"), "desc must name provider: {}", gi.description);
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_def_when_capability_has_no_active_provider(pool: sqlx::PgPool) {
    let defs = capability_tool_defs(&pool).await;
    assert!(defs.iter().all(|d| d.name != "generate_image"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn find_returns_top_priority_provider(pool: sqlx::PgPool) {
    seed_provider(&pool, "low", "tts", "edge").await;
    seed_provider(&pool, "top", "tts", "silero").await;
    crate::db::providers::set_provider_active_list(
        &pool, "tts", &[("low".into(), 10), ("top".into(), 1)]).await.unwrap();
    let def = find_capability_tool(&pool, "synthesize_speech").await.expect("found");
    assert!(def.description.contains("top"));
    assert!(!def.description.contains("low"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn find_is_none_without_provider(pool: sqlx::PgPool) {
    assert!(find_capability_tool(&pool, "search_web").await.is_none());
    assert!(find_capability_tool(&pool, "not_a_capability").await.is_none());
}
```

- [ ] **Step 3: Запустить DB-тесты.**

Run: `make test-db` (или `cargo test -p opex-core capability_tools` с заданным `DATABASE_URL`)
Expected: PASS (4 DB-теста + 4 unit из Task 1).

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/agent/capability_tools.rs
git commit -m "feat(capability-tools): фабрика defs/find с топ-провайдером (+ FK-сид в тестах)"
```

---

### Task 3: Влить capability-инструменты в список инструментов LLM (+ фильтр дублей)

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (trait + сборка `tool_list` ~539-551)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (реализация ~362)
- Test: `crates/opex-core/src/agent/capability_tools.rs`

**Interfaces:**
- Produces: метод трейта `async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef>;`

- [ ] **Step 1: Объявить метод в трейте.** В `context_builder.rs` рядом с `fn internal_tool_definitions(&self)` (~139):

```rust
    async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef>;
```

- [ ] **Step 2: Реализовать в engine.** В `engine/context_builder.rs` рядом с `internal_tool_definitions` (~362):

```rust
    async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef> {
        crate::agent::capability_tools::capability_tool_defs(&self.cfg().db).await
    }
```

- [ ] **Step 3: Влить в `tool_list` + отфильтровать YAML-дубли.** В `context_builder.rs`: (a) перед строкой `tool_list.extend(yaml_filtered.into_iter()...)` (~551) добавить фильтр, (b) после неё — capability-defs. Фильтр-страховка защищает от дубля независимо от удаления файлов и 30-сек YAML-кэша.

```rust
            // Capability-имена зарезервированы за встроенными инструментами —
            // выкинуть одноимённые YAML, чтобы не было дубля в списке LLM.
            yaml_filtered.retain(|t| !crate::agent::capability_tools::is_capability_tool(&t.name));
            tool_list.extend(yaml_filtered.into_iter().map(|t| t.to_tool_definition()));

            // Built-in capability tools (один на активную media-capability).
            tool_list.extend(
                deps.capability_tool_defs().await.into_iter().map(|t| t.to_tool_definition()),
            );
```

- [ ] **Step 4: Тест — `to_tool_definition` несёт провайдера.** В `capability_tools.rs`:

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn tool_definition_description_carries_provider(pool: sqlx::PgPool) {
    seed_provider(&pool, "searxng", "websearch", "searxng").await;
    crate::db::providers::set_provider_active_list(&pool, "websearch", &[("searxng".into(), 1)])
        .await.unwrap();
    let td = find_capability_tool(&pool, "search_web").await.unwrap().to_tool_definition();
    assert_eq!(td.name, "search_web");
    assert!(td.description.contains("searxng"));
}
```

- [ ] **Step 5: Проверка.**

Run: `make check && make test-db`
Expected: компиляция зелёная; тест PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/capability_tools.rs
git commit -m "feat(capability-tools): инструменты в списке LLM + фильтр YAML-дублей"
```

---

### Task 4: `available_tool_names` и `has_tool` учитывают capability

**Files:**
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (`available_tool_names` ~403-417)
- Modify: `crates/opex-core/src/agent/engine/mod.rs` (`has_tool` ~267-296)

- [ ] **Step 1: Добавить capability в `available_tool_names` + отфильтровать дубли.** В `engine/context_builder.rs`: при добавлении YAML-инструментов (цикл ~406-412) пропускать capability-имена, затем добавить capability-defs.

```rust
        for yt in self.load_yaml_tools_cached().await {
            if crate::agent::capability_tools::is_capability_tool(&yt.name) {
                continue;
            }
            tools.push(opex_types::ToolDefinition {
                name: yt.name.clone(),
                description: yt.description.clone(),
                input_schema: serde_json::json!({}),
            });
        }
        for def in crate::agent::capability_tools::capability_tool_defs(&self.cfg().db).await {
            tools.push(opex_types::ToolDefinition {
                name: def.name.clone(),
                description: def.description.clone(),
                input_schema: serde_json::json!({}),
            });
        }
```

- [ ] **Step 2: Capability-ветка в `has_tool`.** Текущий `has_tool` (engine/mod.rs:267) — `async fn has_tool(&self, name: &str) -> bool`, делает ТОЛЬКО файловую проверку `workspace/tools/{name}.yaml` (+ `status: disabled`). Добавить capability-ветку ПЕРВОЙ строкой тела:

```rust
    async fn has_tool(&self, name: &str) -> bool {
        // Capability-инструменты: доступны, если есть активный провайдер.
        if crate::agent::capability_tools::is_capability_tool(name) {
            return crate::agent::capability_tools::find_capability_tool(&self.cfg().db, name)
                .await.is_some();
        }
        // … СУЩЕСТВУЮЩАЯ файловая проверка ниже без изменений …
        let dir = std::path::Path::new(&self.cfg().workspace_dir).join("tools");
        // (оставить остальное тело как есть)
        // …
    }
```

> Потребители `has_tool` (`context_builder.rs:265`, `subagent_runner.rs:65`, `openai_compat.rs:69` — все зовут `executor.has_tool("search_web")`) НЕ трогаются: после удаления `search_web.yaml` (Task 10) блок «Web Search» в промпте теперь определяется активным websearch-провайдером, а не файлом.

- [ ] **Step 3: Проверка.**

Run: `make check && make test-db`
Expected: зелёная компиляция; тесты PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/engine/mod.rs
git commit -m "feat(capability-tools): visibility — available_tool_names + has_tool"
```

---

### Task 5: Диспетч — `resolve_tool` (capability ∪ YAML-файл)

**Files:**
- Modify: `crates/opex-core/src/agent/capability_tools.rs` (добавить `resolve_tool`)
- Modify: `crates/opex-core/src/agent/engine_dispatch.rs` (~169)
- Test: `crates/opex-core/src/agent/capability_tools.rs`

**Interfaces:**
- Produces: `pub async fn resolve_tool(workspace_dir: &str, db: &sqlx::PgPool, name: &str) -> Option<YamlToolDef>` — capability имеет приоритет над YAML-файлом.

- [ ] **Step 1: Реализовать `resolve_tool`.**

```rust
/// Разрешить имя инструмента в YamlToolDef: capability-имена зарезервированы
/// (приоритет над YAML-файлом), иначе — обычный YAML-инструмент.
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

(весь нижеследующий блок — draft-гейт, `github_`-проверка, `channel_action`-ветка ~202-204, cache, `execute_oauth` — без изменений; capability-tool имеет `status: Verified`).

- [ ] **Step 3: Тест resolve.**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn resolve_prefers_capability(pool: sqlx::PgPool) {
    seed_provider(&pool, "p", "imagegen", "fal").await;
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

### Task 6: Dispatcher-путь (`tool_use`) — capability в extension-списке

**Files:**
- Modify: `crates/opex-core/src/agent/dispatcher/lookup.rs` (`build_extension_tool_list` ~29, `find_extension_tool` ~87)
- Modify: `crates/opex-core/src/agent/tool_handlers/tool_use.rs` (call-sites)

**Interfaces:**
- Изменяет сигнатуры: `build_extension_tool_list(..., db: &sqlx::PgPool, mcp)` и `find_extension_tool(..., db: &sqlx::PgPool, mcp)`.

> Почему: при включённом tool-dispatcher прямой список LLM урезается до static-core (`context_builder.rs:575`), а capability-инструменты достигаются через `tool_use → search/describe → find_extension_tool`. Без этой задачи они станут невызываемы для dispatcher-агентов после удаления YAML.

- [ ] **Step 1: Добавить `db` и влить capability-defs в `build_extension_tool_list`.** В `lookup.rs` добавить параметр `db: &sqlx::PgPool` и после YAML-блока (~68):

```rust
    // YAML tools (без capability-имён — они идут отдельным блоком).
    let yaml = crate::tools::yaml_tools::load_yaml_tools(workspace_dir, false).await;
    for t in yaml {
        if (!t.required_base || is_base_agent)
            && !deny.iter().any(|d| d == &t.name)
            && !promoted.contains(&t.name)
            && !crate::agent::capability_tools::is_capability_tool(&t.name)
        {
            out.push(t.to_tool_definition());
        }
    }

    // Built-in capability tools.
    for def in crate::agent::capability_tools::capability_tool_defs(db).await {
        if !deny.iter().any(|d| d == &def.name) && !promoted.contains(&def.name) {
            out.push(def.to_tool_definition());
        }
    }
```

- [ ] **Step 2: Прокинуть `db` в `find_extension_tool`.** Добавить параметр `db` и передать в `build_extension_tool_list`.

- [ ] **Step 3: Обновить call-sites.** В `tool_handlers/tool_use.rs` найти вызовы `build_extension_tool_list(`/`find_extension_tool(` и передать `&self.cfg().db` (или доступный пул — сверить, как там получается engine/cfg). Запустить `grep build_extension_tool_list|find_extension_tool` по `crates/opex-core/src` — обновить ВСЕ вызовы.

- [ ] **Step 4: Проверка.**

Run: `make check`
Expected: зелёная (все call-sites обновлены).

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/dispatcher/lookup.rs crates/opex-core/src/agent/tool_handlers/tool_use.rs
git commit -m "feat(capability-tools): доступны через tool_use dispatcher"
```

---

### Task 7: Системные обращения по имени — `maybe_auto_tts` и `handle_tool_test`

**Files:**
- Modify: `crates/opex-core/src/agent/engine/run.rs` (`maybe_auto_tts` ~248)
- Modify: `crates/opex-core/src/agent/pipeline/handlers.rs` (`handle_tool_test` ~529 сигнатура, ~547 вызов)
- Modify: `crates/opex-core/src/agent/tool_handlers/tools_mgmt.rs` (call-site ~31)

- [ ] **Step 1: Переключить `maybe_auto_tts`.** В `engine/run.rs:248` заменить:

```rust
        let tool = match crate::agent::capability_tools::resolve_tool(
            &self.cfg().workspace_dir, &self.cfg().db, "synthesize_speech",
        ).await {
            Some(t) => t,
            None => {
                tracing::warn!("auto-tts: synthesize_speech unavailable (no tts provider active?)");
                return;
            }
        };
```

- [ ] **Step 2: Добавить `db` в сигнатуру `handle_tool_test`.** Текущая сигнатура (`handlers.rs:529`): `handle_tool_test(workspace_dir, http_client, ssrf_client, secrets, agent_name, oauth, args)`. Добавить `db: &sqlx::PgPool` (например после `workspace_dir`).

- [ ] **Step 3: Использовать `resolve_tool`.** В `handlers.rs:547` заменить:

```rust
    let tool = match crate::agent::capability_tools::resolve_tool(
        workspace_dir, db, tool_name,
    ).await {
        Some(t) => t,
        None => return format!("Tool '{}' not found. Use tool_list() to see available tools.", tool_name),
    };
```

- [ ] **Step 4: Обновить call-site.** В `tool_handlers/tools_mgmt.rs:31` (`ToolTestHandler::handle`) передать `deps.db` в `handle_tool_test(...)` (в `ToolDeps` есть `pub db: &'a PgPool`, tool_registry.rs:29).

- [ ] **Step 5: Проверка.**

Run: `make check`
Expected: зелёная.

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/engine/run.rs crates/opex-core/src/agent/pipeline/handlers.rs crates/opex-core/src/agent/tool_handlers/tools_mgmt.rs
git commit -m "fix(capability-tools): auto-tts и tool_test через resolve_tool"
```

---

### Task 8: Убрать `augment_search_web_description` и параметр `provider` из промптов

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (вызов ~558-560; определение `augment_search_web_description` ~668; trait-метод `active_websearch_providers` ~111; тесты ~1122-1160)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (impl `active_websearch_providers` ~315)
- Modify: `crates/opex-core/src/agent/workspace.rs` (~494-495)
- Modify: `workspace/skills/web-search.md`, `workspace/skills/daily-briefing.md`

- [ ] **Step 1: Убрать вызов augment.** В `context_builder.rs` удалить строки ~558-560 (`let ws_providers = …; augment_search_web_description(…);`).

- [ ] **Step 2: Удалить функцию, метод и тесты.** Удалить `fn augment_search_web_description(...)` (~668). Удалить trait-метод `active_websearch_providers` (декл `context_builder.rs:111`) И его impl (`engine/context_builder.rs:315`) — иначе `make check` упадёт на несоответствии trait/impl. Удалить три теста по именам: `augments_search_web_with_active_providers`, `augment_search_web_no_providers`, `augment_search_web_noop_when_tool_absent` (~1122-1160).

- [ ] **Step 3: Поправить промпт workspace.rs.** В `workspace.rs:494-495` убрать упоминание `provider` из описания `search_web` (заменить «`search_web` (optionally pass `provider`…)» на «`search_web` for web search»).

- [ ] **Step 4: Поправить скиллы.** В `workspace/skills/web-search.md` и `workspace/skills/daily-briefing.md` заменить все `search_web(provider="…")` → `search_web(query="…")`.

- [ ] **Step 5: Проверка.**

Run: `make check && cargo test -p opex-core context_builder -- --nocapture`
Expected: зелёная; удалённых тестов нет, остальные PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/workspace.rs workspace/skills/web-search.md workspace/skills/daily-briefing.md
git commit -m "refactor(capability-tools): убран augment/active_websearch_providers, search_web без provider"
```

---

### Task 9: Subagent denylist

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs` (`SUBAGENT_DENIED_TOOLS` ~17-24)

- [ ] **Step 1: Добавить 5 имён.** В `SUBAGENT_DENIED_TOOLS` (сейчас 6 имён) добавить:

```rust
    "generate_image",
    "synthesize_speech",
    "analyze_image",
    "transcribe_audio",
    "search_web",
```

> Если решено разрешить `search_web` research-субагентам — убрать его из списка (остальные 4 деним обязательно).

- [ ] **Step 2: Unit-тест denylist.** В тест-модуле `subagent.rs`:

```rust
#[test]
fn capability_tools_denied_to_subagents() {
    for name in ["generate_image", "synthesize_speech", "analyze_image", "transcribe_audio"] {
        assert!(SUBAGENT_DENIED_TOOLS.contains(&name), "{name} must be denied to subagents");
    }
}
```

- [ ] **Step 3: Запустить.**

Run: `cargo test -p opex-core subagent -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/agent/pipeline/subagent.rs
git commit -m "security(capability-tools): capability-инструменты в SUBAGENT_DENIED_TOOLS"
```

---

### Task 10: Hard cutover — удалить 5 YAML, починить skill-audit

**Files:**
- Delete: `workspace/tools/{generate_image,synthesize_speech,search_web,transcribe_audio,analyze_image}.yaml`
- Modify: `crates/opex-core/src/skills/mod.rs` (`audit_all_skills_required_tools_exist` ~766)

- [ ] **Step 1: Удалить файлы.**

```bash
git rm workspace/tools/generate_image.yaml workspace/tools/synthesize_speech.yaml workspace/tools/search_web.yaml workspace/tools/transcribe_audio.yaml workspace/tools/analyze_image.yaml
```

- [ ] **Step 2: Запустить skill-audit — убедиться, что падает.**

Run: `cargo test -p opex-core skills::tests::audit_all_skills_required_tools_exist -- --nocapture`
Expected: FAIL — скиллы ссылаются на `search_web`/`generate_image`/`analyze_image`, которых нет в known-set.

- [ ] **Step 3: Добавить capability-имена в known-set.** В `skills/mod.rs` в тесте `audit_all_skills_required_tools_exist` (~766), где строится множество известных инструментов, добавить:

```rust
        for name in crate::agent::capability_tools::CAPABILITY_TOOL_NAMES {
            known.insert(name.to_string());
        }
```

> `media_processing_is_video_only` (~762) ассертит `tools_required == ["analyze_image"]` — `media-processing.md` не трогаем, тест остаётся валидным.

- [ ] **Step 4: Запустить тесты.**

Run: `cargo test -p opex-core skills -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/skills/mod.rs workspace/tools/
git commit -m "refactor(capability-tools): hard cutover — удалить 5 YAML, починить skill-audit"
```

---

### Task 11: UI — capability-инструменты в `/api/yaml-tools` (read-only)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/yaml_tools.rs` (`api_yaml_tools_list_global` ~26)

- [ ] **Step 1: Прочитать `api_yaml_tools_list_global`** (~26). DTO — инлайновый `json!({name, description, endpoint, method, status, parameters_count, tags})`, `Vec<Value>`. Хендлер: `State(_state): State<InfraServices>`.

- [ ] **Step 2: Включить capability-инструменты как read-only.** Переименовать `_state` → `state`; в конце сборки списка добавить:

```rust
    for def in crate::agent::capability_tools::capability_tool_defs(&state.db).await {
        out.push(serde_json::json!({
            "name": def.name,
            "description": def.description,
            "endpoint": def.endpoint,
            "method": def.method,
            "status": "builtin",
            "parameters_count": def.parameters.len(),
            "tags": def.tags,
            "builtin": true,
        }));
    }
```

(точную форму полей сверить с существующими записями в этом хендлере; добавить `"builtin": true`, чтобы UI скрыл verify/disable — UI data-driven рендер.)

- [ ] **Step 3: Проверка.**

Run: `make check`
Expected: зелёная. (Ручная проверка GET `/api/yaml-tools` после деплоя: 5 capability-инструментов присутствуют со `status: "builtin"`.)

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/gateway/handlers/yaml_tools.rs
git commit -m "feat(capability-tools): показывать в UI как builtin (read-only)"
```

---

### Task 12: Полная проверка + регрессии

- [ ] **Step 1: Компиляция и линт.**

Run: `make check && make lint`
Expected: оба зелёные.

- [ ] **Step 2: Полный DB-прогон (включая media_background — это `#[sqlx::test]`).**

Run: `make test-db`
Expected: PASS (capability_tools, skills, media_background image_ready/send_photo/voice, fse_security, subagent denylist).

- [ ] **Step 3: Финальный коммит/сводка.**

```bash
git add -A && git commit -m "test(capability-tools): зелёный прогон Фазы A" || echo "nothing to commit"
git log --oneline -14
```

---

## Self-Review (выполнено автором после ревью кода)

- **Исправлено по ревью:** FK-сид провайдеров во всех DB-тестах (хелпер `seed_provider`); `synthesize_speech` сохраняет `body_template`(`input`)+`timeout:600`; добавлена Task 6 (`dispatcher/lookup.rs` — иначе capability недоступны при tool-dispatcher); фильтр YAML-дублей в Task 3/4 (устраняет дубль между вливанием и удалением); явный проброс `db` в `handle_tool_test` + call-site `tools_mgmt.rs`; `has_tool` — точное тело (файловая проверка, capability-ветка первой); удаление `active_websearch_providers` (обе точки); Task 11 — `api_yaml_tools_list_global` + `state.db`; Task 9 — честный unit-тест denylist вместо ложной ссылки на `integration_fse_security`; media_background регресс — через `make test-db`.
- **Покрытие спеки (Фаза A):** все пункты §миграции + компоненты покрыты задачами 1-12. Деплой-чистка `~/opex/workspace/tools/` — операционный шаг (отмечен в спеке п.9), не код.
- **Call-sites `find_yaml_tool`/обнаружения:** engine_dispatch (T5), maybe_auto_tts (T7), handle_tool_test (T7), dispatcher/lookup.rs (T6) — все покрыты.
- **Типы:** `resolve_tool`/`find_capability_tool`/`capability_tool_defs`/`is_capability_tool` — единые сигнатуры; `get_active_providers(db,cap)->Vec<(String,i32)>` и FK-сид подтверждены кодом.
