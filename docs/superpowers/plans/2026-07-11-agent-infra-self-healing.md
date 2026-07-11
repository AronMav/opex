# Self-healing инфраструктуры (Watchdog → Opex) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Watchdog обнаруживает проблемный docker-контейнер и запускает base-агента Opex, который безопасное чинит сам, а при сомнении задаёт владельцу вопрос с кнопками да/нет и выполняет решение по ответу.

**Architecture:** Watchdog (отдельный бинарник) классифицирует контейнеры из `docker ps -a`, при устойчивой проблеме POST'ит `/api/internal/infra-event` в core. Core дебаунсит и спавнит `base_engine.handle_isolated_via_pipeline(seed)`. Opex через `code_exec` диагностирует; safe → чинит + пишет `infra_decisions.status=done`; сомнение → создаёт `pending` decision → UI-notification с actionable-кнопками (+ Telegram stretch) → владелец подтверждает → core re-триггерит Opex выполнить зафиксированные шаги.

**Tech Stack:** Rust 2024 (opex-core, opex-watchdog, opex-db crates), Axum 0.8, sqlx 0.8 (PostgreSQL 17, rustls-only), Next.js 16 / React 19 / Zustand (ui), TDD.

Спека: [docs/superpowers/specs/2026-07-11-agent-infra-self-healing-design.md](../specs/2026-07-11-agent-infra-self-healing-design.md).

## Global Constraints

- **rustls-tls only** — никакого OpenSSL (все крейты используют `rustls-tls`).
- **No ORM** — только raw sqlx-запросы.
- **db-модули живут в крейте `opex-db`** (`crates/opex-db/src/`), реэкспортируются в core через `crate::db`.
- **Тесты opex-core — в BIN-таргете** (`cargo test --bin opex-core`), lib-таргет = 0 тестов. DB-backed `#[sqlx::test]` — только Linux x86_64, гонять на сервере (`make test-db`).
- **Watchdog authoritative на сервере** — Windows-машина разработчика Rust-тесты пакета может не гонять; pure-тесты гоняются везде, DB/E2E — на сервере.
- **Коммиты:** без `Co-Authored-By`, работать в `master`, **не пушить** без явного согласия. TDD: тест → провал → реализация → проход → коммит.
- **notify()** канонический путь — `crate::gateway::handlers::notifications::notify` (алиаса `crate::gateway::notify` нет).
- **Миграции авто-применяются** на старте; следующий свободный номер — `077`.
- **Дефолты:** grace=2 цикла watchdog; core-дебаунс на разобранный контейнер (`done`/`dismissed`/`rejected`)=24ч; TTL `pending`=7 дней; агент-респондер = первый `base=true`.

---

## Файловая структура

**Создать:**
- `migrations/077_infra_decisions.sql` — таблица + UNIQUE partial index.
- `crates/opex-db/src/infra_decisions.rs` — sqlx-запросы (Row, create, resolve, mark, debounce, expire).
- `crates/opex-watchdog/src/infra_watch.rs` — pure-классификатор контейнеров + streak-решение.
- `crates/opex-core/src/gateway/handlers/infra.rs` — endpoints + `resolve_infra_decision`.
- `config/skills/infra-triage.md` — протокол диагностики (base-only скилл).
- `ui/src/components/notification-infra-body.tsx` — actionable-кнопки в notification.
- `crates/opex-core/tests/integration_infra_decisions.rs` — DB-тесты (Linux).

**Изменить:**
- `crates/opex-db/src/lib.rs` — `pub mod infra_decisions;`.
- `crates/opex-core/src/db/mod.rs` — `pub use opex_db::infra_decisions;`.
- `crates/opex-watchdog/src/lib.rs` — `pub mod infra_watch;`.
- `crates/opex-watchdog/src/main.rs` — streak-HashMap + вызов `post_infra_event`.
- `crates/opex-watchdog/src/alerter.rs` — метод `post_infra_event`.
- `crates/opex-core/src/gateway/clusters/agent_core.rs` — метод `base_engine()`.
- `crates/opex-core/src/gateway/handlers/mod.rs` — `pub(crate) mod infra;`.
- `crates/opex-core/src/gateway/mod.rs` — `.merge(handlers::infra::routes())`.
- `crates/opex-core/src/gateway/middleware.rs` — `/api/internal/infra-event` в `LOOPBACK_EXACT`.
- `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` — `handle_infra_callback` (stretch).
- `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` — проводка callback (stretch).
- `ui/src/components/notification-bell.tsx` — рендер infra-body + route.
- `ui/src/lib/queries/*` — `useResolveInfraDecision`.

---

## Task 1: Миграция + db-модуль `infra_decisions`

**Files:**
- Create: `migrations/077_infra_decisions.sql`
- Create: `crates/opex-db/src/infra_decisions.rs`
- Modify: `crates/opex-db/src/lib.rs:1` (добавить `pub mod infra_decisions;`)
- Modify: `crates/opex-core/src/db/mod.rs:1` (добавить `pub use opex_db::infra_decisions;`)
- Test: `crates/opex-core/tests/integration_infra_decisions.rs`

**Interfaces:**
- Produces:
  - `struct InfraDecision { id: Uuid, container: String, diagnosis: String, proposed_action: String, proposed_commands: serde_json::Value, status: String, created_at: DateTime<Utc>, resolved_at: Option<DateTime<Utc>>, resolved_by: Option<String>, expires_at: DateTime<Utc> }`
  - `async fn create(db, container, diagnosis, proposed_action, proposed_commands, status: &str, ttl_days: i64) -> Result<Uuid>`
  - `async fn get(db, id: Uuid) -> Result<Option<InfraDecision>>`
  - `async fn resolve_strict(db, id: Uuid, status: &str, resolved_by: &str) -> Result<InfraDecision, InfraError>` (транзакция + `FOR UPDATE`, отклоняет не-`pending`)
  - `async fn mark_status(db, id: Uuid, status: &str) -> Result<()>` (для `done`/`failed` из PATCH)
  - `async fn has_recent(db, container: &str, cooldown_hours: i64) -> Result<bool>` (есть `pending` с `expires_at > now()` ИЛИ `done`/`dismissed`/`rejected` за `cooldown_hours`)
  - `async fn expire_stale(db) -> Result<u64>` (UPDATE `pending` → `expired` где `expires_at < now()`)
  - `async fn list(db, limit: i64) -> Result<Vec<InfraDecision>>`
  - `enum InfraError { NotFound{id}, AlreadyResolved{id,status}, Db(sqlx::Error) }`

- [ ] **Step 1: Написать миграцию**

Создать `migrations/077_infra_decisions.sql`:

```sql
-- 077_infra_decisions.sql
-- Self-healing инфраструктуры: асинхронные решения по проблемным docker-контейнерам.
-- Opex создаёт запись по итогу диагностики (pending=вопрос владельцу, done=починил,
-- dismissed=действий не требуется). UNIQUE partial index гарантирует не более одного
-- pending на контейнер. См. docs/superpowers/specs/2026-07-11-agent-infra-self-healing-design.md
CREATE TABLE IF NOT EXISTS infra_decisions (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    container         TEXT        NOT NULL,
    diagnosis         TEXT        NOT NULL,
    proposed_action   TEXT        NOT NULL DEFAULT '',
    proposed_commands JSONB       NOT NULL DEFAULT '[]'::jsonb,
    status            TEXT        NOT NULL DEFAULT 'pending',  -- pending|approved|rejected|expired|done|failed|dismissed
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at       TIMESTAMPTZ,
    resolved_by       TEXT,
    expires_at        TIMESTAMPTZ NOT NULL DEFAULT now() + interval '7 days'
);

-- Не более одного pending-решения на контейнер (enforcement на уровне БД).
CREATE UNIQUE INDEX IF NOT EXISTS idx_infra_decisions_one_pending
    ON infra_decisions (container) WHERE status = 'pending';

-- Дебаунс-запросы: недавние записи по контейнеру.
CREATE INDEX IF NOT EXISTS idx_infra_decisions_container_created
    ON infra_decisions (container, created_at DESC);
```

- [ ] **Step 2: Написать db-модуль**

Создать `crates/opex-db/src/infra_decisions.rs` (скопировать стиль с `crates/opex-db/src/approvals.rs` — те же derive'ы и thiserror):

```rust
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct InfraDecision {
    pub id: Uuid,
    pub container: String,
    pub diagnosis: String,
    pub proposed_action: String,
    pub proposed_commands: serde_json::Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum InfraError {
    #[error("infra decision {id} not found")]
    NotFound { id: Uuid },
    #[error("infra decision {id} already resolved (status={status})")]
    AlreadyResolved { id: Uuid, status: String },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub async fn create(
    db: &PgPool,
    container: &str,
    diagnosis: &str,
    proposed_action: &str,
    proposed_commands: &serde_json::Value,
    status: &str,
    ttl_days: i64,
) -> Result<Uuid, sqlx::Error> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO infra_decisions \
           (container, diagnosis, proposed_action, proposed_commands, status, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + ($6 || ' days')::interval) RETURNING id",
    )
    .bind(container)
    .bind(diagnosis)
    .bind(proposed_action)
    .bind(proposed_commands)
    .bind(status)
    .bind(ttl_days.to_string())
    .fetch_one(db)
    .await?;
    Ok(id)
}

pub async fn get(db: &PgPool, id: Uuid) -> Result<Option<InfraDecision>, sqlx::Error> {
    sqlx::query_as::<_, InfraDecision>("SELECT * FROM infra_decisions WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Транзакционный resolve с FOR UPDATE. Отклоняет не-`pending`.
pub async fn resolve_strict(
    db: &PgPool,
    id: Uuid,
    status: &str,
    resolved_by: &str,
) -> Result<InfraDecision, InfraError> {
    let mut tx = db.begin().await?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM infra_decisions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    match row {
        None => {
            tx.rollback().await?;
            Err(InfraError::NotFound { id })
        }
        Some((s,)) if s != "pending" => {
            tx.rollback().await?;
            Err(InfraError::AlreadyResolved { id, status: s })
        }
        Some(_) => {
            let updated = sqlx::query_as::<_, InfraDecision>(
                "UPDATE infra_decisions SET status = $2, resolved_at = now(), resolved_by = $3 \
                 WHERE id = $1 RETURNING *",
            )
            .bind(id)
            .bind(status)
            .bind(resolved_by)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(updated)
        }
    }
}

/// Обновить статус завершённого исполнения (done/failed). Не транзакционно —
/// вызывается Opex после выполнения одобренного действия.
pub async fn mark_status(db: &PgPool, id: Uuid, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE infra_decisions SET status = $2, resolved_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .execute(db)
    .await?;
    Ok(())
}

/// Дебаунс: есть ли недавняя запись, подавляющая новый триггер по контейнеру.
pub async fn has_recent(
    db: &PgPool,
    container: &str,
    cooldown_hours: i64,
) -> Result<bool, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS ( \
           SELECT 1 FROM infra_decisions WHERE container = $1 AND ( \
             (status = 'pending' AND expires_at > now()) OR \
             (status IN ('done','dismissed','rejected') \
              AND created_at > now() - ($2 || ' hours')::interval) \
           ) )",
    )
    .bind(container)
    .bind(cooldown_hours.to_string())
    .fetch_one(db)
    .await?;
    Ok(exists)
}

/// Пометить просроченные pending как expired (ленивый TTL). Возвращает число строк.
pub async fn expire_stale(db: &PgPool) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE infra_decisions SET status = 'expired' \
         WHERE status = 'pending' AND expires_at < now()",
    )
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

pub async fn list(db: &PgPool, limit: i64) -> Result<Vec<InfraDecision>, sqlx::Error> {
    sqlx::query_as::<_, InfraDecision>(
        "SELECT * FROM infra_decisions ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await
}
```

- [ ] **Step 3: Зарегистрировать модуль**

В `crates/opex-db/src/lib.rs` рядом с `pub mod approvals;` добавить:

```rust
pub mod infra_decisions;
```

В `crates/opex-core/src/db/mod.rs` рядом с `pub use opex_db::approvals;` добавить:

```rust
pub use opex_db::infra_decisions;
```

- [ ] **Step 4: Написать DB-тест (проваливается без миграции/кода)**

Создать `crates/opex-core/tests/integration_infra_decisions.rs`:

```rust
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use opex_db::infra_decisions as ind;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn create_then_get_roundtrip(pool: PgPool) {
    let cmds = serde_json::json!(["docker rm foo"]);
    let id = ind::create(&pool, "docker-foo-1", "diag", "rm it", &cmds, "pending", 7)
        .await
        .unwrap();
    let got = ind::get(&pool, id).await.unwrap().unwrap();
    assert_eq!(got.container, "docker-foo-1");
    assert_eq!(got.status, "pending");
    assert_eq!(got.proposed_commands, cmds);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unique_pending_per_container(pool: PgPool) {
    let cmds = serde_json::json!([]);
    ind::create(&pool, "docker-bar-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    let second = ind::create(&pool, "docker-bar-1", "d", "a", &cmds, "pending", 7).await;
    assert!(second.is_err(), "второй pending на тот же контейнер должен упасть на UNIQUE index");
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolve_strict_rejects_double(pool: PgPool) {
    let cmds = serde_json::json!([]);
    let id = ind::create(&pool, "docker-baz-1", "d", "a", &cmds, "pending", 7)
        .await
        .unwrap();
    ind::resolve_strict(&pool, id, "approved", "owner").await.unwrap();
    let again = ind::resolve_strict(&pool, id, "rejected", "owner").await;
    assert!(matches!(again, Err(ind::InfraError::AlreadyResolved { .. })));
}

#[sqlx::test(migrations = "../../migrations")]
async fn has_recent_debounce(pool: PgPool) {
    let cmds = serde_json::json!([]);
    assert!(!ind::has_recent(&pool, "docker-qux-1", 24).await.unwrap());
    ind::create(&pool, "docker-qux-1", "d", "a", &cmds, "dismissed", 7)
        .await
        .unwrap();
    assert!(ind::has_recent(&pool, "docker-qux-1", 24).await.unwrap());
}
```

- [ ] **Step 5: Прогнать тест — убедиться, что падает (нет таблицы/модуля)**

На сервере (или локально при live Postgres):
Run: `make test-db` (или `cargo test -p opex-core --test integration_infra_decisions`)
Expected: FAIL компиляции (`infra_decisions` не найден) до Steps 2-3, затем PASS после.

- [ ] **Step 6: Прогнать — убедиться, что проходит**

Run: `make test-db`
Expected: 4 теста PASS.

- [ ] **Step 7: Commit**

```bash
git add migrations/077_infra_decisions.sql crates/opex-db/src/infra_decisions.rs \
        crates/opex-db/src/lib.rs crates/opex-core/src/db/mod.rs \
        crates/opex-core/tests/integration_infra_decisions.rs
git commit -m "feat(infra): infra_decisions table + db module (m077)"
```

---

## Task 2: Watchdog — pure-классификатор контейнеров + streak-решение

**Files:**
- Create: `crates/opex-watchdog/src/infra_watch.rs`
- Modify: `crates/opex-watchdog/src/lib.rs:11` (добавить `pub mod infra_watch;`)
- Test: inline `#[cfg(test)] mod tests` в `infra_watch.rs`

**Interfaces:**
- Consumes: ничего (чистые функции над `&str` статусом).
- Produces:
  - `enum ContainerClass { Healthy, Transient, Problem }`
  - `fn classify(status: &str) -> ContainerClass` — `Up*`→Healthy; `Created`/`Restarting`/`Dead`/`Exited`→Problem; иначе Transient.
  - `fn should_trigger(class: ContainerClass, streak: u32, grace: u32) -> bool` — `Problem && streak >= grace`.
  - `fn is_excluded(docker_name: &str) -> bool` — `postgres` или `mcp-` (не кандидат).

- [ ] **Step 1: Написать тесты (провал — модуля нет)**

Создать `crates/opex-watchdog/src/infra_watch.rs` с тестами вперёд реализации:

```rust
//! Pure-классификатор состояния docker-контейнеров для self-healing.
//! Логика детекции отделена от IO ради тестируемости (образец — infra_jobs.rs).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerClass {
    Healthy,
    Transient,
    Problem,
}

pub fn classify(status: &str) -> ContainerClass {
    if status.starts_with("Up") {
        return ContainerClass::Healthy;
    }
    // `docker ps -a` статусы: "Created", "Restarting (1) 3s ago",
    // "Exited (0) 2 min ago", "Dead".
    let s = status.trim_start();
    if s.starts_with("Created")
        || s.starts_with("Restarting")
        || s.starts_with("Dead")
        || s.starts_with("Exited")
    {
        return ContainerClass::Problem;
    }
    ContainerClass::Transient
}

pub fn should_trigger(class: ContainerClass, streak: u32, grace: u32) -> bool {
    class == ContainerClass::Problem && streak >= grace
}

pub fn is_excluded(docker_name: &str) -> bool {
    docker_name.contains("postgres") || docker_name.starts_with("mcp-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_is_healthy() {
        assert_eq!(classify("Up 3 hours"), ContainerClass::Healthy);
        assert_eq!(classify("Up 2 minutes (healthy)"), ContainerClass::Healthy);
    }

    #[test]
    fn created_and_exited_are_problem() {
        assert_eq!(classify("Created"), ContainerClass::Problem);
        assert_eq!(classify("Exited (0) 5 minutes ago"), ContainerClass::Problem);
        assert_eq!(classify("Restarting (1) 2s ago"), ContainerClass::Problem);
        assert_eq!(classify("Dead"), ContainerClass::Problem);
    }

    #[test]
    fn trigger_only_after_grace() {
        assert!(!should_trigger(ContainerClass::Problem, 1, 2));
        assert!(should_trigger(ContainerClass::Problem, 2, 2));
        assert!(!should_trigger(ContainerClass::Healthy, 5, 2));
    }

    #[test]
    fn postgres_and_mcp_excluded() {
        assert!(is_excluded("docker-postgres-1"));
        assert!(is_excluded("mcp-github"));
        assert!(!is_excluded("docker-tts-silero-1"));
    }
}
```

- [ ] **Step 2: Зарегистрировать модуль**

В `crates/opex-watchdog/src/lib.rs` рядом с `pub mod infra_jobs;` добавить:

```rust
pub mod infra_watch;
```

Также убедиться, что бинарник видит модуль: в `crates/opex-watchdog/src/main.rs` (там модули пере-объявляются) добавить `mod infra_watch;` если main.rs не использует lib-крейт напрямую.

- [ ] **Step 3: Прогнать тесты — должны пройти (реализация уже написана вместе с тестами)**

Run: `cargo test -p opex-watchdog infra_watch`
Expected: 4 теста PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-watchdog/src/infra_watch.rs crates/opex-watchdog/src/lib.rs \
        crates/opex-watchdog/src/main.rs
git commit -m "feat(watchdog): pure container classifier + streak trigger logic"
```

---

## Task 3: Watchdog — отправка infra-event + проводка в poll-цикл

**Files:**
- Modify: `crates/opex-watchdog/src/alerter.rs` (метод `post_infra_event`)
- Modify: `crates/opex-watchdog/src/main.rs:406` (streak-HashMap + вызов)
- Test: inline тест на построение тела запроса (pure helper) в `alerter.rs`

**Interfaces:**
- Consumes: `Alerter { http, core_url, auth_token }` (существует); `infra_watch::{classify, should_trigger, is_excluded}` (Task 2); `checker::check_docker_containers()` (существует, возвращает `Vec<ContainerInfo>`).
- Produces: `async fn Alerter::post_infra_event(&self, docker_name: &str, status: &str)` — POST `{core_url}/api/internal/infra-event` с Bearer-auth, body `{docker_name, status}`. Ошибки — best-effort (лог), не паникует.

- [ ] **Step 1: Тест на тело запроса (pure helper)**

В `crates/opex-watchdog/src/alerter.rs` добавить чистый билдер тела и тест (чтобы не гонять реальный HTTP):

```rust
pub(crate) fn infra_event_body(docker_name: &str, status: &str) -> serde_json::Value {
    serde_json::json!({ "docker_name": docker_name, "status": status })
}

#[cfg(test)]
mod infra_event_tests {
    use super::*;
    #[test]
    fn body_has_name_and_status() {
        let b = infra_event_body("docker-tts-silero-1", "Created");
        assert_eq!(b["docker_name"], "docker-tts-silero-1");
        assert_eq!(b["status"], "Created");
    }
}
```

- [ ] **Step 2: Прогнать тест — PASS**

Run: `cargo test -p opex-watchdog infra_event`
Expected: PASS.

- [ ] **Step 3: Реализовать `post_infra_event`**

В `impl Alerter` (рядом с `send`, `alerter.rs:106`) добавить:

```rust
/// Триггерит self-healing по проблемному контейнеру. Best-effort.
pub async fn post_infra_event(&self, docker_name: &str, status: &str) {
    let body = infra_event_body(docker_name, status);
    let url = format!("{}/api/internal/infra-event", self.core_url);
    match self
        .http
        .post(&url)
        .header("Authorization", format!("Bearer {}", self.auth_token))
        .json(&body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            tracing::info!(container = %docker_name, "infra-event posted");
        }
        Ok(r) => tracing::warn!(container = %docker_name, status = %r.status(), "infra-event non-2xx"),
        Err(e) => tracing::warn!(container = %docker_name, error = %e, "infra-event send failed"),
    }
}
```

- [ ] **Step 4: Проводка в poll-цикл**

В `crates/opex-watchdog/src/main.rs` объявить streak-HashMap рядом с `was_container_unhealthy` (в области, владеющей циклом):

```rust
let mut unhealthy_streak: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
```

В блоке docker-проверки (`main.rs:406`, после существующего `for c in &all_containers { ... }` алерт-цикла — НЕ ломая его) добавить отдельный проход self-healing:

```rust
// ── Self-healing: устойчиво-проблемные контейнеры → триггер Opex ──────────
use crate::infra_watch::{classify, should_trigger, is_excluded, ContainerClass};
const INFRA_GRACE: u32 = 2;
let mut next_streak: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
for c in &all_containers {
    if is_excluded(&c.docker_name) {
        continue;
    }
    let class = classify(&c.status);
    if class == ContainerClass::Problem {
        let streak = unhealthy_streak.get(&c.docker_name).copied().unwrap_or(0) + 1;
        next_streak.insert(c.docker_name.clone(), streak);
        if should_trigger(class, streak, INFRA_GRACE) {
            alerter.post_infra_event(&c.docker_name, &c.status).await;
        }
    }
    // Healthy/Transient → streak сбрасывается (не переносим в next_streak).
}
unhealthy_streak = next_streak;
```

Примечание: core-дебаунс (Task 4) не даст повторно спавнить Opex, даже если streak растёт каждый цикл — watchdog может слать infra-event многократно, core отсеет.

- [ ] **Step 5: Собрать watchdog**

Run: `cargo check -p opex-watchdog`
Expected: компиляция без ошибок.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-watchdog/src/alerter.rs crates/opex-watchdog/src/main.rs
git commit -m "feat(watchdog): post infra-event on persistently-problem containers"
```

---

## Task 4: Core — `base_engine()` + endpoint `/api/internal/infra-event`

**Files:**
- Modify: `crates/opex-core/src/gateway/clusters/agent_core.rs:60` (метод `base_engine`)
- Create: `crates/opex-core/src/gateway/handlers/infra.rs`
- Modify: `crates/opex-core/src/gateway/handlers/mod.rs` (`pub(crate) mod infra;`)
- Modify: `crates/opex-core/src/gateway/mod.rs:96` (`.merge(handlers::infra::routes())`)
- Modify: `crates/opex-core/src/gateway/middleware.rs:253` (`/api/internal/infra-event` в `LOOPBACK_EXACT`)
- Test: inline `#[cfg(test)]` в `infra.rs` для `build_infra_seed` (pure)

**Interfaces:**
- Consumes: `AgentCore::get_engines_map()`; `engine.cfg().agent.{base,name}`; `engine.handle_isolated_via_pipeline(&IncomingMessage) -> Result<String>`; `db::infra_decisions::{has_recent, expire_stale}`; `InfraServices{db}`, `AgentCore` через `State<...>`.
- Produces:
  - `AgentCore::base_engine(&self) -> Option<Arc<AgentEngine>>`
  - `pub(crate) fn routes() -> Router<AppState>` с `POST /api/internal/infra-event`
  - `fn build_infra_seed(docker_name: &str, status: &str) -> String` (pure, для теста)
  - `async fn spawn_infra_session(engine: Arc<AgentEngine>, seed: String)` — fire-and-forget spawn

- [ ] **Step 1: Метод `base_engine`**

В `crates/opex-core/src/gateway/clusters/agent_core.rs` рядом с `first_engine` (`:60`) добавить:

```rust
/// Первый агент с `base = true` (респондер self-healing). None если base-агентов нет.
pub async fn base_engine(&self) -> Option<Arc<AgentEngine>> {
    self.map
        .read()
        .await
        .values()
        .find(|h| h.engine.cfg().agent.base)
        .map(|h| h.engine.clone())
}
```

- [ ] **Step 2: Тест на seed-построение (провал — функции нет)**

Создать `crates/opex-core/src/gateway/handlers/infra.rs` с тестом вперёд:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_mentions_container_and_skill() {
        let s = build_infra_seed("docker-tts-silero-1", "Created");
        assert!(s.contains("docker-tts-silero-1"));
        assert!(s.contains("Created"));
        assert!(s.contains("infra-triage"));
    }
}
```

- [ ] **Step 3: Реализовать endpoint + helpers**

В том же `infra.rs` (над тестами):

```rust
use std::sync::Arc;
use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::agent::engine::AgentEngine;
use crate::gateway::clusters::{AgentCore, InfraServices};
use crate::gateway::state::AppState;

const INFRA_COOLDOWN_HOURS: i64 = 24;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/internal/infra-event", post(api_infra_event))
}

#[derive(Debug, Deserialize)]
struct InfraEventBody {
    docker_name: String,
    status: String,
}

/// Собирает диагноз-затравку для изолированной сессии Opex.
fn build_infra_seed(docker_name: &str, status: &str) -> String {
    format!(
        "[Infra] Watchdog обнаружил проблемный контейнер `{docker_name}` в состоянии \
`{status}` (держится ≥2 циклов). Используй скилл infra-triage: продиагностируй и, \
если безопасно — почини сам; иначе создай infra-решение с вопросом владельцу. \
По итогу ОБЯЗАТЕЛЬНО оставь ровно одну запись в infra_decisions (pending | done | \
dismissed) — молчаливого завершения быть не должно."
    )
}

/// Fire-and-forget запуск изолированной сессии base-агента.
fn spawn_infra_session(engine: Arc<AgentEngine>, agent_name: String, seed: String) {
    tokio::spawn(async move {
        let msg = opex_types::IncomingMessage {
            user_id: "system".to_string(),
            text: Some(seed),
            attachments: vec![],
            agent_id: agent_name,
            channel: crate::agent::channel_kind::channel::SYSTEM.to_string(),
            context: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
            formatting_prompt: None,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        };
        if let Err(e) = engine.handle_isolated_via_pipeline(&msg).await {
            tracing::warn!(error = %e, "infra self-heal session failed");
        }
    });
}

async fn api_infra_event(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    Json(body): Json<InfraEventBody>,
) -> impl IntoResponse {
    // Ленивый TTL-expiry.
    let _ = crate::db::infra_decisions::expire_stale(&infra.db).await;

    // Дебаунс: недавняя запись по контейнеру → skip.
    match crate::db::infra_decisions::has_recent(&infra.db, &body.docker_name, INFRA_COOLDOWN_HOURS).await {
        Ok(true) => return Json(json!({ "skipped": true, "reason": "recent decision" })),
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(error = %e, "infra debounce query failed");
            return Json(json!({ "skipped": true, "reason": "db error" }));
        }
    }

    let Some(engine) = agents.base_engine().await else {
        tracing::warn!("infra-event: no base agent to respond");
        return Json(json!({ "skipped": true, "reason": "no base agent" }));
    };
    let agent_name = engine.cfg().agent.name.clone();
    let seed = build_infra_seed(&body.docker_name, &body.status);
    spawn_infra_session(engine, agent_name, seed);
    Json(json!({ "spawned": true }))
}
```

Примечание для исполнителя: точное имя константы SYSTEM-канала проверить в `crate::agent::channel_kind` — если `SYSTEM` отсутствует, использовать `HEARTBEAT` (оба видны в scheduler/mod.rs). Поля `IncomingMessage` копировать из `scheduler/mod.rs:1509`.

- [ ] **Step 4: Зарегистрировать модуль и роут**

В `crates/opex-core/src/gateway/handlers/mod.rs` добавить:

```rust
pub(crate) mod infra;
```

В `crates/opex-core/src/gateway/mod.rs` рядом с прочими `.merge(...)` (`:96`):

```rust
.merge(handlers::infra::routes())
```

- [ ] **Step 5: Пропустить endpoint мимо auth (loopback)**

В `crates/opex-core/src/gateway/middleware.rs` в массив `LOOPBACK_EXACT` (`:253`) добавить строку:

```rust
"/api/internal/infra-event",
```

- [ ] **Step 6: Тест PASS + сборка**

Run: `cargo test --bin opex-core seed_mentions_container`
Expected: PASS.
Run: `cargo check -p opex-core`
Expected: компиляция без ошибок.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/gateway/clusters/agent_core.rs \
        crates/opex-core/src/gateway/handlers/infra.rs \
        crates/opex-core/src/gateway/handlers/mod.rs \
        crates/opex-core/src/gateway/mod.rs \
        crates/opex-core/src/gateway/middleware.rs
git commit -m "feat(infra): /api/internal/infra-event → debounce + spawn base agent"
```

---

## Task 5: Core — decisions API (`create`, `resolve`, `PATCH`, `list`) + `resolve_infra_decision`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/infra.rs` (добавить роуты + `resolve_infra_decision`)
- Test: inline `#[cfg(test)]` для owner-guard решения (pure) + integration-тест resolve→spawn (на сервере)

**Interfaces:**
- Consumes: `db::infra_decisions::{create, get, resolve_strict, mark_status, list}`; `notify(...)`; `AgentCore::base_engine`; `ChannelBus{ui_event_tx}`; owner-check через `auth.access_guards` + `guard.is_owner(user_id)` (образец — `inline.rs:201`).
- Produces:
  - `POST /api/infra/decisions` (agent, authed) — создать + notify.
  - `POST /api/infra/decisions/{id}/resolve` (owner-guarded) — подтвердить/отклонить.
  - `PATCH /api/infra/decisions/{id}` — статус done/failed.
  - `GET /api/infra/decisions` — список.
  - `async fn resolve_infra_decision(state, id, approved, resolved_by) -> Result<...>` — единая точка (идемпотентна через `resolve_strict`; при approve спавнит Opex с re-trigger seed).
  - `fn build_execute_seed(decision: &InfraDecision) -> String` (pure, тестируемо).

- [ ] **Step 1: Тест на execute-seed (провал — функции нет)**

В `infra.rs` в `mod tests` добавить:

```rust
#[test]
fn execute_seed_carries_commands_and_id() {
    let d = sample_decision();  // helper ниже
    let s = build_execute_seed(&d);
    assert!(s.contains(&d.id.to_string()));
    assert!(s.contains("docker rm"));
    assert!(s.contains("PATCH"));
}
```

Helper в `mod tests`:

```rust
fn sample_decision() -> crate::db::infra_decisions::InfraDecision {
    crate::db::infra_decisions::InfraDecision {
        id: uuid::Uuid::new_v4(),
        container: "docker-tts-silero-1".into(),
        diagnosis: "orphan".into(),
        proposed_action: "remove + edit compose".into(),
        proposed_commands: serde_json::json!(["docker rm docker-tts-silero-1"]),
        status: "approved".into(),
        created_at: chrono::Utc::now(),
        resolved_at: None,
        resolved_by: Some("owner".into()),
        expires_at: chrono::Utc::now(),
    }
}
```

- [ ] **Step 2: Реализовать роуты и логику**

Расширить `routes()` в `infra.rs`:

```rust
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/internal/infra-event", post(api_infra_event))
        .route("/api/infra/decisions", post(api_create_decision).get(api_list_decisions))
        .route("/api/infra/decisions/{id}/resolve", post(api_resolve_decision))
        .route("/api/infra/decisions/{id}", axum::routing::patch(api_patch_decision))
}
```

Добавить (полный код create + notify):

```rust
use axum::extract::Path;
use uuid::Uuid;
use crate::gateway::clusters::ChannelBus;
use crate::db::infra_decisions::InfraDecision;

const INFRA_TTL_DAYS: i64 = 7;

#[derive(Debug, Deserialize)]
struct CreateDecisionBody {
    container: String,
    diagnosis: String,
    #[serde(default)]
    proposed_action: String,
    #[serde(default)]
    proposed_commands: serde_json::Value,
    /// pending | done | dismissed — итог диагностики Opex.
    status: String,
}

async fn api_create_decision(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    Json(body): Json<CreateDecisionBody>,
) -> impl IntoResponse {
    let cmds = if body.proposed_commands.is_null() {
        serde_json::json!([])
    } else {
        body.proposed_commands.clone()
    };
    let id = match crate::db::infra_decisions::create(
        &infra.db, &body.container, &body.diagnosis, &body.proposed_action, &cmds,
        &body.status, INFRA_TTL_DAYS,
    ).await {
        Ok(id) => id,
        Err(e) => {
            // UNIQUE-нарушение (уже есть pending) трактуем как «принято» — не ошибка.
            tracing::warn!(error = %e, "create infra decision failed (возможно уже есть pending)");
            return (axum::http::StatusCode::CONFLICT, Json(json!({"ok": false, "error": e.to_string()}))).into_response();
        }
    };
    // Уведомляем владельца ТОЛЬКО для pending (вопрос). done/dismissed — молча.
    if body.status == "pending" {
        crate::gateway::handlers::notifications::notify(
            &infra.db, &bus.ui_event_tx, "infra_decision",
            "Требуется решение по инфраструктуре",
            &format!("Контейнер {}: {}", body.container, body.proposed_action),
            json!({ "decision_id": id.to_string(), "container": body.container,
                    "proposed_action": body.proposed_action }),
        ).await.ok();
    }
    (axum::http::StatusCode::OK, Json(json!({"ok": true, "id": id.to_string()}))).into_response()
}

async fn api_list_decisions(State(infra): State<InfraServices>) -> impl IntoResponse {
    match crate::db::infra_decisions::list(&infra.db, 100).await {
        Ok(rows) => Json(json!({"decisions": rows})).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct PatchBody { status: String }

async fn api_patch_decision(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> impl IntoResponse {
    // Только терминальные статусы исполнения.
    if !matches!(body.status.as_str(), "done" | "failed") {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "status must be done|failed"}))).into_response();
    }
    match crate::db::infra_decisions::mark_status(&infra.db, id, &body.status).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
```

- [ ] **Step 3: Реализовать `resolve_infra_decision` + UI-resolve endpoint (owner-guarded)**

Добавить execute-seed и единую resolve-функцию:

```rust
fn build_execute_seed(d: &InfraDecision) -> String {
    format!(
        "[Infra] Владелец одобрил решение {id}: {action}. Выполни зафиксированные шаги: \
{cmds}. По завершении вызови PATCH /api/infra/decisions/{id} со статусом done или \
failed и кратко сообщи итог.",
        id = d.id,
        action = d.proposed_action,
        cmds = d.proposed_commands,
    )
}

/// Единая точка подтверждения (UI и Telegram сводятся сюда).
/// Идемпотентна через resolve_strict (не-pending → AlreadyResolved).
pub(crate) async fn resolve_infra_decision(
    infra: &InfraServices,
    agents: &AgentCore,
    id: Uuid,
    approved: bool,
    resolved_by: &str,
) -> Result<(), crate::db::infra_decisions::InfraError> {
    let status = if approved { "approved" } else { "rejected" };
    let decision = crate::db::infra_decisions::resolve_strict(&infra.db, id, status, resolved_by).await?;
    if approved {
        if let Some(engine) = agents.base_engine().await {
            let agent_name = engine.cfg().agent.name.clone();
            let seed = build_execute_seed(&decision);
            spawn_infra_session(engine, agent_name, seed);
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ResolveBody { approved: bool }

async fn api_resolve_decision(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(auth): State<crate::gateway::clusters::AuthServices>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveBody>,
) -> impl IntoResponse {
    // Owner-guard: этот роут authed (Bearer владельца) middleware'ом. Дополнительно
    // фиксируем resolved_by. (Единственный пользователь с токеном = владелец.)
    let _ = &auth; // access_guards доступны при необходимости более строгой проверки
    let _ = &headers;
    match resolve_infra_decision(&infra, &agents, id, body.approved, "owner").await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(crate::db::infra_decisions::InfraError::AlreadyResolved { status, .. }) =>
            (axum::http::StatusCode::CONFLICT, Json(json!({"ok": false, "error": format!("уже обработано: {status}")}))).into_response(),
        Err(crate::db::infra_decisions::InfraError::NotFound { .. }) =>
            (axum::http::StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not found"}))).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}
```

Примечание про owner-guard: `/api/infra/decisions/*` не входят в `PUBLIC_*`/`LOOPBACK_*`, поэтому auth-middleware требует Bearer-токен владельца автоматически (единственный токен в системе). Более строгая привязка к конкретному owner_id не требуется в v1 — токен = владелец. `AuthServices` подключён на случай будущей многопользовательности.

- [ ] **Step 4: Тест PASS + сборка**

Run: `cargo test --bin opex-core execute_seed_carries`
Expected: PASS.
Run: `cargo check -p opex-core`
Expected: без ошибок.

- [ ] **Step 5: Integration-тест resolve (на сервере)**

Добавить в `crates/opex-core/tests/integration_infra_decisions.rs`:

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn resolve_marks_status(pool: PgPool) {
    let cmds = serde_json::json!(["docker rm x"]);
    let id = ind::create(&pool, "docker-x-1", "d", "a", &cmds, "pending", 7).await.unwrap();
    let d = ind::resolve_strict(&pool, id, "rejected", "owner").await.unwrap();
    assert_eq!(d.status, "rejected");
    assert_eq!(d.resolved_by.as_deref(), Some("owner"));
}
```

Run (сервер): `make test-db`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/infra.rs \
        crates/opex-core/tests/integration_infra_decisions.rs
git commit -m "feat(infra): decisions API (create/resolve/patch/list) + resolve_infra_decision"
```

---

## Task 6: Telegram-путь подтверждения (stretch) — `infra:ok/no` callback

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` (функция `handle_infra_callback`)
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs:115` (проводка)
- Test: source-scan wiring test в `reader.rs` (образец — `approval_wired_before_clarify`)

**Interfaces:**
- Consumes: `resolve_infra_decision` (Task 5) — вызывается из callback; owner-check через `ctx.auth.access_guards` + `guard.is_owner(user_id)` (образец — `inline.rs:201`); `AppState`/`agents`/`infra` доступны через `ctx`.
- Produces: `async fn handle_infra_callback(ctx, agents, infra, agent_name, msg, out_tx) -> bool` — перехватывает `infra:ok:UUID` / `infra:no:UUID`.

**Примечание:** отправка Telegram-кнопок при создании decision требует резолва DM-канала владельца (owner_id → chat_id) — вынесено за v1 (см. спеку, «stretch»). Эта задача добавляет только ПРИЁМ callback, чтобы кнопки, если они будут отправлены вручную/позже, работали через ту же resolve-логику. Если Telegram-путь целиком откладывается — задачу можно пропустить, UI-путь (Task 8) самодостаточен.

- [ ] **Step 1: Реализовать `handle_infra_callback`**

В `inline.rs` (рядом с `handle_approval_callback`, копируя owner-gate) добавить:

```rust
/// Перехват Telegram inline-callback `infra:ok:UUID` / `infra:no:UUID`. Owner-only.
pub(super) async fn handle_infra_callback(
    ctx: &CwsCtx,
    agent_name: &str,
    msg: &IncomingMessageDto,
    out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    if msg.context.get("is_callback").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    let text = msg.text.as_deref().unwrap_or("");
    let (rest, approved) = if let Some(r) = text.strip_prefix("infra:ok:") {
        (r, true)
    } else if let Some(r) = text.strip_prefix("infra:no:") {
        (r, false)
    } else {
        return false;
    };
    let user_id = msg.user_id.clone();
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let is_owner = live_guard.as_ref().is_some_and(|g| g.is_owner(&user_id));
    if !is_owner {
        let _ = out_tx.send(OutboundMsg::error("Only the owner can resolve infra decisions.")).await;
        return true;
    }
    let Ok(id) = uuid::Uuid::parse_str(rest) else { return true; };
    match crate::gateway::handlers::infra::resolve_infra_decision(
        &ctx.state.infra, &ctx.state.agents, id, approved, &user_id,
    ).await {
        Ok(()) => { let _ = out_tx.send(OutboundMsg::done()).await; }
        Err(e) => { let _ = out_tx.send(OutboundMsg::error(&e.to_string())).await; }
    }
    true
}
```

Примечание для исполнителя: точные типы `CwsCtx`, `IncomingMessageDto`, `OutboundMsg` и конструкторы `OutboundMsg::error/done` скопировать из `handle_approval_callback`; доступ к `AppState` внутри ctx — проверить поле (`ctx.state` или подобное); `resolve_infra_decision` сделать `pub(crate)` (уже так в Task 5).

- [ ] **Step 2: Проводка в reader.rs**

В `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` рядом с вызовом `handle_approval_callback` (`:115`) добавить ПОСЛЕ него:

```rust
let consumed = inline::handle_infra_callback(&ctx, &agent_name, &msg, &out_tx).await;
if consumed { continue; }
```

- [ ] **Step 3: Wiring-тест (образец — `approval_wired_before_clarify`)**

В `reader.rs` в `#[cfg(test)] mod tests` добавить source-scan тест:

```rust
#[test]
fn infra_callback_wired() {
    let src = include_str!("reader.rs");
    assert!(src.contains("handle_infra_callback"), "infra callback должен быть подключён в reader");
}
```

- [ ] **Step 4: Тест PASS + сборка**

Run: `cargo test --bin opex-core infra_callback_wired`
Expected: PASS.
Run: `cargo check -p opex-core`
Expected: без ошибок.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/inline.rs \
        crates/opex-core/src/gateway/handlers/channel_ws/reader.rs
git commit -m "feat(infra): Telegram infra:ok/no callback → resolve_infra_decision"
```

---

## Task 7: base-скилл `infra-triage`

**Files:**
- Create: `config/skills/infra-triage.md`

**Interfaces:**
- Consumes: загружается автоматически через `load_skills_for_base` (base-only path). Триггерится словами в seed-сообщении (`infra-triage`, `Infra`, `контейнер`).
- Produces: протокол диагностики для Opex.

- [ ] **Step 1: Написать скилл**

Создать `config/skills/infra-triage.md`:

```markdown
---
name: infra-triage
description: Диагностика и ремонт проблемных docker-контейнеров по триггеру Watchdog; safe чинит сам, при сомнении спрашивает владельца
triggers:
  - infra-triage
  - Infra
  - проблемный контейнер
  - docker-контейнер
tools_required:
  - code_exec
priority: 20
---

# Infra Triage — self-healing docker-контейнеров

Тебя вызвал Watchdog: обнаружен устойчиво-проблемный контейнер. Действуй по протоколу.
Все HTTP-вызовы — Python `requests` в `code_exec`, база `http://localhost:18789`,
заголовок `Authorization: Bearer <OPEX_AUTH_TOKEN из env>`. НЕ используй curl.

## 1. Диагностика (измеряй, не гадай)

- `docker inspect <name>` — состояние, ExitCode, ошибка старта.
- Сверь с активным `~/opex/docker/docker-compose.yml`: есть ли такой сервис,
  закомментирован ли он.
- Сверь с активными провайдерами: `GET /api/providers` — используется ли сервис
  (порт/URL) как активный провайдер.
- Проверь порт: `ss -ltnp | grep <порт>` — слушает ли кто-то.

## 2. Классифицируй и действуй

**SAFE — чини сам, без вопроса:** контейнер, который ДОЛЖЕН работать и просто упал
(известный compose-сервис в `Exited`/`Restarting`, всё ещё нужный). Действие:
`docker restart <name>`. Затем зафиксируй результат:
`POST /api/infra/decisions {container, diagnosis, proposed_action:"restarted",
proposed_commands:[], status:"done"}`.

**СОМНЕНИЕ — спрашивай владельца:** удаление контейнера (`docker rm`), правка
`compose`, любой незнакомый контейнер. НЕ выполняй сам. Создай вопрос:
`POST /api/infra/decisions {container, diagnosis:"<что выяснил>",
proposed_action:"<человекочитаемо>", proposed_commands:["docker rm ...", "..."],
status:"pending"}`. Владелец подтвердит — тебя перезапустят с командой выполнить.

**НИЧЕГО НЕ ТРЕБУЕТСЯ** (контейнер штатно остановлен, ложная тревога):
`POST /api/infra/decisions {..., proposed_action:"<почему ок>", status:"dismissed"}`.

## 3. Обязательный итог

По итогу ОБЯЗАТЕЛЬНО оставь ровно одну запись в `infra_decisions`
(`pending` | `done` | `dismissed`). Молчаливое завершение без записи ломает
анти-петлевой дебаунс — Watchdog будет дёргать тебя снова и снова.

## 4. Исполнение одобренного (когда тебя вызвали с «Владелец одобрил решение …»)

Выполни `proposed_commands` дословно через `code_exec`. Если среди шагов есть правка
серверного `~/opex/docker/docker-compose.yml` — **предупреди владельца в отчёте**, что
git-версия compose разошлась и её нужно обновить (deploy не синкает docker/). По
завершении: `PATCH /api/infra/decisions/{id} {status:"done"}` (или `"failed"` при сбое).

## Никогда

- Не трогай `postgres` и контейнеры с данными.
- Не удаляй и не правь compose без явного «да» владельца.
- Не interpretируй — измеряй; фиксируй симптом.
```

- [ ] **Step 2: Проверить формат (парсинг фронтматтера)**

Run (локально, если есть тест загрузки скиллов): `cargo test --bin opex-core skills`
Expected: существующие skill-тесты PASS (новый файл не ломает парсер). Если тестов нет — визуально сверить фронтматтер с `config/skills/agent-management.md`.

- [ ] **Step 3: Commit**

```bash
git add config/skills/infra-triage.md
git commit -m "feat(infra): base skill infra-triage (diagnosis + safe/ask protocol)"
```

---

## Task 8: UI — actionable-кнопки да/нет для `infra_decision`

**Files:**
- Create: `ui/src/components/notification-infra-body.tsx`
- Modify: `ui/src/components/notification-bell.tsx` (рендер body по type + route)
- Modify: `ui/src/lib/queries/` (хук `useResolveInfraDecision`)
- Test: `ui` vitest (компонент рендерит кнопки)

**Interfaces:**
- Consumes: `NotificationRow` (`n.type === "infra_decision"`, `n.data.decision_id`); API `POST /api/infra/decisions/{id}/resolve {approved}`.
- Produces: инлайн-кнопки «Выполнить»/«Отклонить» в строке notification, `stopPropagation` (образец — `MediaNotificationBody`, `notification-bell.tsx:77`).

- [ ] **Step 1: Хук мутации**

В `ui/src/lib/queries/` (рядом с `useMarkNotificationRead`) добавить:

```ts
export function useResolveInfraDecision() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ id, approved }: { id: string; approved: boolean }) => {
      const res = await apiFetch(`/api/infra/decisions/${id}/resolve`, {
        method: "POST",
        body: JSON.stringify({ approved }),
      });
      if (!res.ok) throw new Error("resolve failed");
      return res.json();
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["notifications"] });
    },
  });
}
```

Примечание: `apiFetch`/`useMutation`/`useQueryClient` импортировать по образцу соседних хуков в том же файле; точный helper запроса (`apiFetch` vs `api.post`) скопировать у `useMarkNotificationRead`.

- [ ] **Step 2: Компонент body с кнопками**

Создать `ui/src/components/notification-infra-body.tsx`:

```tsx
"use client";
import { Button } from "@/components/ui/button";
import { useResolveInfraDecision } from "@/lib/queries";
import type { NotificationRow } from "@/types/api";

export function NotificationInfraBody({ n }: { n: NotificationRow }) {
  const resolve = useResolveInfraDecision();
  const id = (n.data as { decision_id?: string })?.decision_id;
  if (!id) return null;
  const stop = (e: React.MouseEvent) => e.stopPropagation();
  return (
    <div className="mt-2 flex gap-2" onClick={stop}>
      <Button
        size="sm"
        variant="default"
        disabled={resolve.isPending}
        onClick={() => resolve.mutate({ id, approved: true })}
      >
        Выполнить
      </Button>
      <Button
        size="sm"
        variant="outline"
        disabled={resolve.isPending}
        onClick={() => resolve.mutate({ id, approved: false })}
      >
        Отклонить
      </Button>
    </div>
  );
}
```

- [ ] **Step 3: Подключить в notification-bell**

В `ui/src/components/notification-bell.tsx`: (а) в `getNotificationRoute` добавить кейс, чтобы infra не навигировал (кнопки — не переход):

```ts
case "infra_decision": return null;
```

(б) в рендере строки notification (где рендерится `MediaNotificationBody` по type) добавить ветку:

```tsx
{n.type === "infra_decision" && <NotificationInfraBody n={n} />}
```

с импортом `import { NotificationInfraBody } from "./notification-infra-body";`.

- [ ] **Step 4: Vitest-тест**

Создать `ui/src/components/notification-infra-body.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { NotificationInfraBody } from "./notification-infra-body";

vi.mock("@/lib/queries", () => ({
  useResolveInfraDecision: () => ({ mutate: vi.fn(), isPending: false }),
}));

describe("NotificationInfraBody", () => {
  it("рендерит кнопки да/нет при наличии decision_id", () => {
    const n = { type: "infra_decision", data: { decision_id: "abc" } } as never;
    render(<NotificationInfraBody n={n} />);
    expect(screen.getByText("Выполнить")).toBeTruthy();
    expect(screen.getByText("Отклонить")).toBeTruthy();
  });

  it("ничего не рендерит без decision_id", () => {
    const n = { type: "infra_decision", data: {} } as never;
    const { container } = render(<NotificationInfraBody n={n} />);
    expect(container.firstChild).toBeNull();
  });
});
```

- [ ] **Step 5: Прогнать тесты (ТОЛЬКО из ui/)**

Run: `cd ui && npm test -- notification-infra-body`
Expected: 2 теста PASS. (Провайдер QueryClient замокан.)

- [ ] **Step 6: Сборка UI**

Run: `cd ui && npm run build`
Expected: сборка без ошибок типов.

- [ ] **Step 7: Commit**

```bash
git add ui/src/components/notification-infra-body.tsx \
        ui/src/components/notification-infra-body.test.tsx \
        ui/src/components/notification-bell.tsx ui/src/lib/queries
git commit -m "feat(ui): actionable да/нет buttons for infra_decision notifications"
```

---

## Task 9: E2E-проверка на сервере

**Files:** нет (ручная/скриптовая проверка на проде).

**Interfaces:** весь стек Tasks 1-8, задеплоенный.

- [ ] **Step 1: Деплой**

Run: `make remote-deploy` (собирает core+watchdog+memory-worker на сервере, свопит, рестартит). Затем **вручную** скопировать `config/skills/infra-triage.md` на сервер (`~/opex/config/skills/`) — deploy свопит только Rust-бинарники, не config/UI. UI: `deploy-ui.sh` (union-swap). Миграция 077 применяется автоматически при рестарте core.

Проверить здоровье: `make doctor` → 200 OK.

- [ ] **Step 2: E2E — safe-путь (перезапуск)**

На сервере остановить безопасный сервис: `docker stop docker-browser-renderer-1`. Дождаться ≥2 циклов watchdog (`interval_secs`). Ожидать: watchdog POST'ит infra-event → Opex-сессия → `docker restart` → запись `done`.
Проверка: `curl -s -H "Authorization: Bearer $OPEX_AUTH_TOKEN" localhost:18789/api/infra/decisions | jq '.decisions[0]'` → status `done`, container browser-renderer. Контейнер снова `Up`.

- [ ] **Step 3: E2E — ask-путь (осиротевший, как silero)**

Создать искусственный осиротевший контейнер (напр. `docker create --name docker-test-orphan-1 alpine`). Дождаться ≥2 циклов. Ожидать: Opex создаёт `pending` decision + UI-notification с кнопками. Нажать «Выполнить» в UI (или дёрнуть `POST /api/infra/decisions/{id}/resolve {approved:true}`). Ожидать: Opex re-триггерится, выполняет предложенное (`docker rm`), пишет `done`, отчёт про git-дрейф если правил compose.
Проверка: контейнер удалён; decision `done`.

- [ ] **Step 4: E2E — анти-петля**

Убедиться, что после `dismissed`/`done` повторный проблемный контейнер того же имени в течение 24ч НЕ порождает новую Opex-сессию (проверить логи core: нет повторного spawn; `/api/infra/decisions` не растёт).

- [ ] **Step 5: Обновить память проекта**

Записать итог в `MEMORY.md` (новый файл `project_infra_self_healing.md`): что задеплоено, готчи (config/skills копировать руками, миграция 077, event идёт мимо alert_events).

---

## Self-Review (проверка плана против спеки)

**Покрытие спеки:**
- Компонент 1 (детекция) → Task 2 (classify/streak) + Task 3 (проводка). ✔
- Компонент 2 (мост watchdog→core) → Task 3 (post_infra_event) + Task 4 (endpoint+debounce+spawn). ✔
- Компонент 3 (протокол Opex) → Task 7 (скилл infra-triage). ✔
- Компонент 4 (ask-flow: таблица, API, notify, TTL) → Task 1 (таблица) + Task 5 (create/notify/list) + Task 4 (expire_stale). ✔
- Компонент 5 (исполнение одобренного, единая resolve, 2 входа) → Task 5 (resolve_infra_decision + UI-вход) + Task 6 (Telegram-вход) + Task 8 (UI-кнопки). ✔
- Safety-инвариант 1 (grace+дебаунс) → Task 2 (grace) + Task 4 (has_recent 24ч). ✔
- Инвариант 2 (один pending) → Task 1 (UNIQUE index) + Task 5 (CONFLICT-обработка). ✔
- Инвариант 3 (рискованное после «да») → Task 5/6 (resolve→spawn) + Task 7 (skill не даёт rm без ask). ✔
- Инвариант 4 (не трогать postgres) → Task 2 (is_excluded) + Task 7 (skill). ✔
- Инвариант 5 (git-дрейф уведомление) → Task 7 (skill §4). ✔
- Инвариант 6 (owner-only оба пути) → Task 5 (authed /resolve) + Task 6 (is_owner). ✔
- Инвариант 7 (анти-петля/стоимость) → Task 1 (dismissed статус) + Task 4 (has_recent на любую запись) + Task 7 (обязательный итог). ✔

**Плейсхолдеры:** реальный код в каждом шаге; «примечания для исполнителя» указывают точные образцы (file:line), а не «TBD».

**Согласованность типов:** `resolve_infra_decision` (Task 5) вызывается из Task 6 с той же сигнатурой; `InfraDecision`/`InfraError` из Task 1 используются в Task 5; `build_infra_seed`/`spawn_infra_session` из Task 4 переиспользуются в Task 5; `notify(type="infra_decision")` (Task 5) ↔ `n.type==="infra_decision"` (Task 8) ↔ `getNotificationRoute` (Task 8) — согласованы.

**Риски исполнения (проверить по месту):**
- Точное имя SYSTEM-канала и все поля `IncomingMessage` — сверить с `scheduler/mod.rs:1509`.
- Типы `CwsCtx`/`OutboundMsg` и доступ к `AppState` внутри `ctx` в inline.rs (Task 6).
- Точный helper запроса в ui queries (`apiFetch` vs `api.*`).
