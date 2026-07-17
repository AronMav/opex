# OPEX API Reference

**Base URL:** `http://<host>:18789`

**Аутентификация:** Все маршруты требуют `Authorization: Bearer <OPEX_AUTH_TOKEN>`, если явно не помечены как **Public**. Токен настраивается через `gateway.auth_token_env` в `opex.toml`.

**Rate limiting:** 500 неудачных попыток авторизации с одного IP активируют блокировку на 30 секунд. Общий rate limiting запросов настраивается через `limits.max_requests_per_minute`. Loopback и аутентифицированные запросы освобождены.

---

## Содержание

1. [Аутентификация](#1-аутентификация)
2. [Мониторинг и здоровье](#2-мониторинг-и-здоровье)
3. [Агенты](#3-агенты)
4. [Chat — OpenAI-Compatible](#4-chat--openai-compatible)
5. [Chat SSE (Native Streaming)](#5-chat-sse-native-streaming)
6. [Сессии и сообщения](#6-сессии-и-сообщения)
7. [Память](#7-память)
8. [Инструменты и MCP](#8-инструменты-и-mcp)
9. [YAML-инструменты](#9-yaml-инструменты)
10. [Навыки](#10-навыки)
11. [Каналы](#11-каналы)
12. [Cron-задачи](#12-cron-задачи)
13. [Задачи](#13-задачи)
14. [Подтверждения](#14-подтверждения)
15. [Webhooks](#15-webhooks)
16. [Секреты](#16-секреты)
17. [Конфигурация](#17-конфигурация)
18. [Резервное копирование и восстановление](#18-резервное-копирование-и-восстановление)
19. [Сервисы](#19-сервисы)
20. [Watchdog](#20-watchdog)
21. [Провайдеры](#21-провайдеры)
22. [TTS и Canvas](#22-tts-и-canvas)
23. [Загрузка медиа](#23-загрузка-медиа)
24. [Workspace](#24-workspace)
25. [Файлы Workspace (Подписанные URL)](#25-файлы-workspace-подписанные-url)
26. [OAuth](#26-oauth)
27. [Доступ / Сопряжение](#27-доступ--сопряжение)
28. [WebSocket (UI Events)](#28-websocket-ui-events)
29. [Email-триггеры (Gmail)](#29-email-триггеры-gmail)
30. [Интеграция с GitHub](#30-интеграция-с-github)
31. [Настройка](#31-настройка)
32. [Сеть](#32-сеть)
33. [Уведомления](#33-уведомления)
34. [Куратор](#34-куратор)
35. [Ошибки сессий](#35-ошибки-сессий)
36. [CSP-отчёты](#36-csp-отчёты)

---

## 1. Аутентификация

### WS Ticket

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `POST` | `/api/auth/ws-ticket` | Required | Выдать одноразовый WebSocket-тикет |

Эндпоинт WebSocket (`/ws`) требует аутентификации. Используйте этот эндпоинт для получения краткоживущего тикета и передайте его как `?ticket=<uuid>`.

**Ответ:**
```json
{ "ticket": "uuid-v4-string" }
```

Тикеты действительны **30 секунд** и используются однократно.

---

## 2. Мониторинг и здоровье

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `GET` | `/health` | Public | Проверка живости — возвращает `200 OK`, без тела |
| `GET` | `/api/status` | Required | Полный статус шлюза |
| `GET` | `/api/stats` | Required | Статистика сообщений/сессий |
| `GET` | `/api/usage` | Required | Сводка по использованию токенов |
| `GET` | `/api/usage/daily` | Required | Разбивка использования токенов по дням |
| `GET` | `/api/usage/sessions` | Required | Использование токенов по сессиям |
| `GET` | `/api/doctor` | Required | Глубокая проверка здоровья всех подсистем |
| `GET` | `/api/health/dashboard` | Required | Дашборд метрик времени выполнения |
| `GET` | `/api/audit` | Required | Лог событий аудита |
| `GET` | `/api/audit/tools` | Required | Лог вызовов инструментов (аудит) |

### GET /api/status

**Ответ:**
```json
{
  "status": "ok",
  "version": "0.x.x",
  "uptime_seconds": 12345,
  "db": true,
  "listen": "0.0.0.0:18789",
  "agents": ["main", "analyst"],
  "memory_chunks": 2901,
  "scheduled_jobs": 3,
  "active_sessions": 5,
  "tools_registered": 12
}
```

### GET /api/stats

**Ответ:**
```json
{
  "messages_today": 42,
  "sessions_today": 5,
  "total_messages": 18000,
  "total_sessions": 592,
  "recent_sessions": [
    {
      "id": "uuid",
      "agent_id": "main",
      "channel": "ui",
      "last_message_at": "2026-03-27T10:00:00Z",
      "title": "Session title"
    }
  ]
}
```

### GET /api/usage

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `days` | integer | 30 | Окно просмотра назад |
| `agent` | string | — | Фильтр по имени агента |

### GET /api/doctor

Возвращает статус здоровья всех подсистем с измерениями задержки.

**Ответ:**
```json
{
  "ok": true,
  "checks": {
    "database": { "status": "ok", "latency_ms": 2, "message": "..." },
    "toolgate":  { "status": "ok", "latency_ms": 15, "message": "..." },
    "secrets":   { "status": "ok", "message": "..." },
    "channels":  { "status": "ok", "latency_ms": 3, "message": "..." }
  }
}
```

Каждая проверка имеет `status` (`"ok"`, `"warn"` или `"error"`), `message`, опциональные `latency_ms`, `fix_hint` и `details`.

### GET /api/health/dashboard

Возвращает счётчики времени выполнения и размеры пулов. Неизвестные поля непрозрачны — клиенты не должны предполагать стабильность набора полей.

**Ответ:**
```json
{
  "version": "0.x.x",
  "sse_events_dropped_total": { "<agent>": { "<event_type>": 0 } },
  "csp_violations": {},
  "csp_violations_overflow": 0,
  "active_agents": 2,
  "sse_streams": 1,
  "approval_waiters": 0,
  "auth_rate_limiter_size": 0,
  "request_rate_limiter_size": 0,
  "stream_registry_size": 1,
  "db_pool_total": 5,
  "db_pool_idle": 4,
  "memory_worker_heartbeat_age_secs": 120,
  "session_timeline_table_size_bytes": 204800,
  "uptime_secs": 3600
}
```

### GET /api/audit

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `agent` | string | — | Фильтр по имени агента |
| `event_type` | string | — | Фильтр по типу события |
| `search` | string | — | Подстрочный поиск (без учёта регистра) по агенту, типу события, актору и JSON-деталям; спецсимволы LIKE экранируются |
| `limit` | integer | 100 | Максимум результатов (max 500) |
| `offset` | integer | 0 | Смещение для пагинации |

### GET /api/audit/tools

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `agent` | string | — | Фильтр по агенту |
| `tool` | string | — | Фильтр по имени инструмента |
| `days` | integer | 7 | Окно просмотра назад |
| `limit` | integer | 100 | Максимум результатов (max 500) |

---

## 3. Агенты

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/agents` | Список всех агентов (базовые первыми, затем по алфавиту) |
| `POST` | `/api/agents` | Создать нового агента |
| `GET` | `/api/agents/{name}` | Получить детали агента |
| `PUT` | `/api/agents/{name}` | Обновить конфигурацию агента |
| `DELETE` | `/api/agents/{name}` | Удалить агента |
| `POST` | `/api/agents/{name}/model-override` | Временно переопределить модель LLM в памяти |
| `GET` | `/api/agents/{name}/tasks` | Список задач для агента |
| `GET` | `/api/agents/{name}/hooks` | Получить конфигурацию хуков агента |

### GET /api/agents

**Ответ:**
```json
{
  "agents": [
    {
      "name": "main",
      "language": "ru",
      "model": "MiniMax-M2.5",
      "provider": "minimax",
      "icon": "🤖",
      "temperature": 1.0,
      "has_access": true,
      "access_mode": "allowlist",
      "has_heartbeat": false,
      "heartbeat_cron": null,
      "heartbeat_timezone": null,
      "tool_policy": { "allow": [], "deny": [], "allow_all": true },
      "routing_count": 0,
      "is_running": true,
      "config_dirty": false,
      "base": true
    }
  ]
}
```

### POST /api/agents

Создать нового агента. Конфиг записывается в `config/agents/{name}.toml`, агент запускается немедленно. Первый созданный агент автоматически получает `base = true` с ограниченными настройками доступа по умолчанию.

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `name` | string | Да | Имя агента (буквенно-цифровые символы, `-`, `_`, максимум 32 символа) |
| `provider` | string | Да | Тип LLM-провайдера (например, `minimax`, `openai`, `anthropic`) |
| `model` | string | Да | Идентификатор модели |
| `provider_connection` | string | Нет | Именованный ID соединения с LLM-провайдером (переопределяет provider/model) |
| `language` | string | Нет | Подсказка языка ответа |
| `temperature` | float | Нет | Температура сэмплирования |
| `max_tokens` | integer | Нет | Максимум токенов ответа |
| `icon` | string | Нет | Emoji или иконка |
| `voice` | string | Нет | Название голоса TTS (хранится как scoped secret `TTS_VOICE`) |
| `access` | object\|null | Нет | Конфигурация контроля доступа |
| `heartbeat` | object\|null | Нет | Конфигурация heartbeat cron |
| `tools` | object\|null | Нет | Политика инструментов |
| `compaction` | object\|null | Нет | Конфигурация сжатия контекста |
| `session` | object\|null | Нет | Конфигурация управления сессиями |
| `routing` | array\|null | Нет | Правила маршрутизации LLM |
| `approval` | object\|null | Нет | Конфигурация подтверждения человеком |
| `tool_loop` | object\|null | Нет | Конфигурация цикла инструментов |
| `max_tools_in_context` | integer | Нет | Максимум определений инструментов, вводимых в контекст |
| `max_history_messages` | integer | Нет | Максимум сообщений из истории сессий |
| `daily_budget_tokens` | integer | Нет | Дневной лимит токенов (0 = без лимита) |

**Объект `access`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `mode` | string | `"allowlist"`, `"open"` или `"restricted"` |
| `owner_id` | string | ID пользователя-владельца канала с правами администратора |

**Объект `heartbeat`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `cron` | string | Cron-выражение |
| `timezone` | string | IANA-часовой пояс (по умолчанию `"UTC"`) |
| `announce_to` | string | Имя канала для публикации heartbeat-сообщений |

**Объект `tools`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `allow` | array | Явно разрешённые имена инструментов |
| `deny` | array | Явно запрещённые имена инструментов |
| `allow_all` | bool | Если true, все инструменты разрешены по умолчанию |
| `deny_all_others` | bool | Если true, разрешён только список `allow` |
| `groups.git` | bool | Включить группу git-инструментов |
| `groups.tool_management` | bool | Включить инструменты управления инструментами |
| `groups.skill_editing` | bool | Включить инструменты редактирования навыков |
| `groups.session_tools` | bool | Включить инструменты управления сессиями |

**Объект `compaction`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `enabled` | bool | Включить автоматическое сжатие контекста |
| `threshold` | float | Порог заполнения токенов, при котором запускается сжатие |
| `preserve_tool_calls` | bool | Сохранять пары вызов/результат инструмента в сводке |
| `preserve_last_n` | integer | Всегда сохранять последние N сообщений |
| `max_context_tokens` | integer | Жёсткий лимит перед аварийным сжатием |

**Объект `approval`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `enabled` | bool | Включить подтверждение человеком |
| `require_for` | array | Имена инструментов, требующих подтверждения |
| `require_for_categories` | array | Категории инструментов, требующих подтверждения |
| `timeout_seconds` | integer | Секунды ожидания перед авто-отклонением |

**Объект `tool_loop`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `max_iterations` | integer | Максимум вызовов инструментов на ход сессии |
| `compact_on_overflow` | bool | Запускать сжатие при превышении числа итераций |
| `detect_loops` | bool | Включить n-gram детекцию зацикливания |
| `warn_threshold` | integer | Количество итераций для предупреждения о зацикливании |
| `break_threshold` | integer | Количество итераций для принудительного прерывания |
| `max_consecutive_failures` | integer | Подряд идущих ошибок инструмента до прерывания |
| `max_auto_continues` | integer | Максимум автоматических продолжений |
| `max_loop_nudges` | integer | Максимум nudge-сообщений о зацикливании |
| `ngram_cycle_length` | integer | N-gram окно для детекции циклов |

**Объект `session`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `dm_scope` | string | Идентификатор области DM-сессий |
| `ttl_days` | integer | Дней до истечения простаивающих сессий |
| `max_messages` | integer | Максимум сообщений до авто-сжатия |
| `prune_tool_output_after_turns` | integer | Удалять вывод инструмента из контекста через N ходов |

**Объект `skill_review`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `enabled` | bool | Включить проверку навыков после сессий |
| `min_tool_calls` | integer | Минимум вызовов инструментов для запуска проверки |

**Объект `hooks`:**

| Поле | Тип | Описание |
|------|-----|----------|
| `log_all_tool_calls` | bool | Логировать каждый вызов инструмента в аудит |
| `block_tools` | array | Имена инструментов для блокировки (альтернатива deny-списку) |

**Ответ:** `{ "ok": true, "name": "agent-name" }`

### GET /api/agents/{name}

Возвращает полный `AgentDetailDto`. Флаг `config_dirty` равен `true`, когда работающая конфигурация расходится с конфигурацией на диске.

### PUT /api/agents/{name}

Обновить конфигурацию агента. Принимает те же поля, что и `POST /api/agents`. Семантика слияния полей:
- **Поле отсутствует в запросе**: существующее значение сохраняется.
- **Явный `null`**: значение очищается.
- **Значение предоставлено**: значение обновляется.

Флаги `base` **никогда** не изменяются через PUT — берутся с диска.
Переименование агента (через поле `name` в запросе) обновляет 21 таблицу БД в транзакции, переименовывает директорию workspace и переносит scoped-секреты. Базовые агенты не могут быть переименованы.

**Ответ:** `{ "ok": true, "name": "new-name" }`

### DELETE /api/agents/{name}

Останавливает и удаляет агента. Файл конфигурации удаляется. Возвращает `{ "ok": true }`.

### POST /api/agents/{name}/model-override

Временно переопределить модель LLM для запущенного агента (в памяти, теряется при перезапуске).

**Тело запроса:**
```json
{ "model": "gpt-4o", "provider": "openai" }
```

---

## 4. Chat — OpenAI-Compatible

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `POST` | `/v1/chat/completions` | Required | Завершения чата в формате OpenAI |
| `GET` | `/v1/models` | Required | Список доступных моделей |
| `POST` | `/v1/embeddings` | Required | Проксировать запрос эмбеддингов в Toolgate |

### POST /v1/chat/completions

Завершения чата в формате OpenAI. Поддерживает потоковый (`"stream": true`) и непотоковый режимы.

**Тело запроса:**

| Поле | Тип | Описание |
|------|-----|----------|
| `messages` | array | Массив сообщений в формате OpenAI |
| `model` | string | Имя модели (информационное) |
| `temperature` | float | Температура сэмплирования |
| `stream` | bool | Включить SSE-потоковую передачу (по умолчанию: false) |
| `agent` | string | Расширение OPEX: имя целевого агента |

---

## 5. Chat SSE (Native Streaming)

| Метод | Путь | Описание |
|--------|------|----------|
| `POST` | `/api/chat` | Начать потоковую сессию чата (SSE) |
| `GET` | `/api/chat/{id}/stream` | Возобновить поток по ID потока |
| `POST` | `/api/chat/{id}/abort` | Прервать текущий поток |

### POST /api/chat

Основной эндпоинт чата. Возвращает Server-Sent Events, совместимые с **Vercel AI SDK v3** (хук `useChat`).

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `agent` | string | Да | Имя целевого агента |
| `message` | string | Да | Текст сообщения пользователя |
| `session_id` | string (UUID) | Нет | Продолжить существующую сессию |
| `channel` | string | Нет | Идентификатор исходного канала (по умолчанию: `"ui"`) |
| `user_id` | string | Нет | Идентификатор пользователя для контроля доступа |
| `attachments` | array | Нет | Файловые вложения |
| `leaf_message_id` | UUID | Нет | Возобновить с конкретного листа ветки |
| `user_message_id` | UUID | Нет | Явный ID сообщения для идемпотентности |
| `tool_policy_override` | object | Нет | Переопределение политики инструментов для этого запроса |
| `formatting_prompt` | string | Нет | Дополнительная инструкция форматирования |

**Объект вложения:**

| Поле | Тип | Описание |
|------|-----|----------|
| `url` | string | Публичный URL вложения |
| `content_type` | string | MIME-тип (например, `image/jpeg`, `audio/ogg`) |
| `filename` | string | Оригинальное имя файла |

**Типы SSE-событий:**

| Тип события | Описание | Ключевые поля |
|-------------|----------|--------------|
| `data-session-id` | Первое событие; содержит ID сессии | `{ "sessionId": "uuid" }` |
| `start` | Поток начат | `{ "session_id": "uuid", "stream_id": "uuid" }` |
| `text-start` | Начало текстового блока | `{ "id": "block-uuid" }` |
| `text-delta` | Фрагмент текста | `{ "delta": "text" }` |
| `text-end` | Текстовый блок завершён | `{}` |
| `tool-input-start` | Начало вызова инструмента | `{ "toolCallId": "id", "toolName": "search" }` |
| `tool-input-delta` | Аргументы инструмента в потоке | `{ "toolCallId": "id", "inputTextDelta": "{\"q\":" }` |
| `tool-input-available` | Полный вызов инструмента готов | `{ "toolCallId": "id", "toolName": "search", "input": {...} }` |
| `tool-output-available` | Результат инструмента готов | `{ "toolCallId": "id", "output": "..." }` |
| `rich-card` | Структурированная карточка отображения | `{ "cardType": "...", "data": {...} }` |
| `file` | Файл, созданный инструментом | `{ "url": "...", "mediaType": "audio/ogg" }` |
| `sync` | Синхронизация сообщения | `{ "content": "...", "toolCalls": [...], "status": "...", "error": null }` |
| `tool-approval-needed` | Инструмент ожидает одобрения человека | `{ "approvalId": "uuid", "toolName": "...", "args": {...} }` |
| `tool-approval-resolved` | Решение по одобрению принято | `{ "approvalId": "uuid", "decision": "approve" }` |
| `reconnecting` | Переподключение потока | `{}` |
| `usage` | Обновление использования токенов | `{ "input_tokens": 100, "output_tokens": 50 }` |
| `finish` | Поток завершён | `{ "usage": {...}, "tools_used": [...] }` |
| `error` | Ошибка при обработке | `{ "errorText": "error text" }` |

### GET /api/chat/{id}/stream

Возобновить ранее начатый поток по его `stream_id`. Возвращает тот же формат SSE.

### POST /api/chat/{id}/abort

Прервать текущий поток. Агент прекращает обработку.

**Ответ:** `{ "ok": true }` или `{ "ok": false, "error": "stream not found" }`

---

## 6. Сессии и сообщения

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/sessions` | Список сессий (требует `?agent=`) |
| `DELETE` | `/api/sessions` | Удалить все сессии (требует `?agent=` или `?channel=`) |
| `GET` | `/api/sessions/search` | Полнотекстовый поиск по сообщениям |
| `GET` | `/api/sessions/stuck` | Найти зависшие/устаревшие сессии для повтора |
| `GET` | `/api/sessions/failures` | Постраничный лог ошибок сессий |
| `GET` | `/api/sessions/{id}` | Получить метаданные сессии |
| `PATCH` | `/api/sessions/{id}` | Обновить заголовок или UI-состояние сессии (требует `?agent=`) |
| `DELETE` | `/api/sessions/{id}` | Удалить сессию и все сообщения (требует `?agent=`) |
| `POST` | `/api/sessions/{id}/invite` | Пригласить агента в мультиагентную сессию |
| `POST` | `/api/sessions/{id}/fork` | Создать ответвлённое сообщение от существующего (требует `?agent=`) |
| `GET` | `/api/sessions/{id}/chain` | Получить полную цепочку сессий (родитель + дочерние) (требует `?agent=`) |
| `POST` | `/api/sessions/{id}/retry` | Повторно запустить последнее сообщение пользователя через движок (требует `?agent=`) |
| `GET` | `/api/sessions/{id}/messages` | Список сообщений в сессии (требует `?agent=`) |
| `GET` | `/api/sessions/{id}/failures` | Записи об ошибках для одной сессии |
| `DELETE` | `/api/messages/{id}` | Удалить одно сообщение (требует `?agent=`) |
| `POST` | `/api/messages/{id}/feedback` | Установить обратную связь по сообщению (требует `?agent=`) |
| `PATCH` | `/api/messages/{id}/bookmark` | Установить/снять закладку на сообщении (требует `?agent=`) |
| `GET` | `/api/messages/bookmarked` | Список сообщений с закладками (`?agent=` или `?all=true`) |

### GET /api/sessions

| Параметр | Тип | Обязательно | Описание |
|----------|-----|-------------|----------|
| `agent` | string | Да | Фильтр по имени агента (владение, не участие) |
| `channel` | string | Нет | Фильтр по каналу (через запятую) |
| `limit` | integer | Нет | Максимум результатов (по умолчанию 20, max 100) |
| `before_last_message_at` | timestamp | Нет | Keyset-курсор: `last_message_at` последней строки предыдущей страницы |
| `before_id` | uuid | Нет | Keyset-курсор (tie-break): `id` последней строки предыдущей страницы |

Keyset-пагинация по `(last_message_at, id)` DESC: `before_last_message_at` и
`before_id` передаются вместе (только один — `400 Bad Request`). Пропуск обоих
возвращает первую страницу.

**Ответ:**
```json
{
  "sessions": [
    {
      "id": "uuid",
      "agent_id": "main",
      "user_id": "12345",
      "channel": "ui",
      "started_at": "2026-03-27T10:00:00Z",
      "last_message_at": "2026-03-27T10:05:00Z",
      "title": "Discussion about X",
      "metadata": {},
      "run_status": "idle",
      "participants": [],
      "parent_session_id": null,
      "end_reason": null
    }
  ],
  "total": 42
}
```

### DELETE /api/sessions

| Параметр | Тип | Обязательно | Описание |
|----------|-----|-------------|----------|
| `agent` | string | Условно | Удалить все сессии этого агента |
| `channel` | string | Условно | Удалить все сессии этого канала (через запятую) |

Требуется один из параметров: `agent` или `channel`.

**Ответ:** `{ "ok": true, "deleted": 5 }`

### GET /api/sessions/search

Полнотекстовый поиск по истории сообщений + секция заголовков сессий.
`plainto_tsquery('russian')` по FTS-индексу; сниппет строится через
`ts_headline` с маркерами `<b>…</b>` вокруг совпадений (клиент рендерит их как
`<mark>`, не как HTML).

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `q` | string | Обязательно | Поисковый запрос (пустой → `400`) |
| `agent` | string | — | Обязателен, если не задан `all=true` (иначе `400`) |
| `all` | bool | `false` | Искать по всем агентам (escape-hatch для Ctrl+K «по всем») |
| `limit` | integer | 30 | Максимум сообщений (max 100) |

Требуется **либо** `agent`, **либо** `all=true`. Заголовки сессий ограничены 10
результатами независимо от `limit`.

**Ответ:** (`count` = число сообщений в `messages`)
```json
{
  "messages": [
    {
      "message_id": "uuid",
      "session_id": "uuid",
      "session_title": "Discussion about X",
      "agent_id": "main",
      "snippet": "…нашёл <b>ответ</b> здесь…",
      "content": "полный текст сообщения",
      "role": "user",
      "user_id": "...",
      "channel": "ui",
      "created_at": "2026-07-16T00:00:00Z",
      "rank": 0.95
    }
  ],
  "sessions": [
    {
      "session_id": "uuid",
      "title": "Discussion about X",
      "agent_id": "main",
      "last_message_at": "2026-07-16T00:00:00Z"
    }
  ],
  "count": 3
}
```

### GET /api/messages/bookmarked

Список сообщений, отмеченных закладкой (секция «Избранное» в Ctrl+K).

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `agent` | string | — | Обязателен, если не задан `all=true` (иначе `400`) |
| `all` | bool | `false` | По всем агентам (escape-hatch, как в `/api/sessions/search`) |
| `limit` | integer | 50 | Максимум результатов (max 200) |

**Ответ:** (`preview` — усечение `content` до 160 символов)
```json
{
  "items": [
    {
      "message_id": "uuid",
      "session_id": "uuid",
      "session_title": "Discussion about X",
      "agent_id": "main",
      "preview": "первые ~160 символов сообщения…",
      "role": "assistant",
      "bookmarked_at": "2026-07-17T10:00:00Z"
    }
  ]
}
```

### PATCH /api/messages/{id}/bookmark

Установить/снять закладку. Требует `?agent=<владелец>` — JOIN через
`sessions.agent_id` защищает от межагентной записи (IDOR-guard, как у
`/api/messages/{id}/feedback`).

| Параметр | Тип | Обязательно | Описание |
|----------|-----|-------------|----------|
| `agent` | string | Да | Владелец сообщения |

**Тело:** `{ "bookmarked": true }`

**Ответ:** `204 No Content` при успехе; `404`, если сообщение не найдено или
принадлежит другому агенту.

### GET /api/sessions/stuck

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `stale_secs` | integer | 90 | Секунд без активности до признания зависшей |
| `max_retries` | integer | 3 | Порог максимального числа повторов |

**Ответ:** `{ "sessions": [{"id": "uuid", "agent_id": "main"}] }`

### GET /api/sessions/{id}

Возвращает облегчённые метаданные сессии для разрешения прямых ссылок. Параметр `agent` не требуется.

**Ответ:** `{ "id": "uuid", "agent_id": "main", "channel": "ui", "run_status": "idle" }`

### PATCH /api/sessions/{id}

**Тело запроса** (все поля опциональны):
```json
{
  "title": "New session title",
  "ui_state": { "key": "value" }
}
```

`ui_state` объединяется с метаданными сессии. Должен быть JSON-объектом до 1 КБ.

### POST /api/sessions/{id}/invite

**Тело запроса:**
```json
{ "agent_name": "Agent2" }
```

**Ответ:** `{ "participants": ["main", "Agent2"] }`

### POST /api/sessions/{id}/fork

Создаёт ответвлённое сообщение пользователя. Обеспечивает навигацию по дереву разговора.

**Тело запроса:**
```json
{
  "branch_from_message_id": "uuid",
  "content": "New user message text"
}
```

**Ответ:**
```json
{
  "message_id": "uuid",
  "parent_message_id": "uuid",
  "branch_from_message_id": "uuid"
}
```

### POST /api/sessions/{id}/retry

Повторно запускает последнее сообщение пользователя через движок в фоновой задаче. Полезно для восстановления зависших сессий.

**Ответ:** `{ "ok": true, "retry_count": 1 }` или `409` если сессия не в запущенном состоянии.

### GET /api/sessions/{id}/messages

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `limit` | integer | 50 | Максимум результатов (max 200) |
| `agent` | string | **Обязательно** | Проверка владения — запрос без него отклоняется с `400` |
| `before_id` | uuid | — | Курсор пагинации |

### POST /api/messages/{id}/feedback

**Тело запроса:**
```json
{ "feedback": 1 }
```

Значения: `1` = лайк, `-1` = дизлайк, `0` = очистить.

---

## 7. Память

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/memory` | Список/поиск фрагментов памяти |
| `POST` | `/api/memory` | Создать фрагмент памяти вручную |
| `GET` | `/api/memory/stats` | Статистика памяти |
| `GET` | `/api/memory/export` | Экспортировать всю память в JSON |
| `GET` | `/api/memory/fts-language` | Получить настройку языка FTS |
| `PUT` | `/api/memory/fts-language` | Установить язык FTS |
| `DELETE` | `/api/memory/{id}` | Удалить фрагмент памяти |
| `PATCH` | `/api/memory/{id}` | Обновить фрагмент памяти |
| `GET` | `/api/memory/tasks` | Список задач индексации памяти |
| `GET` | `/api/memory/documents` | Список исходных документов |
| `GET` | `/api/memory/documents/{id}` | Получить детали документа |
| `PATCH` | `/api/memory/documents/{id}` | Обновить метаданные документа |
| `DELETE` | `/api/memory/documents/{id}` | Удалить документ и его фрагменты |

### GET /api/memory

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `query` | string | — | Семантический/FTS поисковый запрос |
| `limit` | integer | 20 | Максимум результатов (max 100) |
| `offset` | integer | 0 | Смещение для пагинации |

Когда `query` предоставлен, выполняется гибридный семантический + FTS поиск. Без `query` возвращает постраничный список.

**Объект фрагмента памяти (результат поиска):**
```json
{
  "id": "uuid",
  "content": "User prefers concise answers",
  "source": "shared",
  "relevance_score": 0.87,
  "similarity": 0.91,
  "pinned": false
}
```

### POST /api/memory

**Тело запроса:**
```json
{
  "agent": "main",
  "content": "User's birthday is March 15",
  "pinned": true
}
```

### PUT /api/memory/fts-language

**Тело запроса:**
```json
{ "language": "russian" }
```

Допустимые значения: `simple`, `english`, `russian` и другие конфигурации полнотекстового поиска PostgreSQL.

### PATCH /api/memory/{id}

**Тело запроса** (все поля опциональны):
```json
{ "content": "Updated fact text", "pinned": true }
```

---

## 8. Инструменты и MCP

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/tool-definitions` | Список всех имён инструментов, видимых агентам (system + YAML + MCP) |
| `GET` | `/api/tools` | Список зарегистрированных HTTP-сервисов инструментов |
| `POST` | `/api/tools` | Зарегистрировать новый сервис инструментов |
| `PUT` | `/api/tools/{name}` | Обновить сервис инструментов |
| `DELETE` | `/api/tools/{name}` | Удалить сервис инструментов |
| `GET` | `/api/mcp` | Список MCP-серверов |
| `POST` | `/api/mcp` | Зарегистрировать MCP-сервер |
| `PUT` | `/api/mcp/{name}` | Обновить MCP-сервер |
| `DELETE` | `/api/mcp/{name}` | Удалить MCP-сервер |
| `POST` | `/api/mcp/{name}/reload` | Перезагрузить MCP-сервер |
| `POST` | `/api/mcp/{name}/toggle` | Включить или отключить MCP-сервер |

### GET /api/tool-definitions

Возвращает отсортированный список всех имён инструментов, доступных в системе (встроенные + YAML + MCP).

**Ответ:** `{ "tools": ["memory_search", "workspace_write", "web_search", ...] }`

---

## 9. YAML-инструменты

YAML-инструменты — это HTTP-определения инструментов, хранящиеся как `.yaml` файлы в `workspace/tools/`.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/yaml-tools` | Список всех YAML-инструментов (все статусы) |
| `POST` | `/api/yaml-tools` | Создать новый YAML-инструмент |
| `GET` | `/api/yaml-tools/{tool}` | Получить определение YAML-инструмента |
| `PUT` | `/api/yaml-tools/{tool}` | Обновить YAML-инструмент |
| `DELETE` | `/api/yaml-tools/{tool}` | Удалить YAML-инструмент |
| `POST` | `/api/yaml-tools/{tool}/verify` | Перевести инструмент в статус verified |
| `POST` | `/api/yaml-tools/{tool}/disable` | Перевести инструмент в статус disabled |
| `POST` | `/api/yaml-tools/{tool}/enable` | Повторно включить отключённый инструмент |

Псевдонимы совместимости для каждого агента:

| Метод | Путь |
|--------|------|
| `GET` | `/api/agents/{name}/yaml-tools` |
| `POST` | `/api/agents/{name}/yaml-tools/{tool}/verify` |
| `POST` | `/api/agents/{name}/yaml-tools/{tool}/disable` |

### POST /api/yaml-tools

**Тело запроса:**
```json
{ "content": "name: get_weather\ndescription: ...\nmethod: GET\nendpoint: ...\n..." }
```

Поле `content` — YAML-строка. Инструмент создаётся со статусом `verified`.

**Статусы инструментов:**

| Статус | Расположение | Описание |
|--------|-------------|----------|
| `verified` | `workspace/tools/*.yaml` | Активен, доступен агентам |
| `draft` | `workspace/tools/draft/*.yaml` | В разработке, ещё не активен |
| `disabled` | `workspace/tools/disabled/*.yaml` | Архивирован, недоступен |

**Формат YAML-инструмента:**
```yaml
name: get_weather
description: Get current weather for a location
method: GET
endpoint: https://api.example.com/weather
parameters:
  - name: location
    type: string
    description: City name
    required: true
auth:
  type: bearer_env
  key: WEATHER_API_KEY
response_transform: "$.current"
```

**Типы аутентификации:**

| type | Описание |
|------|----------|
| `bearer_env` | Читать API-ключ из env-переменной с именем `key` |
| `none` | Без аутентификации |

---

## 10. Навыки

Навыки — это Markdown-файлы в `workspace/skills/`. Общие фрагменты промптов, вводимые в контекст агента.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/skills` | Список всех навыков |
| `GET` | `/api/skills/repairs` | Список предложений по исправлению навыков |
| `PATCH` | `/api/skills/repairs/{id}` | Разрешить предложение по исправлению навыка |
| `GET` | `/api/skills/{skill}` | Получить содержимое навыка |
| `PUT` | `/api/skills/{skill}` | Создать или обновить навык |
| `DELETE` | `/api/skills/{skill}` | Удалить навык |
| `GET` | `/api/skills/{skill}/versions` | Список истории версий навыка |
| `GET` | `/api/skills/{skill}/versions/{vid}` | Получить конкретную версию |
| `POST` | `/api/skills/{skill}/versions/{vid}/restore` | Восстановить навык к предыдущей версии |
| `POST` | `/api/skills/{skill}/snapshot` | Создать снимок вручную |
| `GET` | `/api/skills/{skill}/curator-decisions` | Получить решения куратора для навыка |

Псевдонимы для каждого агента:

| Метод | Путь |
|--------|------|
| `GET` | `/api/agents/{name}/skills` |
| `GET` | `/api/agents/{name}/skills/{skill}` |
| `PUT` | `/api/agents/{name}/skills/{skill}` |
| `DELETE` | `/api/agents/{name}/skills/{skill}` |

### PUT /api/skills/{skill}

**Тело запроса:**
```json
{ "content": "# Web Search Strategy\n\nUse SearXNG for general queries..." }
```

---

## 11. Каналы

Каналы соединяют агентов с платформами обмена сообщениями (Telegram, Discord и т.д.).

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/channels` | Список всех каналов всех агентов |
| `GET` | `/api/channels/active` | Список подключённых в данный момент адаптеров каналов |
| `POST` | `/api/channels/notify` | Отправить уведомление через канал |
| `GET` | `/api/agents/{name}/channels` | Список каналов агента |
| `POST` | `/api/agents/{name}/channels` | Создать канал для агента |
| `PUT` | `/api/agents/{name}/channels/{id}` | Обновить канал |
| `DELETE` | `/api/agents/{name}/channels/{id}` | Удалить канал |
| `POST` | `/api/agents/{name}/channels/{id}/restart` | Перезапустить адаптер канала |
| `POST` | `/api/agents/{name}/channels/{id}/ack` | Подтвердить ошибку канала |
| `GET` | `/api/agents/{name}/channels/{id}/status` | Получить статус канала |
| `GET` | `/api/agents/{name}/hooks` | Получить конфигурацию хуков агента |
| `GET` | `/ws/channel/{agent_name}` | WebSocket-эндпоинт для адаптеров каналов |

### Объект канала

```json
{
  "id": "uuid",
  "agent_name": "main",
  "channel_type": "telegram",
  "display_name": "My Bot",
  "config": {},
  "status": "running",
  "error_msg": null
}
```

### POST /api/agents/{name}/channels

**Поддерживаемые типы каналов:** `telegram`, `discord`, `matrix`, `irc`, `slack`, `whatsapp`

**Тело запроса:**
```json
{
  "channel_type": "telegram",
  "display_name": "My Bot",
  "config": { "bot_token": "5092435297:AAH..." }
}
```

Поля учётных данных (`bot_token`, `access_token`, `password`, `app_token`, `verify_token`) извлекаются из `config` и хранятся в хранилище секретов. Возвращаемый `config` содержит эти поля в замаскированном виде.

**Ответ:** `{ "ok": true, "id": "uuid", "status": "stopped" }`

### POST /api/channels/notify

Отправить уведомление через канал без прохождения через LLM агента.

**Тело запроса:**
```json
{
  "channel_id": "uuid",
  "text": "Notification message",
  "parse_mode": "MarkdownV2"
}
```

### /ws/channel/{agent_name}

Адаптеры каналов подключаются через WebSocket. Аутентификация:
- Заголовок `Authorization: Bearer <token>`
- Параметр запроса `?ticket=<uuid>` (из `POST /api/auth/ws-ticket`)

---

## 12. Cron-задачи

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/cron` | Список всех cron-задач |
| `POST` | `/api/cron` | Создать cron-задачу |
| `PUT` | `/api/cron/{id}` | Обновить cron-задачу |
| `DELETE` | `/api/cron/{id}` | Удалить cron-задачу |
| `POST` | `/api/cron/{id}/run` | Немедленно запустить cron-задачу |
| `GET` | `/api/cron/{id}/runs` | Получить историю запусков задачи |
| `GET` | `/api/cron/runs` | Получить историю запусков всех задач |

### POST /api/cron

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `name` | string | Да | Уникальное имя задачи |
| `agent` | string | Да | Имя целевого агента |
| `task` | string | Да | Сообщение-задача, отправляемое агенту |
| `cron` | string | Условно | Cron-выражение (обязательно, если `run_once != true`) |
| `timezone` | string | Нет | IANA-часовой пояс (по умолчанию: `UTC`) |
| `announce_to` | string/object | Нет | Канал для отправки вывода |
| `silent` | bool | Нет | Если true, вывод агента игнорируется (по умолчанию: false) |
| `jitter_secs` | integer | Нет | Случайная задержка перед выполнением |
| `run_once` | bool | Нет | Одноразовая задача (требует `run_at`) |
| `run_at` | datetime | Условно | ISO 8601 дата/время для одноразовых задач |
| `tool_policy` | object | Нет | Переопределение политики инструментов для этой задачи |

**Объект задачи:**
```json
{
  "id": "uuid",
  "name": "morning-briefing",
  "agent": "main",
  "cron": "0 9 * * *",
  "timezone": "UTC",
  "task": "Prepare daily briefing",
  "enabled": true,
  "silent": false,
  "announce_to": "telegram",
  "jitter_secs": 0,
  "run_once": false,
  "run_at": null,
  "created_at": "2026-01-01T00:00:00Z",
  "last_run": "2026-03-27T06:00:00Z",
  "next_run": "2026-03-28T06:00:00Z",
  "tool_policy": null
}
```

---

## 13. Задачи

Обратитесь к документации задач агента через `GET /api/agents/{name}/tasks`.

---

## 14. Подтверждения

Подтверждение человеком для чувствительных вызовов инструментов. Эндпоинты подтверждений зарегистрированы в роутере `/api/agents`.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/approvals` | Список ожидающих подтверждений |
| `POST` | `/api/approvals/{id}/resolve` | Одобрить или отклонить ожидающее действие |
| `GET` | `/api/approvals/allowlist` | Список автоматически одобряемых инструментов |
| `POST` | `/api/approvals/allowlist` | Добавить инструмент в список разрешённых |
| `DELETE` | `/api/approvals/allowlist/{id}` | Удалить из списка разрешённых |

### POST /api/approvals/{id}/resolve

**Тело запроса:**
```json
{ "decision": "approve" }
```

Значения: `"approve"` или `"deny"`.

### POST /api/approvals/allowlist

**Тело запроса:**
```json
{ "tool_name": "workspace_write", "agent": "main" }
```

---

## 15. Webhooks

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/webhooks` | Список всех webhooks |
| `POST` | `/api/webhooks` | Создать webhook |
| `PUT` | `/api/webhooks/{id}` | Обновить webhook |
| `DELETE` | `/api/webhooks/{id}` | Удалить webhook |
| `POST` | `/api/webhooks/{id}/regenerate-secret` | Перегенерировать секрет webhook |
| `POST` | `/webhook/{name}` | Эндпоинт триггера (аутентификация по секрету webhook) |

### POST /api/webhooks

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `name` | string | Да | Уникальное имя webhook (используется в URL триггера) |
| `agent` | string | Да | Имя целевого агента |
| `prompt_prefix` | string | Нет | Текст, предшествующий полезной нагрузке при отправке агенту |
| `enabled` | bool | Нет | По умолчанию: `true` |
| `webhook_type` | string | Нет | `generic` (по умолчанию) или `github` |
| `event_filter` | array | Нет | GitHub webhooks: список типов событий (например, `["push", "pull_request"]`) |

**Ответ:** `201 Created` с полным объектом webhook, включая **полный секрет** (виден только при создании).

### POST /api/webhooks/{id}/regenerate-secret

**Ответ:** `{ "ok": true, "secret": "new-64-char-hex-string" }`

### POST /webhook/{name}

Эндпоинт триггера для внешних систем. **Не** за стандартным middleware аутентификации — аутентифицируется по секрету webhook.

**Методы аутентификации:**

| Тип | Метод аутентификации |
|-----|---------------------|
| `generic` | `Authorization: Bearer <secret>` |
| `github` | `X-Hub-Signature-256: sha256=<hmac>` |

**Параметры запроса:**

| Параметр | Описание |
|----------|----------|
| `async=true` | Вернуть немедленно; обработать полезную нагрузку в фоне |

**Rate limiting:** 5 ошибок аутентификации за 5 минут блокируют webhook на 10 минут.

**Ответ (sync):** `{ "ok": true, "response": "Agent response text" }`  
**Ответ (async):** `{ "ok": true, "queued": true }`

---

## 16. Секреты

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/secrets` | Список всех секретов (значения замаскированы) |
| `POST` | `/api/secrets` | Создать или обновить секрет |
| `GET` | `/api/secrets/{name}` | Получить секрет |
| `DELETE` | `/api/secrets/{name}` | Удалить секрет |

### POST /api/secrets

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `name` | string | Да | Имя секрета |
| `value` | string | Условно | Значение секрета (обязательно, если не обновляется только описание) |
| `description` | string | Нет | Человекочитаемое описание |
| `scope` | string | Нет | Имя агента для per-agent секретов; пустое для глобальных |

**Порядок разрешения:** `(name, scope)` → `(name, "")` global → переменная окружения.

### GET /api/secrets/{name}

| Параметр | Тип | Описание |
|----------|-----|----------|
| `scope` | string | Область агента (пустая для глобальной) |
| `reveal` | bool | Вернуть открытое значение (по умолчанию: false) |

---

## 17. Конфигурация

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/config` | Получить конфигурацию шлюза |
| `PUT` | `/api/config` | Обновить конфигурацию шлюза |
| `GET` | `/api/config/export` | Экспортировать полную конфигурацию в JSON |
| `POST` | `/api/config/import` | Импортировать конфигурацию из JSON |
| `GET` | `/api/config/schema` | Получить JSON Schema конфигурации шлюза |
| `POST` | `/api/restart` | Перезапустить процесс шлюза |

### GET /api/config/schema

Возвращает JSON Schema для `config/opex.toml`. Полезно для редакторов конфигурации UI и клиентской валидации.

### POST /api/restart

Сигнализирует процессу о выходе (systemd или watchdog перезапустит его).

**Ответ:** `{ "ok": true, "message": "restarting..." }`

---

## 18. Резервное копирование и восстановление

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/backup` | Список доступных резервных копий |
| `POST` | `/api/backup` | Создать новую резервную копию |
| `GET` | `/api/backup/{filename}` | Скачать файл резервной копии |
| `DELETE` | `/api/backup/{filename}` | Удалить файл резервной копии |
| `POST` | `/api/restore` | Восстановить из резервной копии |

### POST /api/backup

Создаёт полную резервную копию в директории `backups/`.

**Ответ:**
```json
{
  "ok": true,
  "filename": "opex-backup-2026-03-27T10-00-00Z.json",
  "path": "backups/opex-backup-2026-03-27T10-00-00Z.json"
}
```

### POST /api/restore

Ограничение размера тела: настраивается через `limits.max_restore_size_mb` (по умолчанию 500 МБ). Использует потоковую валидацию по частям — стандартный лимит axum 2 МБ отключён для этого эндпоинта.

**Тело запроса:**
```json
{ "filename": "opex-backup-2026-03-27T10-00-00Z.json" }
```

---

## 19. Сервисы

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/services` | Список всех управляемых сервисов (Docker + нативные процессы) |
| `POST` | `/api/services/{name}/{action}` | Выполнить действие над сервисом |
| `POST` | `/api/containers/{name}/restart` | Перезапустить Docker-контейнер (ограничено whitelist-ом) |

### POST /api/services/{name}/{action}

**Действия для Docker-сервисов:** `restart`, `rebuild`, `start`, `stop`, `status`, `logs`

**Действия для нативных управляемых процессов** (channels, toolgate): `restart`, `start`, `stop`, `status`, `logs`

`rebuild` — только для Docker. Для нативных процессов `restart` и `rebuild` оба вызывают `pm.restart()`.

**Ответ:**
```json
{ "ok": true, "action": "restart", "service": "toolgate", "managed": true }
```

### POST /api/containers/{name}/restart

Только whitelist — только несекретные контейнеры (browser-renderer, searxng, mcp-*). Контейнер PostgreSQL исключён.

---

## 20. Watchdog

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/watchdog/status` | Текущий статус watchdog (читает `/tmp/opex-watchdog.json`) |
| `GET` | `/api/watchdog/config` | Читать конфигурацию watchdog TOML |
| `PUT` | `/api/watchdog/config` | Обновить конфигурацию watchdog TOML |
| `GET` | `/api/watchdog/settings` | Читать настройки оповещений из БД |
| `PUT` | `/api/watchdog/settings` | Обновить настройки оповещений |
| `POST` | `/api/watchdog/restart/{name}` | Выполнить команду перезапуска для проверки |

Эти эндпоинты зарегистрированы в `monitoring.rs`, а не в отдельном обработчике watchdog.

### PUT /api/watchdog/config

**Тело запроса:**
```json
{ "config": "# TOML content\n[global]\n..." }
```

Конфигурация проверяется как валидный TOML перед сохранением.

### PUT /api/watchdog/settings

| Ключ | Тип | Описание |
|------|-----|----------|
| `alert_channel_ids` | array | UUID каналов для отправки оповещений |
| `alert_events` | array | Типы событий, вызывающие оповещения |

---

## 21. Провайдеры

Все провайдеры (LLM и медиа) используют эндпоинт `/api/providers`. Различаются по полю `kind` (`"text"`, `"stt"`, `"tts"`, `"vision"`, `"imagegen"`, `"embedding"`).

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/provider-types` | Список поддерживаемых типов LLM-провайдеров |
| `GET` | `/api/media-drivers` | Список доступных типов медиа-драйверов |
| `GET` | `/api/media-config` | Экспорт медиа-конфигурации, совместимой с toolgate |
| `GET` | `/api/providers` | Список настроенных провайдеров (фильтр по `?kind=`) |
| `POST` | `/api/providers` | Создать провайдера |
| `GET` | `/api/providers/{id}` | Получить провайдера |
| `PUT` | `/api/providers/{id}` | Обновить провайдера |
| `PATCH` | `/api/providers/{id}` | Обновить опции CLI для провайдера |
| `DELETE` | `/api/providers/{id}` | Удалить провайдера |
| `GET` | `/api/providers/{id}/models` | Список моделей этого провайдера |
| `GET` | `/api/providers/{id}/resolve` | Разрешить детали соединения |
| `POST` | `/api/providers/{id}/test-cli` | Протестировать CLI-провайдера |
| `GET` | `/api/provider-active` | Получить активного провайдера по capability |
| `PUT` | `/api/provider-active` | Установить активного провайдера для capability |

### POST /api/providers (LLM)

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `name` | string | Да | Человекочитаемое имя соединения (буквенно-цифровые, `-`, `_`) |
| `kind` | string | Да | `"text"` для LLM-провайдеров |
| `provider_type` | string | Да | ID типа провайдера (например, `openai`, `anthropic`, `minimax`) |
| `base_url` | string | Нет | Переопределение базового URL |
| `api_key` | string | Нет | API-ключ (хранится в vault, замаскирован в ответах) |
| `default_model` | string | Нет | Модель по умолчанию для этого соединения |
| `notes` | string | Нет | Внутренние заметки |

### Допустимые значения `kind`

| Kind | Описание |
|------|----------|
| `text` | Провайдер текстовой генерации LLM |
| `stt` | Речь в текст |
| `tts` | Текст в речь |
| `vision` | Описание изображений / визуальное понимание |
| `imagegen` | Генерация изображений |
| `embedding` | Текстовые эмбеддинги |

### PUT /api/provider-active

**Тело запроса:**
```json
{
  "stt": "whisper-local",
  "tts": "qwen3-tts-voice",
  "vision": "qwen35-local",
  "embedding": "local-embed"
}
```

Опустите capability, чтобы оставить активного провайдера для неё без изменений.

---

## 22. TTS и Canvas

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/tts/voices` | Список доступных голосов TTS |
| `POST` | `/api/tts/synthesize` | Синтезировать речь |
| `GET` | `/api/canvas/{agent}` | Получить текущее состояние canvas |
| `DELETE` | `/api/canvas/{agent}` | Очистить состояние canvas |

### POST /api/tts/synthesize

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `text` | string | Да | Текст для синтеза |
| `voice` | string | Нет | Имя голоса или идентификатор клона (например, `clone:MyVoice`) |

**Ответ:** Аудио-бинарный файл с соответствующим заголовком `Content-Type` (`audio/mpeg`, `audio/ogg` и т.д.), или JSON-ошибка.

### GET /api/canvas/{agent}

**Ответ:**
```json
{
  "visible": true,
  "agent": "main",
  "action": "present",
  "content_type": "markdown",
  "content": "# Current canvas content\n...",
  "title": null
}
```

Когда canvas пуст: `{ "visible": false }`.

---

## 23. Загрузка медиа

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `POST` | `/api/media/upload` | Required | Загрузить файл (макс. 20 МБ) |
| `POST` | `/api/media/transcribe` | Required | Транскрибировать аудио через STT (макс. 20 МБ) |
| `POST` | `/api/vision/analyze` | Required | Анализировать изображение через провайдера vision |
| `GET` | `/uploads/{filename}` | Public / Signed | Отдать загруженный файл |

### POST /api/media/upload

Загрузка multipart-формы. Сохраняет в `workspace/uploads/{uuid}.{ext}`.

**Разрешённые расширения:** `jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `ico`, `mp4`, `webm`, `mov`, `avi`, `ogg`, `oga`, `mp3`, `wav`, `flac`, `aac`, `m4a`, `pdf`, `docx`, `xlsx`, `pptx`, `txt`, `md`, `csv`, `log`, `json`, `toml`, `yaml`, `yml`, `zip`, `tar`, `gz`, `bin`. Другие расширения сохраняются как `.bin`.

**Ответ:**
```json
{ "url": "http://host:18789/uploads/uuid.jpg", "filename": "uuid.jpg", "size": 204800 }
```

### POST /api/media/transcribe

Загрузка аудио multipart. Проксирует в Toolgate `/transcribe`.

| Query-параметр | По умолчанию | Описание |
|----------------|--------------|----------|
| `lang` | `"ru"` | Подсказка языка для транскрипции |

**Поддерживаемые аудиорасширения:** `webm`, `mp4`, `ogg`, `oga`, `mp3`, `wav`, `m4a`, `aac`, `flac`.

**Ответ:** `{ "text": "<transcript>" }` или `503` если STT не настроен.

### GET /uploads/{filename}

Отдаёт загруженные файлы. Безопасность: HMAC-подпись URL настраивается через `uploads.require_signature`.

- `require_signature = true`: `403` при отсутствии `?sig=&exp=`.
- `require_signature = false` (по умолчанию): без подписи OK; если подпись присутствует, она всё равно проверяется.
- Истёкшая подпись: `410 Gone`.
- Неверная подпись: `403 Forbidden`.
- Path traversal (`..`, `/`, `\`): `400 Bad Request`.

---

## 24. Workspace

Просмотр, чтение, запись и удаление файлов в директории `workspace/`.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/workspace` | Просмотр корня workspace |
| `GET` | `/api/workspace/{*path}` | Список директории или чтение файла |
| `PUT` | `/api/workspace/{*path}` | Записать файл |
| `DELETE` | `/api/workspace/{*path}` | Удалить файл |

Все пути строго ограничены `workspace/`. Path traversal через символические ссылки отклоняется с `403 Forbidden`.

### GET /api/workspace/{*path}

Для **директорий**: возвращает JSON-список.
```json
{
  "entries": [
    { "name": "tools", "is_dir": true, "display": "tools/ (4.2 KB)" },
    { "name": "notes.md", "is_dir": false, "display": "notes.md (1.2 KB)" }
  ]
}
```

Для **файлов**: возвращает содержимое файла с соответствующим `Content-Type`.

### PUT /api/workspace/{*path}

**Тело запроса:** Содержимое файла (любой content type). Родительские директории создаются автоматически.

---

## 25. Файлы Workspace (Подписанные URL)

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `GET` | `/workspace-files/{*path}` | HMAC-подпись | Отдать артефакты workspace через подписанный URL |

HMAC-подписанный доступ к файлам workspace, созданным инструментами (`workspace_write`, `workspace_edit`, `code_exec`). Bearer-токен не требуется — безопасность обеспечивается через `?sig=<hmac>&exp=<unix_ts>`.

Полезная нагрузка подписи: `HMAC-SHA256("{path}:{exp}", upload_key)`.

Возвращает `403 Forbidden` для недействительных/истёкших подписей, `403` при выходе пути за пределы workspace.

---

## 26. OAuth

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `GET` | `/api/oauth/callback` | Public | OAuth callback (вызывается провайдером OAuth) |
| `GET` | `/api/oauth/providers` | Required | Список поддерживаемых OAuth-провайдеров |
| `GET` | `/api/oauth/accounts` | Required | Список настроенных OAuth-аккаунтов |
| `POST` | `/api/oauth/accounts` | Required | Создать OAuth-аккаунт |
| `DELETE` | `/api/oauth/accounts/{id}` | Required | Удалить OAuth-аккаунт |
| `POST` | `/api/oauth/accounts/{id}/connect` | Required | Инициировать поток авторизации OAuth |
| `POST` | `/api/oauth/accounts/{id}/revoke` | Required | Отозвать OAuth-токены |
| `GET` | `/api/agents/{name}/oauth/bindings` | Required | Список привязок OAuth агента |
| `POST` | `/api/agents/{name}/oauth/bindings` | Required | Привязать OAuth-аккаунт к агенту |
| `DELETE` | `/api/agents/{name}/oauth/bindings/{provider}` | Required | Удалить привязку OAuth |

### POST /api/oauth/accounts

**Тело запроса:**
```json
{
  "provider": "google",
  "display_name": "Work Google Account",
  "client_id": "xxx.apps.googleusercontent.com",
  "client_secret": "GOCSPX-..."
}
```

### POST /api/oauth/accounts/{id}/connect

Генерирует URL авторизации. Перенаправьте пользователя на этот URL для завершения OAuth.

**Ответ:** `{ "auth_url": "https://accounts.google.com/o/oauth2/auth?..." }`

### POST /api/agents/{name}/oauth/bindings

**Тело запроса:** `{ "account_id": "uuid" }`

---

## 27. Доступ / Сопряжение

Агенты с `access.mode: allowlist` требуют одобрения пользователей перед чатом. Процесс сопряжения использует 6-символьный код.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/access/{agent}/pending` | Список ожидающих запросов сопряжения |
| `POST` | `/api/access/{agent}/approve/{code}` | Одобрить запрос сопряжения |
| `POST` | `/api/access/{agent}/reject/{code}` | Отклонить запрос сопряжения |
| `GET` | `/api/access/{agent}/users` | Список одобренных пользователей |
| `DELETE` | `/api/access/{agent}/users/{user_id}` | Удалить пользователя из allowlist |

### Процесс сопряжения

1. Пользователь отправляет `/start` или код сопряжения боту.
2. Core создаёт ожидающую запись с 6-символьным кодом.
3. Администратор вызывает `POST /api/access/{agent}/approve/{code}`.
4. Пользователь добавляется в `allowed_users`.

### GET /api/access/{agent}/pending

**Ответ:**
```json
{
  "pending": [
    {
      "code": "ABC123",
      "user_id": "123456789",
      "channel": "telegram",
      "created_at": "2026-03-27T10:00:00Z"
    }
  ]
}
```

### GET /api/access/{agent}/users

**Ответ:**
```json
{
  "users": [
    {
      "channel_user_id": "123456789",
      "display_name": "User",
      "approved_at": "2026-01-15T12:00:00Z"
    }
  ]
}
```

---

## 28. WebSocket (UI Events)

| Путь | Авторизация | Описание |
|------|-------------|----------|
| `GET /ws` | Ticket или Bearer | Поток событий реального времени для UI |

Аутентификация:
- Параметр запроса `?ticket=<uuid>` (из `POST /api/auth/ws-ticket`)
- Заголовок `Authorization: Bearer <token>` при запросе обновления

### Типы UI-событий

| Событие | Описание |
|---------|----------|
| `agent_processing` | Агент начал/прекратил обработку |
| `session_updated` | Изменились метаданные сессии или сообщения |
| `cron_completed` | Завершена запланированная cron-задача |
| `task_updated` | Изменился статус задачи |
| `approval_pending` | Новый запрос на подтверждение инструмента |
| `approval_resolved` | Подтверждение разрешено |
| `channel_status` | Адаптер канала подключился/отключился |
| `memory_updated` | Фрагмент памяти создан или обновлён |
| `agent_joined` | Агент приглашён в мультиагентную сессию |
| `log` | Строка лога в реальном времени |

---

## 29. Email-триггеры (Gmail)

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `GET` | `/api/triggers/email` | Required | Список Gmail-триггеров |
| `POST` | `/api/triggers/email` | Required | Создать Gmail-триггер |
| `DELETE` | `/api/triggers/email/{id}` | Required | Удалить Gmail-триггер |
| `POST` | `/api/triggers/email/push` | Public | Эндпоинт push-уведомлений Gmail Pub/Sub |

### POST /api/triggers/email

**Тело запроса:**

| Поле | Тип | Обязательно | Описание |
|------|-----|-------------|----------|
| `agent` | string | Да | Имя целевого агента |
| `oauth_account_id` | string | Да | UUID подключённого Google OAuth-аккаунта |
| `label_filter` | array | Нет | ID ярлыков Gmail для фильтрации (например, `["INBOX"]`) |
| `prompt_prefix` | string | Нет | Текст, предшествующий содержимому email |

Gmail watch-подписка автоматически регистрируется в Google Pub/Sub.

### POST /api/triggers/email/push

Вызывается Google Pub/Sub при получении новой почты. Bearer-токен не требуется.

---

## 30. Интеграция с GitHub

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/agents/{name}/github/repos` | Список разрешённых GitHub-репозиториев агента |
| `POST` | `/api/agents/{name}/github/repos` | Добавить GitHub-репозиторий в allowlist |
| `DELETE` | `/api/agents/{name}/github/repos/{id}` | Удалить репозиторий из allowlist |

### POST /api/agents/{name}/github/repos

**Тело запроса:**
```json
{ "owner": "octocat", "repo": "hello-world" }
```

---

## 31. Настройка

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `GET` | `/api/setup/status` | Public | Выполнена ли первоначальная настройка |
| `GET` | `/api/setup/requirements` | Public | Чеклист предварительных требований (Docker, БД, дисковое пространство) |
| `POST` | `/api/setup/complete` | Required | Отметить настройку как выполненную (защищено — `403` после завершения) |

### GET /api/setup/status

**Ответ:**
```json
{ "needs_setup": true }
```

`needs_setup` определяется из таблицы `system_flags` (не по количеству агентов).

### GET /api/setup/requirements

Возвращает чеклист для мастера настройки. Публичный эндпоинт — токен не требуется.

**Ответ:**
```json
{
  "requirements": [
    { "name": "database", "ok": true, "message": "PostgreSQL 17 reachable" },
    { "name": "master_key", "ok": true, "message": "OPEX_MASTER_KEY set" },
    { "name": "provider", "ok": false, "message": "No LLM provider configured" },
    { "name": "agent", "ok": false, "message": "No agents created" }
  ]
}
```

### POST /api/setup/complete

Отмечает экземпляр как полностью настроенный. Защищено `setup_guard_middleware` — возвращает `403` если уже выполнено.

**Тело запроса:**
```json
{ "provider": "openai", "model": "gpt-4o-mini", "agent_name": "assistant" }
```

**Ответ:** `{ "ok": true }`

---

## 32. Сеть

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/network/addresses` | Список обнаруженных сетевых адресов (LAN, WAN, Tailscale) |

### GET /api/network/addresses

Возвращает WAN IP (с определением CGNAT), статус Tailscale, LAN-интерфейсы и mDNS-имя хоста. WAN IP кэшируется на 5 минут.

**Ответ:**
```json
{
  "wan_ip": "1.2.3.4",
  "wan_cgnat": false,
  "tailscale": { "status": "...", "ip": "100.x.x.x" },
  "interfaces": [
    { "name": "eth0", "ip": "192.168.1.85", "family": "ipv4" }
  ],
  "mdns_hostname": "opex.local",
  "port": 18789
}
```

---

## 33. Уведомления

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/notifications` | Список уведомлений |
| `PATCH` | `/api/notifications/{id}` | Отметить уведомление как прочитанное |
| `POST` | `/api/notifications/read-all` | Отметить все уведомления как прочитанные |
| `DELETE` | `/api/notifications/clear` | Удалить все прочитанные уведомления |

### GET /api/notifications

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `limit` | integer | 50 | Максимум результатов (max 200) |
| `offset` | integer | 0 | Смещение для пагинации |

**Ответ:**
```json
{
  "items": [
    {
      "id": "uuid",
      "type": "agent_error",
      "title": "Agent failed",
      "body": "Provider returned 401",
      "read": false,
      "created_at": "2026-04-06T12:00:00Z",
      "data": {}
    }
  ],
  "unread_count": 3,
  "limit": 50,
  "offset": 0
}
```

Примечание: Backend сериализует `notification_type` как `"type"` в JSON.

### PATCH /api/notifications/{id}

**Тело запроса:** `{ "read": true }`  
**Ответ:** `{ "ok": true }`

### POST /api/notifications/read-all

**Ответ:** `{ "ok": true, "updated": 5 }`

### DELETE /api/notifications/clear

Удаляет только прочитанные уведомления. Непрочитанные сохраняются.

**Ответ:** `{ "ok": true, "deleted": 12 }`

---

## 34. Куратор

Куратор — это автоматизированная система обслуживания навыков, которая проверяет, исправляет и архивирует навыки.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/curator/status` | Текущий статус куратора и информация о последнем запуске |
| `GET` | `/api/curator/config` | Получить конфигурацию куратора |
| `PUT` | `/api/curator/config` | Обновить конфигурацию куратора |
| `POST` | `/api/curator/run` | Запустить куратора вручную |
| `GET` | `/api/curator/runs` | Список истории запусков куратора |
| `GET` | `/api/curator/runs/{id}` | Получить детали запуска куратора |
| `GET` | `/api/curator-decisions/recent` | Последнее решение куратора по каждому навыку |
| `GET` | `/api/skills/{skill}/curator-decisions` | Решения куратора для конкретного навыка |

### GET /api/curator/status

**Ответ:**
```json
{
  "enabled": true,
  "cron": "0 3 * * *",
  "last_run_at": "2026-05-01T03:00:00Z",
  "last_run_id": "uuid",
  "last_phase1": 12,
  "last_phase2": 3,
  "last_phase3": 1
}
```

### GET /api/curator/config

**Ответ:**
```json
{
  "enabled": true,
  "cron": "0 3 * * *",
  "min_idle_minutes": 30,
  "stale_after_days": 90,
  "archive_after_days": 180,
  "max_repairs_per_run": 5,
  "agent_name": "main"
}
```

### GET /api/curator-decisions/recent

Возвращает последнее решение по каждому навыку в виде плоской карты.

**Ответ:**
```json
{
  "web-search": {
    "action": "keep",
    "reason": "...",
    "decided_at": "2026-05-01T03:00:00Z"
  }
}
```

---

## 35. Ошибки сессий

API только для чтения для структурированного лога ошибок сессий.

| Метод | Путь | Описание |
|--------|------|----------|
| `GET` | `/api/sessions/failures` | Постраничный список ошибок |
| `GET` | `/api/sessions/{session_id}/failures` | Записи об ошибках для одной сессии |

### GET /api/sessions/failures

| Параметр | Тип | По умолчанию | Описание |
|----------|-----|--------------|----------|
| `agent` | string | — | Фильтр по имени агента |
| `limit` | integer | 50 | Максимум результатов |
| `offset` | integer | 0 | Смещение для пагинации |

**Ответ:**
```json
{
  "failures": [
    {
      "id": "uuid",
      "session_id": "uuid",
      "agent_id": "main",
      "failed_at": "2026-04-01T12:00:00Z",
      "failure_kind": "provider_error",
      "error_message": "Provider returned 500",
      "retry_count": 2,
      "resolved": false
    }
  ],
  "total": 5
}
```

---

## 36. CSP-отчёты

| Метод | Путь | Авторизация | Описание |
|--------|------|-------------|----------|
| `POST` | `/api/csp-report` | Public | Получать отчёты о нарушениях Content Security Policy |

Эндпоинт CSP-отчётов браузера. Ограничение скорости отдельно от стандартных API-эндпоинтов. Тело ограничено 64 КБ. Отчёты агрегируются в счётчики метрик, видимые в `/api/health/dashboard`.

---

## Ошибки

Все ошибки используют единый JSON-формат:

```json
{ "error": "human-readable error message" }
```

**Стандартные HTTP-коды статуса:**

| Статус | Значение |
|--------|----------|
| `400` | Неверный запрос — отсутствующие или недействительные параметры |
| `401` | Не авторизован — отсутствующий или недействительный Bearer-токен |
| `403` | Запрещено — path traversal, несоответствие владения или feature guard |
| `404` | Не найдено |
| `409` | Конфликт — ресурс уже существует или проблема параллельного состояния |
| `410` | Устарело — истёкший подписанный URL |
| `413` | Полезная нагрузка слишком большая — файл превышает 20 МБ |
| `429` | Слишком много запросов — превышен rate limit или активна блокировка |
| `500` | Внутренняя ошибка сервера |
| `503` | Сервис недоступен — зависимость не настроена (эмбеддинги, STT и т.д.) |

---

## Примечания

### Блокировка авторизации

500 неудачных попыток авторизации (с одного IP) активируют 30-секундную блокировку для запросов без корректного заголовка `Authorization`. Loopback-адреса освобождены. Аутентифицированные запросы освобождены от rate limiting.

### Политика повторных попыток LLM

Неудавшиеся вызовы LLM повторяются до 3 раз с экспоненциальным откатом. Повторы запускаются при HTTP `429`, `500`, `502`, `503`.

### Порядок разрешения секретов

Для любого имени секрета и области агента: `(name, scope)` → `(name, "")` global → переменная окружения.

### SSE Backpressure

Эндпоинт Chat SSE использует ограниченные каналы (256/512 событий) с backpressure. Переполненные события отбрасываются и подсчитываются в `/api/health/dashboard` под `sse_events_dropped_total`.

### CORS

CORS origins настраивается через `gateway.cors_origins`. Если пусто, разрешает UI-порт (`:5173`) и API-порт на том же хосте.

### Ветвление сессий

`parent_message_id` и `branch_from_message_id` в сообщениях обеспечивают навигацию по дереву разговора. `POST /api/sessions/{id}/fork` создаёт новую ветку; активный путь через дерево резолвится на клиенте (`ui/src/stores/chat-history.ts::resolveActivePath`), а не через отдельный эндпоинт.
