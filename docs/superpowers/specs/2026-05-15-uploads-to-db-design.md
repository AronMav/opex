# Uploads to PostgreSQL — design

**Date:** 2026-05-15
**Status:** approved design, ready for plan

## Goal

Move user-uploaded and tool-generated binary assets (agent icons, TTS audio, generated images) from the local `workspace/uploads/` directory into PostgreSQL so they survive every deploy and binary swap.

## Problem

`workspace/uploads/` lives outside the deploy bundle:

- `Makefile::deploy` ships `deploy-binary + deploy-ui + deploy-migrations + deploy-prompts + deploy-docker`.
- It does NOT sync `workspace/uploads/`.
- Production Pi (`192.168.1.82`) right now: all three agent configs reference `{uuid}.jpg` filenames; the directory is empty; all icons are broken.
- Same problem affects tool outputs (TTS audio, image-gen): they go through `save_binary_to_uploads()` into the same directory and would also vanish on any deploy that wipes the workspace.

The pattern matches what was already done for channel credentials (TOML → encrypted vault) — those used to "слетать" on deploy for the same reason. This spec applies the same solution to binary assets.

## Architecture

Single polymorphic `uploads` table holds every binary tied to an agent or a message. Two consumer classes:

| `owner_type` | `owner_id` | `expires_at` | Replace policy |
| --- | --- | --- | --- |
| `agent_icon` | agent name (e.g. `"Hyde"`) | `NULL` (permanent) | Unique per agent; PUT replaces |
| `tool_output` | message ID UUID (or `NULL` if pre-persist) | `NOW() + 30 days` (configurable) | Append-only; cleanup job deletes expired |

Serving uses the existing HMAC-signed URL pattern from Phase 64 SEC-03: `GET /api/uploads/{id}?sig={hmac}&exp={unix_ts}` is excluded from the auth middleware (so HTML `img` and `audio` tags work without bearer headers), and `crates/opex-core/src/uploads.rs::verify_url_signature` validates the HMAC on each request. URL minting moves from filesystem-path inputs to upload-row IDs.

**HMAC namespace preservation:** the signed payload changes from `"uploads:{filename}:{exp}"` to `"uploads:{id}:{exp}"`. The namespace tag stays `"uploads:"`. This keeps the existing `cross_namespace_forgery_rejected` test invariant alive (a `workspace_files:` URL still cannot be forged with the `uploads:` HMAC key). Only the second token in the signed payload changes from filename to UUID-as-string.

The cleanup loop already exists for `session_timeline` retention. We extend it with one more `DELETE FROM uploads WHERE expires_at < NOW()` query and a `uploads_retention_days` knob in `CleanupConfig`.

## Database schema

`migrations/m052_uploads_table.sql`:

```sql
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
```

- `size_bytes <= 10 MB`: enough for high-res avatars and 60-second TTS clips; rejected at handler before INSERT so we never write oversized rows.
- `sha256` enables future deduplication; the initial implementation simply stores it for forensics.
- `owner_id` is TEXT (not UUID) because `agent_icon` rows use the agent name; for `tool_output` rows we encode the message UUID as canonical string via `Uuid::to_string()`. Heterogeneity is intentional and confined to two known consumer types.
- The partial unique index `uploads_agent_icon_unique` enforces one-icon-per-agent at the DB level. PostgreSQL requires the `ON CONFLICT` clause to repeat the partial predicate explicitly:

  ```sql
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
  RETURNING id;
  ```

  Without the `WHERE owner_type = 'agent_icon'` in the conflict target, PG errors with "there is no unique or exclusion constraint matching the ON CONFLICT specification".
- `expires_at IS NULL` for agent_icon rows; the partial index on `expires_at` skips them, so the cleanup query stays cheap.

## API surface

**`PUT /api/agents/{name}/icon`** — bearer-authed. Body: `multipart/form-data` with a single `image` field. Validates MIME (one of `image/png`, `image/jpeg`, `image/webp`, `image/gif`), `size_bytes <= 10 MB`, and agent existence. INSERTs or REPLACEs the `agent_icon` row and returns JSON `{ "icon_url": "<signed URL>" }`.

**`DELETE /api/agents/{name}/icon`** — bearer-authed. Deletes the `agent_icon` row for the named agent. Returns `204 No Content`.

**`GET /api/uploads/{id}?sig=…&exp=…`** — unauthenticated route excluded from the bearer middleware; HMAC verification happens in the handler. Validates the signature; on success streams BYTEA with `Content-Type` from `mime`, `Content-Length` from `size_bytes`, and `ETag` set to the hex of `sha256`. On invalid or expired signature: `403`. On missing row: `404`. On expired `expires_at`: `410 Gone`.

**DTO field changes:**

- `AgentDetailDto.icon: Option<String>` (filename, line 184 of `dto_structs.rs`) — **removed** along with `AgentSettings.icon`.
- `AgentSummaryDto.icon: Option<String>` (filename, line 232) — **removed** for the same reason.
- `AgentDetailDto.icon_url: Option<String>` — **kept**; this is the actual UI contract.
- `AgentSummaryDto.icon_url: Option<String>` — **kept**.

**`signed_icon_url` rewire — batch-prefetch pattern (NOT async-cascade):**

To avoid making `agent_to_summary_dto` and `agent_to_detail_dto` async (which would ripple through every handler that builds these DTOs), the lookup happens **outside** the DTO factory:

1. Each agents handler (list/get) does ONE upfront DB query to fetch all relevant `agent_icon` upload IDs:

   ```rust
   let icon_ids: HashMap<String, Uuid> = db::uploads::list_agent_icon_ids(pool, &agent_names).await?;
   ```

2. Pass the map into the DTO factory:

   ```rust
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

`signed_icon_url` stays **synchronous**; `agent_to_summary_dto` / `agent_to_detail_dto` stay synchronous. This matches the existing pattern where `upload_key` is passed in as a parameter rather than fetched inside the DTO factory.

`save_binary_to_uploads()` lives in `crates/opex-core/src/agent/pipeline/handlers.rs:277` with three call sites:

- `crates/opex-core/src/agent/pipeline/channel_actions.rs:150,200`
- `crates/opex-core/src/agent/pipeline/media_background.rs:527`
- `crates/opex-core/src/agent/pipeline/media_background.rs:624`

Its existing signature `async fn save_binary_to_uploads(workspace_dir, data, hint, upload_key, ttl_secs) -> Result<(String, String)>` (returning `(url, media_type)`) is **preserved** so the three callers compile unchanged. Implementation rewires the body to:

1. detect media type via `detect_media_type(data, hint)` (existing helper)
2. INSERT a `tool_output` row into `uploads` (sha256 + size + mime + bytes + `expires_at = NOW() + retention_days`)
3. mint a signed `/api/uploads/{id}` URL
4. return `(url, media_type)` exactly as before

The `__file__:` SSE marker keeps its existing format — only the URL substring changes. UI and channel adapters are transparent. The `workspace_dir` parameter becomes unused at call site but is kept in the signature to minimise diff scope; a follow-up commit can drop it from all four signatures.

## Code changes

| Layer | File | Change |
| --- | --- | --- |
| Migration | `migrations/m052_uploads_table.sql` (new) | Create table + indexes |
| DB layer | `crates/opex-core/src/db/uploads.rs` (new) | `get_by_id(pool, id) -> Option<UploadRow>`, `upsert_agent_icon(pool, name, mime, data) -> Uuid`, `insert_tool_output(pool, owner_id, mime, data, retention_days) -> Uuid`, `delete_agent_icon(pool, name)`, `cleanup_expired(pool) -> u64` |
| HTTP handler | `crates/opex-core/src/gateway/handlers/uploads.rs` (new) | `GET /api/uploads/{id}` reads HMAC params, calls `verify_url_signature`, streams BYTEA. Excluded from auth middleware. |
| HTTP handler | `crates/opex-core/src/gateway/handlers/agents/icon.rs` (new) | `PUT /api/agents/{name}/icon` (multipart) and `DELETE /api/agents/{name}/icon` |
| DTO struct | `crates/opex-core/src/gateway/handlers/agents/dto_structs.rs` | Drop `AgentDetailDto.icon` (line 184) and `AgentSummaryDto.icon` (line 232). Keep both `icon_url` fields. TS regen propagates. |
| DTO factory | `crates/opex-core/src/gateway/handlers/agents/dto.rs` | `signed_icon_url(agent_name: &str, icon_ids: &HashMap<String, Uuid>, upload_key: Option<&[u8; 32]>) -> Option<String>` — stays sync; reads from a prefetched map. New helper `db::uploads::list_agent_icon_ids(pool, &[agent_name]) -> HashMap<String, Uuid>` called once per request in the agents handler before DTO construction. |
| Tool dispatch | `crates/opex-core/src/agent/pipeline/handlers.rs:277` (definition) | `save_binary_to_uploads` body rewired to INSERT a `tool_output` row + mint `/api/uploads/{id}` URL. Signature **preserved**: `async fn save_binary_to_uploads(workspace_dir, data, hint, upload_key, ttl_secs) -> Result<(String, String)>`. `workspace_dir` becomes unused at the body level; kept in signature for now. |
| Tool dispatch callers | `crates/opex-core/src/agent/pipeline/channel_actions.rs:150,200` + `crates/opex-core/src/agent/pipeline/media_background.rs:527,624` | No source changes (signature preserved); behaviour now writes to DB instead of file. |
| Config | `crates/opex-core/src/config/mod.rs` | Remove `AgentSettings.icon: Option<String>`. Add `CleanupConfig.uploads_retention_days: u32` (default 30) and `default_uploads_retention_days() -> u32 { 30 }`. |
| Signing | `crates/opex-core/src/uploads.rs` | Rename `mint_workspace_file_url` → `mint_uploads_url(id: Uuid, key, ttl)`. URL format becomes `{base}/api/uploads/{id}?sig=...&exp=...`. Updated `verify_url_signature` accepts the id-based path. Existing 30+ tests in this file get adjusted for the new path shape. |
| Cleanup loop | `crates/opex-core/src/scheduler/mod.rs` — alongside the existing `add_session_timeline_cleanup_hourly` registration in `crates/opex-core/src/main.rs` | Add new method `Scheduler::add_uploads_cleanup_hourly(pool, retention_days)`. The cron body runs `DELETE FROM uploads WHERE expires_at IS NOT NULL AND expires_at < NOW()` and logs the row count via tracing. main.rs adds one registration call mirroring the session_timeline pattern. |
| Routing | `crates/opex-core/src/gateway/mod.rs` + `crates/opex-core/src/gateway/middleware.rs:204` | `merge(uploads::routes())` + `merge(agents::icon::routes())`. Update `PUBLIC_PREFIX` in `middleware.rs:204`: **replace** `"/uploads/"` with `"/api/uploads/"` (no parallel coexistence — old route is removed cleanly). Drop the old `/uploads/*` static-file route from the router. `/workspace-files/` and `/webhook/` exclusions stay. |
| UI | `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` | Change the icon-upload PUT endpoint from the legacy `/uploads/...` flow to `/api/agents/{name}/icon` multipart. The existing `img` tag binding (`src={dto.icon_url}`) is unchanged. |
| TS types | `ui/src/types/api.generated.ts` (regen) | Regenerated by `make gen-types` after `AgentSettings.icon` field is dropped. Already in sync at this commit; nothing manual. |
| TOML | `config/agents/Hyde.toml`, `Arty.toml`, `Alma.toml` (Pi) | Remove dead `icon = "..."` lines. Optional; serde would ignore unknown fields after the struct removal, but tidiness is cheap. |

Estimated 15 files touched, ~12-15 commits.

## Migration flow

1. `m052_uploads_table.sql` runs at startup; table is created.
2. New code reads `agent_icon` rows from the table; the old `AgentSettings.icon` field has been removed.
3. All three production agents lose their icon reference (the underlying files were already missing — current state is "icon broken"; this just makes it official).
4. User re-uploads icons via the UI; the new flow writes to DB; icons survive every subsequent deploy.
5. One cleanup commit removes the stale `icon = "..."` lines from the three TOMLs for tidiness.

No data migration script is needed because there is no data to migrate — the filesystem entries are already gone on the Pi. If a different installation has actual files in `workspace/uploads/`, a one-off script reading the filesystem and INSERTing into uploads is straightforward but is **out of scope** for this spec.

## Risk register

| Risk | Likelihood | Mitigation |
| --- | --- | --- |
| Existing signed `/uploads/{filename}` URLs cached in browser / channel adapters → 404 after switch | High initially | URL change happens at the same moment `signed_icon_url` re-mints, so the DTO emits the new URL; clients refetch on next render. Channel adapters re-mint on every message send. Worst case: a still-open browser tab shows broken icon until refresh. Acceptable. |
| `save_binary_to_uploads()` has call sites we miss; some tools still try to write to filesystem | Medium | Discovery commit greps the workspace for the function name; every call site is updated. The function itself moves to write-to-DB; no filesystem fallback. |
| BYTEA reads of large icons cause memory pressure on Pi (1 GB RAM) | Low | 10 MB cap per row; PG streams BYTEA in chunks; reqwest streaming on the handler side. Worst case: a single in-flight request holds up to 10 MB; far below the agent-engine working set. |
| sqlx COPY/INSERT of 10 MB row through the standard pool stalls | Low | Standard `INSERT ... VALUES ($1, $2, $3, …, $bytea)` parametrized; sqlx handles BYTEA via $bytea parameter binding without TOAST quirks. `make test-db` already exercises BYTEA paths in `memory_chunks.embedding` (halfvec) and `secrets.cipher` (BYTEA). |
| Old `AgentSettings.icon` field removal breaks tests that construct it | Low | Discovery commit greps `AgentSettings` test fixtures and updates them. Existing 1280+ test set covers the surface. |
| Cleanup job races with concurrent reads → 404 mid-stream | Low | `DELETE` is transactional; reads outside the txn either see the row or don't. A row currently being streamed is held by a separate connection's snapshot until the response ends. PG MVCC handles this. |
| Lazy migration: live Pi deploy in the middle of an active session loses an in-flight tool output | Low | Drain timeout in `ShutdownConfig` (30s) gives in-flight requests time to complete before the next binary picks up. Newly issued URLs from the new binary point to the new table. No URL straddles binaries. |

## Test guards

**Pre-existing (must keep passing through every commit):**

- All `crates/opex-core/src/uploads.rs::tests::*` (30+ tests for HMAC mint/verify, percent-encoding, expiry, path canonicalize). Path-shape change requires updates to assertions matching `http://h/uploads/...` to `http://h/api/uploads/...`.
- `tests/integration_upload_hmac.rs` — the existing HMAC roundtrip integration test, also needs URL-shape updates.
- `tests/integration_path_canonicalize.rs` — SEC-02 path canonicalize tests, may need adjustment if the path canonicalization logic touches the uploads route.
- Cross-namespace forgery tests (`uploads::tests::cross_namespace_forgery_rejected`) — confirm the `uploads:` namespace tag in the HMAC stays the same so signed URLs minted before and after this change can't be cross-forged with `workspace_files:` URLs.

**New (added in this spec):**

- `crates/opex-core/src/db/uploads.rs::tests` — round-trip for `upsert_agent_icon`, `insert_tool_output`, `delete_agent_icon`, `cleanup_expired`, `get_by_id`. sqlx_test fixtures, real Postgres via `make test-db`.
- `crates/opex-core/src/gateway/handlers/agents/icon.rs::tests` — PUT/DELETE happy paths, MIME validation, size cap rejection.
- `crates/opex-core/src/gateway/handlers/uploads.rs::tests` — GET returns BYTEA with correct headers; expired-row returns 410; missing row returns 404; invalid HMAC returns 403.
- New integration test `tests/integration_uploads_db.rs` — end-to-end: PUT icon → GET via signed URL → DELETE → GET 404.

## Acceptance criteria

- `cargo clippy --all-targets -- -D warnings` clean at every commit.
- `make test-db` (real Postgres) clean.
- Workspace clippy invariant: `cargo tree -e normal | grep -E 'openssl-sys|native-tls'` empty (rustls preserved).
- `migrations/m052_uploads_table.sql` applies cleanly to the m051 baseline on Pi.
- `/api/doctor` on Pi continues to return 16/16 ok (15/16 acceptable if `qwen3-tts-local` is the only failure as documented).
- Three production agents can be re-uploaded with icons via the UI; the icons survive a deploy cycle (stop service → swap binary → start) without loss.
- Old `/uploads/*` filesystem route is removed (404 for any request to it); the new `/api/uploads/{id}` route handles all binary serving.
- TS-types drift check stays green (auto-regenerate after `AgentSettings.icon` removal).
- Per-module trend record in acceptance commit: lines added/removed per file, new test count.

## Out of scope (deliberately)

- **Image resize/format conversion**. Store as-uploaded. UI renders the bytes natively in an HTML image element. Server-side processing (e.g. WebP conversion) is a follow-up if bandwidth becomes a concern.
- **Per-user upload quotas**. The 10 MB per-row cap is the only limit.
- **External CDN / object store**. PostgreSQL BYTEA is sufficient for the project's scale (a few hundred agent icons + thousands of tool outputs in retention).
- **Backward-compat fallback on `/uploads/{filename}`**. The old route is removed cleanly; any cached browser tab will see a broken icon until refresh, which is acceptable for the cutover window.
- **TOML → DB migration for OTHER agent state** (LLM config, channels, hooks, watchdog, …). That is a much larger architectural change; see `docs/superpowers/specs/2026-05-14-refactoring-roadmap.md` for the broader roadmap context. This spec stays narrowly focused on binary assets.
- **Migration of pre-existing icon files** from any user's local installation. Pi is empty; if a contributor has a populated `workspace/uploads/`, a one-off script reading the directory and INSERTing rows is trivially writable but is left out of this spec.
- **Dedup based on `sha256`**. Column is stored for future use but no INSERT-time dedup is performed in this spec.

## Effort

~2 focused days:

- Discovery + schema design + DB layer + tests: 0.5 day
- Handlers (GET/PUT/DELETE) + signing rewire + auth middleware exclusion: 0.5 day
- DTO + `signed_icon_url` rewire + `save_binary_to_uploads` rewire: 0.5 day
- UI multipart endpoint switch + sweep dead TOML lines + acceptance: 0.5 day

## Next step

Hand off to `superpowers:writing-plans` to expand this design into a per-commit step-by-step plan: each commit's exact files, before/after content, test command to run, and commit-message template. The plan should be detailed enough that an engineer (or subagent) can execute it linearly without re-deciding any design point.
