<h1 align="center">
  <img src="docs/assets/opex-banner.png" alt="OPEX — самостоятельно развёртываемый AI-шлюз, построенный как инфраструктура" width="820">
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

**OPEX — самостоятельно развёртываемый шлюз AI-агентов, построенный как инфраструктура, а не как чат-приложение.** Один Rust-бинарник размером ~14 МБ обслуживает HTTP API, жизненный цикл агентов, LLM-вызовы, инструменты, память, планировщик и секреты на любой Linux-машине — x86_64 или ARM64, вплоть до платы класса Raspberry Pi. Агенты живут в Telegram, Discord, Slack, Matrix, IRC, WhatsApp и почте, пока работают на вашем сервере — с шифрованным хранилищем секретов, SSRF-защитой инструментов, песочницей для кода и watchdog'ом, который напишет вам, если что-то сломалось.

Всё, что выше ядра, — файлы, которые можно править: личность агента — Markdown, инструмент — десять строк YAML, навык — Markdown-заметка. Изменили файл — изменилось поведение. Без пересборки и перезапуска.

---

## Установка

```bash
tar xzf opex-v<VERSION>.tar.gz
cd opex
./setup.sh
```

Установщик настраивает Docker, Bun, Python 3, PostgreSQL, генерирует `.env` и создаёт systemd-сервисы. После завершения откройте `http://your-server:18789` — дальше проведёт мастер из 4 шагов.

Сборка из исходников: клонируйте репозиторий и запустите `./setup.sh` — он обнаружит недостающие toolchain и скомпилирует. Требуется Rust (toolchain закреплён в `rust-toolchain.toml`), Node.js 22+, Docker, Bun 1.x, Python 3.

Обновление — одна команда: `~/opex/update.sh opex-v<VERSION>.tar.gz` — с сохранением `.env`, `config/`, `workspace/` и базы данных.

---

## Чем OPEX действительно отличается

Self-hosted, мульти-провайдер, web UI, RAG, голос, генерация изображений, MCP — это есть у каждого проекта в нише, и у OPEX тоже. Ниже — то, чего вы *не* найдёте у других:

<table>
<tr><td><b>Один маленький бинарник</b></td><td>Ядро — один Rust-бинарник ~14 МБ (только rustls, без OpenSSL), без Node и Python на горячем пути. Три Rust-сервиса — ядро, watchdog, memory worker — плюс PostgreSQL 17 + pgvector; адаптеры каналов (Bun) и медиа-хаб (Python) работают как управляемые дочерние процессы, Docker хостит песочницу и MCP-контейнеры.</td></tr>
<tr><td><b>Безопасность включена по умолчанию</b></td><td>Не плагины и не флаги конфига — всё в коробке и включено: vault с аутентифицированным шифрованием (ChaCha20-Poly1305), вычищающий креды из конфигов; SSRF-блокировка на уровне DNS-резолвера (иммунна к DNS-rebinding — это не проверка строки URL); PII-редакция по умолчанию — редактирует, а не блокирует; provenance-маркировка недоверенного контента; Docker-песочница; deny-first политика инструментов. Полная модель угроз — ниже.</td></tr>
<tr><td><b>Эксплуатация из коробки</b></td><td>Отдельный watchdog-бинарник напишет вам в ваш же чат-канал, если агент завис или процесс упал. Самовосстанавливающийся супервизор процессов, <code>/api/doctor</code> с 15 проверками здоровья (включая сканирование workspace на утёкшие креды), бэкапы <code>pg_dump</code> в один клик.</td></tr>
<tr><td><b>Каталог моделей, который не устаревает</b></td><td>Показывать стоимость умеют многие — но цифры обычно берутся из зашитого в репозиторий прайс-файла, который отстаёт от реальности. OPEX определяет контекстные окна, цены и возможности тысяч моделей из живого внешнего каталога (models.dev ∪ OpenRouter): $-учёт по сессиям с cache- и reasoning-токенами, пресеты провайдеров с автозаполнением URL / типа / списка моделей, параметры запросов гейтятся по фактическим возможностям модели.</td></tr>
<tr><td><b>Память, которую можно прочитать</b></td><td>Долгосрочная память агента — Markdown-файлы: редактируются руками, дружат с git, являются источником истины. Редкость — индекс за ними: pgvector + полнотекст + триграммы в одном PostgreSQL-запросе с MMR-ранжированием, автоматическая синхронизация из файлов, два уровня — сырой с временным затуханием и закреплённый постоянный.</td></tr>
<tr><td><b>Всё — файл</b></td><td>Личности и навыки — Markdown, инструменты — YAML (авторизация из vault, JSONPath-трансформации, импорт из OpenAPI), всё с hot-reload. Встроенный Куратор по расписанию архивирует устаревшие навыки и чинит сломанные.</td></tr>
<tr><td><b>Работает без присмотра</b></td><td>Cron с часовыми поясами и джиттером и heartbeat'ы агентов — это база; отличие — goal loop: отдельный проход LLM-судьи проверяет, достигнута ли поставленная цель на самом деле, а не «модель перестала вызывать инструменты». Результаты уходят в любой канал; человек остаётся в контуре: подтверждения (обратный отсчёт, редактируемые аргументы) и инструмент <code>clarify</code> для вопросов посреди выполнения.</td></tr>
<tr><td><b>Флот, а не бот</b></td><td>Агенты в общей сессии — всегда живые пиры: вы (или другой агент) можете писать им, опрашивать и останавливать посреди разговора (ask / status / kill), а не запускать одноразовые сабагентские прогоны. Маршрутизация @-упоминаниями, жёсткий denylist инструментов для субагентов без рекурсивного спавна, shadow-git-чекпоинты рабочих папок с <code>/rollback</code>.</td></tr>
</table>

---

## Что поддерживается

| Поверхность | Что работает |
| --- | --- |
| **Каналы** | Telegram, Discord, Slack, Matrix, IRC, WhatsApp, Email — один процесс-адаптер, нативное меню команд в Telegram, коды сопряжения и allowlist'ы, голосовые режимы по каналам |
| **LLM-бэкенды** | 30 типов провайдеров, пресеты из каталога в один клик, любой OpenAI-совместимый эндпоинт, локальные Ollama / vLLM, Claude CLI и Gemini OAuth как бэкенды; правила маршрутизации по агентам с failover |
| **Медиа** | STT ×9 провайдеров, TTS ×8, Vision ×8, ImageGen ×5, эмбеддинги, веб-поиск (Brave / SearXNG / Ollama) — всё за единым подключаемым реестром |
| **Стандарты** | MCP-серверы как on-demand Docker-контейнеры, импорт инструментов из OpenAPI, OpenAI-совместимый `/v1` API, LSP-интеллект (pyright) для агентов |

---

## Модель безопасности

Агент с инструментами — это поверхность атаки. В OPEX это входное условие проектирования, а не запоздалая мысль:

| Угроза | Защита |
| --- | --- |
| Утечка кредов через конфиги или дампы БД | Vault на ChaCha20-Poly1305; токены каналов автоматически извлекаются из конфигов и никогда не хранятся открытым текстом; в `.env` ровно 3 ключа |
| Запросы агента достают до вашей LAN (SSRF) | Блокировка приватных IP на уровне DNS — сначала резолв, потом фильтр (RFC 1918, link-local, CGNAT, IPv6 ULA, Teredo, 6to4), что закрывает DNS-rebinding |
| Выполнение недоверенного кода | Docker-песочница для не-base агентов; deny-first политика инструментов проверяется раньше любого allowlist; субагенты наследуют жёсткий denylist |
| Prompt-инъекция через файлы и вывод инструментов | Внешний контент оборачивается в provenance-маркеры `<file_output trust="untrusted">`; PII (телефоны, почта, карты, API-ключи) редактируется до вызова LLM |
| Разрушительный вызов инструмента | Подтверждение человеком по каждому инструменту — с обратным отсчётом и редактируемыми аргументами; журнал аудита; shadow-git-чекпоинт + `/rollback` |
| Перебор и хотлинк | Rate limiting с блокировкой перебора токена; HMAC-подписанные истекающие URL для каждого файла |
| Побег из workspace | Канонизация путей и разрешение симлинков — агент не выйдет за пределы своей директории |

> [!IMPORTANT]
> Сделайте резервную копию `OPEX_MASTER_KEY` — он расшифровывает vault и не восстанавливается при утере.

---

## Всё — файл

Новый HTTP-инструмент — это один YAML-файл в `workspace/tools/`, доступный со следующего запроса, без кода и перезапуска:

```yaml
name: get_weather
description: "Текущая погода по координатам (Open-Meteo)"
endpoint: "https://api.open-meteo.com/v1/forecast"
method: GET
parameters:
  latitude:  { type: number, required: true, location: query }
  longitude: { type: number, required: true, location: query }
response_transform: "$.current"
```

Авторизация берётся из vault (`bearer_env`, API-ключ, header, OAuth refresh — 8 режимов), ответы обрезаются JSONPath'ом, бинарные результаты уходят прямо в чат-канал как фото или голосовые. Есть OpenAPI-спека? Импортируйте — каждая операция станет черновиком инструмента.

Тот же принцип везде:

| Слой | Формат | Вступает в силу |
| --- | --- | --- |
| Личность, память, навыки | Markdown | Со следующего сообщения |
| Инструменты | YAML | Со следующего запроса |
| Конфиг агента | TOML | Hot-reload (file watcher) |
| Провайдеры, модели | Реестр в UI/API | Немедленно |
| Файл-обработчики | Python-плагины в toolgate | Hot-reload |
| Каналы | Отдельный Bun-процесс | При переподключении адаптера |

---

## Архитектура

```text
opex-core       — HTTP API, жизненный цикл агентов, LLM-вызовы, диспетчер
  │               инструментов, память, секреты, планировщик, каталог моделей
  ├── channels/ — чат-адаптеры (TypeScript/Bun, управляемый процесс)
  └── toolgate/ — медиа-хаб: STT, TTS, Vision, ImageGen, эмбеддинги
                  (Python/FastAPI, управляемый процесс)

opex-watchdog        — внешний монитор здоровья с алертами в каналы
opex-memory-worker   — фоновая переиндексация эмбеддингов через очередь в PostgreSQL

PostgreSQL 17 + pgvector — сессии, сообщения, память, cron, секреты, usage
MCP-серверы / песочница  — Docker-контейнеры по требованию
```

Rust-ядро не знает ни одного протокола мессенджеров и не содержит встроенных SDK провайдеров. Каждая внешняя поверхность — каналы, медиа-сервисы, LLM-бэкенды, MCP-инструменты — отделена определённой протокольной границей. Именно это делает каждый слой заменяемым без изменения ядра. См. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

---

## Конфигурация

Три переменные в `.env`; всё остальное живёт в шифрованном vault или TOML:

```bash
OPEX_AUTH_TOKEN=...   # аутентификация API
OPEX_MASTER_KEY=...   # ключ vault (ChaCha20-Poly1305)
DATABASE_URL=...      # строка подключения PostgreSQL
```

Конфиг агента — `config/agents/{Name}.toml`, перечитывается на лету:

```toml
[agent]
name = "Assistant"
language = "ru"
provider = "openai"
model = "gpt-4o-mini"

[agent.tool_loop]
max_iterations = 50
detect_loops = true
```

---

## Разработка

```bash
make check           # cargo check --all-targets
make test            # cargo test (пропускает sqlx::test без БД)
make lint            # cargo clippy --all-targets -- -D warnings
make remote-deploy   # сборка на сервере → атомарная подмена + рестарт
make doctor          # GET /api/doctor
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
└── migrations/             # Миграции PostgreSQL (автозапуск при старте)
```

---

## Лицензия

MIT — см. [LICENSE](LICENSE).
