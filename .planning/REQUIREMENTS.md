# Requirements: HydeClaw v0.29.0 Harness Quality

**Defined:** 2026-05-08
**Core Value:** Стабильная и безопасная AI-платформа с self-hosted фокусом

## v1 Requirements

### Prompt Caching (CACHE)

- [ ] **CACHE-01**: Оператор может включить/выключить prompt caching через `[agent] prompt_cache = true/false` в TOML — флаг уже существует, нужны тест и документация поведения для не-Anthropic провайдеров
- [ ] **CACHE-02**: CLAUDE.md системного агента участвует в cache_control как отдельная 3-я breakpoint (после system prompt и tool definitions)
- [x] **CACHE-03**: Метрики cache_read_input_tokens и cache_creation_input_tokens отображаются в `/api/health/dashboard` и записываются в usage_log
- [ ] **CACHE-04**: Prompt caching применяется только к Anthropic-провайдеру; другие провайдеры игнорируют флаг без ошибки и без изменения поведения

### Auto-Compaction (COMP)

- [ ] **COMP-01**: Авто-компакция срабатывает при достижении порога контекстного окна (настраиваемый `[agent] compaction_threshold`, дефолт 0.85)
- [ ] **COMP-02**: Подсчёт токенов для порога включает `input_tokens + cache_read_input_tokens + cache_creation_input_tokens` (фикс бага: с активным кешированием input_tokens занижен)
- [ ] **COMP-03**: `default_context_for_model()` возвращает 1M токенов для claude-opus-4-7, claude-opus-4-6, claude-sonnet-4-6 вместо ошибочных 200K
- [ ] **COMP-04**: Оператор может задать кастомную инструкцию для компакции через `[agent] compaction_prompt = "..."` в TOML

### Tool defer_loading (DEFER)

- [ ] **DEFER-01**: YAML-инструменты с `defer_loading: true` передаются в LLM только с именем и описанием (stub), полная JSON-схема загружается при dispatch вызова
- [ ] **DEFER-02**: defer_loading-инструменты используют per-pipeline-invocation состояние (не per-engine), исключая кросс-сессионное загрязнение

### Hook API (HOOK)

- [ ] **HOOK-01**: Агент может определить PreToolUse-хук в TOML с доступом к имени инструмента и аргументам
- [ ] **HOOK-02**: Агент может определить PostToolUse-хук с доступом к результату выполнения инструмента
- [ ] **HOOK-03**: SessionStart-хук срабатывает только при `reentry_mode == NewSession`, не при resume или продолжении сессии
- [ ] **HOOK-04**: Хук может вернуть `allow`, `deny` (с текстом ошибки) или `modify` (с изменёнными аргументами)

### Model Routing (ROUTE)

- [ ] **ROUTE-01**: `ProviderRouteConfig` поддерживает `min_input_tokens` и `min_tool_count` как условия роутинга по сложности задачи
- [ ] **ROUTE-02**: Роутинг по сложности не нарушает prompt cache state (каждый route-target ведёт отдельный cache context)

### Refactoring (REF)

- [x] **REF-03**: Rate-limiter в `hydeclaw-gateway-util/src/rate_limiter.rs` использует DashMap 6 вместо `Arc<Mutex<HashMap>>`, все `.await` на DashMap guard-ах отсутствуют

## v2 Requirements (отложено)

### Eval Framework

- Eval-фреймворк / golden dataset / CI regression gates — высокая сложность, отдельный milestone
- LLM-as-judge для качества ответов агентов — зависит от eval-фреймворка

## Out of Scope

| Feature | Reason |
| --- | --- |
| Eval framework / CI gates | Высокая сложность, отдельный milestone |
| Новые типы каналов | Не связано с harness quality |
| Fine-tuning / model training | Out of scope для self-hosted gateway |
| OpenAI Assistants API | HydeClaw использует raw API напрямую |

## Traceability

| Requirement | Phase | Status |
| --- | --- | --- |
| REF-03 | Phase 67 | Complete |
| CACHE-01 | Phase 68 | Pending |
| CACHE-02 | Phase 68 | Pending |
| CACHE-03 | Phase 68 | Complete |
| CACHE-04 | Phase 68 | Pending |
| COMP-01 | Phase 69 | Pending |
| COMP-02 | Phase 69 | Pending |
| COMP-03 | Phase 69 | Pending |
| COMP-04 | Phase 69 | Pending |
| ROUTE-01 | Phase 70 | Pending |
| ROUTE-02 | Phase 70 | Pending |
| DEFER-01 | Phase 71 | Pending |
| DEFER-02 | Phase 71 | Pending |
| HOOK-01 | Phase 72 | Pending |
| HOOK-02 | Phase 72 | Pending |
| HOOK-03 | Phase 72 | Pending |
| HOOK-04 | Phase 72 | Pending |

**Coverage:**

- v1 requirements: 16 total
- Mapped to phases: 16
- Unmapped: 0 ✓

---

*Requirements defined: 2026-05-08*
*Last updated: 2026-05-08 — traceability aligned to roadmap phases 67–72*
