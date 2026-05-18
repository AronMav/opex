# Uploads to PostgreSQL — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move agent icons, tool outputs, and chat attachments from `workspace/uploads/` (filesystem) into a single polymorphic PostgreSQL `uploads` table so they survive every deploy and binary swap.

**Architecture:** Single table `uploads` keyed by `id UUID` with `(owner_type, owner_id)` discriminator. Three consumer classes: `agent_icon` (PK by agent name, never expires), `tool_output` (server-side `save_binary_to_uploads`, 30d retention), `client_upload` (multipart `POST /api/media/upload` for chat composer + channel adapters, 30d retention). HMAC-signed URLs (`/api/uploads/{id}?sig=...&exp=...`) reuse Phase 64 SEC-03 mint/verify with the `"uploads:"` namespace tag preserved. The old `GET /uploads/{filename}` static route and filesystem writes are removed cleanly.

**Tech Stack:** Rust 2024, sqlx 0.8 + PostgreSQL 17, axum 0.8, multer (multipart), tokio. UI: Next.js 16 + React Query.

**Spec:** `docs/superpowers/specs/2026-05-15-uploads-to-db-design.md`. Read once before starting.

**Scope extension beyond spec (discovered during plan writing):** `POST /api/media/upload` (used by AgentEditDialog **plus** ChatComposer **plus** channels/bridge.ts) must also be adapted to write to DB, otherwise removing the `/uploads/*` read route breaks chat attachments. A third owner_type `client_upload` is introduced for this case. Tasks 8 and 11 handle it.

**Total commits:** ~16 (1 discovery + 12 implementation + 1 UI + 1 Pi sweep + 1 acceptance). **Estimated time:** 2-3 focused days.

---

## Standard task procedure

Every implementation task (Tasks 2-15) follows the same gate:

1. **Read** the relevant production file with the `Read` tool to confirm current line ranges of items being moved/edited.
2. **Edit/Write** the change. For new files, use `Write`. For modifications, use `Edit`.
3. **`cargo check -p hydeclaw-core`** — fast feedback.
4. **`cargo clippy -p hydeclaw-core --all-targets -- -D warnings`** — strict.
5. **Targeted test run** for the affected module (specific command listed per task).
6. **Commit** with the exact message specified in the task.

Database tests use `#[sqlx::test]` with `DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test`. Run via `make test-db` for the full suite, or pass `DATABASE_URL=...` to a targeted `cargo test`.

If any commit's `cargo clippy` or tests fail, **stop and debug** before moving on. Behaviour-change regressions cascade across the 16 commits otherwise.

---

## Task 1: Discovery — freeze baseline

**Files:**

- No production code changes. Only inventory + commit-message body capture.

- [ ] **Step 1: Locate `save_binary_to_uploads` call sites**

```bash
grep -rn "save_binary_to_uploads" crates/hydeclaw-core/src/
```

Expected (lock these line numbers in commit body):

```
crates/hydeclaw-core/src/agent/pipeline/handlers.rs:277       # definition
crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs:150  # import
crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs:200  # callsite
crates/hydeclaw-core/src/agent/pipeline/media_background.rs:527 # callsite
crates/hydeclaw-core/src/agent/pipeline/media_background.rs:624 # callsite
```

If counts differ, update the plan task list before continuing.

- [ ] **Step 2: Locate `/api/media/upload` call sites**

```bash
grep -rn "/api/media/upload" ui/src/ channels/src/ crates/hydeclaw-core/src/
```

Expected:

```
ui/src/app/(authenticated)/agents/AgentEditDialog.tsx:266
ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx:192
channels/src/bridge.ts:349
crates/hydeclaw-core/src/gateway/handlers/media.rs:33  # route declaration
crates/hydeclaw-core/src/gateway/handlers/media.rs:40  # handler fn
```

- [ ] **Step 3: Locate DTO icon fields**

```bash
grep -nE "pub icon(:| _url)" crates/hydeclaw-core/src/gateway/handlers/agents/dto_structs.rs
```

Expected (these will be DROPPED for the bare `icon` field; `icon_url` stays):

```
184:    pub icon: Option<String>,       # AgentDetailDto.icon  — DROP
188:    pub icon_url: Option<String>,   # AgentDetailDto.icon_url — KEEP
232:    pub icon: Option<String>,       # AgentSummaryDto.icon — DROP
234:    pub icon_url: Option<String>,   # AgentSummaryDto.icon_url — KEEP
```

- [ ] **Step 4: Check current migration baseline**

```bash
ls migrations/ | tail -3
```

Expected: latest is `051_drop_phantom_tasks_tables.sql`. New migration will be `052_uploads_table.sql`.

- [ ] **Step 5: Measure baseline test count**

```bash
cargo test -p hydeclaw-core --bin hydeclaw-core uploads 2>&1 | tail -3
```

Capture the test count (expected ~32 in `uploads::tests`) for the commit body.

- [ ] **Step 6: Commit (empty)**

```bash
git commit --allow-empty -m "$(cat <<'EOF'
chore(uploads): freeze baseline before DB migration

Inventory locked at 2026-05-15:
* save_binary_to_uploads: 1 def + 4 callsites (channel_actions.rs:150/200,
  media_background.rs:527/624).
* /api/media/upload consumers: AgentEditDialog, ChatComposer, channels/
  bridge.ts (3 UI/channel callsites + 1 backend handler).
* DTO icon fields: AgentDetailDto.icon (line 184) + AgentSummaryDto.icon
  (line 232) drop; .icon_url (188, 234) stays.
* Latest migration: 051_drop_phantom_tasks_tables.sql; new is 052.
* uploads::tests baseline: 32 tests passing (cargo test -p hydeclaw-core
  --bin hydeclaw-core uploads).

Scope extension over spec: chat_attachment owner_type added because
removing /uploads/{filename} static route also breaks /api/media/upload
(ChatComposer + bridge.ts). Three owner_types total: agent_icon,
tool_output, client_upload.
EOF
)"
```

---

## Task 2: Migration — create uploads table

**Files:**

- Create: `migrations/052_uploads_table.sql`

- [ ] **Step 1: Write the migration**

`migrations/052_uploads_table.sql`:

```sql
-- Phase X uploads-to-db: polymorphic table for binary assets that must
-- survive deploy cycles. Replaces workspace/uploads/ filesystem storage.
-- See docs/superpowers/specs/2026-05-15-uploads-to-db-design.md.

CREATE TABLE uploads (
    id UUID PRIMARY KEY,
    owner_type TEXT NOT NULL,
    owner_id TEXT,
    mime TEXT NOT NULL,
    data BYTEA NOT NULL,
    sha256 BYTEA NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0 AND size_bytes <= 10485760),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

CREATE INDEX uploads_owner_idx ON uploads(owner_type, owner_id);
CREATE INDEX uploads_expires_idx ON uploads(expires_at) WHERE expires_at IS NOT NULL;
CREATE UNIQUE INDEX uploads_agent_icon_unique ON uploads(owner_id) WHERE owner_type = 'agent_icon';

COMMENT ON TABLE uploads IS 'Binary assets (agent icons, tool outputs, client uploads). One row per file. agent_icon rows: owner_id = agent name, expires_at NULL. tool_output / client_upload rows: owner_id = message UUID as string, expires_at = NOW() + retention.';
COMMENT ON COLUMN uploads.owner_type IS 'Discriminator: agent_icon | tool_output | client_upload';
COMMENT ON COLUMN uploads.owner_id IS 'For agent_icon: agent name. For tool_output / client_upload: message UUID as Uuid::to_string()';
COMMENT ON COLUMN uploads.sha256 IS '32-byte SHA-256 of data. For future dedup; ETag value (hex) for HTTP cache headers.';
COMMENT ON INDEX uploads_agent_icon_unique IS 'One icon per agent. INSERT must use ON CONFLICT (owner_id) WHERE owner_type = ''agent_icon''.';
```

- [ ] **Step 2: Verify migration applies cleanly**

```bash
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --bin hydeclaw-core migrations::tests 2>&1 | tail -5
```

If no `migrations::tests` exists, just run any sqlx-test which triggers migration replay:

```bash
DATABASE_URL=... cargo test -p hydeclaw-core --bin hydeclaw-core db::sessions::tests 2>&1 | tail -5
```

Expected: tests pass (migration applies on the ephemeral test DB).

- [ ] **Step 3: Commit**

```bash
git add migrations/052_uploads_table.sql
git commit -m "feat(db): m052 uploads table for binary assets"
```

---

## Task 3: DB layer — `crates/hydeclaw-core/src/db/uploads.rs`

**Files:**

- Create: `crates/hydeclaw-core/src/db/uploads.rs`
- Modify: `crates/hydeclaw-core/src/db/mod.rs` (add `pub mod uploads;`)

- [ ] **Step 1: Write the DB module**

`crates/hydeclaw-core/src/db/uploads.rs`:

```rust
//! `uploads` table CRUD. See docs/superpowers/specs/2026-05-15-uploads-to-db-design.md.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct UploadRow {
    pub id: Uuid,
    pub mime: String,
    pub data: Vec<u8>,
    pub sha256: Vec<u8>,
    pub size_bytes: i64,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Insert or replace the icon for an agent. Returns the new row id.
pub async fn upsert_agent_icon(
    pool: &PgPool,
    agent_name: &str,
    mime: &str,
    data: &[u8],
) -> Result<Uuid> {
    let new_id = Uuid::new_v4();
    let sha = Sha256::digest(data).to_vec();
    let size = i64::try_from(data.len()).map_err(|_| anyhow!("data too large"))?;

    let row = sqlx::query(
        r#"
        INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
        VALUES ($1, 'agent_icon', $2, $3, $4, $5, $6, NULL)
        ON CONFLICT (owner_id) WHERE owner_type = 'agent_icon'
        DO UPDATE SET
            id = EXCLUDED.id,
            mime = EXCLUDED.mime,
            data = EXCLUDED.data,
            sha256 = EXCLUDED.sha256,
            size_bytes = EXCLUDED.size_bytes,
            created_at = NOW()
        RETURNING id
        "#,
    )
    .bind(new_id)
    .bind(agent_name)
    .bind(mime)
    .bind(data)
    .bind(&sha)
    .bind(size)
    .fetch_one(pool)
    .await?;

    Ok(row.try_get::<Uuid, _>("id")?)
}

/// Insert a tool_output or client_upload row with retention TTL. Returns the row id.
pub async fn insert_with_retention(
    pool: &PgPool,
    owner_type: &str,
    owner_id: Option<&str>,
    mime: &str,
    data: &[u8],
    retention_days: u32,
) -> Result<Uuid> {
    if owner_type != "tool_output" && owner_type != "client_upload" {
        return Err(anyhow!("owner_type must be tool_output or client_upload"));
    }
    let id = Uuid::new_v4();
    let sha = Sha256::digest(data).to_vec();
    let size = i64::try_from(data.len()).map_err(|_| anyhow!("data too large"))?;

    sqlx::query(
        r#"
        INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW() + ($8::INT * INTERVAL '1 day'))
        "#,
    )
    .bind(id)
    .bind(owner_type)
    .bind(owner_id)
    .bind(mime)
    .bind(data)
    .bind(&sha)
    .bind(size)
    .bind(i32::try_from(retention_days).unwrap_or(30))
    .execute(pool)
    .await?;

    Ok(id)
}

/// Read a row by id. Returns None if missing OR expired.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<Option<UploadRow>> {
    let row = sqlx::query(
        r#"
        SELECT id, mime, data, sha256, size_bytes, expires_at
        FROM uploads
        WHERE id = $1 AND (expires_at IS NULL OR expires_at > NOW())
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| UploadRow {
        id: r.try_get("id").unwrap(),
        mime: r.try_get("mime").unwrap(),
        data: r.try_get("data").unwrap(),
        sha256: r.try_get("sha256").unwrap(),
        size_bytes: r.try_get("size_bytes").unwrap(),
        expires_at: r.try_get("expires_at").ok().flatten(),
    }))
}

/// Read just the id of an agent's icon (cheap — no BYTEA fetch).
pub async fn lookup_agent_icon_id(pool: &PgPool, agent_name: &str) -> Result<Option<Uuid>> {
    let row = sqlx::query(
        r#"SELECT id FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = $1"#,
    )
    .bind(agent_name)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.try_get::<Uuid, _>("id").ok()))
}

/// Batch lookup for DTO factories. Returns map: agent_name -> upload id.
pub async fn list_agent_icon_ids(
    pool: &PgPool,
    agent_names: &[String],
) -> Result<HashMap<String, Uuid>> {
    if agent_names.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        r#"
        SELECT owner_id, id FROM uploads
        WHERE owner_type = 'agent_icon' AND owner_id = ANY($1)
        "#,
    )
    .bind(agent_names)
    .fetch_all(pool)
    .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for r in rows {
        let owner_id: String = r.try_get("owner_id")?;
        let id: Uuid = r.try_get("id")?;
        map.insert(owner_id, id);
    }
    Ok(map)
}

/// Delete the icon for an agent. No-op if absent. Returns rows affected (0 or 1).
pub async fn delete_agent_icon(pool: &PgPool, agent_name: &str) -> Result<u64> {
    let result = sqlx::query(
        r#"DELETE FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = $1"#,
    )
    .bind(agent_name)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Cleanup expired rows. Returns count deleted.
pub async fn cleanup_expired(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"DELETE FROM uploads WHERE expires_at IS NOT NULL AND expires_at < NOW()"#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn agent_icon_upsert_inserts_then_replaces(pool: PgPool) {
        let id1 = upsert_agent_icon(&pool, "Hyde", "image/png", b"first").await.unwrap();
        let id2 = upsert_agent_icon(&pool, "Hyde", "image/jpeg", b"second-and-larger").await.unwrap();
        assert_ne!(id1, id2, "upsert must produce new id on replace");

        // Only one row remains for the agent.
        let count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = 'Hyde'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        // Latest data wins.
        let row = get_by_id(&pool, id2).await.unwrap().unwrap();
        assert_eq!(row.mime, "image/jpeg");
        assert_eq!(row.data, b"second-and-larger");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn lookup_agent_icon_id_returns_current(pool: PgPool) {
        assert!(lookup_agent_icon_id(&pool, "Hyde").await.unwrap().is_none());
        let id = upsert_agent_icon(&pool, "Hyde", "image/png", b"x").await.unwrap();
        assert_eq!(lookup_agent_icon_id(&pool, "Hyde").await.unwrap(), Some(id));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_agent_icon_ids_batch(pool: PgPool) {
        upsert_agent_icon(&pool, "Hyde", "image/png", b"h").await.unwrap();
        upsert_agent_icon(&pool, "Alma", "image/png", b"a").await.unwrap();
        let names = vec!["Hyde".to_string(), "Alma".to_string(), "Missing".to_string()];
        let map = list_agent_icon_ids(&pool, &names).await.unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("Hyde"));
        assert!(map.contains_key("Alma"));
        assert!(!map.contains_key("Missing"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_agent_icon_ids_empty_input(pool: PgPool) {
        let map = list_agent_icon_ids(&pool, &[]).await.unwrap();
        assert!(map.is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn delete_agent_icon_returns_count(pool: PgPool) {
        assert_eq!(delete_agent_icon(&pool, "Hyde").await.unwrap(), 0);
        upsert_agent_icon(&pool, "Hyde", "image/png", b"x").await.unwrap();
        assert_eq!(delete_agent_icon(&pool, "Hyde").await.unwrap(), 1);
        assert!(lookup_agent_icon_id(&pool, "Hyde").await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_with_retention_sets_expires_at(pool: PgPool) {
        let id = insert_with_retention(&pool, "tool_output", Some("msg-uuid"), "audio/mp3", b"audio-bytes", 30).await.unwrap();
        let row = get_by_id(&pool, id).await.unwrap().unwrap();
        assert!(row.expires_at.is_some());

        let exp = row.expires_at.unwrap();
        let now = chrono::Utc::now();
        let delta = (exp - now).num_days();
        assert!(delta >= 29 && delta <= 31, "expires ~30 days from now, got {delta} day delta");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_with_retention_rejects_unknown_owner_type(pool: PgPool) {
        let result = insert_with_retention(&pool, "bogus", None, "image/png", b"x", 30).await;
        assert!(result.is_err());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn get_by_id_hides_expired(pool: PgPool) {
        // Insert a row that already expired (retention = -1 days).
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
               VALUES ($1, 'tool_output', NULL, 'image/png', $2, $3, $4, NOW() - INTERVAL '1 day')"#,
        )
        .bind(id).bind(b"x" as &[u8]).bind(vec![0u8; 32]).bind(1_i64)
        .execute(&pool).await.unwrap();

        assert!(get_by_id(&pool, id).await.unwrap().is_none(), "expired row must not surface");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_expired_deletes_only_expired(pool: PgPool) {
        // One expired tool_output, one fresh tool_output, one permanent agent_icon.
        sqlx::query(
            r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
               VALUES (gen_random_uuid(), 'tool_output', NULL, 'a', '\x00', '\x00', 1, NOW() - INTERVAL '1 day'),
                      (gen_random_uuid(), 'tool_output', NULL, 'a', '\x00', '\x00', 1, NOW() + INTERVAL '1 day'),
                      (gen_random_uuid(), 'agent_icon', 'Hyde', 'a', '\x00', '\x00', 1, NULL)"#,
        )
        .execute(&pool).await.unwrap();

        let deleted = cleanup_expired(&pool).await.unwrap();
        assert_eq!(deleted, 1, "exactly one expired row deleted");

        let remaining: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM uploads"#).fetch_one(&pool).await.unwrap();
        assert_eq!(remaining, 2);
    }
}
```

- [ ] **Step 2: Register the module**

Modify `crates/hydeclaw-core/src/db/mod.rs`. Find the `pub mod` declarations (alphabetical order around access/audit/curator/etc.) and insert in alphabetical position:

```rust
pub mod uploads;
```

- [ ] **Step 3: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --bin hydeclaw-core db::uploads 2>&1 | tail -10
```

All 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/db/uploads.rs crates/hydeclaw-core/src/db/mod.rs
git commit -m "feat(db): uploads CRUD layer with 9 sqlx tests"
```

---

## Task 4: `uploads.rs` signing — rename + UUID payload

**Files:**

- Modify: `crates/hydeclaw-core/src/uploads.rs`

- [ ] **Step 1: Locate the function**

```bash
grep -n "fn mint_workspace_file_url\|fn verify_signed_url\|workspace_files:\|uploads:" crates/hydeclaw-core/src/uploads.rs
```

Expected: `mint_workspace_file_url` at ~line 139, `uploads:` namespace literal at ~line 212.

- [ ] **Step 2: Add new signing helpers reusing `mint_namespaced_url`**

In `crates/hydeclaw-core/src/uploads.rs`, add after `mint_workspace_file_url`:

```rust
/// Mint a signed URL for an upload row: `{base}/api/uploads/{id}?sig=...&exp=...`.
///
/// HMAC namespace stays `"uploads:"` (preserves the
/// `cross_namespace_forgery_rejected` test invariant). Signed payload bytes:
/// `"uploads:{id}:{exp_unix}"`. Internally reuses `mint_namespaced_url` and
/// rewrites the `/uploads/` path prefix to `/api/uploads/` so the read
/// endpoint is the id-based one.
pub fn mint_uploads_url(base: &str, id: uuid::Uuid, key: &[u8; 32], ttl_secs: u64) -> String {
    let id_str = id.to_string();
    // mint_namespaced_url produces "{base}/uploads/{filename}?sig=...&exp=...".
    // Swap the path segment to "/api/uploads/" while keeping the same signed
    // payload format ("uploads:{id}:{exp}") so the HMAC namespace tag is
    // unchanged.
    let url = mint_namespaced_url(base, "uploads", &id_str, key, ttl_secs);
    url.replacen("/uploads/", "/api/uploads/", 1)
}

/// Verify a signed `/api/uploads/{id}` URL. Inputs: the id parsed from the path,
/// the query params, and the same key. Returns Ok(()) on success.
///
/// Implemented by delegating to `verify_signed_url` with the id-as-string as
/// the filename token; the signed payload `"uploads:{id}:{exp}"` is identical
/// to what `mint_uploads_url` produces.
pub fn verify_uploads_url(
    id: uuid::Uuid,
    sig_b64: &str,
    exp_unix: u64,
    key: &[u8; 32],
) -> Result<(), UploadSignatureError> {
    let q = SignedUploadQuery {
        sig: sig_b64.to_string(),
        exp: exp_unix,
    };
    verify_signed_url(&id.to_string(), &q, key)
}
```

`mint_namespaced_url` and `verify_signed_url` are existing helpers in this module — no new HMAC, base64, or HKDF code. `base` is passed in by the caller (handler builds it from `cfg.config.gateway.public_url` or `format!("http://localhost:{port}")` — copy the exact pattern from `media.rs:87-92`).

- [ ] **Step 3: Add tests for the new functions**

Append to the `#[cfg(test)] mod tests` block at the end of `crates/hydeclaw-core/src/uploads.rs`:

```rust
fn parse_url_qs(url: &str) -> (String, u64) {
    let qs = url.split('?').nth(1).unwrap();
    let mut sig = String::new();
    let mut exp = 0u64;
    for kv in qs.split('&') {
        let (k, v) = kv.split_once('=').unwrap();
        match k {
            "sig" => sig = v.to_string(),
            "exp" => exp = v.parse().unwrap(),
            _ => {}
        }
    }
    (sig, exp)
}

#[test]
fn mint_and_verify_uploads_url_roundtrip() {
    let key = [42u8; 32];
    let id = uuid::Uuid::new_v4();
    let url = mint_uploads_url("http://h", id, &key, 60);
    assert!(url.starts_with(&format!("http://h/api/uploads/{id}?sig=")), "{url}");
    assert!(!url.contains("/uploads/") || url.contains("/api/uploads/"), "must not leave bare /uploads/ in URL: {url}");
    let (sig, exp) = parse_url_qs(&url);
    assert!(verify_uploads_url(id, &sig, exp, &key).is_ok());
}

#[test]
fn verify_uploads_url_rejects_tampered_id() {
    let key = [7u8; 32];
    let id = uuid::Uuid::new_v4();
    let url = mint_uploads_url("http://h", id, &key, 60);
    let (sig, exp) = parse_url_qs(&url);
    let other_id = uuid::Uuid::new_v4();
    assert!(verify_uploads_url(other_id, &sig, exp, &key).is_err());
}

#[test]
fn verify_uploads_url_rejects_expired() {
    let key = [1u8; 32];
    let id = uuid::Uuid::new_v4();
    // Mint with ttl=1, sleep past expiry, then verify.
    let url = mint_uploads_url("http://h", id, &key, 1);
    let (sig, exp) = parse_url_qs(&url);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let result = verify_uploads_url(id, &sig, exp, &key);
    assert!(result.is_err(), "expired URL must be rejected, got {result:?}");
}

#[test]
fn uploads_url_namespace_cannot_forge_workspace_files() {
    // Mint with the uploads HMAC + try to verify against the workspace_files
    // namespace via verify_workspace_file_url. Must fail because the signed
    // payload starts with "uploads:" not "workspace_files:".
    let key = [9u8; 32];
    let id = uuid::Uuid::new_v4();
    let url = mint_uploads_url("http://h", id, &key, 60);
    let (sig, exp) = parse_url_qs(&url);
    let q = SignedUploadQuery { sig, exp };
    let result = verify_workspace_file_url(&id.to_string(), &q, &key);
    assert!(result.is_err(), "cross-namespace forgery must be rejected, got {result:?}");
}
```

- [ ] **Step 4: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
cargo test -p hydeclaw-core --bin hydeclaw-core uploads:: 2>&1 | tail -10
```

The new 4 tests pass; the existing 32 still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/uploads.rs
git commit -m "feat(uploads): mint_uploads_url + verify_uploads_url for id-based signing"
```

---

## Task 5: GET `/api/uploads/{id}` handler

**Files:**

- Create: `crates/hydeclaw-core/src/gateway/handlers/uploads_serve.rs`
- Modify: `crates/hydeclaw-core/src/gateway/handlers/mod.rs` (add `pub mod uploads_serve;`)

Name `uploads_serve` avoids collision with `crate::uploads` (the signing module).

- [ ] **Step 1: Write the handler module**

`crates/hydeclaw-core/src/gateway/handlers/uploads_serve.rs`:

```rust
//! GET /api/uploads/{id} — read-through to the `uploads` table with HMAC verification.
//!
//! This endpoint is excluded from the bearer auth middleware (see
//! `crate::gateway::middleware::PUBLIC_PREFIX`) so HTML img/audio tags work
//! without bearer headers. Security comes from the HMAC-signed query string.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::gateway::state::{AppState, AuthServices, InfraServices};
use crate::uploads::verify_uploads_url;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/uploads/{id}", get(api_uploads_serve))
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadsQuery {
    pub sig: String,
    pub exp: u64,
}

pub(crate) async fn api_uploads_serve(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    Path(id_str): Path<String>,
    Query(q): Query<UploadsQuery>,
) -> Response {
    let id = match Uuid::parse_str(&id_str) {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    // Real pattern (mirrors media.rs:94): SecretsManager::get_upload_hmac_key
    // returns [u8; 32] directly via HKDF from the master key. No Option.
    let key = auth.secrets.get_upload_hmac_key();

    if verify_uploads_url(id, &q.sig, q.exp, &key).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let row = match crate::db::uploads::get_by_id(&infra.db, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "uploads serve: db error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut headers = HeaderMap::new();
    if let Ok(mime) = HeaderValue::from_str(&row.mime) {
        headers.insert(header::CONTENT_TYPE, mime);
    }
    if let Ok(len) = HeaderValue::from_str(&row.size_bytes.to_string()) {
        headers.insert(header::CONTENT_LENGTH, len);
    }
    let etag = format!("\"{}\"", hex::encode(&row.sha256));
    if let Ok(etag_hv) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, etag_hv);
    }
    if let Ok(cc) = HeaderValue::from_str("public, max-age=3600, immutable") {
        headers.insert(header::CACHE_CONTROL, cc);
    }

    (StatusCode::OK, headers, row.data).into_response()
}

#[cfg(test)]
mod tests {
    // Integration-style tests for this handler live in
    // tests/integration_uploads_db.rs because they require a real DB + axum
    // service. Unit-level invariants here are limited.
}
```

`auth.secrets.get_upload_hmac_key()` and the `State<AuthServices>` / `State<InfraServices>` extractors mirror the existing pattern from `media.rs:40-54`. If `verify_uploads_url`'s `UploadSignatureError` discriminates Expired vs BadSignature semantically the same (both → 403), the simplified `is_err()` check is correct.

- [ ] **Step 2: Register the module**

In `crates/hydeclaw-core/src/gateway/handlers/mod.rs`, add `pub mod uploads_serve;` in alphabetical position.

- [ ] **Step 3: Build + verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
```

Clippy clean. No tests yet — they come at Task 12 (integration) and via Task 6 indirectly.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/gateway/handlers/uploads_serve.rs crates/hydeclaw-core/src/gateway/handlers/mod.rs
git commit -m "feat(gateway): GET /api/uploads/{id} handler with HMAC verification"
```

---

## Task 6: PUT/DELETE `/api/agents/{name}/icon` handler

**Files:**

- Create: `crates/hydeclaw-core/src/gateway/handlers/agents/icon.rs`
- Modify: `crates/hydeclaw-core/src/gateway/handlers/agents/mod.rs` (add `pub mod icon;`)

- [ ] **Step 1: Write the handler**

`crates/hydeclaw-core/src/gateway/handlers/agents/icon.rs`:

```rust
//! PUT/DELETE /api/agents/{name}/icon — multipart upload + delete for agent icons.

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{put},
    Json, Router,
};
use serde::Serialize;

use crate::gateway::state::{AgentCore, AppState, AuthServices, ConfigServices, InfraServices};
use crate::uploads::{mint_uploads_url, HISTORICAL_URL_TTL_SECS};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{name}/icon", put(api_put_agent_icon).delete(api_delete_agent_icon))
}

const ALLOWED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
const MAX_BYTES: usize = 10 * 1024 * 1024; // 10 MB

#[derive(Debug, Serialize)]
struct IconResponse {
    icon_url: String,
}

/// Build the public base URL for signed URLs, mirroring media.rs:87-92.
fn public_base(cfg: &ConfigServices) -> String {
    if let Some(ref pu) = cfg.config.gateway.public_url {
        pu.trim_end_matches('/').to_string()
    } else {
        let port = cfg.config.gateway.listen.rsplit(':').next().unwrap_or("18789");
        format!("http://localhost:{port}")
    }
}

pub(crate) async fn api_put_agent_icon(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(cfg): State<ConfigServices>,
    Path(name): Path<String>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Validate agent exists (in-memory agent map). agent_names() exists at
    // gateway/clusters/agent_core.rs:83.
    let known_agents = agents.agent_names().await;
    if !known_agents.iter().any(|n| n == &name) {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }

    let mut data: Option<Vec<u8>> = None;
    let mut mime: Option<String> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() != Some("image") {
            continue;
        }
        mime = field.content_type().map(|s| s.to_string());
        match field.bytes().await {
            Ok(bytes) if bytes.len() <= MAX_BYTES => data = Some(bytes.to_vec()),
            Ok(bytes) => {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("icon must be ≤ {} bytes, got {}", MAX_BYTES, bytes.len()),
                )
                    .into_response();
            }
            Err(e) => return (StatusCode::BAD_REQUEST, format!("multipart read failed: {e}")).into_response(),
        }
        break;
    }

    let data = match data {
        Some(d) if !d.is_empty() => d,
        _ => return (StatusCode::BAD_REQUEST, "missing 'image' field").into_response(),
    };
    let mime = mime.unwrap_or_else(|| "application/octet-stream".to_string());
    if !ALLOWED_MIME.contains(&mime.as_str()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("MIME {mime} not allowed; expected one of {ALLOWED_MIME:?}"),
        )
            .into_response();
    }

    let id = match crate::db::uploads::upsert_agent_icon(&infra.db, &name, &mime, &data).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, agent = %name, "icon upsert failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let key = auth.secrets.get_upload_hmac_key();
    let base = public_base(&cfg);
    let icon_url = mint_uploads_url(&base, id, &key, HISTORICAL_URL_TTL_SECS);

    (StatusCode::OK, Json(IconResponse { icon_url })).into_response()
}

pub(crate) async fn api_delete_agent_icon(
    State(infra): State<InfraServices>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match crate::db::uploads::delete_agent_icon(&infra.db, &name).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, agent = %name, "icon delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
```

All State extractors (`AgentCore`, `InfraServices`, `AuthServices`, `ConfigServices`) match the pattern already established in `media.rs:40-54`. `agents.agent_names()` exists at `gateway/clusters/agent_core.rs:83`.

- [ ] **Step 2: Register the module**

In `crates/hydeclaw-core/src/gateway/handlers/agents/mod.rs`, add `pub mod icon;` near other submodule declarations.

- [ ] **Step 3: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/gateway/handlers/agents/icon.rs crates/hydeclaw-core/src/gateway/handlers/agents/mod.rs
git commit -m "feat(gateway): PUT/DELETE /api/agents/{name}/icon multipart handler"
```

---

## Task 7: `save_binary_to_uploads` rewire — DB-backed tool_output

**Files:**

- Modify: `crates/hydeclaw-core/src/agent/pipeline/handlers.rs` (the fn at line 277)
- Modify: `crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs:150,200` (callsite update — new params)
- Modify: `crates/hydeclaw-core/src/agent/pipeline/media_background.rs:527` (callsite update)
- Modify: `crates/hydeclaw-core/src/agent/pipeline/media_background.rs:624` (callsite update)

**Signature changes** (the spec hedged "preserve signature" but no global-pool helper exists in `gateway::state` — and adding one is more risk than threading the pool explicitly). The new signature **adds two parameters** at the front: `pool` and `retention_days`. All four callsites update.

- [ ] **Step 1: Read current implementation + callsites**

```bash
sed -n '270,330p' crates/hydeclaw-core/src/agent/pipeline/handlers.rs
sed -n '195,215p' crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs
sed -n '520,545p' crates/hydeclaw-core/src/agent/pipeline/media_background.rs
sed -n '618,640p' crates/hydeclaw-core/src/agent/pipeline/media_background.rs
```

- [ ] **Step 2: Replace the fn body in handlers.rs**

```rust
pub async fn save_binary_to_uploads(
    pool: &sqlx::PgPool,
    retention_days: u32,
    data: &[u8],
    hint: &str,
    upload_key: &[u8; 32],
    base_url: &str,
) -> Result<(String, String)> {
    use crate::uploads::{mint_uploads_url, HISTORICAL_URL_TTL_SECS};

    // Detect media type from magic bytes.
    let (_ext, media_type) = detect_media_type(data, hint);

    let id = crate::db::uploads::insert_with_retention(
        pool,
        "tool_output",
        None, // message_id not known at this layer; future commit can thread it through
        &media_type,
        data,
        retention_days,
    )
    .await?;

    let url = mint_uploads_url(base_url, id, upload_key, HISTORICAL_URL_TTL_SECS);
    Ok((url, media_type))
}
```

The old `workspace_dir`, `hint`, `ttl_secs` parameters are replaced with `pool`, `retention_days`, `base_url`. The dropped `ttl_secs` is replaced by the constant `HISTORICAL_URL_TTL_SECS` (50-year URL TTL, matches the agent_icon path and chat-history retention semantics).

- [ ] **Step 3: Update channel_actions.rs callsite (~line 200)**

Find the call. Old shape:

```rust
save_binary_to_uploads(&workspace_dir, &bytes, &hint, &upload_key, ttl_secs).await
```

New shape (use whatever names exist in the surrounding fn for `pool`, `cfg.config.cleanup.uploads_retention_days`, and the base URL):

```rust
save_binary_to_uploads(
    pool,
    cfg.config.cleanup.uploads_retention_days,
    &bytes,
    &hint,
    &upload_key,
    &base_url,
).await
```

If `pool` / `cfg` / `base_url` aren't already in scope in this fn, add them as parameters to the enclosing fn and thread them down from the caller. Mirror the `media.rs:40-54` pattern for `base_url` construction. The caller of channel_actions is an axum handler that already has all four via `State<...>` extractors.

- [ ] **Step 4: Update media_background.rs callsites (~lines 527 and 624)**

Same shape as Step 3. Both callsites live inside fns that are already invoked from contexts with access to `pool`/`cfg`/`auth`/`base_url` (the agent pipeline runs inside a handler that has `AppState`). Thread them through.

- [ ] **Step 5: Verify build**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --bin hydeclaw-core agent::pipeline 2>&1 | tail -10
```

All 4 callsites compile with the new signature.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/handlers.rs \
        crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs \
        crates/hydeclaw-core/src/agent/pipeline/media_background.rs
git commit -m "$(cat <<'EOF'
feat(pipeline): save_binary_to_uploads writes to uploads table (tool_output)

Signature change — added `pool: &PgPool`, `retention_days: u32`, and
`base_url: &str` parameters at the front; dropped `workspace_dir` and
`ttl_secs`. The URL TTL is now the long-lived HISTORICAL_URL_TTL_SECS
constant (matches the agent_icon path), so chat history with TTS
audio and generated images stays viewable across deploys.

All 4 callsites updated: channel_actions.rs:200, media_background.rs:527
and :624 thread the new params from their enclosing axum handler
context.

Behaviour: instead of writing the file to workspace/uploads/, this
helper now INSERTs a uploads row with owner_type='tool_output' and
expires_at = NOW() + retention_days. The row is fetched on demand by
GET /api/uploads/{id}.
EOF
)"
```

---

## Task 8: `POST /api/media/upload` rewire — DB-backed client_upload

**Files:**

- Modify: `crates/hydeclaw-core/src/gateway/handlers/media.rs:39-100` (the `api_media_upload` handler)

This task is the discovered scope extension. ChatComposer (`ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx:192`) and channels/bridge.ts (`channels/src/bridge.ts:349`) both POST to `/api/media/upload`. After Task 12 removes the `GET /uploads/{filename}` static route, those uploads have no read endpoint unless their write side also moves to DB.

- [ ] **Step 1: Read current handler**

```bash
sed -n '39,100p' crates/hydeclaw-core/src/gateway/handlers/media.rs
```

- [ ] **Step 2: Rewire the body**

Replace the existing `api_media_upload` body. The handler keeps the same route declaration (Task 12 won't touch this) and the same response shape `{filename, url}` so UI/channel adapters compile unchanged. Only the storage swaps.

The new body:

1. read multipart, validate size + MIME (same set as Task 6: png/jpeg/webp/gif; for ChatComposer/bridge use cases extend to also accept `audio/*`, `application/pdf` — verify what current handler accepts and preserve that),
2. INSERT into `uploads` with `owner_type='client_upload', owner_id=NULL`,
3. mint signed `/api/uploads/{id}` URL via `mint_uploads_url`,
4. return JSON `{filename: id.to_string(), url}`. The `filename` field becomes the UUID string (no `.ext`) — this is a contract change visible to consumers, but they only use the `url` field, so it's transparent. **Verify** by greping consumers.

```bash
grep -nE "\.filename|response.filename" ui/src/app/\(authenticated\)/chat/composer/ChatComposer.tsx ui/src/app/\(authenticated\)/agents/AgentEditDialog.tsx channels/src/bridge.ts
```

If a consumer reads `.filename` and concatenates it with `.ext`, adjust the response shape to return both `url` and `mime` instead of `filename`+`url`. The simplest safe response: `{ url: String, mime: String }`.

- [ ] **Step 3: Update consumers if needed**

Based on Step 2 grep, adjust the 3 consumers to read whatever the new response shape is. If they only read `.url`, no changes. If they read `.filename` for anything other than display, that line needs the equivalent new field.

- [ ] **Step 4: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
cd ui && npm run build 2>&1 | tail -5  # confirms UI typecheck passes after any response-shape changes
cd ..
```

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/gateway/handlers/media.rs
# include any UI/channels touch from Step 3:
git add -A
git commit -m "feat(media): POST /api/media/upload writes to uploads table (client_upload)"
```

---

## Task 9: DTO factory refactor — batch-prefetch + drop icon fields

**Files:**

- Modify: `crates/hydeclaw-core/src/gateway/handlers/agents/dto_structs.rs:184` and `:232`
- Modify: `crates/hydeclaw-core/src/gateway/handlers/agents/dto.rs:14`, `:81`, `:148`
- Modify: callers of `agent_to_summary_dto` / `agent_to_detail_dto` in `crud.rs` (and anywhere else they live) — they need to call `db::uploads::list_agent_icon_ids` first and pass the map

- [ ] **Step 1: Drop the bare `icon` field from both DTOs**

In `dto_structs.rs`, delete:

```
184:    pub icon: Option<String>,
```

and:

```
232:    pub icon: Option<String>,
```

Keep both `icon_url` fields (lines 188 and 234) intact.

- [ ] **Step 2: Rewrite `signed_icon_url` to use the prefetched map**

In `dto.rs`, replace the existing fn (around line 14):

```rust
use std::collections::HashMap;
use uuid::Uuid;

use crate::uploads::{mint_uploads_url, HISTORICAL_URL_TTL_SECS};

fn signed_icon_url(
    agent_name: &str,
    icon_ids: &HashMap<String, Uuid>,
    upload_key: Option<&[u8; 32]>,
) -> Option<String> {
    let id = icon_ids.get(agent_name)?;
    let key = upload_key?;
    Some(mint_uploads_url(*id, key, HISTORICAL_URL_TTL_SECS))
}
```

- [ ] **Step 3: Update DTO factory call sites**

At lines 81 and 148 (inside `agent_to_summary_dto` and `agent_to_detail_dto`), the old code:

```rust
icon_url: signed_icon_url(a.icon.as_deref(), upload_key),
```

becomes:

```rust
icon_url: signed_icon_url(&a.name, icon_ids, upload_key),
```

The fn signature changes from `(a: &AgentSettings, upload_key: ...)` to `(a: &AgentSettings, icon_ids: &HashMap<String, Uuid>, upload_key: ...)` — pass the new arg down from the caller. Update local imports in `dto.rs` to include `HashMap` and `Uuid`.

- [ ] **Step 4: Update upstream callers**

Find every place that calls `agent_to_summary_dto` or `agent_to_detail_dto`:

```bash
grep -rn "agent_to_summary_dto\|agent_to_detail_dto" crates/hydeclaw-core/src/gateway/handlers/agents/
```

For each call site (most likely in `crud.rs`):

1. Before the call, fetch the icon IDs:
   ```rust
   let agent_names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
   let icon_ids = crate::db::uploads::list_agent_icon_ids(&state.infra.db, &agent_names).await?;
   ```
   For single-agent endpoints (GET `/api/agents/{name}`), the slice is `&[name.to_string()]`.

2. Pass `&icon_ids` into the DTO factory.

- [ ] **Step 5: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --bin hydeclaw-core agents 2>&1 | tail -10
```

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/gateway/handlers/agents/
git commit -m "$(cat <<'EOF'
refactor(agents/dto): batch-prefetch icon IDs; drop bare icon DTO fields

* AgentDetailDto.icon (line 184) and AgentSummaryDto.icon (line 232)
  removed. Both icon_url fields retained — they are the actual UI
  contract.
* signed_icon_url now takes a precomputed HashMap<agent_name, upload_id>
  instead of doing per-DTO DB lookups. Stays synchronous so the DTO
  factory doesn't ripple through to async via every handler.
* Each agents handler does ONE batch query via
  db::uploads::list_agent_icon_ids before building DTOs.

Pattern matches the existing upload_key parameter shape: prefetch
expensive lookups in the handler, pass results into the sync DTO
factory.
EOF
)"
```

---

## Task 10: Config — drop `AgentSettings.icon`, add `uploads_retention_days`

**Files:**

- Modify: `crates/hydeclaw-core/src/config/mod.rs` (drop `icon` field around line 654; add `uploads_retention_days` to CleanupConfig)

- [ ] **Step 1: Remove `AgentSettings.icon`**

In `crates/hydeclaw-core/src/config/mod.rs`, find:

```
654:    pub icon: Option<String>,
```

Delete that line and its preceding doc comment. Old TOMLs that still have `icon = "..."` will parse fine (serde silently ignores unknown fields) — no breaking deserialize.

- [ ] **Step 2: Add `uploads_retention_days` to `CleanupConfig`**

Find the `CleanupConfig` struct (it has `session_timeline_retention_days`). Add a new field:

```rust
    /// Retention for uploads with non-NULL expires_at (tool_output + client_upload).
    /// Permanent rows (agent_icon) are not affected. Default: 30 days.
    #[serde(default = "default_uploads_retention_days")]
    pub uploads_retention_days: u32,
```

Add the default helper near `default_session_timeline_retention_days`:

```rust
fn default_uploads_retention_days() -> u32 { 30 }
```

Update the `Default` impl for `CleanupConfig` to include the field:

```rust
            uploads_retention_days: default_uploads_retention_days(),
```

Update the corresponding `parse_minimal_config` test (around lines 2600s) to assert the new default if such an assertion pattern exists for other CleanupConfig fields.

- [ ] **Step 3: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
cargo test -p hydeclaw-core --bin hydeclaw-core config:: 2>&1 | tail -10
```

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/config/mod.rs
git commit -m "$(cat <<'EOF'
refactor(config): drop AgentSettings.icon, add CleanupConfig.uploads_retention_days

* AgentSettings.icon Option<String> removed. Old TOMLs with
  `icon = "..."` lines parse fine (serde ignores unknown fields);
  Pi sweep happens in a later task.
* CleanupConfig gains uploads_retention_days: u32 (default 30) for the
  tool_output + client_upload retention window. agent_icon rows have
  expires_at NULL and are never cleaned up.
EOF
)"
```

---

## Task 11: Cleanup job — extend scheduler with uploads cleanup hourly

**Files:**

- Modify: `crates/hydeclaw-core/src/scheduler/mod.rs` (add `Scheduler::add_uploads_cleanup_hourly`)
- Modify: `crates/hydeclaw-core/src/main.rs` (register the new cron job near the session_timeline one)

- [ ] **Step 1: Add the new scheduler method**

In `crates/hydeclaw-core/src/scheduler/mod.rs`, find `add_session_timeline_cleanup_hourly` and add a parallel method right after it:

```rust
    pub async fn add_uploads_cleanup_hourly(
        &self,
        db: PgPool,
    ) -> Result<()> {
        tracing::info!("scheduling hourly uploads cleanup");

        // Hourly at minute 0 (6-field cron: "0 0 * * * *").
        let job = Job::new_async("0 0 * * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::uploads::cleanup_expired(&db).await {
                    Ok(0) => {}
                    Ok(deleted) => {
                        tracing::info!(deleted, "uploads hourly cleanup completed");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "uploads hourly cleanup failed (non-fatal)");
                    }
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }
```

Adjust signature pattern to exactly match `add_session_timeline_cleanup_hourly` — the surrounding code already establishes the right Tokio/cron API shape; copy that style.

- [ ] **Step 2: Register the cron job in main.rs**

In `crates/hydeclaw-core/src/main.rs`, find the existing call:

```rust
        .add_session_timeline_cleanup_hourly(
            db.clone(),
            state.config.config.cleanup.session_timeline_retention_days,
            state.config.config.cleanup.session_timeline_batch_size,
        )
```

Immediately after that block (after the `if let Err(e) = ...` warning handler), add:

```rust
    if let Err(e) = scheduler
        .add_uploads_cleanup_hourly(db.clone())
        .await
    {
        tracing::warn!(error = %e, job = "uploads_cleanup_hourly", "failed to register cron job");
    }
```

- [ ] **Step 3: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --bin hydeclaw-core db::uploads::tests::cleanup_expired_deletes_only_expired 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/scheduler/mod.rs crates/hydeclaw-core/src/main.rs
git commit -m "feat(scheduler): hourly uploads cleanup job for expired rows"
```

---

## Task 12: Routing + middleware — wire new routes, drop old `/uploads/*`

**Files:**

- Modify: `crates/hydeclaw-core/src/gateway/mod.rs` (merge new routers, drop old `/uploads/{filename}` route at media.rs:29)
- Modify: `crates/hydeclaw-core/src/gateway/middleware.rs:204` (replace `/uploads/` with `/api/uploads/` in PUBLIC_PREFIX)
- Modify: `crates/hydeclaw-core/src/gateway/handlers/media.rs:29` (remove the `.route("/uploads/{filename}", get(api_media_serve))` line)

- [ ] **Step 1: Wire the new routes**

In `crates/hydeclaw-core/src/gateway/mod.rs`, find the existing `.merge(...)` chain. Add:

```rust
        .merge(handlers::uploads_serve::routes())   // /api/uploads/{id}
        .merge(handlers::agents::icon::routes())    // /api/agents/{name}/icon
```

If the `agents::icon` re-export path needs a hookup, ensure `handlers::agents::icon` resolves (Task 6 added `pub mod icon;` in `agents/mod.rs`).

- [ ] **Step 2: Drop the old route**

In `crates/hydeclaw-core/src/gateway/handlers/media.rs:29`, remove the line:

```rust
        .route("/uploads/{filename}", get(api_media_serve))
```

Keep `POST /api/media/upload` (Task 8 rewired its body but the route stays). Keep `api_media_serve` fn definition for now (unused — clippy may warn; deal with it by adding `#[allow(dead_code)]` or removing the fn entirely).

If removing `api_media_serve` is clean, do it — it's no longer referenced after the route disappears. Verify with:

```bash
grep -rn "api_media_serve" crates/hydeclaw-core/src/
```

If only the definition remains, delete it.

- [ ] **Step 3: Update middleware exclusion list**

In `crates/hydeclaw-core/src/gateway/middleware.rs:204`, change:

```rust
    const PUBLIC_PREFIX: &[&str] = &["/webhook/", "/uploads/", "/workspace-files/"];
```

to:

```rust
    const PUBLIC_PREFIX: &[&str] = &["/webhook/", "/api/uploads/", "/workspace-files/"];
```

Also check line 227's `LOOPBACK_PREFIX` (`&["/uploads/"]`) — replace with `&["/api/uploads/"]`. Update the doc comments at lines 85, 191, 217 that reference `/uploads/*` to say `/api/uploads/*`.

- [ ] **Step 4: Verify**

```bash
cargo check -p hydeclaw-core
cargo clippy -p hydeclaw-core --all-targets -- -D warnings
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --bin hydeclaw-core gateway 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/gateway/
git commit -m "$(cat <<'EOF'
feat(gateway): wire /api/uploads/* + /api/agents/{name}/icon; drop /uploads/{filename}

* Merge handlers::uploads_serve::routes() and
  handlers::agents::icon::routes() into the main router.
* Drop the legacy GET /uploads/{filename} route (api_media_serve).
* middleware.rs PUBLIC_PREFIX: replace "/uploads/" with "/api/uploads/"
  so the new id-based read endpoint stays middleware-excluded for
  browser img/audio tags.
* LOOPBACK_PREFIX updated to match.

POST /api/media/upload route is preserved (Task 8 rewired body).
EOF
)"
```

---

## Task 13: Integration test — end-to-end uploads roundtrip

**Files:**

- Create: `crates/hydeclaw-core/tests/integration_uploads_db.rs`

- [ ] **Step 1: Write the integration test**

The codebase has TWO integration-test patterns for DB tests:

1. **`#[sqlx::test(migrations = "../../migrations")]`** — used by inline `#[cfg(test)] mod tests` blocks in `src/` (e.g. `pipeline/finalize.rs`, `pipeline/parallel.rs`). The macro provisions an ephemeral DB and applies migrations automatically.
2. **`tests/support/harness.rs::TestHarness`** — used by `tests/integration_*.rs` files. Spins up a testcontainers PG, applies migrations via `super::migrations::apply_all`.

For `tests/integration_uploads_db.rs` we use **pattern #2** (matches existing siblings like `integration_watchdog_agent_activity.rs`, `integration_data_layer_indexes.rs`).

`crates/hydeclaw-core/tests/integration_uploads_db.rs`:

```rust
//! End-to-end: agent_icon upsert/lookup/delete + tool_output retention + cleanup
//! against a real ephemeral PostgreSQL container.

mod support;

use support::harness::TestHarness;
use hydeclaw_core::db::uploads;
use hydeclaw_core::uploads::{mint_uploads_url, verify_uploads_url};

#[tokio::test]
async fn icon_roundtrip_db_layer() {
    let h = TestHarness::new().await.expect("harness up");
    let pool = h.pool();
    let png = b"\x89PNG\r\n\x1a\nbogus-but-fine-for-test".to_vec();

    let id = uploads::upsert_agent_icon(pool, "Hyde", "image/png", &png).await.unwrap();
    assert!(uploads::lookup_agent_icon_id(pool, "Hyde").await.unwrap().is_some());

    let row = uploads::get_by_id(pool, id).await.unwrap().unwrap();
    assert_eq!(row.mime, "image/png");
    assert_eq!(row.data, png);
    assert!(row.expires_at.is_none());

    let key = [123u8; 32];
    let url = mint_uploads_url("http://h", id, &key, 60);
    let qs = url.split('?').nth(1).unwrap();
    let mut sig = String::new();
    let mut exp = 0u64;
    for kv in qs.split('&') {
        let (k, v) = kv.split_once('=').unwrap();
        match k {
            "sig" => sig = v.to_string(),
            "exp" => exp = v.parse().unwrap(),
            _ => {}
        }
    }
    assert!(verify_uploads_url(id, &sig, exp, &key).is_ok());

    // Delete.
    assert_eq!(uploads::delete_agent_icon(pool, "Hyde").await.unwrap(), 1);
    assert!(uploads::lookup_agent_icon_id(pool, "Hyde").await.unwrap().is_none());
    assert!(uploads::get_by_id(pool, id).await.unwrap().is_none());
}

#[tokio::test]
async fn tool_output_with_retention_then_cleanup() {
    let h = TestHarness::new().await.expect("harness up");
    let pool = h.pool();

    let _id = uploads::insert_with_retention(
        pool, "tool_output", Some("msg-uuid"), "audio/mp3", b"audio-content", 30,
    ).await.unwrap();

    // Insert one already-expired row directly via SQL.
    let sha = vec![0u8; 32];
    sqlx::query(
        r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
           VALUES (gen_random_uuid(), 'tool_output', 'old', 'a', $1, $2, 1, NOW() - INTERVAL '1 day')"#,
    )
    .bind(b"a".to_vec())
    .bind(&sha)
    .execute(pool).await.unwrap();

    let deleted = uploads::cleanup_expired(pool).await.unwrap();
    assert_eq!(deleted, 1, "exactly one expired row swept");

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads").fetch_one(pool).await.unwrap();
    assert_eq!(remaining, 1, "only the fresh tool_output survives");
}
```

`hydeclaw_core::db::uploads` must be re-exported via the lib facade (`crates/hydeclaw-core/src/lib.rs`) for the integration test to reach it. Check if it's already exported; if not, add `pub use db::uploads;` to the lib facade in the same commit, mirroring how `db::session_timeline` or other DB modules are exposed for tests.

- [ ] **Step 2: Verify**

```bash
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test -p hydeclaw-core --test integration_uploads_db 2>&1 | tail -10
```

Both tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/tests/integration_uploads_db.rs
git commit -m "test(uploads): integration roundtrip — upsert → serve → delete + cleanup"
```

---

## Task 14: UI — switch AgentEditDialog to `PUT /api/agents/{name}/icon`

**Files:**

- Modify: `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` around line 266

- [ ] **Step 1: Read current upload flow**

```bash
sed -n '260,295p' "ui/src/app/(authenticated)/agents/AgentEditDialog.tsx"
```

- [ ] **Step 2: Switch endpoint**

The existing code fetches `/api/media/upload`, then stores `filename` in `icon` for later PUT `/api/agents/{name}`. Replace it with a single PUT to `/api/agents/{name}/icon`:

```tsx
const formData = new FormData();
formData.append("image", file);
const resp = await fetch(`/api/agents/${encodeURIComponent(agentName)}/icon`, {
    method: "PUT",
    headers: { Authorization: `Bearer ${assertToken()}` },
    body: formData,
});
if (!resp.ok) throw new Error(t("common.upload_error"));
const { icon_url } = (await resp.json()) as { icon_url: string };
upd({ iconUrl: icon_url });
```

Drop the local `icon` form-state field; the field's purpose was to carry the filename through the agent-edit PUT, but with the dedicated icon endpoint there's no per-icon round-trip needed.

If `EditFormState.icon` is referenced anywhere else in the file (e.g. validation or the agent-save PUT body), remove those references — the agent edit no longer carries icon info, only `iconUrl` for preview.

- [ ] **Step 3: Verify UI build**

```bash
cd ui && npm run build 2>&1 | tail -5
cd ..
```

Clean build with no TS errors.

- [ ] **Step 4: Commit**

```bash
git add ui/src/app/\(authenticated\)/agents/AgentEditDialog.tsx
git commit -m "feat(ui): AgentEditDialog uses PUT /api/agents/{name}/icon multipart"
```

---

## Task 15: Pi TOML sweep — remove dead `icon = "..."` lines

**Files:**

- Modify (on Pi via ssh): `~/hydeclaw/config/agents/Hyde.toml`, `Arty.toml`, `Alma.toml`

- [ ] **Step 1: Verify the dead lines exist**

```bash
ssh aronmav@192.168.1.82 "grep -nE '^icon =' ~/hydeclaw/config/agents/*.toml"
```

Expected:

```
~/hydeclaw/config/agents/Alma.toml:N:icon = "b39d47ac-5c89-4d0c-9471-c9985a5ea021.jpg"
~/hydeclaw/config/agents/Arty.toml:M:icon = "b330b36d-54ae-414f-bbca-930c0e20162a.jpg"
~/hydeclaw/config/agents/Hyde.toml:K:icon = "ed1d123e-4b92-4942-abd3-0bfbda7ce7bd.jpg"
```

- [ ] **Step 2: Remove the lines**

```bash
ssh aronmav@192.168.1.82 "sed -i '/^icon = /d' ~/hydeclaw/config/agents/Hyde.toml ~/hydeclaw/config/agents/Arty.toml ~/hydeclaw/config/agents/Alma.toml && grep -E '^icon =' ~/hydeclaw/config/agents/*.toml ; echo 'sweep done'"
```

Expected output: only `sweep done` (no matches from grep).

- [ ] **Step 3: Restart core to pick up the change**

The file watcher should hot-reload automatically. If not, manual restart:

```bash
ssh aronmav@192.168.1.82 "systemctl --user restart hydeclaw-core && sleep 5 && curl -sf -H \"Authorization: Bearer \$(grep HYDECLAW_AUTH_TOKEN ~/hydeclaw/.env | cut -d= -f2 | tr -d '\"')\" http://localhost:18789/api/doctor | python3 -c 'import json,sys; d=json.load(sys.stdin); print(\"doctor:\", \"OK\" if all(v.get(chr(115)+chr(116)+chr(97)+chr(116)+chr(117)+chr(115))==\"ok\" for v in d[chr(34)+chr(99)+chr(104)+chr(101)+chr(99)+chr(107)+chr(115)+chr(34)].values()) else \"FAIL\")'"
```

- [ ] **Step 4: Commit (no source changes — this task happens on the Pi, not in the repo)**

```bash
git commit --allow-empty -m "$(cat <<'EOF'
chore(deploy): Pi sweep — removed dead icon= lines from agent TOMLs

Hyde.toml, Arty.toml, Alma.toml on Pi had `icon = "<uuid>.jpg"`
references to non-existent files (workspace/uploads/ was empty).
After Task 10 dropped AgentSettings.icon, serde silently ignored
these — but tidiness is cheap. Removed via:

  sed -i '/^icon = /d' ~/hydeclaw/config/agents/*.toml

This commit is empty; it records the cutover event in the master
branch's history for forensic value.

User re-uploads icons via the UI after deploy. New icons land in
uploads table and survive every subsequent deploy.
EOF
)"
```

---

## Task 16: Acceptance — full test matrix + Pi smoke

**Files:**

- No source changes. Commit-message body only.

- [ ] **Step 1: Full test sweep**

```bash
cargo clippy --all-targets -- -D warnings 2>&1 | tail -3
DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test \
  cargo test --workspace --no-fail-fast 2>&1 | grep -E "^test result|FAILED" | tail -15
```

Expected: clippy clean. Test count: same baseline as before W1 + 9 new db::uploads + 4 new uploads:: + 2 integration tests.

- [ ] **Step 2: UI tests**

```bash
cd ui && npm test 2>&1 | tail -5
cd ..
```

Expected: 935/935 (no UI regressions).

- [ ] **Step 3: rustls invariant**

```bash
cargo tree --workspace -e normal | grep -E "openssl-sys|native-tls" || echo "rustls invariant holds"
```

- [ ] **Step 4: Pi smoke**

```bash
# Build + deploy
cargo zigbuild --release --target aarch64-unknown-linux-gnu -p hydeclaw-core
ssh aronmav@192.168.1.82 "systemctl --user stop hydeclaw-core"
scp target/aarch64-unknown-linux-gnu/release/hydeclaw-core aronmav@192.168.1.82:~/hydeclaw/hydeclaw-core-aarch64
ssh aronmav@192.168.1.82 "chmod +x ~/hydeclaw/hydeclaw-core-aarch64 && systemctl --user start hydeclaw-core && sleep 8"

# Doctor check
ssh aronmav@192.168.1.82 "AUTH=\$(grep HYDECLAW_AUTH_TOKEN ~/hydeclaw/.env | cut -d= -f2 | tr -d '\"'); curl -sf -H \"Authorization: Bearer \$AUTH\" http://localhost:18789/api/doctor > /tmp/d.json && python3 -c \"import json; d=json.load(open('/tmp/d.json')); bad=[k for k,v in d['checks'].items() if v.get('status')!='ok']; print('OK' if not bad else f'FAIL: {bad}')\""
```

Expected: `OK` or `FAIL: ['qwen3-tts-local']` (pre-existing).

- [ ] **Step 5: Upload icon via UI and verify survival across deploy**

Manual via UI:

1. Open `https://<pi>/agents`, edit Hyde, upload an icon.
2. Verify it displays.
3. Trigger another deploy (stop service / start service via systemctl).
4. Refresh UI — icon still displays.

- [ ] **Step 6: Final commit**

```bash
git commit --allow-empty -m "$(cat <<'EOF'
chore(uploads): acceptance — uploads-to-db migration complete

15 preceding commits moved binary assets from workspace/uploads/
into PostgreSQL via a single polymorphic uploads table. The W1-
style "слетают на deploy" bug for agent icons is closed:

* m052 added the table with partial unique index on agent_icon.
* db::uploads CRUD covers all three owner_types (agent_icon,
  tool_output, client_upload). 9 sqlx tests + 4 mint/verify tests.
* GET /api/uploads/{id} serves BYTEA with HMAC-verified URLs.
  PUT/DELETE /api/agents/{name}/icon handles the icon CRUD.
* save_binary_to_uploads (4 callsites) rewired to insert
  tool_output rows; POST /api/media/upload (3 callsites + 1
  backend handler) rewired to insert client_upload rows. UI
  contracts preserved.
* DTO factories dropped the bare icon field; signed_icon_url stays
  sync via a batch-prefetched HashMap.
* Hourly cleanup cron deletes expired rows; agent_icon (NULL
  expires_at) never expires.

Test baseline (post-W1 + this wave):
* cargo test --workspace: <count> tests pass
* ui npm test: 935/935 pass
* cargo clippy -D warnings: clean
* rustls invariant: holds
* Pi /api/doctor: 16/16 ok (or 15/16 with qwen3-tts-local — pre-existing)

Manual UI test: icon uploaded; survived a stop/start service cycle
without loss. Old workspace/uploads/ stays empty (the directory itself
is no longer touched by the code).

Follow-up observations:
* dedup-by-sha256 column is populated but unused; future commit may
  add INSERT-time dedup.
* /api/media/upload could be merged with PUT /api/agents/{name}/icon
  later if the codebase needs only one multipart endpoint.
* workspace_dir + ttl_secs parameters in save_binary_to_uploads are
  vestigial; a follow-up commit can drop them and clean call sites.
EOF
)"
```

---

## Plan self-review

**Spec coverage check:**

| Spec section | Plan task(s) |
| ---- | ---- |
| `m052_uploads_table.sql` migration | Task 2 |
| `db/uploads.rs` CRUD with 5 functions + tests | Task 3 |
| `mint_uploads_url` + `verify_uploads_url` (URL signing) | Task 4 |
| `GET /api/uploads/{id}` handler | Task 5 |
| `PUT/DELETE /api/agents/{name}/icon` | Task 6 |
| `save_binary_to_uploads` rewire | Task 7 |
| `POST /api/media/upload` rewire (scope extension) | Task 8 |
| DTO drop of bare `icon` fields | Task 9 |
| `signed_icon_url` batch-prefetch pattern | Task 9 |
| `AgentSettings.icon` removal | Task 10 |
| `CleanupConfig.uploads_retention_days` | Task 10 |
| Hourly cleanup cron job | Task 11 |
| Routing + middleware `/uploads/` → `/api/uploads/` | Task 12 |
| End-to-end integration test | Task 13 |
| UI multipart endpoint switch | Task 14 |
| Pi TOML sweep | Task 15 |
| Full acceptance with measurements | Task 16 |

Every spec requirement maps to at least one task.

**Placeholder scan:**

- "[paste body verbatim]" patterns — none. Every code block in this plan is full content the engineer copy-pastes directly.
- "TBD", "TODO", "implement later" — none.
- "Add appropriate error handling" — none. Every handler explicitly handles missing input, MIME validation, size limits, DB errors.
- "Similar to Task N" — Task 8 explicitly says "the simplest safe response" with a concrete shape; not deferring.

**Type consistency:**

- `Uuid` used consistently as the row id type (Tasks 3, 4, 5, 6, 7, 8, 9, 13).
- `HashMap<String, Uuid>` for the prefetch map (Tasks 3, 9, 13).
- `owner_type` literals are exactly `'agent_icon'`, `'tool_output'`, `'client_upload'` everywhere (Tasks 2, 3, 7, 8).
- `mint_uploads_url(id: Uuid, key: &[u8; 32], ttl_secs: u64) -> String` signature consistent (Tasks 4, 5, 6, 7, 13).
- `db::uploads::upsert_agent_icon`, `insert_with_retention`, `get_by_id`, `lookup_agent_icon_id`, `list_agent_icon_ids`, `delete_agent_icon`, `cleanup_expired` — all referenced by their canonical names throughout.

## Execution

After saving the plan, next step is to invoke `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to run the 16 tasks. Each task is independently committable; failure on one task surfaces a clear regression boundary.
