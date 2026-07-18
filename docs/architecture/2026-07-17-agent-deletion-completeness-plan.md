# Agent Deletion Completeness — Plan (rev.2)

**Дата:** 2026-07-17 · **rev.2:** 2026-07-18 (после верификации против кода HEAD + прод-схемы + аудита v2)
**Статус:** proposed, verified — готов к исполнению
**Триггер:** ручное удаление 2 агентов (`Arty`, `Lana`) через UI оставило «хвосты» в БД
и файловой системе. Ручная зачистка проведена (бэкап в
`~/opex/backups/deleted-agents-20260717/`), но корневые причины в коде не устранены.

> **rev.2 — итог верификации:** диагноз корней верен, классификация agent_id-таблиц
> исчерпывающа (23/23 против прод-схемы), но план имел 3 HIGH-дыры: (1) семейство
> **agent_name-таблиц** не каталогизировано (живой хвост: 5 строк Lana в `tool_quality`);
> (2) drift-тест на sqlx-миграциях физически не видит внемиграционную `eventbak_prune`
> (а её добавление в константы сломало бы существующий schema-тест); (3) конфликт с
> аудитом v2 A7/m090 по `pending_messages`/`video_jobs` (идут под DROP, а не в delete-путь).
> Все исправлены ниже. Верификация: сабагент 2026-07-18, факты в git-истории сессии.

## 1. Что произошло (уточнено rev.2)

`Arty` и `Lana` удалены через `DELETE /api/agents/{name}` (`api_delete_agent`,
[crud.rs](../../crates/opex-core/src/gateway/handlers/agents/crud.rs)). Обработчик
отработал по своей зоне: TOML снесён, `cleanup_agent_data` удалил `agent_channels` +
`agent_model_overrides` (по agent_name) + константный список `TABLES_TO_DELETE_BY_AGENT_ID`
(фактический состав: `scheduled_jobs, webhooks, agent_oauth_bindings, gmail_triggers,
agent_github_repos, approval_allowlist, channel_allowed_users, agent_plans`) + uploads
(agent_icon) + vault-scope. Осталось:

| Хвост | Причина |
|---|---|
| `workspace/agents/{name}/` (SOUL/SELF/MEMORY/avatar) | обработчик вообще не трогает ФС |
| `agent_emotion_state`, `memory_chunks`, `outbound_queue`, `stream_jobs`, `pairing_codes`, `pending_approvals` | в `NOT_NULL`-каталоге, но не в delete-списке |
| `tool_quality`, `handler_config`, `handler_jobs`, `pending_skill_repairs` | **agent_name-таблицы — не каталогизированы вообще** (rev.2; Lana-строки в tool_quality живы до сих пор) |
| `profiles` (профиль «Lana» жив в БД) | привязка только по конвенции имени — вне всех каталогов (rev.2, аудит v2 A6) |
| `eventbak_prune` | внемиграционная ручная прод-таблица (бэкап soul-прунинга) — каталогизация в код-константы невозможна (см. §3.2-b) |
| серверные скиллы `lana-*.md` в workspace/skills | созданы агентом, вне workspace/agents/ (аудит v2 A6: триггер `- never` инжектит мусор) |
| `sessions`/`messages`/`audit_*`/`usage_log`/`session_failures` | сохранены by design (см. §3.4) |

**Примечание rev.2:** Arty с тех пор пересоздан и снова живой агент (сессии/чанки от
2026-07-18); его строки в eventbak_prune — намеренный ручной бэкап, не хвост.

## 2. Корневые причины

1. **`TABLES_TO_DELETE_BY_AGENT_ID` — неполное подмножество** ephemeral-состояния.
2. **agent_name-семейство (7 таблиц) вне всех каталогов** (rev.2): `agent_channels`,
   `agent_model_overrides` чистятся ad-hoc; `handler_config` (m075), `handler_jobs` (m067),
   `tool_quality` (m070), `pending_skill_repairs` (m037), `video_jobs` (m064, drop-ripe) —
   никем. Rename-фикс 2026-07-18 покрыл первые 4 для rename, но delete их не трогает.
3. **Каталогизация не защищена в направлении «схема → список»**: существующий
   `test_tables_with_agent_id_all_exist_in_schema` (`crud.rs:1569`) проверяет только
   «список → схема». Плюс `test_rename_mid_failure_leaves_pre_rename_state` (`crud.rs:1472`)
   пиннит ручной устаревший список из 20 таблиц (реально 21+1+7) — фиктивная защита
   (аудит v2 A7).
4. **Директория `workspace/agents/{name}/` не удаляется** (реиндекс её не подхватывает —
   `MEMORY_INDEX_EXCLUDE_DIRS` содержит `agents` — но это утечка «души» удалённого агента).
5. **Category B (история/аудит) — нерешённый продуктовый вопрос** — нужна явная опция,
   а не хардкод.

## 3. Доработка (rev.2)

### 3.1. Исчерпывающая классификация ОБЕИХ колонок-привязок

```rust
enum AgentDataClass {
    /// Per-agent runtime/state. DELETE всегда при удалении агента.
    Ephemeral,
    /// Compliance/история. Сохраняется по умолчанию; удаляется при purge_history=true.
    History,
    /// Deprecated-таблица под DROP (m090) — исключить из всех операций,
    /// удалить из констант ДО миграции DROP (порядок обязателен — аудит v2 A7).
    DropRipe,
}
```

- **Ephemeral (agent_id):** `agent_emotion_state`, `memory_chunks` (см. §3.6),
  `outbound_queue`, `stream_jobs`, `pairing_codes`, `pending_approvals`,
  `scheduled_jobs`, `webhooks`, `agent_oauth_bindings`, `gmail_triggers`,
  `agent_github_repos`, `approval_allowlist`, `channel_allowed_users`, `agent_plans`.
- **Ephemeral (agent_name) — НОВАЯ константа `TABLES_WITH_AGENT_NAME`:**
  `agent_channels`, `agent_model_overrides`, `handler_config`, `handler_jobs`,
  `tool_quality`, `pending_skill_repairs`.
- **History (agent_id):** `sessions`, `messages`, `audit_log`, `audit_events`,
  `usage_log`, `session_failures`, `cron_runs`.
- **DropRipe:** `pending_messages`, `video_jobs` (+ безколоночные file_scenarios,
  file_scenario_outcomes) — НЕ добавлять в delete-путь; исключить из
  `TABLES_WITH_AGENT_ID_NOT_NULL`, затем m090.
- Прочие привязки: `uploads` (owner agent_icon), vault-scope, `sessions.participants`.
- **`profiles`:** привязка только по конвенции имени; профили — самостоятельные
  сущности (могут шариться) → НЕ авто-удалять; UI-диалог удаления агента показывает
  «профиль '{name}' существует — удалить отдельно?» с ссылкой (закрывает Lana-кейс A6).

### 3.2. Двухуровневая защита каталогизации (rev.2 — переработан)

- **(a) `#[sqlx::test]` drift-тест** по `information_schema.columns WHERE column_name
  IN ('agent_id','agent_name')` на миграционной БД: каждая таблица — ровно в одной
  категории (Ephemeral/History/DropRipe). Ловит будущие **миграционные** таблицы.
- **(b) Прод-side чек в `GET /api/doctor`** (rev.2): тот же information_schema-запрос
  на живой БД, warn на таблицы вне классификации. Только это ловит внемиграционные
  ручные таблицы класса `eventbak_prune` (sqlx-тест их физически не видит; сама
  eventbak_prune идёт в известный-allowlist доктора до её ручного DROP по runbook
  после подтверждения decay-волны — аудит v2 §D).
- **(c) Переписать `test_rename_mid_failure_leaves_pre_rename_state`** на импорт
  реальных констант вместо ручного списка (закрывает аудит v2 A7).

### 3.3. Удаление директории workspace

В `api_delete_agent` — best-effort удаление `workspace/agents/{name}/` через
**существующий** path-guard паттерн `agent/workspace.rs` (`dunce::canonicalize` root
+ parent-canonicalize против symlink-escape — стр. ~152, ~590-610; не писать новый).
Логировать исход, не валить удаление при ошибке ФС. Серверные скиллы, созданные
агентом (workspace/skills/*), — осознанный out-of-scope: пункт runbook'а (§3.7).

### 3.4. Опция purge_history (rev.2 — модель FK уточнена)

`DELETE /api/agents/{name}?purge_history=true` (default false). Фактические FK прода:
`messages`/`session_timeline`/`session_failures`/`session_shares`/`session_goals`/
`session_todos`/`stream_jobs`/`pending_approvals` — `ON DELETE CASCADE` от sessions;
`usage_log.session_id` → SET NULL. Поэтому:

1. `DELETE FROM sessions WHERE agent_id=$1` — каскадит почти всё;
2. отдельно: `usage_log` (по agent_id), `handler_jobs` (по agent_name — session_id
   NOT NULL **без FK**, иначе осиротеет), `audit_log`/`audit_events`/`cron_runs`.
3. **Multi-agent кейсы (зафиксировать в docstring + UI-предупреждении):**
   сообщения агента в ЧУЖИХ сессиях (`messages.agent_id` nullable) НЕ удаляются
   (это чужая история); purge сессий, где агент primary, удалит и реплики других
   участников (`sessions.participants`) — предупредить в UI.

### 3.5. Бэкап перед destructive-операциями (rev.2 — обязателен для soul)

Для агентов с `[agent.soul] enabled` авто-дамп Ephemeral (минимум `memory_chunks`
kind=event/reflection) **обязателен** перед DELETE (прямой SQL обходит fail-closed
`refuse_if_biography` — это допустимо для удаления агента, но необратимо);
консистентность с `docs/runbooks/soul-quarantine.md`. Для остальных — опционально
+ runbook.

### 3.6. memory_chunks: scope-нюанс (rev.2 — новый)

`agent_id TEXT NOT NULL DEFAULT ''`; shared-строки могут нести реальный agent_id
автора (подтверждено на проде). Плоский `DELETE WHERE agent_id=$1` снёс бы shared-
знание, которым пользуются другие агенты. Решение: удалять `WHERE agent_id=$1 AND
scope='private'` (private facts + soul events/reflections), shared-строки автора —
оставить (обезличить `agent_id=''`) — знание общее, автор не важен.

### 3.7. LiveAgent-субагенты (rev.2 — новый)

`api_delete_agent` останавливает только главный engine-handle; session-scoped
субагенты удаляемого агента (SessionAgentPool в чужих сессиях) не убиваются —
добавить обход `session_pools` с kill по имени агента.

## 4. Порядок работ (rev.2)

1. Классификация (обе константы + DropRipe) + drift-тест (a) + doctor-чек (b) +
   починка rename-теста (c). **Первым — ловит регрессии.**
2. Исключить `pending_messages`/`video_jobs` из констант → **затем m090** (drop 4
   deprecated-таблиц; file_scenarios 4 строки + video_jobs 11 строк экспортнуть).
3. Добавить пропущенные Ephemeral в delete-путь (agent_id-остаток + agent_name-шестёрка,
   с §3.6-фильтром для memory_chunks) + LiveAgent-kill (§3.7).
4. Удаление workspace-дир (переиспользуемый path-guard).
5. `purge_history` (CASCADE-модель §3.4) + UI-чекбокс + profiles-подсказка (§3.1).
6. Runbook `docs/runbooks/agent-deletion.md` (вкл. skills-хвосты, eventbak_prune,
   бэкап-прецедент 2026-07-17) + ссылка из ARCHITECTURE.md.
7. Разовая зачистка текущих хвостов Lana: `tool_quality` (5 строк), профиль в БД,
   `lana-*.md` скиллы (аудит v2 A6 — можно раньше остальных пунктов).

## 5. Тесты

- `test_every_agent_binding_is_classified` — information_schema (agent_id+agent_name)
  ↔ классификация, exhaustive (§3.2-a).
- `test_ephemeral_delete_removes_all_state` — строки во всех Ephemeral (обе колонки),
  удалить, 0 остатков; shared memory_chunks выжил обезличенным (§3.6).
- `test_history_preserved_by_default` / `test_purge_history_cascades` (проверить
  CASCADE-происхождение: timeline/failures/shares исчезли без явных DELETE).
- `test_workspace_dir_removed_on_delete` + негативный symlink-escape.
- rename-тест на реальных константах (§3.2-c).

## 6. Замечания по инциденту 2026-07-17 (историческая справка)

Ручная зачистка Category A+B выполнена; бэкап `~/opex/backups/deleted-agents-20260717/`
(76M). **rev.2:** Arty впоследствии пересоздан (живой агент); остаточные хвосты Lana
на 2026-07-18 — `tool_quality` 5 строк, профиль в `profiles`, 2 скилла — вошли в §4 п.7.
