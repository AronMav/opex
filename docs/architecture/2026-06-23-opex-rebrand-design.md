# Полное переименование Opex → OPEX

**Дата:** 2026-06-23
**Статус:** дизайн одобрен, готов к writing-plans
**Тип:** ребрендинг + локализация (Rust + UI + инфраструктура + БД)

## Цель

Полностью уйти от имени `opex` во всём проекте и заменить его на `opex`,
сделав русский язык основным. Критерий успеха — `grep -rni opex` по
репозиторию даёт **0 совпадений**.

### Принятые решения

| Вопрос | Решение |
|---|---|
| Глубина | Тотальная, включая рантайм (крейты, env, пути, systemd, БД, docker) |
| Бренд в видимом тексте | Везде `OPEX` (латиница). `ОРЕХ` — русское прочтение, упоминается опционально |
| Русский язык | UI по умолчанию `ru`; README русский основной (англ. → `README.en.md`); `docs/` переводятся |
| scaffold/системные подсказки | Только переименование, язык НЕ меняем |
| Домен | `hc.aronmav.ru` **остаётся** (DNS/сертификаты не трогаем) |
| БД | Переименовываем (`opex` → `opex`: имя БД, роль, пароль) |
| GitHub-репо | Переименовываем `AronMav/opex` → `AronMav/opex` (ручная операция, авто-редирект) |
| Версия | Бамп `0.30.0` → `0.31.0` как маркер |

### Стратегия исполнения

Выбран **поэтапный, обратно-совместимый** подход (вариант A): код-ренейм
деплоится без простоя за счёт dual-read env, миграция инфраструктуры — отдельный
обратимый шаг, финальная зачистка fallback — в конце. Согласуется с правилами
проекта «incremental deploy only» и «no push without approval».

Три PR / фазы:

- **PR1** — код, бренд, локализация (без простоя; работает на старой инфре).
- **PR2** — миграция сервера (пути, env, systemd, БД, docker) по runbook.
- **PR3** — зачистка dual-read fallback и legacy-shim; финальный аудит.

---

## Каноническая таблица соответствий (источник истины)

### Код / сборка (PR1, латиница обязательна)

| Было | Стало |
|---|---|
| крейты `opex-core/-types/-watchdog/-memory-worker/-db/-embedding/-gateway-util` | `opex-core/-types/-watchdog/-memory-worker/-db/-embedding/-gateway-util` |
| импорты `opex_core::`, `opex_types::` … | `opex_core::`, `opex_types::` … |
| бинарники `opex-core-x86_64` и т.д. | `opex-core-x86_64` и т.д. |
| RUST_LOG/OTEL `opex_core`, `opex-memory-worker`, service.name | `opex_core`, `opex-memory-worker` |
| insta-снапшоты `opex_core__…snap` | `opex_core__…snap` (переименовать файлы) |
| `config/opex.toml` | `config/opex.toml` |
| `config/watchdog.toml` (имена сервисов внутри) | обновить ссылки |
| mDNS-хостнейм `opex.local` (хардкод в `network.rs`) | `opex.local` |

### Рантайм / env (PR1 — dual-read, PR2 — миграция на сервере)

| Было | Стало |
|---|---|
| `OPEX_AUTH_TOKEN`, `OPEX_MASTER_KEY` | `OPEX_AUTH_TOKEN`, `OPEX_MASTER_KEY` (значения 1:1!) |
| `OPEX_CORE_URL`, `OPEX_DISABLE_REDRIVE` | `OPEX_CORE_URL`, `OPEX_DISABLE_REDRIVE` |
| test-only `OPEX_PG_TEST_IMAGE`, `…_GEMINI_TEST_*`, `…_MIGRATION_BUDGET_MS`, `…_OAUTH_CREDENTIALS_PATH`, `…_GEMINI_TEST_TOKEN_ENDPOINT` | `OPEX_*` (без fallback — переименовать сразу) |

### Клиент / localStorage (PR1)

| Было | Стало | Примечание |
|---|---|---|
| `opex.auth.token` | `opex.auth.token` | **+ shim**: читать старый ключ, если нового нет (не разлогинит) |
| `opex.language` | `opex.language` | **+ shim** (не сбросит выбранный язык) |
| `opex.draft.*`, `opex.chat.lastSession`, `opex.lastSession`, `opex-rq` (IndexedDB), `opex_wizard_progress`, `opex_debug_sse`, событие `opex:stop-stream` | `opex.*` / `opex-rq` / `opex:stop-stream` | без shim — эфемерное, допустим сброс |

### Инфраструктура (PR2 — runbook на сервере)

| Было | Стало |
|---|---|
| `~/opex` (install), `~/opex-src` (build) | `~/opex`, `~/opex-src` |
| systemd `opex-core/-watchdog/-memory-worker.service` | `opex-core/-watchdog/-memory-worker.service` |
| `.deploy.env` `SERVER_DIR=~/opex` | `~/opex` |
| docker-образы `opex-pg`, `opex-tts-silero`, `opex-mcp-*` | `opex-pg`, `opex-tts-silero`, `opex-mcp-*` (retag/пересборка на сервере) |
| docker-сеть `opex`, volume-маунт `~/opex/workspace` | сеть `opex`, `~/opex/workspace` |
| `POSTGRES_DB=opex`, `pg_isready -U opex`, роль/пароль `opex` | `opex` (на живой БД через `ALTER DATABASE/ROLE … RENAME` + `ALTER ROLE … PASSWORD`) |
| `DATABASE_URL=…opex:opex@…/opex` | `…opex:opex@…/opex` |

---

## Капкан миграций БД и его решение

Слово `opex` в `migrations/*.sql` встречается двух видов:

1. **Комментарии** (`-- Opex — consolidated schema` в 001, 022, 029, 033, и др.).
2. **Функциональный тег `@opex:ephemeral`** (m030, m050) — живая метка в
   каталоге БД: `COMMENT ON TABLE … IS '@opex:ephemeral …'`; код бэкапа
   (`gateway/handlers/backup.rs`) ищет ephemeral-таблицы по
   `LIKE '@opex:ephemeral%'`.

**Проблема:** sqlx хранит SHA-384-чексумму каждого применённого файла миграции в
таблице `_sqlx_migrations`. Любое редактирование уже применённого файла (даже
комментария) меняет чексумму → на старте sqlx выдаёт «migration was previously
applied but has been modified» и **отказывается стартовать**. Публичного тумблера
«игнорировать checksum mismatch» в sqlx нет.

**Решение (полный покрас + безопасный прод):**

1. Редактируем `opex`→`opex` во **всех** `.sql`, включая комментарии старых
   миграций. Меняются **только комментарии** — схема БД не затрагивается.
2. Новая миграция `051_rename_ephemeral_tag.sql` переписывает живые
   `COMMENT ON TABLE … @opex:ephemeral` → `@opex:ephemeral` для всех
   ephemeral-таблиц; в коде меняем discovery-префикс на `@opex:ephemeral%`.
3. **Реконсиляция чексумм** (одноразово, в runbook PR2): helper-бинарник
   `opex-migrate-checksums` считает чексуммы новых файлов **тем же кодом sqlx**
   (байт-в-байт с тем, что эмбеддит бинарник), печатает
   `UPDATE _sqlx_migrations SET checksum = … WHERE version = …`, применяем к живой
   БД **до** запуска нового бинарника. Так как изменились только комментарии,
   применённая схема идентична → операция обратима и не трогает данные.

Итог: `grep -rni opex` пуст, прод-БД стартует без ошибок.

---

## PR1 — код, бренд, локализация (без простоя)

**Цель:** репозиторий полностью становится `opex`, но новый бинарник заводится на
**старой** инфраструктуре (старый `.env` с `OPEX_*`, старые пути
`~/opex`) — деплой безопасен.

**Объём:**

1. **Крейты:** переименовать 7 директорий `crates/opex-*` → `crates/opex-*`;
   `[package] name`, workspace `members`, path-deps (`opex-types = { path = … }`);
   все `use opex_*::` → `use opex_*::`; имена бинарников; RUST_LOG/OTEL-строки;
   insta-снапшоты (`opex_core__…` → `opex_core__…`).
2. **Env — dual-read:** ввести хелпер `env_var("AUTH_TOKEN")`, читающий
   `OPEX_AUTH_TOKEN`, при отсутствии — `OPEX_AUTH_TOKEN`. Применить к
   `AUTH_TOKEN`, `MASTER_KEY`, `CORE_URL`, `DISABLE_REDRIVE`. Автогенерация `.env`
   в `main.rs` пишет уже `OPEX_*`. Тестовые env — переименовать сразу без
   fallback.
3. **Конфиг:** `config/opex.toml` → `config/opex.toml`; загрузчик в
   `config/mod.rs` и дефолт-аргумент memory-worker; `config/watchdog.toml`.
4. **Миграции:** правки комментариев во всех `.sql` + новая `051` (см. раздел
   выше).
5. **Бренд-тексты:** все видимые строки → `OPEX` (UI-заголовки, `app-sidebar`,
   `layout`, `login`/`setup`, scaffold `SOUL.md`/`MEMORY.md`, скиллы, тексты
   доков). mDNS `opex.local` → `opex.local`.
6. **Локализация:** UI default `locale: 'en'` → `'ru'`; README на русском
   (англ. → `README.en.md`); перевод **верхнеуровневых** доков на русский
   (`docs/API.md`, `docs/ARCHITECTURE.md`, `docs/CONFIGURATION.md`,
   `docs/DEPLOYMENT.md`, `docs/UPGRADE_NOTES.md`, `SECURITY.md`, `CONTRIBUTING.md`).
   Исторические `docs/architecture/*` и `docs/releases/*` **не переводим** (это
   история) — в них меняется только имя `opex`→`opex`. localStorage-префикс
   `opex.` → `opex.` + shim для `auth.token` и `language`.
7. **Версия:** бамп `[workspace.package] version` `0.30.0` → `0.31.0` +
   синхронизация в `ui/package.json`, `channels/package.json`.
8. **CI/скрипты:** `.github/workflows/*` (`-p opex-core` → `-p opex-core`),
   `Makefile` (**только крейт-таргеты** `-p opex-*` → `-p opex-*`),
   `release.sh`/`setup.sh`/`update.sh`/`uninstall.sh`, `scripts/*`.
   Пути/имена юнитов на сервере остаются старыми в PR1; deploy-конфиги
   (`.deploy.env`, `deploy/server/*.service`, `docker-compose.yml`) и
   path/unit-строки `Makefile` правит PR2 синхронно с миграцией (т.е. `Makefile`
   затрагивается обоими PR, но разными строками).

**Граница PR1:** на сервере НЕ трогаем `~/opex`, systemd-юниты, имя БД,
docker-образы/сеть.

**Проверка PR1:** `make check`, `make lint`, `cargo test` (DB-тесты при наличии
`DATABASE_URL`), `cd ui && npm run build && npm test`, `cd channels && bun test`.
Ручная проверка: `make remote-deploy` — сервис поднимается на **старом**
`.env`/путях (доказательство работы dual-read).

---

## PR2 — миграция сервера (runbook)

**Код в PR2** (синхронно с миграцией): `.deploy.env` (`SERVER_DIR`),
`deploy/server/*.service`, `docker/opex-core.service`,
`docker/docker-compose.yml` (образы, сеть, volume, `POSTGRES_DB`, `DATABASE_URL`),
`docker/.env.example`, `scripts/server-deploy.sh`, `scripts/mcp-deploy.sh`,
`Makefile` (пути/имена юнитов/бинарников).

**Runbook (упорядоченный, обратимый). Бэкап до всего:**

| # | Шаг | Гард / откат |
|---|---|---|
| 0 | `pg_dump` БД + `tar` `~/opex` + копия `.env` в `~/opex-migration-backup/` | при ошибке — restore из бэкапа |
| 1 | Стоп `opex-core/-watchdog/-memory-worker` | `start` старых юнитов |
| 2 | В `.env`: ключи `OPEX_*`→`OPEX_*`, **значения 1:1** (особенно `MASTER_KEY`) | dual-read из PR1 страхует частичную правку |
| 3 | `mv ~/opex ~/opex`, `mv ~/opex-src ~/opex-src` | `mv` обратно |
| 4 | БД: `docker exec psql -d postgres` → `ALTER DATABASE opex RENAME TO opex; ALTER ROLE opex RENAME TO opex; ALTER ROLE opex PASSWORD 'opex';` → правка `DATABASE_URL` | `ALTER … RENAME` обратимы; **прежде проверить, что pg-data — сохраняемый volume, а не пересоздаётся при смене имени проекта** |
| 5 | **Реконсиляция чексумм**: helper печатает `UPDATE _sqlx_migrations …`, применяем к БД `opex` | без этого новый бинарник падает на проверке миграций; UPDATE трогает только метаданные |
| 6 | Docker: `docker tag opex-*→opex-*` (pg, tts-silero, mcp-*), правка compose (`name: opex`, volume `~/opex/workspace`), `docker compose up -d` | retag обратим; **критично не потерять pg-data-volume** |
| 7 | Новые юниты `opex-core/-watchdog/-memory-worker.service` (новые пути, `EnvironmentFile`, бинарники), `daemon-reload`, `disable` старых, `enable --now` новых | держать старые `.service` до подтверждения |
| 8 | Сборка `cargo build --release -p opex-core -p opex-watchdog -p opex-memory-worker` в `~/opex-src` → atomic swap в `~/opex/opex-*-x86_64` | старые бинарники в бэкапе |
| 9 | Верификация: `make doctor` (16/16), `make logs`, UI на `hc.aronmav.ru`, тест-сообщение в Telegram | при красном — откат снизу вверх |

**Три «не убей прод» гарда (фиксируются явно):**

1. **`MASTER_KEY` значение сохраняется 1:1** — иначе vault секретов нечитаем.
2. **pg-data volume переживает переименование сети/проекта compose** — иначе
   потеря БД (проверить тип volume до шага 6).
3. **Чексумм-реконсиляция до старта** нового бинарника.

---

## PR3 — финальная зачистка

После нескольких дней стабильной работы сервера на `opex`
(наблюдение через `make doctor`/`make logs`):

1. Убрать dual-read fallback — `env_var` читает только `OPEX_*`.
2. Убрать localStorage-shim для `auth.token` и `language`.
3. Снести старые `opex-*.service` с сервера и `~/opex-migration-backup/`
   (после подтверждения).
4. Финальный аудит: `grep -rni opex` по репо = **0**.

---

## Тестирование

- **PR1:** `make check` + `make lint` + `cargo test` (DB-тесты при наличии
  `DATABASE_URL`) + `cd ui && npm run build && npm test` + `cd channels && bun
  test`. insta-снапшоты пересняты под `opex_core__…`. Ручная проверка: новый
  бинарник стартует на **старом** `.env` (доказательство dual-read).
- **PR2:** прогон runbook на сервере; gate = `make doctor` 16/16 + живой
  Telegram-обмен + UI логинится (shim сохранил токен).
- **PR3:** `grep -rni opex` = 0; `make doctor` 16/16 после удаления fallback.

## Откат

- **PR1** — git revert + `make remote-deploy` (инфра не менялась).
- **PR2** — пошаговый откат по таблице runbook снизу вверх; полный — restore из
  `~/opex-migration-backup/` (pg_dump + tar + .env).
- **PR3** — revert; требует возврата `OPEX_*` ключей на сервере (поэтому PR3
  только после стабилизации).

## Критерии готовности

1. `grep -rni opex` по репозиторию = 0.
2. Прод на `hc.aronmav.ru` работает: `make doctor` 16/16, Telegram-обмен, UI-чат.
3. Vault секретов читается (доказывает сохранность `MASTER_KEY`).
4. БД переименована в `opex`, данные на месте, миграции проходят без
   checksum-ошибок.
5. UI по умолчанию на русском; README русский (+`README.en.md`); `docs/`
   переведены.
6. GitHub-репо `AronMav/opex` (авто-редирект со старого), бейджи/ссылки
   обновлены.
7. Версия `0.31.0`.

## Вне объёма (YAGNI)

- Домен `hc.aronmav.ru` не меняем.
- Squash миграций не делаем (старые файлы редактируются только текстово, схема не
  трогается).
- Язык code-комментариев и commit-истории не переводим.
- scaffold/системные подсказки агентам — только переименование, язык не меняем.
