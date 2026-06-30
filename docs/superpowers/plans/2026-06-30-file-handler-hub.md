# File Handler Hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract OPEX file processing out of the Rust core into self-describing Python handlers hosted in toolgate, surface per-file-type action buttons in the chat composer, and generalize the durable video queue into a universal async handler queue — without breaking existing behaviour.

**Architecture:** toolgate gains a *handler hub* — self-describing `.py` handlers (XML descriptor in a top comment) that reuse toolgate's provider registry. The Rust core becomes a thin orchestrator: a discovery cache of handler manifests, mime→buttons matching with a tiered trust gate, `GET /api/files/{upload_id}/actions` + `POST /api/files/{upload_id}/run`, provenance wrapping, and a **universal `handler_jobs` durable queue**. Sync handlers run inline on toolgate's event loop; heavy/async handlers (video) run **out-of-process** so the single-process HTTP facade is never blocked. The core downloads upload bytes over loopback in Rust and POSTs them as multipart (toolgate never fetches loopback URLs, which its SSRF guard blocks). v1 authoring is trusted-only (human + `base` agent); untrusted-agent isolation is explicitly deferred.

**Tech Stack:** Rust 2024 (axum 0.8, sqlx 0.8, reqwest 0.12 rustls-tls, tokio); Python 3.14 (FastAPI, httpx, pymupdf, python-docx, watchfiles, pytest-asyncio, respx); TypeScript/React (Next.js 16, React 19, vitest, @testing-library/react); PostgreSQL 17 + pgvector.

**Source spec:** [docs/superpowers/specs/2026-06-30-file-handler-hub-design.md](../specs/2026-06-30-file-handler-hub-design.md)

## Global Constraints

- **rustls-tls only** — never add OpenSSL (all reqwest/sqlx use rustls features).
- **TDD** — write the failing test first, then the minimal implementation, every task.
- **Frequent commits**, conventional messages; **NO `Co-Authored-By` trailer**.
- **Work in `master`**; **never `git push`** without explicit user approval.
- toolgate HTTP facade stays **`--workers 1 --loop asyncio`**; long/async handlers run **out-of-process** (a `runner.py` subprocess per job).
- toolgate uses **`watchfiles`** (already a dependency) for hot-reload — NOT `watchdog`.
- **toolgate must never fetch a loopback URL** (`validate_url_ssrf` blocks loopback by design). The core downloads upload bytes over loopback **in Rust** and sends them as `multipart` to `/handlers/{id}/run`.
- Migration numbers (next free; highest existing is `065`): **066** = `messages.source` column, **067** = `handler_jobs` table, **068** = `video_jobs` deprecation (non-destructive, no `DROP TABLE`).
- **Builtin handler ids are reserved**; **workspace-tier handlers are allowed-by-default** (valid only under the v1 trusted-author model).
- Bilingual **ru/en** labels everywhere a user-facing string is added.
- Wire type: toolgate emits `ScenarioOutcome` snake_case JSON `{status, summary_text, artifact_urls, reason}`; `status ∈ {ok, failed, unsupported, too_large, timeout}` (core's `agent/file_scenario/outcome.rs` is the source of truth; its extra `video_accepted` field defaults on deserialize and is ignored by toolgate).

## Scope adjustments from multi-agent review

This plan was drafted and then hardened across two adversarial critic passes. Two deliberate scope decisions came out of that:

1. **Legacy post-send chips + Telegram `fse:` callbacks are NOT migrated here.** They keep using the existing in-core `file_scenarios` mechanism (`ScenarioChoice{scenario_id: Uuid, …}`). The new **composer buttons** are the v1 surface. Migrating channels/chips onto `HandlerRegistry` is a future follow-up (it needs a handler→scenario_id mapping). Consequently the in-core *sync* dispatch (`dispatch.rs`, `dispatch_seam.rs`) **stays**; only the **video** pipeline is removed in Phase 6.
2. **Async video continuity:** Phase 3 keeps `summarize_video` on the legacy `video_jobs` path; Phase 5 builds the universal queue, ports `summarize_video` to a Python async handler, and re-points BOTH the composer async button (`files.rs`) and the legacy auto-YouTube-detection enqueue site onto `handler_jobs`; only then does Phase 6 delete the legacy video pipeline.

## Implementation errata (apply these when you reach the cited spots)

The final critic flagged three small mismatches between the drafted snippets and the live tree. Apply these:

1. **`uploads_local_url` module path.** It lives at **`crate::agent::url_tools::uploads_local_url`** (`pub(crate)`), NOT `crate::uploads`. In **Phase 3 / Task 5** (`files.rs` sync run path) use `crate::agent::url_tools::uploads_local_url`. (Phase 5 already uses the correct path.) `crate::uploads` only exports `mint_uploads_url` + `web_uploads_base`.
2. **Phase 4 / Task 3 composer edit.** The real `handleFileAdd` computes `uploadPath` via an IIFE (`(() => { try { return new URL(result.url).pathname } catch { return result.url } })()`) and uses `uuid()` (not `crypto.randomUUID()`). Apply the intended change — add `uploadId: result.filename` to the `AttachmentEntry` object — against that real surrounding code.
3. **Phase 2 / Task 3 `extract_document`.** Use the existing in-repo idiom `import docx; docx.Document(io.BytesIO(file.bytes))` (both `import docx` and `from docx import Document` resolve via python-docx; match the existing `routers/documents.py` style).

---

## Phase 1 — Contract + descriptor

This phase lays down the **schema single-source-of-truth** on both sides of the boundary: the Python `HandlerDescriptor` + XML parser/validator (`toolgate/handlers/descriptor.py`) that every later toolgate task consumes, and a Rust wire-contract test proving the `ScenarioOutcome` snake_case JSON that toolgate handlers will emit round-trips into the existing `agent/file_scenario/outcome.rs` type. No endpoints, no loader, no `ctx`, no builtins yet. Nothing in this phase changes runtime behaviour — it only adds new files and a frozen test.

Per **R9**, the Python `HandlerResult.to_dict` (4 keys) and Rust `ScenarioOutcome` serialization (5 keys, `video_accepted` always present via `#[serde(default)]` with no `skip_serializing_if`) are deliberately asymmetric but wire-compatible: core deserializes the 4-key toolgate JSON and `video_accepted` defaults to `false`; toolgate ignores any extra key. Task 4 asserts that asymmetric round-trip explicitly rather than exact-key equality.

---

### Task 1: Package skeleton + `DescriptorError` and `HandlerDescriptor` dataclass

**Files:**
- Create: `toolgate/handlers/__init__.py`
- Create: `toolgate/handlers/descriptor.py`
- Test: `toolgate/tests/test_handlers_descriptor.py`

**Interfaces:**
- Consumes: nothing (first task in project).
- Produces: `class DescriptorError(Exception)`; `@dataclass HandlerDescriptor{id:str, labels:dict[str,str], descriptions:dict[str,str], icon:str, match_mimes:list[str], max_size_mb:int|None, capability:str|None, execution:str, output:str, params:list[dict], order:int, enabled:bool, tier:str}` — consumed verbatim by Phase 2 `loader.py`, `context.py`, `router.py` and by Phase 3 Rust `HandlerManifest`.

- [ ] **Step 1: Write the failing test** (construct the dataclass + raise the error directly, no parser yet)

```python
# toolgate/tests/test_handlers_descriptor.py
"""Unit tests for toolgate.handlers.descriptor."""

import pytest

from handlers.descriptor import DescriptorError, HandlerDescriptor


def test_descriptor_error_is_exception():
    err = DescriptorError("bad descriptor")
    assert isinstance(err, Exception)
    assert str(err) == "bad descriptor"


def test_handler_descriptor_holds_all_fields():
    d = HandlerDescriptor(
        id="transcribe",
        labels={"ru": "Транскрибировать", "en": "Transcribe"},
        descriptions={"ru": "Речь в текст", "en": "Speech to text"},
        icon="mic",
        match_mimes=["audio/*", "video/*"],
        max_size_mb=200,
        capability="stt",
        execution="sync",
        output="text",
        params=[{"name": "language", "type": "string", "default": "ru", "required": False}],
        order=10,
        enabled=True,
        tier="builtin",
    )
    assert d.id == "transcribe"
    assert d.labels["ru"] == "Транскрибировать"
    assert d.match_mimes == ["audio/*", "video/*"]
    assert d.max_size_mb == 200
    assert d.capability == "stt"
    assert d.execution == "sync"
    assert d.output == "text"
    assert d.params[0]["name"] == "language"
    assert d.order == 10
    assert d.enabled is True
    assert d.tier == "builtin"


def test_handler_descriptor_optional_fields_default_to_none():
    d = HandlerDescriptor(
        id="save",
        labels={"en": "Save"},
        descriptions={},
        icon="save",
        match_mimes=["*/*"],
        max_size_mb=None,
        capability=None,
        execution="sync",
        output="file",
        params=[],
        order=99,
        enabled=True,
        tier="builtin",
    )
    assert d.max_size_mb is None
    assert d.capability is None
    assert d.descriptions == {}
```

- [ ] **Step 2: Run test to verify it fails** — `cd toolgate && pytest tests/test_handlers_descriptor.py -q`
  Expected failure: `ModuleNotFoundError: No module named 'handlers'` (package + module do not exist yet).

- [ ] **Step 3: Write minimal implementation**

```python
# toolgate/handlers/__init__.py
"""toolgate handler hub — self-describing Python file handlers."""
```

```python
# toolgate/handlers/descriptor.py
"""HandlerDescriptor dataclass + XML descriptor parser/validator.

Single source of truth for the handler schema. One handler = one .py file
whose leading "# <handler> ... # </handler>" comment block describes it.
"""

from __future__ import annotations

from dataclasses import dataclass


class DescriptorError(Exception):
    """Raised when a handler descriptor block is missing, malformed, or invalid."""


@dataclass
class HandlerDescriptor:
    id: str
    labels: dict[str, str]
    descriptions: dict[str, str]
    icon: str
    match_mimes: list[str]
    max_size_mb: int | None
    capability: str | None
    execution: str  # "sync" | "async"
    output: str  # "text" | "file" | "card"
    params: list[dict]
    order: int
    enabled: bool
    tier: str  # "builtin" | "workspace"
```

- [ ] **Step 4: Run test to verify it passes** — `cd toolgate && pytest tests/test_handlers_descriptor.py -q`
  Expected: `3 passed`.

- [ ] **Step 5: Commit**
```
git add toolgate/handlers/__init__.py toolgate/handlers/descriptor.py toolgate/tests/test_handlers_descriptor.py
git commit -m "feat(toolgate): add HandlerDescriptor dataclass + DescriptorError"
```

---

### Task 2: `parse_descriptor` — happy path (extract block, strip "# ", parse XML)

**Files:**
- Modify: `toolgate/handlers/descriptor.py`
- Test: `toolgate/tests/test_handlers_descriptor.py`

**Interfaces:**
- Consumes: `HandlerDescriptor`, `DescriptorError` (Task 1).
- Produces: `parse_descriptor(source: str, tier: str) -> HandlerDescriptor` — consumed by Phase 2 `loader.py`.

- [ ] **Step 1: Write the failing test** (append to `test_handlers_descriptor.py`)

```python
from handlers.descriptor import parse_descriptor

_TRANSCRIBE_SRC = '''\
# <handler>
#   <id>transcribe</id>
#   <label lang="ru">Транскрибировать</label>
#   <label lang="en">Transcribe</label>
#   <description lang="ru">Речь из аудио/видео в текст</description>
#   <description lang="en">Speech from audio/video to text</description>
#   <icon>mic</icon>
#   <match>
#     <mime>audio/*</mime>
#     <mime>video/*</mime>
#     <max_size_mb>200</max_size_mb>
#   </match>
#   <capability>stt</capability>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>

async def run(ctx, file, params):
    return ctx.result.text("hi")
'''


def test_parse_descriptor_happy_path():
    d = parse_descriptor(_TRANSCRIBE_SRC, tier="builtin")
    assert d.id == "transcribe"
    assert d.labels == {"ru": "Транскрибировать", "en": "Transcribe"}
    assert d.descriptions["ru"] == "Речь из аудио/видео в текст"
    assert d.icon == "mic"
    assert d.match_mimes == ["audio/*", "video/*"]
    assert d.max_size_mb == 200
    assert d.capability == "stt"
    assert d.execution == "sync"
    assert d.output == "text"
    assert d.params == [
        {"name": "language", "type": "string", "default": "ru", "required": False}
    ]
    assert d.order == 10
    assert d.enabled is True
    assert d.tier == "builtin"


def test_parse_descriptor_minimal_defaults():
    src = '''\
# <handler>
#   <id>save</id>
#   <label lang="en">Save</label>
#   <icon>save</icon>
#   <match>
#     <mime>*/*</mime>
#   </match>
#   <execution>sync</execution>
# </handler>

async def run(ctx, file, params):
    return ctx.result.text("")
'''
    d = parse_descriptor(src, tier="workspace")
    assert d.id == "save"
    assert d.labels == {"en": "Save"}
    assert d.descriptions == {}
    assert d.match_mimes == ["*/*"]
    assert d.max_size_mb is None
    assert d.capability is None
    assert d.output == "text"  # default when <output> omitted
    assert d.params == []
    assert d.order == 100  # default when <order> omitted
    assert d.enabled is True  # default when <enabled> omitted
    assert d.tier == "workspace"
```

- [ ] **Step 2: Run test to verify it fails** — `cd toolgate && pytest tests/test_handlers_descriptor.py -q`
  Expected failure: `ImportError: cannot import name 'parse_descriptor' from 'handlers.descriptor'`.

- [ ] **Step 3: Write minimal implementation** (add imports + extractor + parser to `descriptor.py`)

Add `import re` and `import xml.etree.ElementTree as ET` to the imports, then append:

```python
_BLOCK_RE = re.compile(r"#\s*<handler>(.*?)#\s*</handler>", re.DOTALL)


def _extract_block(source: str) -> str:
    """Pull the leading '# <handler> ... # </handler>' comment block out of a
    handler source file and strip the leading '# ' from each line so the
    remainder is valid XML."""
    m = _BLOCK_RE.search(source)
    if not m:
        raise DescriptorError("no <handler> descriptor block found")
    inner = m.group(0)
    lines = []
    for line in inner.splitlines():
        stripped = line.lstrip()
        if not stripped.startswith("#"):
            continue
        # remove the leading '#' and a single following space if present
        body = stripped[1:]
        if body.startswith(" "):
            body = body[1:]
        lines.append(body)
    return "\n".join(lines)


def _text(el, tag: str, default: str | None = None) -> str | None:
    child = el.find(tag)
    if child is None or child.text is None:
        return default
    return child.text.strip()


def parse_descriptor(source: str, tier: str) -> HandlerDescriptor:
    """Parse a handler source file's descriptor block into a validated
    HandlerDescriptor. Raises DescriptorError on any structural or validation
    failure (fail-closed)."""
    xml_str = _extract_block(source)
    try:
        root = ET.fromstring(xml_str)
    except ET.ParseError as e:
        raise DescriptorError(f"malformed descriptor XML: {e}") from e

    labels: dict[str, str] = {}
    for el in root.findall("label"):
        lang = el.get("lang")
        if lang and el.text:
            labels[lang] = el.text.strip()

    descriptions: dict[str, str] = {}
    for el in root.findall("description"):
        lang = el.get("lang")
        if lang and el.text:
            descriptions[lang] = el.text.strip()

    match_el = root.find("match")
    match_mimes: list[str] = []
    max_size_mb: int | None = None
    if match_el is not None:
        for m in match_el.findall("mime"):
            if m.text:
                match_mimes.append(m.text.strip())
        size_txt = _text(match_el, "max_size_mb")
        if size_txt is not None:
            try:
                max_size_mb = int(size_txt)
            except ValueError as e:
                raise DescriptorError(
                    f"descriptor max_size_mb must be an integer, got '{size_txt}'"
                ) from e

    params: list[dict] = []
    params_el = root.find("params")
    if params_el is not None:
        for p in params_el.findall("param"):
            params.append(
                {
                    "name": p.get("name", ""),
                    "type": p.get("type", "string"),
                    "default": p.get("default"),
                    "required": p.get("required", "false").strip().lower() == "true",
                }
            )

    order_txt = _text(root, "order")
    enabled_txt = _text(root, "enabled")

    return HandlerDescriptor(
        id=(_text(root, "id") or "").strip(),
        labels=labels,
        descriptions=descriptions,
        icon=_text(root, "icon", "file") or "file",
        match_mimes=match_mimes,
        max_size_mb=max_size_mb,
        capability=_text(root, "capability"),
        execution=(_text(root, "execution") or "").strip(),
        output=_text(root, "output", "text") or "text",
        params=params,
        order=int(order_txt) if order_txt is not None else 100,
        enabled=(enabled_txt is None) or enabled_txt.strip().lower() == "true",
        tier=tier,
    )
```

- [ ] **Step 4: Run test to verify it passes** — `cd toolgate && pytest tests/test_handlers_descriptor.py -q`
  Expected: `5 passed`.

- [ ] **Step 5: Commit**
```
git add toolgate/handlers/descriptor.py toolgate/tests/test_handlers_descriptor.py
git commit -m "feat(toolgate): parse_descriptor extracts + parses handler XML block"
```

---

### Task 3: `parse_descriptor` fail-closed validation (bad id, missing mime, bad execution, missing block, missing label)

**Files:**
- Modify: `toolgate/handlers/descriptor.py`
- Test: `toolgate/tests/test_handlers_descriptor.py`

**Interfaces:**
- Consumes: `parse_descriptor`, `DescriptorError` (Task 2).
- Produces: the validation contract relied on by Phase 2 `loader.py`, which catches `DescriptorError` per-file and skips+logs (fail-closed: one bad handler never breaks the registry). Per the contract, `parse_descriptor` validates: `id` matches `^[a-z0-9_-]+$`, `>=1` mime, `execution in {sync,async}`, plus `>=1` label and a present `<handler>` block.

- [ ] **Step 1: Write the failing test** (append to `test_handlers_descriptor.py`)

```python
def test_parse_descriptor_rejects_missing_block():
    with pytest.raises(DescriptorError, match="no <handler> descriptor block"):
        parse_descriptor("async def run(ctx, file, params):\n    pass\n", tier="builtin")


def test_parse_descriptor_rejects_empty_id():
    src = '''\
# <handler>
#   <label lang="en">X</label>
#   <match><mime>*/*</mime></match>
#   <execution>sync</execution>
# </handler>
'''
    with pytest.raises(DescriptorError, match="id"):
        parse_descriptor(src, tier="builtin")


def test_parse_descriptor_rejects_bad_id_chars():
    src = '''\
# <handler>
#   <id>Bad ID!</id>
#   <label lang="en">X</label>
#   <match><mime>*/*</mime></match>
#   <execution>sync</execution>
# </handler>
'''
    with pytest.raises(DescriptorError, match="id"):
        parse_descriptor(src, tier="builtin")


def test_parse_descriptor_rejects_no_mime():
    src = '''\
# <handler>
#   <id>nomime</id>
#   <label lang="en">X</label>
#   <match></match>
#   <execution>sync</execution>
# </handler>
'''
    with pytest.raises(DescriptorError, match="mime"):
        parse_descriptor(src, tier="builtin")


def test_parse_descriptor_rejects_missing_match_element():
    src = '''\
# <handler>
#   <id>nomatch</id>
#   <label lang="en">X</label>
#   <execution>sync</execution>
# </handler>
'''
    with pytest.raises(DescriptorError, match="mime"):
        parse_descriptor(src, tier="builtin")


def test_parse_descriptor_rejects_bad_execution():
    src = '''\
# <handler>
#   <id>badexec</id>
#   <label lang="en">X</label>
#   <match><mime>*/*</mime></match>
#   <execution>maybe</execution>
# </handler>
'''
    with pytest.raises(DescriptorError, match="execution"):
        parse_descriptor(src, tier="builtin")


def test_parse_descriptor_accepts_async_execution():
    src = '''\
# <handler>
#   <id>summarize_video</id>
#   <label lang="en">Summarize</label>
#   <match><mime>video/*</mime></match>
#   <execution>async</execution>
# </handler>
'''
    d = parse_descriptor(src, tier="builtin")
    assert d.execution == "async"


def test_parse_descriptor_rejects_missing_label():
    src = '''\
# <handler>
#   <id>nolabel</id>
#   <match><mime>*/*</mime></match>
#   <execution>sync</execution>
# </handler>
'''
    with pytest.raises(DescriptorError, match="label"):
        parse_descriptor(src, tier="builtin")
```

- [ ] **Step 2: Run test to verify it fails** — `cd toolgate && pytest tests/test_handlers_descriptor.py -q`
  Expected failure: the current `parse_descriptor` does not validate, so the rejection tests fail — e.g. `test_parse_descriptor_rejects_no_mime` returns a descriptor instead of raising (`DID NOT RAISE DescriptorError`), and `test_parse_descriptor_rejects_bad_id_chars` likewise returns instead of raising.

- [ ] **Step 3: Write minimal implementation** — add two module constants and a validation block in `parse_descriptor` just before the `return HandlerDescriptor(...)`.

Add near the top of `descriptor.py` (after `_BLOCK_RE`):

```python
_ID_RE = re.compile(r"^[a-z0-9_-]+$")
_VALID_EXECUTION = {"sync", "async"}
```

Insert this validation block immediately before the `return HandlerDescriptor(...)` statement. It computes `hid` and `execution` once, validates them, then the `return` is updated to reuse those locals:

```python
    hid = (_text(root, "id") or "").strip()
    execution = (_text(root, "execution") or "").strip()

    if not hid:
        raise DescriptorError("descriptor missing required <id>")
    if not _ID_RE.match(hid):
        raise DescriptorError(
            f"descriptor id '{hid}' must match ^[a-z0-9_-]+$"
        )
    if not labels:
        raise DescriptorError(f"descriptor '{hid}' missing required <label>")
    if not match_mimes:
        raise DescriptorError(
            f"descriptor '{hid}' must declare at least one <mime>"
        )
    if execution not in _VALID_EXECUTION:
        raise DescriptorError(
            f"descriptor '{hid}' execution must be 'sync' or 'async', got '{execution}'"
        )
```

Then change the `return HandlerDescriptor(...)` constructor to reuse the validated locals instead of re-reading from `root`:

```python
    return HandlerDescriptor(
        id=hid,
        labels=labels,
        descriptions=descriptions,
        icon=_text(root, "icon", "file") or "file",
        match_mimes=match_mimes,
        max_size_mb=max_size_mb,
        capability=_text(root, "capability"),
        execution=execution,
        output=_text(root, "output", "text") or "text",
        params=params,
        order=int(order_txt) if order_txt is not None else 100,
        enabled=(enabled_txt is None) or enabled_txt.strip().lower() == "true",
        tier=tier,
    )
```

- [ ] **Step 4: Run test to verify it passes** — `cd toolgate && pytest tests/test_handlers_descriptor.py -q`
  Expected: `13 passed` (5 from Tasks 1-2 + 8 new validation/acceptance tests).

- [ ] **Step 5: Commit**
```
git add toolgate/handlers/descriptor.py toolgate/tests/test_handlers_descriptor.py
git commit -m "feat(toolgate): fail-closed descriptor validation (id/label/mime/execution)"
```

---

### Task 4: Rust wire-contract test — toolgate JSON round-trips into `ScenarioOutcome`

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/outcome.rs` (extend the existing `#[cfg(test)] mod tests` block only — no production code change)
- Test: same file (Rust unit tests).

**Interfaces:**
- Consumes: existing `ScenarioOutcome { status, summary_text, artifact_urls, reason, video_accepted }` and `ScenarioStatus` (snake_case serde) from `agent/file_scenario/outcome.rs`. Constructor in scope: `ScenarioOutcome::ok(summary_text: String, artifact_urls: Vec<String>)`.
- Produces: a frozen wire-contract assertion that the 4-key JSON the Phase 2 Python `ResultBuilder` emits — `{"status":"ok","summary_text":..,"artifact_urls":[..],"reason":null}` — deserialises into `ScenarioOutcome` (with `video_accepted` defaulting to `false`) and that `ScenarioOutcome` re-serialises into the toolgate-compatible shape. Phase 2/3/5 rely on this exact shape (sync `/handlers/{id}/run` response + runner complete callback). Per **R9**, the assertion is the asymmetric round-trip (4-key in, `video_accepted==false`), NOT exact-key equality — the Rust type intentionally carries a 5th `video_accepted` key that the Python side omits.

- [ ] **Step 1: Write the failing test** (add these test fns inside the existing `mod tests` block in `outcome.rs`)

```rust
    #[test]
    fn toolgate_ok_json_deserialises_into_outcome() {
        // The EXACT 4-key JSON a toolgate ResultBuilder.text(...) emits (Phase 2).
        // `video_accepted` is absent on the wire; serde default => false (R9).
        let wire = r#"{"status":"ok","summary_text":"transcript here","artifact_urls":["/api/uploads/1?sig=x&exp=9"],"reason":null}"#;
        let o: ScenarioOutcome = serde_json::from_str(wire).unwrap();
        assert_eq!(o.status, ScenarioStatus::Ok);
        assert_eq!(o.summary_text, "transcript here");
        assert_eq!(o.artifact_urls, vec!["/api/uploads/1?sig=x&exp=9".to_string()]);
        assert!(o.reason.is_none());
        assert!(!o.video_accepted, "absent video_accepted must default to false");
    }

    #[test]
    fn toolgate_failed_json_deserialises_into_outcome() {
        let wire = r#"{"status":"failed","summary_text":"","artifact_urls":[],"reason":"HTTP 502"}"#;
        let o: ScenarioOutcome = serde_json::from_str(wire).unwrap();
        assert_eq!(o.status, ScenarioStatus::Failed);
        assert_eq!(o.reason.as_deref(), Some("HTTP 502"));
        assert!(o.artifact_urls.is_empty());
        assert!(!o.video_accepted);
    }

    #[test]
    fn toolgate_unsupported_too_large_timeout_statuses_deserialise() {
        for (wire_status, expected) in [
            ("too_large", ScenarioStatus::TooLarge),
            ("unsupported", ScenarioStatus::Unsupported),
            ("timeout", ScenarioStatus::Timeout),
        ] {
            let wire = format!(
                r#"{{"status":"{}","summary_text":"","artifact_urls":[],"reason":"x"}}"#,
                wire_status
            );
            let o: ScenarioOutcome = serde_json::from_str(&wire).unwrap();
            assert_eq!(o.status, expected, "status {} must map", wire_status);
        }
    }

    #[test]
    fn outcome_reserialises_to_toolgate_compatible_shape() {
        // Re-serialising the Rust type keeps the 4 toolgate keys with the right
        // names/values, plus the benign 5th `video_accepted` key (R9). The
        // assertion checks the toolgate-consumed keys, not exact-key equality.
        let o = ScenarioOutcome::ok(
            "hi".into(),
            vec!["/api/uploads/2?sig=y&exp=9".into()],
        );
        let json = serde_json::to_value(&o).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["summary_text"], "hi");
        assert_eq!(json["artifact_urls"][0], "/api/uploads/2?sig=y&exp=9");
        assert!(json["reason"].is_null());
        // The Rust type intentionally emits a 5th key the Python side omits.
        assert_eq!(json["video_accepted"], false, "video_accepted always serialises (R9)");
    }
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core file_scenario::outcome -- --nocapture`
  These tests compile against the already-present contract types, so to force a genuine red first, temporarily flip one assertion in `toolgate_ok_json_deserialises_into_outcome` to `assert!(o.video_accepted)`, run, and observe `assertion failed: o.video_accepted`. This proves the test exercises the real default-to-`false` invariant rather than passing vacuously.

- [ ] **Step 3: Write minimal implementation** — no production code change is required; the contract type already supports the 4-key wire shape and the benign 5th key (verified against `outcome.rs`: `video_accepted` has `#[serde(default)]` and no `skip_serializing_if`). Restore the corrected assertion from Step 2 (`assert!(!o.video_accepted, "absent video_accepted must default to false")`). The deliverable here is the locked-in test that freezes the cross-language contract.

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core file_scenario::outcome`
  Expected: PASS — all existing `outcome` tests plus the 4 new wire-contract tests (`test result: ok. <N> passed; 0 failed`).

- [ ] **Step 5: Commit**
```
git add crates/opex-core/src/agent/file_scenario/outcome.rs
git commit -m "test(core): freeze toolgate ScenarioOutcome wire contract round-trip"
```

---

## Phase 2 — toolgate hub (sync)

This phase builds the toolgate-side handler hub: the execution `ctx` (handlers receive RAW BYTES — they never fetch a loopback URL), the file loader/registry, the four sync built-in handlers, the FastAPI router (multipart `POST /handlers/{id}/run` sync path runs the handler under a per-execution timeout; async returns `501` until Phase 5), app wiring with hot-reload, the `config.workspace_dir` field, and the core change that adds `workspace_dir` to `/api/media-config`. It depends on Phase 1's `toolgate/handlers/descriptor.py` (`HandlerDescriptor`, `DescriptorError`, `parse_descriptor(source, tier)`).

**Resolutions applied:**
- **R12 (BLOCKING SSRF×loopback fix):** `POST /handlers/{id}/run` accepts **multipart form-data** with a `file` field carrying the upload bytes (plus form fields `mime`, `filename`, `params`, `language`, `job_id?`, `source_url?`). The router reads `file.read()` and builds `HandlerFile(bytes=..., source_url=...)`. Builtins use `file.bytes` DIRECTLY — `transcribe`/`describe` pass bytes to the provider via the shared raw client (`ctx.http_client_raw`); `extract_document` parses `file.bytes` via `asyncio.to_thread(_extract_sync, ...)` using pymupdf/python-docx — **no `/extract-text-url` loopback POST**. `download_limited` is **never** called on a loopback url. `ctx.http = SsrfHttpClient(shared)` is exposed ONLY for handler-initiated EXTERNAL fetches (it still validates and blocks private/link-local hosts).
- **R5:** SSRF-safe `ctx.http`; per-execution sync timeout `HANDLER_SYNC_TIMEOUT_SECS=120`; `extract_document` CPU offload via `asyncio.to_thread`; `GET /handlers` fills each manifest's `provider` from the active provider when `capability` is set.
- **R9:** Python `HandlerResult.to_dict` emits EXACTLY 4 keys (`status`, `summary_text`, `artifact_urls`, `reason`); core's `ScenarioOutcome` has a benign 5th key (`video_accepted`, serde default false) it deserializes fine.
- **R6:** real Rust accessors only in the core task (no nonexistent helpers).

Step 0 of Task 1 locates the real toolgate SSRF validator so `SsrfHttpClient` reuses it rather than inventing a name.

---

### Task 1: HandlerContext, ResultBuilder, HandlerFile, SsrfHttpClient (the `ctx` API)

**Files:**
- Create: `toolgate/handlers/__init__.py`
- Create: `toolgate/handlers/context.py`
- Test: `toolgate/tests/test_handlers_context.py`

**Interfaces:**
- Consumes: `registry.aget_active(capability)` → `Provider | None`; provider Protocols whose FIRST arg is the http client (`STTProvider.transcribe(http, audio_bytes, filename, language, model=None)`, `VisionProvider.describe(http, image_bytes, content_type, prompt, max_tokens=2000)`, `TTSProvider.synthesize(http, text, voice, model=None, response_format="mp3", registry=None)`, `ImageGenProvider.generate(http, prompt, size="1024x1024", model=None, quality="standard")`, `EmbeddingProvider.embed(http, texts, model=None)`, `WebSearchProvider.search(http, query, max_results=5)`); the existing toolgate SSRF URL validator reused by `download_limited` (located via grep in Step 0).
- Produces: `HandlerResult`, `ResultBuilder`, `SsrfHttpClient`, `HandlerContext` (exposes `ctx.http_client_raw` — the shared client for provider/byte calls — AND `ctx.http` — the SSRF wrapper for external fetches), `HandlerFile` (now carries `source_url: str | None`), `build_context(registry, http_client, job_id=None, core_url=None, auth_token=None) -> HandlerContext`. Consumed by `loader.py`, `runner.py`, every builtin, and `router.py` (later tasks).

- [ ] **Step 0: Locate the existing SSRF validator that `download_limited` uses** (real helper name — do NOT invent one)

```bash
cd toolgate && grep -rn "def download_limited" helpers.py
cd toolgate && grep -rnE "ssrf|private|validate.*url|is_internal|resolve|is_loopback|link.local" helpers.py
```
Expected: `download_limited(http, url, ...)` definition + the URL-validation call it makes (the grounding notes a `validate_url_ssrf` / equivalent that blocks `localhost`/`127.0.0.1`/`0.0.0.0`/`ip.is_loopback`). Record the EXACT name; the `SsrfHttpClient` below imports and calls THAT function. If the guard is inline (not a named function), extract it into a module-level `validate_url_ssrf(url)` in `helpers.py` first, leave `download_limited` calling the extracted function, and import that.

- [ ] **Step 1: Write the failing test**

```python
# toolgate/tests/test_handlers_context.py
import pytest

from handlers.context import (
    build_context,
    HandlerContext,
    HandlerFile,
    HandlerResult,
    ResultBuilder,
    SsrfHttpClient,
)


class _FakeSTT:
    name = "fake-stt"

    async def transcribe(self, http, audio_bytes, filename, language, model=None):
        # the wrapper must inject the SHARED RAW client + forward kwargs
        assert http is _FakeRegistry.sentinel_http
        assert audio_bytes == b"AUDIO"
        assert language == "en"
        return "hello world"


class _FakeRegistry:
    sentinel_http = object()

    def __init__(self, active):
        self._active = active

    async def aget_active(self, capability):
        return self._active.get(capability)


def test_result_builder_text_shape():
    r = ResultBuilder().text("hi")
    assert isinstance(r, HandlerResult)
    assert r.to_dict() == {
        "status": "ok",
        "summary_text": "hi",
        "artifact_urls": [],
        "reason": None,
    }


def test_result_to_dict_emits_exactly_four_keys():
    # R9: Python wire shape is 4 keys; core deserializes this (video_accepted
    # defaults false). Never emit a 5th key.
    assert set(ResultBuilder().text("x").to_dict().keys()) == {
        "status", "summary_text", "artifact_urls", "reason",
    }


def test_result_builder_failed_unsupported_too_large():
    assert ResultBuilder().failed("boom").to_dict()["status"] == "failed"
    assert ResultBuilder().failed("boom").to_dict()["reason"] == "boom"
    assert ResultBuilder().unsupported("nope").to_dict()["status"] == "unsupported"
    assert ResultBuilder().too_large("big").to_dict()["status"] == "too_large"


@pytest.mark.asyncio
async def test_ctx_exposes_raw_client():
    # R12: ctx.http_client_raw is the SHARED client used for provider/byte calls.
    reg = _FakeRegistry({})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    assert ctx.http_client_raw is _FakeRegistry.sentinel_http


@pytest.mark.asyncio
async def test_ctx_stt_wrapper_injects_raw_client_and_forwards():
    reg = _FakeRegistry({"stt": _FakeSTT()})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    out = await ctx.stt.transcribe(b"AUDIO", language="en")
    assert out == "hello world"


@pytest.mark.asyncio
async def test_ctx_stt_missing_provider_raises():
    reg = _FakeRegistry({})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    with pytest.raises(RuntimeError, match="no active stt provider"):
        await ctx.stt.transcribe(b"AUDIO", language="en")


@pytest.mark.asyncio
async def test_ctx_progress_is_noop_without_job_id():
    reg = _FakeRegistry({})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    # no job_id → must not raise, must not POST
    await ctx.progress("downloading", 10)


@pytest.mark.asyncio
async def test_ctx_http_is_ssrf_safe_and_blocks_private(monkeypatch):
    import httpx
    import handlers.context as ctxmod

    blocked = {"called_with": None}

    def _fake_validate(url):
        blocked["called_with"] = url
        if "169.254" in url or "127.0.0.1" in url:
            raise ValueError("blocked private/link-local URL")

    # patch the SSRF validator the SsrfHttpClient was wired to
    monkeypatch.setattr(ctxmod, "validate_url_ssrf", _fake_validate)

    def _ok(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"ok")

    async with httpx.AsyncClient(transport=httpx.MockTransport(_ok)) as client:
        ctx = build_context(_FakeRegistry({}), client)
        # ctx.http must be the SSRF wrapper, not the raw client
        assert isinstance(ctx.http, SsrfHttpClient)
        # public host passes the validator + reaches the transport
        r = await ctx.http.get("http://example.com/x")
        assert r.status_code == 200
        assert blocked["called_with"] == "http://example.com/x"
        # private host is rejected before any request
        with pytest.raises(ValueError, match="blocked"):
            await ctx.http.get("http://169.254.169.254/latest")


def test_handler_file_fields_with_source_url():
    f = HandlerFile(bytes=b"X", mime="audio/ogg", filename="a.ogg", size=1,
                    source_url="https://youtu.be/abc")
    assert f.bytes == b"X" and f.mime == "audio/ogg" and f.size == 1
    assert f.filename == "a.ogg" and f.source_url == "https://youtu.be/abc"


def test_handler_file_source_url_defaults_none():
    f = HandlerFile(bytes=b"X", mime="text/plain", filename="a.txt", size=1)
    assert f.source_url is None
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd toolgate && pytest tests/test_handlers_context.py -q
```
Expected: collection/import error `ModuleNotFoundError: No module named 'handlers.context'` (and `handlers` package missing).

- [ ] **Step 3: Write minimal implementation**

```python
# toolgate/handlers/__init__.py
"""File-handler hub: self-describing Python handlers loaded by toolgate."""
```

```python
# toolgate/handlers/context.py
"""Execution context for file handlers.

`ctx` is the ONLY sanctioned API a handler sees. Handlers receive the file's
RAW BYTES (R12) — they never fetch a loopback URL (toolgate's SSRF guard hard-
blocks loopback, and core already downloads the upload in Rust and POSTs the
bytes as multipart). Provider wrappers inject the shared httpx.AsyncClient
internally so handlers never touch the client or credentials.

Two http surfaces are exposed:
  - ctx.http_client_raw : the shared httpx.AsyncClient. Used for provider calls
    (the provider Protocols take it as first arg) and direct byte work. NOT
    SSRF-validated because it is only ever pointed at trusted provider backends.
  - ctx.http : an SsrfHttpClient wrapper for handler-initiated EXTERNAL fetches
    (e.g. a workspace handler hitting a public API). Every .get/.post validates
    the URL via the same guard download_limited uses, blocking private /
    link-local hosts.

`ctx.result` builds the ScenarioOutcome wire shape the core consumes
(status/summary_text/artifact_urls/reason) — exactly 4 keys (R9)."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

import httpx

# Reuse toolgate's existing SSRF URL validator (the same one download_limited
# calls). Step 0 confirmed/extracted this name in helpers.py.
from helpers import validate_url_ssrf

log = logging.getLogger("toolgate.handlers")


@dataclass
class HandlerFile:
    bytes: bytes
    mime: str
    filename: str
    size: int
    # For url-based handlers (e.g. video) where core sends a source_url form
    # field and no upload bytes. None for normal upload-backed handlers.
    source_url: str | None = None


@dataclass
class HandlerResult:
    """Mirrors the core ScenarioOutcome wire type (snake_case)."""

    status: str = "ok"
    summary_text: str = ""
    artifact_urls: list[str] = field(default_factory=list)
    reason: str | None = None

    def to_dict(self) -> dict[str, Any]:
        # R9: emit EXACTLY these 4 keys. Core's ScenarioOutcome has a 5th field
        # (video_accepted) with serde default false; it deserializes this fine.
        return {
            "status": self.status,
            "summary_text": self.summary_text,
            "artifact_urls": list(self.artifact_urls),
            "reason": self.reason,
        }


class ResultBuilder:
    """Builds a HandlerResult. `.file`/`.card` carry the b64/card payload in
    artifact_urls/summary_text; v1 sync handlers mostly use `.text`."""

    def text(self, s: str) -> HandlerResult:
        return HandlerResult(status="ok", summary_text=s)

    def file(self, data: bytes, mime: str) -> HandlerResult:
        import base64

        b64 = base64.b64encode(data).decode("ascii")
        return HandlerResult(
            status="ok",
            summary_text=f"[file {mime} {len(data)} bytes]",
            artifact_urls=[f"data:{mime};base64,{b64}"],
        )

    def card(self, card_type: str, data: dict) -> HandlerResult:
        import json

        return HandlerResult(
            status="ok",
            summary_text=json.dumps({"card_type": card_type, "data": data}),
        )

    def failed(self, reason: str) -> HandlerResult:
        return HandlerResult(status="failed", reason=reason)

    def unsupported(self, reason: str) -> HandlerResult:
        return HandlerResult(status="unsupported", reason=reason)

    def too_large(self, reason: str) -> HandlerResult:
        return HandlerResult(status="too_large", reason=reason)


class SsrfHttpClient:
    """SSRF-safe facade over the shared httpx.AsyncClient (R5/R12). Every
    .get/.post validates the URL via the same guard download_limited uses, so a
    handler fetching an attacker-influenced URL cannot reach private/link-local
    hosts. Used ONLY for handler-initiated EXTERNAL fetches — provider/byte
    calls use ctx.http_client_raw."""

    def __init__(self, http: httpx.AsyncClient):
        self._http = http

    async def get(self, url: str, **kwargs):
        validate_url_ssrf(url)
        return await self._http.get(url, **kwargs)

    async def post(self, url: str, **kwargs):
        validate_url_ssrf(url)
        return await self._http.post(url, **kwargs)


class _CapabilityWrapper:
    """Resolves the active provider for `capability` per call and injects the
    shared RAW http client as the provider Protocol's first positional
    argument (R12: providers call their own trusted backends)."""

    def __init__(self, registry, http: httpx.AsyncClient, capability: str):
        self._registry = registry
        self._http = http
        self._capability = capability

    async def _resolve(self):
        provider = await self._registry.aget_active(self._capability)
        if provider is None:
            raise RuntimeError(f"no active {self._capability} provider")
        return provider

    # STT
    async def transcribe(self, audio_bytes: bytes, *, filename: str = "audio.ogg",
                         language: str = "ru", model: str | None = None) -> str:
        p = await self._resolve()
        return await p.transcribe(self._http, audio_bytes, filename, language, model)

    # Vision
    async def describe(self, image_bytes: bytes, *, content_type: str,
                       prompt: str, max_tokens: int = 2000) -> str:
        p = await self._resolve()
        return await p.describe(self._http, image_bytes, content_type, prompt, max_tokens)

    # TTS
    async def synthesize(self, text: str, *, voice: str, model: str | None = None,
                         response_format: str = "mp3") -> bytes:
        p = await self._resolve()
        return await p.synthesize(self._http, text, voice, model, response_format,
                                  registry=self._registry)

    # ImageGen
    async def generate(self, prompt: str, *, size: str = "1024x1024",
                       model: str | None = None, quality: str = "standard") -> bytes:
        p = await self._resolve()
        return await p.generate(self._http, prompt, size, model, quality)

    # Embedding
    async def embed(self, texts: list[str], *, model: str | None = None) -> list[list[float]]:
        p = await self._resolve()
        return await p.embed(self._http, texts, model)

    # WebSearch
    async def search(self, query: str, *, max_results: int = 5) -> list[dict]:
        p = await self._resolve()
        return await p.search(self._http, query, max_results)


@dataclass
class HandlerContext:
    stt: _CapabilityWrapper
    vision: _CapabilityWrapper
    tts: _CapabilityWrapper
    imagegen: _CapabilityWrapper
    search: _CapabilityWrapper
    embed: _CapabilityWrapper
    http: SsrfHttpClient
    http_client_raw: httpx.AsyncClient
    result: ResultBuilder
    log: logging.Logger
    _job_id: str | None = None
    _core_url: str | None = None
    _auth_token: str | None = None

    async def progress(self, phase: str, pct: int) -> None:
        """Post progress to the core progress callback when a job_id is set;
        a no-op for sync handlers (no job_id). Uses the RAW client because the
        core callback URL is a trusted loopback endpoint, not handler input."""
        if not self._job_id or not self._core_url:
            return
        url = f"{self._core_url.rstrip('/')}/api/files/jobs/{self._job_id}/progress"
        headers = {}
        if self._auth_token:
            headers["Authorization"] = f"Bearer {self._auth_token}"
        try:
            await self.http_client_raw.post(url, json={"phase": phase, "pct": pct},
                                            headers=headers, timeout=10.0)
        except Exception as e:  # progress is best-effort
            self.log.warning("progress callback failed: %s", e)


def build_context(registry, http_client: httpx.AsyncClient, job_id: str | None = None,
                  core_url: str | None = None, auth_token: str | None = None) -> HandlerContext:
    return HandlerContext(
        stt=_CapabilityWrapper(registry, http_client, "stt"),
        vision=_CapabilityWrapper(registry, http_client, "vision"),
        tts=_CapabilityWrapper(registry, http_client, "tts"),
        imagegen=_CapabilityWrapper(registry, http_client, "imagegen"),
        search=_CapabilityWrapper(registry, http_client, "websearch"),
        embed=_CapabilityWrapper(registry, http_client, "embedding"),
        http=SsrfHttpClient(http_client),
        http_client_raw=http_client,
        result=ResultBuilder(),
        log=log,
        _job_id=job_id,
        _core_url=core_url,
        _auth_token=auth_token,
    )
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd toolgate && pytest tests/test_handlers_context.py -q
```
Expected: `11 passed`.

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/__init__.py toolgate/handlers/context.py toolgate/tests/test_handlers_context.py toolgate/helpers.py
git commit -m "feat(toolgate): handler ctx (HandlerContext/ResultBuilder/HandlerFile + raw client + SSRF-safe ctx.http)"
```

---

### Task 2: HandlerRegistry loader (import boundary + reserved-id collision)

**Files:**
- Create: `toolgate/handlers/loader.py`
- Test: `toolgate/tests/test_handlers_loader.py`

**Interfaces:**
- Consumes: `from handlers.descriptor import HandlerDescriptor, DescriptorError, parse_descriptor` (Phase 1); a handler file = leading `# <handler>…# </handler>` XML comment + `async def run(ctx, file, params)`.
- Produces: `LoadedHandler{descriptor, run, tier}`, `HandlerRegistry` with `load_all(builtin_dir, workspace_dir)`, `get(id) -> LoadedHandler | None`, `manifests() -> list[dict]` (each item has `provider: None`, filled later by the router per R5), `reload_file(path)`, `etag() -> str`. Consumed by `router.py`, `app.py`, `runner.py`.

- [ ] **Step 1: Write the failing test**

```python
# toolgate/tests/test_handlers_loader.py
import textwrap
from pathlib import Path

from handlers.loader import HandlerRegistry, LoadedHandler

GOOD = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="ru">Эхо</label>
    #   <label lang="en">Echo</label>
    #   <icon>file</icon>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    #   <output>text</output>
    #   <order>5</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text(file.filename)
''')

SYNTAX_ERR = textwrap.dedent('''\
    # <handler>
    #   <id>broken</id>
    #   <label lang="en">Broken</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params)   # missing colon -> SyntaxError
        return ctx.result.text("x")
''')

DUP_BUILTIN = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="en">Shadow</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("shadow")
''')


def _write(d: Path, name: str, body: str) -> Path:
    p = d / name
    p.write_text(body, encoding="utf-8")
    return p


def test_load_all_registers_builtin(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "echo.py", GOOD)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)
    lh = reg.get("echo")
    assert isinstance(lh, LoadedHandler)
    assert lh.tier == "builtin"
    assert lh.descriptor.id == "echo"
    assert callable(lh.run)


def test_syntax_error_file_skipped_not_crash(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "ok.py", GOOD)
    _write(builtin, "bad.py", SYNTAX_ERR)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)  # must not raise
    assert reg.get("echo") is not None
    assert reg.get("broken") is None


def test_workspace_cannot_shadow_builtin_id(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "echo.py", GOOD)
    ws = tmp_path / "workspace"
    ws.mkdir()
    fh = ws / "file_handlers"
    fh.mkdir()
    _write(fh, "shadow.py", DUP_BUILTIN)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), str(ws))
    # builtin wins; the workspace clash is rejected (still builtin tier)
    assert reg.get("echo").tier == "builtin"


def test_manifests_and_etag(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "echo.py", GOOD)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)
    ms = reg.manifests()
    assert len(ms) == 1
    item = ms[0]
    assert item["id"] == "echo"
    assert item["labels"] == {"ru": "Эхо", "en": "Echo"}
    assert item["match"]["mime"] == ["text/*"]
    assert item["execution"] == "sync"
    assert item["tier"] == "builtin"
    assert item["provider"] is None  # router fills this from the active provider
    e1 = reg.etag()
    assert isinstance(e1, str) and e1
    # stable for identical content
    assert reg.etag() == e1
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd toolgate && pytest tests/test_handlers_loader.py -q
```
Expected: `ModuleNotFoundError: No module named 'handlers.loader'`.

- [ ] **Step 3: Write minimal implementation**

```python
# toolgate/handlers/loader.py
"""Scans builtin + workspace handler files, parses their XML descriptor,
imports the module, and captures `run`. Every per-file load is wrapped in
try/except so a bad workspace file is skipped+logged, never aborting the scan.
Builtin ids are reserved: a workspace file reusing one is rejected (builtin
wins)."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import logging
import os
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from handlers.descriptor import HandlerDescriptor, DescriptorError, parse_descriptor

log = logging.getLogger("toolgate.handlers")


@dataclass
class LoadedHandler:
    descriptor: HandlerDescriptor
    run: Callable
    tier: str


def _read_source(path: str) -> str:
    return Path(path).read_text(encoding="utf-8")


def _import_run(path: str):
    """Import a handler module from `path` and return its `run` coroutine fn."""
    mod_name = f"_handler_{uuid.uuid4().hex}"
    spec = importlib.util.spec_from_file_location(mod_name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot create import spec for {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    run = getattr(module, "run", None)
    if run is None or not callable(run):
        raise ImportError(f"{path} has no callable `run`")
    return run


class HandlerRegistry:
    def __init__(self) -> None:
        self._handlers: dict[str, LoadedHandler] = {}

    def load_all(self, builtin_dir: str, workspace_dir: str | None) -> None:
        self._handlers = {}
        # Builtin tier FIRST — its ids become reserved.
        self._scan_dir(builtin_dir, "builtin")
        if workspace_dir:
            ws = os.path.join(workspace_dir, "file_handlers")
            self._scan_dir(ws, "workspace")

    def _scan_dir(self, directory: str, tier: str) -> None:
        if not directory or not os.path.isdir(directory):
            return
        for name in sorted(os.listdir(directory)):
            if not name.endswith(".py") or name.startswith("_"):
                continue
            self._load_one(os.path.join(directory, name), tier)

    def _load_one(self, path: str, tier: str) -> None:
        try:
            source = _read_source(path)
            descriptor = parse_descriptor(source, tier)
            if descriptor.id in self._handlers:
                existing = self._handlers[descriptor.id]
                log.warning(
                    "handler id %r in %s clashes with existing %s handler - rejected",
                    descriptor.id, path, existing.tier,
                )
                return
            run = _import_run(path)
            self._handlers[descriptor.id] = LoadedHandler(descriptor, run, tier)
            log.info("loaded handler %s (tier=%s)", descriptor.id, tier)
        except DescriptorError as e:
            log.warning("skipping handler file %s: descriptor error: %s", path, e)
        except (SyntaxError, ImportError) as e:
            log.warning("skipping handler file %s: import error: %s", path, e)
        except Exception as e:
            log.warning("skipping handler file %s: unexpected error: %s", path, e)

    def get(self, handler_id: str) -> LoadedHandler | None:
        return self._handlers.get(handler_id)

    def reload_file(self, path: str) -> None:
        """Reload a single workspace file in place (hot-reload). Builtin-id
        clashes are still rejected by _load_one. A deleted file is a no-op
        (the previously loaded handler stays until the next full load_all)."""
        if not os.path.isfile(path):
            return
        self._load_one(path, "workspace")

    def manifests(self) -> list[dict]:
        items = [self._manifest(h) for h in self._handlers.values()]
        items.sort(key=lambda m: (m["order"], m["id"]))
        return items

    def _manifest(self, h: LoadedHandler) -> dict:
        d = h.descriptor
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
            "tier": h.tier,
        }

    def etag(self) -> str:
        canonical = json.dumps(self.manifests(), sort_keys=True,
                               ensure_ascii=False).encode("utf-8")
        return '"' + hashlib.sha256(canonical).hexdigest() + '"'
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd toolgate && pytest tests/test_handlers_loader.py -q
```
Expected: `4 passed`.

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/loader.py toolgate/tests/test_handlers_loader.py
git commit -m "feat(toolgate): HandlerRegistry loader with import boundary + reserved-id collision"
```

---

### Task 3: Four sync built-in handlers (save, transcribe, describe, extract_document)

**Files:**
- Create: `toolgate/handlers/builtin/__init__.py`
- Create: `toolgate/handlers/builtin/save.py`
- Create: `toolgate/handlers/builtin/transcribe.py`
- Create: `toolgate/handlers/builtin/describe.py`
- Create: `toolgate/handlers/builtin/extract_document.py`
- Test: `toolgate/tests/test_handlers_builtin.py`

**Interfaces:**
- Consumes (R12 — bytes, never loopback fetch): `HandlerContext` (`ctx.stt.transcribe`, `ctx.vision.describe`, `ctx.result.*`), `HandlerFile` (`file.bytes`, `file.mime`, `file.filename`); `helpers.default_vision_prompt`; document extraction parses `file.bytes` IN-PROCESS via `asyncio.to_thread(_extract_sync, ...)` (pymupdf for PDF, python-docx for DOCX, decode for text) — NOT a `/extract-text-url` loopback POST.
- Produces: 4 builtin handler files with ids `save`, `transcribe`, `describe`, `extract_document` matching `FSE_DEFAULT_ALLOWLIST`. Consumed by the loader at startup and by `router.py`.

- [ ] **Step 0: Confirm the existing document-parse helpers in `routers/documents.py`** (reuse pymupdf/python-docx names; don't invent)

```bash
cd toolgate && grep -rnE "import fitz|from docx|fitz\.open|Document\(|def .*extract" routers/documents.py
```
Expected: the `import fitz` (pymupdf) + python-docx usage in `/extract-text-url`. The `_extract_sync` in `extract_document.py` below mirrors that exact parsing (open from `bytes`, not from a url). Use the same module imports found here.

- [ ] **Step 1: Write the failing test**

```python
# toolgate/tests/test_handlers_builtin.py
import os

import pytest

from handlers.context import build_context, HandlerFile
from handlers.loader import HandlerRegistry

BUILTIN_DIR = os.path.join(os.path.dirname(__file__), "..", "handlers", "builtin")


class _FakeSTT:
    name = "fake-stt"
    async def transcribe(self, http, audio_bytes, filename, language, model=None):
        # R12: handler must pass the RAW bytes straight through.
        assert audio_bytes == b"AUDIO"
        return f"transcript:{language}"


class _FakeVision:
    name = "fake-vision"
    async def describe(self, http, image_bytes, content_type, prompt, max_tokens=2000):
        assert image_bytes == b"IMG"
        return f"vision:{content_type}"


class _FakeRegistry:
    def __init__(self, active):
        self._active = active
    async def aget_active(self, capability):
        return self._active.get(capability)


def _load(handler_id):
    reg = HandlerRegistry()
    reg.load_all(os.path.abspath(BUILTIN_DIR), None)
    lh = reg.get(handler_id)
    assert lh is not None, f"{handler_id} not registered"
    return lh


def test_all_four_builtins_parse_and_register():
    reg = HandlerRegistry()
    reg.load_all(os.path.abspath(BUILTIN_DIR), None)
    for hid in ("save", "transcribe", "describe", "extract_document"):
        lh = reg.get(hid)
        assert lh is not None
        assert lh.tier == "builtin"
        assert lh.descriptor.execution == "sync"


@pytest.mark.asyncio
async def test_save_returns_ok_with_filename():
    lh = _load("save")
    ctx = build_context(_FakeRegistry({}), object())
    f = HandlerFile(bytes=b"X", mime="application/pdf", filename="d.pdf", size=1)
    out = await lh.run(ctx, f, {})
    d = out.to_dict()
    assert d["status"] == "ok"
    # bytes already persisted by core; save just confirms it.
    assert "d.pdf" in d["summary_text"]


@pytest.mark.asyncio
async def test_transcribe_uses_stt_provider_with_raw_bytes():
    lh = _load("transcribe")
    ctx = build_context(_FakeRegistry({"stt": _FakeSTT()}), object())
    f = HandlerFile(bytes=b"AUDIO", mime="audio/ogg", filename="a.ogg", size=5)
    out = await lh.run(ctx, f, {"language": "en"})
    assert out.to_dict()["summary_text"] == "transcript:en"


@pytest.mark.asyncio
async def test_describe_uses_vision_provider_with_raw_bytes():
    lh = _load("describe")
    ctx = build_context(_FakeRegistry({"vision": _FakeVision()}), object())
    f = HandlerFile(bytes=b"IMG", mime="image/png", filename="i.png", size=3)
    out = await lh.run(ctx, f, {})
    assert out.to_dict()["summary_text"] == "vision:image/png"


@pytest.mark.asyncio
async def test_extract_document_parses_plain_text_bytes():
    # R12: extract parses file.bytes in-process (no loopback POST).
    lh = _load("extract_document")
    ctx = build_context(_FakeRegistry({}), object())
    f = HandlerFile(bytes="Привет мир".encode("utf-8"), mime="text/plain",
                    filename="d.txt", size=10)
    out = await lh.run(ctx, f, {})
    d = out.to_dict()
    assert d["status"] == "ok"
    assert "Привет мир" in d["summary_text"]


@pytest.mark.asyncio
async def test_extract_document_respects_max_chars():
    lh = _load("extract_document")
    ctx = build_context(_FakeRegistry({}), object())
    f = HandlerFile(bytes=("A" * 100).encode("utf-8"), mime="text/plain",
                    filename="d.txt", size=100)
    out = await lh.run(ctx, f, {"max_chars": 10})
    assert len(out.to_dict()["summary_text"]) == 10
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd toolgate && pytest tests/test_handlers_builtin.py -q
```
Expected: registry returns `None` for the builtin ids — `AssertionError: save not registered` (the builtin files do not exist yet).

- [ ] **Step 3: Write minimal implementation**

```python
# toolgate/handlers/builtin/__init__.py
"""Built-in trusted handlers ported from the in-core FSE dispatch table."""
```

```python
# toolgate/handlers/builtin/save.py
# <handler>
#   <id>save</id>
#   <label lang="ru">Сохранить</label>
#   <label lang="en">Save</label>
#   <description lang="ru">Сохранить файл как есть</description>
#   <description lang="en">Keep the file as-is</description>
#   <icon>save</icon>
#   <match>
#     <mime>*/*</mime>
#   </match>
#   <execution>sync</execution>
#   <output>file</output>
#   <order>1</order>
#   <enabled>true</enabled>
# </handler>
"""save — keep the uploaded file as a persisted artifact (no processing).

The bytes are already persisted in core uploads (core downloaded them in Rust
and POSTed the multipart). This handler just confirms persistence; the core
records a file-derived message referencing the original upload."""

from handlers.context import HandlerResult


async def run(ctx, file, params):
    return HandlerResult(
        status="ok",
        summary_text=f"Saved {file.filename} ({file.size} bytes)",
        artifact_urls=[],
    )
```

```python
# toolgate/handlers/builtin/transcribe.py
# <handler>
#   <id>transcribe</id>
#   <label lang="ru">Транскрибировать</label>
#   <label lang="en">Transcribe</label>
#   <description lang="ru">Речь из аудио/видео в текст</description>
#   <description lang="en">Speech from audio/video to text</description>
#   <icon>mic</icon>
#   <match>
#     <mime>audio/*</mime>
#     <mime>video/*</mime>
#     <max_size_mb>200</max_size_mb>
#   </match>
#   <capability>stt</capability>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>
"""transcribe — speech-to-text via the active STT provider.

R12: the upload bytes arrive on file.bytes; the provider wrapper passes the
shared raw client to the STT backend (a trusted provider endpoint)."""


async def run(ctx, file, params):
    language = params.get("language", "ru")
    text = await ctx.stt.transcribe(
        file.bytes, filename=file.filename, language=language
    )
    return ctx.result.text(text)
```

```python
# toolgate/handlers/builtin/describe.py
# <handler>
#   <id>describe</id>
#   <label lang="ru">Описать</label>
#   <label lang="en">Describe</label>
#   <description lang="ru">Описание изображения</description>
#   <description lang="en">Image description</description>
#   <icon>image</icon>
#   <match>
#     <mime>image/*</mime>
#     <max_size_mb>20</max_size_mb>
#   </match>
#   <capability>vision</capability>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="prompt" type="string" default="" required="false"/>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>
"""describe — image description via the active vision provider.

R12: the upload bytes arrive on file.bytes; the provider wrapper passes the
shared raw client to the vision backend (a trusted provider endpoint)."""

from helpers import default_vision_prompt


async def run(ctx, file, params):
    prompt = (params.get("prompt") or "").strip()
    language = params.get("language", "ru")
    if not prompt:
        prompt = default_vision_prompt(language)
    text = await ctx.vision.describe(
        file.bytes, content_type=file.mime, prompt=prompt
    )
    return ctx.result.text(text)
```

```python
# toolgate/handlers/builtin/extract_document.py
# <handler>
#   <id>extract_document</id>
#   <label lang="ru">Извлечь текст</label>
#   <label lang="en">Extract text</label>
#   <description lang="ru">Текст из PDF/DOCX/текстовых файлов</description>
#   <description lang="en">Text from PDF/DOCX/text files</description>
#   <icon>file-text</icon>
#   <match>
#     <mime>application/pdf</mime>
#     <mime>application/vnd.openxmlformats-officedocument.wordprocessingml.document</mime>
#     <mime>application/msword</mime>
#     <mime>text/*</mime>
#     <max_size_mb>50</max_size_mb>
#   </match>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="max_chars" type="int" default="8000" required="false"/>
#   </params>
#   <order>20</order>
#   <enabled>true</enabled>
# </handler>
"""extract_document — text extraction parsed IN-PROCESS from file.bytes (R12).

PDF via pymupdf (fitz), DOCX via python-docx, everything text/* (and unknown)
via best-effort UTF-8 decode. The blocking CPU parse runs in a worker thread
via asyncio.to_thread (R5 CPU-offload). NO loopback /extract-text-url POST —
toolgate's SSRF guard blocks loopback and core already handed us the bytes."""

import asyncio
import io

import fitz  # pymupdf
from docx import Document

_DOCX_MIMES = {
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/msword",
}


def _extract_sync(data: bytes, mime: str) -> str:
    if mime == "application/pdf":
        parts = []
        with fitz.open(stream=data, filetype="pdf") as doc:
            for page in doc:
                parts.append(page.get_text())
        return "\n".join(parts)
    if mime in _DOCX_MIMES:
        doc = Document(io.BytesIO(data))
        return "\n".join(p.text for p in doc.paragraphs)
    # text/* and unknown -> best-effort decode
    return data.decode("utf-8", errors="replace")


async def run(ctx, file, params):
    max_chars = int(params.get("max_chars", 8000))
    try:
        text = await asyncio.to_thread(_extract_sync, file.bytes, file.mime)
    except Exception as e:  # corrupt/unsupported document
        return ctx.result.failed(f"extract failed: {e}")
    if max_chars > 0:
        text = text[:max_chars]
    return ctx.result.text(text)
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd toolgate && pytest tests/test_handlers_builtin.py -q
```
Expected: `6 passed`.

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/builtin/ toolgate/tests/test_handlers_builtin.py
git commit -m "feat(toolgate): port save/transcribe/describe/extract_document to sync builtins (raw-bytes)"
```

---

### Task 4: Router — GET /handlers (ETag+304, provider fill), GET /handlers/{id}, POST /handlers/{id}/run (multipart sync + per-execution timeout)

**Files:**
- Create: `toolgate/handlers/router.py`
- Test: `toolgate/tests/test_handlers_router.py`

**Interfaces:**
- Consumes: `HandlerRegistry.manifests()/get()/etag()`; `build_context(registry, http_client, core_url=...)`; `HandlerFile`; `request.app.state.registry` (the `ProviderRegistry` — used to fill `provider` per R5), `request.app.state.http_client`, and a new `request.app.state.handlers` (the python `HandlerRegistry`, wired in Task 6).
- Produces: `router` (APIRouter); module constant `HANDLER_SYNC_TIMEOUT_SECS = 120` (R5); `async def run_handler(...)` (the SAME function Phase 5 extends in place per R10). `GET /handlers` → `{handlers, etag}` + `ETag` header + `304` on `If-None-Match`, each manifest's `provider` filled from the active provider when `capability` is set; `GET /handlers/{id}`; `POST /handlers/{id}/run` accepts **multipart form-data** (R12): `file` (UploadFile, optional for url-only handlers) + form fields `mime`, `filename`, `params` (JSON string), `language`, `job_id?`, `source_url?` → sync path reads `file.read()`, builds `HandlerFile`, runs under `asyncio.wait_for(..., HANDLER_SYNC_TIMEOUT_SECS)`, returns `HandlerResult.to_dict()` (ScenarioOutcome wire json); async path returns `501` until the Phase 5 runner lands.

- [ ] **Step 1: Write the failing test**

```python
# toolgate/tests/test_handlers_router.py
import textwrap

from fastapi import FastAPI
from fastapi.testclient import TestClient

from handlers.loader import HandlerRegistry
from handlers.router import router as handlers_router

GOOD = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="ru">Эхо</label>
    #   <label lang="en">Echo</label>
    #   <icon>file</icon>
    #   <match><mime>text/*</mime><max_size_mb>1</max_size_mb></match>
    #   <execution>sync</execution>
    #   <output>text</output>
    #   <order>5</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text(file.bytes.decode("utf-8"))
''')

# capability-bearing handler exercises the R5 provider-fill on GET /handlers
WITH_CAP = textwrap.dedent('''\
    # <handler>
    #   <id>cap</id>
    #   <label lang="en">Cap</label>
    #   <match><mime>audio/*</mime></match>
    #   <capability>stt</capability>
    #   <execution>sync</execution>
    #   <output>text</output>
    #   <order>6</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("cap")
''')

# an async handler exercises the 501-until-Phase-5 branch
ASYNC_H = textwrap.dedent('''\
    # <handler>
    #   <id>slow</id>
    #   <label lang="en">Slow</label>
    #   <match><mime>video/*</mime></match>
    #   <execution>async</execution>
    #   <output>text</output>
    #   <order>7</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("slow")
''')


class _FakeProvider:
    name = "fake-stt-provider"


class _FakeRegistry:
    def __init__(self, active=None):
        self._active = active or {}
    async def aget_active(self, capability):
        return self._active.get(capability)


def _build_client(tmp_path, provider_active=None):
    import httpx

    builtin = tmp_path / "builtin"
    builtin.mkdir()
    (builtin / "echo.py").write_text(GOOD, encoding="utf-8")
    (builtin / "cap.py").write_text(WITH_CAP, encoding="utf-8")
    (builtin / "slow.py").write_text(ASYNC_H, encoding="utf-8")
    hreg = HandlerRegistry()
    hreg.load_all(str(builtin), None)

    app = FastAPI()
    app.include_router(handlers_router)
    # R12: handlers receive raw bytes; the router never fetches a URL. The
    # shared client is only used by provider calls (none in these tests).
    app.state.http_client = httpx.AsyncClient(
        transport=httpx.MockTransport(lambda r: httpx.Response(200))
    )
    app.state.registry = _FakeRegistry(provider_active)
    app.state.handlers = hreg
    return TestClient(app)


def _run(client, handler_id, *, content=b"hello-file", mime="text/plain",
         filename="a.txt", params="{}", language="ru", source_url=None):
    files = {"file": (filename, content, mime)}
    data = {"mime": mime, "filename": filename, "params": params,
            "language": language}
    if source_url is not None:
        data["source_url"] = source_url
    return client.post(f"/handlers/{handler_id}/run", files=files, data=data)


def test_get_handlers_shape_and_etag(tmp_path):
    client = _build_client(tmp_path)
    r = client.get("/handlers")
    assert r.status_code == 200
    body = r.json()
    ids = {h["id"] for h in body["handlers"]}
    assert {"echo", "cap", "slow"} <= ids
    assert "etag" in body and r.headers["etag"] == body["etag"]


def test_get_handlers_fills_provider_from_active(tmp_path):
    client = _build_client(tmp_path, provider_active={"stt": _FakeProvider()})
    body = client.get("/handlers").json()
    cap = next(h for h in body["handlers"] if h["id"] == "cap")
    assert cap["provider"] == "fake-stt-provider"
    echo = next(h for h in body["handlers"] if h["id"] == "echo")
    assert echo["provider"] is None  # no capability -> stays None


def test_get_handlers_provider_none_when_no_active(tmp_path):
    client = _build_client(tmp_path, provider_active={})
    body = client.get("/handlers").json()
    cap = next(h for h in body["handlers"] if h["id"] == "cap")
    assert cap["provider"] is None


def test_get_handlers_304_on_if_none_match(tmp_path):
    client = _build_client(tmp_path)
    etag = client.get("/handlers").headers["etag"]
    r = client.get("/handlers", headers={"If-None-Match": etag})
    assert r.status_code == 304


def test_get_single_handler(tmp_path):
    client = _build_client(tmp_path)
    r = client.get("/handlers/echo")
    assert r.status_code == 200
    assert r.json()["id"] == "echo"
    assert client.get("/handlers/missing").status_code == 404


def test_run_sync_multipart_returns_scenario_outcome(tmp_path):
    client = _build_client(tmp_path)
    r = _run(client, "echo", content=b"hello-file")
    assert r.status_code == 200
    out = r.json()
    assert out == {
        "status": "ok",
        "summary_text": "hello-file",
        "artifact_urls": [],
        "reason": None,
    }


def test_run_missing_handler_404(tmp_path):
    client = _build_client(tmp_path)
    r = _run(client, "nope")
    assert r.status_code == 404


def test_run_async_handler_returns_501_until_phase5(tmp_path):
    client = _build_client(tmp_path)
    r = _run(client, "slow", content=b"vid", mime="video/mp4", filename="v.mp4")
    assert r.status_code == 501
    assert r.json()["error"] == "async_runner_not_available"


def test_run_sync_timeout_returns_timeout_outcome(tmp_path, monkeypatch):
    # Force the configured sync timeout to ~0 so any await trips it.
    import handlers.router as rmod
    monkeypatch.setattr(rmod, "HANDLER_SYNC_TIMEOUT_SECS", 0.0)
    client = _build_client(tmp_path)
    r = _run(client, "echo", content=b"hello-file")
    assert r.status_code == 200
    out = r.json()
    assert out["status"] == "timeout"
    assert out["reason"] == "per-execution timeout"
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd toolgate && pytest tests/test_handlers_router.py -q
```
Expected: `ModuleNotFoundError: No module named 'handlers.router'`.

- [ ] **Step 3: Write minimal implementation**

```python
# toolgate/handlers/router.py
"""FastAPI routes for the file-handler hub.

GET /handlers          — manifests + ETag (304 on If-None-Match), mirrors the
                         ProviderRegistry discovery contract. Each manifest's
                         `provider` is filled from the active provider when the
                         handler declares a `capability` (R5).
GET /handlers/{id}     — single manifest (debug/UI).
POST /handlers/{id}/run — execute. R12: this is MULTIPART form-data; the upload
                         bytes arrive in the `file` field (core downloaded them
                         in Rust and POSTed them — toolgate NEVER fetches a
                         loopback url). SYNC handlers run inline under a
                         per-execution timeout and return a ScenarioOutcome
                         json. ASYNC handlers spawn an out-of-process runner —
                         that branch is added in Phase 5 (R10 extends THIS fn);
                         until then it returns 501 so the sync path is fully
                         testable.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os

from fastapi import APIRouter, File, Form, Request, Response, UploadFile
from fastapi.responses import JSONResponse

from handlers.context import HandlerFile, build_context

log = logging.getLogger("toolgate.handlers")

# R5: hard ceiling on a single sync handler execution.
HANDLER_SYNC_TIMEOUT_SECS = 120.0

router = APIRouter(tags=["handlers"])


def _core_url() -> str:
    return os.environ.get("CORE_API_URL", "http://127.0.0.1:18789")


async def _manifests_with_provider(request: Request) -> list[dict]:
    """Manifests with `provider` filled from the active provider per R5."""
    registry = request.app.state.handlers
    provider_registry = request.app.state.registry
    out: list[dict] = []
    for m in registry.manifests():
        cap = m.get("capability")
        if cap:
            try:
                active = await provider_registry.aget_active(cap)
            except Exception:  # provider lookup is best-effort for discovery
                active = None
            m = {**m, "provider": getattr(active, "name", None) if active else None}
        out.append(m)
    return out


@router.get("/handlers")
async def list_handlers(request: Request, response: Response):
    registry = request.app.state.handlers
    etag = registry.etag()
    inm = request.headers.get("if-none-match")
    if inm and inm == etag:
        return Response(status_code=304, headers={"ETag": etag})
    response.headers["ETag"] = etag
    return {"handlers": await _manifests_with_provider(request), "etag": etag}


@router.get("/handlers/{handler_id}")
async def get_handler(handler_id: str, request: Request):
    registry = request.app.state.handlers
    if registry.get(handler_id) is None:
        return JSONResponse(status_code=404, content={"error": "handler_not_found"})
    for m in await _manifests_with_provider(request):
        if m["id"] == handler_id:
            return m
    return JSONResponse(status_code=404, content={"error": "handler_not_found"})


@router.post("/handlers/{handler_id}/run")
async def run_handler(
    handler_id: str,
    request: Request,
    file: UploadFile | None = File(default=None),
    mime: str = Form(...),
    filename: str = Form(...),
    params: str = Form(default="{}"),
    language: str = Form(default="ru"),
    job_id: str | None = Form(default=None),
    source_url: str | None = Form(default=None),
):
    registry = request.app.state.handlers
    lh = registry.get(handler_id)
    if lh is None:
        return JSONResponse(status_code=404, content={"error": "handler_not_found"})

    descriptor = lh.descriptor
    if descriptor.execution == "async":
        # Out-of-process runner is delivered in Phase 5 (R10 extends THIS fn).
        return JSONResponse(status_code=501,
                            content={"error": "async_runner_not_available"})

    # R12: bytes arrive in the multipart `file` field — never fetched here.
    data = await file.read() if file is not None else b""

    try:
        parsed_params = json.loads(params) if params else {}
    except json.JSONDecodeError:
        parsed_params = {}
    if not isinstance(parsed_params, dict):
        parsed_params = {}
    parsed_params.setdefault("language", language)

    f = HandlerFile(bytes=data, mime=mime, filename=filename, size=len(data),
                    source_url=source_url)
    http = request.app.state.http_client
    ctx = build_context(request.app.state.registry, http, core_url=_core_url())

    # R5: per-execution timeout on the handler body.
    try:
        result = await asyncio.wait_for(lh.run(ctx, f, parsed_params),
                                        timeout=HANDLER_SYNC_TIMEOUT_SECS)
    except asyncio.TimeoutError:
        return JSONResponse(status_code=200, content={
            "status": "timeout", "summary_text": "",
            "artifact_urls": [], "reason": "per-execution timeout",
        })
    except Exception as e:
        log.exception("handler %s failed", handler_id)
        return JSONResponse(status_code=200, content=ctx.result.failed(str(e)).to_dict())
    return JSONResponse(status_code=200, content=result.to_dict())
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd toolgate && pytest tests/test_handlers_router.py -q
```
Expected: `9 passed`.

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/router.py toolgate/tests/test_handlers_router.py
git commit -m "feat(toolgate): /handlers router (ETag discovery + provider fill + multipart sync run with per-exec timeout)"
```

---

### Task 5: config.workspace_dir field

**Files:**
- Modify: `toolgate/config.py`
- Test: `toolgate/tests/test_config_workspace_dir.py`

**Interfaces:**
- Consumes: `ProvidersConfig(**data)` parse of `/api/media-config` JSON (extended by Task 7 to include `workspace_dir`).
- Produces: `ProvidersConfig.workspace_dir: str | None = None`. Consumed by `app.py` lifespan (Task 6) to point `HandlerRegistry.load_all` at the absolute workspace path.

- [ ] **Step 1: Write the failing test**

```python
# toolgate/tests/test_config_workspace_dir.py
from config import ProvidersConfig


def test_workspace_dir_defaults_none():
    cfg = ProvidersConfig()
    assert cfg.workspace_dir is None


def test_workspace_dir_parsed_from_payload():
    cfg = ProvidersConfig(**{
        "version": 1,
        "active": {},
        "providers": {},
        "workspace_dir": "/home/aronmav/opex/workspace",
    })
    assert cfg.workspace_dir == "/home/aronmav/opex/workspace"


def test_workspace_dir_none_when_absent():
    cfg = ProvidersConfig(**{"version": 1, "active": {}, "providers": {}})
    assert cfg.workspace_dir is None
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd toolgate && pytest tests/test_config_workspace_dir.py -q
```
Expected: `test_workspace_dir_defaults_none` / `test_workspace_dir_parsed_from_payload` fail — `ProvidersConfig` has no `workspace_dir` attribute (`AttributeError`).

- [ ] **Step 3: Write minimal implementation**

```python
# toolgate/config.py  — add the field to ProvidersConfig
class ProvidersConfig(BaseModel):
    version: int = 1
    active: dict[str, str | None] = Field(default_factory=dict)
    providers: dict[str, ProviderConfig] = Field(default_factory=dict)
    # Absolute path to the OPEX workspace dir, supplied by Core via
    # /api/media-config so toolgate loads workspace/file_handlers/*.py without
    # guessing. None when Core hasn't sent it yet (degraded/old core).
    workspace_dir: str | None = None
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd toolgate && pytest tests/test_config_workspace_dir.py tests/test_config.py -q
```
Expected: all pass (new file + existing `test_config.py` unaffected).

- [ ] **Step 5: Commit**

```bash
git add toolgate/config.py toolgate/tests/test_config_workspace_dir.py
git commit -m "feat(toolgate): add ProvidersConfig.workspace_dir field"
```

---

### Task 6: app.py wiring — HandlerRegistry in lifespan + watchfiles hot-reload

**Files:**
- Modify: `toolgate/app.py`
- Test: `toolgate/tests/test_handlers_app_wiring.py`

**Interfaces:**
- Consumes: `HandlerRegistry.load_all(builtin_dir, workspace_dir)`, `reload_file(path)`; `handlers.router.router`; `registry.config.workspace_dir` (Task 5); `watchfiles.awatch`.
- Produces: `app.state.handlers` (the python `HandlerRegistry`); `/handlers` routes mounted; `app._builtin_handlers_dir()`; a debounced watch task on `workspace/file_handlers`. Consumed by the router (Task 4) and by Phase 3+ E2E.

- [ ] **Step 0: Confirm the real lifespan tail + registry-config accessor** (grep before editing)

```bash
cd toolgate && grep -nE "await registry.aload|yield|aclose|http_client|app.include_router|from registry import" app.py
cd toolgate && grep -nE "config|aget_active|ProvidersConfig" registry.py
```
Expected: the existing `await registry.aload()` → `yield` → `await http_client.aclose()` tail in `lifespan`, the `app.include_router(...)` block, and the attribute exposing the loaded `ProvidersConfig` on the registry (grounding states `registry.config.workspace_dir`; confirm the exact accessor name and use it verbatim in Step 3).

- [ ] **Step 1: Write the failing test**

```python
# toolgate/tests/test_handlers_app_wiring.py
import importlib
import os

from fastapi.testclient import TestClient


def _empty_load():
    from config import ProvidersConfig
    return ProvidersConfig()


def test_app_mounts_handlers_and_state(monkeypatch):
    # Keep registry warm-up from touching the network.
    monkeypatch.setattr("registry._aload_config_from_api", _empty_load)
    import app as app_module
    importlib.reload(app_module)
    with TestClient(app_module.app) as client:
        # app.state.handlers is populated in lifespan with the builtin handlers
        assert hasattr(app_module.app.state, "handlers")
        ids = {m["id"] for m in app_module.app.state.handlers.manifests()}
        assert {"save", "transcribe", "describe", "extract_document"} <= ids
        # the router is mounted
        r = client.get("/handlers")
        assert r.status_code == 200
        got = {h["id"] for h in r.json()["handlers"]}
        assert {"save", "transcribe", "describe", "extract_document"} <= got


def test_builtin_dir_resolution(monkeypatch):
    monkeypatch.setattr("registry._aload_config_from_api", _empty_load)
    import app as app_module
    importlib.reload(app_module)
    # the helper must resolve to an existing directory containing the builtins
    d = app_module._builtin_handlers_dir()
    assert os.path.isdir(d)
    assert os.path.isfile(os.path.join(d, "transcribe.py"))
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd toolgate && pytest tests/test_handlers_app_wiring.py -q
```
Expected: fails — `app.state` has no `handlers`, `app_module._builtin_handlers_dir` is undefined (`AttributeError`), and `/handlers` returns 404.

- [ ] **Step 3: Write minimal implementation**

Add the imports + module-level registry + helpers near the top of `toolgate/app.py` (after the existing `from registry import ...` line; `os` and `log` already exist in app.py):

```python
# toolgate/app.py — additions
import asyncio
from pathlib import Path

from handlers.loader import HandlerRegistry
from handlers import router as handlers_router_mod

handler_registry = HandlerRegistry()


def _builtin_handlers_dir() -> str:
    return str(Path(__file__).resolve().parent / "handlers" / "builtin")


async def _watch_workspace_handlers(app: FastAPI, ws_dir: str) -> None:
    """Hot-reload workspace/file_handlers/*.py via watchfiles.awatch.
    A parse/import error in a changed file is caught inside reload_file
    (logged, previous registry kept). watchfiles debounces internally."""
    from watchfiles import awatch

    target = os.path.join(ws_dir, "file_handlers")
    os.makedirs(target, exist_ok=True)
    try:
        async for changes in awatch(target):
            for _change, path in changes:
                if path.endswith(".py"):
                    app.state.handlers.reload_file(path)
                    log.info("hot-reloaded handler file %s", path)
    except asyncio.CancelledError:
        return
    except Exception as e:
        log.warning("workspace handler watcher stopped: %s", e)
```

Replace the tail of the existing `lifespan` (the `await registry.aload()` / `yield` / `aclose` block) with:

```python
    await registry.aload()

    # File-handler hub: load builtins + workspace, mount in app.state.
    app.state.handlers = handler_registry
    ws_dir = registry.config.workspace_dir
    handler_registry.load_all(_builtin_handlers_dir(), ws_dir)
    watch_task = None
    if ws_dir:
        watch_task = asyncio.create_task(_watch_workspace_handlers(app, ws_dir))
    yield
    if watch_task:
        watch_task.cancel()
    if http_client:
        await http_client.aclose()
```

And mount the router alongside the other `app.include_router(...)` calls:

```python
app.include_router(handlers_router_mod.router)
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd toolgate && pytest tests/test_handlers_app_wiring.py -q
```
Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add toolgate/app.py toolgate/tests/test_handlers_app_wiring.py
git commit -m "feat(toolgate): wire HandlerRegistry into lifespan + watchfiles hot-reload + mount router"
```

---

### Task 7: Core — add `workspace_dir` to `/api/media-config`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/providers.rs`
- Test: `crates/opex-core/src/gateway/handlers/providers.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::config::WORKSPACE_DIR` (`"workspace"`); the existing `/api/media-config` JSON builder `json!({"version":1, "active":active_map, "providers":provider_map})`.
- Produces: the same json plus `"workspace_dir": <abs path>`, and `pub(crate) fn media_config_workspace_dir() -> String`. Consumed by toolgate `config.py` (Task 5) + `app.py` lifespan (Task 6).

- [ ] **Step 0: Confirm the real json-building site + WORKSPACE_DIR const name** (grep before editing)

```bash
grep -rn "\"active\": active_map" crates/opex-core/src/gateway/handlers/providers.rs
grep -rn "WORKSPACE_DIR" crates/opex-core/src/config
```
Expected: the exact `json!({...})` block that emits `version/active/providers` (the media-config response) and the canonical `pub const WORKSPACE_DIR: &str = "workspace";` (or equivalent). Use the exact const path found here in Step 3 (the draft assumes `crate::config::WORKSPACE_DIR`); if the JSON keys are built differently (e.g. via a struct), add the `workspace_dir` field there instead.

- [ ] **Step 1: Write the failing test**

```rust
// crates/opex-core/src/gateway/handlers/providers.rs — append at end of file
#[cfg(test)]
mod workspace_dir_tests {
    use super::media_config_workspace_dir;

    #[test]
    fn workspace_dir_is_absolute_and_ends_with_workspace() {
        let dir = media_config_workspace_dir();
        let p = std::path::Path::new(&dir);
        assert!(p.is_absolute(), "workspace_dir must be absolute, got {dir}");
        assert!(
            dir.replace('\\', "/").ends_with("workspace"),
            "workspace_dir must end with the workspace component, got {dir}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p opex-core workspace_dir_is_absolute -- --nocapture
```
Expected: compile error — `media_config_workspace_dir` is undefined (function not yet created).

- [ ] **Step 3: Write minimal implementation**

Add the resolver helper just above the media-config json builder:

```rust
/// Absolute path to the workspace dir, exported to toolgate via /api/media-config
/// so it can load workspace/file_handlers/*.py without guessing. Falls back to
/// the relative WORKSPACE_DIR string joined to CWD when canonicalize fails
/// (e.g. the dir doesn't exist yet at first boot).
pub(crate) fn media_config_workspace_dir() -> String {
    let rel = std::path::Path::new(crate::config::WORKSPACE_DIR);
    match std::fs::canonicalize(rel) {
        Ok(abs) => abs.to_string_lossy().into_owned(),
        Err(_) => std::env::current_dir()
            .map(|cwd| cwd.join(rel))
            .unwrap_or_else(|_| rel.to_path_buf())
            .to_string_lossy()
            .into_owned(),
    }
}
```

Then add the field to the returned media-config JSON (the `json!` block found in Step 0):

```rust
    json!({
        "version": 1,
        "active": active_map,
        "providers": provider_map,
        "workspace_dir": media_config_workspace_dir(),
    })
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p opex-core workspace_dir_is_absolute -- --nocapture && cargo check -p opex-core
```
Expected: `test ... ok` (1 passed) and `cargo check` succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/providers.rs
git commit -m "feat(core): expose absolute workspace_dir in /api/media-config for toolgate handler hub"
```

---

## Phase 3 — core orchestration (sync)

> **Deferred (per resolution R2):** migrating the post-send SSE "file-scenario-chips" event and the Telegram `fse:` callback onto `HandlerRegistry` is a future follow-up. `opex_types::sse::ScenarioChoice` is `{scenario_id: Uuid, label, executor}` — structurally incompatible with string handler ids — and those surfaces keep using the existing legacy `file_scenarios` mechanism (scenario_id: Uuid) untouched. Phase 3's only client surfaces are the new `GET /api/files/{upload_id}/actions` + `POST /api/files/{upload_id}/run` endpoints feeding the composer (Phase 4). The legacy in-core sync dispatch (`dispatch.rs`, `dispatch_seam.rs` incl. `PendingAlternative`) therefore STAYS — it still powers the legacy chips/Telegram path; and `summarize_video` stays on the legacy `video_jobs` dispatch in this phase (the async route here only returns a 202 stub; Phase 5 amends THIS files.rs async branch to enqueue `handler_jobs` — per R13).

> **SSRF×loopback note (R12):** in this phase the core does the loopback upload download IN RUST (mirroring `dispatch.rs::run_transcribe`) and POSTs the raw bytes to toolgate as `multipart/form-data` (field `file` + text fields `mime`, `filename`, `params`, `language`). Toolgate NEVER receives a loopback URL — `validate_url_ssrf` would reject it. The minted signed url is used ONLY by core's own loopback GET.

---

### Task 1: `HandlerManifest` + `HandlerButton` wire types and `match_buttons` tiered gate

**Files:**
- Create: `crates/opex-core/src/agent/handler_registry.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (add `pub mod handler_registry;`)

**Interfaces:**
- Consumes: `crate::agent::fse::allowlist::FSE_DEFAULT_ALLOWLIST` (`&["transcribe","describe","extract_document","save","summarize_video"]`); the toolgate manifest item wire shape `{id, labels{lang}, descriptions{lang}, icon, match:{mime:[..], max_size_mb:int|null}, capability, provider, execution, output, params:[..], order, tier}`.
- Produces: `pub struct HandlerMatch`, `pub struct HandlerManifest` (R7 shape), `pub struct HandlerButton{ id, label, icon, params }`, `pub fn match_buttons(manifests:&[HandlerManifest], mime:&str, size:u64, enabled_allowlist:&[String], lang:&str)->Vec<HandlerButton>` (consumed by `gateway/handlers/files.rs` in Task 5).

- [ ] **Step 1: Write the failing test** (append to `handler_registry.rs` test module)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mf(id: &str, tier: &str, mimes: &[&str], max_mb: Option<u64>, order: i32) -> HandlerManifest {
        let mut labels = HashMap::new();
        labels.insert("ru".to_string(), format!("{id}-ru"));
        labels.insert("en".to_string(), format!("{id}-en"));
        HandlerManifest {
            id: id.to_string(),
            labels,
            descriptions: HashMap::new(),
            icon: "mic".to_string(),
            match_: HandlerMatch {
                mime: mimes.iter().map(|s| s.to_string()).collect(),
                max_size_mb: max_mb,
            },
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: "text".to_string(),
            params: serde_json::json!([]),
            order,
            tier: tier.to_string(),
        }
    }

    fn full() -> Vec<String> {
        FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manifest_deserializes_from_toolgate_json() {
        let raw = serde_json::json!({
            "id": "transcribe",
            "labels": {"ru": "Транскрибировать", "en": "Transcribe"},
            "descriptions": {"ru": "речь в текст"},
            "icon": "mic",
            "match": {"mime": ["audio/*", "video/*"], "max_size_mb": 200},
            "capability": "stt",
            "provider": "speaches-local",
            "execution": "sync",
            "output": "text",
            "params": [{"name": "language", "type": "string", "default": "ru", "required": false}],
            "order": 10,
            "tier": "builtin"
        });
        let m: HandlerManifest = serde_json::from_value(raw).unwrap();
        assert_eq!(m.id, "transcribe");
        assert_eq!(m.match_.mime, vec!["audio/*".to_string(), "video/*".to_string()]);
        assert_eq!(m.match_.max_size_mb, Some(200));
        assert_eq!(m.tier, "builtin");
        assert_eq!(m.provider.as_deref(), Some("speaches-local"));
        assert_eq!(m.labels.get("ru").map(|s| s.as_str()), Some("Транскрибировать"));
    }

    #[test]
    fn manifest_defaults_missing_optional_fields() {
        // A minimal manifest (only id + execution) must deserialize with defaults.
        let raw = serde_json::json!({"id": "save", "execution": "sync"});
        let m: HandlerManifest = serde_json::from_value(raw).unwrap();
        assert_eq!(m.id, "save");
        assert!(m.match_.mime.is_empty());
        assert!(m.match_.max_size_mb.is_none());
        assert!(m.labels.is_empty());
        assert_eq!(m.order, 0);
    }

    #[test]
    fn builtin_button_requires_allowlist_membership() {
        // builtin "transcribe" matches audio/* and is in the full allowlist → button
        let ms = vec![mf("transcribe", "builtin", &["audio/*"], Some(200), 10)];
        let out = match_buttons(&ms, "audio/ogg", 1_000, &full(), "ru");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "transcribe");
        assert_eq!(out[0].label, "transcribe-ru");

        // same builtin id, but operator disabled it in the toggle → hidden
        let enabled = vec!["describe".to_string()];
        let out2 = match_buttons(&ms, "audio/ogg", 1_000, &enabled, "ru");
        assert!(out2.is_empty(), "disabled builtin must not produce a button");
    }

    #[test]
    fn unknown_builtin_id_is_never_offered() {
        // tier=builtin but id not in the const FSE_DEFAULT_ALLOWLIST → never a button,
        // even if a hand-edited allowlist row somehow lists it.
        let ms = vec![mf("rm_rf", "builtin", &["audio/*"], None, 1)];
        let bogus_enabled = vec!["rm_rf".to_string()];
        assert!(match_buttons(&ms, "audio/ogg", 1, &bogus_enabled, "ru").is_empty());
    }

    #[test]
    fn workspace_button_is_default_on_ignoring_allowlist() {
        // a workspace-tier handler not in the allowlist still produces a button
        let ms = vec![mf("my_ocr", "workspace", &["image/*"], None, 5)];
        let out = match_buttons(&ms, "image/png", 1_000, &full(), "ru");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "my_ocr");
    }

    #[test]
    fn non_matching_mime_and_oversize_are_excluded() {
        let ms = vec![mf("transcribe", "builtin", &["audio/*"], Some(1), 10)];
        // wrong mime
        assert!(match_buttons(&ms, "image/png", 1_000, &full(), "ru").is_empty());
        // 2 MB > 1 MB cap
        assert!(match_buttons(&ms, "audio/ogg", 2 * 1024 * 1024, &full(), "ru").is_empty());
        // within cap
        assert_eq!(match_buttons(&ms, "audio/ogg", 100, &full(), "ru").len(), 1);
    }

    #[test]
    fn buttons_sorted_by_order_and_label_falls_back_to_en() {
        let ms = vec![
            mf("describe", "workspace", &["image/*"], None, 20),
            mf("save", "workspace", &["image/*"], None, 10),
        ];
        // "fr" missing → falls back to "en"
        let out = match_buttons(&ms, "image/png", 1, &full(), "fr");
        assert_eq!(out.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), vec!["save", "describe"]);
        assert_eq!(out[0].label, "save-en");
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core handler_registry:: 2>&1 | tail -20` — fails to compile: `cannot find type HandlerManifest`.

- [ ] **Step 3: Write minimal implementation** (top of `crates/opex-core/src/agent/handler_registry.rs`)
```rust
//! Core-side discovery cache + matcher for toolgate-hosted file handlers.
//! `HandlerManifest` mirrors the toolgate `GET /handlers` item wire shape;
//! `match_buttons` is the pure tiered trust gate (builtin∩allowlist,
//! workspace default-on) that turns a mime+size into composer buttons.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent::fse::allowlist::FSE_DEFAULT_ALLOWLIST;

/// Inner `"match"` object of a manifest: mime globs + an optional size cap.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HandlerMatch {
    #[serde(default)]
    pub mime: Vec<String>,
    #[serde(default)]
    pub max_size_mb: Option<u64>,
}

/// One handler manifest as served by toolgate `GET /handlers`. Serde field
/// names match the toolgate JSON; the nested object is read via
/// `#[serde(rename = "match")]` (Rust keyword → `match_`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandlerManifest {
    pub id: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub descriptions: HashMap<String, String>,
    #[serde(default)]
    pub icon: String,
    #[serde(rename = "match", default)]
    pub match_: HandlerMatch,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub execution: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub tier: String,
}

/// A composer button derived from a manifest for a concrete file.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HandlerButton {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub params: serde_json::Value,
}

/// True if `mime` matches a glob like `audio/*` or an exact `application/pdf`.
fn mime_glob_matches(pattern: &str, mime: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        mime.split('/').next() == Some(prefix)
    } else if pattern == "*" || pattern == "*/*" {
        true
    } else {
        pattern.eq_ignore_ascii_case(mime)
    }
}

/// Localize a manifest label: requested `lang`, else `en`, else the id.
fn localize(m: &HandlerManifest, lang: &str) -> String {
    m.labels
        .get(lang)
        .or_else(|| m.labels.get("en"))
        .cloned()
        .unwrap_or_else(|| m.id.clone())
}

/// Pure tiered match: filter manifests by mime-glob + `max_size_mb`, apply the
/// trust gate by tier (builtin → must be one of the 5 const ids AND an enabled
/// member of the allowlist; workspace → allowed by default), then localize +
/// sort by `order` then id.
pub fn match_buttons(
    manifests: &[HandlerManifest],
    mime: &str,
    size: u64,
    enabled_allowlist: &[String],
    lang: &str,
) -> Vec<HandlerButton> {
    let mut matched: Vec<&HandlerManifest> = manifests
        .iter()
        .filter(|m| m.match_.mime.iter().any(|p| mime_glob_matches(p, mime)))
        .filter(|m| match m.match_.max_size_mb {
            Some(cap) => size <= cap.saturating_mul(1024 * 1024),
            None => true,
        })
        .filter(|m| match m.tier.as_str() {
            "builtin" => {
                // builtin ids hard-anchored to the const; allowlist toggle gates which are on.
                FSE_DEFAULT_ALLOWLIST.contains(&m.id.as_str())
                    && enabled_allowlist.iter().any(|x| x == &m.id)
            }
            // workspace (and any future tier) → default-on for v1 trusted authors
            _ => true,
        })
        .collect();

    matched.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    matched
        .into_iter()
        .map(|m| HandlerButton {
            id: m.id.clone(),
            label: localize(m, lang),
            icon: m.icon.clone(),
            params: m.params.clone(),
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core handler_registry:: 2>&1 | tail -20` — all 8 tests PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/agent/handler_registry.rs crates/opex-core/src/agent/mod.rs
git commit -m "feat(handlers): HandlerManifest wire type + tiered match_buttons gate"
```

---

### Task 2: `HandlerRegistry` cache with conditional-GET (ETag) refresh

**Files:**
- Modify: `crates/opex-core/src/agent/handler_registry.rs`

**Interfaces:**
- Consumes: `HandlerManifest` (Task 1); toolgate `GET /handlers` → `{"handlers":[..], "etag":".."}` with an `ETag` response header + `304` on `If-None-Match`.
- Produces: `pub struct HandlerRegistry{ inner:Arc<RwLock<HandlerCache>>, toolgate_url, http }` (derives `Clone`) with `pub fn new(toolgate_url:String, http:reqwest::Client)->Self`, `pub async fn refresh(&self)`, `pub async fn manifests(&self)->Vec<HandlerManifest>` (consumed by `files.rs` + `AppState` `FromRef` in Tasks 4-5).

- [ ] **Step 1: Write the failing test** (append to the `tests` module). Uses `wiremock` (already a dev-dependency of `opex-core` — used by the provider tests):
```rust
    #[tokio::test]
    async fn refresh_loads_then_keeps_cache_on_304() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "handlers": [{
                "id": "transcribe",
                "labels": {"ru": "Транскрибировать"},
                "icon": "mic",
                "match": {"mime": ["audio/*"], "max_size_mb": 200},
                "execution": "sync",
                "output": "text",
                "params": [],
                "order": 10,
                "tier": "builtin"
            }],
            "etag": "abc123"
        });
        // First GET → 200 with ETag.
        Mock::given(method("GET"))
            .and(path("/handlers"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"abc123\"")
                    .set_body_json(&body),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Subsequent GETs (conditional) → 304.
        Mock::given(method("GET"))
            .and(path("/handlers"))
            .respond_with(ResponseTemplate::new(304))
            .mount(&server)
            .await;

        let reg = HandlerRegistry::new(server.uri(), reqwest::Client::new());
        reg.refresh().await;
        let ms = reg.manifests().await;
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].id, "transcribe");

        // second refresh: server returns 304 → cache kept, manifests unchanged
        reg.refresh().await;
        assert_eq!(reg.manifests().await.len(), 1, "304 must keep prior cache");
    }

    #[tokio::test]
    async fn refresh_failsoft_keeps_cache_when_toolgate_down() {
        let reg = HandlerRegistry::new("http://127.0.0.1:1".to_string(), reqwest::Client::new());
        // never loaded; a failing refresh must not panic and leaves empty cache
        reg.refresh().await;
        assert!(reg.manifests().await.is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core handler_registry::tests::refresh 2>&1 | tail -20` — fails: `cannot find type HandlerRegistry`.

- [ ] **Step 3: Write minimal implementation** (append below `match_buttons`; add `use std::sync::Arc;` and `use tokio::sync::RwLock;` to the file's imports)
```rust
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cached state behind the registry lock.
#[derive(Default)]
pub struct HandlerCache {
    manifests: Vec<HandlerManifest>,
    etag: Option<String>,
}

/// Discovery cache of toolgate handler manifests. Refresh via conditional GET
/// (`If-None-Match` ETag); a 304 or a transport error keeps the prior cache
/// (fail-soft, so composer buttons still render when toolgate is briefly down).
#[derive(Clone)]
pub struct HandlerRegistry {
    inner: Arc<RwLock<HandlerCache>>,
    toolgate_url: String,
    http: reqwest::Client,
}

/// Top-level shape of the toolgate `GET /handlers` response.
#[derive(Deserialize)]
struct HandlersResponse {
    handlers: Vec<HandlerManifest>,
    #[serde(default)]
    etag: Option<String>,
}

impl HandlerRegistry {
    pub fn new(toolgate_url: String, http: reqwest::Client) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HandlerCache::default())),
            toolgate_url,
            http,
        }
    }

    /// Conditional GET of `{toolgate_url}/handlers`. 200 replaces the cache;
    /// 304 / any non-2xx / transport error / bad JSON leaves it untouched.
    pub async fn refresh(&self) {
        let url = format!("{}/handlers", self.toolgate_url.trim_end_matches('/'));
        let prior_etag = self.inner.read().await.etag.clone();
        let mut req = self.http.get(&url);
        if let Some(tag) = &prior_etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, tag.clone());
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "handler registry refresh failed; keeping cache");
                return;
            }
        };
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return;
        }
        if !resp.status().is_success() {
            tracing::warn!(status = %resp.status(), "handler registry refresh non-2xx; keeping cache");
            return;
        }
        let header_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        match resp.json::<HandlersResponse>().await {
            Ok(parsed) => {
                let mut guard = self.inner.write().await;
                guard.manifests = parsed.handlers;
                guard.etag = header_etag.or(parsed.etag);
            }
            Err(e) => {
                tracing::warn!(error = %e, "handler registry bad JSON; keeping cache");
            }
        }
    }

    /// Snapshot of the cached manifests (clones the Vec — small, ≤ ~20 items).
    pub async fn manifests(&self) -> Vec<HandlerManifest> {
        self.inner.read().await.manifests.clone()
    }
}
```

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core handler_registry::tests::refresh 2>&1 | tail -20` — both tests PASS. (If `wiremock` is NOT yet a dev-dependency of `opex-core`, add it: `cargo add --dev wiremock -p opex-core` and re-run; commit `Cargo.toml`/`Cargo.lock` together with this task.)

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/agent/handler_registry.rs Cargo.toml Cargo.lock
git commit -m "feat(handlers): HandlerRegistry cache w/ conditional-GET ETag refresh"
```

---

### Task 3: provenance wrapper + migration 066 (`messages.source`)

**Files:**
- Create: `crates/opex-core/src/agent/provenance.rs`
- Create: `migrations/066_messages_source.sql`
- Modify: `crates/opex-core/src/agent/mod.rs` (add `pub mod provenance;`)

**Interfaces:**
- Produces: `pub fn wrap_file_output(handler_id:&str, upload_id:&str, body:&str)->String` (applied **at persist time** by `files.rs` in Task 5 with real ids — R4); the `messages.source TEXT` column (set to `'file_handler'` by the same INSERT). No `context_builder`/`MessageRow` change is needed: the wrapper is already baked into `content`.

- [ ] **Step 1: Write the failing test** (create `provenance.rs` with its test module)
```rust
//! Provenance tagging for file-derived message content. The LLM-facing content
//! of a `source='file_handler'` message is wrapped in a `<file_output>`
//! delimiter so the model treats it as untrusted data, not instructions (closes
//! the multimodal prompt-injection channel — FSE extensibility research
//! 2026-06-24). The wrapper is applied once, at persist time, with the real
//! handler + upload ids; the stored `content` already carries it.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_body_with_handler_and_upload_attrs() {
        let out = wrap_file_output("transcribe", "abc-123", "привет мир");
        assert_eq!(
            out,
            "<file_output handler=\"transcribe\" upload=\"abc-123\" trust=\"untrusted\">\nпривет мир\n</file_output>"
        );
    }

    #[test]
    fn escapes_quotes_in_attribute_values() {
        // a forged handler/upload id with a quote must not break out of the attr
        let out = wrap_file_output("a\"b", "u\"d", "body");
        assert!(out.starts_with("<file_output handler=\"a&quot;b\" upload=\"u&quot;d\""));
        assert!(out.contains("trust=\"untrusted\""));
        assert!(out.ends_with("</file_output>"));
    }

    #[test]
    fn body_is_preserved_verbatim_between_delimiters() {
        let body = "line1\nline2 with <tags> & ampersand";
        let out = wrap_file_output("h", "u", body);
        assert!(out.contains(&format!("\n{body}\n")), "body must survive verbatim: {out}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core provenance:: 2>&1 | tail -20` — fails: `cannot find function wrap_file_output`.

- [ ] **Step 3: Write minimal implementation** (prepend above the test module in `provenance.rs`)
```rust
/// Escape `&` and `"` for safe inclusion in an XML-ish attribute value.
fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

/// Wrap file-derived `body` in a `<file_output>` provenance delimiter. The
/// attributes carry the originating handler + upload id; `trust="untrusted"`
/// signals the model this is data from a file, not instructions. The body is
/// inserted verbatim (only the attribute values are escaped) so the LLM sees
/// the exact processed text on its own line.
pub fn wrap_file_output(handler_id: &str, upload_id: &str, body: &str) -> String {
    format!(
        "<file_output handler=\"{}\" upload=\"{}\" trust=\"untrusted\">\n{}\n</file_output>",
        attr_escape(handler_id),
        attr_escape(upload_id),
        body
    )
}
```
Create `migrations/066_messages_source.sql`:
```sql
-- File Handler Hub Phase 3: provenance tag for file-derived messages.
-- NULL = ordinary message (trunk default). 'file_handler' = produced by a
-- handler run. The <file_output> provenance wrapper is baked into the stored
-- `content` at persist time (no read-path edit needed); this column lets the
-- UI strip the wrapper for display in a later follow-up.
ALTER TABLE messages ADD COLUMN source TEXT;
```

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core provenance:: 2>&1 | tail -20` — 3 tests PASS. (Migration is exercised end-to-end in Task 5's `#[sqlx::test]`.)

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/agent/provenance.rs crates/opex-core/src/agent/mod.rs migrations/066_messages_source.sql
git commit -m "feat(handlers): wrap_file_output provenance + migration 066 messages.source"
```

---

### Task 4: wire `HandlerRegistry` into `AppState` + construct it in `main.rs`

**Files:**
- Modify: `crates/opex-core/src/gateway/state.rs`
- Modify: `crates/opex-core/src/main.rs`

**Interfaces:**
- Consumes: `crate::agent::handler_registry::HandlerRegistry::new(toolgate_url, http)` (Task 2); the resolved `toolgate_url` String already bound at `main.rs:382`.
- Produces: `AppState.handlers: HandlerRegistry` field + `impl FromRef<AppState> for HandlerRegistry` (consumed by `files.rs` handlers in Task 5).

- [ ] **Step 1: Write the failing test** (append to `state.rs`, under `#[cfg(test)]`)
```rust
#[cfg(test)]
mod handlers_field_tests {
    use super::*;
    use crate::agent::handler_registry::HandlerRegistry;

    #[tokio::test]
    async fn appstate_exposes_handler_registry_via_fromref() {
        let reg = HandlerRegistry::new("http://127.0.0.1:9011".to_string(), reqwest::Client::new());
        let state = AppState {
            agents: crate::gateway::clusters::AgentCore::test_empty().await,
            auth: crate::gateway::clusters::AuthServices::test_new(),
            infra: crate::gateway::clusters::InfraServices::test_new(),
            channels: crate::gateway::clusters::ChannelBus::test_new(),
            config: crate::gateway::clusters::ConfigServices::test_new(),
            status: crate::gateway::clusters::StatusMonitor::test_new(),
            handlers: reg,
        };
        // FromRef must resolve the new field for axum State extraction.
        let extracted = HandlerRegistry::from_ref(&state);
        assert!(extracted.manifests().await.is_empty());
    }
}
```
> NOTE: if the existing `test_*` cluster constructors are named differently in `state.rs`, mirror EXACTLY whatever an existing `#[cfg(test)]` block in `state.rs` already uses to build an `AppState` (do not invent constructor names). The only NEW assertion is that the `handlers` field exists and `FromRef` resolves it.

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core handlers_field_tests 2>&1 | tail -20` — fails: `struct AppState has no field named handlers`.

- [ ] **Step 3: Write minimal implementation**. In `state.rs`, add the field to `AppState` (after `status`) and the `FromRef`:
```rust
#[derive(Clone)]
pub struct AppState {
    pub agents:   AgentCore,
    pub auth:     AuthServices,
    pub infra:    InfraServices,
    pub channels: ChannelBus,
    pub config:   ConfigServices,
    pub status:   StatusMonitor,
    /// Discovery cache of toolgate-hosted file handlers (File Handler Hub).
    pub handlers: crate::agent::handler_registry::HandlerRegistry,
}
```
```rust
impl FromRef<AppState> for crate::agent::handler_registry::HandlerRegistry {
    fn from_ref(s: &AppState) -> Self { s.handlers.clone() }
}
```
In `main.rs`, inside the `let state = gateway::AppState { ... }` literal (after the `status: ...` field at ~line 730), add the new field using the already-resolved `toolgate_url` String (bound at line 382):
```rust
        handlers: crate::agent::handler_registry::HandlerRegistry::new(
            toolgate_url.clone(),
            reqwest::Client::new(),
        ),
```

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core handlers_field_tests 2>&1 | tail -20` — PASS; `cargo check -p opex-core 2>&1 | tail -5` — clean.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/gateway/state.rs crates/opex-core/src/main.rs
git commit -m "feat(handlers): add HandlerRegistry to AppState + construct in main"
```

---

### Task 5: `files.rs` handlers — owner-gate + `GET .../actions` + `POST .../run` (sync, multipart-to-toolgate) + register routes

**Files:**
- Create: `crates/opex-core/src/gateway/handlers/files.rs`
- Modify: `crates/opex-core/src/gateway/handlers/mod.rs` (add `pub(crate) mod files;`)
- Modify: `crates/opex-core/src/gateway/mod.rs` (`.merge(handlers::files::routes())`)

**Interfaces:**
- Consumes: `HandlerRegistry::{refresh, manifests}` + `match_buttons` (Tasks 1-2); `db::uploads::get_by_id(pool,id)->Result<Option<UploadRow{mime,size_bytes,..}>>`; `agent::fse::allowlist_store::get_enabled_allowlist(db)->Vec<String>`; `infra.secrets.get_upload_hmac_key()->[u8;32]` (R6); `uploads::{mint_uploads_url, web_uploads_base, uploads_local_url}` (R6); `ScenarioOutcome`/`ScenarioStatus`; `provenance::wrap_file_output` (R4); `config.config.{toolgate_url, gateway.listen, uploads.signed_url_ttl_secs}`; `channels.ui_event_tx`. Mirrors the loopback-download + multipart-POST pattern of `agent/file_scenario/dispatch.rs::run_transcribe` (R12).
- Produces: `pub(crate) fn routes()->Router<AppState>`; `pub(crate) async fn assert_upload_accessible(db, upload_id)->Result<UploadMeta,(StatusCode,Json)>` (R3); `GET /api/files/{upload_id}/actions`; `POST /api/files/{upload_id}/run` (sync path → loopback-download bytes in Rust → multipart POST toolgate → persist `source='file_handler'` message wrapped via `wrap_file_output` + broadcast `file` SSE; async path → 202 stub, amended in Phase 5).

- [ ] **Step 1: Write the failing test** (in `files.rs` `#[cfg(test)]`)
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outcome_four_key_json_defaults_video_accepted() {
        // R9: toolgate emits 4 keys; ScenarioOutcome has a 5th (video_accepted,
        // serde default) — deserialization must succeed with it false.
        let raw = r#"{"status":"ok","summary_text":"привет мир","artifact_urls":["/api/uploads/1?sig=x"],"reason":null}"#;
        let o: crate::agent::file_scenario::outcome::ScenarioOutcome =
            serde_json::from_str(raw).unwrap();
        assert_eq!(o.status, crate::agent::file_scenario::outcome::ScenarioStatus::Ok);
        assert_eq!(o.summary_text, "привет мир");
        assert_eq!(o.artifact_urls, vec!["/api/uploads/1?sig=x".to_string()]);
        assert!(!o.video_accepted, "missing key defaults to false");
    }

    #[test]
    fn parse_outcome_too_large_from_toolgate_json() {
        let raw = r#"{"status":"too_large","summary_text":"","artifact_urls":[],"reason":"over 50MB"}"#;
        let o: crate::agent::file_scenario::outcome::ScenarioOutcome =
            serde_json::from_str(raw).unwrap();
        assert_eq!(o.status, crate::agent::file_scenario::outcome::ScenarioStatus::TooLarge);
        assert_eq!(o.reason.as_deref(), Some("over 50MB"));
    }

    #[test]
    fn run_request_deserializes_from_composer_body() {
        let raw = serde_json::json!({
            "handler_id": "transcribe",
            "params": {"language": "ru"},
            "session_id": "00000000-0000-0000-0000-000000000001",
            "agent": "Atlas"
        });
        let req: FileRunRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.handler_id, "transcribe");
        assert_eq!(req.agent, "Atlas");
        assert_eq!(req.params["language"], "ru");
    }

    #[test]
    fn run_toolgate_url_is_built_correctly() {
        assert_eq!(
            toolgate_run_url("http://localhost:9011/", "transcribe"),
            "http://localhost:9011/handlers/transcribe/run"
        );
        assert_eq!(
            toolgate_run_url("http://localhost:9011", "describe"),
            "http://localhost:9011/handlers/describe/run"
        );
    }

    #[test]
    fn persisted_content_carries_file_output_wrapper() {
        // The persist body for an ok outcome is the wrapped summary (R4).
        let upload = "11111111-1111-1111-1111-111111111111";
        let wrapped =
            crate::agent::provenance::wrap_file_output("transcribe", upload, "распознанный текст");
        assert!(wrapped.starts_with(&format!(
            "<file_output handler=\"transcribe\" upload=\"{upload}\" trust=\"untrusted\">"
        )));
        assert!(wrapped.contains("\nраспознанный текст\n"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core gateway::handlers::files 2>&1 | tail -20` — fails: `cannot find type FileRunRequest` / `cannot find function toolgate_run_url`.

- [ ] **Step 3: Write minimal implementation** (`files.rs`). The sync path mirrors `dispatch.rs::run_transcribe` exactly (R12): mint loopback signed url → core GETs the bytes over loopback → POST `multipart/form-data` (field `file` + text fields) to toolgate. Toolgate never sees the loopback url.
```rust
//! File Handler Hub — core orchestration routes (sync path).
//! `GET /api/files/{upload_id}/actions` returns the per-file button list;
//! `POST /api/files/{upload_id}/run` re-checks the tiered gate server-side,
//! downloads the upload bytes over LOOPBACK (in Rust), POSTs them as
//! multipart/form-data to toolgate `/handlers/{id}/run`, then persists the
//! result as a provenance-wrapped `source='file_handler'` message + SSE-
//! broadcasts produced artifacts. Toolgate never receives a loopback URL
//! (its SSRF guard would reject it) — mirrors dispatch.rs::run_transcribe (R12).

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agent::file_scenario::outcome::{ScenarioOutcome, ScenarioStatus};
use crate::agent::handler_registry::{HandlerRegistry, match_buttons};
use crate::gateway::AppState;
use crate::gateway::clusters::{ChannelBus, ConfigServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/files/{upload_id}/actions", get(get_file_actions))
        .route("/api/files/{upload_id}/run", post(run_file_handler))
}

#[derive(Deserialize)]
pub(crate) struct ActionsQuery {
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub session: Option<Uuid>,
    #[serde(default)]
    pub lang: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct FileRunRequest {
    pub handler_id: String,
    #[serde(default)]
    pub params: Value,
    pub session_id: Uuid,
    pub agent: String,
    #[serde(default)]
    pub lang: Option<String>,
}

/// Minimal upload facts the owner-gate proves before any handler runs.
#[derive(Debug, Clone)]
pub(crate) struct UploadMeta {
    pub mime: String,
    pub size: u64,
}

/// Build the toolgate run endpoint for `id`, tolerant of a trailing slash.
pub(crate) fn toolgate_run_url(toolgate_url: &str, id: &str) -> String {
    format!("{}/handlers/{}/run", toolgate_url.trim_end_matches('/'), id)
}

/// R3 owner-gate (single-tenant v1): the upload must exist and be one of the
/// user-facing owner types. v1 is single-user, so existence + type IS the gate;
/// a multi-tenant per-user ACL is a deferred follow-up. Returns the mime+size
/// both endpoints need so the row is read exactly once.
pub(crate) async fn assert_upload_accessible(
    db: &sqlx::PgPool,
    upload_id: Uuid,
) -> Result<UploadMeta, (StatusCode, Json<Value>)> {
    // get_by_id returns the row (id/mime/data/size/expires); fetch owner_type
    // separately (cheap scalar, no BYTEA) to enforce the type gate.
    let owner_type: Option<String> = sqlx::query_scalar(
        r#"SELECT owner_type FROM uploads
           WHERE id = $1 AND (expires_at IS NULL OR expires_at > NOW())"#,
    )
    .bind(upload_id)
    .fetch_optional(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "assert_upload_accessible owner_type lookup failed");
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "db"})))
    })?;

    let owner_type = match owner_type {
        Some(t) => t,
        None => return Err((StatusCode::NOT_FOUND, Json(json!({"error": "upload not found"})))),
    };
    if owner_type != "client_upload" && owner_type != "tool_output" {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "upload not accessible"}))));
    }

    let row = crate::db::uploads::get_by_id(db, upload_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "assert_upload_accessible row lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "db"})))
        })?
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"error": "upload not found"}))))?;

    Ok(UploadMeta { mime: row.mime, size: row.size_bytes.max(0) as u64 })
}

async fn get_file_actions(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    Path(upload_id): Path<Uuid>,
    Query(q): Query<ActionsQuery>,
) -> impl IntoResponse {
    let meta = match assert_upload_accessible(&infra.db, upload_id).await {
        Ok(m) => m,
        Err((status, body)) => return (status, body).into_response(),
    };
    handlers.refresh().await;
    let manifests = handlers.manifests().await;
    let enabled = crate::agent::fse::allowlist_store::get_enabled_allowlist(&infra.db).await;
    let lang = q.lang.as_deref().unwrap_or("ru");
    let buttons = match_buttons(&manifests, &meta.mime, meta.size, &enabled, lang);
    Json(json!({ "buttons": buttons })).into_response()
}

async fn run_file_handler(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    State(config): State<ConfigServices>,
    State(channels): State<ChannelBus>,
    Path(upload_id): Path<Uuid>,
    Json(req): Json<FileRunRequest>,
) -> impl IntoResponse {
    // 1. Owner-gate (R3): exists + accessible type; also yields mime/size.
    let meta = match assert_upload_accessible(&infra.db, upload_id).await {
        Ok(m) => m,
        Err((status, body)) => return (status, body).into_response(),
    };

    // 2. Re-check the tiered gate server-side (client-sent buttons are not trusted).
    let lang = req.lang.as_deref().unwrap_or("ru");
    handlers.refresh().await;
    let manifests = handlers.manifests().await;
    let enabled = crate::agent::fse::allowlist_store::get_enabled_allowlist(&infra.db).await;
    let allowed = match_buttons(&manifests, &meta.mime, meta.size, &enabled, lang)
        .iter()
        .any(|b| b.id == req.handler_id);
    if !allowed {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "handler not permitted for this file"})),
        )
            .into_response();
    }

    // Async path is Phase 5 (per R13 it is amended IN THIS BRANCH to enqueue a
    // handler_jobs row + 202 ack). In Phase 3 it is a stub 202 only.
    if manifests
        .iter()
        .find(|m| m.id == req.handler_id)
        .map(|m| m.execution.as_str())
        == Some("async")
    {
        return (
            StatusCode::ACCEPTED,
            Json(json!({"accepted": true, "note": "async handled in Phase 5"})),
        )
            .into_response();
    }

    // 3. Mint a LOOPBACK signed url and download the upload bytes IN RUST (R12).
    //    Toolgate never sees this url — its SSRF guard rejects loopback.
    let key = infra.secrets.get_upload_hmac_key();
    let ttl = config.config.uploads.signed_url_ttl_secs;
    let web_url = crate::uploads::mint_uploads_url(crate::uploads::web_uploads_base(), upload_id, &key, ttl);
    let loopback = crate::uploads::uploads_local_url(&web_url, &config.config.gateway.listen);

    let http = reqwest::Client::new();
    let bytes = match http.get(&loopback).send().await {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Json(ScenarioOutcome::failed(format!("upload read: {e}"))).into_response();
            }
        },
        Ok(r) => {
            return Json(ScenarioOutcome::failed(format!("upload fetch HTTP {}", r.status().as_u16())))
                .into_response();
        }
        Err(e) => {
            return Json(ScenarioOutcome::failed(format!("upload fetch: {e}"))).into_response();
        }
    };

    // 4. POST multipart/form-data to toolgate /handlers/{id}/run (R12): bytes in
    //    field "file", metadata in text fields. Mirrors dispatch.rs::run_transcribe.
    let url = toolgate_run_url(
        config.config.toolgate_url.as_deref().unwrap_or("http://localhost:9011"),
        &req.handler_id,
    );
    let params_str = serde_json::to_string(&req.params).unwrap_or_else(|_| "{}".to_string());
    let file_part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(upload_id.to_string())
        .mime_str(&meta.mime)
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.to_vec()));
    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("mime", meta.mime.clone())
        .text("filename", upload_id.to_string())
        .text("size", meta.size.to_string())
        .text("params", params_str)
        .text("language", lang.to_string());

    let outcome: ScenarioOutcome = match http.post(&url).multipart(form).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(o) => o,
            Err(e) => ScenarioOutcome::failed(format!("toolgate bad JSON: {e}")),
        },
        Ok(resp) => ScenarioOutcome::failed(format!("toolgate HTTP {}", resp.status().as_u16())),
        Err(e) => ScenarioOutcome::failed(format!("toolgate request: {e}")),
    };

    // 5. On ok: persist a provenance-wrapped message (source='file_handler', no
    //    explicit status → table default per R8) + SSE-broadcast artifacts.
    if matches!(outcome.status, ScenarioStatus::Ok) {
        let content = crate::agent::provenance::wrap_file_output(
            &req.handler_id,
            &upload_id.to_string(),
            &outcome.summary_text,
        );
        let _ = sqlx::query(
            r#"INSERT INTO messages (session_id, agent_id, role, content, source)
               VALUES ($1, $2, 'assistant', $3, 'file_handler')"#,
        )
        .bind(req.session_id)
        .bind(&req.agent)
        .bind(&content)
        .execute(&infra.db)
        .await;

        for artifact in &outcome.artifact_urls {
            let _ = channels.ui_event_tx.send(
                json!({"type": "file", "url": artifact, "mediaType": meta.mime}).to_string(),
            );
        }
    }

    Json(outcome).into_response()
}
```
Then register: in `gateway/handlers/mod.rs` add `pub(crate) mod files;`, and in `gateway/mod.rs` `router()` add `.merge(handlers::files::routes())` next to the other `.merge(...)` calls.

> NOTE on accessor verification before writing Step 3: confirm with a quick read of `db/uploads.rs` that `get_by_id` returns `Result<Option<UploadRow>>` and the row field for size is `size_bytes: i64` (the grounding lists `insert_with_retention` + `MAX_UPLOAD_BYTES`; the row getter and field names must match the real struct — do not invent). Likewise confirm `ConfigServices` exposes `config.gateway.listen`, `config.toolgate_url: Option<String>`, and `config.uploads.signed_url_ttl_secs`, and that `InfraServices` exposes `db: PgPool` + `secrets.get_upload_hmac_key() -> [u8;32]` (R6). If any field/getter name differs, use the real one — the contract names above are authoritative for intent, the local field names are whatever `state.rs`/`uploads.rs` actually define.

- [ ] **Step 3b: Add DB-backed tests** (append to the `tests` module — run under `make test-db`)
```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn owner_gate_accepts_client_upload_and_yields_mime(pool: sqlx::PgPool) {
        let id = crate::db::uploads::insert_with_retention(
            &pool, "client_upload", Some("user-1"), "audio/ogg", b"OggSfake".to_vec(), 30,
        ).await.unwrap();
        let meta = super::assert_upload_accessible(&pool, id).await.unwrap();
        assert_eq!(meta.mime, "audio/ogg");
        assert_eq!(meta.size, b"OggSfake".len() as u64);

        // missing upload → 404
        let err = super::assert_upload_accessible(&pool, uuid::Uuid::new_v4()).await.unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn messages_source_column_exists_after_066(pool: sqlx::PgPool) {
        // Migration 066 must apply: inserting with source='file_handler' succeeds
        // and the column defaults NULL otherwise.
        sqlx::query(
            r#"INSERT INTO messages (session_id, agent_id, role, content, source)
               VALUES (gen_random_uuid(), 'Atlas', 'assistant', 'x', 'file_handler')"#,
        )
        .execute(&pool).await.unwrap();
        let src: Option<String> =
            sqlx::query_scalar(r#"SELECT source FROM messages WHERE source = 'file_handler' LIMIT 1"#)
                .fetch_one(&pool).await.unwrap();
        assert_eq!(src.as_deref(), Some("file_handler"));
    }
```
> NOTE: if `insert_with_retention`'s owner-id parameter is non-optional or typed differently than `Option<&str>`, match its real signature (grounding: `insert_with_retention(pool,"client_upload"|"tool_output",owner_id,mime,data,retention_days)->Uuid`). The owner-id value is irrelevant to the gate (the gate keys on owner_type only).

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core gateway::handlers::files 2>&1 | tail -20` (unit tests PASS); with a DB: `make test-db` covers the `#[sqlx::test]` cases (migration 066 applies; owner-gate + column verified); `cargo check -p opex-core 2>&1 | tail -5` — clean.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/gateway/handlers/files.rs crates/opex-core/src/gateway/handlers/mod.rs crates/opex-core/src/gateway/mod.rs
git commit -m "feat(handlers): /api/files actions + sync run (loopback-download + multipart) with owner-gate + provenance persist"
```

---

### Task 6: phase verification gate

**Files:**
- No code changes — verification only.

**Interfaces:**
- Confirms the Phase 3 surfaces compile, lint clean, and pass: `handler_registry` (Tasks 1-2), `provenance` (Task 3), `AppState.handlers` wiring (Task 4), `files.rs` actions/run + owner-gate + provenance persist (Task 5). No `context_builder`/`MessageRow` edit exists (R4: provenance is baked into stored `content` at persist time). No SSE-chips/Telegram migration exists (R2: deferred). Toolgate is never handed a loopback URL — core downloads bytes in Rust and POSTs multipart (R12). The async branch is a 202 stub only — Phase 5 amends THIS files.rs branch to enqueue `handler_jobs` (R13).

- [ ] **Step 1: Confirm no descoped / loopback-to-toolgate symbols leaked in.** Grep must return nothing — these were removed per R2/R4/R12 and must not exist in Phase 3 code:
```bash
grep -rn "buttons_to_scenario_choices\|run_handler_for_callback\|maybe_wrap_provenance\|ScenarioChoice {\|signed_url" crates/opex-core/src/agent/handler_registry.rs crates/opex-core/src/gateway/handlers/files.rs crates/opex-core/src/agent/provenance.rs
```
Expected: empty output (no matches). In particular, NO `signed_url` form field is sent to toolgate (R12 — core downloads bytes in Rust and POSTs multipart `file`). The legacy `dispatch_seam.rs` `ScenarioChoice` is untouched and out of Phase 3 scope.

- [ ] **Step 2: Run the full gate.**
```bash
make check 2>&1 | tail -5
make lint 2>&1 | tail -10
cargo test -p opex-core handler_registry:: provenance:: gateway::handlers::files 2>&1 | tail -20
```
Expected: `cargo check` clean; `clippy` zero warnings; all unit tests PASS. (DB-backed `#[sqlx::test]` cases run under `make test-db`.)

- [ ] **Step 3: (No-op) — verification task carries no implementation step.**

- [ ] **Step 4: Confirm green.** All three commands above report success with no warnings/failures.

- [ ] **Step 5: Commit** (only if the gate surfaced a fixable lint/format nit; otherwise nothing to commit)
```bash
git add -A
git commit -m "chore(handlers): phase 3 verification gate green (check/lint/tests)"
```

---

## Phase 4 — UI composer

The button shape is `FileActionButton{id,label,icon,params}` with `label` already localized server-side. Per **R1** the upload identifier is the upload row UUID (`result.filename` from `POST /api/media/upload`), NOT the served URL path — `ChatComposer.handleFileAdd` must capture it into a new `uploadId` field on `AttachmentEntry`, and `FileActionButtons` receives that UUID. Video still routes through the legacy async path; in this phase its button is just another action button (the routing decision is server-side). This phase renders buttons, runs on click, shows a spinner.

---

### Task 1: Add `FileActionButton` / `FileActionsResponse` types

**Files:**
- Modify: `ui/src/types/api.ts`
- Test: `ui/src/__tests__/file-action-types.test.ts`

**Interfaces:**
- Consumes: the core wire contract `GET /api/files/{upload_id}/actions` → `{buttons:[{id,label,icon,params}]}` and `POST /api/files/{upload_id}/run` body `{handler_id, params, session_id, agent}` (from Phase 3 `gateway/handlers/files.rs`, `HandlerButton`).
- Produces: `FileActionButton`, `FileActionsResponse` (consumed by `FileActionButtons.tsx` in Task 2 and the integration in Task 3).

- [ ] **Step 1: Write the failing test** (compile-level type-shape assertion; the `upload_id` in any URL is a row UUID per R1)
```ts
// ui/src/__tests__/file-action-types.test.ts
import { describe, it, expect } from "vitest";
import type { FileActionButton, FileActionsResponse } from "@/types/api";

describe("file-handler action types", () => {
  it("FileActionButton has id/label/icon/params", () => {
    const btn: FileActionButton = {
      id: "transcribe",
      label: "Транскрибировать",
      icon: "mic",
      params: { language: "ru" },
    };
    expect(btn.id).toBe("transcribe");
    expect(btn.label).toBe("Транскрибировать");
    expect(btn.icon).toBe("mic");
    expect(btn.params).toEqual({ language: "ru" });
  });

  it("FileActionsResponse wraps a buttons array", () => {
    const resp: FileActionsResponse = {
      buttons: [{ id: "describe", label: "Describe", icon: "image", params: {} }],
    };
    expect(resp.buttons).toHaveLength(1);
    expect(resp.buttons[0].id).toBe("describe");
  });
});
```

- [ ] **Step 2: Run test to verify it fails** — `cd ui && npm test -- file-action-types`. Expected failure: `error TS2305: Module '"@/types/api"' has no exported member 'FileActionButton'` (and `FileActionsResponse`); the test file fails to compile/import.

- [ ] **Step 3: Write minimal implementation** — append to the end of `ui/src/types/api.ts`:
```ts
// ── File Handler Hub (Phase 4) ─────────────────────────────────────────────────
// Source: crates/opex-core/src/gateway/handlers/files.rs (HandlerButton)
// GET /api/files/{upload_id}/actions → { buttons: FileActionButton[] }
// `upload_id` is the upload ROW UUID (the `filename` field of POST /api/media/upload).
// `label` is already localized server-side by the request locale.

export interface FileActionButton {
  id: string;
  label: string;
  icon: string;
  params: Record<string, unknown>;
}

export interface FileActionsResponse {
  buttons: FileActionButton[];
}
```

- [ ] **Step 4: Run test to verify it passes** — `cd ui && npm test -- file-action-types`. Expected: 2 tests pass (PASS).

- [ ] **Step 5: Commit**
```bash
git add ui/src/types/api.ts ui/src/__tests__/file-action-types.test.ts
git commit -m "feat(ui): add FileActionButton/FileActionsResponse types for file handler hub"
```

---

### Task 2: `FileActionButtons` component — fetch actions, render buttons, run on click with spinner

**Files:**
- Create: `ui/src/app/(authenticated)/chat/composer/FileActionButtons.tsx`
- Test: `ui/src/app/(authenticated)/chat/composer/__tests__/FileActionButtons.test.tsx`

**Interfaces:**
- Consumes: `apiGet<FileActionsResponse>(path)` and `apiPost<unknown>(path, body)` from `@/lib/api`; `useLanguageStore((s)=>s.locale)` from `@/stores/language-store`; types `FileActionButton`, `FileActionsResponse` from `@/types/api` (Task 1).
- Produces: `FileActionButtons` (named export) with props `{ uploadId: string; mime: string; agent: string; sessionId: string | null }` — consumed by `ChatComposer.tsx` in Task 3. Per R1, `uploadId` is the upload row UUID, so all `/api/files/{uploadId}/...` calls use that UUID (the test uses `11111111-1111-1111-1111-111111111111`, never a `/uploads/...` path).

- [ ] **Step 1: Write the failing test**
```tsx
// ui/src/app/(authenticated)/chat/composer/__tests__/FileActionButtons.test.tsx
import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/stores/language-store", () => ({
  useLanguageStore: (selector?: (s: { locale: string }) => unknown) => {
    const state = { locale: "ru" };
    return selector ? selector(state) : state;
  },
}));

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock("@/lib/api", () => ({
  apiGet: (...a: unknown[]) => apiGet(...a),
  apiPost: (...a: unknown[]) => apiPost(...a),
}));

import { FileActionButtons } from "../FileActionButtons";

const UPLOAD_ID = "11111111-1111-1111-1111-111111111111";
const PROPS = { uploadId: UPLOAD_ID, mime: "audio/ogg", agent: "main", sessionId: "sess-1" };

describe("FileActionButtons", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    apiGet.mockResolvedValue({
      buttons: [
        { id: "transcribe", label: "Транскрибировать", icon: "mic", params: { language: "ru" } },
        { id: "describe", label: "Описать", icon: "image", params: {} },
      ],
    });
    apiPost.mockResolvedValue({});
  });

  it("fetches actions for the upload + agent + session on mount", async () => {
    render(<FileActionButtons {...PROPS} />);
    await waitFor(() =>
      expect(apiGet).toHaveBeenCalledWith(
        `/api/files/${UPLOAD_ID}/actions?agent=main&session=sess-1`,
      ),
    );
  });

  it("renders a button per returned action with its localized label", async () => {
    render(<FileActionButtons {...PROPS} />);
    expect(await screen.findByRole("button", { name: "Транскрибировать" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Описать" })).toBeInTheDocument();
  });

  it("renders nothing when no buttons are returned", async () => {
    apiGet.mockResolvedValue({ buttons: [] });
    const { container } = render(<FileActionButtons {...PROPS} />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });

  it("click POSTs run with handler_id + params + session_id + agent", async () => {
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    fireEvent.click(btn);
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith(`/api/files/${UPLOAD_ID}/run`, {
        handler_id: "transcribe",
        params: { language: "ru" },
        session_id: "sess-1",
        agent: "main",
      }),
    );
  });

  it("shows a spinner on the clicked button while running", async () => {
    let resolveRun: (v: unknown) => void = () => {};
    apiPost.mockImplementation(() => new Promise((r) => { resolveRun = r; }));
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    fireEvent.click(btn);
    await waitFor(() => expect(btn).toBeDisabled());
    expect(btn.querySelector(".animate-spin")).not.toBeNull();
    resolveRun({});
    await waitFor(() => expect(btn).not.toBeDisabled());
  });
});
```

- [ ] **Step 2: Run test to verify it fails** — `cd ui && npm test -- FileActionButtons`. Expected failure: `Failed to resolve import "../FileActionButtons"` (the component file does not exist yet) → all 5 tests error.

- [ ] **Step 3: Write minimal implementation**
```tsx
// ui/src/app/(authenticated)/chat/composer/FileActionButtons.tsx
"use client";

import React, { useEffect, useState, useCallback } from "react";
import { apiGet, apiPost } from "@/lib/api";
import { useLanguageStore } from "@/stores/language-store";
import type { FileActionButton, FileActionsResponse } from "@/types/api";
import { Loader2, Mic, Image as ImageIcon, FileText, Save, Video, Wand2 } from "lucide-react";

interface FileActionButtonsProps {
  // upload ROW UUID (the `filename` returned by POST /api/media/upload), NOT a URL path.
  uploadId: string;
  mime: string;
  agent: string;
  sessionId: string | null;
}

// Small icon lookup keyed by the descriptor's <icon> string. Unknown → generic.
const ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  mic: Mic,
  image: ImageIcon,
  document: FileText,
  save: Save,
  video: Video,
};

function IconFor({ name }: { name: string }) {
  const Cmp = ICONS[name] ?? Wand2;
  return <Cmp className="h-3.5 w-3.5" />;
}

export function FileActionButtons({ uploadId, mime, agent, sessionId }: FileActionButtonsProps) {
  // locale drives the server-side label localization (re-fetch when it changes).
  const locale = useLanguageStore((s) => s.locale);
  const [buttons, setButtons] = useState<FileActionButton[]>([]);
  const [running, setRunning] = useState<string | null>(null);

  useEffect(() => {
    if (!uploadId) {
      setButtons([]);
      return;
    }
    let cancelled = false;
    const session = sessionId ?? "";
    const qs = `agent=${encodeURIComponent(agent)}&session=${encodeURIComponent(session)}`;
    apiGet<FileActionsResponse>(`/api/files/${encodeURIComponent(uploadId)}/actions?${qs}`)
      .then((resp) => {
        if (!cancelled) setButtons(resp.buttons ?? []);
      })
      .catch(() => {
        if (!cancelled) setButtons([]); // fail-soft: no buttons, file still attachable
      });
    return () => {
      cancelled = true;
    };
    // mime is included so the slot re-fetches if the same attachment id is reused
    // for a different file; locale re-fetches localized labels.
  }, [uploadId, agent, sessionId, mime, locale]);

  const run = useCallback(
    async (btn: FileActionButton) => {
      if (running) return;
      setRunning(btn.id);
      try {
        await apiPost(`/api/files/${encodeURIComponent(uploadId)}/run`, {
          handler_id: btn.id,
          params: btn.params,
          session_id: sessionId,
          agent,
        });
      } catch (err) {
        const { toast } = await import("sonner");
        toast.error(err instanceof Error ? err.message : "run failed");
      } finally {
        setRunning(null);
      }
    },
    [running, uploadId, sessionId, agent],
  );

  if (buttons.length === 0) return null;

  return (
    <div className="flex flex-wrap items-center gap-1.5 px-3 pt-2">
      {buttons.map((btn) => (
        <button
          key={btn.id}
          type="button"
          disabled={running !== null}
          onClick={() => run(btn)}
          className="inline-flex items-center gap-1.5 rounded-lg border border-border/60 bg-muted/30 px-2.5 py-1 text-xs font-medium text-foreground/80 hover:bg-muted/60 hover:text-foreground transition-colors disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {running === btn.id ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <IconFor name={btn.icon} />}
          {btn.label}
        </button>
      ))}
    </div>
  );
}
```

- [ ] **Step 4: Run test to verify it passes** — `cd ui && npm test -- FileActionButtons`. Expected: 5 tests pass (PASS) — fetch URL assertion (UUID path), per-button render, empty render, run POST body, spinner-disable lifecycle.

- [ ] **Step 5: Commit**
```bash
git add "ui/src/app/(authenticated)/chat/composer/FileActionButtons.tsx" "ui/src/app/(authenticated)/chat/composer/__tests__/FileActionButtons.test.tsx"
git commit -m "feat(ui): FileActionButtons — fetch/render per-file handler buttons, run on click with spinner"
```

---

### Task 3: Capture the upload row UUID in `handleFileAdd` (add `uploadId` to `AttachmentEntry`)

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx`
- Test: `ui/src/app/(authenticated)/chat/composer/__tests__/ChatComposer.upload-id.test.tsx`

**Interfaces:**
- Consumes: the existing `POST /api/media/upload` response `{ url, filename, size }` where `filename` is the upload ROW UUID (per R1); the existing `AttachmentEntry` shape `{ id, name, file, content[] }`.
- Produces: `AttachmentEntry.uploadId: string` set to `result.filename` (the UUID), consumed by the Task 4 integration. This is the dedicated fix for the critic's identifier finding — the actions/run round-trip MUST key off this UUID, not `new URL(result.url).pathname`.

- [ ] **Step 1: Write the failing test** — upload a file via the hidden input, then assert the attachment row carries the UUID (`filename`), not the served path. The attachment is read out of the chat store's `setAttachments`; this test spies on the `POST /api/media/upload` body via `global.fetch` and asserts `FileActionButtons` (mocked) receives `uploadId === filename`.
```tsx
// ui/src/app/(authenticated)/chat/composer/__tests__/ChatComposer.upload-id.test.tsx
import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/react";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "ru" }),
}));

vi.mock("@/lib/api", () => ({
  assertToken: () => "test-token",
}));

vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [] }),
}));

// Capture the props FileActionButtons is rendered with.
const fabSpy = vi.fn();
vi.mock("../FileActionButtons", () => ({
  FileActionButtons: (props: Record<string, unknown>) => {
    fabSpy(props);
    return <div data-testid="fab" data-upload={String(props.uploadId)} />;
  },
}));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { agents: ["main"], token: "test-token" };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", currentAgent: "main" }) },
  ),
}));

const chatState = {
  currentAgent: "main",
  agents: {
    main: {
      messageSource: { mode: "history", sessionId: "sess-9" },
      connectionPhase: "idle",
      pendingMessage: null,
    },
  },
};
const useChatStore: any = (selector?: (s: typeof chatState) => unknown) =>
  selector ? selector(chatState) : chatState;
useChatStore.getState = () => chatState;
vi.mock("@/stores/chat-store", () => ({
  useChatStore: (selector?: (s: typeof chatState) => unknown) => useChatStore(selector),
  isActivePhase: (p?: string) => p === "streaming" || p === "connecting",
}));

vi.mock("../../hooks/use-voice-recorder", () => ({
  useVoiceRecorder: () => ({ state: "idle", start: vi.fn(), stop: vi.fn(), elapsed: 0, level: 0 }),
}));

import { ChatComposer } from "../ChatComposer";

const UPLOAD_UUID = "abc-123-uuid";

describe("ChatComposer captures upload row UUID", () => {
  const realFetch = global.fetch;
  beforeEach(() => {
    vi.clearAllMocks();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      // url is a served path, filename is the row UUID (R1).
      json: async () => ({ url: "/uploads/something-else.ogg", filename: UPLOAD_UUID, size: 10 }),
    }) as unknown as typeof fetch;
  });
  afterEach(() => {
    global.fetch = realFetch;
  });

  it("passes uploadId = response.filename (the row UUID), not the URL path", async () => {
    const { container } = render(<ChatComposer />);
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "voice.ogg", { type: "audio/ogg" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    await waitFor(() => expect(screen.getByTestId("fab")).toBeInTheDocument());
    const props = fabSpy.mock.calls.at(-1)![0];
    expect(props.uploadId).toBe(UPLOAD_UUID);
    expect(String(props.uploadId)).not.toContain("/uploads/");
  });
});
```

- [ ] **Step 2: Run test to verify it fails** — `cd ui && npm test -- ChatComposer.upload-id`. Expected failure: `expected '/uploads/something-else.ogg' to be 'abc-123-uuid'` (or `expected undefined to be 'abc-123-uuid'`) — `ChatComposer` neither stores `result.filename` on the attachment nor renders `FileActionButtons` with it yet. (Task 4 wires the render; this task's red comes from the missing `uploadId` field — the render assertion is satisfied once both Task 3 + Task 4 land, so run this test again at the end of Task 4 to confirm green.)

- [ ] **Step 3: Write minimal implementation** — two edits to `ChatComposer.tsx`.

Edit 3a — extend the `AttachmentEntry` interface (find its declaration near the top of the file) with the new field:
```tsx
interface AttachmentEntry {
  id: string;
  name: string;
  file: File;
  uploadId: string; // upload ROW UUID (result.filename), used for /api/files/{uploadId}/...
  content: { type: string; data: string; mimeType: string; filename?: string }[];
}
```

Edit 3b — inside `handleFileAdd`, after the upload `result` is parsed and `uploadPath` is derived, set `uploadId` from `result.filename` when building the new attachment:
```tsx
      const uploadPath = new URL(result.url).pathname;
      setAttachments((prev) => [
        ...prev,
        {
          id: crypto.randomUUID(),
          name: file.name,
          file,
          uploadId: result.filename, // R1: the row UUID, distinct from the served URL path
          content: [{ type: "file", data: uploadPath, mimeType: file.type, filename: file.name }],
        },
      ]);
```

- [ ] **Step 4: Run test to verify it passes** — `cd ui && npm test -- ChatComposer.upload-id` (re-run after Task 4 wires the render). Expected after both this task and Task 4: 1 test passes — `props.uploadId === "abc-123-uuid"` and contains no `/uploads/`.

- [ ] **Step 5: Commit**
```bash
git add "ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx" "ui/src/app/(authenticated)/chat/composer/__tests__/ChatComposer.upload-id.test.tsx"
git commit -m "feat(ui): capture upload row UUID on AttachmentEntry for file handler actions"
```

---

### Task 4: Integrate `FileActionButtons` into `ChatComposer` per attachment, above the textarea

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx`
- Test: `ui/src/app/(authenticated)/chat/composer/__tests__/ChatComposer.file-actions.test.tsx`

**Interfaces:**
- Consumes: `FileActionButtons` (Task 2); the `AttachmentEntry` with `uploadId` (Task 3); `useChatStore((s)=>s.currentAgent)` and the active session id derived from the per-agent `messageSource`.
- Produces: one `<FileActionButtons>` per attachment, wired with `uploadId` (the row UUID from Task 3), `mime` (`content[0].mimeType`), `agent` (current agent), `sessionId` (active session). Per R1 the test asserts `uploadId` is the UUID, never `/uploads/...`.

- [ ] **Step 1: Write the failing test** — renders `ChatComposer`, simulates an upload via the hidden input, asserts a `FileActionButtons` mounts with `uploadId` = the row UUID + the other props. `FileActionButtons` is mocked so this test asserts integration wiring only.
```tsx
// ui/src/app/(authenticated)/chat/composer/__tests__/ChatComposer.file-actions.test.tsx
import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/react";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "ru" }),
}));

vi.mock("@/lib/api", () => ({
  assertToken: () => "test-token",
}));

vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [] }),
}));

// Capture the props FileActionButtons is rendered with.
const fabSpy = vi.fn();
vi.mock("../FileActionButtons", () => ({
  FileActionButtons: (props: Record<string, unknown>) => {
    fabSpy(props);
    return <div data-testid="fab" data-upload={String(props.uploadId)} data-mime={String(props.mime)} />;
  },
}));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { agents: ["main"], token: "test-token" };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", currentAgent: "main" }) },
  ),
}));

// Chat store: provide currentAgent + a per-agent slot with an active session id.
const chatState = {
  currentAgent: "main",
  agents: {
    main: {
      messageSource: { mode: "history", sessionId: "sess-9" },
      connectionPhase: "idle",
      pendingMessage: null,
    },
  },
};
const useChatStore: any = (selector?: (s: typeof chatState) => unknown) =>
  selector ? selector(chatState) : chatState;
useChatStore.getState = () => chatState;
vi.mock("@/stores/chat-store", () => ({
  useChatStore: (selector?: (s: typeof chatState) => unknown) => useChatStore(selector),
  isActivePhase: (p?: string) => p === "streaming" || p === "connecting",
}));

vi.mock("../../hooks/use-voice-recorder", () => ({
  useVoiceRecorder: () => ({ state: "idle", start: vi.fn(), stop: vi.fn(), elapsed: 0, level: 0 }),
}));

import { ChatComposer } from "../ChatComposer";

const UPLOAD_UUID = "abc-123-uuid";

describe("ChatComposer file action buttons", () => {
  const realFetch = global.fetch;
  beforeEach(() => {
    vi.clearAllMocks();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ url: "/uploads/served-path.ogg", filename: UPLOAD_UUID, size: 10 }),
    }) as unknown as typeof fetch;
  });
  afterEach(() => {
    global.fetch = realFetch;
  });

  it("renders FileActionButtons per attachment with uploadId(UUID) + mime + agent + session", async () => {
    const { container } = render(<ChatComposer />);
    // Simulate an uploaded attachment via the hidden file input.
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "voice.ogg", { type: "audio/ogg" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    await waitFor(() => expect(screen.getByTestId("fab")).toBeInTheDocument());
    const props = fabSpy.mock.calls.at(-1)![0];
    expect(props.uploadId).toBe(UPLOAD_UUID);
    expect(String(props.uploadId)).not.toContain("/uploads/");
    expect(props.mime).toBe("audio/ogg");
    expect(props.agent).toBe("main");
    expect(props.sessionId).toBe("sess-9");
  });
});
```

- [ ] **Step 2: Run test to verify it fails** — `cd ui && npm test -- ChatComposer.file-actions`. Expected failure: `Unable to find an element by: [data-testid="fab"]` — `ChatComposer` does not yet render `FileActionButtons`, so the spy is never called and no `fab` element appears.

- [ ] **Step 3: Write minimal implementation** — three edits to `ChatComposer.tsx` (Task 3 already added the `uploadId` field + capture).

Edit 3a — add the import alongside the other relative composer imports (after the `ModelDropdown` import line):
```tsx
import { ModelDropdown } from "./ModelDropdown";
import { FileActionButtons } from "./FileActionButtons";
```

Edit 3b — derive the active session id near the other `useChatStore` selectors at the top of `ChatComposer` (after the `messageSource` selector line):
```tsx
  const messageSource = useChatStore((s) => s.agents[s.currentAgent]?.messageSource ?? EMPTY_MESSAGE_SOURCE);
  const activeSessionId =
    messageSource.mode === "history" ? messageSource.sessionId : null;
```

Edit 3c — inside the existing `attachments.map((att) => ( ... ))` block, render `FileActionButtons` under the filename row. Wrap the attachment row + buttons in a `flex-col` container; the buttons appear above the textarea because the whole attachments block is rendered above it in the form:
```tsx
          {attachments.length > 0 && attachments.map((att) => (
            <div key={att.id} className="flex flex-col">
              <div className="flex items-center gap-2 px-3 pt-2 text-xs text-muted-foreground">
                <Paperclip className="h-3 w-3" />
                <span className="truncate max-w-[200px]">{att.name}</span>
                <button
                  type="button"
                  aria-label={t("chat.remove_attachment")}
                  onClick={() => setAttachments((prev) => prev.filter((a) => a.id !== att.id))}
                  className="rounded p-0.5 hover:bg-muted/50 text-muted-foreground/60 hover:text-muted-foreground transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <X size={12} />
                </button>
              </div>
              <FileActionButtons
                uploadId={att.uploadId}
                mime={att.content[0]?.mimeType ?? att.file.type}
                agent={currentAgent}
                sessionId={activeSessionId}
              />
            </div>
          ))}
```

- [ ] **Step 4: Run test to verify it passes** — `cd ui && npm test -- ChatComposer.file-actions`. Expected: 1 test passes (PASS) — `FileActionButtons` mounts with `uploadId="abc-123-uuid"` (UUID, no `/uploads/`), `mime="audio/ogg"`, `agent="main"`, `sessionId="sess-9"`. Then run the full sibling set `cd ui && npm test -- FileActionButtons file-action-types ChatComposer.upload-id ChatComposer.file-actions` to confirm no regressions (all PASS).

- [ ] **Step 5: Commit**
```bash
git add "ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx" "ui/src/app/(authenticated)/chat/composer/__tests__/ChatComposer.file-actions.test.tsx"
git commit -m "feat(ui): render per-attachment FileActionButtons above the composer input"
```

---

## Phase 5 — universal async queue

> Scope note (R2/R11): this phase introduces the durable `handler_jobs` queue, the toolgate out-of-process runner, the core async worker + callback endpoints, and ports `summarize_video` to a toolgate async builtin. In this phase we ALSO re-point the surviving auto-trigger (URL-video detection in `subagent.rs`) and the composer async run-branch (files.rs) off the legacy `video_jobs` queue onto `handler_jobs`, so Phase 6 can delete the legacy in-core video pipeline without losing the auto-trigger. The legacy `video_jobs` table/worker still exist after this phase (deleted/deprecated in Phase 6); nothing NEW is enqueued onto it once this phase lands.
>
> SSRF/loopback rule (R12, BLOCKING): toolgate `helpers.validate_url_ssrf()` hard-blocks loopback and `download_limited()` always calls it, so toolgate must NEVER fetch a loopback signed URL. CORE downloads the upload bytes over loopback (mirroring the existing `dispatch.rs::run_transcribe`) and POSTs them to toolgate as `multipart/form-data`. The async runner receives the bytes via a tempfile PATH written by toolgate — it does NO network fetch of the upload.

---

### Task 1: migration 067_handler_jobs.sql + HandlerJob DB module (state machine, R14 schema)
**Files:**
- Create: `migrations/067_handler_jobs.sql`
- Create: `crates/opex-core/src/db/handler_jobs.rs`
- Modify: `crates/opex-core/src/db/mod.rs`
- Test: `crates/opex-core/src/db/handler_jobs.rs` (inline `#[cfg(test)]`, `#[sqlx::test]`)

**Interfaces:**
- Consumes: existing migration runner (auto-runs `migrations/` on startup); `#[sqlx::test(migrations = "../../migrations")]` pattern.
- Produces (R14): `crate::db::handler_jobs::{HandlerJob, insert_handler_job, claim_next_handler_job, mark_handler_job_processing, mark_handler_job_done, mark_handler_job_failed, update_handler_job_progress, recover_stale_handler_jobs, get_handler_job}`. `insert_handler_job(db, upload_id:Option<Uuid>, source_ref:Option<&str>, handler_id:&str, agent_name:&str, session_id:Uuid, params:&serde_json::Value)->Result<Uuid>`. `HandlerJob` carries `upload_id:Option<Uuid>` AND `source_ref:Option<String>` (upload-based + url-based jobs). `HandlerJob::error()` reads `result.reason`. Consumed by Tasks 3, 4, 5 + subagent.rs re-point.

- [ ] **Step 1: Write the failing test** (state-machine + recover_stale + dual-source columns, inline in `handler_jobs.rs`)
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_then_claim_marks_processing(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let upload = uuid::Uuid::new_v4();
        let id = insert_handler_job(
            &pool,
            Some(upload),
            None,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({"language": "ru"}),
        )
        .await
        .unwrap();

        let claimed = claim_next_handler_job(&pool).await.unwrap().expect("a job");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "processing");
        assert_eq!(claimed.attempts, 1, "claim increments attempts");
        assert_eq!(claimed.handler_id, "summarize_video");
        assert_eq!(claimed.upload_id, Some(upload));
        assert_eq!(claimed.source_ref, None);

        // Only one queued row → second claim finds nothing.
        assert!(claim_next_handler_job(&pool).await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_url_job_carries_source_ref(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let id = insert_handler_job(
            &pool,
            None,
            Some("https://www.youtube.com/watch?v=abc"),
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({"language": "ru"}),
        )
        .await
        .unwrap();
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.upload_id, None);
        assert_eq!(row.source_ref.as_deref(), Some("https://www.youtube.com/watch?v=abc"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn progress_then_done_persists(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let id = insert_handler_job(&pool, None, None, "summarize_video", "Atlas", sid, &serde_json::json!({}))
            .await
            .unwrap();
        claim_next_handler_job(&pool).await.unwrap().unwrap();

        update_handler_job_progress(&pool, id, "digest", 42).await.unwrap();
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.phase.as_deref(), Some("digest"));
        assert_eq!(row.pct, Some(42));

        mark_handler_job_done(&pool, id, &serde_json::json!({"status": "ok", "summary_text": "x"}))
            .await
            .unwrap();
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "done");
        assert_eq!(row.result.as_ref().unwrap()["status"], "ok");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mark_failed_records_reason(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();
        let id = insert_handler_job(&pool, None, None, "summarize_video", "Atlas", sid, &serde_json::json!({}))
            .await
            .unwrap();
        claim_next_handler_job(&pool).await.unwrap().unwrap();

        mark_handler_job_failed(&pool, id, "boom").await.unwrap();
        let row = get_handler_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(row.error(), Some("boom"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recover_stale_resets_below_cap_and_fails_at_cap(pool: sqlx::PgPool) {
        let sid = uuid::Uuid::new_v4();

        // Row A: attempts=1, stuck processing → reset to queued.
        let a = insert_handler_job(&pool, None, None, "summarize_video", "Atlas", sid, &serde_json::json!({}))
            .await
            .unwrap();
        claim_next_handler_job(&pool).await.unwrap().unwrap(); // attempts=1, processing

        // Row B: force attempts=3, stuck processing → marked failed.
        let b = insert_handler_job(&pool, None, None, "summarize_video", "Atlas", sid, &serde_json::json!({}))
            .await
            .unwrap();
        sqlx::query("UPDATE handler_jobs SET status='processing', attempts=3 WHERE id=$1")
            .bind(b)
            .execute(&pool)
            .await
            .unwrap();

        let n = recover_stale_handler_jobs(&pool).await.unwrap();
        assert_eq!(n, 2, "both stuck rows touched");

        let ra = get_handler_job(&pool, a).await.unwrap().unwrap();
        assert_eq!(ra.status, "queued", "attempts<3 resets to queued");

        let rb = get_handler_job(&pool, b).await.unwrap().unwrap();
        assert_eq!(rb.status, "failed", "attempts>=3 marked failed");
        assert_eq!(rb.error(), Some("exceeded retry limit after crash"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `make test-db` (or with `DATABASE_URL` set: `cargo test -p opex-core handler_jobs -- --nocapture`). Expected failure: compile error `cannot find function insert_handler_job` / unresolved module `handler_jobs` (and at runtime the migration `067_handler_jobs.sql` does not yet exist).

- [ ] **Step 3: Write minimal implementation**

`migrations/067_handler_jobs.sql` (R14 — `upload_id` UUID NULL **and** `source_ref` TEXT NULL):
```sql
-- Universal durable async queue for File Handler Hub jobs.
-- Generalizes video_jobs (064/065): handler-agnostic, params/result are JSONB.
-- Carries BOTH upload-based (upload_id) and url-based (source_ref) sources so a
-- YouTube link and an attached video file both flow through the same queue.
CREATE TABLE handler_jobs (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    upload_id   UUID,
    source_ref  TEXT,
    handler_id  TEXT NOT NULL,
    agent_name  TEXT NOT NULL,
    session_id  UUID NOT NULL,
    params      JSONB NOT NULL DEFAULT '{}',
    status      TEXT NOT NULL DEFAULT 'queued'
                CHECK (status IN ('queued','processing','done','failed')),
    phase       TEXT,
    pct         INT,
    result      JSONB,
    attempts    INT  NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX handler_jobs_claim_idx ON handler_jobs (status, created_at);
```

`crates/opex-core/src/db/handler_jobs.rs`:
```rust
//! Universal durable queue for File Handler Hub async jobs. Generalizes
//! video_jobs — handler-agnostic (params/result are JSONB catch-alls) and
//! source-agnostic (upload_id for uploaded files, source_ref for external URLs).

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HandlerJob {
    pub id: Uuid,
    pub upload_id: Option<Uuid>,
    pub source_ref: Option<String>,
    pub handler_id: String,
    pub agent_name: String,
    pub session_id: Uuid,
    pub params: serde_json::Value,
    pub status: String,
    pub phase: Option<String>,
    pub pct: Option<i32>,
    pub result: Option<serde_json::Value>,
    pub attempts: i32,
}

impl HandlerJob {
    /// Convenience accessor for the failure reason recorded under `result.reason`.
    pub fn error(&self) -> Option<&str> {
        self.result.as_ref()?.get("reason")?.as_str()
    }
}

const COLS: &str = "id, upload_id, source_ref, handler_id, agent_name, session_id, \
                    params, status, phase, pct, result, attempts";

/// Enqueue a queued job. Exactly one of `upload_id` / `source_ref` is normally
/// set (upload-based vs url-based source). Returns the new id.
pub async fn insert_handler_job(
    db: &PgPool,
    upload_id: Option<Uuid>,
    source_ref: Option<&str>,
    handler_id: &str,
    agent_name: &str,
    session_id: Uuid,
    params: &serde_json::Value,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO handler_jobs \
             (upload_id, source_ref, handler_id, agent_name, session_id, params) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(upload_id)
    .bind(source_ref)
    .bind(handler_id)
    .bind(agent_name)
    .bind(session_id)
    .bind(params)
    .fetch_one(db)
    .await?;
    Ok(id)
}

/// Atomically claim the oldest queued job (queued → processing, +attempts).
pub async fn claim_next_handler_job(db: &PgPool) -> anyhow::Result<Option<HandlerJob>> {
    let job: Option<HandlerJob> = sqlx::query_as(&format!(
        "UPDATE handler_jobs SET status='processing', attempts=attempts+1, updated_at=now() \
         WHERE id = ( \
             SELECT id FROM handler_jobs WHERE status='queued' \
             ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED \
         ) RETURNING {COLS}"
    ))
    .fetch_optional(db)
    .await?;
    Ok(job)
}

pub async fn mark_handler_job_processing(db: &PgPool, id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE handler_jobs SET status='processing', updated_at=now() WHERE id=$1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn update_handler_job_progress(
    db: &PgPool,
    id: Uuid,
    phase: &str,
    pct: i32,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE handler_jobs SET phase=$2, pct=$3, updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(phase)
        .bind(pct)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn mark_handler_job_done(
    db: &PgPool,
    id: Uuid,
    result: &serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE handler_jobs SET status='done', result=$2, updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(result)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn mark_handler_job_failed(db: &PgPool, id: Uuid, error: &str) -> anyhow::Result<()> {
    // Store the error string under result.reason so the wire shape stays uniform
    // with ScenarioOutcome ({status, reason}); HandlerJob::error() reads it back.
    let result = serde_json::json!({ "status": "failed", "reason": error });
    sqlx::query("UPDATE handler_jobs SET status='failed', result=$2, updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(result)
        .execute(db)
        .await?;
    Ok(())
}

/// Reset rows stuck in 'processing' (crash recovery). Jobs attempted 3+ times
/// are marked failed instead of retried (mirrors video_jobs).
pub async fn recover_stale_handler_jobs(db: &PgPool) -> anyhow::Result<u64> {
    let res = sqlx::query(
        "UPDATE handler_jobs \
         SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'queued' END, \
             result = CASE WHEN attempts >= 3 \
                          THEN jsonb_build_object('status','failed','reason','exceeded retry limit after crash') \
                          ELSE result END, \
             updated_at = now() \
         WHERE status = 'processing'",
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

pub async fn get_handler_job(db: &PgPool, id: Uuid) -> anyhow::Result<Option<HandlerJob>> {
    let job: Option<HandlerJob> =
        sqlx::query_as(&format!("SELECT {COLS} FROM handler_jobs WHERE id=$1"))
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(job)
}
```

Register the module in `crates/opex-core/src/db/mod.rs`:
```rust
pub mod handler_jobs;
```

- [ ] **Step 4: Run test to verify it passes** — `make test-db` (or `cargo test -p opex-core handler_jobs`). Expected: `insert_then_claim_marks_processing`, `insert_url_job_carries_source_ref`, `progress_then_done_persists`, `mark_failed_records_reason`, `recover_stale_resets_below_cap_and_fails_at_cap` all PASS.

- [ ] **Step 5: Commit**
```bash
git add migrations/067_handler_jobs.sql crates/opex-core/src/db/handler_jobs.rs crates/opex-core/src/db/mod.rs
git commit -m "feat(file-hub): add handler_jobs durable queue (migration 067 + db module, dual upload/url source)"
```

---

### Task 2: toolgate async path — extend Phase-2 `run_handler` (202 + tempfile + spawn runner.py)
**Files:**
- Modify: `toolgate/handlers/router.py`
- Create: `toolgate/handlers/runner.py`
- Modify: `toolgate/handlers/context.py` (add `HandlerResult.to_dict()` if absent)
- Test: `toolgate/tests/test_handlers_async.py`

**Interfaces:**
- Consumes (Phase 2, R12): the SAME `async def run_handler(handler_id, request)` route handler added in Phase 2 — it parses MULTIPART form-data (`file` upload bytes optional, plus `mime`, `filename`, `params` JSON string, `language`, `job_id` optional, `source_url` optional), reads `request.app.state.handlers`, branches on `loaded.descriptor.execution`; `HandlerRegistry.get(id) -> LoadedHandler|None`; `build_context(registry, http_client, job_id=None, core_url=None, auth_token=None) -> HandlerContext`; `HandlerFile{bytes, mime, filename, size, signed_url, source_url}`; `HandlerResult` (wire `{status, summary_text, artifact_urls, reason}`).
- Produces (R10 + R12): `POST /handlers/{id}/run` returns `202 {"accepted": true, "job_id": ...}` for `execution=async` — the async branch lives IN-PLACE in the Phase-2 `run_handler` (no new/renamed fn, no `_run_sync`). For upload-based async jobs toolgate writes the multipart bytes to a `tempfile.NamedTemporaryFile(delete=False)` and spawns the runner with that temp PATH (NO loopback URL). `toolgate/handlers/runner.py` CLI entrypoint posts `POST {core_url}/api/files/jobs/{job_id}/progress` + `.../complete`, reading bytes from the temp path (no network fetch) and deleting it in `finally`. Consumed by Task 3 (core callbacks) + Task 4 (core worker).

- [ ] **Step 1: Write the failing test** (`toolgate/tests/test_handlers_async.py`)
```python
"""Async handler path (R12): the Phase-2 run_handler returns 202 + spawns the
runner out-of-process from a tempfile PATH (no loopback fetch); the runner posts
progress + complete callbacks to core, reading bytes from the temp path."""
import json
import sys
from pathlib import Path

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from handlers.loader import HandlerRegistry  # noqa: E402
from handlers import runner as runner_mod  # noqa: E402


class _UploadFile:
    """Minimal stand-in for fastapi.UploadFile."""
    def __init__(self, data: bytes):
        self._data = data

    async def read(self) -> bytes:
        return self._data


class _FakeRequest:
    """Minimal stand-in for fastapi.Request exposing app.state.handlers."""
    def __init__(self, registry):
        self.app = type("A", (), {"state": type("S", (), {"handlers": registry})()})()


@pytest.mark.asyncio
async def test_async_handler_run_returns_202_and_spawns_runner_with_tempfile(monkeypatch, tmp_path):
    """An execution=async handler must NOT run inline — run_handler writes the
    upload bytes to a tempfile, returns 202, and spawns the runner with the PATH."""
    from handlers import router as router_mod

    reg = HandlerRegistry()
    reg.load_all(
        builtin_dir=str(Path(__file__).resolve().parents[1] / "handlers" / "builtin"),
        workspace_dir=None,
    )
    # summarize_video is the only async builtin (Task 5).
    assert reg.get("summarize_video") is not None
    assert reg.get("summarize_video").descriptor.execution == "async"

    spawned = {}

    async def fake_exec(*args, **kwargs):
        spawned["argv"] = args

        class _Proc:
            pid = 4242
        return _Proc()

    monkeypatch.setattr(router_mod.asyncio, "create_subprocess_exec", fake_exec)

    resp = await router_mod.run_handler(
        "summarize_video",
        _FakeRequest(reg),
        file=_UploadFile(b"VIDEOBYTES"),
        mime="video/mp4",
        filename="v.mp4",
        params="{}",
        language="ru",
        job_id="job-123",
        source_url=None,
    )
    assert resp.status_code == 202
    payload = json.loads(bytes(resp.body))
    assert payload == {"accepted": True, "job_id": "job-123"}

    argv = " ".join(str(a) for a in spawned["argv"])
    assert "runner" in argv
    # The spawned spec must reference a real temp path holding the bytes (NOT a URL).
    spec_arg = spawned["argv"][-1]
    spec = json.loads(spec_arg)
    assert spec["job_id"] == "job-123"
    assert spec["temp_path"]
    assert Path(spec["temp_path"]).read_bytes() == b"VIDEOBYTES"
    Path(spec["temp_path"]).unlink(missing_ok=True)


@pytest.mark.asyncio
async def test_runner_reads_tempfile_then_posts_progress_and_complete(monkeypatch, tmp_path):
    """The runner reads bytes from the temp path (NO network fetch), runs the
    handler, posts progress + a final ScenarioOutcome (4-key wire), and deletes
    the temp file afterwards."""
    posts = []

    class FakeAsyncClient:
        def __init__(self, *a, **k):
            pass
        async def __aenter__(self):
            return self
        async def __aexit__(self, *a):
            return False
        async def post(self, url, json=None, headers=None, **k):
            posts.append((url, json))
            return httpx.Response(200, request=httpx.Request("POST", url))
        async def aclose(self):
            pass

    monkeypatch.setattr(runner_mod.httpx, "AsyncClient", FakeAsyncClient)

    async def fake_run(ctx, file, params):
        await ctx.progress("digest", 50)
        return ctx.result.text("итоговый конспект")

    class FakeLoaded:
        class descriptor:
            execution = "async"
        run = staticmethod(fake_run)

    class FakeReg:
        def load_all(self, **k):
            pass
        def get(self, _id):
            return FakeLoaded()

    class FakeResultBuilder:
        def text(self, s):
            class _R:
                def to_dict(self_inner):
                    return {"status": "ok", "summary_text": s,
                            "artifact_urls": [], "reason": None}
            return _R()

    class FakeCtx:
        def __init__(self):
            self.result = FakeResultBuilder()
        async def progress(self, phase, pct):
            pass

    monkeypatch.setattr(runner_mod, "_load_registry", lambda http: FakeReg())
    monkeypatch.setattr(runner_mod, "build_context", lambda *a, **k: FakeCtx())

    temp = tmp_path / "upload.bin"
    temp.write_bytes(b"FAKEBYTES")

    spec = {
        "handler_id": "summarize_video",
        "temp_path": str(temp),
        "source_url": None,
        "mime": "video/mp4",
        "filename": "v.mp4",
        "params": {},
        "language": "ru",
        "job_id": "job-123",
        "core_url": "http://127.0.0.1:18789",
        "auth_token": "tok",
    }
    await runner_mod.run_job(spec)

    urls = [u for u, _ in posts]
    assert any(u.endswith("/api/files/jobs/job-123/progress") for u in urls), urls
    assert any(u.endswith("/api/files/jobs/job-123/complete") for u in urls), urls
    complete = next(b for u, b in posts if u.endswith("/complete"))
    assert complete == {"status": "ok", "summary_text": "итоговый конспект",
                        "artifact_urls": [], "reason": None}
    # Temp file deleted by the runner's finally.
    assert not temp.exists(), "runner must delete the temp file"
```

- [ ] **Step 2: Run test to verify it fails** — `cd toolgate && pytest tests/test_handlers_async.py -q`. Expected failure: `ModuleNotFoundError: handlers.runner` (and the async branch / 202 path + tempfile spawn not yet present in `run_handler`).

- [ ] **Step 3: Write minimal implementation**

In `toolgate/handlers/router.py`, extend the EXISTING Phase-2 `run_handler` in place by adding the `execution == "async"` branch at the top (R10 — no `_run_sync`, no 3-arg `registry` shim; the Phase-2 sync logic stays inline below). The function takes multipart form params (R12 — `file` bytes, `mime`, `filename`, `params` JSON string, `language`, `job_id`, `source_url`). Ensure the imports + runner path constant are present:
```python
import asyncio
import json
import os
import sys
import tempfile
from pathlib import Path

from fastapi import File, Form, Request, UploadFile
from fastapi.responses import JSONResponse

_RUNNER_PATH = str(Path(__file__).resolve().parent / "runner.py")


@router.post("/handlers/{handler_id}/run")
async def run_handler(
    handler_id: str,
    request: Request,
    file: UploadFile | None = File(default=None),
    mime: str = Form(default=""),
    filename: str = Form(default="file"),
    params: str = Form(default="{}"),
    language: str = Form(default="ru"),
    job_id: str | None = Form(default=None),
    source_url: str | None = Form(default=None),
):
    """Phase-2 route handler, extended in Phase 5 with the async branch.

    R12: NO loopback signed_url is fetched here. Sync handlers run on the raw
    multipart bytes; async handlers persist the bytes to a tempfile and the
    out-of-process runner reads that PATH (or fetches an EXTERNAL source_url).
    """
    registry = request.app.state.handlers
    loaded = registry.get(handler_id)
    if loaded is None:
        return JSONResponse(status_code=404, content={"error": f"unknown handler {handler_id}"})

    parsed_params = json.loads(params) if params else {}
    upload_bytes = await file.read() if file is not None else b""

    if loaded.descriptor.execution == "async":
        # Persist upload bytes to a tempfile so the out-of-process runner reads
        # the PATH (R12: never a loopback URL). url-based async (video) passes
        # source_url instead and writes no tempfile.
        temp_path = None
        if upload_bytes:
            tf = tempfile.NamedTemporaryFile(prefix="opex-handler-", delete=False)
            tf.write(upload_bytes)
            tf.close()
            temp_path = tf.name
        spec = {
            "handler_id": handler_id,
            "temp_path": temp_path,
            "source_url": source_url,
            "mime": mime,
            "filename": filename,
            "params": parsed_params,
            "language": language,
            "job_id": job_id,
            "core_url": os.environ.get("CORE_API_URL", "http://localhost:18789"),
            "auth_token": os.environ.get("OPEX_AUTH_TOKEN", ""),
        }
        await asyncio.create_subprocess_exec(
            sys.executable, "-m", "handlers.runner", json.dumps(spec)
        )
        return JSONResponse(status_code=202, content={"accepted": True, "job_id": job_id})

    # ── sync path (Phase 2, inline) ──────────────────────────────────────────
    # ... existing Phase-2 sync logic: build HandlerFile from upload_bytes,
    #     outcome = await asyncio.wait_for(loaded.run(ctx, file_obj, parsed_params),
    #                                       timeout=HANDLER_SYNC_TIMEOUT_SECS),
    #     return JSONResponse(content=outcome.to_dict())  (timeout -> status="timeout")
```
(Phase 2 already registered the route; in Phase 5 the signature gains `job_id`/`source_url` form fields and the async branch — the route decorator stays a single `@router.post("/handlers/{handler_id}/run")`.)

`toolgate/handlers/runner.py`:
```python
"""Out-of-process handler-runner (R12). Launched per async job by router.py via
`python -m handlers.runner '<json spec>'`.

Reads the JSON job spec from argv[1] (or stdin), rebuilds the registry + ctx,
loads the file bytes FROM THE LOCAL TEMP PATH (never a loopback fetch) or sets
source_url for url-based handlers, runs the handler, and posts progress + the
final ScenarioOutcome to the core callbacks. Deletes the temp file when done.
"""
import asyncio
import json
import logging
import os
import sys
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from handlers.loader import HandlerRegistry
from handlers.context import build_context, HandlerFile

log = logging.getLogger("toolgate.runner")

_BUILTIN_DIR = str(Path(__file__).resolve().parent / "builtin")


def _load_registry(http) -> HandlerRegistry:
    reg = HandlerRegistry()
    reg.load_all(builtin_dir=_BUILTIN_DIR, workspace_dir=None)
    return reg


def _outcome_dict(outcome) -> dict:
    if hasattr(outcome, "to_dict"):
        return outcome.to_dict()
    if isinstance(outcome, dict):
        return outcome
    return {
        "status": getattr(outcome, "status", "failed"),
        "summary_text": getattr(outcome, "summary_text", ""),
        "artifact_urls": getattr(outcome, "artifact_urls", []),
        "reason": getattr(outcome, "reason", None),
    }


async def run_job(spec: dict) -> None:
    job_id = spec["job_id"]
    core_url = spec["core_url"].rstrip("/")
    auth = spec.get("auth_token", "")
    headers = {"Authorization": f"Bearer {auth}"} if auth else {}
    temp_path = spec.get("temp_path")

    try:
        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10.0, read=None, write=10.0, pool=120.0)
        ) as http:
            registry = _load_registry(http)
            loaded = registry.get(spec["handler_id"])
            if loaded is None:
                await http.post(
                    f"{core_url}/api/files/jobs/{job_id}/complete",
                    headers=headers,
                    json={"status": "failed", "summary_text": "",
                          "artifact_urls": [], "reason": f"unknown handler {spec['handler_id']}"},
                )
                return

            async def progress_cb(phase: str, pct: int) -> None:
                try:
                    await http.post(
                        f"{core_url}/api/files/jobs/{job_id}/progress",
                        headers=headers,
                        json={"phase": phase, "pct": pct},
                    )
                except Exception as e:  # progress is best-effort
                    log.warning("progress post failed: %s", e)

            ctx = build_context(
                registry, http, job_id=job_id, core_url=core_url, auth_token=auth
            )
            ctx.progress = progress_cb  # bind the live callback for this job

            # R12: read bytes from the local temp path (NO network fetch). For
            # url-based handlers (video) the bytes are empty and source_url drives it.
            data = b""
            if temp_path and os.path.exists(temp_path):
                data = Path(temp_path).read_bytes()

            file = HandlerFile(
                bytes=data,
                mime=spec.get("mime") or "application/octet-stream",
                filename=spec.get("filename", "file"),
                size=len(data),
                signed_url="",
                source_url=spec.get("source_url"),
            )

            try:
                outcome = await loaded.run(ctx, file, spec.get("params", {}))
                payload = _outcome_dict(outcome)
            except Exception as e:
                log.exception("handler run failed")
                payload = {"status": "failed", "summary_text": "",
                           "artifact_urls": [], "reason": str(e)}

            await http.post(
                f"{core_url}/api/files/jobs/{job_id}/complete",
                headers=headers,
                json=payload,
            )
    finally:
        if temp_path and os.path.exists(temp_path):
            try:
                os.unlink(temp_path)
            except OSError as e:
                log.warning("temp cleanup failed: %s", e)


def main() -> None:
    raw = sys.argv[1] if len(sys.argv) > 1 else sys.stdin.read()
    asyncio.run(run_job(json.loads(raw)))


if __name__ == "__main__":
    main()
```

If `HandlerResult` from Phase 1 does not already expose `.to_dict()`, add it in `toolgate/handlers/context.py` (4-key wire shape — R9 asymmetry is benign; toolgate emits 4 keys, core deserializes them with `video_accepted` defaulting false). Also give `HandlerFile` a `source_url` field (R12):
```python
    def to_dict(self) -> dict:
        return {
            "status": self.status,
            "summary_text": self.summary_text,
            "artifact_urls": self.artifact_urls,
            "reason": self.reason,
        }
```

- [ ] **Step 4: Run test to verify it passes** — `cd toolgate && pytest tests/test_handlers_async.py -q`. Expected: `test_async_handler_run_returns_202_and_spawns_runner_with_tempfile` and `test_runner_reads_tempfile_then_posts_progress_and_complete` PASS (`2 passed`).

- [ ] **Step 5: Commit**
```bash
git add toolgate/handlers/router.py toolgate/handlers/runner.py toolgate/handlers/context.py toolgate/tests/test_handlers_async.py
git commit -m "feat(file-hub): toolgate async path — 202 + tempfile-backed out-of-process runner (no loopback fetch)"
```

---

### Task 3: core callback endpoints in files.rs (progress + complete) → WS file_job_progress + deliver + post_action (R17 MCP)
**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/files.rs`
- Test: `crates/opex-core/src/gateway/handlers/files.rs` (inline `#[cfg(test)]`, pure helper test)

**Interfaces:**
- Consumes: `crate::db::handler_jobs::{get_handler_job, update_handler_job_progress, mark_handler_job_done, mark_handler_job_failed, HandlerJob}` (Task 1); `crate::agent::file_scenario::outcome::{ScenarioOutcome, ScenarioStatus}` (grounding wire type); `crate::agent::provenance::wrap_file_output` (Phase 3); `AppState{infra.db, channels.ui_event_tx, agents}`; MCP plumbing (R17 — CONFIRMED real): `engine.mcp() -> &Option<Arc<crate::mcp::McpRegistry>>` (`agent/engine/mod.rs:241`), `McpRegistry::call_tool(&self, mcp_name:&str, tool_name:&str, arguments:&serde_json::Value) -> anyhow::Result<String>` (`mcp/mod.rs:212`), `state.agents.get_engine(&str) -> Option<Arc<AgentEngine>>` (`gateway/clusters/agent_core.rs:48`).
- Produces: `POST /api/files/jobs/{job_id}/progress` (`JobProgressBody{phase, pct}`) and `POST /api/files/jobs/{job_id}/complete` (`ScenarioOutcome` JSON) routes merged into `files::routes()`; pure `fn file_job_progress_event(...) -> serde_json::Value` (tested). Persists the async outcome as a `source='file_handler'` message with provenance wrapping (R4/R8). Consumed by Task 4 worker + Task 6 UI.

- [ ] **Step 1: Write the failing test** (the WS-event-shape helper, inline in `files.rs`)
```rust
#[cfg(test)]
mod async_callback_tests {
    use super::*;

    #[test]
    fn file_job_progress_event_has_generic_shape() {
        let ev = file_job_progress_event(
            "job-1",
            "summarize_video",
            "sess-9",
            "digest",
            42,
            "processing",
        );
        assert_eq!(ev["type"], "file_job_progress");
        assert_eq!(ev["job_id"], "job-1");
        assert_eq!(ev["handler_id"], "summarize_video");
        assert_eq!(ev["session_id"], "sess-9");
        assert_eq!(ev["phase"], "digest");
        assert_eq!(ev["pct"], 42);
        assert_eq!(ev["status"], "processing");
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core file_job_progress_event`. Expected failure: `cannot find function file_job_progress_event in this scope`.

- [ ] **Step 3: Write minimal implementation** — add to `files.rs`:
```rust
use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;

use crate::agent::file_scenario::outcome::{ScenarioOutcome, ScenarioStatus};
use crate::db::handler_jobs;
use crate::gateway::AppState;

#[derive(Debug, Deserialize)]
pub(crate) struct JobProgressBody {
    pub phase: String,
    pub pct: i32,
}

/// Generic WS event broadcast on every async-job progress/terminal step.
/// Generalization of `video_progress` (the queue is handler-agnostic).
pub(crate) fn file_job_progress_event(
    job_id: &str,
    handler_id: &str,
    session_id: &str,
    phase: &str,
    pct: i32,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "file_job_progress",
        "job_id": job_id,
        "handler_id": handler_id,
        "session_id": session_id,
        "phase": phase,
        "pct": pct,
        "status": status,
    })
}

/// Internal callback: runner reports incremental progress.
pub(crate) async fn job_progress(
    State(state): State<AppState>,
    AxPath(job_id): AxPath<uuid::Uuid>,
    Json(body): Json<JobProgressBody>,
) -> axum::http::StatusCode {
    let db = &state.infra.db;
    if let Err(e) =
        handler_jobs::update_handler_job_progress(db, job_id, &body.phase, body.pct).await
    {
        tracing::warn!(error = %e, %job_id, "job_progress: db update failed");
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
    }
    if let Ok(Some(job)) = handler_jobs::get_handler_job(db, job_id).await {
        let ev = file_job_progress_event(
            &job_id.to_string(),
            &job.handler_id,
            &job.session_id.to_string(),
            &body.phase,
            body.pct,
            "processing",
        );
        let _ = state.channels.ui_event_tx.send(ev.to_string());
    }
    axum::http::StatusCode::NO_CONTENT
}

/// Internal callback: runner reports the final ScenarioOutcome.
pub(crate) async fn job_complete(
    State(state): State<AppState>,
    AxPath(job_id): AxPath<uuid::Uuid>,
    Json(outcome): Json<ScenarioOutcome>,
) -> axum::http::StatusCode {
    let db = &state.infra.db;
    let job = match handler_jobs::get_handler_job(db, job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return axum::http::StatusCode::NOT_FOUND,
        Err(e) => {
            tracing::warn!(error = %e, %job_id, "job_complete: load failed");
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let is_ok = matches!(outcome.status, ScenarioStatus::Ok);
    let terminal = if is_ok { "done" } else { "failed" };

    let result_json = serde_json::to_value(&outcome).unwrap_or_else(|_| serde_json::json!({}));
    if is_ok {
        let _ = handler_jobs::mark_handler_job_done(db, job_id, &result_json).await;
        // Persist as a file-derived message + run the generic post-action.
        deliver_async_outcome(&state, &job, &outcome).await;
    } else {
        let reason = outcome
            .reason
            .clone()
            .unwrap_or_else(|| "handler failed".to_string());
        let _ = handler_jobs::mark_handler_job_failed(db, job_id, &reason).await;
    }

    let ev = file_job_progress_event(
        &job_id.to_string(),
        &job.handler_id,
        &job.session_id.to_string(),
        "done",
        100,
        terminal,
    );
    let _ = state.channels.ui_event_tx.send(ev.to_string());
    axum::http::StatusCode::NO_CONTENT
}

/// Persist the async outcome as a file-derived assistant message (R4/R8:
/// provenance-wrapped content, source='file_handler', no explicit status) and
/// run the generic post-completion action (MCP/Obsidian vault write).
async fn deliver_async_outcome(
    state: &AppState,
    job: &handler_jobs::HandlerJob,
    outcome: &ScenarioOutcome,
) {
    // 1. Provenance-wrap with the REAL handler_id + upload_id (R4). url-based
    //    jobs (no upload) carry an empty upload id in the wrapper.
    let upload_id = job
        .upload_id
        .map(|u| u.to_string())
        .unwrap_or_default();
    let content =
        crate::agent::provenance::wrap_file_output(&job.handler_id, &upload_id, &outcome.summary_text);

    // 2. Persist (R8: omit status → table default 'complete'; source='file_handler').
    if let Err(e) = sqlx::query(
        "INSERT INTO messages (session_id, agent_id, role, content, is_mirror, source) \
         VALUES ($1, $2, 'assistant', $3, true, 'file_handler')",
    )
    .bind(job.session_id)
    .bind(&job.agent_name)
    .bind(&content)
    .execute(&state.infra.db)
    .await
    {
        tracing::error!(error = %e, job_id = %job.id, "deliver_async_outcome: persist failed");
    }

    // 3. Generic post-action: the handler may request an MCP vault write via a
    //    `post_action` object in the result JSON (KEEP the Obsidian write core-bound).
    run_post_action(state, job, outcome).await;

    // 4. Live push so open tabs render without a reload (mirrors video deliver()).
    let ev = serde_json::json!({
        "type": "video_summary_ready",
        "session_id": job.session_id.to_string(),
        "text": outcome.summary_text,
    });
    let _ = state.channels.ui_event_tx.send(ev.to_string());
}

/// Run the optional `post_action` carried in the outcome JSON. v1 supports the
/// Obsidian vault note write (`post_action.kind == "obsidian_note"`), reusing
/// the existing core MCP plumbing (R17). Any other kind is ignored.
async fn run_post_action(
    state: &AppState,
    job: &handler_jobs::HandlerJob,
    outcome: &ScenarioOutcome,
) {
    // mark_handler_job_done stored result_json before this runs, so re-read the
    // row to pick up the `post_action` catch-all from the runner outcome.
    let row = match handler_jobs::get_handler_job(&state.infra.db, job.id).await {
        Ok(Some(r)) => r,
        _ => return,
    };
    let action = match row.result.as_ref().and_then(|r| r.get("post_action")) {
        Some(a) => a.clone(),
        None => return,
    };
    if action.get("kind").and_then(|k| k.as_str()) != Some("obsidian_note") {
        return;
    }
    let engine = match state.agents.get_engine(&job.agent_name).await {
        Some(e) => e,
        None => return,
    };
    // R17: engine.mcp() -> &Option<Arc<McpRegistry>>.
    let mcp = match engine.mcp() {
        Some(m) => m.clone(),
        None => {
            tracing::warn!(job_id = %job.id, "run_post_action: MCP disabled — skipping vault write");
            return;
        }
    };
    let folder = action.get("folder").and_then(|v| v.as_str()).unwrap_or("Summary");
    let filename = action.get("filename").and_then(|v| v.as_str()).unwrap_or("note.md");
    let content = action
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or(&outcome.summary_text);
    // R17: McpRegistry::call_tool(&self, mcp_name, tool_name, &serde_json::Value).
    if let Err(e) = mcp
        .call_tool(
            "mcp-obsidian",
            "create_note",
            &serde_json::json!({ "folder": folder, "filename": filename, "content": content }),
        )
        .await
    {
        tracing::warn!(error = %e, job_id = %job.id, "run_post_action: create_note failed");
    } else if let Err(e) = mcp
        .call_tool(
            "mcp-obsidian",
            "commit_vault",
            &serde_json::json!({ "message": format!("file-handler note: {filename}") }),
        )
        .await
    {
        tracing::warn!(error = %e, job_id = %job.id, "run_post_action: commit_vault failed");
    }
}
```
Register the two internal routes inside the existing `pub(crate) fn routes()` in `files.rs`:
```rust
        .route("/api/files/jobs/{job_id}/progress", axum::routing::post(job_progress))
        .route("/api/files/jobs/{job_id}/complete", axum::routing::post(job_complete))
```
Note: the result-JSON re-read in `run_post_action` happens after `mark_handler_job_done` has stored `result_json` (`job_complete` calls `mark_handler_job_done` before `deliver_async_outcome`, so `post_action` is present in the row). The MCP accessors above are the CONFIRMED real symbols (R17) — `engine.mcp()`, `McpRegistry::call_tool`, `state.agents.get_engine`. If MCP is unavailable, `run_post_action` is a safe no-op.

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core file_job_progress_event` then `make check`. Expected: `file_job_progress_event_has_generic_shape` PASSES and the crate compiles.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/gateway/handlers/files.rs
git commit -m "feat(file-hub): core async callbacks — job progress/complete endpoints + provenance persist + post_action MCP write"
```

---

### Task 4: amend files.rs POST /run async branch — insert handler_jobs (R13/R15 surviving seam)
**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/files.rs`
- Test: `crates/opex-core/src/gateway/handlers/files.rs` (inline `#[cfg(test)]`, `#[sqlx::test]` enqueue test)

**Interfaces:**
- Consumes: `crate::db::handler_jobs::insert_handler_job` (Task 1); `R3` `assert_upload_accessible` + `UploadMeta` (Phase 3, already in `files.rs`); the Phase-3 `FileRunRequest{handler_id, params, session_id, agent}` + the 202-stub async branch in `POST /api/files/{upload_id}/run`; `crate::agent::handler_registry::HandlerManifest.execution` (Phase 3 `AppState.handlers` registry) to tell sync from async.
- Produces: the async branch now calls `insert_handler_job(db, Some(upload_id), None, &handler_id, &agent, session_id, &params)` and returns `202 {"accepted": true, "job_id": <uuid>}` (R13 — this is the surface Phase 6 keeps; the dispatch.rs arm is NOT edited). A testable seam `async fn enqueue_async_run(db, upload_id:Uuid, handler_id:&str, agent:&str, session_id:Uuid, params:&serde_json::Value) -> anyhow::Result<uuid::Uuid>`.

> This task fixes the critic's coverage gap: Phase 3 left the async branch returning a literal stub with NO enqueue. Phase 5 wires the real `insert_handler_job` into THIS branch (the one Phase 6 keeps), NOT into `dispatch.rs::run_summarize_video` (which Phase 6 deletes).

- [ ] **Step 1: Write the failing test** (enqueue seam, inline in `files.rs`)
```rust
#[cfg(test)]
mod async_enqueue_tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn enqueue_async_run_inserts_queued_handler_job(pool: sqlx::PgPool) {
        let upload = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        let job_id = enqueue_async_run(
            &pool,
            upload,
            "summarize_video",
            "Atlas",
            sid,
            &serde_json::json!({ "language": "ru" }),
        )
        .await
        .unwrap();

        let row = crate::db::handler_jobs::get_handler_job(&pool, job_id)
            .await
            .unwrap()
            .expect("job exists");
        assert_eq!(row.status, "queued");
        assert_eq!(row.handler_id, "summarize_video");
        assert_eq!(row.upload_id, Some(upload));
        assert_eq!(row.session_id, sid);
        assert_eq!(row.params["language"], "ru");
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core enqueue_async_run` (or `make test-db`). Expected failure: `cannot find function enqueue_async_run in this scope`.

- [ ] **Step 3: Write minimal implementation** — add the seam to `files.rs` and call it from the existing async branch of `POST /api/files/{upload_id}/run`:
```rust
/// Enqueue an async handler run onto the universal `handler_jobs` queue (R13).
/// Returns the new job id. Upload-based source → `Some(upload_id)`, `source_ref=None`.
pub(crate) async fn enqueue_async_run(
    db: &sqlx::PgPool,
    upload_id: uuid::Uuid,
    handler_id: &str,
    agent: &str,
    session_id: uuid::Uuid,
    params: &serde_json::Value,
) -> anyhow::Result<uuid::Uuid> {
    crate::db::handler_jobs::insert_handler_job(
        db,
        Some(upload_id),
        None,
        handler_id,
        agent,
        session_id,
        params,
    )
    .await
}
```
Then replace the Phase-3 async 202-stub in the `file_run` handler. Where Phase 3 returned the placeholder ack, insert the real enqueue (the `upload_id`, `req.handler_id`, `req.agent`, `req.session_id`, `req.params` bindings are already in scope from the Phase-3 handler; `state.infra.db` is the pool):
```rust
    // execution == "async": enqueue onto handler_jobs (R13) — file_handler_worker
    // (Task 5) dispatches it to toolgate; the runner posts back via the callbacks.
    let job_id = match enqueue_async_run(
        &state.infra.db,
        upload_id,
        &req.handler_id,
        &req.agent,
        req.session_id,
        &req.params,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "file_run: async enqueue failed");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to enqueue job" })),
            )
                .into_response();
        }
    };
    return (
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true, "job_id": job_id.to_string() })),
    )
        .into_response();
```

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core enqueue_async_run` (or `make test-db`) then `make check`. Expected: `enqueue_async_run_inserts_queued_handler_job` PASSES; crate compiles.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/gateway/handlers/files.rs
git commit -m "feat(file-hub): wire files.rs async run-branch to insert handler_jobs (surviving enqueue seam)"
```

---

### Task 5: file_handler_worker.rs — generalize spawn_video_worker (poll handler_jobs, POST multipart run w/ job_id, recover_stale)
**Files:**
- Create: `crates/opex-core/src/agent/file_handler_worker.rs`
- Modify: `crates/opex-core/src/agent/mod.rs`
- Modify: `crates/opex-core/src/main.rs`
- Test: `crates/opex-core/src/agent/file_handler_worker.rs` (inline `#[cfg(test)]`, wiremock dispatch test)

**Interfaces:**
- Consumes: `crate::db::handler_jobs::{claim_next_handler_job, mark_handler_job_processing, mark_handler_job_failed, recover_stale_handler_jobs, HandlerJob}` (Task 1); `crate::uploads::{mint_uploads_url, web_uploads_base}` + `crate::agent::url_tools::uploads_local_url` (the real loopback helper, same as `dispatch.rs::run_transcribe`); `infra.secrets.get_upload_hmac_key() -> [u8;32]` (R6 — real accessor, NOT `master_key`); `AppState{config.config.toolgate_url, config.config.gateway.listen, config.config.uploads.signed_url_ttl_secs, infra.db, infra.secrets}`.
- Produces: `pub fn spawn_file_handler_worker(state: &AppState, shutdown: CancellationToken)`; `pub async fn dispatch_async_job(http, toolgate_url, gateway_listen, signed_url_base, key, ttl_secs, job) -> anyhow::Result<()>` (testable seam). R12: the worker DOWNLOADS the upload bytes over loopback in Rust and POSTs `multipart/form-data` (field `file` + text fields + `job_id`) to toolgate; url-based jobs send the `source_url` form field and no `file`. Consumed by `main.rs` startup.

- [ ] **Step 1: Write the failing test** (wiremock asserts multipart POST `/handlers/{id}/run` with `job_id`, accepts 202; R12)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn upload_job() -> crate::db::handler_jobs::HandlerJob {
        crate::db::handler_jobs::HandlerJob {
            id: uuid::Uuid::new_v4(),
            upload_id: Some(uuid::Uuid::new_v4()),
            source_ref: None,
            handler_id: "summarize_video".into(),
            agent_name: "Atlas".into(),
            session_id: uuid::Uuid::new_v4(),
            params: serde_json::json!({"language": "ru"}),
            status: "processing".into(),
            phase: None,
            pct: None,
            result: None,
            attempts: 1,
        }
    }

    fn url_job() -> crate::db::handler_jobs::HandlerJob {
        let mut j = upload_job();
        j.upload_id = None;
        j.source_ref = Some("https://www.youtube.com/watch?v=abc".into());
        j
    }

    #[tokio::test]
    async fn dispatch_upload_job_posts_multipart_with_loopback_bytes_and_accepts_202() {
        // The upload server returns bytes that core fetches over loopback (R12),
        // then re-POSTs as multipart to toolgate.
        let uploads = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"VIDEOBYTES".to_vec()))
            .mount(&uploads)
            .await;

        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/summarize_video/run"))
            .and(header_exists("content-type")) // multipart/form-data; boundary=...
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "accepted": true, "job_id": "ignored"
            })))
            .mount(&toolgate)
            .await;

        // gateway_listen points at the uploads mock so uploads_local_url resolves there.
        let listen = uploads.uri().trim_start_matches("http://").to_string();
        let http = reqwest::Client::new();
        let key = [7u8; 32];
        let res = dispatch_async_job(
            &http,
            &toolgate.uri(),
            &listen,
            "",          // signed_url_base = root-relative (web_uploads_base)
            &key,
            600,
            &upload_job(),
        )
        .await;

        assert!(res.is_ok(), "202 must be treated as success: {res:?}");
    }

    #[tokio::test]
    async fn dispatch_url_job_posts_source_url_without_file() {
        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/summarize_video/run"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&toolgate)
            .await;

        let http = reqwest::Client::new();
        let key = [7u8; 32];
        // url job: no upload, no loopback download — source_ref drives it.
        let res = dispatch_async_job(
            &http,
            &toolgate.uri(),
            "127.0.0.1:18789",
            "",
            &key,
            600,
            &url_job(),
        )
        .await;
        assert!(res.is_ok(), "202 must be treated as success: {res:?}");
    }

    #[tokio::test]
    async fn dispatch_errors_on_non_2xx() {
        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/summarize_video/run"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&toolgate)
            .await;

        let http = reqwest::Client::new();
        let key = [7u8; 32];
        let res =
            dispatch_async_job(&http, &toolgate.uri(), "127.0.0.1:18789", "", &key, 600, &url_job()).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("500"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p opex-core file_handler_worker`. Expected failure: `cannot find function dispatch_async_job` / unresolved module `file_handler_worker`.

- [ ] **Step 3: Write minimal implementation** — `crates/opex-core/src/agent/file_handler_worker.rs`:
```rust
//! Universal durable worker for the File Handler Hub async queue (handler_jobs).
//! Generalization of video_worker.rs: claims a job, and (R12) for upload-based
//! jobs DOWNLOADS the upload bytes over loopback in Rust and POSTs them to
//! toolgate /handlers/{id}/run as multipart (field "file" + job_id), mirroring
//! dispatch.rs::run_transcribe. url-based jobs send the source_url form field
//! and no "file". The 202 means the runner was spawned; results come back via
//! the core callback endpoints (files.rs), not this worker.

use anyhow::Context as _;
use tokio_util::sync::CancellationToken;

use crate::db::handler_jobs::{self, HandlerJob};
use crate::gateway::AppState;

/// POST toolgate /handlers/{id}/run as multipart with job_id (R12). Treats any
/// 2xx (incl. 202 Accepted) as success.
pub async fn dispatch_async_job(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    signed_url_base: &str,
    key: &[u8; 32],
    ttl_secs: u64,
    job: &HandlerJob,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/handlers/{}/run",
        toolgate_url.trim_end_matches('/'),
        job.handler_id
    );
    let language = job
        .params
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("ru")
        .to_string();
    let params_str = serde_json::to_string(&job.params).unwrap_or_else(|_| "{}".to_string());

    let mut form = reqwest::multipart::Form::new()
        .text("mime", String::new())
        .text("filename", "upload".to_string())
        .text("params", params_str)
        .text("language", language)
        .text("job_id", job.id.to_string());

    if let Some(upload_id) = job.upload_id {
        // R12: download the upload bytes over loopback in Rust (mirror run_transcribe),
        // then attach as the "file" part — toolgate never fetches a loopback URL.
        let public = crate::uploads::mint_uploads_url(signed_url_base, upload_id, key, ttl_secs);
        let local = crate::agent::url_tools::uploads_local_url(&public, gateway_listen);
        let resp = http
            .get(&local)
            .send()
            .await
            .with_context(|| format!("loopback GET {local} failed"))?;
        if !resp.status().is_success() {
            anyhow::bail!("loopback upload fetch HTTP {}", resp.status().as_u16());
        }
        let bytes = resp.bytes().await.context("read upload bytes")?;
        let part = reqwest::multipart::Part::bytes(bytes.to_vec()).file_name("upload");
        form = form.part("file", part);
    } else if let Some(source_ref) = &job.source_ref {
        // url-based job (e.g. YouTube): pass the external URL, no "file" part.
        form = form.text("source_url", source_ref.clone());
    }

    let resp = http
        .post(&url)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "toolgate /handlers/{}/run HTTP {}",
            job.handler_id,
            resp.status().as_u16()
        );
    }
    Ok(())
}

/// Spawn the background async-handler worker (concurrency = 1 in v1).
pub fn spawn_file_handler_worker(state: &AppState, shutdown: CancellationToken) {
    let db = state.infra.db.clone();
    let toolgate_url = state
        .config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let gateway_listen = state.config.config.gateway.listen.clone();
    let ttl_secs = state.config.config.uploads.signed_url_ttl_secs;
    let signed_url_base = crate::uploads::web_uploads_base().to_string();
    let key = state.infra.secrets.get_upload_hmac_key(); // R6: real accessor → [u8;32]
    let http = reqwest::Client::new();

    tokio::spawn(async move {
        // Crash recovery: reset rows stuck in 'processing' from a previous run.
        match handler_jobs::recover_stale_handler_jobs(&db).await {
            Ok(n) if n > 0 => {
                tracing::info!(recovered = n, "file_handler_worker: recovered stale jobs")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "file_handler_worker: stale recovery failed"),
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }

            let job = match handler_jobs::claim_next_handler_job(&db).await {
                Ok(Some(j)) => j,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "file_handler_worker: claim failed");
                    continue;
                }
            };

            tracing::info!(job_id = %job.id, handler = %job.handler_id, "file_handler_worker: dispatching");
            // claim already set status=processing; keep it explicit for clarity.
            let _ = handler_jobs::mark_handler_job_processing(&db, job.id).await;

            if let Err(e) = dispatch_async_job(
                &http,
                &toolgate_url,
                &gateway_listen,
                &signed_url_base,
                &key,
                ttl_secs,
                &job,
            )
            .await
            {
                tracing::warn!(job_id = %job.id, error = %e, "file_handler_worker: dispatch failed");
                let _ = handler_jobs::mark_handler_job_failed(&db, job.id, &e.to_string()).await;
            }
            // Success path is terminal-by-callback: the runner posts /complete.
        }
        tracing::info!("file_handler_worker: stopped");
    });
}
```
Register the module in `crates/opex-core/src/agent/mod.rs`:
```rust
pub mod file_handler_worker;
```
In `crates/opex-core/src/main.rs`, alongside the existing `spawn_video_worker(&state, shutdown.clone())` call, add:
```rust
    crate::agent::file_handler_worker::spawn_file_handler_worker(&state, shutdown.clone());
```

- [ ] **Step 4: Run test to verify it passes** — `cargo test -p opex-core file_handler_worker` then `make check`. Expected: `dispatch_upload_job_posts_multipart_with_loopback_bytes_and_accepts_202`, `dispatch_url_job_posts_source_url_without_file`, `dispatch_errors_on_non_2xx` PASS, crate compiles.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/agent/file_handler_worker.rs crates/opex-core/src/agent/mod.rs crates/opex-core/src/main.rs
git commit -m "feat(file-hub): universal async worker — multipart-dispatch handler_jobs w/ job_id, recover stale on startup"
```

---

### Task 6: port summarize_video to toolgate builtin (execution=async) + re-point legacy URL auto-trigger off video_jobs (R13)
**Files:**
- Create: `toolgate/handlers/builtin/summarize_video.py`
- Modify: `toolgate/handlers/context.py` (`HandlerResult.post_action` field)
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs` (re-point URL-video enqueue from `video_jobs` to `handler_jobs`)
- Test: `toolgate/tests/test_handlers_summarize_video.py`

**Interfaces:**
- Consumes (Phase 1–2): `HandlerContext{ stt, progress, result }`, `HandlerFile{bytes, mime, filename, source_url}`, descriptor XML block + `async def run(ctx, file, params)` convention; existing `video_helpers.extract_audio` (media logic stays in Python); `handlers.descriptor.parse_descriptor`. `crate::db::handler_jobs::insert_handler_job` (Task 1); existing `crate::agent::pipeline::subagent::detect_video_links`.
- Produces: builtin handler id `summarize_video`, `execution=async`, emitting a `HandlerResult` whose serialized JSON carries a `post_action` requesting the Obsidian note write (Task 3 reads it). The subagent URL-video auto-trigger now enqueues `handler_jobs` (source_ref = the YouTube link, upload_id=None) instead of `video_jobs` — so Phase 6 can delete the legacy video pipeline without losing the auto-trigger (R13). The legacy `video_jobs` enqueue is removed from this path.

> R13/R15 note: the file-attachment `summarize_video` enqueue now lives in `files.rs` (Task 4, composer button). This task handles the OTHER surviving enqueue — the URL auto-detection in `subagent.rs` — re-pointing it to `handler_jobs`. The `dispatch.rs::run_summarize_video` arm is NOT edited here (Phase 6 deletes it).

- [ ] **Step 1: Write the failing test** (`toolgate/tests/test_handlers_summarize_video.py`)
```python
"""summarize_video builtin: async descriptor + run() produces an outcome whose
serialized JSON includes a post_action for the vault write, posting progress."""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from handlers.descriptor import parse_descriptor  # noqa: E402
from handlers.builtin import summarize_video as sv  # noqa: E402


def _descriptor_source() -> str:
    return Path(sv.__file__).read_text(encoding="utf-8")


def test_descriptor_is_async_video():
    desc = parse_descriptor(_descriptor_source(), tier="builtin")
    assert desc.id == "summarize_video"
    assert desc.execution == "async"
    assert any(m.startswith("video/") for m in desc.match_mimes)


@pytest.mark.asyncio
async def test_run_returns_outcome_with_post_action(monkeypatch):
    progress = []

    class FakeOutcome:
        def __init__(self, s):
            self.status = "ok"
            self.summary_text = s
            self.artifact_urls = []
            self.reason = None
            self.post_action = None
        def to_dict(self):
            d = {"status": self.status, "summary_text": self.summary_text,
                 "artifact_urls": self.artifact_urls, "reason": self.reason}
            if self.post_action is not None:
                d["post_action"] = self.post_action
            return d

    class FakeResult:
        def text(self, s):
            return FakeOutcome(s)

    class FakeStt:
        async def transcribe(self, audio_bytes, language="ru"):
            return "[00:00] речь из видео"

    class FakeCtx:
        def __init__(self):
            self.stt = FakeStt()
            self.result = FakeResult()
        async def progress(self, phase, pct):
            progress.append((phase, pct))

    # Media helper is heavy (ffmpeg) — stub it.
    async def fake_extract(ctx, file):
        return b"AUDIO"
    monkeypatch.setattr(sv, "extract_audio_from_file", fake_extract)

    class FakeFile:
        bytes = b"VIDEOBYTES"
        mime = "video/mp4"
        filename = "v.mp4"
        source_url = None

    out = await sv.run(FakeCtx(), FakeFile(), {"language": "ru"})
    d = out.to_dict()
    assert d["status"] == "ok"
    assert d["summary_text"]
    assert d["post_action"]["kind"] == "obsidian_note"
    assert d["post_action"]["filename"].endswith(".md")
    assert any(p[0] == "digest" for p in progress)
```

- [ ] **Step 2: Run test to verify it fails** — `cd toolgate && pytest tests/test_handlers_summarize_video.py -q`. Expected failure: `ModuleNotFoundError: handlers.builtin.summarize_video`.

- [ ] **Step 3: Write minimal implementation** — `toolgate/handlers/builtin/summarize_video.py` (R12: uses `file.bytes` from the temp path the runner loaded, OR `file.source_url` for url-based jobs — NO loopback fetch):
```python
# <handler>
#   <id>summarize_video</id>
#   <label lang="ru">Конспект видео</label>
#   <label lang="en">Summarize video</label>
#   <description lang="ru">Транскрипт + конспект видео в Obsidian</description>
#   <description lang="en">Transcribe + digest a video into an Obsidian note</description>
#   <icon>video</icon>
#   <match>
#     <mime>video/mp4</mime>
#     <mime>video/quicktime</mime>
#     <mime>video/x-matroska</mime>
#     <mime>video/webm</mime>
#     <max_size_mb>4096</max_size_mb>
#   </match>
#   <capability>stt</capability>
#   <execution>async</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>40</order>
#   <enabled>true</enabled>
# </handler>
"""Async video handler. Heavy media work (ffmpeg/STT) runs in Python here on the
runner-provided bytes (uploaded file) or via yt-dlp on file.source_url (external
link). The final Obsidian note write is requested back to core via the
`post_action` field (the MCP/vault write stays core-bound, per the design §4)."""

import os
import tempfile

from video_helpers import extract_audio


async def extract_audio_from_file(ctx, file) -> bytes:
    """Extract audio from the runner-provided video bytes (R12: NO network fetch
    of a loopback URL). Separated so tests can stub the ffmpeg path. For
    source_url-based jobs the heavy fetch path would download via yt-dlp; v1
    upload-based jobs operate on file.bytes."""
    with tempfile.TemporaryDirectory() as work_dir:
        path = os.path.join(work_dir, "upload.mp4")
        with open(path, "wb") as f:
            f.write(file.bytes)
        return await extract_audio(path)


def _slug(title: str) -> str:
    safe = "".join(c if c.isalnum() or c in " -_" else "" for c in title).strip()
    return (safe.replace(" ", "-")[:60] or "video").lower()


async def run(ctx, file, params):
    language = params.get("language", "ru")

    await ctx.progress("fetch", 10)
    audio = await extract_audio_from_file(ctx, file)

    await ctx.progress("transcribe", 30)
    transcript = await ctx.stt.transcribe(audio, language=language)

    await ctx.progress("digest", 50)
    # v1 digest: transcript-backed note body (LLM digest delegated to a future
    # cycle; here we assemble a readable transcript-backed note).
    title = (file.filename or "video").rsplit(".", 1)[0]
    body = (
        f"## Резюме\n\n{transcript[:600].strip()}\n\n"
        f"## Полный транскрипт\n\n{transcript.strip()}\n"
    )
    note = f"---\ntitle: {title}\n---\n\n# {title}\n\n{body}"

    await ctx.progress("saving", 90)
    out = ctx.result.text(body[:600].strip())
    # Request the core-side Obsidian write (serialized as result.post_action).
    out.post_action = {
        "kind": "obsidian_note",
        "folder": "Summary",
        "filename": f"{_slug(title)}.md",
        "content": note,
    }
    return out
```
The handler sets `out.post_action` on the `HandlerResult`. To make `to_dict()` serialize it, add a `post_action: dict | None = None` field to `HandlerResult` in `toolgate/handlers/context.py` and include it only when set (append to the `to_dict()` from Task 2, keeping the base 4-key shape when no post_action):
```python
        if getattr(self, "post_action", None) is not None:
            d["post_action"] = self.post_action
```

Then re-point the legacy URL-video auto-trigger off `video_jobs`. In `crates/opex-core/src/agent/pipeline/subagent.rs`, replace the `enqueue_video_job` call (the `detect_video_links` loop) with `handler_jobs::insert_handler_job` using `source_ref = &link`, `upload_id = None` (the `db`, `session_id`, `agent_name` bindings are already in scope):
```rust
    let mut video_accepted = false;
    for link in detect_video_links(user_text) {
        // File Handler Hub Phase 5 (R13): URL-video auto-trigger now rides the
        // universal handler_jobs queue (source_ref = the link) so Phase 6 can
        // delete the legacy video_jobs pipeline without losing this auto-trigger.
        match crate::db::handler_jobs::insert_handler_job(
            db,
            None,
            Some(&link),
            "summarize_video",
            agent_name,
            session_id,
            &serde_json::json!({ "language": agent_language }),
        )
        .await
        {
            Ok(_) => {
                enriched.push_str("\n\n🎬 Видео по ссылке принято, готовлю сводку.");
                video_accepted = true;
            }
            Err(e) => tracing::warn!(error = %e, link = %link, "video url enqueue failed"),
        }
    }
```

- [ ] **Step 4: Run test to verify it passes** — `cd toolgate && pytest tests/test_handlers_summarize_video.py -q` then `make check`. Expected: `test_descriptor_is_async_video` and `test_run_returns_outcome_with_post_action` PASS; core compiles with the subagent re-point.

- [ ] **Step 5: Commit**
```bash
git add toolgate/handlers/builtin/summarize_video.py toolgate/handlers/context.py toolgate/tests/test_handlers_summarize_video.py crates/opex-core/src/agent/pipeline/subagent.rs
git commit -m "feat(file-hub): port summarize_video to async builtin + re-point URL auto-trigger onto handler_jobs"
```

---

### Task 7: UI — generalize video_progress to file_job_progress (ws.ts type + chat/page subscription)
**Files:**
- Modify: `ui/src/types/ws.ts`
- Create: `ui/src/app/(authenticated)/chat/file-job-progress.ts`
- Modify: `ui/src/app/(authenticated)/chat/page.tsx`
- Test: `ui/src/app/(authenticated)/chat/__tests__/file-job-progress.test.tsx`

**Interfaces:**
- Consumes: WS event `{"type":"file_job_progress", job_id, handler_id, session_id, phase, pct, status}` (Task 3 wire contract); `useWsSubscription(type, cb)`; `useChatStore.getState().{setVideoProgress, clearVideoProgress}`; `qk.sessionMessages(session_id)`.
- Produces: `WsFileJobProgress` interface added to the `WsEvent` union; a `useWsSubscription("file_job_progress", …)` handler in `chat/page.tsx` (mirrors `video_progress`): progress → `setVideoProgress`; terminal `done|failed` → `clearVideoProgress` + invalidate messages. Pure `handleFileJobProgress` extracted for testing.

- [ ] **Step 1: Write the failing test** (`ui/src/app/(authenticated)/chat/__tests__/file-job-progress.test.tsx`)
```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { handleFileJobProgress } from "@/app/(authenticated)/chat/file-job-progress";

describe("handleFileJobProgress", () => {
  const setVideoProgress = vi.fn();
  const clearVideoProgress = vi.fn();
  const invalidate = vi.fn();
  const store = { setVideoProgress, clearVideoProgress };

  beforeEach(() => {
    setVideoProgress.mockClear();
    clearVideoProgress.mockClear();
    invalidate.mockClear();
  });

  it("sets progress for an in-flight phase", () => {
    handleFileJobProgress(
      { session_id: "s1", phase: "digest", pct: 42, status: "processing" },
      store,
      invalidate,
    );
    expect(setVideoProgress).toHaveBeenCalledWith("s1", "digest", expect.any(String));
    expect(clearVideoProgress).not.toHaveBeenCalled();
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("clears + invalidates on done", () => {
    handleFileJobProgress(
      { session_id: "s1", phase: "done", pct: 100, status: "done" },
      store,
      invalidate,
    );
    expect(clearVideoProgress).toHaveBeenCalledWith("s1");
    expect(invalidate).toHaveBeenCalledWith("s1");
  });

  it("clears + invalidates on failed", () => {
    handleFileJobProgress(
      { session_id: "s1", phase: "x", pct: 0, status: "failed" },
      store,
      invalidate,
    );
    expect(clearVideoProgress).toHaveBeenCalledWith("s1");
    expect(invalidate).toHaveBeenCalledWith("s1");
  });
});
```

- [ ] **Step 2: Run test to verify it fails** — `cd ui && npm test -- file-job-progress`. Expected failure: `Failed to resolve import "@/app/(authenticated)/chat/file-job-progress"`.

- [ ] **Step 3: Write minimal implementation**

`ui/src/types/ws.ts` — add the interface and extend the union:
```ts
export interface WsFileJobProgress {
  type: "file_job_progress";
  job_id: string;
  handler_id: string;
  session_id: string;
  phase: string;
  pct: number;
  status: "processing" | "done" | "failed";
}
```
and add `| WsFileJobProgress` to the `WsEvent` union.

Create `ui/src/app/(authenticated)/chat/file-job-progress.ts`:
```ts
/** Pure handler for the universal async-job progress WS event
 * (generalization of video_progress). Extracted for unit-testing. */
export interface FileJobProgressData {
  session_id: string;
  phase: string;
  pct: number;
  status: "processing" | "done" | "failed";
}

interface ProgressStore {
  setVideoProgress: (sessionId: string, phase: string, text: string) => void;
  clearVideoProgress: (sessionId: string) => void;
}

export function handleFileJobProgress(
  data: FileJobProgressData,
  store: ProgressStore,
  invalidateMessages: (sessionId: string) => void,
): void {
  if (data.status === "done" || data.status === "failed") {
    store.clearVideoProgress(data.session_id);
    invalidateMessages(data.session_id);
    return;
  }
  store.setVideoProgress(data.session_id, data.phase, `${data.pct}%`);
}
```

In `ui/src/app/(authenticated)/chat/page.tsx`, add the subscription right after the existing `video_progress` one (import `handleFileJobProgress`):
```tsx
  useWsSubscription("file_job_progress", useCallback((data: {
    job_id: string; handler_id: string; session_id: string;
    phase: string; pct: number; status: "processing" | "done" | "failed";
  }) => {
    handleFileJobProgress(
      data,
      useChatStore.getState(),
      (sid) => queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sid) }),
    );
  }, []));
```

- [ ] **Step 4: Run test to verify it passes** — `cd ui && npm test -- file-job-progress`. Expected: all 3 `handleFileJobProgress` cases PASS. Then `cd ui && npm run build` to confirm the WS union + page typecheck.

- [ ] **Step 5: Commit**
```bash
git add ui/src/types/ws.ts ui/src/app/(authenticated)/chat/file-job-progress.ts "ui/src/app/(authenticated)/chat/__tests__/file-job-progress.test.tsx" "ui/src/app/(authenticated)/chat/page.tsx"
git commit -m "feat(file-hub): UI file_job_progress WS event — generalize video_progress handling"
```

---

## Phase 6 — cleanup

> **Scope (per R11 + R15):** This phase deletes **only the in-core video pipeline** that the Python `summarize_video` async handler (Phase 5) now owns: `agent/file_scenario/video_summary.rs`, `agent/file_scenario/video_worker.rs`, the `opex_db::video_jobs` module, the `SummarizeVideo` dispatch arm + `run_summarize_video`, the YouTube/Yandex-link enqueue in `subagent.rs`, and the **entire `EnqueueCtx` plumbing** (`EnqueueCtx` struct, the `enqueue: Option<EnqueueCtx>` field on `DispatchInput`, the `run_builtin` `enqueue` param, and the seam construction site) — removed **cleanly** (no `#[allow(dead_code)]`, per R15). The now-orphaned `ScenarioOutcome::video_accepted(...)` constructor (its only callers were `run_summarize_video`) is also removed.
>
> **KEEP (per R2 + R11):** `agent/file_scenario/dispatch.rs` (the 4 remaining sync arms: `transcribe`/`describe`/`extract_document`/`save`), `agent/file_scenario/dispatch_seam.rs` (`PendingAlternative` + `ScenarioChoice` + the sync `dispatch_attachments` seam — it still powers the legacy post-send "file-scenario-chips" SSE + Telegram `fse:` callback path), `agent/file_scenario/{outcome,rewrite,sniff,owner_gate}.rs`, the `ScenarioOutcome.video_accepted: bool` **wire field** (R9 — serde default `false`), `agent/fse/*` (allowlist incl. all 5 ids in `FSE_DEFAULT_ALLOWLIST` + seeder), `gateway/handlers/file_scenarios/` incl. `run.rs` + `run_scenario_and_persist` (the legacy Telegram/web run executor), and the `file_scenarios` table + the skill-binding agent tool.
>
> **Deferred (R2):** migrating the legacy chips/Telegram path onto the new `HandlerRegistry` is a future follow-up. This phase does NOT touch that path beyond removing its dead video branch.
>
> **Note on `FSE_DEFAULT_ALLOWLIST`:** it is defined in `agent/fse/allowlist.rs` and re-exported via `agent/file_scenario/outcome.rs`. It KEEPS all 5 ids (incl. `summarize_video`) — it is the GLOBAL autorun gate AND the `HandlerRegistry` builtin-tier gate (the 5 const ids). After this phase, `summarize_video` is still an allowlist member but is NO LONGER an in-core dispatch builtin (Python owns it), so the in-core `resolve()` table drops to the 4 sync arms.

---

### Task 1: Remove the in-core video call-sites (subagent enqueue + main.rs worker/recovery)

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs`
- Modify: `crates/opex-core/src/main.rs`
- Create: `crates/opex-core/tests/integration_phase6_no_video_refs.rs`

**Interfaces:**
- Consumes (being removed): `opex_db::video_jobs::enqueue_video_job`, `opex_db::video_jobs::recover_stuck_video_jobs`, `crate::agent::file_scenario::video_worker::spawn_video_worker`, the file-local `subagent::{detect_video_links, is_supported_video_host}` helpers.
- Keeps untouched (R2): the `crate::agent::file_scenario::dispatch_seam::dispatch_attachments(...)` enrich call + `enrich_with_attachments(...)` + `rewrite::rewrite_enriched_text(...)` (legacy sync chips path).
- Produces: a pure source-text guard test `integration_phase6_no_video_refs` proving the live (non-test) tree no longer enqueues legacy video jobs or spawns the in-core video worker, so the module deletions in Tasks 3–4 cannot strand a live caller. The live replacement for recovery/worker is `opex_db::handler_jobs::recover_stale` + `crate::agent::file_handler_worker::spawn_file_handler_worker` (Phase 5).

- [ ] **Step 1: Write the failing test** — create `crates/opex-core/tests/integration_phase6_no_video_refs.rs`:

```rust
//! Phase 6 deletion gate: the in-core video pipeline is replaced by the Python
//! `summarize_video` async handler + the universal `handler_jobs` queue
//! (Phase 5). This pure source-text guard (no DB, no toolgate) asserts the live
//! consumers no longer reference the legacy video symbols, so deleting
//! `video_worker.rs` / `video_summary.rs` / `db/video_jobs.rs` in later Phase 6
//! tasks cannot strand a live call site.
//!
//! NOTE: the legacy sync chips / Telegram path (`dispatch_seam`, `dispatch.rs`
//! transcribe/describe/extract/save arms, `file_scenarios/run.rs`) is KEPT and
//! deliberately NOT asserted-against here (R2).

/// `subagent.rs` (the enrich seam) must no longer enqueue legacy video jobs from
/// detected YouTube/Yandex links — that path is gone (Python owns video now).
#[test]
fn subagent_has_no_legacy_video_enqueue() {
    let src = include_str!("../src/agent/pipeline/subagent.rs");
    assert!(
        !src.contains("video_jobs::enqueue_video_job"),
        "subagent.rs still enqueues legacy video_jobs"
    );
    assert!(
        !src.contains("detect_video_links"),
        "subagent.rs still has the dead detect_video_links helper"
    );
    assert!(
        !src.contains("is_supported_video_host"),
        "subagent.rs still has the dead is_supported_video_host helper"
    );
    // The legacy sync attachment dispatch (chips/Telegram, R2) must survive.
    assert!(
        src.contains("dispatch_attachments"),
        "subagent.rs must keep the sync dispatch_attachments enrich seam (R2)"
    );
}

/// `main.rs` must no longer spawn the in-core video worker nor recover stuck
/// video_jobs — Phase 5 replaced both with the universal file_handler_worker +
/// handler_jobs recovery.
#[test]
fn main_has_no_video_worker_or_recovery() {
    let src = include_str!("../src/main.rs");
    assert!(
        !src.contains("spawn_video_worker"),
        "main.rs still spawns the legacy video_worker"
    );
    assert!(
        !src.contains("recover_stuck_video_jobs"),
        "main.rs still recovers legacy video_jobs"
    );
    assert!(
        !src.contains("shutdown_video"),
        "main.rs still has the dead shutdown_video token alias"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs`
  - Expected: both tests FAIL — the live files still contain `video_jobs::enqueue_video_job` + `detect_video_links` + `is_supported_video_host` (subagent.rs) and `spawn_video_worker` + `recover_stuck_video_jobs` + `shutdown_video` (main.rs).

- [ ] **Step 3: Write minimal implementation** — remove the video call-sites.

  **Edit `crates/opex-core/src/agent/pipeline/subagent.rs`** — delete the YouTube/Yandex-link enqueue block (lines ~239–252). The current region reads:

```rust
    enrich_with_attachments(&mut enriched, attachments);

    // Enqueue a `url` video-summarization job for each YouTube link detected
    // in the original (pre-PII-redacted) user text so the job stores the real URL.
    // A successful enqueue marks `video_accepted` so the caller short-circuits the
    // LLM loop — the agent must NOT also try to fetch/transcribe the YouTube link.
    let mut video_accepted = false;
    for link in detect_video_links(user_text) {
        match opex_db::video_jobs::enqueue_video_job(db, session_id, agent_name, "url", &link, None).await {
            Ok(_) => {
                enriched.push_str("\n\n🎬 Видео по ссылке принято, готовлю сводку.");
                video_accepted = true;
            }
            Err(e) => tracing::warn!(error = %e, link = %link, "video url enqueue failed"),
        }
    }
```

  Replace it with (the in-core URL-video enqueue is gone; auto-detected video links are re-pointed onto `handler_jobs` in Phase 5 / R13 at the enrich enqueue site there, not here):

```rust
    enrich_with_attachments(&mut enriched, attachments);

    // Phase 6: the in-core YouTube/Yandex video-URL enqueue was removed. Video is
    // now a Python `summarize_video` async handler driven by the universal
    // `handler_jobs` queue (Phase 5). This enrich seam keeps only the legacy sync
    // attachment dispatch (transcribe/describe/extract/save) that still powers the
    // post-send chips + Telegram path (R2).
```

  Then update the `video_accepted` recomputation tail (line ~277), which reads:

```rust
    let video_accepted = video_accepted || outcomes.iter().any(|o| o.video_accepted);

    EnrichResult { text: enriched, outcomes, pending_alternatives, video_accepted }
```

  to (no in-core dispatch arm sets `video_accepted` after the `SummarizeVideo` arm is gone, so it is always `false` from this seam; the field is retained on `EnrichResult` + the `ScenarioOutcome` wire type per R9):

```rust
    // Phase 6: no in-core dispatch arm sets `video_accepted` anymore (video is a
    // Python async handler), so it is always false from this seam — the LLM loop
    // is never short-circuited here. The field is retained on the wire type (R9).
    let video_accepted = false;

    EnrichResult { text: enriched, outcomes, pending_alternatives, video_accepted }
```

  Then **delete the now-dead `is_supported_video_host` (lines ~288–305) and `detect_video_links` (lines ~308–325) free functions**, and delete their unit tests in the `#[cfg(test)] mod tests` block: `detect_video_links_youtube_only` (line ~932) and `detect_video_links_accepts_yandex_disk` (line ~953). Also delete the `#[sqlx::test]` `enrich_youtube_link_sets_video_accepted` (line ~1027, which asserts a `video_jobs` row is written) — that path is gone. KEEP `enrich_plain_text_leaves_video_accepted_false` (line ~1059) but its assertion `!result.video_accepted` still holds, and KEEP the sync-document test at line ~1019 (`synchronous document scenario must leave video_accepted=false`).

  **Edit `crates/opex-core/src/main.rs`** — delete the recovery call (lines ~735–738):

```rust
    // Recover any video_jobs stuck in 'processing' from a previous crash.
    if let Err(e) = opex_db::video_jobs::recover_stuck_video_jobs(&state.infra.db).await {
        tracing::warn!(error = %e, "video_jobs recovery failed");
    }
```

  (Phase 5 already added `opex_db::handler_jobs::recover_stale(&state.infra.db)` recovery; this video_jobs recovery is now dead.) Delete the `shutdown_video` binding (line ~1321):

```rust
    let shutdown_video = shutdown.clone();
```

  and the worker spawn (lines ~1348–1349):

```rust
    // Video summarization worker (durable video_jobs queue).
    crate::agent::file_scenario::video_worker::spawn_video_worker(state, shutdown_video);
```

  Note: `shutdown` (the parent token) is still consumed at line ~1322 by `let shutdown_health = shutdown;`, so removing only the `shutdown_video` alias leaves no orphan. Confirm `state` is no longer moved-then-used after the deletion (the `spawn_video_worker(state, …)` was the last consumer of `state` in this fn; after removal the fn ends right after the health-monitor `tokio::spawn`, so `state`'s remaining uses are all the `state.channels.*` clones earlier — no move error). If `cargo check` reports `state` as still owned-but-unused at the end, that is fine (it is `&AppState` borrowed throughout `spawn_background_tasks` — no move).

- [ ] **Step 4: Run test to verify it passes**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs`
  - Expected: PASS — `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/tests/integration_phase6_no_video_refs.rs \
        crates/opex-core/src/agent/pipeline/subagent.rs \
        crates/opex-core/src/main.rs
git commit -m "refactor(fse): retire in-core video call-sites (subagent enqueue + worker)

Video is now a Python summarize_video async handler driven by the universal
handler_jobs queue (Phase 5). Removes the YouTube/Yandex URL enqueue from the
enrich seam (+ dead detect_video_links/is_supported_video_host helpers and
their tests) and the main.rs video_worker spawn + video_jobs recovery + the
shutdown_video token alias. The legacy sync attachment dispatch (chips/Telegram,
R2) is untouched. Adds a source-grep guard."
```

---

### Task 2: Drop the `SummarizeVideo` dispatch arm + the `EnqueueCtx` plumbing (keep both files)

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch.rs`
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch_seam.rs`
- Modify: `crates/opex-core/src/agent/file_scenario/outcome.rs`
- Modify: `crates/opex-core/tests/integration_phase6_no_video_refs.rs` (extend)

**Interfaces:**
- Consumes (being removed, R15 — cleanly, NO `#[allow(dead_code)]`): `BuiltinAction::SummarizeVideo`, `dispatch::run_summarize_video`, the `"summarize_video"` arm in `resolve()`, the `EnqueueCtx<'a>` struct, the `enqueue: Option<EnqueueCtx<'a>>` field on `DispatchInput`, the `enqueue` param on `dispatch_seam::run_builtin`, the per-attachment `EnqueueCtx` construction in `dispatch_seam::dispatch_attachments`, and the orphaned `ScenarioOutcome::video_accepted(...)` constructor in `outcome.rs`.
- Keeps (R9/R11): `dispatch.rs` (the `Save`/`Transcribe`/`Describe`/`ExtractDocument` arms + `dispatch_action` + `resolve`), `dispatch_seam.rs` (`PendingAlternative`, `ScenarioChoice`, the sync `dispatch_attachments` seam, `run_builtin` for the 4 sync arms), and the `ScenarioOutcome.video_accepted: bool` **wire field** (serde default `false`, R9).
- Produces: a closed builtin set of exactly the 4 sync arms; `summarize_video` no longer resolves to an in-core builtin (it is a Python-tier handler); `DispatchInput` has no `enqueue` field; `run_builtin` has no `enqueue` param.

> **Allowlist reconciliation:** `FSE_DEFAULT_ALLOWLIST` KEEPS all 5 ids (autorun + HandlerRegistry builtin-tier gate). But `summarize_video` is no longer an in-core dispatch builtin, so the `every_allowlist_member_resolves` unit test (dispatch.rs ~454) is updated to exclude the now-Python-owned `summarize_video` and assert it does NOT resolve.

- [ ] **Step 1: Write the failing test** — append to `crates/opex-core/tests/integration_phase6_no_video_refs.rs`:

```rust
/// The closed in-core dispatch builtin set must no longer contain SummarizeVideo
/// (video moved to the Python handler tier), and the EnqueueCtx plumbing must be
/// fully removed (R15 — clean, no dead_code attrs). dispatch.rs / dispatch_seam.rs
/// themselves are KEPT — only the video arm + enqueue plumbing is cut.
#[test]
fn dispatch_has_no_summarize_video_or_enqueue_plumbing() {
    let dispatch = include_str!("../src/agent/file_scenario/dispatch.rs");
    assert!(
        !dispatch.contains("SummarizeVideo"),
        "dispatch.rs still declares the SummarizeVideo builtin arm"
    );
    assert!(
        !dispatch.contains("run_summarize_video"),
        "dispatch.rs still defines run_summarize_video"
    );
    assert!(
        !dispatch.contains("EnqueueCtx"),
        "dispatch.rs still declares the EnqueueCtx plumbing (must be removed cleanly per R15)"
    );
    // The kept sync arms must survive the cull.
    assert!(dispatch.contains("BuiltinAction::Transcribe"), "Transcribe arm kept");
    assert!(dispatch.contains("BuiltinAction::Save"), "Save arm kept");

    let seam = include_str!("../src/agent/file_scenario/dispatch_seam.rs");
    assert!(
        !seam.contains("video_jobs"),
        "dispatch_seam.rs still references the deprecated video_jobs table"
    );
    assert!(
        !seam.contains("EnqueueCtx"),
        "dispatch_seam.rs still threads EnqueueCtx (must be removed cleanly per R15)"
    );
    // The kept sync seam must survive.
    assert!(
        seam.contains("PendingAlternative"),
        "dispatch_seam.rs must keep PendingAlternative (legacy chips path, R2)"
    );

    // The wire field stays (R9) but its only-caller constructor is gone (R15).
    let outcome = include_str!("../src/agent/file_scenario/outcome.rs");
    assert!(
        outcome.contains("pub video_accepted"),
        "ScenarioOutcome.video_accepted wire field must be kept (R9)"
    );
    assert!(
        !outcome.contains("pub fn video_accepted"),
        "the orphaned ScenarioOutcome::video_accepted constructor must be removed (R15)"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs dispatch_has_no_summarize_video_or_enqueue_plumbing`
  - Expected: FAIL — `dispatch.rs still declares the SummarizeVideo builtin arm` (the arm, `run_summarize_video`, the enum variant, the `EnqueueCtx` struct/field, the seam construction, the `video_jobs` query, and the `pub fn video_accepted` constructor are all still present).

- [ ] **Step 3: Write minimal implementation**

  **Edit `crates/opex-core/src/agent/file_scenario/dispatch.rs`:**
  - Delete the `EnqueueCtx<'a>` struct (lines ~12–19, including its doc comment).
  - In `pub struct DispatchInput<'a>` delete the field `pub enqueue: Option<EnqueueCtx<'a>>,` (line ~32).
  - In the `match action` block (line ~49) delete the arm `BuiltinAction::SummarizeVideo => run_summarize_video(&input).await,` (line ~54).
  - Delete the entire `async fn run_summarize_video(...)` function (lines ~236–288, including its doc comment).
  - In `pub enum BuiltinAction` delete the `SummarizeVideo,` variant (line ~298).
  - In `pub fn resolve(...)` delete the `"summarize_video" => Some(BuiltinAction::SummarizeVideo),` arm (line ~310). The closed table is now exactly the 4 sync builtins.
  - In `#[cfg(test)] mod tests`: delete `resolve_summarize_video` (lines ~461–464); delete the `summarize_video_enqueues_and_acks` `#[sqlx::test]` (lines ~466–504); delete the `summarize_video_dedup_ack_sets_video_accepted` (lines ~524+), `summarize_video_persists_source_title` (lines ~553+), and `summarize_video_dedup_skips_second_enqueue` (lines ~583+) `#[sqlx::test]`s (all use `EnqueueCtx`/`video_jobs`). For the remaining kept sync-arm test inputs that construct `DispatchInput { .. }`, remove the `enqueue: ...` field line from each so they match the new struct shape.
  - Replace `every_allowlist_member_resolves` (lines ~454–459) with:

```rust
    #[test]
    fn every_in_core_allowlist_member_resolves() {
        // `summarize_video` is now a Python async handler (Phase 5/6), not an
        // in-core dispatch builtin, so it is excluded here. The other 4 const
        // members remain in-core deterministic builtins.
        for name in crate::agent::file_scenario::outcome::FSE_DEFAULT_ALLOWLIST {
            if *name == "summarize_video" {
                continue;
            }
            assert!(resolve(name).is_some(), "in-core allowlist member {name} must resolve");
        }
        assert!(
            resolve("summarize_video").is_none(),
            "summarize_video must NOT resolve to an in-core builtin — Python owns it"
        );
    }
```

  - Replace the `video_accepted_flag_only_on_video_ack_constructor` test (lines ~508–519) — its first assertion calls the removed `ScenarioOutcome::video_accepted(...)` constructor, so retarget it to assert the kept constructors leave the wire field `false` (validating the serde default):

```rust
    /// The `video_accepted` wire field (R9) defaults false on every surviving
    /// constructor — no in-core path sets it true anymore (video is Python now).
    #[test]
    fn surviving_constructors_leave_video_accepted_false() {
        assert!(!ScenarioOutcome::ok("transcript".into(), vec![]).video_accepted);
        assert!(!ScenarioOutcome::save("saved".into(), vec![]).video_accepted);
        assert!(!ScenarioOutcome::failed("boom".into()).video_accepted);
        assert!(!ScenarioOutcome::unsupported("nope".into()).video_accepted);
        assert!(!ScenarioOutcome::timeout().video_accepted);
    }
```

  **Edit `crates/opex-core/src/agent/file_scenario/outcome.rs`:**
  - Delete the `pub fn video_accepted(summary_text: String, artifact_urls: Vec<String>) -> Self { ... }` constructor (line ~48, its only callers were in `run_summarize_video`, now deleted). KEEP the `pub video_accepted: bool` struct field (R9 wire field, `#[serde(default)]`) and KEEP `status_from_http` + the other constructors (`ok`/`save`/`failed`/`unsupported`/`timeout`/`too_large`).
  - If `outcome.rs` has an inline test invoking `ScenarioOutcome::video_accepted(...)`, delete only that test/assertion; the `FSE_DEFAULT_ALLOWLIST` const-membership test at line ~139 (asserting the 5 ids) is UNCHANGED.

  **Edit `crates/opex-core/src/agent/file_scenario/dispatch_seam.rs`:**
  - In `async fn run_builtin(...)` (line ~85) delete the `enqueue: Option<crate::agent::file_scenario::dispatch::EnqueueCtx<'_>>,` parameter (line ~92), and delete the `enqueue,` line from the `DispatchInput { .. }` it constructs (line ~102).
  - In `dispatch_attachments` default-binding branch, delete the per-attachment `EnqueueCtx` construction (lines ~189–197):

```rust
                // Build enqueue context once per attachment; pass Some(enq) to the
                // default-binding run path so summarize_video can enqueue.
                // Save-fallback arms (0-binding and no-default branches) pass None.
                let enq = crate::agent::file_scenario::dispatch::EnqueueCtx {
                    db,
                    session_id,
                    agent_name,
                    source_type: "file",
                };
```

  and change the corresponding default-binding `run_builtin(action_to_run, …, att, Some(enq))` call (lines ~198–207) to drop the trailing `Some(enq)` argument entirely (the param no longer exists):

```rust
                let outcome = run_builtin(
                    action_to_run,
                    http_client,
                    gateway_listen,
                    toolgate_url,
                    agent_language,
                    att,
                )
                .await;
```

  - In the two save-fallback branches (lines ~258–267 and ~273–282) drop the trailing `None` argument from each `run_builtin("save", …, att, None)` call (the param is gone):

```rust
                let outcome = run_builtin(
                    "save",
                    http_client,
                    gateway_listen,
                    toolgate_url,
                    agent_language,
                    att,
                )
                .await;
```

  - Since `db`, `session_id`, `agent_name` are now no longer consumed by the (deleted) `EnqueueCtx` construction, verify they are still used elsewhere in `dispatch_attachments`: `db` is used by `get_enabled_allowlist(db)`, `list_enabled_for_match_type(db, …)`, and `audit_spawn(db.clone(), …)` — so it stays. `session_id` and `agent_name` were used ONLY by the deleted `EnqueueCtx`; they become unused params. To avoid `unused_variable` warnings under `-D warnings`, rename them to `_session_id` and `_agent_name` in the `dispatch_attachments` signature (do NOT change call sites — they remain positional). Add a one-line comment: `// `_session_id`/`_agent_name` retained for call-site signature stability; the only consumer (video EnqueueCtx) was removed in Phase 6.`
  - Delete the `#[sqlx::test] async fn video_default_enqueues_job_not_sync_call` test (lines ~1476–1515) — it seeds a `video/*` → `summarize_video` default and asserts a `video_jobs` row was written; that path is gone.

- [ ] **Step 4: Run test to verify it passes**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs dispatch_has_no_summarize_video_or_enqueue_plumbing`
  - Expected: PASS. (Whole-crate `cargo check` still errors on the `opex_db::video_jobs` module + the `video_summary`/`video_worker` modules — deleted in Tasks 3–4; the full gate is Task 6. Optionally run `cargo check -p opex-core 2>&1 | head -20` and confirm the only remaining errors reference `video_summary`/`video_worker`/`video_jobs`, addressed next.)

- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/agent/file_scenario/dispatch.rs \
        crates/opex-core/src/agent/file_scenario/dispatch_seam.rs \
        crates/opex-core/src/agent/file_scenario/outcome.rs \
        crates/opex-core/tests/integration_phase6_no_video_refs.rs
git commit -m "refactor(fse): drop SummarizeVideo arm + EnqueueCtx plumbing (clean)

Video is a Python async handler now. Removes the SummarizeVideo builtin
(arm + run_summarize_video + enum variant + resolve mapping), the entire
EnqueueCtx plumbing (struct + DispatchInput.enqueue field + run_builtin param +
seam construction), the seam video_jobs test, and the orphaned
ScenarioOutcome::video_accepted constructor — all cleanly, no dead_code attrs
(R15). KEEPS dispatch.rs (4 sync arms), dispatch_seam.rs (PendingAlternative +
sync seam), and the ScenarioOutcome.video_accepted wire field (R9).
FSE_DEFAULT_ALLOWLIST keeps all 5 ids (autorun + handler-tier gate)."
```

---

### Task 3: Delete the in-core async video pipeline (`video_summary.rs`, `video_worker.rs`)

**Files:**
- Delete: `crates/opex-core/src/agent/file_scenario/video_summary.rs`
- Delete: `crates/opex-core/src/agent/file_scenario/video_worker.rs`
- Modify: `crates/opex-core/src/agent/file_scenario/mod.rs`
- Modify: `crates/opex-core/src/lib.rs`
- Modify: `crates/opex-core/tests/integration_phase6_no_video_refs.rs` (extend)

**Interfaces:**
- Consumes (being removed): `crate::agent::file_scenario::video_worker::{spawn_video_worker, emit_video_progress, ...}`, `crate::agent::file_scenario::video_summary::*`. The live replacement is `crate::agent::file_handler_worker::spawn_file_handler_worker` (Phase 5) + WS `file_job_progress`.
- Module-mount facts (R16): `video_summary` is mounted in **TWO** places — `agent/file_scenario/mod.rs` line 13 (`pub mod video_summary;`) **and** `lib.rs` lines 101–102 (`#[path = "video_summary.rs"] pub mod video_summary;`). `video_worker` is declared in **ONE** place only — `agent/file_scenario/mod.rs` line 14 (`pub mod video_worker;`); it is **NOT** mounted in `lib.rs`. This task removes the `mod.rs` `video_summary` + `video_worker` decls AND the `lib.rs` `video_summary` mount.
- Produces: no `video_summary` / `video_worker` modules in the `file_scenario` tree; WS `video_progress` / `video_summary_ready` emission fully superseded by the generic `file_job_progress` (Phase 5).

- [ ] **Step 1: Write the failing test** — append to `crates/opex-core/tests/integration_phase6_no_video_refs.rs`:

```rust
/// The in-core async-video modules must be gone from the file_scenario tree, the
/// mod facade must not declare them, and lib.rs must not mount video_summary.
#[test]
fn video_modules_are_deleted() {
    let mod_rs = include_str!("../src/agent/file_scenario/mod.rs");
    assert!(
        !mod_rs.contains("pub mod video_summary") && !mod_rs.contains("pub mod video_worker"),
        "file_scenario/mod.rs still declares video_summary / video_worker"
    );
    let lib_rs = include_str!("../src/lib.rs");
    assert!(
        !lib_rs.contains("video_summary") && !lib_rs.contains("video_worker"),
        "lib.rs still mounts the deleted video_summary/video_worker module"
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/agent/file_scenario");
    assert!(!dir.join("video_summary.rs").exists(), "video_summary.rs must be deleted");
    assert!(!dir.join("video_worker.rs").exists(), "video_worker.rs must be deleted");
    // Kept shell must survive.
    assert!(dir.join("dispatch.rs").exists(), "dispatch.rs must be kept (R11)");
    assert!(dir.join("dispatch_seam.rs").exists(), "dispatch_seam.rs must be kept (R11)");
    assert!(dir.join("owner_gate.rs").exists(), "owner_gate.rs must be kept");
}
```

- [ ] **Step 2: Run test to verify it fails**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs video_modules_are_deleted`
  - Expected: FAIL — `file_scenario/mod.rs still declares video_summary / video_worker` (the `pub mod video_summary;` / `pub mod video_worker;` lines, the `lib.rs` `#[path]` mount, and both source files still exist).

- [ ] **Step 3: Write minimal implementation**

  Delete the two files:
```bash
git rm crates/opex-core/src/agent/file_scenario/video_summary.rs \
       crates/opex-core/src/agent/file_scenario/video_worker.rs
```

  **Edit `crates/opex-core/src/agent/file_scenario/mod.rs`** — remove the two declarations `pub mod video_summary;` (line 13) and `pub mod video_worker;` (line 14). Keep every other declaration and re-export exactly as-is (R11 keeps `dispatch`, `dispatch_seam`, `outcome`, `rewrite`, `sniff`, `owner_gate` + their `pub use` lines, incl. the `pub use dispatch_seam::{dispatch_attachments, PendingAlternative, ScenarioChoice};` and `pub use outcome::{FSE_DEFAULT_ALLOWLIST, ScenarioOutcome, ScenarioStatus};`).

  **Edit `crates/opex-core/src/lib.rs`** — in the `pub mod file_scenario { ... }` facade block, remove the `video_summary` mount (lines 99–102):

```rust
        // Task 7: video summary builder (pure leaf module with serde only).
        // Inline tests use opex_types only — safe to expose from lib facade.
        #[path = "video_summary.rs"]
        pub mod video_summary;
```

  Leave the `rewrite`/`dispatch`/`owner_gate` mounts (lines 91–97, 104–109) untouched. (`video_worker` is NOT mounted in lib.rs — no lib.rs edit needed for it.)

- [ ] **Step 4: Run test to verify it passes**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs video_modules_are_deleted`
  - Expected: PASS — `test result: ok`.

- [ ] **Step 5: Commit**
```bash
git add -A crates/opex-core/src/agent/file_scenario/ crates/opex-core/src/lib.rs \
          crates/opex-core/tests/integration_phase6_no_video_refs.rs
git commit -m "refactor(fse): delete in-core video_summary + video_worker

Async video is now a Python summarize_video handler driven by the universal
handler_jobs queue + file_handler_worker (Phase 5). Removes the legacy in-core
video pipeline + the mod.rs decls + the lib.rs video_summary facade mount
(video_worker was never mounted in lib.rs). The kept FSE shell (dispatch,
dispatch_seam, outcome, rewrite, sniff, owner_gate) is untouched."
```

---

### Task 4: Remove the `opex_db::video_jobs` module + add non-destructive migration 068

**Files:**
- Delete: `crates/opex-db/src/video_jobs.rs`
- Modify: `crates/opex-db/src/lib.rs`
- Create: `migrations/068_video_jobs_deprecate.sql`
- Modify: `crates/opex-core/tests/integration_phase6_no_video_refs.rs` (extend)

**Interfaces:**
- Consumes (being removed): `opex_db::video_jobs::{VideoJob, enqueue_video_job, claim_next_video_job, recover_stuck_video_jobs, find_recent_active_video_job, ...}` — the last live callers were retired in Tasks 1–2.
- Produces: an `opex-db` crate with no `video_jobs` module. Migrations `064_video_jobs.sql` / `065_video_jobs_source_title.sql` are KEPT for history. `068` is a non-destructive deprecation marker (no `DROP TABLE`, R15) so existing deployments are not broken and migration checksums stay monotonic. Highest existing migration is 065; 066 (Phase 3) + 067 (Phase 5) precede this — no collision.

- [ ] **Step 1: Write the failing test** — append to `crates/opex-core/tests/integration_phase6_no_video_refs.rs`:

```rust
/// opex-db must no longer expose the video_jobs module, and the deprecation
/// migration must be non-destructive (no DROP TABLE).
#[test]
fn video_jobs_module_removed_and_migration_non_destructive() {
    let dblib = include_str!("../../opex-db/src/lib.rs");
    assert!(
        !dblib.contains("pub mod video_jobs"),
        "opex-db lib still declares video_jobs"
    );
    let mig = include_str!("../../../migrations/068_video_jobs_deprecate.sql");
    assert!(
        !mig.to_uppercase().contains("DROP TABLE"),
        "068 must NOT drop video_jobs (history-preserving deprecation only)"
    );
    assert!(
        mig.to_lowercase().contains("video_jobs"),
        "068 should reference video_jobs in its deprecation note"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs video_jobs_module_removed_and_migration_non_destructive`
  - Expected: FAIL — first a compile error on the missing `migrations/068_video_jobs_deprecate.sql` (`include_str!`), then `opex-db lib still declares video_jobs`.

- [ ] **Step 3: Write minimal implementation**

  Create `migrations/068_video_jobs_deprecate.sql` (non-destructive marker — the table and its rows are retained for history, just no longer read/written):

```sql
-- 068: Deprecate the legacy video_jobs queue (File Handler Hub, Phase 6).
--
-- The in-core video pipeline (video_worker.rs / video_summary.rs /
-- opex_db::video_jobs) has been replaced by the universal handler_jobs queue
-- (migration 067) + the Python summarize_video async handler. The video_jobs
-- table is NO LONGER read or written by any code path.
--
-- We deliberately do NOT drop the table: existing rows are historical job
-- records and a destructive migration would break rollback/audit. Operators may
-- drop it manually once retention is no longer needed:
--     DROP TABLE IF EXISTS video_jobs;
--
-- This migration only re-comments the table so the sequence stays monotonic.
COMMENT ON TABLE video_jobs IS
  'DEPRECATED (m068, 2026-06-30): superseded by handler_jobs. No longer read/written.';
```

  Delete the module:
```bash
git rm crates/opex-db/src/video_jobs.rs
```

  **Edit `crates/opex-db/src/lib.rs`** — remove the single `pub mod video_jobs;` declaration (line 11). Leave all other `pub mod ...;` / `pub use ...;` lines exactly as they are (only that one line is removed).

- [ ] **Step 4: Run test to verify it passes**
  - Command: `cargo test -p opex-core --test integration_phase6_no_video_refs video_jobs_module_removed_and_migration_non_destructive`
  - Expected: PASS — `test result: ok`.

- [ ] **Step 5: Commit**
```bash
git add crates/opex-db/src/lib.rs migrations/068_video_jobs_deprecate.sql \
        crates/opex-core/tests/integration_phase6_no_video_refs.rs
git rm crates/opex-db/src/video_jobs.rs
git commit -m "refactor(db): remove video_jobs module; deprecate table (m068, non-destructive)

The legacy video_jobs queue is superseded by handler_jobs (m067). Drops the
Rust module (no live caller after Phase 6 Tasks 1-2) and adds a
history-preserving deprecation marker migration — the table and its rows are
retained, not dropped (rollback/audit safety; checksums stay monotonic)."
```

---

### Task 5: Confirm the kept FSE integration tests still compile + pass unchanged

**Files:**
- Read-only verify (no edits expected): `crates/opex-core/tests/integration_fse_regression.rs`, `crates/opex-core/tests/integration_fse_security.rs`, `crates/opex-core/tests/integration_fse_affordance.rs`

**Interfaces:**
- Consumes (all KEPT per R2/R11, so these imports still resolve): `opex_core::agent::file_scenario::dispatch::{dispatch_action, DispatchInput, resolve}`, `dispatch_seam.rs` (via `include_str!`), `gateway/handlers/file_scenarios/run.rs` + `run_scenario_and_persist` (via `include_str!`), `opex_core::agent::fse::allowlist::{validate_binding_write, is_allowed_for_autorun, FSE_DEFAULT_ALLOWLIST}`, `opex_core::agent::file_scenario::assert_fse_owner`.
- Produces: documented confirmation that NO retarget is needed (these three suites do not touch the deleted video pipeline) — they exercise the kept sync dispatch, the kept run executor, the allowlist, and the owner-gate. The one structural change from Task 2 that touches them is the removed `DispatchInput.enqueue` field: if any of these suites constructs a `DispatchInput { .. }` with an `enqueue:` field, that one field line must be dropped (otherwise no edit).

> This task is a **verification gate**, not a deletion. R2/R11 keep `dispatch.rs`, `dispatch_seam.rs`, and `run.rs`, so every import in these three files still resolves. The only Task-2 ripple is (a) the removed `DispatchInput.enqueue` field and (b) `summarize_video` no longer resolving to an in-core builtin. The security file asserts the *allowlist const* has 5 members (still holds — the const is unchanged); the regression file dispatches only transcribe/describe/extract_document; the affordance file `include_str!`s `run.rs`/`inline.rs`/`dispatch_seam.rs`, all kept.

- [ ] **Step 1: Write the failing test** — none. The check is that the kept suites still build after Tasks 1–4:
  - Command: `cargo test -p opex-core --test integration_fse_regression --test integration_fse_security --test integration_fse_affordance --no-run`
  - Expected: COMPILE OK (no `unresolved import` — `dispatch`, `dispatch_seam`, `run.rs`, `assert_fse_owner`, the allowlist fns all still exist). If a suite constructs `DispatchInput { ..., enqueue: None }`, expect a `struct DispatchInput has no field named enqueue` error here pinpointing the exact line to fix in Step 3.

- [ ] **Step 2: Run test to verify** (failure mode to guard against = compile error)
  - Command: `cargo test -p opex-core --test integration_fse_regression --test integration_fse_security --test integration_fse_affordance --no-run`
  - Expected failure signals: `error[E0432]: unresolved import opex_core::agent::file_scenario::dispatch_seam` would mean a kept module was wrongly deleted (revert that deletion); `error[E0560]: struct ... has no field named enqueue` means a suite still threads the removed field (fix in Step 3). With Tasks 1–4 done correctly and no `enqueue:` literal in these suites, this compiles clean.

- [ ] **Step 3: Write minimal implementation** — **only if** Step 2 surfaced an error from a removed-field ripple or an over-reach. Permissible narrow edits: (a) drop a `enqueue: None,` / `enqueue: Some(...)` field line from any `DispatchInput { .. }` constructed in these suites; (b) if any suite asserted `resolve("summarize_video").is_some()`, change it to `resolve("summarize_video").is_none()` to match the Task-2 closed table. Otherwise leave all three files untouched. (The security file currently asserts only the const at line ~65 and `resolve(code_exec).is_none()` — both still valid; no edit expected there.)

- [ ] **Step 4: Run test to verify it passes**
  - Command: `cargo test -p opex-core --test integration_fse_regression --test integration_fse_security --test integration_fse_affordance`
  - Expected: PASS — `test result: ok` for each suite (the wiremock-backed transcribe/describe/extract regression, the allowlist + fail-closed security guards, and the owner-gate/affordance source guards all still hold).

- [ ] **Step 5: Commit** — only if Step 3 made an edit; otherwise skip this commit (nothing changed).
```bash
# Only if a removed-field ripple or drifted assertion was corrected:
git add crates/opex-core/tests/integration_fse_regression.rs \
        crates/opex-core/tests/integration_fse_security.rs \
        crates/opex-core/tests/integration_fse_affordance.rs
git commit -m "test(fse): keep FSE integration suites green after video-pipeline removal

dispatch.rs / dispatch_seam.rs / run.rs are kept (R2/R11), so the regression,
security, and affordance suites still resolve and pass. Drops the removed
DispatchInput.enqueue field where threaded; summarize_video is no longer an
in-core builtin (the const-based allowlist guard is unchanged)."
```

---

### Task 6: Update CLAUDE.md architecture notes + final whole-branch gate

**Files:**
- Modify: `d:\GIT\bogdan\opex\CLAUDE.md`
- Test: (gate only — full repo build/lint/test suite across Rust + Python + UI)

**Interfaces:**
- Consumes: the entire post-cleanup tree (Tasks 1–5) + Phases 1–5 deliverables (`toolgate/handlers/*`, `agent/handler_registry.rs`, `gateway/handlers/files.rs`, `db/handler_jobs.rs`, `agent/file_handler_worker.rs`, `agent/provenance.rs`, migrations 066/067/068).
- Produces: documentation that matches the shipped architecture (no live references to the deleted in-core video pipeline; accurate KEPT-vs-removed description) + a green full gate proving the whole branch builds, lints, and tests across Rust + Python + UI.

- [ ] **Step 1: Write the failing test** — the "test" is a doc-accuracy grep gate. Confirm CLAUDE.md still describes the removed in-core video pipeline:
  - Command: `grep -nE 'video_worker\.rs|video_summary\.rs|VIDEO async \(to generalize\)|video_jobs' CLAUDE.md`
  - Expected: matches found (the "VIDEO async (to generalize)" block + `video_worker.rs`/`video_summary.rs`/`video_jobs` mentions describing the in-core pipeline, which must be replaced with the File Handler Hub description).

- [ ] **Step 2: Run test to verify it fails**
  - Command: `grep -cE 'video_worker|video_summary|VIDEO async \(to generalize\)' CLAUDE.md`
  - Expected: a non-zero count — the doc still mentions the removed in-core pipeline.

- [ ] **Step 3: Write minimal implementation** — edit CLAUDE.md. Add a "File Handler Hub" subsection under "### Tools (`src/tools/`)" and rewrite/remove the stale "VIDEO async (to generalize)" block. Concrete new text:

```markdown
### File Handler Hub (toolgate handlers + core orchestration)

File processing (transcribe / describe / extract_document / save / summarize_video
+ custom handlers) lives in **toolgate** as self-describing Python handlers
(`toolgate/handlers/builtin/*.py` + `workspace/file_handlers/*.py`, hot-reloaded
via watchfiles). Each handler = an XML descriptor comment + `async def run(ctx, file, params)`.

- **Discovery:** core `agent/handler_registry.rs` (`HandlerRegistry` in `AppState`)
  does a conditional GET of toolgate `GET /handlers` (ETag, ~30s, fail-soft).
- **Matching:** pure-Rust `match_buttons(mime, size, enabled_allowlist, lang)` —
  builtin-tier handlers gated by the GLOBAL `fse.allowlist` (the 5 const ids in
  `FSE_DEFAULT_ALLOWLIST`); workspace-tier allowed by default (trusted-author v1).
- **Run (bytes, never loopback URL):** `gateway/handlers/files.rs` —
  `GET /api/files/{id}/actions` (buttons), `POST /api/files/{id}/run`. Core
  downloads the upload bytes via a loopback signed URL (`mint_uploads_url` +
  `uploads_local_url`) and POSTs **multipart** ("file" + mime/filename/params/
  language) to toolgate `/handlers/{id}/run`; toolgate NEVER fetches the loopback
  URL (mirrors the existing `dispatch.rs` run_transcribe, R12). Sync → inline
  outcome; async → `handler_jobs` row.
- **Async queue:** universal `handler_jobs` table (m067, carries upload_id OR
  source_ref for url-based jobs) + `agent/file_handler_worker.rs`
  (`spawn_file_handler_worker`, 5s poll, stale recovery). The out-of-process
  Python runner reads bytes from a tempfile (no network fetch), posts progress →
  `POST /api/files/jobs/{id}/progress` (WS `file_job_progress`) and the final
  `ScenarioOutcome` → `POST /api/files/jobs/{id}/complete`.
- **Provenance:** `agent/provenance.rs::wrap_file_output` wraps the persisted
  message content (`messages.source='file_handler'`, m066) with
  `<file_output trust="untrusted">` at INSERT time, before it reaches the LLM.

**Coexisting legacy path (KEPT, not migrated — see Phase 6/R2):** the in-core
`agent/file_scenario/{dispatch,dispatch_seam,outcome,rewrite,sniff,owner_gate}.rs`
shell + `agent/fse/allowlist*` + `gateway/handlers/file_scenarios/run.rs`
(`run_scenario_and_persist`) + the `file_scenarios` table (m060/m061) + the
skill-binding **agent tool** (`agent/tool_handlers/file_scenario.rs`) still power
the post-send "file-scenario-chips" SSE affordance and the Telegram `fse:` callback.
Migrating those onto the HandlerRegistry is a future follow-up.

**Removed in Phase 6:** the in-core async **video** pipeline
(`agent/file_scenario/video_summary.rs`, `video_worker.rs`), the `SummarizeVideo`
dispatch arm + its `EnqueueCtx` plumbing (struct + `DispatchInput.enqueue` field +
`run_builtin` param + seam construction) + the `ScenarioOutcome::video_accepted`
constructor, and the `opex_db::video_jobs` module. Video is now the Python
`summarize_video` async handler on the `handler_jobs` queue. The `video_jobs`
table is deprecated, not dropped (m068, history-preserving). The
`ScenarioOutcome.video_accepted` serde wire field is retained (defaults false).
```

  Then **remove the existing "VIDEO async (to generalize)" block** (and any other `video_worker.rs` / `video_summary.rs` / `video_jobs` mention describing the removed in-core pipeline) so the grep gate is clean. Do NOT remove references that describe the kept legacy `file_scenarios` / dispatch path.

- [ ] **Step 4: Run test to verify it passes** — doc gate + reference-cleanliness grep + full whole-branch gate (R11 final gate):
  - Doc gate: `grep -cE 'video_worker\.rs|video_summary\.rs|VIDEO async \(to generalize\)' CLAUDE.md` → expected `0`.
  - Reference-cleanliness grep (R11/R16): `grep -rn "spawn_video_worker\|video_jobs::\|video_summary::\|run_summarize_video\|BuiltinAction::SummarizeVideo\|detect_video_links\|is_supported_video_host\|EnqueueCtx\|recover_stuck_video_jobs" crates/ --include="*.rs" | grep -v "tests/integration_phase6_no_video_refs.rs"` → expected **no output** (no live reference to any deleted video/enqueue symbol remains; the only allowed hits are the guard-test string literals, which the grep excludes).
  - Rust build: `make check` → expected `Finished` (no errors). Then `make lint` → expected `Finished` with `-D warnings` clean.
  - Rust tests: `cargo test -p opex-core -p opex-db` (DB-backed `#[sqlx::test]` skipped without `DATABASE_URL`, as documented) → expected all non-DB tests `ok`, incl. `integration_phase6_no_video_refs` (5 guard tests) + the kept FSE suites.
  - Python: `cd toolgate && pytest tests/` → expected all green (handler descriptor/loader/ctx/builtin/runner suites).
  - UI: `cd ui && npm test` → expected vitest one-shot all pass (incl. `FileActionButtons` tests).

- [ ] **Step 5: Commit**
```bash
git add CLAUDE.md
git commit -m "docs: document File Handler Hub; record Phase 6 video-pipeline removal

Adds the toolgate HandlerRegistry architecture (handlers + ctx + bytes-multipart
run + handler_jobs + provenance), notes the KEPT legacy file_scenarios/dispatch/
run path (R2), and removes the stale in-core 'VIDEO async' notes. Phase 6 deleted
only the in-core video pipeline (video_summary/video_worker/video_jobs +
SummarizeVideo arm + EnqueueCtx plumbing). Final whole-branch gate green:
make check + make lint + toolgate pytest + ui vitest."
```
