# Инициатива — «Открытые треды» как доп. источник целей — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 1)
**База:** задеплоенная gated-инициатива (v1 + 2A harden + decompose B-narrow). Спека v1 `docs/superpowers/specs/2026-07-11-agent-soul-stage-c-initiative-design.md` (§3.3 initiative_tick, §8 «доп. источники целей»).

---

## 1. Цель и не-цели

**Цель:** обогатить источник инициативных целей — кроме рефлексий/SELF.md, агент предлагает довести **незавершённые треды** (задачи/просьбы, которые пользователь поднял, но не довели в недавних сессиях). Предложения становятся конкретнее и полезнее.

**Решение брейншторма (источник):** извлекать open_items при обработке завершённой сессии в `knowledge_extractor` (недоверенный user-текст дистиллируется LLM ОДИН раз + санитайзится, не течёт сырым в proposal-промпт). initiative_tick читает свежие open_threads.

**Инвариант gated сохраняется:** предложение по-прежнему owner-gated; open_threads лишь грунтуют генерацию, не меняют cap/гейт.

**Не-цели (v1):** явный трекинг резолюции («тред закрыт») — v1 полагается на recency-окно + memory-decay; отдельный флаг (обогащение уже-включённой инициативы, извлечение под soul.enabled).

---

## 2. Что переиспользуется

- `knowledge_extractor.rs`: `ExtractedKnowledge` (user_facts/outcomes/feedback/events), `extraction_prompt` (2 варианта — с soul / без; возвращают JSON-массивы), `save_events` (index через `memory_store.index(text, source, pinned, scope, agent)`), `EXTRACTION_TIMEOUT`, `json_repair`.
- `sanitize_soul_text(&str, max_chars) -> Option<String>` (санитайз при записи, как events; `EVENT_MAX_CHARS=300`).
- `MemoryService::index/get` (source-scoped chunks; events используют source `soul_event:{session_id}`).
- `initiative_tick`/`generate_proposal` (`agent/initiative/tick.rs`) — расширяется сигнатура.
- Существующие sanitize + is_trivial_goal guard + cap 1/день на выходе proposal — без изменений.

---

## 3. Компоненты

### 3.1 Извлечение open_items (`knowledge_extractor`)

- `ExtractedKnowledge` += `#[serde(default)] open_items: Vec<String>`.
- Оба варианта `extraction_prompt` (with/without soul) — добавить в JSON-контракт массив `"open_items": ["..."]` с инструкцией: «незавершённые задачи/просьбы, которые пользователь поднял в этой сессии, но которые НЕ доведены до конца (агент не выполнил или обещал позже). Каждая — одна конкретная фраза. Пусто, если всё завершено». (Извлекается тем же LLM-вызовом — доп. стоимость нулевая.)
- Новая `save_open_threads(session_id, agent_name, memory_store, open_items)`: каждый item → `sanitize_soul_text(item, EVENT_MAX_CHARS)` (None → отброс); cap `MAX_OPEN_ITEMS=5`; сохранить как `memory_store.index(clean, &format!("open_thread:{session_id}"), false /*not pinned*/, "private", agent_name)`. `kind='fact'` (по умолчанию index → decayable; треды транзиентны, стареют, в отличие от бессмертной биографии event/reflection). Вызывается в `extract_and_save_inner` рядом с `save_events` (под тем же путём; извлекается всегда, читается только инициативой).

### 3.2 Чтение свежих тредов

Новая функция (в `db/memory_queries.rs` или `knowledge_extractor`): `recent_open_threads(db, agent_name, limit, since_days) -> Vec<String>` — свежие чанки с `source LIKE 'open_thread:%'` для агента (recency-окно `since_days`, напр. 7; `ORDER BY created_at DESC LIMIT limit`, напр. 5; дедуп по content). Fail-soft → пустой вектор.

### 3.3 Интеграция в `initiative_tick`

Перед `generate_proposal` собрать `open_threads = recent_open_threads(db, agent_name, 5, 7)` (fail-soft). Передать в `generate_proposal(provider, agent_name, self_md_text, &open_threads)`.

### 3.4 Промпт `generate_proposal`

Расширить: блок SELF.md + блок недавних тредов (framed):
```
Исходя из души агента {agent} (SELF.md ниже) И недавних незавершённых тредов
(что пользователь начал/просил, но не доведено), предложи ОДНУ конкретную цель.
Приоритет — довести начатое для пользователя, если есть релевантный тред.
Верни строго JSON: {"goal": "...", "rationale": "..."}.

SELF.md:
{self_md}

Недавние незавершённые треды:
{open_threads_joined или "(нет)"}
```
`focus`-генерация (generate_focus) — БЕЗ изменений (только SELF.md).

### 3.5 Безопасность

- open_items — LLM-дистилляция разговора (не сырой user-текст), санитайзятся `sanitize_soul_text` при записи (как events). В proposal-промпт идут уже чистыми. Выход proposal санитайзится + `is_trivial_goal` + cap 1/день. Injection-безопасно (тот же барьер, что вся soul-линия).
- Треды — `kind='fact'` (не биография) → generic memory decay/delete их касается (стареют естественно) — это by-design (транзиентность).
- Extraction под `soul_deps.cfg.enabled` (как events) — не-soul агенты open_threads не копят.

---

## 4. Поток данных (E2E)

```
session finished → knowledge_extractor LLM:
  {user_facts, outcomes, feedback, events, [NEW open_items]}
  → save_open_threads: sanitize each → memory_chunk source=open_thread:{sid} kind=fact
initiative_tick (после reflection):
  open_threads = recent_open_threads(agent, 5, 7d)
  generate_proposal(SELF.md + open_threads) → цель, грунтованная в незавершённом
  → sanitize + is_trivial guard + cap → пропозал (gated) → доставка (2A)
```

---

## 5. Обработка ошибок

- open_items отсутствуют/пусты в JSON → `#[serde(default)]` → пустой вектор (норма).
- sanitize отбрасывает item → просто не сохраняется.
- `recent_open_threads` провал (БД/память) → пустой вектор → generate_proposal с «(нет)» тредов (деградирует к SELF.md-only, как сейчас).
- Извлечение — часть существующего extraction (fail-soft: провал не рушит сессию).

---

## 6. Тестирование

- **Юнит:** парс `open_items` из JSON extraction (валид/отсутствует→пусто); sanitize отбрасывает инъекцию; `generate_proposal`-промпт содержит блок тредов (при непустом) / «(нет)» (при пустом).
- **sqlx:** `save_open_threads` → `recent_open_threads` возвращает свежие, recency-окно отсекает старые, дедуп.
- **E2E на сервере (manual):** сессия где пользователь просит X но не доводит → extractor извлекает open_item → memory_chunk open_thread → initiative_tick при след. предложении грунтует цель в этом треде (наблюдать в логах/предложении).

---

## 7. Файловая структура (для плана)

- `crates/opex-core/src/agent/knowledge_extractor.rs` — `ExtractedKnowledge.open_items` + оба `extraction_prompt` (JSON += open_items) + `save_open_threads` + вызов в `extract_and_save_inner`.
- `crates/opex-core/src/db/memory_queries.rs` (или knowledge_extractor) — `recent_open_threads`.
- `crates/opex-core/src/agent/initiative/tick.rs` — сбор open_threads + `generate_proposal` сигнатура + промпт.

**Декомпозиция (~4 задачи):** (1) ExtractedKnowledge.open_items + extraction_prompt (оба варианта); (2) save_open_threads + recent_open_threads + sqlx; (3) initiative_tick + generate_proposal промпт; (4) юнит-тесты (парс/санитайз/промпт) — фолдится в 1/3.

---

## 8. Что дальше (вне v1)

- Явная резолюция тредов (пометка resolved при завершении релевантной цели).
- Открытые треды в UI-панели плана.
