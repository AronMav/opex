# Deployment Guide

## Overview

OPEX — единый Rust-бинарник, который запускает HTTP API, агентов, LLM-вызовы,
channel-адаптеры (TypeScript/Bun) и media-хаб (Python/FastAPI) как дочерние процессы.
Инфраструктура (PostgreSQL + pgvector, SearXNG, browser-renderer) поднимается через
Docker Compose. MCP-серверы управляются on-demand через bollard API.

```
systemd: opex-core (Rust)
  ├── child: channels (Bun/TypeScript) — Telegram, Discord, Matrix, IRC, Slack
  ├── child: toolgate (Python/FastAPI) — STT, TTS, Vision, Embeddings
  └── connects to:
      ├── PostgreSQL 17 + pgvector (Docker)
      ├── SearXNG (Docker, порт 8080)
      ├── browser-renderer (Docker, порт 9020)
      └── LLM providers (HTTPS)

systemd: opex-watchdog (Rust) — health monitoring + alerting
systemd: opex-memory-worker (Rust) — background embedding tasks
```

---

## Prerequisites

### Инструменты сборки (только для dev/CI)

| Инструмент | Версия | Зачем |
|---|---|---|
| Rust + cargo | stable | сборка бинарников |
| cargo-zigbuild | latest | кросс-компиляция на ARM64 без OpenSSL |
| Node.js + npm | 22.x | сборка UI |
| Bun | 1.x | runtime channel-адаптеров |
| Python 3 | 3.11+ | toolgate |

### Runtime (на целевом сервере)

| Компонент | Зачем |
|---|---|
| Docker 20+ с Docker Compose | PostgreSQL, SearXNG, browser-renderer, MCP-контейнеры |
| Bun 1.x | channels/ runtime |
| Python 3.11+ + venv | toolgate/.venv |
| systemd (user-mode) | управление сервисами |

> **Почему zigbuild?** Кросс-компиляция aarch64 требует линковки под Linux. Zigbuild
> предоставляет кросс-линкер через Zig toolchain. Весь Rust-код использует `rustls-tls`
> — OpenSSL нигде не требуется и намеренно исключён.

---

## Local Development

```bash
# Проверка кода
make check           # cargo check --all-targets

# Тесты
make test            # cargo test
cargo test test_name -- --nocapture  # один тест с выводом

# Lint
make lint            # cargo clippy --all-targets -- -D warnings

# UI (dev server)
cd ui && npm run dev        # порт 3000
cd ui && npm test           # vitest (one-shot)

# Channel adapters
cd channels && bun test

# Генерация TypeScript-типов из Rust
make gen-types       # cargo run --features ts-gen --bin gen_ts_types
```

---

## Building

### Для host-архитектуры

```bash
cargo build --release
# бинарники: target/release/opex-{core,watchdog,memory-worker}
```

### Для ARM64 (Raspberry Pi)

```bash
make build-arm64
# эквивалент:
# cargo zigbuild --release --target aarch64-unknown-linux-gnu \
#   -p opex-core -p opex-watchdog -p opex-memory-worker
# бинарники: target/aarch64-unknown-linux-gnu/release/opex-{core,watchdog,memory-worker}
```

### UI

```bash
make ui
# эквивалент: cd ui && npm run build
# RSC flattening: ui/build/adapter.cjs (via experimental.adapterPath, встроен в next build)
# выходная директория: ui/out/
```

---

## Release Packaging

```bash
./release.sh 0.4.0          # сборка для host-архитектуры
./release.sh 0.4.0 --all    # сборка для aarch64 + x86_64
```

Скрипт:

1. Синхронизирует версию в `Cargo.toml`, `ui/package.json`, `channels/package.json`
2. Компилирует все три бинарника для каждого target
3. Собирает Next.js UI
4. Упаковывает архив `release/opex-v<VERSION>.tar.gz`

**Содержимое архива:**

```
opex/
├── opex-core-aarch64        # Rust binary (ARM64)
├── opex-core-x86_64         # Rust binary (x86_64)
├── opex-watchdog-aarch64
├── opex-watchdog-x86_64
├── opex-memory-worker-aarch64
├── opex-memory-worker-x86_64
├── opex-ui.tar.gz           # pre-built Next.js static UI
├── config/                  # default agent configs, opex.toml
├── migrations/              # SQL migrations (43 файла, 001..043)
├── workspace/               # tools, skills, mcp definitions
├── channels/                # TypeScript source (bun install на месте)
├── toolgate/                # Python source (pip install на месте)
├── docker/                  # docker-compose.yml + Dockerfiles
├── scripts/                 # mcp-deploy.sh, check_compose_limits.sh
├── setup.sh                 # интерактивный инсталлятор
├── update.sh                # updater
├── uninstall.sh             # полное удаление
├── .env.example             # шаблон env-файла
└── VERSION                  # версия (читается setup/update скриптами)
```

**CI/CD release:** тег `v*` запускает `.github/workflows/release.yml` — GitHub Actions
собирает через zigbuild, публикует `opex-v*.tar.gz` в GitHub Releases.

---

## Fresh Install on Pi

### Метод 1: через release archive (рекомендуется)

```bash
# На dev-машине
scp release/opex-v<VERSION>.tar.gz aronmav@192.168.1.82:~/

# На Pi
ssh aronmav@192.168.1.82
tar xzf opex-v<VERSION>.tar.gz
cd opex
./setup.sh
```

`setup.sh` в автоматическом режиме:

1. Обнаруживает пакетный менеджер (apt/dnf/pacman/apk)
2. Устанавливает Docker (если нет) через `curl -fsSL https://get.docker.com | sh`
3. Устанавливает Bun через `curl -fsSL https://bun.sh/install | bash`
4. Устанавливает Python3 / python3-venv
5. Генерирует `.env` с random auth-токеном и master-ключом
6. Настраивает Docker TCP listener на `127.0.0.1:2375` (нужен для bollard API)
7. Собирает Docker-образы: `opex-pg:17-age-pgvector`, `browser-renderer`, `opex-sandbox`
8. Создаёт on-demand MCP контейнеры (`--profile on-demand create --no-recreate`)
9. Запускает инфраструктуру: PostgreSQL, SearXNG, browser-renderer
10. Устанавливает bun-зависимости channels (`bun install`)
11. Создаёт Python venv для toolgate (`python3 -m venv .venv && pip install -r requirements.txt`)
12. Создаёт systemd user-service файлы
13. Запускает все три сервиса

**Параметры setup.sh:**

```bash
./setup.sh --verbose    # показывать полный вывод вместо спиннеров
./setup.sh --dry-run    # показать план без изменений
./setup.sh --no-systemd # пропустить установку systemd-юнитов
```

### Метод 2: из исходников

```bash
git clone <repo> opex
cd opex
./setup.sh  # автоматически обнаружит отсутствие бинарников и скомпилирует
```

### Ручная настройка .env

Если `setup.sh` не используется, создай `.env` в директории установки:

```bash
# ~/.env (или ~/opex/.env)
OPEX_AUTH_TOKEN=$(openssl rand -hex 32)
OPEX_MASTER_KEY=$(openssl rand -hex 32)
DATABASE_URL=postgresql://opex:opex@localhost:5432/opex
```

> **ВАЖНО:** `OPEX_MASTER_KEY` шифрует secrets vault (ChaCha20-Poly1305).
> Потеря ключа = потеря всех сохранённых секретов. Бэкапь отдельно от `.env`.

**Политика:** только эти 3 переменные принадлежат `.env`. Всё остальное
(API-ключи, bot tokens) хранится в secrets vault через Web UI или
`POST /api/secrets`.

---

## Database Migrations

Миграции применяются **автоматически при каждом старте** через sqlx:

```rust
// crates/opex-core/src/main.rs
sqlx::migrate::Migrator::new(std::path::Path::new("migrations"))
    .await?
    .run(&db_pool)
    .await?;
```

Файлы: `migrations/001_init.sql` ... `migrations/043_messages_is_mirror.sql`.
43 миграции. Накатываются идемпотентно. Ручное применение не нужно.

---

## Deploy from Dev Machine

Целевой хост задаётся через `.deploy.env` или env-переменную:

```bash
# .deploy.env (в корне репозитория, не коммитить)
PI_HOST=aronmav@192.168.1.82
```

### Полный деплой

```bash
make deploy
```

Выполняет последовательно:

1. `make build-arm64` — zigbuild всех трёх бинарников
2. `make deploy-binary` — scp бинарников + `systemctl --user restart` каждого сервиса
3. `make deploy-ui` — `npm run build`, удаление старого `ui/out` на Pi, tar-архив и распаковка
4. `make deploy-migrations` — scp `migrations/*.sql`
5. `make deploy-docker` — rsync `docker/` на Pi + `docker compose up -d --build`
6. Health check через `/api/doctor`

### Частичный деплой

```bash
make deploy-binary      # только Rust-бинарники (build + scp + restart)
make deploy-ui          # только UI (build + deploy)
make deploy-migrations  # только SQL-миграции
make deploy-docker      # только docker compose (rsync + rebuild + up)
```

### Только toolgate (Python)

Toolgate — нативный процесс, не Docker. Деплой изменённых `.py` файлов:

```bash
# Пример: изменился toolgate/providers/tts.py
scp toolgate/providers/tts.py aronmav@192.168.1.82:~/opex/toolgate/providers/
curl -X POST -H "Authorization: Bearer $TOKEN" http://192.168.1.82:18789/api/services/toolgate/restart
```

**Не нужно:** docker build, docker push, пересборка образа.
**Не нужно:** перезапуск opex-core — Core перезапускает toolgate как child process.

---

## Systemd Services

`setup.sh` создаёт три user-mode юнита в `~/.config/systemd/user/`:

### opex-core.service

```ini
[Unit]
Description=OPEX Core
After=network.target

[Service]
Type=simple
WorkingDirectory=/home/user/opex
ExecStart=/home/user/opex/opex-core-aarch64
EnvironmentFile=/home/user/opex/.env
Environment=PATH=/home/user/.bun/bin:/home/user/.local/bin:/usr/local/bin:/usr/bin:/bin
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

### opex-watchdog.service

```ini
[Unit]
Description=OPEX Watchdog
After=opex-core.service

[Service]
Type=notify
WorkingDirectory=/home/user/opex
ExecStart=/home/user/opex/opex-watchdog-aarch64 config/watchdog.toml
EnvironmentFile=/home/user/opex/.env
Environment=OPEX_CORE_URL=http://localhost:18789
WatchdogSec=120
Restart=always
RestartSec=10
```

### opex-memory-worker.service

```ini
[Unit]
Description=OPEX Memory Worker
After=opex-core.service

[Service]
Type=notify
WorkingDirectory=/home/user/opex
ExecStart=/home/user/opex/opex-memory-worker-aarch64
EnvironmentFile=/home/user/opex/.env
WatchdogSec=300
Restart=always
RestartSec=10
```

### Управление сервисами

```bash
# Статус
systemctl --user status opex-core
make status                              # ssh + systemctl status

# Логи
journalctl --user -u opex-core -f --no-pager
make logs                                # ssh + journalctl -f

# Перезапуск
systemctl --user restart opex-core
make restart                             # ssh + systemctl restart

# Включить автозапуск при загрузке (без логина)
loginctl enable-linger $USER
```

> **docker/opex-core.service** в репозитории — устаревший пример для system-mode
> (`WantedBy=multi-user.target`). setup.sh создаёт user-mode юниты. Не использовать
> docker/opex-core.service напрямую.

---

## Docker Infrastructure

### Запуск инфраструктуры

```bash
cd ~/opex
docker compose -f docker/docker-compose.yml up -d postgres searxng browser-renderer
```

### Сервисы

| Сервис | Образ | Порт | Назначение |
|---|---|---|---|
| `postgres` | `opex-pg:17-age-pgvector` | `127.0.0.1:5432` | PostgreSQL 17 + pgvector + Apache AGE |
| `searxng` | `searxng/searxng:latest` | `127.0.0.1:8080` | Метапоисковик для агентов |
| `browser-renderer` | `browser-renderer:latest` | `127.0.0.1:9020` | Headless browser для web scraping |

Все три всегда запущены. Memory limits и CPU через env-переменные в `docker/.env`:

```bash
POSTGRES_MEM_LIMIT=2g
POSTGRES_CPUS=1.5
SEARXNG_MEM_LIMIT=256m
BROWSER_RENDERER_MEM_LIMIT=1g
```

### MCP-серверы (on-demand)

MCP-контейнеры управляются core через bollard API. Запускаются по требованию агента,
останавливаются после idle timeout. Профиль `on-demand` в docker-compose.yml —
контейнеры создаются заранее, но не запускаются.

Встроенные MCP-серверы:

| Контейнер | Порт | Назначение |
|---|---|---|
| `mcp-stock-analysis` | 9003 | Финансовый анализ |
| `mcp-weather` | 9004 | Погода |
| `mcp-obsidian` | 9005 | Obsidian vault |
| `mcp-browser-cdp` | 9030 | Browser CDP automation |
| `mcp-postgres` | 127.0.0.1:9007 | PostgreSQL MCP |
| `mcp-fetch` | 9040 | HTTP fetch |
| `mcp-memory` | 9041 | Knowledge graph |
| `mcp-sequential-thinking` | 9042 | Step-by-step reasoning |
| `mcp-time` | 9044 | Time/timezone |
| `mcp-filesystem` | 9045 | Filesystem access |
| `mcp-git` | 9046 | Git operations |
| `mcp-notion` | 9048 | Notion |
| `mcp-todoist` | 9049 | Todoist |

Добавить новый MCP-сервер:

```bash
# Node.js MCP из официального образа
~/opex/scripts/mcp-deploy.sh stdio-node mcp/fetch:latest fetch 9040

# Python MCP через pip
~/opex/scripts/mcp-deploy.sh stdio-python mcp-server-git git 9012 mcp-server-git

# Внешний HTTP MCP
~/opex/scripts/mcp-deploy.sh url https://context7.com/mcp context7

# Удалить
~/opex/scripts/mcp-deploy.sh remove fetch
```

### Docker TCP listener

Core подключается к Docker через bollard HTTP API (не Unix socket):

```json
{"hosts": ["unix:///var/run/docker.sock", "tcp://127.0.0.1:2375"]}
```

setup.sh настраивает это автоматически. При ручной установке:

```bash
sudo tee /etc/docker/daemon.json > /dev/null <<'EOF'
{"hosts": ["unix:///var/run/docker.sock", "tcp://127.0.0.1:2375"]}
EOF
sudo mkdir -p /etc/systemd/system/docker.service.d
sudo tee /etc/systemd/system/docker.service.d/override.conf > /dev/null <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/bin/dockerd
EOF
sudo systemctl daemon-reload && sudo systemctl restart docker
```

---

## Nginx (Static UI Serving)

Core сам отдаёт статику из `ui/out/` при отсутствии nginx. Nginx нужен для:

- TLS termination
- Кастомных заголовков (CSP)
- Кэширования статики

Минимальный конфиг:

```nginx
server {
    listen 80;
    server_name opex.local;

    # Static UI (Next.js out/)
    root /home/user/opex/ui/out;
    index index.html;

    # Proxy API + SSE + WebSocket к core
    location /api/ {
        proxy_pass http://127.0.0.1:18789;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 300s;

        # Phase 64 SEC-05: CSP observation mode (report-only)
        add_header Content-Security-Policy-Report-Only "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self' ws: wss:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; report-uri /api/csp-report" always;
    }

    location /uploads/ {
        proxy_pass http://127.0.0.1:18789;
    }

    # SPA fallback
    location / {
        try_files $uri $uri.html $uri/ /index.html;

        # Phase 64 SEC-05: CSP observation mode
        add_header Content-Security-Policy-Report-Only "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self' ws: wss:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; report-uri /api/csp-report" always;
    }
}
```

> **CSP статус:** заголовок `Content-Security-Policy-Report-Only` — observation mode.
> Нарушения логируются в `/api/csp-report`, но не блокируются.
> Переключить на `Content-Security-Policy` после проверки логов
> (CodeMirror/Mermaid/KaTeX workers).

setup.sh автоматически добавляет CSP заголовок в существующий конфиг если найдет
`/etc/nginx/sites-available/opex` или `/etc/nginx/conf.d/opex.conf`.

---

## Health Check

```bash
# Через make
make doctor
# эквивалент:
ssh aronmav@192.168.1.82 \
  "curl -sf -H 'Authorization: Bearer $AUTH' http://localhost:18789/api/doctor | python3 -m json.tool"

# Локально
curl -sf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" http://localhost:18789/api/doctor

# Простая проверка без токена
curl -sf http://localhost:18789/health
```

`/api/doctor` возвращает состояние всех компонентов: postgres, toolgate, channels,
провайдеры, память, scheduled jobs. Все секции должны иметь `"ok": true`.

---

## Updating

```bash
# Загрузить архив на сервер
scp release/opex-v<VERSION>.tar.gz aronmav@192.168.1.82:~/

# Запустить updater
ssh aronmav@192.168.1.82 "~/opex/update.sh ~/opex-v<VERSION>.tar.gz"
```

`update.sh` выполняет:

1. Создаёт бэкап `.env` (`→ .env.bak`)
2. Останавливает все сервисы + убивает orphaned процессы (bun, uvicorn)
3. Заменяет бинарники (по arch)
4. Заменяет UI (`ui/out/`)
5. Обновляет channels source + `bun install`
6. Обновляет toolgate source + `pip install -r requirements.txt` (сохраняет `.venv`)
7. Обновляет docker/ + пересобирает образы (rsync, сохраняет docker/.env)
8. Обновляет migrations/
9. Обновляет scripts/, setup.sh, update.sh, uninstall.sh
10. Восстанавливает `.env` из бэкапа если был изменён
11. Запускает сервисы через systemctl
12. Health check на `http://localhost:18789/health`

**Что сохраняется:** `.env`, `config/`, `workspace/`, PostgreSQL data (volume `pgdata`).

**Что заменяется:** бинарники, UI, channels, toolgate, docker, migrations, scripts.

---

## Rollback

OPEX не имеет встроенного rollback. Стратегия:

### Бинарники

```bash
# Перед обновлением (на Pi) сделать бэкап
cp ~/opex/opex-core-aarch64 ~/opex/opex-core-aarch64.bak

# Откат
cp ~/opex/opex-core-aarch64.bak ~/opex/opex-core-aarch64
systemctl --user restart opex-core
```

### База данных

Миграции `sqlx` накатываются в одну сторону. Откат схемы не предусмотрен.
Для отката создай бэкап БД до обновления:

```bash
# Бэкап
docker exec -t docker-postgres-1 pg_dump -U opex opex > backup-$(date +%Y%m%d).sql

# Восстановление
docker exec -i docker-postgres-1 psql -U opex opex < backup-20260504.sql
```

### Полная переустановка

```bash
./uninstall.sh      # удаляет всё: сервисы, Docker, данные, директорию
# затем setup.sh из предыдущей версии архива
```

---

## Uninstall

```bash
./uninstall.sh          # с подтверждением
./uninstall.sh --yes    # без подтверждений (опасно)
```

Удаляет:

- Все systemd-юниты `opex*.service`
- Все Docker контейнеры (compose + bollard-managed `hc-*`, `mcp-*`)
- Docker volume `pgdata` (все данные БД)
- Docker network `opex`
- Docker образы `opex-*`, `browser-renderer`, `searxng/*`
- Всю директорию установки

Не удаляет: Docker engine, Bun, Python, Node.js.

---

## First Launch: Setup Wizard

После первого запуска открой Web UI по адресу `http://your-server:18789`.
4-шаговый wizard:

1. Requirements check (Docker, PostgreSQL, disk space)
2. Провайдер + тест API ключа
3. Создание первого агента (`base = true`, `access.mode = "restricted"`)
4. Настройка Telegram channel (опционально)

После `POST /api/setup/complete` wizard закрывается навсегда (`system_flags` в БД).

```bash
# Проверить статус
curl -sf -H "Authorization: Bearer $TOKEN" http://localhost:18789/api/setup/status

# Проверить требования (без авторизации)
curl -sf http://localhost:18789/api/setup/requirements
```

---

## Security Hardening

### Сеть

- Ограничь порт 18789 до localhost или доверенных IP
- Используй reverse proxy (nginx/caddy) с TLS для внешнего доступа
- Firewall: закрой 18789, 5432, 8080, 9020 от внешнего интернета

### Аутентификация

- Auth токен: 32+ байт hex (`openssl rand -hex 32`)
- Ротируй `OPEX_AUTH_TOKEN` периодически
- Authenticated requests (valid Bearer) не подпадают под rate limiting (300 rpm default)
- Auth lockout: 500 failed attempts → 30s блок для запросов без Authorization header

### Secrets

- `.env` никогда не коммитить в git
- Бэкапить `OPEX_MASTER_KEY` отдельно
- API-ключи провайдеров хранить в secrets vault (не в config файлах)
- Credentials каналов (bot_token и др.) хранятся в зашифрованном vault, не в JSONB

### Sandbox

- Non-base агенты выполняют code_exec в Docker-контейнерах (изолировано)
- Base агенты (`base = true`) работают на хосте — давать только доверенным агентам
- `.ssh`, `.aws` и подобные директории заблокированы от bind mounts в sandbox
- Sensitive env vars (`OPEX_*`, `DATABASE_URL`) фильтруются из окружения sandbox

### CORS

```toml
# config/opex.toml
[gateway]
cors_origins = ["https://your-domain.com"]
```

---

## Troubleshooting

### Core не стартует

```bash
journalctl --user -u opex-core -f --no-pager
```

Частые причины:

- PostgreSQL не запущен: `docker ps | grep postgres`
- Неверный `DATABASE_URL` в `.env`
- Порт 18789 занят другим процессом
- Отсутствует `.env` файл
- Docker TCP listener не настроен: `curl http://127.0.0.1:2375/version`

### Channels не подключаются

```bash
curl -H "Authorization: Bearer $TOKEN" http://localhost:18789/api/doctor
```

Смотри `channels.ok`. Если false:

- Bun не установлен: `bun --version`
- Директория `channels/` или `node_modules` отсутствует
- WebSocket connection refused (смотри логи core)

### Toolgate / STT / TTS / Embeddings не работают

```bash
curl -H "Authorization: Bearer $TOKEN" http://localhost:18789/api/doctor
```

Смотри `toolgate.*`. Если проблема:

- toolgate venv не создан: `ls ~/opex/toolgate/.venv`
- Toolgate упал: перезапусти через API

  ```bash
  curl -X POST -H "Authorization: Bearer $TOKEN" http://localhost:18789/api/services/toolgate/restart
  ```

- Провайдер embedding не настроен в Active Providers

### Memory Worker завис

```bash
systemctl --user restart opex-memory-worker
```

Worker автоматически восстанавливает застрявшие задачи (`processing` → reset) при старте.

### Высокое потребление памяти

- Норма idle: ~40-80 MB для core
- Проверь Docker: `docker stats`
- Перезапусти memory worker: `systemctl --user restart opex-memory-worker`

### Логи

```bash
# Core
journalctl --user -u opex-core -f --no-pager
make logs                             # через ssh

# Watchdog
journalctl --user -u opex-watchdog -f

# Memory worker
journalctl --user -u opex-memory-worker -f

# Все сервисы
journalctl --user -u 'opex-*' -f
```

---

## File Layout on Pi

```
~/opex/
├── opex-core-aarch64         # main binary
├── opex-watchdog-aarch64     # watchdog binary
├── opex-memory-worker-aarch64# memory worker binary
├── .env                      # secrets (AUTH_TOKEN, MASTER_KEY, DATABASE_URL)
├── VERSION                   # текущая версия
├── setup.sh / update.sh / uninstall.sh
├── config/
│   ├── opex.toml             # основной конфиг
│   ├── agents/               # agent configs (Name.toml)
│   └── services/             # service registry YAMLs
├── migrations/               # SQL migrations (читаются при старте)
├── workspace/
│   ├── tools/                # YAML tool definitions
│   ├── skills/               # shared skill markdown files
│   ├── agents/               # agent workspace (MEMORY.md, etc.)
│   ├── mcp/                  # MCP server definitions
│   └── uploads/              # binary file uploads
├── channels/                 # TypeScript source (bun runtime)
│   ├── src/
│   └── node_modules/
├── toolgate/                 # Python source (uvicorn runtime)
│   ├── app.py
│   ├── providers/
│   ├── requirements.txt
│   └── .venv/
├── docker/
│   ├── docker-compose.yml
│   ├── .env                  # POSTGRES_USER, POSTGRES_PASSWORD
│   ├── Dockerfile.postgres
│   ├── Dockerfile.sandbox
│   ├── mcp/                  # MCP Dockerfiles
│   └── mcp-bridge/
├── scripts/
│   └── mcp-deploy.sh
└── ui/
    └── out/                  # static Next.js build
```

---

## CI/CD

| Workflow | Триггер | Что делает |
|---|---|---|
| `.github/workflows/ci.yml` | push/PR на master | cargo check + test + clippy; tsc --noEmit; npm run build; API types drift check |
| `.github/workflows/release.yml` | push tag `v*` | zigbuild aarch64+x86_64; npm run build; публикует release archive |
| `.github/workflows/integration.yml` | push/PR на master | интеграционные тесты |

Release workflow использует Ubuntu GitHub Actions runner + Zig 0.13.0 для кросс-компиляции.
