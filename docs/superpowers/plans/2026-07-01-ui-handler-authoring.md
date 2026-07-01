# UI file-handler authoring — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the operator edit, configure, and create file handlers (including the 5 builtins, via a workspace override) from the File Handlers tab in the UI.

**Architecture:** A handler stays a self-describing `.py` (XML descriptor comment + `run()`). toolgate gains an exec-free validator (`POST /handlers/validate`) and a loader change so a `workspace/file_handlers/{id}.py` **shadows** a same-id builtin (override); the pristine builtin is retained for reset. Core gains operator-only admin endpoints (`GET /api/handlers/{id}/source`, `POST /api/handlers`, `PUT/DELETE /api/handlers/{id}`) that validate via toolgate then write ONLY into `workspace/file_handlers/`; toolgate hot-reloads and core refreshes its manifest cache. The UI extends the tab with a CodeMirror editor + descriptor form (block-on-error save), Create/Edit/Delete/Reset actions, and status badges.

**Tech Stack:** Python/FastAPI (toolgate), Rust/axum + sqlx (core), Next.js/React/CodeMirror/TanStack Query (UI). Spec: `docs/superpowers/specs/2026-07-01-ui-handler-authoring-design.md`.

## Global Constraints

- **Spec of record:** `docs/superpowers/specs/2026-07-01-ui-handler-authoring-design.md`.
- **Operator-only, behind bearer auth** (all `/api/*`). Agents cannot author handlers — no agent tool. Untrusted-agent isolation stays deferred.
- **Writes go ONLY into `workspace/file_handlers/{id}.py`** (core `config::WORKSPACE_DIR = "workspace"`). Builtin source (`toolgate/handlers/builtin/{id}.py`) is READ-ONLY (never written).
- **`id` must match `^[a-z0-9_-]+$`** (traversal-safe — the path is always `workspace/file_handlers/{id}.py`). Enforce at both core and toolgate.
- **Builtin ids = the 5 in `FSE_DEFAULT_ALLOWLIST`** (`transcribe, describe, extract_document, save, summarize_video`). Core uses this const to decide builtin-vs-workspace semantics.
- **Block-on-error:** a handler is written ONLY if toolgate `/handlers/validate` returns `ok:true`. Validation is **exec-free** (descriptor parse + `ast.parse` + AST check for a top-level `run`; never import/exec the module).
- **Gating unchanged:** `match_buttons` gates builtin ids by the allowlist; an override keeps its builtin id → still gated. New ids are workspace-tier (always-on).
- **rustls only** (core); no Co-Authored-By in commits; work on `master`.
- **Per-task gate:** toolgate → `cd toolgate && python -m pytest` (the touched tests); core → `cargo check --all-targets` + `cargo clippy --all-targets -- -D warnings`; UI → `cd ui && npm test` + `npm run build`. Full `cargo test` OOMs the Windows linker — use `cargo check`/`clippy` + focused `--lib` tests locally; the server runs the full suite.

---

## File Structure

**toolgate:**
- Create `toolgate/handlers/validate.py` — exec-free `validate_source(source, expected_id?) -> dict`.
- Modify `toolgate/handlers/loader.py` — builtins retained separately; workspace shadows builtin (override); reset resurfaces builtin; manifest `source` + id-based `tier`.
- Modify `toolgate/handlers/router.py` — add `POST /handlers/validate`.
- Tests: `toolgate/tests/test_handler_validate.py`, extend `toolgate/tests/test_handler_loader.py` (or the existing loader test file).

**core:**
- Modify `crates/opex-core/src/agent/handler_registry.rs` — `HandlerManifest.source` field.
- Modify `crates/opex-core/src/gateway/handlers/handlers_admin.rs` — `HandlerAdminRow.source`; new endpoints + a validate/write helper module section; a pooled client.
- Modify `crates/opex-core/src/db/audit.rs` — new event-type consts.

**UI:**
- Modify `ui/src/types/api.ts` — `HandlerSourceDto`, `HandlerAdminRow.source`.
- Modify `ui/src/lib/queries.ts` — `useHandlerSource`, `useCreateHandler`, `useUpdateHandler`, `useDeleteHandler`.
- Create `ui/src/app/(authenticated)/tools/HandlerEditor.tsx` — CodeMirror + descriptor form + validation errors.
- Create `ui/src/app/(authenticated)/tools/handler-descriptor.ts` — pure `renderDescriptorBlock(fields, source)` + a fixture round-trip test.
- Modify `ui/src/app/(authenticated)/tools/page.tsx` — Edit/Create/Delete/Reset actions, badges, dialog.
- Modify `ui/src/i18n/locales/{en,ru}.json` — `tools.handler_*` keys.

---

# Phase T — toolgate (validator + override loader + endpoint)

### Task T1: Exec-free validator

**Files:**
- Create: `toolgate/handlers/validate.py`
- Test: `toolgate/tests/test_handler_validate.py`

**Interfaces:**
- Produces: `validate_source(source: str, expected_id: str | None = None) -> dict` returning `{"ok": bool, "descriptor": dict | None, "errors": [{"field": str, "message": str}]}`. Consumed by T3 (router) and core (Task C3).

- [ ] **Step 1: Write the failing test**

Create `toolgate/tests/test_handler_validate.py`:

```python
from handlers.validate import validate_source

GOOD = '''# <handler>
#   <id>my_ocr</id>
#   <label lang="en">OCR</label>
#   <match><mime>image/*</mime></match>
#   <execution>sync</execution>
# </handler>
async def run(ctx, file, params):
    return ctx.result.ok("hi")
'''

def test_valid_source_ok_with_descriptor():
    r = validate_source(GOOD, expected_id="my_ocr")
    assert r["ok"] is True
    assert r["errors"] == []
    assert r["descriptor"]["id"] == "my_ocr"
    assert r["descriptor"]["match"]["mime"] == ["image/*"]

def test_bad_descriptor_reports_error():
    r = validate_source("async def run(ctx, file, params):\n    return None\n")
    assert r["ok"] is False
    assert any(e["field"] == "descriptor" for e in r["errors"])

def test_bad_python_reports_error():
    src = GOOD.replace("async def run(ctx, file, params):", "async def run(:")
    r = validate_source(src)
    assert r["ok"] is False
    assert any(e["field"] == "python" for e in r["errors"])

def test_missing_run_reports_error():
    src = GOOD.rsplit("async def run", 1)[0] + "x = 1\n"
    r = validate_source(src)
    assert r["ok"] is False
    assert any("run" in e["message"] for e in r["errors"])

def test_id_mismatch_reports_error():
    r = validate_source(GOOD, expected_id="different")
    assert r["ok"] is False
    assert any(e["field"] == "id" for e in r["errors"])
```

- [ ] **Step 2: Run it — expect FAIL** (`ModuleNotFoundError: handlers.validate`)

Run: `cd toolgate && python -m pytest tests/test_handler_validate.py -q`

- [ ] **Step 3: Implement `toolgate/handlers/validate.py`**

```python
"""Exec-free validation of a handler source: descriptor parse + Python syntax
+ presence of a top-level `run` function — WITHOUT importing/executing the
module (never runs untrusted top-level code)."""

from __future__ import annotations

import ast

from handlers.descriptor import DescriptorError, parse_descriptor


def validate_source(source: str, expected_id: str | None = None) -> dict:
    errors: list[dict] = []
    descriptor: dict | None = None

    # 1. descriptor block (fail-closed parse; no exec)
    try:
        d = parse_descriptor(source, "workspace")
        descriptor = {
            "id": d.id,
            "labels": d.labels,
            "descriptions": d.descriptions,
            "icon": d.icon,
            "match": {"mime": d.match_mimes, "max_size_mb": d.max_size_mb},
            "capability": d.capability,
            "execution": d.execution,
            "output": d.output,
            "params": d.params,
            "order": d.order,
            "enabled": d.enabled,
        }
        if expected_id is not None and d.id != expected_id:
            errors.append({
                "field": "id",
                "message": f"descriptor id '{d.id}' must match handler id '{expected_id}'",
            })
    except DescriptorError as e:
        errors.append({"field": "descriptor", "message": str(e)})

    # 2. Python syntax — parse only, never execute.
    tree: ast.Module | None = None
    try:
        tree = ast.parse(source)
    except SyntaxError as e:
        errors.append({"field": "python", "message": f"syntax error: {e}"})

    # 3. a top-level `run` function must exist (async def or def).
    if tree is not None:
        has_run = any(
            isinstance(n, (ast.AsyncFunctionDef, ast.FunctionDef)) and n.name == "run"
            for n in tree.body
        )
        if not has_run:
            errors.append({"field": "python", "message": "no top-level `run` function defined"})

    return {"ok": not errors, "descriptor": descriptor, "errors": errors}
```

- [ ] **Step 4: Run — expect PASS.** `cd toolgate && python -m pytest tests/test_handler_validate.py -q`

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/validate.py toolgate/tests/test_handler_validate.py
git commit -m "feat(toolgate): exec-free handler validate_source (descriptor + ast + run check)"
```

---

### Task T2: Loader override model (workspace shadows builtin; reset resurfaces)

**Files:**
- Modify: `crates/../toolgate/handlers/loader.py` (the `HandlerRegistry` class)
- Test: `toolgate/tests/test_handler_loader_override.py` (create)

**Interfaces:**
- Produces: manifest dicts now carry `"source": "builtin" | "override" | "workspace"`; `"tier"` is `"builtin"` for a reserved-builtin id (pristine or overridden) else `"workspace"`. `reload_file`/`remove_file` support override + reset.

- [ ] **Step 1: Write the failing test**

Create `toolgate/tests/test_handler_loader_override.py`:

```python
import os
from handlers.loader import HandlerRegistry

BUILTIN = '''# <handler>
#   <id>describe</id>
#   <label lang="en">Describe</label>
#   <match><mime>image/*</mime></match>
#   <execution>sync</execution>
# </handler>
async def run(ctx, file, params):
    return ctx.result.ok("builtin")
'''

OVERRIDE = BUILTIN.replace('ctx.result.ok("builtin")', 'ctx.result.ok("override")')

NEWWS = '''# <handler>
#   <id>my_ocr</id>
#   <label lang="en">OCR</label>
#   <match><mime>image/*</mime></match>
#   <execution>sync</execution>
# </handler>
async def run(ctx, file, params):
    return ctx.result.ok("ws")
'''


def _write(p, s):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w", encoding="utf-8") as f:
        f.write(s)


def test_workspace_shadows_builtin_and_reset_resurfaces(tmp_path):
    bdir = tmp_path / "builtin"
    wdir = tmp_path / "ws"
    _write(str(bdir / "describe.py"), BUILTIN)
    reg = HandlerRegistry()
    reg.load_all(str(bdir), str(wdir))

    # pristine builtin
    m = {x["id"]: x for x in reg.manifests()}
    assert m["describe"]["source"] == "builtin"
    assert m["describe"]["tier"] == "builtin"

    # add an override → shadows the builtin
    ov = str(wdir / "file_handlers" / "describe.py")
    _write(ov, OVERRIDE)
    reg.reload_file(ov)
    m = {x["id"]: x for x in reg.manifests()}
    assert m["describe"]["source"] == "override"
    assert m["describe"]["tier"] == "builtin", "override keeps the builtin id/tier for gating"

    # remove the override → the pristine builtin resurfaces (reset to default)
    os.remove(ov)
    reg.remove_file(ov)
    m = {x["id"]: x for x in reg.manifests()}
    assert m["describe"]["source"] == "builtin"


def test_new_workspace_id_is_workspace_tier(tmp_path):
    bdir = tmp_path / "builtin"
    wdir = tmp_path / "ws"
    os.makedirs(str(bdir), exist_ok=True)
    _write(str(wdir / "file_handlers" / "my_ocr.py"), NEWWS)
    reg = HandlerRegistry()
    reg.load_all(str(bdir), str(wdir))
    m = {x["id"]: x for x in reg.manifests()}
    assert m["my_ocr"]["source"] == "workspace"
    assert m["my_ocr"]["tier"] == "workspace"
```

- [ ] **Step 2: Run — expect FAIL** (no `source` key; override rejected).

Run: `cd toolgate && python -m pytest tests/test_handler_loader_override.py -q`

- [ ] **Step 3: Rewrite the loader internals**

In `toolgate/handlers/loader.py`, change `HandlerRegistry` so builtins are retained separately and workspace shadows them. Replace `__init__`, `load_all`, `_load_one`, `reload_file`, `remove_file`, and `_manifest` with:

```python
    def __init__(self) -> None:
        self._handlers: dict[str, LoadedHandler] = {}
        # Pristine builtins kept separately so an override can be reverted
        # (reset-to-default) by resurfacing the builtin.
        self._builtins: dict[str, LoadedHandler] = {}
        # Maps normalized absolute path → workspace handler id registered from it.
        self._path_to_id: dict[str, str] = {}

    def load_all(self, builtin_dir: str, workspace_dir: str | None) -> None:
        self._handlers = {}
        self._builtins = {}
        self._path_to_id = {}
        # Builtin tier FIRST — retained in _builtins AND seeded as effective.
        self._scan_dir(builtin_dir, "builtin")
        self._builtins = dict(self._handlers)
        if workspace_dir:
            ws = os.path.join(workspace_dir, "file_handlers")
            self._scan_dir(ws, "workspace")

    def _load_one(self, path: str, tier: str) -> None:
        try:
            source = _read_source(path)
            descriptor = parse_descriptor(source, tier)
            if tier == "workspace":
                # Collision rules: a workspace id matching a BUILTIN id is an
                # allowed OVERRIDE (shadows the builtin). A workspace id matching
                # another WORKSPACE handler from a different path is rejected.
                existing = self._handlers.get(descriptor.id)
                if existing is not None and existing.tier == "workspace":
                    log.warning(
                        "handler id %r in %s clashes with existing workspace handler - rejected",
                        descriptor.id, path,
                    )
                    return
            run = _import_run(path)
            self._handlers[descriptor.id] = LoadedHandler(descriptor, run, tier)
            if tier == "workspace":
                self._path_to_id[self._norm(path)] = descriptor.id
            log.info("loaded handler %s (tier=%s)", descriptor.id, tier)
        except DescriptorError as e:
            log.warning("skipping handler file %s: descriptor error: %s", path, e)
        except (SyntaxError, ImportError) as e:
            log.warning("skipping handler file %s: import error: %s", path, e)
        except Exception as e:
            log.warning("skipping handler file %s: unexpected error: %s", path, e)

    def reload_file(self, path: str) -> None:
        if not os.path.isfile(path):
            return
        norm = self._norm(path)
        old_id = self._path_to_id.pop(norm, None)
        if old_id is not None and old_id in self._handlers and self._handlers[old_id].tier == "workspace":
            del self._handlers[old_id]
            # If this was an override of a builtin, resurface the builtin.
            if old_id in self._builtins:
                self._handlers[old_id] = self._builtins[old_id]
        self._load_one(path, "workspace")

    def remove_file(self, path: str) -> None:
        norm = self._norm(path)
        old_id = self._path_to_id.pop(norm, None)
        if old_id is not None and old_id in self._handlers and self._handlers[old_id].tier == "workspace":
            del self._handlers[old_id]
            # Override removed → resurface the pristine builtin (reset-to-default).
            if old_id in self._builtins:
                self._handlers[old_id] = self._builtins[old_id]
                log.info("reset handler %r to builtin default (override removed)", old_id)
            else:
                log.info("removed workspace handler %r (file deleted)", old_id)

    def _manifest(self, h: LoadedHandler) -> dict:
        d = h.descriptor
        is_builtin_id = d.id in self._builtins
        overridden = is_builtin_id and h.tier == "workspace"
        source = "override" if overridden else ("builtin" if is_builtin_id else "workspace")
        tier = "builtin" if is_builtin_id else "workspace"
        return {
            "id": d.id,
            "labels": d.labels,
            "descriptions": d.descriptions,
            "icon": d.icon,
            "match": {"mime": d.match_mimes, "max_size_mb": d.max_size_mb},
            "capability": d.capability,
            "provider": None,  # filled by the router from the active provider (R5)
            "execution": d.execution,
            "output": d.output,
            "params": d.params,
            "order": d.order,
            "tier": tier,
            "source": source,
        }
```

(The module docstring at the top of `loader.py` should drop the "reject a builtin id" wording — reword to "a workspace file with a builtin id overrides it".)

- [ ] **Step 4: Run — expect PASS.** `cd toolgate && python -m pytest tests/test_handler_loader_override.py tests/test_handler_loader.py -q` (run the pre-existing loader test too; if a pre-existing test asserts the OLD reject-builtin behavior, update it to the override behavior and note it in the commit).

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/loader.py toolgate/tests/test_handler_loader_override.py
git commit -m "feat(toolgate): workspace file overrides same-id builtin (shadow + reset); manifest source field"
```

---

### Task T3: `POST /handlers/validate` endpoint

**Files:**
- Modify: `toolgate/handlers/router.py`
- Test: `toolgate/tests/test_handler_validate_route.py` (create)

**Interfaces:**
- Produces: `POST /handlers/validate` body `{"source": str, "id"?: str}` → `{"ok", "descriptor", "errors"}` (200 always; `ok` carries the verdict). Consumed by core Task C3.

- [ ] **Step 1: Write the failing test**

Create `toolgate/tests/test_handler_validate_route.py` (mirror the existing router test's app-construction pattern in `toolgate/tests/` — use the same TestClient/app fixture the repo already uses for `/handlers` route tests):

```python
# Use the same FastAPI app/TestClient fixture pattern as the existing
# toolgate router tests (e.g. tests/test_handlers_router.py).
def test_validate_route_ok(client):
    src = ('# <handler>\n#   <id>my_ocr</id>\n#   <label lang="en">OCR</label>\n'
           '#   <match><mime>image/*</mime></match>\n#   <execution>sync</execution>\n'
           '# </handler>\nasync def run(ctx, file, params):\n    return None\n')
    r = client.post("/handlers/validate", json={"source": src, "id": "my_ocr"})
    assert r.status_code == 200
    body = r.json()
    assert body["ok"] is True
    assert body["descriptor"]["id"] == "my_ocr"

def test_validate_route_reports_errors(client):
    r = client.post("/handlers/validate", json={"source": "x = (", "id": "bad"})
    assert r.status_code == 200
    body = r.json()
    assert body["ok"] is False
    assert body["errors"]
```

- [ ] **Step 2: Run — expect FAIL** (404 no route).

Run: `cd toolgate && python -m pytest tests/test_handler_validate_route.py -q`

- [ ] **Step 3: Add the route**

In `toolgate/handlers/router.py`, add near the other routes (after the imports add `from handlers.validate import validate_source`; add a Pydantic body model or read the raw JSON):

```python
from fastapi import Body
from handlers.validate import validate_source

@router.post("/handlers/validate")
async def validate_handler(payload: dict = Body(...)):
    source = payload.get("source")
    if not isinstance(source, str):
        return JSONResponse(status_code=400, content={"error": "missing 'source'"})
    expected_id = payload.get("id")
    if expected_id is not None and not isinstance(expected_id, str):
        expected_id = None
    return validate_source(source, expected_id)
```

- [ ] **Step 4: Run — expect PASS.** `cd toolgate && python -m pytest tests/test_handler_validate_route.py -q`

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/router.py toolgate/tests/test_handler_validate_route.py
git commit -m "feat(toolgate): POST /handlers/validate endpoint"
```

---

# Phase C — core (manifest source + admin endpoints)

### Task C1: `HandlerManifest.source` + `HandlerAdminRow.source`

**Files:**
- Modify: `crates/opex-core/src/agent/handler_registry.rs` (struct + test builder)
- Modify: `crates/opex-core/src/gateway/handlers/handlers_admin.rs` (row + `from_manifest` + test builders)

**Interfaces:**
- Produces: `HandlerManifest.source: String` (serde default ""), surfaced as `HandlerAdminRow.source`.

- [ ] **Step 1: Add the field to `HandlerManifest`**

In `handler_registry.rs`, after the `tier` field (line ~49) add:

```rust
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub source: String,
```

Update the test builder `mf(...)` (line ~210) and the `manifest_deserializes_from_toolgate_json` / `manifest_defaults_missing_optional_fields` expectations to include `source` (the builders add `source: String::new()`; the deserialize tests need no change since `source` is `#[serde(default)]`).

- [ ] **Step 2: Add `source` to `HandlerAdminRow`**

In `handlers_admin.rs`, add `pub source: String,` to `HandlerAdminRow` (after `tier`), set it in `from_manifest` (`source: m.source.clone(),`), and add `source: String::new()` to the two in-test `HandlerManifest { ... }` literals + the `manifest(...)` test builder.

- [ ] **Step 3: Compile + focused tests**

Run: `cargo check --all-targets && cargo test -p opex-core --lib handler_registry handlers_admin`
Expected: clean; existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/handler_registry.rs crates/opex-core/src/gateway/handlers/handlers_admin.rs
git commit -m "feat(handlers): carry manifest source (builtin/override/workspace) into the admin row"
```

---

### Task C2: `GET /api/handlers/{id}/source`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/handlers_admin.rs`

**Interfaces:**
- Produces: `GET /api/handlers/{id}/source` → `{ id, source, source_kind }` (`source_kind` ∈ `builtin|override|workspace`). `source` = the override file if present, else the pristine builtin file, else the workspace file. Consumed by the UI editor (Task U1/U2).

- [ ] **Step 1: Add id validation + path helpers (module scope in handlers_admin.rs)**

Add near the top helpers:

```rust
use std::path::{Path, PathBuf};

/// `id` must be `^[a-z0-9_-]+$` — no path separators (traversal-safe).
fn valid_handler_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Workspace override/handler path: `workspace/file_handlers/{id}.py`.
fn workspace_handler_path(id: &str) -> PathBuf {
    Path::new(crate::config::WORKSPACE_DIR).join("file_handlers").join(format!("{id}.py"))
}

/// Pristine builtin source path (read-only): `toolgate/handlers/builtin/{id}.py`.
fn builtin_handler_path(id: &str) -> PathBuf {
    Path::new("toolgate").join("handlers").join("builtin").join(format!("{id}.py"))
}

fn is_builtin_id(id: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&id)
}
```

- [ ] **Step 2: Write the failing test**

Add to `handlers_admin.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn handler_id_validation_blocks_traversal() {
        assert!(valid_handler_id("my_ocr"));
        assert!(valid_handler_id("summarize_video"));
        assert!(!valid_handler_id("../etc/passwd"));
        assert!(!valid_handler_id("a/b"));
        assert!(!valid_handler_id("Bad"));
        assert!(!valid_handler_id(""));
    }
```

Run: `cargo test -p opex-core --lib handlers_admin::tests::handler_id_validation_blocks_traversal` → FAIL (fn absent) then PASS after Step 1.

- [ ] **Step 3: Add the route + handler**

Add to `routes()`:

```rust
        .route("/api/handlers/{id}/source", get(api_get_handler_source))
```

Add the handler:

```rust
/// `GET /api/handlers/{id}/source` → raw `.py` for the editor. Precedence:
/// workspace override → pristine builtin (starting point for a new override) →
/// workspace-only handler. 404 if none exists and id is not a builtin.
async fn api_get_handler_source(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid handler id" }))).into_response();
    }
    let ws = workspace_handler_path(&id);
    if let Ok(src) = tokio::fs::read_to_string(&ws).await {
        let kind = if is_builtin_id(&id) { "override" } else { "workspace" };
        return Json(json!({ "id": id, "source": src, "source_kind": kind })).into_response();
    }
    if is_builtin_id(&id) {
        if let Ok(src) = tokio::fs::read_to_string(builtin_handler_path(&id)).await {
            return Json(json!({ "id": id, "source": src, "source_kind": "builtin" })).into_response();
        }
    }
    (StatusCode::NOT_FOUND, Json(json!({ "error": "handler source not found" }))).into_response()
}
```

Add `use axum::routing::get;` already present; ensure `Path` import for the extractor (`axum::extract::Path` fully-qualified above avoids a clash with `std::path::Path`).

- [ ] **Step 4: Compile + test**

Run: `cargo check --all-targets && cargo test -p opex-core --lib handlers_admin`
Expected: clean; id test passes. (The route reads files at runtime; a DB/integration test is deferred to the server E2E.)

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/handlers_admin.rs
git commit -m "feat(handlers): GET /api/handlers/{id}/source (override/builtin/workspace precedence)"
```

---

### Task C3: Create + edit (`POST /api/handlers`, `PUT /api/handlers/{id}`)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/handlers_admin.rs`
- Modify: `crates/opex-core/src/db/audit.rs` (add event consts)

**Interfaces:**
- Consumes: toolgate `POST /handlers/validate`; `ConfigServices.config.toolgate_url`.
- Produces: `POST /api/handlers` `{id, source}` → 201 | 400(validation/collision); `PUT /api/handlers/{id}` `{source}` → 200 | 400. Both write `workspace/file_handlers/{id}.py`, refresh the cache, audit.

- [ ] **Step 1: Add audit event consts**

In `crates/opex-core/src/db/audit.rs` `event_types` module, add:

```rust
    pub const HANDLER_CREATED: &str = "handler_created";
    pub const HANDLER_UPDATED: &str = "handler_updated";
    pub const HANDLER_DELETED: &str = "handler_deleted";
```

(If `audit.rs` has an `all()`/test that enumerates consts, add these there too — mirror how the existing consts are listed.)

- [ ] **Step 2: Add a pooled client + the toolgate-validate helper**

In `handlers_admin.rs`:

```rust
static HANDLERS_HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
fn handlers_http() -> &'static reqwest::Client {
    HANDLERS_HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Ask toolgate to validate a source (exec-free). Returns `Ok(())` on ok:true,
/// `Err(errors_json)` on ok:false, `Err(...)` on transport failure (fail-closed:
/// a validation we can't run must NOT write).
async fn toolgate_validate(toolgate_url: &str, id: &str, source: &str) -> Result<(), serde_json::Value> {
    let url = format!("{}/handlers/validate", toolgate_url.trim_end_matches('/'));
    let resp = handlers_http()
        .post(&url)
        .json(&json!({ "source": source, "id": id }))
        .send()
        .await
        .map_err(|e| json!({ "errors": [{ "field": "toolgate", "message": e.to_string() }] }))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| json!({ "errors": [{ "field": "toolgate", "message": e.to_string() }] }))?;
    if body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        Ok(())
    } else {
        Err(body)
    }
}

const MAX_HANDLER_BYTES: usize = 256 * 1024;
```

- [ ] **Step 3: Add the create + edit routes**

```rust
        .route("/api/handlers", get(api_list_handlers).post(api_create_handler))
        .route(
            "/api/handlers/{id}",
            axum::routing::put(api_update_handler).delete(api_delete_handler),
        )
```

(Keep the existing `/api/handlers` GET; `.post(...)` chains onto it. `/api/handlers/{id}` is a distinct path from `/api/handlers/{id}/source` and `/api/handlers/allowlist` — axum literal segments take priority, but keep `allowlist` + `{id}/source` registered as their own routes.)

Handlers:

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct CreateHandlerBody { pub id: String, pub source: String }
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateHandlerBody { pub source: String }

fn too_big(src: &str) -> bool { src.len() > MAX_HANDLER_BYTES }

async fn write_and_refresh(
    handlers: &HandlerRegistry, path: &std::path::Path, source: &str,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() { tokio::fs::create_dir_all(dir).await?; }
    tokio::fs::write(path, source).await?;
    handlers.refresh().await; // best-effort; toolgate hot-reloads via watchfiles
    Ok(())
}

/// `POST /api/handlers` — create a NEW workspace handler. Rejects a builtin id
/// (edit it via PUT to create an override) or an existing workspace file.
async fn api_create_handler(
    State(infra): State<InfraServices>,
    State(config): State<crate::gateway::clusters::ConfigServices>,
    State(handlers): State<HandlerRegistry>,
    Json(body): Json<CreateHandlerBody>,
) -> impl IntoResponse {
    if !valid_handler_id(&body.id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid handler id" }))).into_response();
    }
    if too_big(&body.source) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "source too large" }))).into_response();
    }
    if is_builtin_id(&body.id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "id is a builtin; edit it (PUT) to create an override" }))).into_response();
    }
    let path = workspace_handler_path(&body.id);
    if path.exists() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "handler already exists" }))).into_response();
    }
    let toolgate_url = config.config.toolgate_url.clone().unwrap_or_else(|| "http://localhost:9011".to_string());
    if let Err(errs) = toolgate_validate(&toolgate_url, &body.id, &body.source).await {
        return (StatusCode::BAD_REQUEST, Json(errs)).into_response();
    }
    if let Err(e) = write_and_refresh(&handlers, &path, &body.source).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
    }
    crate::db::audit::audit_spawn(infra.db.clone(), String::new(),
        crate::db::audit::event_types::HANDLER_CREATED, Some("ui".into()),
        json!({ "id": body.id }));
    (StatusCode::CREATED, Json(json!({ "id": body.id }))).into_response()
}

/// `PUT /api/handlers/{id}` — edit. Builtin id → writes/updates the workspace
/// override; workspace id → overwrites its file.
async fn api_update_handler(
    State(infra): State<InfraServices>,
    State(config): State<crate::gateway::clusters::ConfigServices>,
    State(handlers): State<HandlerRegistry>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateHandlerBody>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid handler id" }))).into_response();
    }
    if too_big(&body.source) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "source too large" }))).into_response();
    }
    let toolgate_url = config.config.toolgate_url.clone().unwrap_or_else(|| "http://localhost:9011".to_string());
    if let Err(errs) = toolgate_validate(&toolgate_url, &id, &body.source).await {
        return (StatusCode::BAD_REQUEST, Json(errs)).into_response();
    }
    let path = workspace_handler_path(&id);
    if let Err(e) = write_and_refresh(&handlers, &path, &body.source).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
    }
    crate::db::audit::audit_spawn(infra.db.clone(), String::new(),
        crate::db::audit::event_types::HANDLER_UPDATED, Some("ui".into()),
        json!({ "id": id }));
    (StatusCode::OK, Json(json!({ "id": id }))).into_response()
}
```

(`ConfigServices` `FromRef<AppState>` must exist — it does, used by `files.rs`. Import path: `crate::gateway::clusters::ConfigServices`.)

- [ ] **Step 4: Write the failing/passing unit tests (validation-guard logic, no DB)**

Add tests that exercise the pure guards (`valid_handler_id`, `is_builtin_id`, `too_big`, `workspace_handler_path`):

```rust
    #[test]
    fn create_guards() {
        assert!(is_builtin_id("transcribe"));
        assert!(!is_builtin_id("my_ocr"));
        assert!(too_big(&"x".repeat(MAX_HANDLER_BYTES + 1)));
        assert!(!too_big("small"));
        assert_eq!(
            workspace_handler_path("my_ocr"),
            std::path::Path::new("workspace/file_handlers/my_ocr.py")
        );
    }
```

Run: `cargo test -p opex-core --lib handlers_admin`

- [ ] **Step 5: Compile clean + commit**

Run: `cargo check --all-targets && cargo clippy --all-targets -- -D warnings`

```bash
git add crates/opex-core/src/gateway/handlers/handlers_admin.rs crates/opex-core/src/db/audit.rs
git commit -m "feat(handlers): POST /api/handlers + PUT /api/handlers/{id} (validate via toolgate, write workspace, audit)"
```

---

### Task C4: Delete / reset (`DELETE /api/handlers/{id}`)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/handlers_admin.rs`

**Interfaces:**
- Produces: `DELETE /api/handlers/{id}` → 200. Workspace id → delete the file. Builtin id with an override file → delete the override (reset to default). Pristine builtin (no override) → 400.

- [ ] **Step 1: Add the handler** (route already added in C3 Step 3)

```rust
/// `DELETE /api/handlers/{id}` — delete a workspace handler, or RESET a builtin
/// (delete its override → the pristine builtin resurfaces). Pristine builtin → 400.
async fn api_delete_handler(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid handler id" }))).into_response();
    }
    let path = workspace_handler_path(&id);
    if !path.exists() {
        // No workspace file: a pristine builtin cannot be deleted; anything else is 404.
        let code = if is_builtin_id(&id) { StatusCode::BAD_REQUEST } else { StatusCode::NOT_FOUND };
        let msg = if is_builtin_id(&id) { "builtin handlers cannot be deleted (already at default)" } else { "handler not found" };
        return (code, Json(json!({ "error": msg }))).into_response();
    }
    if let Err(e) = tokio::fs::remove_file(&path).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
    }
    handlers.refresh().await;
    crate::db::audit::audit_spawn(infra.db.clone(), String::new(),
        crate::db::audit::event_types::HANDLER_DELETED, Some("ui".into()),
        json!({ "id": id, "reset": is_builtin_id(&id) }));
    let reset = is_builtin_id(&id);
    (StatusCode::OK, Json(json!({ "id": id, "reset": reset }))).into_response()
}
```

- [ ] **Step 2: Compile + clippy + commit**

Run: `cargo check --all-targets && cargo clippy --all-targets -- -D warnings && cargo test -p opex-core --lib handlers_admin`

```bash
git add crates/opex-core/src/gateway/handlers/handlers_admin.rs
git commit -m "feat(handlers): DELETE /api/handlers/{id} (delete workspace / reset builtin override)"
```

---

# Phase U — UI (editor + form + tab actions)

### Task U1: Types + query hooks

**Files:**
- Modify: `ui/src/types/api.ts`
- Modify: `ui/src/lib/queries.ts`

**Interfaces:**
- Produces: `HandlerSourceDto`; `HandlerAdminRow.source`; hooks `useHandlerSource(id)`, `useCreateHandler()`, `useUpdateHandler()`, `useDeleteHandler()`.

- [ ] **Step 1: Types** — in `ui/src/types/api.ts`, add `source: "builtin" | "override" | "workspace";` to `HandlerAdminRow`, and:

```ts
export interface HandlerSourceDto {
  id: string;
  source: string;
  source_kind: "builtin" | "override" | "workspace";
}
```

- [ ] **Step 2: Hooks** — in `ui/src/lib/queries.ts` add (import `HandlerSourceDto`; `apiGet/apiPost/apiPut/apiDelete` already imported):

```ts
export function useHandlerSource(id: string | null) {
  return useQuery({
    queryKey: ["handlers", "source", id],
    queryFn: () => apiGet<HandlerSourceDto>(`/api/handlers/${id}/source`),
    enabled: !!id,
  })
}

export function useCreateHandler() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { id: string; source: string }) =>
      apiPost<{ id: string }>("/api/handlers", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.handlers }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useUpdateHandler() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { id: string; source: string }) =>
      apiPut<{ id: string }>(`/api/handlers/${data.id}`, { source: data.source }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.handlers }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useDeleteHandler() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => apiDelete<{ id: string }>(`/api/handlers/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.handlers }),
    onError: (e: Error) => toast.error(e.message),
  })
}
```

> **NB:** `apiPost/apiPut/apiDelete` throw on non-2xx with the response body's `error`. The create/edit 400 (validation) surfaces as a thrown Error whose message is the JSON error — the editor (U2) instead calls the endpoint directly to render structured `errors[]` inline (see U2). These hooks are for the simple delete/reset + optimistic-list paths.

- [ ] **Step 3: Build + typecheck**

Run: `cd ui && npm run build`  → clean.

- [ ] **Step 4: Commit**

```bash
git add ui/src/types/api.ts ui/src/lib/queries.ts
git commit -m "feat(ui): handler source + create/update/delete query hooks"
```

---

### Task U2: Descriptor-block render util + HandlerEditor component

**Files:**
- Create: `ui/src/app/(authenticated)/tools/handler-descriptor.ts`
- Create: `ui/src/app/(authenticated)/tools/HandlerEditor.tsx`
- Test: `ui/src/app/(authenticated)/tools/__tests__/handler-descriptor.test.ts`

**Interfaces:**
- Produces: `renderDescriptorBlock(fields: DescriptorFields): string` and `spliceDescriptor(source: string, fields: DescriptorFields): string`; `<HandlerEditor id? source sourceKind onSaved onClose />`.

- [ ] **Step 1: Write the failing test for the pure render util**

Create `handler-descriptor.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { renderDescriptorBlock, spliceDescriptor } from "../handler-descriptor";

const FIELDS = {
  id: "my_ocr", labels: { en: "OCR", ru: "ОЦР" }, descriptions: {},
  icon: "file", mime: ["image/*"], max_size_mb: 20, execution: "sync" as const,
  order: 100, enabled: true,
};

describe("descriptor block", () => {
  it("renders a # <handler> comment block with the fields", () => {
    const b = renderDescriptorBlock(FIELDS);
    expect(b).toMatch(/^# <handler>/);
    expect(b).toContain("#   <id>my_ocr</id>");
    expect(b).toContain('#   <label lang="en">OCR</label>');
    expect(b).toContain("#     <mime>image/*</mime>");
    expect(b).toContain("#     <max_size_mb>20</max_size_mb>");
    expect(b).toContain("#   <execution>sync</execution>");
    expect(b).toContain("# </handler>");
  });

  it("splices over an existing block, preserving the code body", () => {
    const src = "# <handler>\n#   <id>old</id>\n# </handler>\nasync def run(): pass\n";
    const out = spliceDescriptor(src, FIELDS);
    expect(out).toContain("<id>my_ocr</id>");
    expect(out).toContain("async def run(): pass");
    expect(out).not.toContain("<id>old</id>");
  });
});
```

Run: `cd ui && npx vitest run "src/app/(authenticated)/tools/__tests__/handler-descriptor.test.ts"` → FAIL.

- [ ] **Step 2: Implement `handler-descriptor.ts`**

```ts
export interface DescriptorFields {
  id: string;
  labels: Record<string, string>;
  descriptions: Record<string, string>;
  icon: string;
  mime: string[];
  max_size_mb: number | null;
  execution: "sync" | "async";
  order: number;
  enabled: boolean;
}

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** Render the `# <handler> … # </handler>` comment block from descriptor fields. */
export function renderDescriptorBlock(f: DescriptorFields): string {
  const L: string[] = ["# <handler>", `#   <id>${esc(f.id)}</id>`];
  for (const [lang, txt] of Object.entries(f.labels)) L.push(`#   <label lang="${esc(lang)}">${esc(txt)}</label>`);
  for (const [lang, txt] of Object.entries(f.descriptions)) if (txt) L.push(`#   <description lang="${esc(lang)}">${esc(txt)}</description>`);
  if (f.icon) L.push(`#   <icon>${esc(f.icon)}</icon>`);
  L.push("#   <match>");
  for (const m of f.mime) L.push(`#     <mime>${esc(m)}</mime>`);
  if (f.max_size_mb != null) L.push(`#     <max_size_mb>${f.max_size_mb}</max_size_mb>`);
  L.push("#   </match>");
  L.push(`#   <execution>${f.execution}</execution>`);
  L.push(`#   <order>${f.order}</order>`);
  L.push(`#   <enabled>${f.enabled}</enabled>`);
  L.push("# </handler>");
  return L.join("\n");
}

/** Replace an existing leading descriptor block (or prepend one) in `source`. */
export function spliceDescriptor(source: string, f: DescriptorFields): string {
  const block = renderDescriptorBlock(f);
  const re = /# <handler>[\s\S]*?# <\/handler>/;
  return re.test(source) ? source.replace(re, block) : `${block}\n${source}`;
}
```

Run: `cd ui && npx vitest run "src/app/(authenticated)/tools/__tests__/handler-descriptor.test.ts"` → PASS.

- [ ] **Step 3: Build the `HandlerEditor` component**

Create `HandlerEditor.tsx` — a dialog/sheet holding: a descriptor form (id read-only on edit; labels ru/en, icon, mime globs, max_size_mb, execution select, order, enabled switch), a **CodeMirror (python)** editor for the full `.py`, a **Save** button, and an inline errors area. On save it POSTs (create) or PUTs (edit) the CURRENT source; on a 400 it parses `{errors:[{field,message}]}` and shows them inline in red (block-on-error), on success it calls `onSaved()` (which invalidates the handlers query). Form changes call `spliceDescriptor(source, fields)` to keep the source in sync; a "Sync from code" action calls the validate endpoint and, on `ok`, repopulates the form from the returned `descriptor`. Ground it in the existing CodeMirror usage (search `@uiw/react-codemirror` / `python()` in the repo — the `/workspace` editor and YAML-tools editor use CodeMirror; mirror their imports + theme). Save calls the endpoints directly (not the throwing hooks) to render structured errors:

```tsx
// on save (create):
const res = await fetch("/api/handlers", { method: "POST", headers: authJsonHeaders(), body: JSON.stringify({ id, source }) });
if (!res.ok) { setErrors(((await res.json()).errors) ?? [{ field: "", message: `HTTP ${res.status}` }]); return; }
// edit: PUT `/api/handlers/${id}` with { source }
```

Use the repo's auth-header helper (the same one `apiGet` uses — `getToken()` from `@/lib/api`). Keep the component focused; if it grows past ~250 lines, split the form into a child `DescriptorForm.tsx`.

- [ ] **Step 4: Verify build**

Run: `cd ui && npm run build` → clean (component compiles; wired into the tab in U3).

- [ ] **Step 5: Commit**

```bash
git add "ui/src/app/(authenticated)/tools/handler-descriptor.ts" \
        "ui/src/app/(authenticated)/tools/HandlerEditor.tsx" \
        "ui/src/app/(authenticated)/tools/__tests__/handler-descriptor.test.ts"
git commit -m "feat(ui): HandlerEditor (CodeMirror + descriptor form) + descriptor-block render util"
```

---

### Task U3: Wire actions into the File Handlers tab

**Files:**
- Modify: `ui/src/app/(authenticated)/tools/page.tsx`
- Modify: `ui/src/i18n/locales/en.json` + `ru.json`
- Test: extend `ui/src/app/(authenticated)/tools/__tests__/handlers-tab.test.tsx`

**Interfaces:** consumes U1 hooks + U2 `HandlerEditor`.

- [ ] **Step 1: i18n keys (both locales, flat dotted)** — add after the existing `tools.handler_*` keys:

`en.json`: `"tools.handler_edit": "Edit"`, `"tools.handler_create": "Create handler"`, `"tools.handler_delete": "Delete"`, `"tools.handler_reset": "Reset to default"`, `"tools.handler_source_builtin": "default"`, `"tools.handler_source_override": "edited"`, `"tools.handler_source_workspace": "workspace"`, `"tools.handler_save": "Save"`, `"tools.handler_id": "Handler id"`, `"tools.handler_invalid": "Fix the errors before saving"`.
`ru.json`: same keys — `"Редактировать"`, `"Создать обработчик"`, `"Удалить"`, `"Сбросить к эталону"`, `"эталон"`, `"изменён"`, `"workspace"`, `"Сохранить"`, `"ID обработчика"`, `"Исправьте ошибки перед сохранением"`.

- [ ] **Step 2: Card actions + badge in `renderHandlerCard`** — add an **Edit** button (opens `HandlerEditor` with the id; the editor fetches source via `useHandlerSource`), a **Delete/Reset** button (`useDeleteHandler().mutate(h.id)` — label "Reset to default" when `h.source === "override"`, "Delete" when `h.source === "workspace"`, hidden when `h.source === "builtin"`), and a status badge from `h.source` (`builtin→default`, `override→edited`, `workspace→workspace`). Add a **"Create handler"** button in the tab header opening `HandlerEditor` in create mode (empty id + a starter template string).

- [ ] **Step 3: Extend the vitest**

In `handlers-tab.test.tsx`, add a case: the workspace handler card shows an "Edit" action and a "Delete" action; the builtin card (source `builtin`) shows Edit but not Delete; clicking Delete on the workspace card calls the delete mutation with its id. Mock `useHandlerSource`/`useCreateHandler`/`useUpdateHandler`/`useDeleteHandler` in the existing `@/lib/queries` mock (add them alongside the current handler mocks). Give the mocked rows a `source` field.

- [ ] **Step 4: Gate**

Run: `cd ui && npm test` (green) + `cd ui && npm run build` (clean).

- [ ] **Step 5: Commit**

```bash
git add "ui/src/app/(authenticated)/tools/page.tsx" ui/src/i18n/locales/en.json ui/src/i18n/locales/ru.json \
        "ui/src/app/(authenticated)/tools/__tests__/handlers-tab.test.tsx"
git commit -m "feat(ui): File Handlers tab — edit/create/delete/reset actions + status badges"
```

---

# Phase D — Gate + deploy

### Task D1: Whole-feature gate + deploy + E2E

**Files:** none (verification + deploy).

- [ ] **Step 1: Full gate**

Run: `cargo check --all-targets` → 0; `cargo clippy --all-targets -- -D warnings` → 0; `cd toolgate && python -m pytest -q` → green; `cd ui && npm test` → green; `cd ui && npm run build` → clean.

- [ ] **Step 2: Deploy** (per the established runbook)

- Push (with approval): `git push origin master`.
- Core + toolgate: `ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'` (builds core, syncs toolgate `.py` incl. `handlers/` subpackage — confirm `validate.py` landed under `~/opex/toolgate/handlers/`, restarts).
- UI: `cd ui && npm run build`, tar `ui/out`, scp, atomic-swap `~/opex/ui/out`.

- [ ] **Step 3: E2E on the server**

- Create a workspace handler from the UI (or `POST /api/handlers`) → it appears in `GET /api/files/{id}/actions` for a matching file.
- Edit a builtin (`PUT /api/handlers/transcribe` with a tweaked source) → `GET /api/handlers` shows `source:"override"`; `GET /api/handlers/transcribe/source` returns the override.
- Reset (`DELETE /api/handlers/transcribe`) → `source:"builtin"` again.
- Invalid save (`POST` with broken Python) → 400 with `errors[]`, and NO file written.
- `doctor` ok; no core/toolgate errors.

- [ ] **Step 4: Commit any deploy-doc note** (if CLAUDE.md needs the new endpoints noted).

---

## Self-Review

**1. Spec coverage:** Decision 1 (code+form) → U2/U3. Decision 2 (override) → T2 + C2/C3/C4. Decision 3 (block-on-error) → T1/T3 + C3. Decision 4 (core writes workspace) → C2/C3/C4. Decision 5 (operator-only) → Global Constraints + all endpoints behind auth. Decision 6 (id gating) → T2 (`tier` by builtin id) + unchanged `match_buttons`. `POST /handlers/validate` → T3. `source` field → T2/C1. All spec sections map to a task.

**2. Placeholder scan:** No TBD/TODO. Code shown for each step. The only prose-described piece is the `HandlerEditor` component body (U2 Step 3) — it names the exact endpoints, the error-render contract, the `spliceDescriptor`/validate calls, and the CodeMirror grounding; its pure logic (`handler-descriptor.ts`) is fully specified + tested. This is deliberate (a React dialog composed from existing patterns), not a gap.

**3. Type consistency:** `source` is `"builtin"|"override"|"workspace"` in toolgate manifest (T2), Rust `HandlerManifest.source`/`HandlerAdminRow.source` (C1), TS `HandlerAdminRow.source` + `HandlerSourceDto.source_kind` (U1). Endpoints: `POST /api/handlers {id,source}`, `PUT /api/handlers/{id} {source}`, `DELETE /api/handlers/{id}`, `GET /api/handlers/{id}/source` — consistent across C2/C3/C4 and U1 hooks. `DescriptorFields` shared by `renderDescriptorBlock`/`spliceDescriptor` (U2).

## Deploy notes

- toolgate `.py` (validate.py + loader.py + router.py) → server-deploy syncs `toolgate/` incl. subpackages; core restart re-spawns toolgate. Confirm `validate.py` is present under `~/opex/toolgate/handlers/`.
- Overrides live in `~/opex/workspace/file_handlers/` (survive deploy); builtins ship in the source tree.
- UI needs the manual `ui/out` swap.

## Out of scope / deferred (YAGNI)

- Param-schema editing via the form (code only in v1).
- Handler versioning / diff / history.
- Agent-authored handlers (untrusted isolation deferred).
- Sandboxed test-run before save (Decision #3 = block-on-error).
- The `*/*` mime-glob `save` bug (separate follow-up).
