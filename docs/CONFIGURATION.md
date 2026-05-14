# HydeClaw Configuration Reference

Полный справочник по всем файлам конфигурации, переменным окружения и форматам данных HydeClaw.

---

## Содержание

1. [Обзор файлов конфигурации](#1-обзор-файлов-конфигурации)
2. [.env — переменные окружения](#2-env--переменные-окружения)
3. [config/hydeclaw.toml — главный конфиг](#3-confighydeclawttoml--главный-конфиг)
4. [config/agents/{Name}.toml — конфиг агента](#4-configagentsnametoml--конфиг-агента)
5. [workspace/tools/*.yaml — YAML-инструменты](#5-workspacetoolsyaml--yaml-инструменты)
6. [workspace/mcp/*.yaml — MCP-серверы](#6-workspacemcpyaml--mcp-серверы)
7. [Хранилище секретов (Secrets Vault)](#7-хранилище-секретов-secrets-vault)
8. [Toolgate — конфиг провайдеров](#8-toolgate--конфиг-провайдеров)
9. [Развёртывание на Pi (ARM64)](#9-развёртывание-на-pi-arm64)

---

## 1. Обзор файлов конфигурации

| Файл | Назначение | Горячая перезагрузка |
|------|-----------|---------------------|
| `.env` | 3 системных ключа (токен, мастер-ключ, БД) | Нет (только при старте) |
| `config/hydeclaw.toml` | Всё остальное: сервер, лимиты, процессы, Docker | Да (через `notify`-крейт) |
| `config/agents/{Name}.toml` | Конфигурация отдельного агента | Да |
| `workspace/tools/*.yaml` | Declarative HTTP-инструменты | Да (перезагружаются при каждом запросе) |
| `workspace/mcp/*.yaml` | MCP-сервера (workspace-level) | Нет |
| `config/services/*.yaml` | Service registry (URL, healthcheck, concurrency) | Нет |

**Расположение конфигов на Pi:**
- Binary: `~/hydeclaw/hydeclaw-core-aarch64`
- Config: `~/hydeclaw/config/`
- Workspace: `~/hydeclaw/workspace/`

---

## 2. .env — переменные окружения

Файл `.env` загружается автоматически при старте бинарника. Порядок поиска:

1. Директория бинарного файла (`$(dirname binary)/.env`) — приоритет
2. Текущая рабочая директория (`.env`)
3. Если файл не найден — используются переменные окружения (systemd `EnvironmentFile=`)

**При первом запуске** `.env` создаётся автоматически с генерацией `HYDECLAW_AUTH_TOKEN` и `HYDECLAW_MASTER_KEY` через `rand`. `DATABASE_URL` нужно добавить вручную.

**Только 3 ключа принадлежат `.env`:**

| Переменная | Обязательна | Описание |
|-----------|-------------|----------|
| `HYDECLAW_AUTH_TOKEN` | Да | Bearer-токен для всех API-запросов. Используется UI, channel-адаптерами, Makefile. Автогенерируется как 32-байтный hex. |
| `HYDECLAW_MASTER_KEY` | Да | 64-символьный hex (32 байта) для ChaCha20-Poly1305 шифрования secrets vault. **Никогда не меняется после первого запуска.** |
| `DATABASE_URL` | Да | PostgreSQL connection string. Пример: `postgresql://hydeclaw:hydeclaw@localhost:5432/hydeclaw`. |

> **Все остальные секреты** (API-ключи, токены ботов, пароли провайдеров) хранятся в зашифрованном vault через UI или API. Никогда не добавляйте дополнительные переменные в `.env`.

---

## 3. config/hydeclaw.toml — главный конфиг

Загружается при старте через `AppConfig::load()`. Путь передаётся как первый аргумент CLI или берётся из дефолта `config/hydeclaw.toml`.

### [gateway]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `listen` | String | `"0.0.0.0:18789"` | Адрес и порт HTTP-сервера |
| `auth_token_env` | Option\<String\> | — | Имя env-переменной с auth-токеном (обычно `"HYDECLAW_AUTH_TOKEN"`) |
| `public_url` | Option\<String\> | `http://localhost:{port}` | Публичный URL для внешних ссылок (медиа, uploads) |
| `cors_origins` | Vec\<String\> | `[]` | Разрешённые CORS origins. Если пусто — auto-derived из listen-адреса |
| `cors_docker_subnets` | Vec\<String\> | `[]` | Дополнительные подсети для авто-вывода CORS (для Docker bridge сетей) |

```toml
[gateway]
listen = "0.0.0.0:18789"
auth_token_env = "HYDECLAW_AUTH_TOKEN"
public_url = "http://192.168.1.82:18789"
# cors_origins = ["http://localhost:3000"]
```

### [database]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `url` | String | — | PostgreSQL connection string. Переопределяется env `DATABASE_URL` |

```toml
[database]
url = "postgresql://hydeclaw:hydeclaw@localhost:5432/hydeclaw"
```

### [limits]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `max_requests_per_minute` | u32 | `300` | Rate limit (rpm). Аутентифицированные запросы освобождены. Loopback освобождён. |
| `max_tool_concurrency` | u32 | `10` | Максимум параллельных tool-вызовов (семафор в engine) |
| `request_timeout_secs` | u64 | `180` | Timeout одного запроса (LLM-loop + tools), секунды. `0` = без лимита |
| `max_agent_turns` | usize | `5` | Максимум agent-to-agent turns в одном цикле (API-only, не используется внутри loop) |
| `max_inter_agent_context_chars` | usize | `2000` | Максимум символов для inter-agent контекста (API-only) |
| `max_restore_size_mb` | u64 | `500` | Лимит тела `POST /api/restore` в МБ. Превышение → 413 |
| `max_sessions_per_agent` | u32 | `500` | Максимум сессий на агента. `0` = без лимита |

```toml
[limits]
max_requests_per_minute = 100
max_tool_concurrency = 10
request_timeout_secs = 180
max_restore_size_mb = 500
max_sessions_per_agent = 500
```

### [uploads]

Конфиг подписанных URL для `GET /uploads/*` (фаза 64 SEC-03).

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `signed_url_ttl_secs` | u64 | `86400` (24ч) | TTL подписанных URL в секундах |
| `require_signature` | bool | `false` | Требовать HMAC-верификацию на каждый запрос (flip в `true` в v0.19.1) |

```toml
[uploads]
signed_url_ttl_secs = 86400
require_signature = false
```

### [subagents]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `true` | Разрешить запуск субагентов |
| `default_mode` | String | `"in-process"` | Режим по умолчанию: `"in-process"` или `"docker"` |
| `max_concurrent_in_process` | u32 | `5` | Максимум одновременных in-process субагентов |
| `max_concurrent_docker` | u32 | `3` | Максимум одновременных Docker субагентов |
| `docker_timeout` | String | `"5m"` | Timeout для Docker-субагентов |
| `in_process_timeout` | String | `"2m"` | Timeout для in-process субагентов |
| `core_image` | Option\<String\> | — | Docker-образ Core для Docker-субагентов |

```toml
[subagents]
enabled = true
default_mode = "in-process"
max_concurrent_in_process = 5
max_concurrent_docker = 3
docker_timeout = "5m"
in_process_timeout = "2m"
```

### [discussion]

Конфигурация multi-agent discussion-режима. Все поля имеют дефолты.

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `max_rounds` | u32 | `2` | Максимум раундов обсуждения (1–3; >3 ведёт к sycophancy) |
| `agent_timeout_secs` | u64 | `120` | Timeout на ответ одного агента |
| `anonymize_after_round1` | bool | `true` | Анонимизировать ответы начиная со 2-го раунда |
| `devils_advocate` | bool | `true` | Последний агент играет роль devil's advocate |
| `synthesize` | bool | `true` | Финальный synthesizer-проход после всех раундов |
| `max_response_len` | usize | `1500` | Максимум символов ответа агента (перед обрезкой в следующем раунде) |

### [docker]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `compose_file` | String | `"docker/docker-compose.yml"` | Путь к docker-compose.yml |
| `rebuild_allowed` | Vec\<String\> | `[]` | Whitelist сервисов, разрешённых для rebuild/restart через API |
| `rebuild_timeout_secs` | u64 | `300` | Timeout команды rebuild в секундах |

```toml
[docker]
compose_file = "docker/docker-compose.yml"
rebuild_allowed = ["browser-renderer", "searxng"]
rebuild_timeout_secs = 300
```

### [sandbox]

Конфигурация sandbox для инструмента `code_exec`. Требует Docker.

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить инструмент `code_exec` |
| `image` | String | `"python:3.12-slim"` | Docker-образ для выполнения кода |
| `timeout_secs` | u64 | `30` | Timeout выполнения (контейнер убивается) |
| `memory_mb` | u32 | `256` | Лимит памяти на выполнение (МБ) |
| `cpu_limit` | f64 | `1.0` | Лимит CPU (дробные CPU, 1.0 = одно ядро) |
| `extra_binds` | Vec\<String\> | `[]` | Дополнительные volume mounts (e.g. `"docker/toolgate:/toolgate"`) |

```toml
[sandbox]
enabled = true
image = "hydeclaw-sandbox:latest"
extra_binds = []
```

### [memory]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `true` | Включить embedding-память |
| `embed_dim` | Option\<u32\> | — | Размерность векторов (авто-определяется при старте) |
| `embed_dimensions` | Option\<u32\> | — | Запрашиваемые dimensions для API (для моделей с гибким output, e.g. Qwen3-Embedding) |
| `fts_language` | Option\<String\> | — | PostgreSQL FTS-словарь (e.g. `"russian"`, `"english"`). Авто-определяется из языка base-агента |
| `pinned_budget_tokens` | u32 | `2000` | Максимум токенов для pinned chunks в L0 контексте |
| `compression_age_days` | u32 | `30` | Возраст (дней), после которого non-pinned chunks доступны для сжатия |

```toml
[memory]
# embed_dim = 2560  # авто-определяется при старте
# fts_language = "russian"
pinned_budget_tokens = 2000
compression_age_days = 30
```

> **Важно:** `embed_url` и `embed_model` не хранятся в toml — они управляются через реестр провайдеров (таблица `providers` в БД).

### [memory_worker]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `true` | Включить memory worker |
| `poll_interval_secs` | u64 | `5` | Интервал опроса очереди (catch-up safety net) в секундах |
| `notify_mode` | String | `"listen"` | Режим пробуждения: `"listen"` (первичный — PostgreSQL LISTEN/NOTIFY) или `"poll"` (только опрос, debug/back-compat) |

```toml
[memory_worker]
enabled = true
poll_interval_secs = 5
notify_mode = "listen"
```

> Memory worker — отдельный бинарник (`hydeclaw-memory-worker`). Режим `listen` использует `PgListener` на канале `memory_tasks_new` (migration 023), опрос остаётся как safety net.

### [backup]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить автоматические бэкапы |
| `cron` | String | `"0 0 5 * * *"` | Cron-расписание (6 полей: sec min hour dom mon dow). Дефолт: ежедневно в 05:00 UTC |
| `retention_days` | u32 | `7` | Хранить бэкапы N дней |
| `postgres_container` | String | `"docker-postgres-1"` | Имя PostgreSQL Docker-контейнера для `pg_dump`/`pg_restore` |

```toml
[backup]
enabled = true
cron = "0 3 * * *"
retention_days = 7
```

### [curator]

Планировщик ревизии навыков (skills). Запускает автоматический анализ и ремонт устаревших skills.

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить curator |
| `cron` | String | `"0 3 * * 0"` | Cron-расписание (каждое воскресенье в 03:00) |
| `min_idle_minutes` | u32 | `30` | Минимальное время простоя агента перед запуском |
| `stale_after_days` | u32 | `30` | Навыки старше N дней считаются устаревшими |
| `archive_after_days` | u32 | `90` | Навыки старше N дней архивируются |
| `max_repairs_per_run` | u32 | `10` | Максимум ремонтов за один запуск |
| `agent_name` | String | `"Hyde"` | Имя агента, выполняющего ревизию |

### [cleanup]

Настройки очистки таблицы хронологического лога событий сессий (`session_timeline`).

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `session_timeline_retention_days` | u32 | `7` | Хранить timeline-записи N дней. `0` = отключить очистку |
| `session_timeline_batch_size` | i64 | `5000` | Строк удаляется за одну batch-итерацию (минимизирует удержание блокировок) |

### [shutdown]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `drain_timeout_secs` | u64 | `30` | Максимум секунд ожидания завершения in-flight агентов при SIGTERM. systemd `TimeoutStopSec` должен быть `drain_timeout_secs + 10` |

### [tailscale]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить Tailscale serve интеграцию |
| `funnel` | bool | `false` | `true` = `tailscale funnel` (публичный интернет), `false` = `tailscale serve` (только Tailnet) |

### [otel]

OpenTelemetry (требует feature flag `otel` при компиляции).

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить экспорт трейсов. Также нужна env `OTEL_EXPORTER_OTLP_ENDPOINT` |
| `service_name` | String | `"hydeclaw-core"` | Имя сервиса, передаваемое в коллектор |

### [agent]

Глобальные дефолты параметров LLM. Переопределяются настройками конкретного агента.

> Этот раздел пока резервирован — конкретные поля `AgentSectionConfig` не имеют публичных полей (задел для будущего).

### [agent_tool]

Таймауты для инструмента `agent` (run/message/collect/status/kill). **Hot-reloadable** через config watcher и `PUT /api/config`.

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `message_wait_for_idle_secs` | u64 | `60` | Ожидание освобождения агента перед отправкой сообщения |
| `message_result_secs` | u64 | `300` | Ожидание `last_result` от агента (sync `run`, sync `message`, `collect`) |
| `safety_timeout_secs` | u64 | `600` | Внешний defense-in-depth timeout. Должен быть строго больше `message_wait_for_idle + message_result` |

```toml
[agent_tool]
message_wait_for_idle_secs = 60
message_result_secs = 300
safety_timeout_secs = 600
```

> Инвариант: `safety_timeout_secs > message_wait_for_idle_secs + message_result_secs`. При нарушении — только предупреждение, не ошибка.

### [[managed_process]]

Массив секций. Каждая описывает нативный дочерний процесс (не Docker), управляемый Core.

| Поле | Тип | Обязательно | Описание |
|------|-----|------------|----------|
| `name` | String | Да | Имя сервиса (`"channels"`, `"toolgate"`) |
| `command` | Vec\<String\> | Да | Команда + аргументы |
| `working_dir` | String | Да | Рабочая директория (относительно cwd Core) |
| `env_passthrough` | Vec\<String\> | Нет | Имена env-переменных, проксируемых из окружения Core |
| `env_extra` | HashMap\<String, String\> | Нет | Дополнительные env-переменные (поддерживает `${VAR}` подстановку) |
| `health_url` | Option\<String\> | Нет | URL health-check (зарезервировано для будущего) |
| `port` | Option\<u16\> | Нет | TCP-порт сервиса (для ожидания освобождения при рестарте) |
| `memory_max` | Option\<String\> | Нет | Лимит памяти (зарезервировано; лимиты управляются через systemd unit Core) |
| `cpu_quota` | Option\<String\> | Нет | Лимит CPU (зарезервировано) |

```toml
[[managed_process]]
name = "channels"
command = ["bun", "run", "src/index.ts"]
working_dir = "channels"
env_passthrough = ["HYDECLAW_AUTH_TOKEN"]
env_extra = { HYDECLAW_CORE_WS = "ws://localhost:18789", HEALTH_PORT = "3100" }
health_url = "http://localhost:3100/health"
port = 3100

[[managed_process]]
name = "toolgate"
command = [".venv/bin/python", "-m", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "9011", "--workers", "1", "--loop", "asyncio", "--limit-concurrency", "50"]
working_dir = "toolgate"
env_passthrough = ["HYDECLAW_AUTH_TOKEN"]
env_extra = { AUTH_TOKEN = "${HYDECLAW_AUTH_TOKEN}", INTERNAL_NETWORK = "127.0.0.0/8", CORE_API_URL = "http://localhost:18789" }
health_url = "http://localhost:9011/health"
port = 9011
```

### [mcp]

MCP-серверы в форме `[mcp.NAME]`. Альтернативный способ (workspace-level через `workspace/mcp/*.yaml`).

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `url` | Option\<String\> | — | Прямой URL (без Docker) |
| `container` | Option\<String\> | — | Имя Docker-контейнера (если `url` не задан) |
| `port` | Option\<u16\> | — | Docker-порт (если `url` не задан) |
| `mode` | String | `"on-demand"` | Режим запуска |
| `idle_timeout` | Option\<String\> | — | Timeout простоя для on-demand контейнеров |
| `protocol` | String | `"mcp"` | Протокол |
| `enabled` | bool | `true` | Включить сервер |

---

## 4. config/agents/{Name}.toml — конфиг агента

Файлы агентов хранятся в `config/agents/`. Имя файла — это **точное имя агента** (регистрозависимо). Загружаются при старте, поддерживают hot-reload.

> Поля `base` и флаги системного агента **никогда** не изменяются через PUT API — они всегда берутся с диска.

### [agent] — основные поля

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `name` | String | — | **Обязательно.** Имя агента (совпадает с именем файла) |
| `language` | String | `"ru"` | Язык агента (влияет на FTS-словарь памяти) |
| `provider` | String | — | **Обязательно.** LLM-провайдер (`"openai"`, `"anthropic"`, `"google"`, `"http"`) |
| `model` | String | — | **Обязательно.** Имя модели |
| `provider_connection` | Option\<String\> | — | Имя именованного провайдера из таблицы `providers` (переопределяет `provider`/`model`) |
| `fallback_provider` | Option\<String\> | — | Fallback провайдер при N подряд ошибках LLM |
| `tts_provider` | Option\<String\> | — | Имя TTS-провайдера для голосовых ответов через channel actions |
| `temperature` | f64 | `1.0` | Температура генерации |
| `max_tokens` | Option\<u32\> | — | Максимум output tokens (дефолт провайдера если не задано) |
| `base` | bool | `false` | Системный агент: нельзя переименовать/удалить через API, SOUL.md и IDENTITY.md только для чтения |
| `max_history_messages` | Option\<usize\> | `50` | Максимум сообщений истории в LLM-контексте |
| `max_tools_in_context` | Option\<usize\> | — | Лимит инструментов в контексте (relevance-based отбор при превышении) |
| `daily_budget_tokens` | u64 | `0` | Лимит токенов (input+output) в день. `0` = без лимита |
| `max_agent_turns` | Option\<usize\> | — | Переопределение глобального `limits.max_agent_turns` для этого агента |
| `max_failover_attempts` | u32 | `3` | Максимум попыток failover при multi-provider routing |
| `icon` | Option\<String\> | — | Путь к иконке агента (`"uploads/agent-icon.png"`) |

### [agent.access]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `mode` | String | `"open"` | `"open"` — доступен всем, `"restricted"` — только owner + approved users |
| `owner_id` | Option\<String\> | — | User ID владельца бота (автоматически разрешён в restricted-режиме) |

```toml
[agent.access]
mode = "restricted"
owner_id = "123456789"
```

### [agent.heartbeat]

| Поле | Тип | Обязательно | Описание |
|------|-----|------------|----------|
| `cron` | String | Да | Cron-расписание запуска heartbeat |
| `timezone` | Option\<String\> | Нет | Часовой пояс для cron (e.g. `"Europe/Moscow"`) |
| `announce_to` | Option\<String\> | Нет | Канал для анонса результатов (`"telegram"`) |

```toml
[agent.heartbeat]
cron = "0 9 * * *"
timezone = "Europe/Moscow"
announce_to = "telegram"
```

### [agent.tools]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `allow` | Vec\<String\> | `[]` | Белый список инструментов |
| `deny` | Vec\<String\> | `[]` | Чёрный список инструментов (проверяется первым, применяется ко ВСЕМ инструментам включая системные) |
| `allow_all` | bool | `false` | Разрешить все инструменты |
| `deny_all_others` | bool | `false` | Запрещать все инструменты кроме перечисленных в `allow` |
| `groups.git` | bool | `true` | Группа git-инструментов (`git_status`, `git_diff`, `git_commit`, `git_push`, `git_pull`, `git_ssh_key`) |
| `groups.tool_management` | bool | `true` | Группа управления инструментами (`tool_create`, `tool_list`, `tool_test`, ...) |
| `groups.skill_editing` | bool | `true` | Группа редактирования навыков (`skill_create`, `skill_update`, `skill_list`) |
| `groups.session_tools` | bool | `true` | Группа сессионных инструментов (`sessions_list`, `sessions_history`, ...) |

```toml
[agent.tools]
deny = ["code_exec", "workspace_delete"]
```

### [agent.delegation]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `max_depth` | u8 | `1` | Максимальная глубина рекурсивного порождения субагентов. `1` = субагенты НЕ могут порождать дальнейших. Минимум `1`. |
| `blocked_tools_extra` | Vec\<String\> | `[]` | Добавить в deny-list субагентов (расширяет `SUBAGENT_DENIED_TOOLS`) |
| `blocked_tools_override` | Vec\<String\> | `[]` | Если непусто — ЗАМЕНЯЕТ весь `SUBAGENT_DENIED_TOOLS` |

> Нельзя задавать `blocked_tools_extra` и `blocked_tools_override` одновременно.

```toml
[agent.delegation]
max_depth = 2
blocked_tools_extra = ["code_exec"]
```

### [agent.compaction]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `true` | Включить автосжатие контекста |
| `threshold` | f64 | `0.8` | Доля заполнения контекста, при которой срабатывает сжатие |
| `preserve_tool_calls` | bool | `false` | Сохранять tool calls в сжатом контексте |
| `preserve_last_n` | u32 | `10` | Количество последних сообщений, которые никогда не сжимаются |
| `max_context_tokens` | Option\<u32\> | — | Переопределение максимума контекстных токенов (авто-определяется из имени модели) |
| `protect_first_n` | usize | `3` | Head protection: первые N сообщений (system + first user + first assistant) всегда сохраняются |
| `summary_target_ratio` | f64 | `0.20` | Доля бюджета токенов для tail (tail_budget = context_limit * threshold * ratio) |
| `anti_thrash_min_savings` | f64 | `0.10` | Минимальное сокращение (доля) для применения сжатия. Защита от thrashing. |
| `anti_thrash_max_skips` | u8 | `2` | Максимум неэффективных сжатий подряд до пропуска |
| `extract_to_memory` | bool | `true` | Извлекать факты в pgvector рядом с summary |

### [agent.skill_review]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить фоновый анализ навыков после сессий |
| `min_tool_calls` | u32 | `3` | Минимум tool-вызовов в сессии для запуска анализа |

### [agent.session]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `dm_scope` | String | `"per-channel-peer"` | Область DM-сессий: `"shared"`, `"per-channel-peer"`, `"per-peer"`, `"per-chat"` |
| `ttl_days` | u32 | `30` | Удалять сессии старше N дней. `0` = никогда |
| `max_messages` | u32 | `0` | Максимум сообщений в сессии. `0` = без лимита |
| `prune_tool_output_after_turns` | Option\<usize\> | — | Проактивно заменять tool-результаты старше N turns на `"[output omitted, N chars]"` перед первым LLM-вызовом |

### [agent.approval]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `enabled` | bool | `false` | Включить систему подтверждений |
| `require_for` | Vec\<String\> | `[]` | Конкретные инструменты, требующие подтверждения |
| `require_for_categories` | Vec\<String\> | `[]` | Категории инструментов: `"system"`, `"destructive"`, `"external"` |
| `timeout_seconds` | u64 | `300` | Timeout авто-отклонения (5 минут) |

### [agent.tool_loop]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `max_iterations` | usize | `50` | Максимум итераций tool loop до принудительного финального ответа |
| `compact_on_overflow` | bool | `true` | Попытка mid-loop сжатия при переполнении контекста |
| `detect_loops` | bool | `true` | Включить детектор зацикливания |
| `warn_threshold` | usize | `5` | Одинаковых последовательных вызовов до предупреждения |
| `break_threshold` | usize | `10` | Одинаковых последовательных вызовов до остановки |
| `max_consecutive_failures` | usize | `3` | Подряд ошибок LLM до переключения на fallback провайдер |
| `max_auto_continues` | u8 | `5` | Максимум авто-продолжений при незавершённом ответе LLM |
| `max_loop_nudges` | usize | `3` | Максимум nudge-сообщений о зацикливании до force-stop |
| `ngram_cycle_length` | usize | `6` | Максимальная длина цикла в n-gram детекции (3..=N) |
| `error_break_threshold` | Option\<usize\> | `3` | Подряд ошибок одного инструмента до прерывания |

### [agent.watchdog]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `inactivity_secs` | u64 | `600` | Секунд бездействия сессии до принудительного завершения watchdog-ом |

### [agent.hooks]

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `log_all_tool_calls` | bool | `false` | Логировать каждый tool call и результат через tracing |
| `block_tools` | Vec\<String\> | `[]` | Тихо блокировать инструменты (без диалога подтверждения) |
| `webhooks` | Vec\<WebhookConfig\> | `[]` | Исходящие HTTP-webhook подписки |

### [[agent.hooks.webhooks]]

| Поле | Тип | Описание |
|------|-----|----------|
| `url` | String | Абсолютный HTTP/HTTPS URL (обязательно) |
| `events` | Vec\<String\> | События для подписки: `"BeforeMessage"`, `"AfterResponse"`, `"BeforeToolCall"`, `"AfterToolResult"`, `"OnError"` |

```toml
[agent.hooks]
log_all_tool_calls = true

[[agent.hooks.webhooks]]
url = "https://example.com/hook"
events = ["BeforeToolCall", "AfterToolResult"]
```

### [[agent.routing]]

Multi-provider routing (переопределяет `provider`/`model`). Правила оцениваются по порядку, первое совпадение побеждает.

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `condition` | String | `"default"` | Условие: `"default"` / `"always"`, `"short"` (<300 chars), `"long"` (>2000 chars), `"with_tools"`, `"financial"`, `"analytical"`, `"code"`, `"fallback"` |
| `connection` | Option\<String\> | — | Имя провайдера из таблицы `providers` |
| `model` | Option\<String\> | — | Переопределение модели |
| `temperature` | Option\<f64\> | — | Переопределение температуры |
| `cooldown_secs` | u64 | `60` | Cooldown после failover-ошибки. Минимум `1`. |

```toml
[[agent.routing]]
condition = "code"
connection = "deepseek-coder"
model = "deepseek-coder-v2"

[[agent.routing]]
condition = "default"
connection = "claude-sonnet"
```

### Пример полного конфига агента

```toml
[agent]
name = "Hyde"
language = "ru"
provider = "anthropic"
model = "claude-sonnet-4-5"
temperature = 1.0
base = true
max_history_messages = 100
daily_budget_tokens = 0

[agent.access]
mode = "restricted"
owner_id = "123456789"

[agent.tools]
deny = []

[agent.delegation]
max_depth = 2
blocked_tools_extra = []

[agent.compaction]
enabled = true
threshold = 0.8
preserve_last_n = 10
extract_to_memory = true

[agent.session]
dm_scope = "per-channel-peer"
ttl_days = 30

[agent.tool_loop]
max_iterations = 50
detect_loops = true

[agent.watchdog]
inactivity_secs = 600
```

---

## 5. workspace/tools/*.yaml — YAML-инструменты

Декларативные HTTP-инструменты. Каждый файл — один инструмент. Загружаются при старте, доступны через `find_yaml_tool(workspace_dir, name)`.

Имя файла: `{tool_name}.yaml`. Имена инструментов: только `[a-zA-Z0-9_-]`.

### Поля YamlToolDef

| Поле | Тип | Обязательно | Описание |
|------|-----|------------|----------|
| `name` | String | Да | Имя инструмента (уникально, `[a-zA-Z0-9_-]`) |
| `description` | String | Да | Описание для LLM (JSON Schema description) |
| `endpoint` | String | Да | URL вызываемого HTTP-эндпоинта. Поддерживает path params: `{param}` |
| `method` | String | Да | HTTP-метод: `GET`, `POST`, `PUT`, `DELETE`, `PATCH` |
| `status` | String | `"verified"` | Статус: `"verified"`, `"draft"`, `"disabled"` |
| `extends` | Option\<String\> | Нет | Наследует поля из другого инструмента (e.g. `extends: github` для общих заголовков) |
| `headers` | Map\<String, String\> | Нет | Дополнительные HTTP-заголовки |
| `parameters` | Map\<String, YamlParam\> | Нет | Параметры инструмента (JSON Schema для LLM) |
| `auth` | Option\<YamlAuth\> | Нет | Конфигурация аутентификации |
| `body_template` | Option\<String\> | Нет | Mustache-шаблон тела запроса (`{{param}}` подстановка). Переменные окружения: `${ENV_VAR}` |
| `response_transform` | Option\<String\> | Нет | JSONPath-выражение для извлечения поля из ответа. `"$"` = весь ответ |
| `response_pipeline` | Vec | Нет | Пайплайн обработки ответа: `jsonpath`, `pick_fields`, `sort_by`, `limit` |
| `channel_action` | Option | Нет | Побочный эффект после вызова (отправить голос/файл через канал) |
| `timeout` | u64 | `60` | Timeout вызова в секундах |
| `content_type` | String | `"application/json"` | Content-Type тела запроса |
| `required_base` | bool | `false` | Только для `base = true` агентов |
| `parallel` | bool | `false` | Безопасен для параллельного выполнения с другими parallel-safe инструментами |
| `required_secrets` | Vec\<String\> | `[]` | Секреты, необходимые внутренним роутерам toolgate |
| `rate_limit.max_calls_per_minute` | Option\<u32\> | Нет | Лимит вызовов в минуту (зарезервировано) |
| `retry.max_attempts` | u32 | `1` | Максимум попыток при transient-ошибках |
| `retry.backoff_base_ms` | u64 | `1000` | База backoff в миллисекундах |
| `retry.retry_on` | Vec\<u16\> | `[429, 500, 502, 503, 504]` | HTTP-коды для повтора |
| `cache.ttl` | u64 | — | TTL кэша в секундах (forward-compat, не используется в runtime) |
| `response_schema` | Option\<JSON\> | Нет | Схема ответа для LLM (добавляется к description) |
| `graphql.query` | String | — | GraphQL-запрос (переопределяет `body_template`) |
| `graphql.variables` | Map | Нет | Переменные GraphQL с `{{param}}` подстановкой |
| `tags` | Vec\<String\> | `[]` | Теги (для документации/фильтрации) |
| `created_by` | String | `""` | Автор инструмента |

### Поля YamlParam (параметры инструмента)

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `type` | String | `"string"` | Тип: `"string"`, `"integer"`, `"number"`, `"boolean"`, `"array"`, `"object"` |
| `required` | bool | `false` | Обязательный параметр |
| `location` | String | `"body"` | Размещение: `"body"`, `"query"`, `"path"`, `"header"` |
| `description` | String | `""` | Описание для LLM |
| `default` | Option\<JSON\> | — | Значение по умолчанию |
| `default_from_env` | Option\<String\> | — | Имя env/secret для дефолта (если LLM не предоставил значение) |
| `enum` | Vec\<String\> | `[]` | Допустимые значения |
| `minimum` | Option\<f64\> | — | Минимальное значение (для числовых) |
| `maximum` | Option\<f64\> | — | Максимальное значение (для числовых) |
| `examples` | Vec\<String\> | `[]` | Примеры (добавляются к description) |

### Конфигурация аутентификации (YamlAuth)

| `type` | Поля | Описание |
|--------|------|----------|
| `bearer_env` | `key: ENV_VAR` | Bearer-токен из env/vault |
| `basic_env` | `username_key`, `password_key` | HTTP Basic из двух env/vault переменных |
| `api_key_header` | `key: ENV_VAR`, `header_name: "X-API-Key"` | API-ключ в заголовке |
| `api_key_query` | `key: ENV_VAR`, `param_name: "api_key"` | API-ключ как query-параметр |
| `custom` | `headers: { "Header": "${ENV_VAR}" }` | Произвольные заголовки с env-подстановкой |
| `oauth_refresh` | `key`, `token_url`, `token_body`, `token_field` | OAuth2 refresh token flow |
| `oauth_provider` | — | Использует OAuth2 access token из OAuthManager |
| `none` | — | Без аутентификации |

### channel_action

| Поле | Значения | Описание |
|------|----------|----------|
| `action` | `"send_voice"`, `"send_file"`, `"send_photo"` | Тип канального действия |
| `data_field` | `"_binary"` или JSONPath | Источник данных: `"_binary"` = бинарный ответ, JSONPath = поле JSON |

### Примеры инструментов

**Внешний API с auth:**
```yaml
name: tavily_search
description: "AI-optimized web search via Tavily."
endpoint: "https://api.tavily.com/search"
method: POST
auth:
  type: bearer_env
  key: TAVILY_API_KEY
body_template: '{"query":"{{query}}","search_depth":"{{search_depth}}","include_answer":true,"max_results":5}'
parameters:
  query:
    type: string
    required: true
    description: "Search query"
  search_depth:
    type: string
    default: "basic"
    description: "'basic' or 'advanced'"
response_transform: "$.results"
status: draft
```

**Path-параметры + Bearer auth:**
```yaml
name: ha_call_service
endpoint: "http://homeassistant.local:8123/api/services/{domain}/{service}"
method: POST
parameters:
  domain:
    type: string
    required: true
    location: path
  service:
    type: string
    required: true
    location: path
auth:
  type: bearer_env
  key: HA_TOKEN
```

**Channel action (голосовой ответ):**
```yaml
name: synthesize_speech
endpoint: "http://localhost:9011/v1/audio/speech"
method: POST
parameters:
  text:
    type: string
    required: true
    location: body
body_template: '{"input": "{{text}}", "response_format": "opus"}'
channel_action:
  action: send_voice
  data_field: "_binary"
```

---

## 6. workspace/mcp/*.yaml — MCP-серверы

Каждый файл описывает один MCP-сервер. Имя файла = имя сервера.

| Поле | Тип | Дефолт | Описание |
|------|-----|--------|----------|
| `name` | String | — | Имя сервера (из имени файла) |
| `url` | Option\<String\> | — | Прямой URL (без Docker) |
| `container` | Option\<String\> | — | Docker-контейнер |
| `port` | Option\<u16\> | — | Docker-порт |
| `mode` | String | `"on-demand"` | Режим запуска |
| `idle_timeout` | Option\<String\> | — | Timeout простоя |
| `protocol` | String | `"mcp"` | Протокол |
| `enabled` | bool | `true` | Включить |

---

## 7. Хранилище секретов (Secrets Vault)

Секреты шифруются ChaCha20-Poly1305 (мастер-ключ из `HYDECLAW_MASTER_KEY`) и хранятся в таблице `secrets`.

### Структура записи

- `name` — имя секрета (e.g. `TAVILY_API_KEY`)
- `scope` — область видимости: `""` = глобальный, `"AgentName"` = per-agent

### Порядок разрешения (resolution order)

Для каждого секрета с именем `NAME` и агентом `SCOPE`:

1. `(NAME, SCOPE)` — per-agent секрет (vault)
2. `(NAME, "")` — глобальный секрет (vault)
3. `std::env::var(NAME)` — env-переменная (устаревший fallback, с предупреждением)

Метод `get_scoped(name, scope)` реализует эту цепочку. Метод `get(name)` — только шаги 2–3.

### Работа с секретами

- **Через UI:** Settings → Secrets
- **Через API:** `GET /api/secrets`, `POST /api/secrets`, `DELETE /api/secrets/{name}`
- **Scoped секреты:** `POST /api/secrets` с полем `scope: "AgentName"`
- **Channel credentials** (bot_token и т.д.) хранятся под ключом `CHANNEL_CREDENTIALS`, scope = UUID канала. В JSONB-колонке `config` таблицы `agent_channels` credentials **отсутствуют** — они redacted при записи и re-injected из vault при `GET ?reveal=true`.

### Бэкап секретов

Бэкап через `GET /api/backup` включает расшифрованные секреты — это сделано намеренно (portability при смене мастер-ключа). Защита: требует auth token + заголовок `X-Confirm-Restore` при восстановлении.

---

## 8. Toolgate — конфиг провайдеров

Toolgate (`toolgate/`) не имеет статического конфиг-файла. Конфигурация провайдеров загружается динамически из Core API:

- **Эндпоинт:** `GET /api/media-config` (аутентифицированный)
- **Retry:** 5 попыток с backoff 2s, 4s, 6s, 8s, 10s
- **Деградированный режим:** если Core недоступен после 5 попыток, toolgate стартует без провайдеров (503 на capability-эндпоинтах до восстановления связи)

### Переменные окружения toolgate

| Переменная | Дефолт | Описание |
|-----------|--------|----------|
| `CORE_API_URL` | `http://127.0.0.1:18789` | URL Core API для загрузки конфига провайдеров |
| `HYDECLAW_AUTH_TOKEN` / `AUTH_TOKEN` | — | Bearer-токен для аутентификации в Core |
| `INTERNAL_NETWORK` | `127.0.0.0/8` | CIDR внутренней сети (для SSRF-защиты) |

### Структура провайдера (ProviderConfig)

| Поле | Описание |
|------|----------|
| `type` | Тип capability: `"stt"`, `"tts"`, `"vision"`, `"imagegen"`, `"embedding"` |
| `driver` | Драйвер провайдера (e.g. `"openai"`, `"ollama"`, `"qwen3-tts"`) |
| `base_url` | Base URL сервиса |
| `model` | Имя модели (опционально) |
| `api_key` | API-ключ (опционально) |
| `enabled` | Включён ли провайдер |
| `options` | Дополнительные опции (e.g. `{"voice": "nova"}` для TTS) |

Активные провайдеры по capability управляются через UI (Settings → Active Providers) и хранятся в таблице `provider_active`.

---

## 9. Развёртывание на Pi (ARM64)

### Пути на Pi

| Ресурс | Путь |
|--------|------|
| Core binary | `~/hydeclaw/hydeclaw-core-aarch64` |
| Watchdog binary | `~/hydeclaw/hydeclaw-watchdog-aarch64` |
| Memory worker binary | `~/hydeclaw/hydeclaw-memory-worker-aarch64` |
| Config | `~/hydeclaw/config/` |
| Workspace | `~/hydeclaw/workspace/` |
| UI static | `~/hydeclaw/ui/out/` |
| Migrations | `~/hydeclaw/migrations/` |
| Docker configs | `~/hydeclaw/docker/` |
| Toolgate source | `~/hydeclaw/toolgate/` |
| `.env` | `~/hydeclaw/.env` (рядом с бинарником) |

### Deploy workflow

```bash
# Полный deploy
make deploy              # build-arm64 + scp binary + restart systemd + deploy UI + migrations

# Частичный deploy
make deploy-binary       # только бинарник
make deploy-ui           # только UI (npm build + RSC flattening + scp)

# Toolgate (нет Docker, только .py файлы):
scp toolgate/changed_file.py user@pi:~/hydeclaw/toolgate/
curl -X POST http://pi:18789/api/services/toolgate/restart -H "Authorization: Bearer $TOKEN"

# Проверка здоровья
make doctor              # GET /api/doctor
make logs                # journalctl --user -u hydeclaw-core -f
```

### Переменная PI_HOST

```bash
export PI_HOST=aronmav@192.168.1.82
make deploy
```

### Компиляция для ARM64

```bash
make build-arm64         # cargo zigbuild --target aarch64-unknown-linux-gnu
```

> **Важно:** Все crates используют `rustls-tls` feature flags. OpenSSL отсутствует полностью — это позволяет кросс-компиляцию через `zigbuild` без нативного toolchain.

### systemd unit (пример)

```ini
[Service]
ExecStart=/home/user/hydeclaw/hydeclaw-core-aarch64 config/hydeclaw.toml
WorkingDirectory=/home/user/hydeclaw
EnvironmentFile=/home/user/hydeclaw/.env
TimeoutStopSec=40
```

`TimeoutStopSec` должен быть `shutdown.drain_timeout_secs + 10` (дефолт: 30 + 10 = 40).

---

*Документ сгенерирован на основе исходного кода: `crates/hydeclaw-core/src/config/mod.rs`, `crates/hydeclaw-core/src/memory/mod.rs`, `crates/hydeclaw-core/src/secrets.rs`, `crates/hydeclaw-core/src/tools/yaml_tools.rs`, `crates/hydeclaw-memory-worker/src/config.rs`, `config/hydeclaw.toml`, `toolgate/config.py`.*
