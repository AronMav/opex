# OPEX — Примечания по обновлению

> Актуальную версию смотрите на [странице релизов](https://github.com/AronMav/opex/releases) —
> здесь номер не дублируется, чтобы не устаревал.
> Миграции БД применяются автоматически при старте core; `update.sh` сохраняет
> `.env`, `config/`, `workspace/` и базу данных. Разделы ниже описывают только
> обновления, требовавшие ручных действий.

## Обновление до v0.33+: File Scenarios (FSE) → файл-обработчики

В v0.33.0 система file-сценариев (FSE) удалена. Замена — самоописывающиеся
Python-обработчики в toolgate: встроенные (`toolgate/handlers/builtin/*.py`) и
пользовательские (`workspace/file_handlers/*.py`, hot-reload без перезапуска).

- **Действий не требуется**, если вы не создавали кастомных file-сценариев.
- Кастомные сценарии нужно переписать как обработчики: XML-дескриптор в комментарии +
  `async def run(ctx, file, params)`. См. навык `file-handler-guide` и вкладку
  File Handlers в UI.
- Таблицы `file_scenarios` (m069) и `video_jobs` (m068) помечены устаревшими,
  но **не удаляются** — история сохраняется. Видео-конспекты теперь выполняет
  async-обработчик `summarize_video` через очередь `handler_jobs`.
- Начиная с v0.34.0 Docker-контейнер `mcp-youtube-transcript` не используется —
  его можно удалить.

## Обновление до v0.20+: конфиг toolgate → единый источник истины Core API

**Ломающее изменение:** toolgate больше не читает следующие переменные окружения.
Создайте эквивалентные провайдеры через admin UI (или `POST /api/providers`)
**до** перезапуска opex-core, иначе toolgate стартует в **деградированном режиме**
и capability-эндпоинты будут возвращать 503 до настройки провайдеров.

### Удалённые переменные окружения

| Устаревшая env-переменная | Замена (в реестре провайдеров Core) |
| --- | --- |
| `WHISPER_URL`, `OLLAMA_API_KEY` (для STT) | Создать провайдер с `type=stt`, `driver=whisper-local`, `base_url=<ваш URL Whisper>` |
| `VISION_URL`, `VISION_MODEL`, `OLLAMA_API_KEY` | Создать провайдер с `type=vision`, `driver=ollama`, `base_url=<URL vision>`, `default_model=<модель>` |
| `TTS_BACKEND_URL` | Создать провайдер с `type=tts`, `driver=qwen3-tts`, `base_url=<ваш URL Qwen3-TTS>` |
| `MINIMAX_API_KEY` (normalize LLM) | Создать провайдер с `type=text`, `provider_type=openai-compatible`, `base_url=<URL MiniMax>`, `api_key=<ключ>`; затем указать его UUID в `options.normalize_provider_id` TTS-провайдера |

### Проверка миграции

1. **До обновления:** на текущем сервере выведите список env-переменных:

   ```bash
   systemctl --user show-environment | grep -E 'WHISPER|VISION|OLLAMA|TTS_BACKEND|MINIMAX'
   ```

2. **Для каждой перечисленной переменной:** создайте эквивалентный провайдер через UI (Settings → Media Providers → Add Provider).
3. **Для случая MINIMAX normalize:** запишите UUID нового `text`-провайдера. В редакторе TTS-провайдера установите `options.normalize_provider_id = "<этот UUID>"` и `options.normalize = true`.
4. **Обновление:** `./update.sh opex-v<VERSION>.tar.gz`
5. **Проверка:**

   ```bash
   curl -s http://localhost:9011/health | jq .
   ```

   Ожидаемый результат: `"degraded": false`, все используемые capabilities — `true` в карте `capabilities`.

### Откат

Если провайдеры не были созданы заранее, можно:

1. Откатиться к предыдущему бинарнику (`~/opex/opex-core-aarch64.bak`, если сохранён)
2. **или** создать провайдеры ретроспективно через UI — toolgate автоматически перезагрузится при первом соответствующем `PUT /api/providers/{id}`.

### Архитектурное обоснование (config SoT)

Полный контекст проектного решения (деградированный режим, вложенный `normalize_provider_id` и т.д.) см. в `docs/superpowers/specs/2026-04-18-toolgate-config-sot-design.md`.

## v0.20.x → v0.20.next — рефакторинг примитивов toolgate

Роутеры `email`, `calendar` и `bcs_portfolio` в toolgate заменены примитивными эндпоинтами:

| Старый эндпоинт | Новый примитивный эндпоинт |
| --- | --- |
| `POST /email/send` | `POST /primitives/smtp/send` |
| `GET /email/inbox` | `POST /primitives/imap/fetch` |
| `GET /email/search` | `POST /primitives/imap/search` |
| `GET /calendar/today` | `POST /primitives/google_calendar/events/list` |
| `GET /calendar/upcoming` | `POST /primitives/google_calendar/events/list` |
| `POST /calendar/create` | `POST /primitives/google_calendar/events/create` |
| `GET /bcs/portfolio` | `POST /primitives/bcs/portfolio` |

Все учётные данные теперь проходят через хранилище секретов Core. До обновления добавьте следующие секреты через `POST /api/secrets` (замените значения своими).

> Команды curl ниже содержат пароли в открытом виде. Запускайте их из оболочки
> с отключённой историей (`set +o history` в bash, `setopt no_hist_save` в zsh)
> или очистите историю оболочки после.

### Секреты для добавления

```bash
# SMTP + IMAP — необходимы для работы инструментов email_*.
# Порты жёстко заданы как стандартные (587 для SMTP submission, 993 для IMAPS)
# в телах YAML; если нужны нестандартные порты, редактируйте
# workspace/tools/email_{send,check,search}.yaml напрямую.
curl -sSf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"SMTP_HOST","scope":"","value":"smtp.gmail.com"}' http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"IMAP_HOST","scope":"","value":"imap.gmail.com"}' http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"EMAIL_USER","scope":"","value":"you@gmail.com"}' http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"EMAIL_PASS","scope":"","value":"YOUR_APP_PASSWORD"}' http://localhost:18789/api/secrets

# Google Calendar — вставьте весь JSON сервисного аккаунта как одну строку
GSA_JSON=$(cat /path/to/service-account.json | jq -c .)
curl -sSf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d "{\"name\":\"GOOGLE_SA_KEY_JSON\",\"scope\":\"\",\"value\":$(echo "$GSA_JSON" | jq -Rs .)}" \
  http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"GOOGLE_CALENDAR_ID","scope":"","value":"primary"}' http://localhost:18789/api/secrets

# BCS — без изменений, указан для полноты
# (BCS_REFRESH_TOKEN должен уже быть в хранилище)
```

### Что изменилось

Инструменты, которые ранее полагались на env-переменные с учётными данными (`EMAIL_USER`, `EMAIL_PASS`,
`GOOGLE_SA_KEY` как путь к файлу), теперь работают без этих env-переменных;
прежний путь на основе env не функционировал ни в каком стандартном развёртывании
и полностью удалён.

### Зависимости

Toolgate теперь требует `google-api-python-client` и `google-auth`.
Запустите `pip install -r toolgate/requirements.txt` на целевом сервере (или воспользуйтесь
`make deploy` для синхронизации).

### Примечание о поведении календаря

`calendar_today` и `calendar_upcoming` теперь всегда запрашивают окно «следующие 7 дней
с текущего момента» (ранее была неявная выравнивание по началу дня). Параметр `days`
в `calendar_upcoming` принимается, но игнорируется — создайте issue, если вам нужно
старое поведение.

### Архитектурное обоснование (примитивы)

Полный контекст проектного решения (примитивы vs роутеры интеграций, шаблонизация `${VAR}` в
`body_template`, выделение состояния BCS и т.д.) см. в `docs/superpowers/specs/2026-04-19-toolgate-primitives-design.md`.

### Изменение формы ответа calendar_create

Форма ответа для агентов в `calendar_create` изменилась (это ломающее изменение для
промптов агентов, которые ссылались на конкретные поля):

- **Раньше**: `{status, id, link, summary, start, end}` (напрямую из GET-роутера)
- **Теперь**: `{id, summary, html_link}` (извлекается через `response_transform: $.event`)

Переименования полей: `link` → `html_link`. Удалены: `status`, `start`, `end`, дублированный
`summary`. Если ваши промпты агентов или навыки читают эти поля, обновите их перед обновлением.

### Известные ограничения (последующие задачи отслеживаются)

- **Классификация ошибок BCS refresh token**: плохой или истёкший `BCS_REFRESH_TOKEN`
  теперь проявляется как 401 из `/primitives/bcs/portfolio` — агенты должны обнаруживать
  это и предлагать ротацию токена.
- **Пробел в сквозной автоматизации**: нет автоматизированного теста, который загружает реальный
  `workspace/tools/*.yaml`, прогоняет его через YAML-runtime Core и обращается к мокированному
  примитиву. Ручной curl-smoke-тест в Task 11 плана реализации покрывает happy paths.
  Будущий harness интеграционных тестов закроет этот пробел (отслеживается как I3 в плане).
- **Опциональные параметры `body_template` без явных значений**: если LLM опускает
  необязательный параметр (например, `html` в `email_send`), его плейсхолдер
  `{{html}}` не заменяется значением по умолчанию — тело запроса становится
  невалидным JSON и вызов завершается ошибкой. Это унаследованное поведение Core (не привнесённое
  рефакторингом примитивов), но теперь затрагивает больше инструментов. Агенты должны
  всегда передавать опциональные параметры явно до момента, когда Core начнёт материализовывать
  дефолты в карту подстановок.
