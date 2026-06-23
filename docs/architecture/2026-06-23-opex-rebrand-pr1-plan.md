# OPEX Rebrand — PR1 (код, бренд, локализация) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Превратить весь код/бренд/локализацию репозитория из `hydeclaw` в `opex`, сохранив возможность безопасного деплоя нового бинарника на ещё не мигрированную инфраструктуру (старый `.env`, старые пути, старая БД).

**Architecture:** Поэтапный обратно-совместимый ренейм. Код читает новые имена (`OPEX_*`, `config/opex.toml`) с fallback на старые (`HYDECLAW_*`, `config/hydeclaw.toml`), поэтому PR1 деплоится без простоя и без миграции сервера. Миграция инфраструктуры/БД и зачистка fallback — отдельные фазы (PR2/PR3).

**Tech Stack:** Rust 2024 (cargo workspace, 7 крейтов, sqlx, insta), Next.js/React/Zustand (UI), Bun/TypeScript (channels), PostgreSQL 17 + pgvector.

**Спека:** [docs/architecture/2026-06-23-opex-rebrand-design.md](2026-06-23-opex-rebrand-design.md)

## Global Constraints

- **rustls-tls only** — никогда не добавлять OpenSSL-зависимости.
- **Бренд в видимом тексте:** везде `OPEX` (латиница). `ОРЕХ` — только русское прочтение, опционально в README.
- **Имена в коде/env/путях:** только ASCII `opex` (кириллица невозможна).
- **Версия:** `[workspace.package] version` `0.30.0` → `0.31.0`; синк в `ui/package.json`, `channels/package.json`.
- **env-чтение продакшен-ключей** (`AUTH_TOKEN`, `MASTER_KEY`, `CORE_URL`, `DISABLE_REDRIVE`) — через dual-read helper, НИКОГДА не blind-replace `HYDECLAW_*`→`OPEX_*` в местах чтения.
- **Старые файлы миграций в PR1 НЕ редактируются** (checksum-safety; их правки — в PR2).
- **Частые коммиты, сборка зелёная на каждом коммите.**
- **Без `git push`** — только локальные коммиты (правило проекта).
- Команды массовой замены выполняются через Bash-инструмент (Git Bash, POSIX `sed`/`grep`).

## Отклонения от спеки (зафиксированы при детализации)

1. **Комментарии старых миграций (`migrations/0NN_*.sql`) не трогаем в PR1.** Спека относила их к PR1, но редактирование уже применённого файла меняет sqlx-чексумму → деплой PR1 на старую БД упадёт на старте. PR1 добавляет только новую `051_rename_ephemeral_tag.sql` (новая версия применяется чисто) и меняет код-`LIKE`. Правки старых комментариев + реконсиляция чексумм выполняются вместе в PR2.
2. **Загрузчик конфига делаем dual-path** (`config/opex.toml` → fallback `config/hydeclaw.toml`), а не просто переименование. Причина: `server-deploy.sh` синкает только бинарники, конфиг на сервере останется `hydeclaw.toml` → иначе новый бинарник не загрузит конфиг.

---

## File Structure

**Новый код:**
- `crates/opex-core/src/util/env.rs` — dual-read env helper (`env_var`).
- `crates/opex-core/src/util/config_path.rs` — dual-path резолвер пути конфига.
- `crates/opex-migrate-checksums/` — новый бинарный крейт-хелпер (печатает `UPDATE _sqlx_migrations …`); используется в PR2, но компилируется в PR1.
- `migrations/051_rename_ephemeral_tag.sql` — переписывает ephemeral-комментари на `@opex:ephemeral`.
- `README.en.md` — англоязычный README (бывший `README.md`).

**Переименование (git mv):**
- `crates/hydeclaw-{core,types,watchdog,memory-worker,db,embedding,gateway-util}` → `crates/opex-*`.
- `config/hydeclaw.toml` → `config/opex.toml`.
- snapshot-файлы `…/snapshots/hydeclaw_core__*.snap` → `opex_core__*.snap`.

**Модификация (ключевые):** root `Cargo.toml`, все crate `Cargo.toml`, `main.rs` (env autogen + config load), `crates/opex-watchdog/src/main.rs`, `crates/opex-memory-worker/src/main.rs`, `backup.rs` (LIKE), `ui/src/stores/language-store.ts`, `ui/src/stores/auth-store.ts`, `ui/src/stores/chat-persistence.ts`, `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx`, `gateway/handlers/network.rs` (mDNS), `.github/workflows/*`, `Makefile` (крейт-таргеты), `release.sh`/`setup.sh`/`update.sh`/`uninstall.sh`.

---

## Task 1: Переименовать крейты `hydeclaw-*` → `opex-*` (атомарно)

Это единственная атомарная единица, при которой проект компилируется. Включает директории, манифесты, импорты, имена бинарников, лог/OTEL-строки крейтовых имён и snapshot-файлы.

**Files:**
- Modify: `Cargo.toml` (workspace members + `hydeclaw-types` path dep)
- Modify: все `crates/*/Cargo.toml`
- Modify: все `.rs` с `use hydeclaw_*::` / `hydeclaw_*::`
- Rename: `crates/hydeclaw-*` → `crates/opex-*`; `…/snapshots/hydeclaw_core__*.snap` → `opex_core__*.snap`

**Interfaces:**
- Produces: крейты `opex_core`, `opex_types`, `opex_db`, `opex_embedding`, `opex_gateway_util`, бинарники `opex-core`, `opex-watchdog`, `opex-memory-worker`.

- [ ] **Step 1: Переименовать директории крейтов**

```bash
cd "d:/GIT/bogdan/hydeclaw"
for c in core types watchdog memory-worker db embedding gateway-util; do
  git mv "crates/hydeclaw-$c" "crates/opex-$c"
done
```

- [ ] **Step 2: Заменить имена крейтов в манифестах и путях-зависимостях**

```bash
# Имена пакетов и path-deps (дефис) во всех Cargo.toml
grep -rl 'hydeclaw-' --include=Cargo.toml . | xargs sed -i 's/hydeclaw-/opex-/g'
# Корневой workspace.dependencies path: crates/opex-types уже переименован выше
sed -i 's#path = "crates/hydeclaw-types"#path = "crates/opex-types"#' Cargo.toml
```

- [ ] **Step 3: Заменить идентификаторы крейтов в Rust-коде (подчёркивание)**

```bash
# use hydeclaw_core::, hydeclaw_types:: и т.д. + RUST_LOG/OTEL строки с crate-именами
grep -rl 'hydeclaw_' --include='*.rs' . | xargs sed -i 's/hydeclaw_/opex_/g'
# OTEL/лог-строки с дефисом (service.name = "hydeclaw-memory-worker" и пр.)
grep -rl 'hydeclaw-' --include='*.rs' . | xargs sed -i 's/hydeclaw-/opex-/g'
```

- [ ] **Step 4: Переименовать snapshot-файлы insta (имя выводится из имени крейта)**

```bash
cd "d:/GIT/bogdan/hydeclaw/crates/opex-core/src/gateway/handlers/agents/snapshots"
for f in hydeclaw_core__*.snap; do git mv "$f" "${f/hydeclaw_core__/opex_core__}"; done
# Метаданные source: внутри снапшотов (не критично, но для чистоты)
sed -i 's#crates/hydeclaw-core#crates/opex-core#g' opex_core__*.snap
cd "d:/GIT/bogdan/hydeclaw"
```

- [ ] **Step 5: Проверить сборку**

Run: `cargo check --all-targets`
Expected: компиляция без ошибок (0 errors). Предупреждения допустимы.

- [ ] **Step 6: Проверить тесты (включая snapshot-тесты)**

Run: `cargo test --workspace --no-run` затем `cargo test -p opex-core gateway::handlers::agents -- --nocapture`
Expected: snapshot-тесты проходят (контент снапшотов ещё содержит `hydeclaw` — это нормально, его меняет Task 6).

- [ ] **Step 7: Verify нет остаточных крейт-идентификаторов**

Run: `grep -rn 'hydeclaw_' --include='*.rs' . ; grep -rln 'hydeclaw-' --include=Cargo.toml .`
Expected: пусто.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: переименование крейтов hydeclaw-* -> opex-*"
```

---

## Task 2: Dual-read env helper для продакшен-ключей

**Files:**
- Create: `crates/opex-core/src/util/env.rs`
- Modify: `crates/opex-core/src/util/mod.rs` (или `lib.rs` — добавить `pub mod util;`/`pub mod env;` по факту)
- Modify: `crates/opex-core/src/main.rs:50` (autogen), `:1023` (master key), `:890` (DISABLE_REDRIVE)
- Modify: `crates/opex-watchdog/src/main.rs:37,42` (AUTH_TOKEN, CORE_URL)
- Test: `crates/opex-core/src/util/env.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub fn env_var(suffix: &str) -> Option<String>` — читает `OPEX_{suffix}`, при отсутствии `HYDECLAW_{suffix}`.

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/opex-core/src/util/env.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_opex_then_falls_back_to_hydeclaw() {
        let s = "PR1_ENV_TEST_KEY"; // уникальный суффикс, чтобы не конфликтовать
        unsafe { std::env::remove_var(format!("OPEX_{s}")); std::env::remove_var(format!("HYDECLAW_{s}")); }
        assert_eq!(env_var(s), None);

        unsafe { std::env::set_var(format!("HYDECLAW_{s}"), "legacy"); }
        assert_eq!(env_var(s).as_deref(), Some("legacy")); // fallback

        unsafe { std::env::set_var(format!("OPEX_{s}"), "new"); }
        assert_eq!(env_var(s).as_deref(), Some("new")); // приоритет OPEX

        unsafe { std::env::remove_var(format!("OPEX_{s}")); std::env::remove_var(format!("HYDECLAW_{s}")); }
    }
}
```

- [ ] **Step 2: Запустить тест — убедиться, что не компилируется/падает**

Run: `cargo test -p opex-core env_var -- --nocapture`
Expected: FAIL — `cannot find function env_var`.

- [ ] **Step 3: Реализовать helper**

```rust
// crates/opex-core/src/util/env.rs (верх файла)
//! Dual-read env: читает OPEX_<suffix>, при отсутствии — HYDECLAW_<suffix>.
//! Fallback удаляется в PR3 после миграции .env на сервере.

/// Возвращает значение env-переменной по суффиксу, предпочитая префикс `OPEX_`.
pub fn env_var(suffix: &str) -> Option<String> {
    std::env::var(format!("OPEX_{suffix}"))
        .ok()
        .or_else(|| std::env::var(format!("HYDECLAW_{suffix}")).ok())
}
```

Зарегистрировать модуль (если `util` ещё нет — создать `crates/opex-core/src/util/mod.rs` с `pub mod env;` и добавить `pub mod util;` в `lib.rs`).

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

Run: `cargo test -p opex-core env_var -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Применить helper к местам чтения**

В `crates/opex-core/src/main.rs`:
- `:1023` `std::env::var("HYDECLAW_MASTER_KEY")` → `crate::util::env::env_var("MASTER_KEY")` (сохранить ветку auto-generate; `Result`→`Option` — заменить `.unwrap_or_else(|_| …)` на `match … { Some(v)=>v, None=>{…} }`).
- `:890` `std::env::var("HYDECLAW_DISABLE_REDRIVE").is_ok()` → `crate::util::env::env_var("DISABLE_REDRIVE").is_some()`.

В `crates/opex-watchdog/src/main.rs` (helper в core; продублировать минимальную копию в watchdog `src/` либо вынести в `opex-gateway-util`; выбрать существующий общий крейт). Простейшее: добавить такую же `env_var` в watchdog локально:
- `:37` `std::env::var("HYDECLAW_AUTH_TOKEN").unwrap_or_default()` → `env_var("AUTH_TOKEN").unwrap_or_default()`.
- `:42` `std::env::var("HYDECLAW_CORE_URL")` → `env_var("CORE_URL")`.

- [ ] **Step 6: Обновить автогенерацию `.env` на запись `OPEX_*`**

`crates/opex-core/src/main.rs:50`:

```rust
let content = format!(
    "# Auto-generated by opex-core on first run\nOPEX_AUTH_TOKEN={}\nOPEX_MASTER_KEY={}\n",
    hex::encode(auth_bytes),
    hex::encode(key_bytes),
);
```

И `:1031-:1033` (дозапись master key в существующий `.env`): проверять отсутствие `OPEX_MASTER_KEY` и дописывать `OPEX_MASTER_KEY={generated}`.

- [ ] **Step 7: Переименовать test-only env (без fallback)**

```bash
grep -rl 'HYDECLAW_\(PG_TEST_IMAGE\|MIGRATION_BUDGET_MS\|GEMINI_TEST\|OAUTH_CREDENTIALS_PATH\)' --include='*.rs' . \
  | xargs sed -i 's/HYDECLAW_PG_TEST_IMAGE/OPEX_PG_TEST_IMAGE/g; s/HYDECLAW_MIGRATION_BUDGET_MS/OPEX_MIGRATION_BUDGET_MS/g; s/HYDECLAW_GEMINI_TEST/OPEX_GEMINI_TEST/g; s/HYDECLAW_OAUTH_CREDENTIALS_PATH/OPEX_OAUTH_CREDENTIALS_PATH/g'
```

- [ ] **Step 8: Сборка + тесты + grep**

Run: `cargo check --all-targets && cargo test -p opex-core env_var`
Run: `grep -rn 'std::env::var("HYDECLAW_' --include='*.rs' .`
Expected: компиляция ок; grep — пусто (прямых чтений `HYDECLAW_` не осталось).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat: dual-read env (OPEX_* с fallback на HYDECLAW_*) + autogen .env на OPEX_*"
```

---

## Task 3: Dual-path загрузчик конфига + переименование файла

**Files:**
- Create: `crates/opex-core/src/util/config_path.rs`
- Modify: `crates/opex-core/src/main.rs:278,303`
- Modify: `crates/opex-memory-worker/src/main.rs:40`
- Rename: `config/hydeclaw.toml` → `config/opex.toml`
- Test: `crates/opex-core/src/util/config_path.rs`

**Interfaces:**
- Produces: `pub fn resolve_config_path() -> String` — возвращает `config/opex.toml` если файл существует, иначе `config/hydeclaw.toml`.

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/opex-core/src/util/config_path.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn falls_back_when_new_missing() {
        // chooses_existing: при наличии только legacy выбирается legacy
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("config/hydeclaw.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "x").unwrap();
        assert_eq!(
            resolve_config_path_in(dir.path()),
            "config/hydeclaw.toml"
        );
        std::fs::write(dir.path().join("config/opex.toml"), "y").unwrap();
        assert_eq!(resolve_config_path_in(dir.path()), "config/opex.toml");
    }
}
```

- [ ] **Step 2: Запустить — убедиться, что падает**

Run: `cargo test -p opex-core resolve_config_path -- --nocapture`
Expected: FAIL — функция не найдена.

- [ ] **Step 3: Реализовать**

```rust
// crates/opex-core/src/util/config_path.rs
//! Dual-path конфиг: предпочитает config/opex.toml, fallback config/hydeclaw.toml.
//! Fallback удаляется в PR3 после переименования файла на сервере.
use std::path::Path;

/// Резолвит путь конфига относительно текущей рабочей директории.
pub fn resolve_config_path() -> String {
    resolve_config_path_in(Path::new("."))
}

/// Тестируемое ядро: резолвит относительно `base`.
pub fn resolve_config_path_in(base: &Path) -> String {
    if base.join("config/opex.toml").exists() {
        "config/opex.toml".to_string()
    } else {
        "config/hydeclaw.toml".to_string()
    }
}
```

Добавить `pub mod config_path;` в `util/mod.rs`. Добавить `tempfile` в `[dev-dependencies]` крейта `opex-core` (если ещё нет).

- [ ] **Step 4: Запустить — убедиться, что проходит**

Run: `cargo test -p opex-core resolve_config_path -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Подключить в местах загрузки**

`crates/opex-core/src/main.rs:278`: `config::AppConfig::load("config/hydeclaw.toml")?` → `config::AppConfig::load(&crate::util::config_path::resolve_config_path())?`.
`crates/opex-core/src/main.rs:303`: строку `"config/hydeclaw.toml".to_string()` → `crate::util::config_path::resolve_config_path()`.
`crates/opex-memory-worker/src/main.rs:40`: дефолт-аргумент `unwrap_or("config/hydeclaw.toml".into())` → fallback-логика: если аргумент не задан, выбрать `config/opex.toml` при наличии, иначе `config/hydeclaw.toml`.

- [ ] **Step 6: Переименовать файл конфига**

```bash
git mv config/hydeclaw.toml config/opex.toml
```

- [ ] **Step 7: Сборка + проверка локального старта**

Run: `cargo check --all-targets`
Expected: ок. (Локально `config/opex.toml` теперь существует → выбирается он.)

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: dual-path загрузчик конфига + переименование config/opex.toml"
```

---

## Task 4: Миграция `051` + код-`LIKE` для ephemeral-тега

PR1 НЕ редактирует старые миграции. Новая `051` (новая версия) применяется чисто на любой БД и переписывает ephemeral-комментарии на `@opex:ephemeral`. Код-`LIKE` синхронно переключается на новый префикс.

**Files:**
- Create: `migrations/051_rename_ephemeral_tag.sql`
- Modify: `crates/opex-core/src/gateway/handlers/backup.rs:89,102`
- Test: `crates/opex-core/tests/integration_backup_size_cap.rs` (рядом есть DB-тесты бэкапа) — добавить проверку discovery по `@opex:ephemeral`

**Interfaces:**
- Produces: все ephemeral-таблицы имеют `COMMENT … IS '@opex:ephemeral …'`; backup discovery ищет `LIKE '@opex:ephemeral%'`.

- [ ] **Step 1: Написать миграцию 051**

```sql
-- migrations/051_rename_ephemeral_tag.sql
-- Переименование функционального тега @hydeclaw:ephemeral -> @opex:ephemeral.
-- Идемпотентно переустанавливает COMMENT ON TABLE для всех ephemeral-таблиц
-- (см. m030 + m050). Старые миграции не редактируются (checksum-safety).
DO $$
DECLARE
    t text;
    tables text[] := ARRAY[
        'sessions','messages','session_events','session_timeline','usage_log',
        'audit_log','audit_events','notifications','pending_approvals',
        'pending_messages','outbound_queue','memory_tasks','pairing_codes',
        'cron_runs','tool_execution_cache','stream_jobs',
        'graph_extraction_queue','tasks','task_steps'
    ];
    cur text;
BEGIN
    FOREACH t IN ARRAY tables LOOP
        IF to_regclass('public.' || t) IS NOT NULL THEN
            cur := obj_description(('public.' || t)::regclass, 'pg_class');
            IF cur IS NOT NULL AND cur LIKE '@hydeclaw:ephemeral%' THEN
                EXECUTE format(
                    'COMMENT ON TABLE public.%I IS %L',
                    t,
                    '@opex:ephemeral' || substring(cur from length('@hydeclaw:ephemeral') + 1)
                );
            END IF;
        END IF;
    END LOOP;
END $$;
```

- [ ] **Step 2: Поменять код discovery**

`backup.rs:89` (doc-comment) и `:102`:

```rust
//  AND d.description LIKE '@opex:ephemeral%' \
```

```bash
sed -i "s/@hydeclaw:ephemeral/@opex:ephemeral/g" crates/opex-core/src/gateway/handlers/backup.rs
```

- [ ] **Step 3: Написать/обновить DB-тест discovery**

Добавить тест (запускается при наличии `DATABASE_URL`), проверяющий, что после миграций функция discovery возвращает помеченные таблицы и что их комментарии начинаются с `@opex:ephemeral`. Минимум — SQL-проверка:

```rust
// в integration_backup_size_cap.rs (или новый integration_ephemeral_tag.rs), #[sqlx::test]
#[sqlx::test(migrations = "../../migrations")]
async fn ephemeral_tag_is_opex(pool: sqlx::PgPool) {
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM pg_description d \
         JOIN pg_class c ON d.objoid=c.oid AND d.objsubid=0 \
         WHERE d.description LIKE '@opex:ephemeral%'",
    ).fetch_one(&pool).await.unwrap();
    assert!(row.0 > 0, "должны быть таблицы с тегом @opex:ephemeral");
    let legacy: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM pg_description WHERE description LIKE '@hydeclaw:ephemeral%'",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(legacy.0, 0, "не должно остаться тега @hydeclaw:ephemeral");
}
```

- [ ] **Step 4: Прогнать DB-тест**

Run: `make test-db` (или `DATABASE_URL=… cargo test -p opex-core ephemeral_tag_is_opex`)
Expected: PASS — есть `@opex:ephemeral`, нет `@hydeclaw:ephemeral`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(db): миграция 051 — тег @opex:ephemeral + код-LIKE"
```

---

## Task 5: Хелпер реконсиляции чексумм `opex-migrate-checksums`

Бинарник вычисляет sqlx-чексуммы файлов миграций и печатает SQL `UPDATE _sqlx_migrations …`. Нужен в PR2 (после редактирования старых миграций), но компилируется и тестируется в PR1.

**Files:**
- Create: `crates/opex-migrate-checksums/Cargo.toml`, `crates/opex-migrate-checksums/src/main.rs`
- Modify: `Cargo.toml` (добавить в `members`)

**Interfaces:**
- Produces: бинарник, печатающий по строке на миграцию: `UPDATE _sqlx_migrations SET checksum = decode('<hex>','hex') WHERE version = <v>;`

- [ ] **Step 1: Создать крейт**

```toml
# crates/opex-migrate-checksums/Cargo.toml
[package]
name = "opex-migrate-checksums"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
sqlx = { workspace = true }
```

Добавить `"crates/opex-migrate-checksums"` в `Cargo.toml` `members`.

- [ ] **Step 2: Реализовать**

```rust
// crates/opex-migrate-checksums/src/main.rs
//! Печатает UPDATE _sqlx_migrations с актуальными чексуммами файлов миграций.
//! Используется один раз в PR2 после правки комментариев старых миграций,
//! чтобы живая БД приняла изменённые (только комментарии) файлы.
use sqlx::migrate::Migrator;
use std::path::Path;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "migrations".into());
    let m = Migrator::new(Path::new(&dir)).await?;
    for mig in m.iter() {
        let hex: String = mig.checksum.iter().map(|b| format!("{b:02x}")).collect();
        println!(
            "UPDATE _sqlx_migrations SET checksum = decode('{hex}','hex') WHERE version = {};",
            mig.version
        );
    }
    Ok(())
}
```

- [ ] **Step 3: Собрать и прогнать вручную**

Run: `cargo run -p opex-migrate-checksums -- migrations | head -5`
Expected: печатаются строки `UPDATE _sqlx_migrations SET checksum = decode('…','hex') WHERE version = 1;` и т.д. Чексуммы совпадают с тем, что эмбеддит `opex-core` (тот же `Migrator`).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: opex-migrate-checksums — реконсиляция чексумм миграций для PR2"
```

---

## Task 6: Бренд-тексты (Rust + UI + scaffold + skills) + mDNS

Массовая замена видимого текста на `OPEX`/`opex`, КРОМЕ уже обработанных мест (env-чтение, конфиг-путь, миграции) и КРОМЕ старых файлов миграций.

**Files:**
- Modify: `.rs`/`.ts`/`.tsx`/`.md`/`.yaml` бренд-строки; `gateway/handlers/network.rs:189` (mDNS); scaffold `crates/opex-core/scaffold/**`; `config/skills/*`, `workspace/skills/*`.
- Update: insta-снапшоты (контент).

- [ ] **Step 1: Заменить mDNS-хостнейм**

```bash
sed -i 's/hydeclaw\.local/opex.local/g' crates/opex-core/src/gateway/handlers/network.rs
```

- [ ] **Step 2: Массовая замена бренда в исходниках (без миграций, без .env-чтения)**

```bash
# Rust/TS/TSX: оставшиеся текстовые "hydeclaw"/"HydeClaw" (идентификаторы крейтов уже не содержат hydeclaw)
grep -rl 'HydeClaw' --include='*.rs' --include='*.ts' --include='*.tsx' . | xargs sed -i 's/HydeClaw/OPEX/g'
# Прочие нижнерегистровые текстовые упоминания в коде (комментарии/строки), исключая migrations/
grep -rl 'hydeclaw' --include='*.rs' --include='*.ts' --include='*.tsx' . | xargs sed -i 's/hydeclaw/opex/g'
```

- [ ] **Step 3: Scaffold и skills**

```bash
grep -rl 'HydeClaw' crates/opex-core/scaffold config/skills workspace/skills | xargs sed -i 's/HydeClaw/OPEX/g'
grep -rl 'hydeclaw' crates/opex-core/scaffold config/skills workspace/skills | xargs sed -i 's/hydeclaw/opex/g'
```

- [ ] **Step 4: Обновить snapshot-контент**

Run: `cargo insta test -p opex-core --review` затем принять корректные изменения (`a`), либо `cargo insta accept -p opex-core` после ревью diff.
Expected: снапшоты обновлены, в них больше нет `hydeclaw`.

- [ ] **Step 5: Сборка + тесты + grep (вне миграций и deploy-конфигов)**

Run: `cargo check --all-targets && cd ui && npm run build && cd ..`
Run: `grep -rn 'hydeclaw' --include='*.rs' --include='*.ts' --include='*.tsx' . | grep -v migrations`
Expected: сборка ок; grep — пусто (остатки только в migrations/* и deploy-конфигах, их чистит PR2/таски ниже).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: бренд-тексты HydeClaw->OPEX (код, scaffold, skills, mDNS, снапшоты)"
```

---

## Task 7: UI — дефолтный язык `ru` + localStorage-префикс + shim

**Files:**
- Modify: `ui/src/stores/language-store.ts` (`locale: 'en'`→`'ru'`, persist `name`)
- Modify: `ui/src/stores/auth-store.ts` (`hydeclaw.auth.token`, `hydeclaw-rq`)
- Modify: `ui/src/stores/chat-persistence.ts`, `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx`, `ui/src/app/setup/page.tsx`, `ui/src/stores/stream/sse-debug.ts`
- Test: `ui/src/stores/__tests__/` (vitest)

**Interfaces:**
- Produces: localStorage-ключи с префиксом `opex.`; shim читает старый `hydeclaw.auth.token` / `hydeclaw.language` при отсутствии нового.

- [ ] **Step 1: Написать падающий vitest для shim**

```ts
// ui/src/stores/__tests__/ls-migration.test.ts
import { describe, it, expect, beforeEach } from "vitest";
import { readWithLegacy } from "@/stores/ls-migration";

describe("readWithLegacy", () => {
  beforeEach(() => localStorage.clear());
  it("returns new key when present", () => {
    localStorage.setItem("opex.auth.token", "new");
    localStorage.setItem("hydeclaw.auth.token", "old");
    expect(readWithLegacy("opex.auth.token", "hydeclaw.auth.token")).toBe("new");
  });
  it("falls back to legacy and migrates", () => {
    localStorage.setItem("hydeclaw.auth.token", "old");
    expect(readWithLegacy("opex.auth.token", "hydeclaw.auth.token")).toBe("old");
    expect(localStorage.getItem("opex.auth.token")).toBe("old"); // мигрирует
  });
});
```

- [ ] **Step 2: Запустить — убедиться, что падает**

Run: `cd ui && npx vitest run src/stores/__tests__/ls-migration.test.ts`
Expected: FAIL — модуль не найден.

- [ ] **Step 3: Реализовать shim**

```ts
// ui/src/stores/ls-migration.ts
/** Читает новый ключ; при отсутствии — legacy, и переносит его в новый. PR3 удаляет. */
export function readWithLegacy(newKey: string, legacyKey: string): string | null {
  const v = localStorage.getItem(newKey);
  if (v !== null) return v;
  const legacy = localStorage.getItem(legacyKey);
  if (legacy !== null) localStorage.setItem(newKey, legacy);
  return legacy;
}
```

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/stores/__tests__/ls-migration.test.ts`
Expected: PASS.

- [ ] **Step 5: Сменить дефолтный язык + персист-имя**

`ui/src/stores/language-store.ts`: `locale: "en"` → `locale: "ru"`; `{ name: "hydeclaw.language" }` → `{ name: "opex.language" }`. На инициализации стора прочитать legacy через `readWithLegacy` (или migrate-on-load).

- [ ] **Step 6: Переименовать остальные ключи**

```bash
cd "d:/GIT/bogdan/hydeclaw"
# auth.token и language — через shim в коде; остальные ключи (draft/lastSession/wizard/debug/IDB/event) — прямая замена
grep -rl 'hydeclaw' ui/src --include='*.ts' --include='*.tsx' | xargs sed -i 's/hydeclaw\.auth\.token/opex.auth.token/g; s/hydeclaw\.language/opex.language/g; s/hydeclaw\.draft\./opex.draft./g; s/hydeclaw\.chat\.lastSession/opex.chat.lastSession/g; s/hydeclaw\.lastSession/opex.lastSession/g; s/hydeclaw_wizard_progress/opex_wizard_progress/g; s/hydeclaw_debug_sse/opex_debug_sse/g; s/hydeclaw-rq/opex-rq/g; s/hydeclaw:stop-stream/opex:stop-stream/g'
```
Для `auth.token` обернуть чтение в `readWithLegacy("opex.auth.token","hydeclaw.auth.token")` в `auth-store.ts`.

- [ ] **Step 7: Build + тесты UI + grep**

Run: `cd ui && npm run build && npm test`
Run: `grep -rn 'hydeclaw' ui/src`
Expected: build/тесты зелёные; grep пусто.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(ui): дефолт ru + opex.* localStorage с shim для auth.token/language"
```

---

## Task 8: Версия, CI, скрипты сборки (крейт-таргеты)

**Files:**
- Modify: `Cargo.toml` (version), `ui/package.json`, `channels/package.json`
- Modify: `.github/workflows/{ci,release,integration}.yml`, `Makefile` (крейт-таргеты), `release.sh`/`setup.sh`/`update.sh`/`uninstall.sh`
- Modify: `channels/src/**`, `toolgate/**`, `docker/mcp/**` бренд-строки (текстовые)

- [ ] **Step 1: Бамп версии**

`Cargo.toml`: `version = "0.30.0"` → `"0.31.0"`. `ui/package.json` и `channels/package.json` — `"version": "0.31.0"`.

- [ ] **Step 2: CI/Makefile крейт-таргеты**

```bash
grep -rl 'hydeclaw-' .github/workflows Makefile | xargs sed -i 's/hydeclaw-/opex-/g'
```
В `Makefile` оставить серверные пути/юниты (`~/hydeclaw`, `hydeclaw-core.service`) — их меняет PR2. Проверить вручную diff, что заменены только `-p hydeclaw-*` и имена бинарников сборки, а не серверные пути.

- [ ] **Step 3: channels / toolgate / docker mcp — текстовый бренд (без deploy-инфры)**

```bash
grep -rl 'hydeclaw' channels/src toolgate docker/mcp --include='*.ts' --include='*.py' --include='*.json' --include='Dockerfile' \
  | xargs sed -i 's/hydeclaw/opex/g'
```
Не трогать `docker/docker-compose.yml`, `docker/*.service`, `docker/.env.example` — это PR2.

- [ ] **Step 4: Сборка всех компонентов**

Run: `cargo check --all-targets && cd ui && npm run build && cd ../channels && bun test && cd ..`
Expected: всё зелёное.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: версия 0.31.0 + крейт-таргеты CI/Makefile + бренд channels/toolgate/mcp"
```

---

## Task 9: README на русском + перевод верхнеуровневых docs

Контентная задача. «Тест» — ревью + наличие файлов и отсутствие английских заголовков верхнего уровня.

**Files:**
- Rename: `README.md` → `README.en.md`
- Create: `README.md` (русский)
- Modify (перевод на русский): `docs/API.md`, `docs/ARCHITECTURE.md`, `docs/CONFIGURATION.md`, `docs/DEPLOYMENT.md`, `docs/UPGRADE_NOTES.md`, `SECURITY.md`, `CONTRIBUTING.md`
- НЕ переводить (только имя `hydeclaw`→`opex`): `docs/architecture/*`, `docs/releases/*`

- [ ] **Step 1: Перенести английский README**

```bash
git mv README.md README.en.md
```

- [ ] **Step 2: Написать русский README.md**

Создать `README.md` на русском: заголовок `# OPEX`, подзаголовок (одна строка) с указанием русского прочтения «ОРЕХ», обновлённые ссылки/бейджи на `AronMav/opex`, ссылка на `README.en.md` («English version»). Сохранить структуру (Install / The Layers / Docs) переведённой.

- [ ] **Step 3: Перевести верхнеуровневые docs**

Для каждого файла из списка: перевести прозу на русский, сохранив кодовые блоки, команды, пути и идентификаторы как есть; заменить `hydeclaw`→`opex` в тексте/командах. Историю (`docs/architecture/*`, `docs/releases/*`) только просеять через `sed 's/HydeClaw/OPEX/g; s/hydeclaw/opex/g'`.

```bash
grep -rl 'hydeclaw' docs/architecture docs/releases | xargs sed -i 's/HydeClaw/OPEX/g; s/hydeclaw/opex/g'
```

- [ ] **Step 4: Verify**

Run: `ls README.md README.en.md && grep -rin 'hydeclaw' docs README.md README.en.md CONTRIBUTING.md SECURITY.md`
Expected: оба README существуют; grep — пусто.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: README на русском (+README.en.md), перевод верхнеуровневых docs, OPEX-бренд"
```

---

## Финальная проверка PR1

- [ ] `cargo check --all-targets` — ок
- [ ] `cargo clippy --all-targets -- -D warnings` — ок
- [ ] `cargo test` (+ `make test-db` при наличии Postgres) — зелёное
- [ ] `cd ui && npm run build && npm test` — зелёное
- [ ] `cd channels && bun test` — зелёное
- [ ] `grep -rin 'hydeclaw' . | grep -v -E 'migrations/0[0-4][0-9]_|migrations/050_|\.git/|target/|node_modules/'` — остаются только **старые файлы миграций** и **deploy-инфра** (`.deploy.env`, `deploy/server/*.service`, `docker/docker-compose.yml`, серверные пути в `Makefile`/скриптах) → это объём **PR2**.
- [ ] Деплой-репетиция (опц., если уместно): `make remote-deploy` — сервис поднимается на **старом** `.env`/конфиге/путях (доказательство dual-read/dual-path).

---

## Self-Review (выполнено)

- **Spec coverage:** крейты (T1), env dual-read (T2), конфиг dual-path (T3), миграция-тег (T4), checksum-helper для PR2 (T5), бренд+mDNS (T6), UI ru+localStorage (T7), версия+CI+скрипты+channels/toolgate (T8), README+docs (T9). Серверная инфра/БД/docker-compose/старые миграции — осознанно вне PR1 (PR2).
- **Placeholders:** нет TBD/TODO; код тестируемых юнитов приведён полностью; механические задачи имеют конкретные команды и grep-верификацию.
- **Type consistency:** `env_var(&str)->Option<String>`, `resolve_config_path()->String`/`resolve_config_path_in(&Path)->String`, `readWithLegacy(string,string)->string|null` — используются единообразно.
- **Деривации зафиксированы** в разделе «Отклонения от спеки».
