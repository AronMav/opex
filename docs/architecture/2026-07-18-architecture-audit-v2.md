# Полный аудит архитектуры v2 (2026-07-18)

**HEAD:** f0b534cb · 6 параллельных аудиторов (API / Rust / UI / toolgate+channels / БД / конфиг-скиллы-инфра) · прод read-only.
Предыдущий аудит: [2026-07-17-dead-code-architecture-audit.md](2026-07-17-dead-code-architecture-audit.md) — его волна (5 батчей чистки + цикл A + план надёжности тулов) полностью на проде. Этот аудит меряет состояние ПОСЛЕ неё (172 коммита за 2 дня) с вшитыми предохранителями v1 (wire-резервы, multiline-роуты, by-design ручки, 4 tool-пути, INT4-ловушки).

**Гигиена — вся зелёная:** tsc ✓, vitest 1520/1520 ✓, pytest 391/391 ✓, gen-types drift 0 ✓, i18n-паритет 1378=1378 ✓, миграции побайтно чисты (sha384 против `_sqlx_migrations`) ✓, фантомов имён тулов 0 (registry↔списки 35↔35) ✓, новых INT4/INT8-ловушек 0 из ~15 проверенных ✓, мёртвых API-роутов 0 ✓, протокол-шов каналов 3-сторонне согласован ✓.

---

## A. СЛОМАННОЕ / ДЕФЕКТЫ СВЕЖЕГО КОДА (чинить в первую очередь)

### A1. Гонка `mark_sent` затирает `acked` → дубль сообщения через час — BROKEN
Replay/action-путь шлёт wire-frame, затем **detached** `tokio::spawn(mark_sent)` (`handshake.rs:219-223`, `channel_ws/mod.rs:249-256`). Если `ActionResult` адаптера (`reader.rs:231` → `mark_acked`) обгоняет спавн, `mark_sent` (`db/outbound.rs:80-88`, UPDATE **без guard по статусу**) перезаписывает `acked`→`sent`; через 1ч строка снова «stale sent» → `get_pending` реиграет → дубль юзеру. Фикс: `WHERE status='pending'` в mark_sent. CONF: механизм HIGH, частота MED.

### A2. A5 durable-reply застревает до следующего реконнекта — FRESH-DEFECT
Единственный триггер replay — handshake (реконнект). Типовой сценарий: адаптер реконнектится за 1-5с, ход добегает 10-60с → enqueue происходит ПОСЛЕ уже отработавшего replay → сообщение лежит в `outbound_queue` днями (до следующего рестарта адаптера). Периодического drain нет. Фикс: периодический drain (scheduler) или replay-триггер после enqueue при живом соединении канала. `session_queue.rs:246` / `handshake.rs:168-224`. CONF HIGH.

### A3. Таймаут-шов: bridge 300s vs core FIFO → тихая потеря ответа при ЖИВОМ WS — BROKEN (edge)
Bridge-таймаут 300s фиксированный (`bridge.ts:98-103`), core сериализует per-session FIFO с per-request 180+20s: сообщение, вставшее за долгим ходом, получает `Done` после >300s → pending в адаптере уже удалён → `handleOutbound` молча дропает. A5-fallback НЕ срабатывает (WS жив, `out_tx.send` успешен). Юзер получил «Request timeout», ответ потерян навсегда. CONF MED.

### A4. Двойной учёт abort-usage на пути SSE `Interrupted("sink_closed")` — FRESH-DEFECT
`record_usage` фиксирует реальный usage завершённого вызова (`execute.rs:582-611`), затем emit `Finish` падает `Closed` → `Interrupted` → `spawn_record_aborted_usage` пишет ВТОРУЮ строку (len/4). Docstring «record_usage never fires for it» неверен для этой арки. Канальный путь чист (ChannelStatusSink глотает не-TextDelta). Фикс: флаг `usage_recorded_for_final_call` в finalize. `finalize.rs:689-701`. CONF: механизм HIGH, частота LOW-MED.

### A5. Ещё два YAML-тула на reserved-секрете — FIXED
Серверные `~/opex/workspace/tools/core_get_backup.yaml` + `core_get_config.yaml` → `auth: bearer_env OPEX_AUTH_TOKEN` → reserved-гард (`yaml_tools.rs:27-28`) валил каждый вызов. Фикс: добавлен `bearer_internal` auth type, который берёт текущий core token (`gateway::shared_token`) и разрешён только для loopback/internal endpoint'ов; оба YAML-файла на сервере переключены на `bearer_internal`. stale-ссылка `core_get_skills_repairs` (не было YAML-файла) удалена из `heartbeat-maintenance.md`. CONF HIGH.

### A6. Хвосты Ланы: скилл с триггером `- never` инжектит мусор в промпты — DEAD-опасный
`lana-agent-config-read.md` (active, тело «TEMP», триггер `- never`): триггеры = substring → английское слово «never» в любом сообщении матчит и инжектит «TEMP» в системный промпт. + `lana-config-20260716.md` (тоже TEMP) + **профиль Lana остался в БД `profiles`** (агент вычищен). Удалить всё. CONF HIGH.

### A7. Ловушка drop pending_messages — координация ОБЯЗАТЕЛЬНА — FRESH-DEFECT (условный)
m089 приглашает оператора дропнуть таблицу вручную, но `pending_messages` числится в `TABLES_WITH_AGENT_ID_NOT_NULL` (`crud.rs:128`) → после drop любой rename/delete агента = 500+rollback. Плюс тест `test_rename_mid_failure_leaves_pre_rename_state` (`crud.rs:1472-1499`) пиннит устаревший список из 20 таблиц вручную (реально 21 + 4 agent_name) — защита фиктивна. Фикс: убрать из константы + переписать тест на импорт реальных констант, ПОТОМ m090. CONF HIGH.

### A8. Фантом `process_start` пережил v1 в доках/скилле — security-релевантный DOC-ROT
`config/skills/agent-management.md:66` (репо И сервер) + CLAUDE.md (Setup Wizard) всё ещё учат deny-лист с `process_start` — base-агент, следуя скиллу, задэнит несуществующее имя, оставив реальный `process` разрешённым. Код уже кладёт `process` (`crud.rs:485-490`). CONF HIGH.

---

## B. МЁРТВОЕ (чистка, по слоям)

**БД (BROKEN 0):** функция `query_tool_audit` (allow-parked, единственный потребитель снесён); **7 мёртвых индексов** — idx_audit_log_tool (760 kB!), idx_messages_session_compressed (полярность предиката противоположна коду), idx_session_timeline_type (фильтры кода не совпадают с partial), idx_usage_log_status_aborted (код использует негацию), messages_session_step_idx + messages_parallel_batch_idx (фильтров нет), notifications_read_created_at (MED). → миграция-дроп.
**DROP-RIPE 4 таблицы** → единая m090 (после A7-координации): pending_messages (0 строк), file_scenario_outcomes (0), file_scenarios (4 строки 26.06 — экспортнуть), video_jobs (11 done 28.06 — экспортнуть).

**UI (BROKEN 0):** хуки-сироты `inviteAgent`, `useHandlerAllowlist`, `useCreateHandler`/`useUpdateHandler` (HIGH), `useCuratorConfig` (MED, тест-защищён вне REQUIRED_COVERAGE), `selectIsEmpty` (MED); **26 i18n-ключей-сирот ×2 локали** (список в отчёте аудитора). RESERVE: 5 хуков под REQUIRED_COVERAGE-ассертом — не трогать.

**toolgate/channels:** импорт `extract_scene_frames` (video.py:17), вакуум-тест `test_httpx_limits_constants_correct`. **Дозрели отложки v1:** `config.aload_config`+`_aload_config_from_api`+test_config.py — сноси смело; `/summarize-video` — наполовину (единственная реализация frame-дайджеста, фича числится Deferred — держать до решения).

**Сервер-конфиг:** `[typing]`-секция opex.toml (serde игнорирует, typing_mode переехал в JSONB канала); бинарники hydeclaw-* (~70MB×3, 23.06); мусор .bak/.disabled/draft-каталог; **9 draft-тулов** копятся (calendar_*, email_*, news, set_my_icon, tavily) — верифицировать или снести; спящие скиллы email/calendar-management на draft-тулах; дубль its-search vs its-search-optimized (оба active, priority=5 — недетерминизм); 3 поколения бэкап-скиллов active одновременно.

---

## C. DOC-ROT

1. **docs/API.md — 52 недокументированных пути** (profiles/m084!, handlers-tab, checkpoints, plan/initiative, files-hub, commands, share, google-auth, tools/health и ещё ~15 фич): чистка v1 сделала «вычитание», «сложение» не делалось никогда.
2. **docs/API.md §23 uploads — описывает несуществующую архитектуру** (`GET /uploads/{filename}`, workspace/uploads/{uuid}, require_signature) — реальность: `/api/uploads/{id}?sig=&exp=`. + DEPLOYMENT.md nginx `location /uploads/` проксирует в фантом.
3. CLAUDE.md: `GET/POST /api/files/{id}/actions|run` (снесены), `memory_write/memory_search` фантомы (реальный тул `memory` + actions), `process_start` (A8), «default 100 rpm» vs код 300, searxng в compose (закомментирован). docs/ARCHITECTURE.md:504 memory_write/search.
4. `gateway/mod.rs:132` стейл-коммент про files actions/run; `chat/misc.rs:2` про api_context_breakdown; стейл `#[allow(dead_code)]` на живых `fse/allowlist.rs`; stale-ссылка на engine_sse.rs в integration_aborted_usage.rs:16.
5. `config/skills/toolgate-router.md:32` «Add entry to TOOLS.md» (упразднён) — уцелел с v1.

---

## D. ЗАМЕТКИ / РЕЗЕРВЫ (НЕ трогать без причины)

- **`STATUS_ABORTED_FAILOVER`** — эмиттера больше нет (только тесты+индекс m025); решить: удалить или реализовать failover-ветку (execute.rs:504).
- **block↔enabled:false без startup-валидации** — конфиг-дрейф молча вернёт MCP-тул субагентам/каталогу. Идея: warn-проверка пары на старте.
- `/api/tools/health` — operator-curl-only, нет UI и нет в API.md (решить: UI-виджет или задокументировать).
- `/api/watchdog/config` GET+PUT — ноль потребителей ВООБЩЕ (даже watchdog не читает) — кандидат на пересмотр.
- `/api/csp-report` активен только при применённом nginx-сниппете (report-uri) — проверить прод-nginx.
- Email-форма шлёт порты строками (работает по совпадению Node) — Number()-коэрция желательна.
- `eventbak_prune` (прод, 370 строк, вне миграций) — ручной бэкап soul-событий; держать до подтверждения decay, потом ручной DROP. `session_events_pkey` — имя PK пережило rename m049 (косметика). CLAUDE.md «19 таблиц» → 21.
- Wire-резервы подтверждены живыми: ChannelOutbound::Reload, StreamEvent::AgentSwitch, ProcessingPhase::*, capability-имена в SUBAGENT_DENIED_TOOLS, ScenarioOutcome.video_accepted, workspace_helpers.get_secret (внешние handlers), @opentelemetry/api (peer-pin), fse:-tombstone в telegram.ts.
- ALLOW-PARKED из v1: 4 route-орфана (list_connections, get_run, HookRegistry::names, validated_fts_language) — можно удалять; ToolAuditEntry/delete_agent_icon/context_breakdown — держать (харнес/шасси).
- В рабочем дереве — чужая незакоммиченная работа (rate-limiter bearer-exemption: middleware.rs, gateway/mod.rs, opex-gateway-util) + стейл-worktree `.claude/worktrees/agent-a89d45b42df34425f/` — параллельная сессия, не трогать.

---

## Сводные счётчики

| Класс | v2 | (v1 для сравнения) |
|---|---|---|
| BROKEN / FRESH-DEFECT | **8** (A1-A8) | 14 заявлено → ~9 подтверждено |
| DEAD | ~50 позиций (7 индексов, 5 хуков, 52 i18n-записи, конфиг-мусор) | 48 роутов + слои |
| DROP-RIPE таблицы | 4 (+ координация A7) | 0 (были deprecated) |
| DOC-ROT | 5 групп (гл. — API.md «сложение» 52 путей) | — |
| Гигиена (tsc/vitest/pytest/gen-types/миграции/фантомы) | **всё зелёное** | были расхождения |

**Главный вывод:** волна 17-18.07 сработала — мёртвых роутов 0, фантомов имён 0, тесты/типы/кодеген чисты. Оставшиеся дефекты сконцентрированы в **швах доставки каналов** (A1-A3 — три способа потерять/задублировать ответ юзеру) и **хвостах удаления/доков** (Lana, uploads-§23, «сложение» API.md). Рекомендуемый порядок fix-wave: A1+A2+A3 (один канал-батч), A5+A6 (сервер-конфиг, 10 мин), A7 (перед любым DROP), A4, A8+C (док-батч), затем B-чистка.
