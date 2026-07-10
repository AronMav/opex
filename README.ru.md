<h1 align="center">
  <img src="docs/assets/opex-banner.png" alt="OPEX — самостоятельно развёртываемый AI-шлюз, в котором заменяемо всё" width="820">
</h1>

<p align="center">
  <em>Произносится «ОРЕХ»</em>
</p>

<p align="center">
  <a href="https://github.com/AronMav/opex/actions/workflows/ci.yml?branch=master"><img src="https://img.shields.io/github/actions/workflow/status/AronMav/opex/ci.yml?branch=master&style=for-the-badge" alt="CI"></a>
  <a href="https://github.com/AronMav/opex/releases"><img src="https://img.shields.io/github/v/release/AronMav/opex?style=for-the-badge" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="MIT"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust&logoColor=white&style=for-the-badge" alt="Rust"></a>
  <a href="https://github.com/AronMav/opex/releases"><img src="https://img.shields.io/badge/platform-ARM64%20%7C%20x86__64-blue?logo=linux&logoColor=white&style=for-the-badge" alt="Platform"></a>
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="docs/">Документация</a> ·
  <a href="docs/ARCHITECTURE.md">Архитектура</a> ·
  <a href="docs/API.md">API</a> ·
  <a href="SECURITY.md">Безопасность</a>
</p>

**OPEX — самостоятельно развёртываемый AI-шлюз на Rust, построенный вокруг одной идеи: каждый слой заменяем без изменения ядра.** Поведение агентов живёт в Markdown. Инструменты — YAML-файлы. Провайдер меняется одной строкой. Каналы — отдельный процесс. Один бинарник обслуживает HTTP API, жизненный цикл агентов, LLM-вызовы, инструменты, каналы, память и секреты — на домашнем сервере, ARM64 или x86_64, без облачной привязки. Разговаривайте с ним из Telegram, пока он работает на удалённой машине.

Используйте любую модель — **150+ провайдеров из встроенного каталога в один клик**, любой OpenAI-совместимый эндпоинт, локальная Ollama/vLLM. Контекстные окна **5000+ моделей** подставляются автоматически. Переключение — одна строка в TOML, без кода и без vendor-lock.

<table>
<tr><td><b>Всё заменяемо, ничего не зашито</b></td><td>Личность и память агента — Markdown-файлы. Инструменты — YAML. Навыки — Markdown, загружаемые в runtime. Провайдеры — единый реестр, редактируемый из UI. Каналы — отдельный процесс за границей протокола. Меняете файл — меняете поведение, без перезапуска.</td></tr>
<tr><td><b>Встроенный каталог моделей</b></td><td>Контекстные окна, лимиты вывода, цены и возможности 5000+ моделей из <a href="https://models.dev">models.dev</a> + OpenRouter, с фоновым обновлением. Автоопределение окна для любой модели, добавление 150+ провайдеров пресетом (URL/тип/модели заполняются сами), $-учёт по реальным ценам, гейтинг параметров по возможностям модели.</td></tr>
<tr><td><b>Живёт там, где вы</b></td><td>Telegram, Discord, Matrix, IRC, Slack — из одного gateway-процесса. Транскрипция голосовых, обработка медиа, непрерывность разговора между платформами.</td></tr>
<tr><td><b>Мультиагентная оркестрация</b></td><td>Агенты работают в общих сессиях с маршрутизацией через @-упоминания. Пулы session-scoped агентов с жизненным циклом run / async / message / status / kill — параллельные рабочие потоки без общего состояния.</td></tr>
<tr><td><b>Долгосрочная память</b></td><td>PostgreSQL + pgvector, гибридный поиск (семантика + FTS) с MMR-ранжированием. Два уровня: сырой с временным затуханием и закреплённый постоянный. Ключевые факты извлекаются в память при сжатии контекста.</td></tr>
<tr><td><b>Планирование и автоматизации</b></td><td>Cron-планировщик уровня агента с часовыми поясами и джиттером. Ежедневные отчёты, ночные бэкапы, аудиты — на естественном языке, без присмотра, с доставкой в любой канал.</td></tr>
<tr><td><b>Расширяемость по стандартам</b></td><td>Любой MCP-сервер — как on-demand Docker-контейнер, инструменты автообнаруживаются. Файл-обработчики (STT / Vision / TTS / ImageGen / видео) — self-describing Python-плагины с hot-reload. LSP-интеллект (pyright) для агентов.</td></tr>
</table>

---

## Установка

```bash
tar xzf opex-v<VERSION>.tar.gz
cd opex
./setup.sh
```

Установщик настраивает Docker, Bun, Python 3, PostgreSQL, генерирует `.env`, создаёт systemd-сервисы. После завершения откройте `http://your-server:18789`.

Сборка из исходников: клонируйте репозиторий и запустите `./setup.sh` — он обнаружит недостающие toolchain и скомпилирует. Требуется Rust 1.85+ (edition 2024), Node.js 22+, Docker, Bun 1.x, Python 3.

---

## Заменяемые слои

OPEX организован в независимые слои — каждый меняется, расширяется или заменяется, не затрагивая остальные.

**Поведение агентов — TOML + Markdown.** Агент — это TOML-конфиг и папка Markdown-файлов в `workspace/agents/{Name}/`. Личность, память, тон, фоновые задачи — простой текст. Правка файла = новое поведение, без перезапуска.

**Инструменты — YAML.** Положите YAML в `workspace/tools/` — инструмент доступен сразу. Инъекция авторизации (Bearer / API key / заголовок), JSONPath-трансформации ответа, бинарные ответы (фото, голос), SSRF-защита. Без кода.

**Навыки — Markdown по требованию.** Поведенческие инструкции, внедряемые во время инференса, а не зашитые в системный промпт. Агент обнаруживает их и подгружает по совпадению триггеров. Добавили файл — навык появился; удалили — исчез.

**Провайдеры — единый реестр + каталог.** Все LLM- и медиасервисы (STT, TTS, Vision, ImageGen, Embedding) проходят через реестр, редактируемый из Web UI или API. Добавление провайдера — выбор из 150+ пресетов каталога (URL, тип и список моделей заполняются автоматически). Любой OpenAI-совместимый эндпоинт работает сразу.

**Каналы — отдельный процесс.** Адаптеры Telegram / Discord / Matrix / IRC / Slack — TypeScript/Bun-субпроцесс. Ядро не знает протоколов обмена сообщениями: адаптеры шлют `IncomingMessage` через внутренний WebSocket. Новый адаптер — без правок в Rust.

---

## Что меняется без перезапуска

| Слой | Вступает в силу |
| --- | --- |
| SOUL.md / IDENTITY.md | Следующее сообщение |
| Файлы навыков | Следующее сообщение |
| YAML-инструменты | Следующий запрос (кэш 30с) |
| TOML-конфиг агента | Hot-reload (file watcher) |
| Настройки провайдеров | Немедленно через API |
| Каталог моделей | Фоновое обновление (по умолчанию 24ч) |
| Конфигурация канала | При переподключении адаптера |

---

## Каталог моделей

OPEX подтягивает метаданные моделей из внешних агрегаторов и делает их единым источником правды — вместо захардкоженных таблиц.

- **Автоопределение контекстного окна.** Цепочка резолвинга: ручной override → нативный self-report провайдера (`/api/show`, `/v1/models`, `inputTokenLimit`) → **каталог** (models.dev ∪ OpenRouter) → эвристика по имени. 5000+ моделей резолвятся точно; локальные и кастомные — через нативный пробинг.
- **150+ провайдеров в один клик.** Пикер в «добавить провайдера» подставляет base_url, тип и список моделей. Большинство OpenAI-совместимы → добавляются как `openai_compat` без нового кода.
- **$-учёт.** `/api/usage` считает стоимость по реальным ценам каталога, а не по крошечной встроенной таблице.
- **Возможности модели.** `max_tokens` зажимается по лимиту вывода; `temperature` не отправляется моделям, которые его не принимают (o1/reasoning).

---

## Архитектура

Три Rust-бинарника + два управляемых дочерних процесса + Docker-инфраструктура.

```text
opex-core       — HTTP API, жизненный цикл агентов, LLM-вызовы, диспетчер
  │               инструментов, память, секреты, планировщик, каталог моделей
  ├── channels/ — адаптеры чатов (TypeScript/Bun, управляемый процесс)
  └── toolgate/ — медиа-хаб: STT, TTS, Vision, ImageGen, Embeddings
                  (Python/FastAPI, управляемый процесс)

opex-watchdog        — внешний монитор здоровья с оповещением через каналы
opex-memory-worker   — фоновая переиндексация через очередь задач PostgreSQL

PostgreSQL 17 + pgvector — сессии, сообщения, память, cron, секреты, usage
SearXNG                  — мета-поисковик для веб-поиска
browser-renderer         — headless-браузер для автоматизации
MCP-серверы              — on-demand через Docker API
code sandbox             — изолированные контейнеры для кода non-base агентов
```

Rust-ядро не знает ни одного протокола обмена сообщениями и не содержит встроенного SDK провайдеров. Каждая внешняя поверхность — каналы, медиасервисы, LLM-бэкенды, MCP-инструменты — подключена через определённую границу протокола. Именно это делает слои заменяемыми. Подробнее — [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

---

## Безопасность

- **Изоляция рабочего пространства** — канонизация путей и разрешение симлинков; агент не выйдет за свою директорию.
- **SSRF-защита** — блокировка приватных IP на уровне DNS (RFC 1918, link-local, CGNAT, Teredo, 6to4, IPv4-mapped) для исходящих запросов YAML-инструментов; блок-лист внутренних сервисов.
- **Sandbox** — non-base агенты выполняют код в изолированных Docker-контейнерах; base-агенты — на хосте с явным разрешением.
- **Подтверждение инструментов** — human-in-the-loop per-tool; состояние в PostgreSQL.
- **Секреты** — ChaCha20Poly1305, область per-agent, fallback на env; учётные данные не касаются конфигов.
- **Скрытие PII** и **обнаружение prompt injection** — фильтрация ключей/токенов в выводе кода; внешний контент обёрнут в маркеры границ.

> [!IMPORTANT]
> Сохраните резервную копию `OPEX_MASTER_KEY` — он расшифровывает хранилище и не восстанавливается при потере.

---

## Конфигурация

Три переменные в `.env`; всё остальное — в зашифрованном хранилище.

```bash
OPEX_AUTH_TOKEN=...   # аутентификация API
OPEX_MASTER_KEY=...   # ключ хранилища ChaCha20Poly1305
DATABASE_URL=...      # строка подключения PostgreSQL
```

Конфиг агента — `config/agents/{Name}.toml`, hot-reload при изменении:

```toml
[agent]
name = "Assistant"
language = "ru"
provider = "openai"
model = "gpt-4o-mini"
temperature = 0.7

[agent.tool_loop]
max_iterations = 50
detect_loops = true
```

---

## Разработка

```bash
make check           # cargo check --all-targets
make test            # cargo test (без БД пропускает sqlx::test)
make lint            # cargo clippy --all-targets -- -D warnings
make remote-deploy   # сборка на сервере → atomic swap + restart
make doctor          # GET /api/doctor
make logs            # journalctl --user -u opex-core -f
```

```text
opex/
├── crates/
│   ├── opex-core/          # Основной бинарник
│   ├── opex-watchdog/      # Монитор здоровья
│   ├── opex-memory-worker/ # Фоновые задачи
│   └── opex-types/         # Общие типы
├── channels/               # Адаптеры каналов (TypeScript/Bun)
├── toolgate/               # Медиа-хаб (Python/FastAPI)
├── ui/                     # Web UI (Next.js 16)
├── workspace/              # Runtime: tools/, skills/, agents/
├── config/                 # Конфиги агентов и системы (TOML)
├── migrations/             # Миграции PostgreSQL (авто при старте)
└── docker/                 # Compose + Dockerfile
```

---

## Обновление

```bash
~/opex/update.sh opex-v<VERSION>.tar.gz
```

Сохраняет `.env`, `config/`, `workspace/` и базу. После — проверьте `GET /api/doctor`.

---

## Лицензия

MIT — см. [LICENSE](LICENSE).
