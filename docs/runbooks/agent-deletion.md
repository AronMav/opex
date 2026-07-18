# Runbook: удаление агента (полная зачистка)

Когда: оператор удаляет агента через UI (диалог «Delete») или `DELETE /api/agents/{name}`.
Этот runbook описывает семантику удаления, необратимые эффекты, обязательный бэкап
биографии и «хвосты», которые delete-путь **намеренно не трогает**.

Прецедент: инцидент **2026-07-17** (Arty/Lana) — старый `api_delete_agent` чистил
неполное подмножество таблиц, оставляя сироты в `memory_chunks`, `agent_emotion_state`,
`outbound_queue`, `stream_jobs`, `tool_quality` и workspace-директории. Пришлось
вычищать вручную (бэкап 76M). Текущая реализация закрывает эту дыру трёхклассовой
классификацией таблиц (§ ниже), но часть хвостов по-прежнему требует ручного шага.

---

## 1. Что удаляется автоматически

Классификация всех agent-bound таблиц — в
`crates/opex-core/src/gateway/handlers/agents/crud.rs` (константы). Три класса:

| Класс | Константа | Поведение при delete |
|-------|-----------|----------------------|
| **Ephemeral** (agent_id) | `TABLES_TO_DELETE_BY_AGENT_ID` | `DELETE ... WHERE agent_id=$1` в транзакции |
| **Ephemeral** (agent_name) | `TABLES_WITH_AGENT_NAME` | `DELETE ... WHERE agent_name=$1` в транзакции |
| **History/compliance** (agent_id) | `TABLES_HISTORY_AGENT_ID` | **сохраняются**; удаляются ТОЛЬКО при `?purge_history=true` |
| **DropRipe** (deprecated) | `TABLES_DROP_RIPE` | не трогаются; удалены миграцией m090 |

> **Оговорка (`cron_runs`):** формально в классе History, но каскадится
> `ON DELETE CASCADE` от `scheduled_jobs` (Ephemeral). При обычном delete строки
> `cron_runs`, привязанные к живому job'у агента, удаляются вместе с job'ом —
> история запусков крона не переживает удаление. Это pre-existing FK-поведение,
> вне scope фичи; здесь зафиксировано, чтобы «History переживает» не читалось
> буквально для этой таблицы.

`memory_chunks` — особый случay (§3.3 спеки):
- `scope='private'` (личные факты + биография soul: `kind IN (event, reflection)`) — **удаляются**;
- `scope='shared'` (авторская общая память) — **переназначаются** на `agent_id=''` (переживают автора).

Плюс: workspace-директория агента удаляется с canonicalize path-guard; живые
session-pool агенты убиваются.

### Драйф-контроль
Новая agent-bound таблица, добавленная миграцией без классификации, ловится:
- на PR — тестом `test_every_agent_binding_is_classified` (crud.rs);
- на проде — doctor-чеком `agent_table_classification` (`GET /api/doctor`).

Аллоулист исключений: `UNCLASSIFIED_TABLE_ALLOWLIST` (сейчас — только `eventbak_prune`).

---

## 2. Необратимость и purge_history

- Обычный delete (`purge_history=false`, дефолт) — **History сохраняется** (sessions,
  usage_log, audit_log, audit_events, cron_runs, session_failures). Аудит цел.
- `?purge_history=true` — **необратимо**: `DELETE FROM sessions WHERE agent_id=$1`
  каскадит на messages / session_timeline / session_failures / session_shares /
  session_goals / session_todos / stream_jobs / pending_approvals; плюс явный sweep
  usage_log/audit_log/audit_events/cron_runs. Мультиагентные сессии, где удаляемый
  агент был primary, удаляются **целиком** (включая ходы других участников — решение
  владельца). Сообщения этого агента в **чужих** сессиях не трогаются.

UI-диалог показывает чекбокс «purge history» с явным предупреждением о необратимости.

---

## 3. Обязательный бэкап биографии (soul)

Перед destructive-шагами delete-путь читает soul-биографию (`kind IN (event,
reflection)`) и пишет JSON-бэкап. Реализация **fail-closed**: ошибка чтения ИЛИ записи
бэкапа прерывает удаление с 500 (агент остаётся цел). Пустого бэкапа с последующим
удалением быть не может.

Бэкапы на сервере: `~/opex/backups/agent-deletion/{agent}-{timestamp}.json`.
Проверить наличие после удаления soul-агента — обязательно.

---

## 4. Хвосты, которые delete-путь НЕ трогает (ручной шаг)

Delete-путь чистит только каталогизированные таблицы + workspace-дир. Ручной зачистки
после удаления требуют:

1. **Авторские скиллы** `workspace/skills/*.md`, созданные агентом или про агента
   (напр. `lana-agent-config-read.md`). Это shared-ресурсы — оставлены намеренно.
   Удалять руками, если больше не нужны:
   ```bash
   rm ~/opex/workspace/skills/{agent-specific}.md
   ```
2. **`eventbak_prune`** — one-off backup-таблица, аллоулистнута из классификации.
   DROP вручную только после подтверждения, что decay/prune-цикл её отработал.
3. **Профиль** с именем агента, если он больше ни к кому не привязан:
   ```sql
   -- сначала проверить, что профиль ничей
   SELECT * FROM agents_using_profile('{agent}');   -- пусто?
   DELETE FROM profiles WHERE name = '{agent}';
   ```

---

## 5. Миграция m090 (DropRipe)

`migrations/090_drop_deprecated_tables.sql` дропает `pending_messages`, `video_jobs`,
`file_scenarios`, `file_scenario_outcomes`. **Ordering hazard:** m090 обязана
применяться ПОСЛЕ того, как эти таблицы убраны из констант delete/rename-путей (иначе
rename бы обращался к несуществующей таблице). Порядок соблюдён: константы вычищены в
T1, m090 добавлена в T3.

Перед деплоем m090 на прод — экспорт на всякий случай:
```bash
pg_dump -t pending_messages -t video_jobs -t file_scenarios -t file_scenario_outcomes \
  "$DATABASE_URL" > ~/opex/backups/pre-m090-$(date +%Y%m%d).sql
```
(Таблицы deprecated и пустые — экспорт как страховка, не восстановление.)

---

## 6. Пост-деплой смоук

- `GET /health` → 200; `systemctl --user status opex-core` NRestarts=0.
- `SELECT version, success FROM _sqlx_migrations WHERE version = 90;` → success=true.
- `GET /api/doctor` → `agent_table_classification: ok` (eventbak_prune аллоулистнут).
- Rename тестового агента → нет `relation does not exist` (ordering m090 держится).
- UI delete-диалог показывает чекбокс purge + hint профиля.
