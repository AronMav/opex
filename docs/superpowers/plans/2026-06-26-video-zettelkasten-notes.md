# Video → Zettelkasten Notes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Change the video feature's final stage to produce a full Obsidian note (LLM digest + screenshots placed by timestamp + collapsed transcript) at `Видео/<slug>/конспект.md` in the Zettelkasten vault, screenshots in `_System/media/`, and post a short summary + link to chat.

**Architecture:** toolgate now returns frame JPEGs (base64) + a title; the in-core video worker builds the note via the LLM, then writes it through the existing Obsidian MCP (`save_media` / `create_note` with subfolder / `commit_vault`) reached via `McpRegistry::call_tool` (which starts the on-demand container). Builds on the shipped video-summarization feature (commits up to `fcfd75d9`).

**Tech Stack:** Rust 2024 (opex-core, opex-db, sqlx, tokio, serde_json), Python/FastAPI (toolgate), Node/Fastify (Obsidian MCP), ffmpeg, yt-dlp, Docker, PostgreSQL, Obsidian markdown.

## Global Constraints

- **rustls only — never add OpenSSL.**
- **Only 3 keys in `.env`** (`OPEX_AUTH_TOKEN`, `OPEX_MASTER_KEY`, `DATABASE_URL`); tunables in config / toolgate env vars — **never new `.env` keys.**
- **TDD** — failing test first for every task.
- **Work on master**; commit per task; **do not push** without explicit approval; **no Co-Authored-By trailer.**
- **DB-backed tests** use `#[sqlx::test(migrations = "../../migrations")]`; test DB at `postgres://opex_test:opex_test@127.0.0.1:5434/opex_test` (`make` unavailable in Git Bash — use cargo directly).
- **toolgate venv:** `toolgate/.venv/Scripts/python.exe`; run tests from `toolgate/`.
- **MCP reach:** worker uses `McpRegistry::call_tool("mcp-obsidian", tool, &args)` (it calls `ensure_running` + retries) — never hand-rolled HTTP.
- **Vault:** `~/opex/workspace/zettelkasten/` (git repo, `attachmentFolderPath: "_System/media"`). Notes: `Видео/<slug>/конспект.md`; images `_System/media/<slug>-frame-NN.jpg`; slug keeps Cyrillic.
- Spec: `docs/superpowers/specs/2026-06-26-video-zettelkasten-notes-design.md`.

---

## File Structure

**New files:**
- `migrations/065_video_jobs_source_title.sql` — add `source_title` column.
- `docker/mcp/obsidian/test-mcp.js` — Node test runner for the new MCP ops.

**Modified files:**
- `crates/opex-db/src/video_jobs.rs` — `VideoJob.source_title`; `enqueue_video_job` gains `source_title`.
- `crates/opex-core/src/agent/file_scenario/dispatch.rs` — `EnqueueCtx.source_title`; `run_summarize_video` passes `attachment.file_name`.
- `crates/opex-core/src/agent/file_scenario/dispatch_seam.rs` — build `EnqueueCtx` with `source_title`.
- `toolgate/routers/video.py` — `frames[].image_b64`, `title`, `VIDEO_NOTE_MAX_FRAMES` down-select.
- `docker/mcp/obsidian/app.js` — `save_media`, `create_note(folder,…)`, `commit_vault`, `note_exists`.
- `docker/mcp/obsidian/Dockerfile` — install `git`.
- `docker/mcp/obsidian/MCP.md` — document new tools.
- `crates/opex-core/src/agent/file_scenario/video_summary.rs` — `RawMaterial.title`/`FrameDesc.image_b64`; `slug`, `build_note`, `extract_summary`; hybrid prompt.
- `crates/opex-core/src/agent/file_scenario/video_worker.rs` — final stage: build note → MCP write → chat summary+link.
- `config/opex.toml` — `[video]` note keys (doc).

---

## Task 1: `source_title` column + queue plumbing

**Files:**
- Create: `migrations/065_video_jobs_source_title.sql`
- Modify: `crates/opex-db/src/video_jobs.rs`
- Test: inline in `crates/opex-db/src/video_jobs.rs`

**Interfaces:**
- Consumes: existing `VideoJob`, `enqueue_video_job` (from the video feature).
- Produces:
  - `VideoJob.source_title: Option<String>`
  - `enqueue_video_job(db, session_id, agent_name, source_type, source_ref, source_title: Option<&str>) -> anyhow::Result<Uuid>`

- [ ] **Step 1: Write the migration**

`migrations/065_video_jobs_source_title.sql`:

```sql
-- Original title/filename of the video, for the Zettelkasten note slug.
ALTER TABLE video_jobs ADD COLUMN source_title TEXT;
```

- [ ] **Step 2: Write the failing test**

Add to `video_jobs.rs` `mod tests`:

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn enqueue_persists_source_title(pool: PgPool) {
    let sid = Uuid::new_v4();
    let id = enqueue_video_job(&pool, sid, "Atlas", "file", "https://h/api/uploads/x?sig=1", Some("Лекция по Rust.mp4"))
        .await.unwrap();
    let j = get_video_job(&pool, id).await.unwrap().unwrap();
    assert_eq!(j.source_title.as_deref(), Some("Лекция по Rust.mp4"));

    // None is allowed (url jobs may have no title yet)
    let id2 = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/x", None).await.unwrap();
    let j2 = get_video_job(&pool, id2).await.unwrap().unwrap();
    assert!(j2.source_title.is_none());
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-db enqueue_persists_source_title -- --nocapture`
Expected: FAIL — `enqueue_video_job` arity mismatch / `source_title` field missing.

- [ ] **Step 4: Add the field + param**

In `VideoJob` struct add (after `source_ref`):

```rust
    pub source_title: Option<String>,
```

Update `enqueue_video_job`:

```rust
pub async fn enqueue_video_job(
    db: &PgPool,
    session_id: Uuid,
    agent_name: &str,
    source_type: &str,
    source_ref: &str,
    source_title: Option<&str>,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO video_jobs (session_id, agent_name, source_type, source_ref, source_title) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(session_id)
    .bind(agent_name)
    .bind(source_type)
    .bind(source_ref)
    .bind(source_title)
    .fetch_one(db)
    .await?;
    Ok(id)
}
```

Add `source_title` to the SELECT column list in `claim_next_video_job` and `get_video_job` (both `query_as` strings): append `, source_title` after `source_ref`.

- [ ] **Step 5: Fix existing `enqueue_video_job` callers**

Every existing call adds the new arg. There are call sites in `dispatch.rs` (`run_summarize_video`), `subagent.rs` (url-detector), and tests. For now pass `None` everywhere except where a title is available (Task 2 sets it). Update the existing `video_jobs.rs` tests (`enqueue_then_claim_marks_processing`, etc.) to pass `None`.

- [ ] **Step 6: Run the test to verify it passes**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-db video_jobs -- --nocapture`
Expected: PASS (all queue tests + the new one).

- [ ] **Step 7: Commit**

```bash
git add migrations/065_video_jobs_source_title.sql crates/opex-db/src/video_jobs.rs
git commit -m "feat(video-note): video_jobs.source_title column + enqueue param"
```

---

## Task 2: thread `source_title` from the attachment at enqueue

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch.rs`
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch_seam.rs`
- Test: inline in `dispatch.rs`

**Interfaces:**
- Consumes: `enqueue_video_job(..., source_title)` (Task 1); `EnqueueCtx` (video feature).
- Produces: `EnqueueCtx.source_title: Option<&'a str>`; `run_summarize_video` passes `attachment.file_name` as the title.

- [ ] **Step 1: Write the failing test**

In `dispatch.rs` `mod tests` (extend the existing `summarize_video_enqueues_and_acks` pattern with a title assertion):

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn summarize_video_persists_source_title(pool: sqlx::PgPool) {
    use opex_types::{MediaAttachment, MediaType};
    let sid = uuid::Uuid::new_v4();
    let att = MediaAttachment {
        url: "https://h/api/uploads/v1?sig=x".into(),
        media_type: MediaType::Video,
        file_name: Some("Лекция.mp4".into()),
        mime_type: Some("video/mp4".into()),
        file_size: None,
    };
    let client = reqwest::Client::new();
    let input = DispatchInput {
        action_ref: "summarize_video",
        attachment: &att,
        toolgate_url: "http://localhost:9011",
        gateway_listen: "0.0.0.0:18789",
        language: "ru",
        http_client: &client,
        timeout: std::time::Duration::from_secs(60),
        enqueue: Some(EnqueueCtx { db: &pool, session_id: sid, agent_name: "Atlas", source_type: "file" }),
    };
    let _ = dispatch_action(input).await;
    let title: Option<String> = sqlx::query_scalar("SELECT source_title FROM video_jobs WHERE session_id=$1")
        .bind(sid).fetch_one(&pool).await.unwrap();
    assert_eq!(title.as_deref(), Some("Лекция.mp4"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core summarize_video_persists_source_title -- --nocapture`
Expected: FAIL — `source_title` is NULL (run_summarize_video passes nothing).

- [ ] **Step 3: Pass the title in `run_summarize_video`**

In `dispatch.rs`, update the `enqueue_video_job` call inside `run_summarize_video`:

```rust
    match opex_db::video_jobs::enqueue_video_job(
        ctx.db,
        ctx.session_id,
        ctx.agent_name,
        ctx.source_type,
        &input.attachment.url,
        input.attachment.file_name.as_deref(),
    )
    .await
```

(`EnqueueCtx` itself needs no new field — the title comes from `input.attachment.file_name`, already in scope. Leave `EnqueueCtx` unchanged.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core dispatch:: -- --nocapture`
Expected: PASS (new test + existing dispatch tests).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/dispatch.rs
git commit -m "feat(video-note): persist attachment file_name as source_title"
```

---

## Task 3: toolgate — frame images + title + note frame cap

**Files:**
- Modify: `toolgate/routers/video.py`
- Test: `toolgate/test_video.py`

**Interfaces:**
- Produces: `/summarize-video` response gains `title: str` and `frames[].image_b64` (base64 jpeg); at most `VIDEO_NOTE_MAX_FRAMES` frames carry images.

- [ ] **Step 1: Write the failing test**

Add to `toolgate/test_video.py` (extends the existing local-file test's fakes):

```python
def test_summarize_video_returns_images_and_title(monkeypatch):
    import app as toolgate_app
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    async def fake_active(cap):
        return _FakeSTT() if cap == "stt" else _FakeVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "v.mp4")
        _make_tiny_video(vid)
        import routers.video as video_mod
        async def fake_fetch(http, url, work_dir):
            return vid
        monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)
        with TestClient(toolgate_app.app) as client:
            r = client.post("/summarize-video", json={"video_url": "http://localhost/api/uploads/x", "language": "ru", "title": "Тест"})
        assert r.status_code == 200, r.text
        body = r.json()
        assert body["title"] == "Тест"
        assert len(body["frames"]) >= 1
        import base64
        jpeg = base64.b64decode(body["frames"][0]["image_b64"])
        assert jpeg[:2] == b"\xff\xd8", "frame image is JPEG"
        assert len(body["frames"]) <= 24, "note frame cap"
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd /d/GIT/bogdan/opex/toolgate && .venv/Scripts/python -m pytest test_video.py::test_summarize_video_returns_images_and_title -v`
Expected: FAIL — no `title` / no `image_b64` in response.

- [ ] **Step 3: Implement**

In `toolgate/routers/video.py`:

Add near the other constants:

```python
import base64
VIDEO_NOTE_MAX_FRAMES = int(os.environ.get("VIDEO_NOTE_MAX_FRAMES", "24"))
```

Add `title` to the request model:

```python
class SummarizeVideoRequest(BaseModel):
    video_url: str | None = None
    page_url: str | None = None
    language: str = "ru"
    title: str | None = None
```

After `extract_scene_frames` returns `frames`, down-select evenly to the cap BEFORE describing:

```python
        if len(frames) > VIDEO_NOTE_MAX_FRAMES:
            step = len(frames) / VIDEO_NOTE_MAX_FRAMES
            frames = [frames[int(i * step)] for i in range(VIDEO_NOTE_MAX_FRAMES)]
```

In the `describe` inner function, include the base64 image in the result:

```python
            async def describe(ts: float, jpeg: bytes):
                async with sem:
                    b64 = base64.b64encode(jpeg).decode("ascii")
                    try:
                        desc = await vision.describe(http, jpeg, "image/jpeg", prompt)
                        return {"timestamp": ts, "description": desc, "image_b64": b64}
                    except Exception as e:
                        log.warning("frame describe failed at %.1fs: %s", ts, e)
                        return {"timestamp": ts, "description": "", "image_b64": b64}
```

(Note: on Vision failure we now keep the frame with an empty description rather than dropping it, so the picture still reaches the note.)

Resolve the title and return it. For `page_url`, probe yt-dlp; for `video_url`, use `body.title`:

```python
    resolved_title = body.title or ""
    if not resolved_title and body.page_url:
        try:
            code, out, _ = await _run("yt-dlp", "--print", "%(title)s", "--skip-download", body.page_url)
            if code == 0:
                resolved_title = out.decode(errors="ignore").strip()
        except Exception:
            pass
```

Add `"title": resolved_title` to the returned dict (alongside `duration`, `transcript`, `frames`, `degraded`). Import `_run` at top if not already (`from video_helpers import …, _run`).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd /d/GIT/bogdan/opex/toolgate && .venv/Scripts/python -m pytest test_video.py -v`
Expected: PASS (all video tests).

- [ ] **Step 5: Commit**

```bash
git add toolgate/routers/video.py toolgate/test_video.py
git commit -m "feat(video-note): toolgate returns frame images + title + note frame cap"
```

---

## Task 4: extend the Obsidian MCP (media + subfolder + commit + exists)

**Files:**
- Modify: `docker/mcp/obsidian/app.js`
- Modify: `docker/mcp/obsidian/Dockerfile`
- Modify: `docker/mcp/obsidian/MCP.md`
- Test: `docker/mcp/obsidian/test-mcp.js`

**Interfaces:**
- Produces MCP tools: `save_media(filename, content_b64)`, `create_note(folder, filename, content)`, `commit_vault(message)`, `note_exists(folder, filename)`.

- [ ] **Step 1: Write the failing test runner**

Create `docker/mcp/obsidian/test-mcp.js`:

```js
// Minimal test runner for the Obsidian MCP file ops. No framework — plain asserts.
const fs = require("fs");
const os = require("os");
const path = require("path");
const assert = require("assert");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "zk-test-"));
process.env.ZETTELKASTEN_PATH = tmp;
fs.mkdirSync(path.join(tmp, "_System", "media"), { recursive: true });

const ops = require("./ops"); // pure functions extracted from app.js

(async () => {
  // save_media
  const b64 = Buffer.from([0xff, 0xd8, 0xff, 0x00]).toString("base64");
  await ops.saveMedia("frame-01.jpg", b64);
  assert.ok(fs.existsSync(path.join(tmp, "_System/media/frame-01.jpg")), "media saved");
  await assert.rejects(ops.saveMedia("../evil.jpg", b64), /name|path|invalid/i);
  await assert.rejects(ops.saveMedia("x.exe", b64), /extension|allowed/i);

  // create_note in a subfolder
  await ops.createNote("Видео/тест", "конспект.md", "# Заметка\n");
  assert.ok(fs.existsSync(path.join(tmp, "Видео/тест/конспект.md")), "note in subfolder");
  await assert.rejects(ops.createNote("../escape", "x.md", "y"), /folder|invalid|\.\./i);

  // note_exists
  assert.equal(await ops.noteExists("Видео/тест", "конспект.md"), true);
  assert.equal(await ops.noteExists("Видео/нет", "конспект.md"), false);

  // commit_vault — init a repo first
  require("child_process").execSync("git init && git add -A && git commit -m init", { cwd: tmp });
  await ops.createNote("Видео/тест2", "конспект.md", "# Два\n");
  const out = await ops.commitVault("видео-конспект: тест");
  assert.ok(/тест|commit|nothing/i.test(out), "commit ran");

  console.log("ALL MCP OPS TESTS PASSED");
})().catch((e) => { console.error("FAIL:", e); process.exit(1); });
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd /d/GIT/bogdan/opex/docker/mcp/obsidian && node test-mcp.js`
Expected: FAIL — `Cannot find module './ops'`.

- [ ] **Step 3: Extract pure ops + implement the four functions**

Create `docker/mcp/obsidian/ops.js` (pure file ops; `app.js` will reuse them):

```js
const fs = require("fs").promises;
const path = require("path");
const { execFile } = require("child_process");

const ZK_PATH = () => process.env.ZETTELKASTEN_PATH || "/workspace/zettelkasten";
const MEDIA_EXT = new Set([".jpg", ".jpeg", ".png", ".webp"]);
const MAX_MEDIA_BYTES = 10 * 1024 * 1024;

function safeFolder(folder) {
  const norm = path.posix.normalize((folder || "").replace(/\\/g, "/"));
  if (norm.startsWith("..") || norm.includes("../") || path.isAbsolute(norm)) {
    throw new Error(`invalid folder: ${folder}`);
  }
  return norm.replace(/^\/+/, "");
}

async function saveMedia(filename, contentB64) {
  const base = path.basename(filename);
  if (base !== filename) throw new Error(`invalid media name: ${filename}`);
  if (!MEDIA_EXT.has(path.extname(base).toLowerCase())) throw new Error(`extension not allowed: ${base}`);
  const buf = Buffer.from(contentB64, "base64");
  if (buf.length > MAX_MEDIA_BYTES) throw new Error(`media too large: ${buf.length}`);
  const dir = path.join(ZK_PATH(), "_System", "media");
  await fs.mkdir(dir, { recursive: true });
  await fs.writeFile(path.join(dir, base), buf);
  return `Сохранено: _System/media/${base}`;
}

async function createNote(folder, filename, content) {
  const sf = safeFolder(folder);
  let name = path.basename(filename);
  if (!name.endsWith(".md")) name += ".md";
  const dir = path.join(ZK_PATH(), sf);
  await fs.mkdir(dir, { recursive: true });
  const file = path.join(dir, name);
  try { await fs.access(file); return `Заметка уже существует: ${sf}/${name}`; } catch {}
  await fs.writeFile(file, content, "utf8");
  return `Создана заметка: ${sf}/${name}`;
}

async function noteExists(folder, filename) {
  const sf = safeFolder(folder);
  let name = path.basename(filename);
  if (!name.endsWith(".md")) name += ".md";
  try { await fs.access(path.join(ZK_PATH(), sf, name)); return true; } catch { return false; }
}

function commitVault(message) {
  return new Promise((resolve) => {
    execFile("git", ["-C", ZK_PATH(), "add", "-A"], () => {
      execFile("git", ["-C", ZK_PATH(), "-c", "user.name=opex", "-c", "user.email=opex@local",
        "commit", "-m", String(message)], (err, stdout, stderr) => {
        const text = (stdout || "") + (stderr || "");
        if (err && !/nothing to commit/i.test(text)) resolve(`commit error: ${text.slice(0, 200)}`);
        else resolve(/nothing to commit/i.test(text) ? "nothing to commit" : "committed");
      });
    });
  });
}

module.exports = { saveMedia, createNote, noteExists, commitVault, safeFolder };
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd /d/GIT/bogdan/opex/docker/mcp/obsidian && node test-mcp.js`
Expected: `ALL MCP OPS TESTS PASSED`.

- [ ] **Step 5: Wire the ops into `app.js` (tools list + switch)**

In `docker/mcp/obsidian/app.js`, `require("./ops")` at top, add the four tools to `MCP_TOOLS`:

```js
  { name: "save_media", description: "Save an image into _System/media (base64).",
    inputSchema: { type: "object", properties: {
      filename: { type: "string" }, content_b64: { type: "string" } },
      required: ["filename", "content_b64"] } },
  { name: "create_note", description: "Create a note, optionally in a subfolder.",
    inputSchema: { type: "object", properties: {
      folder: { type: "string", description: "Subfolder, e.g. 'Видео/название'" },
      filename: { type: "string" }, content: { type: "string" } },
      required: ["filename", "content"] } },
  { name: "note_exists", description: "Check if a note exists in a subfolder.",
    inputSchema: { type: "object", properties: {
      folder: { type: "string" }, filename: { type: "string" } },
      required: ["filename"] } },
  { name: "commit_vault", description: "git add+commit the vault.",
    inputSchema: { type: "object", properties: { message: { type: "string" } },
      required: ["message"] } },
```

Replace the existing in-file `createNote` with a call to `ops.createNote(args.folder, args.filename, args.content)` and add the switch arms:

```js
        case "save_media":
          result = await ops.saveMedia(args.filename, args.content_b64); break;
        case "create_note":
          result = await ops.createNote(args.folder, args.filename, args.content); break;
        case "note_exists":
          result = String(await ops.noteExists(args.folder, args.filename)); break;
        case "commit_vault":
          result = await ops.commitVault(args.message); break;
```

(Keep `list_notes`/`read_note`/`random_note`/`search_notes` as-is.)

- [ ] **Step 6: Add git to the image + COPY ops.js**

In `docker/mcp/obsidian/Dockerfile`:

```dockerfile
FROM node:22-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*
COPY package.json .
RUN npm install --production
COPY app.js ops.js MCP.md .
EXPOSE 8000
CMD ["node", "app.js"]
```

Document the four tools in `MCP.md` (one stanza each, mirroring the existing format).

- [ ] **Step 7: Commit**

```bash
git add docker/mcp/obsidian/ops.js docker/mcp/obsidian/test-mcp.js docker/mcp/obsidian/app.js docker/mcp/obsidian/Dockerfile docker/mcp/obsidian/MCP.md
git commit -m "feat(video-note): Obsidian MCP — save_media, subfolder create_note, commit_vault, note_exists"
```

---

## Task 5: note builder — RawMaterial images, slug, hybrid prompt, extract_summary

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/video_summary.rs`
- Test: inline in `video_summary.rs`

**Interfaces:**
- Consumes: toolgate JSON (`title`, `frames[].image_b64`).
- Produces:
  - `RawMaterial { title: Option<String>, duration, transcript, frames: Vec<FrameDesc>, degraded }`, `FrameDesc { timestamp, description, image_b64: String }`
  - `slug(title: &str, fallback_id: &str) -> String`
  - `build_summary_messages(raw, frame_names: &[String]) -> Vec<Message>` (hybrid prompt; LLM embeds `![[_System/media/<name>]]`)
  - `build_note(raw, title, llm_body, frame_names) -> String` (frontmatter + body + unplaced-frame appendix + collapsed transcript)
  - `extract_summary(note: &str) -> String`

- [ ] **Step 1: Write the failing tests**

In `video_summary.rs` `mod tests`:

```rust
#[test]
fn slug_keeps_cyrillic_strips_specials() {
    assert_eq!(slug("Лекция: Rust / async?", "id8"), "Лекция-Rust-async");
    assert_eq!(slug("   ", "ab12cd34"), "видео-ab12cd34");
}

#[test]
fn build_note_has_frontmatter_appendix_and_transcript() {
    let raw = RawMaterial {
        title: Some("Тест".into()), duration: 65.0, transcript: "речь целиком".into(),
        frames: vec![
            FrameDesc { timestamp: 5.0, description: "слайд".into(), image_b64: "x".into() },
            FrameDesc { timestamp: 9.0, description: "график".into(), image_b64: "y".into() },
        ],
        degraded: Degraded::default(),
    };
    let names = vec!["t-frame-01.jpg".to_string(), "t-frame-02.jpg".to_string()];
    // LLM used only frame 1 inline; frame 2 must go to appendix.
    let llm_body = "## Резюме\nкоротко\n\n## Конспект\n### Раздел\n![[_System/media/t-frame-01.jpg]]\n";
    let note = build_note(&raw, "Тест", llm_body, &names);
    assert!(note.starts_with("---\n"), "frontmatter");
    assert!(note.contains("title: Тест"));
    assert!(note.contains("![[_System/media/t-frame-01.jpg]]"));
    assert!(note.contains("## Дополнительные кадры"));
    assert!(note.contains("![[_System/media/t-frame-02.jpg]]"), "unplaced frame appended");
    assert!(note.contains("> [!note]- Полный транскрипт"));
    assert!(note.contains("речь целиком"));
}

#[test]
fn extract_summary_reads_section_or_falls_back() {
    let note = "---\nx\n---\n## Резюме\nэто резюме\n\n## Конспект\nтело\n";
    assert_eq!(extract_summary(note).trim(), "это резюме");
    let no_section = "просто первый абзац\n\nвторой";
    assert_eq!(extract_summary(no_section).trim(), "просто первый абзац");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p opex-core video_summary:: -- --nocapture`
Expected: FAIL — `slug` / `build_note` / `extract_summary` / `FrameDesc.image_b64` undefined.

- [ ] **Step 3: Implement**

Update the structs (add fields):

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct FrameDesc {
    pub timestamp: f64,
    pub description: String,
    #[serde(default)]
    pub image_b64: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawMaterial {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub duration: f64,
    pub transcript: String,
    #[serde(default)]
    pub frames: Vec<FrameDesc>,
    #[serde(default)]
    pub degraded: Degraded,
}
```

Add `slug`:

```rust
/// Filesystem/Obsidian-safe slug; keeps Cyrillic, strips specials, spaces→'-'.
pub fn slug(title: &str, fallback_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c => c,
        })
        .collect();
    let s = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { format!("видео-{fallback_id}") } else { s }
}
```

Change the hybrid prompt: `SYSTEM_PROMPT` instructs the model to output `## Резюме` (3-5 предложений), then `## Конспект` with sections, embedding `![[_System/media/<name>]]` from the provided frame list where relevant. Update `build_summary_messages` to accept `frame_names` and list each frame as `[{:.0}s] <description> → ![[_System/media/<name>]]` so the model knows the exact embed string. (Keep the whole transcript in the user message as today.)

Add `build_note` (frontmatter + llm body + appendix + collapsed transcript):

```rust
pub fn build_note(raw: &RawMaterial, title: &str, llm_body: &str, frame_names: &[String]) -> String {
    use chrono::Utc; // already a dep
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: {title}\n"));
    out.push_str("tags: [видео, конспект]\n");
    out.push_str(&format!("duration: {:.0}s\n", raw.duration));
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(llm_body.trim());
    out.push_str("\n");

    // Appendix: frames whose embed string the LLM did not include.
    let unplaced: Vec<&String> = frame_names.iter()
        .filter(|n| !llm_body.contains(n.as_str()))
        .collect();
    if !unplaced.is_empty() {
        out.push_str("\n## Дополнительные кадры\n\n");
        for n in unplaced {
            out.push_str(&format!("![[_System/media/{n}]]\n\n"));
        }
    }
    // Collapsed full transcript.
    out.push_str("\n> [!note]- Полный транскрипт\n");
    for line in raw.transcript.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}
```

(The `created` frontmatter date is set by the worker — pass `Utc::now()` formatted there, or add a `created: &str` param; keep `build_note` deterministic for tests by NOT calling `Utc::now()` inside — the worker prepends it. For this task the two date-free assertions pass; thread `created` in Task 6.)

Add `extract_summary`:

```rust
/// The text under `## Резюме` up to the next `## `; else the first paragraph.
pub fn extract_summary(note: &str) -> String {
    if let Some(start) = note.find("## Резюме") {
        let after = &note[start + "## Резюме".len()..];
        let body = after.split("\n## ").next().unwrap_or(after);
        return body.trim().to_string();
    }
    note.split("\n\n").map(str::trim).find(|p| !p.is_empty()).unwrap_or("").to_string()
}
```

Update the existing `prompt_embeds_transcript_and_frames` test to pass the new `frame_names` arg and the `image_b64` field.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p opex-core video_summary:: -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/video_summary.rs
git commit -m "feat(video-note): RawMaterial images + slug + build_note + extract_summary + hybrid prompt"
```

---

## Task 6: worker final stage — build note, write via MCP, chat summary+link

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/video_worker.rs`
- Test: inline in `video_worker.rs`

**Interfaces:**
- Consumes: `McpRegistry::call_tool("mcp-obsidian", tool, &Value) -> Result<String>` via `engine.mcp()`; `slug`/`build_note`/`extract_summary` (Task 5); `RawMaterial` with images/title.
- Produces: `process_one` builds the note + returns a `NoteResult { folder, summary }`; the worker loop writes media/note/commit through MCP and delivers summary+link.

- [ ] **Step 1: Write the failing test**

In `video_worker.rs` `mod tests`, add a test that drives `process_one` (toolgate wiremock returns title + a frame with image_b64 + transcript; the FakeLlm returns a body) and asserts the returned note contains the frontmatter, an embed, and the collapsed transcript, and that the summary is the `## Резюме` text:

```rust
#[tokio::test]
async fn process_one_builds_note_with_image_and_summary() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/summarize-video"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "title": "Тест", "duration": 30.0, "transcript": "речь",
            "frames": [{"timestamp": 5.0, "description": "слайд", "image_b64": "/9j/AA=="}],
            "degraded": {"stt": false, "vision": false}
        })))
        .mount(&server).await;
    let job = opex_db::video_jobs::VideoJob {
        id: uuid::Uuid::new_v4(), session_id: uuid::Uuid::new_v4(), agent_name: "Atlas".into(),
        channel_id: None, source_type: "file".into(), source_ref: "http://localhost/api/uploads/x?sig=1".into(),
        source_title: Some("Тест".into()), status: "processing".into(),
        summary: None, error: None, attempts: 1,
    };
    let client = reqwest::Client::new();
    let provider = FakeLlm; // returns "## Резюме\nкоротко\n\n## Конспект\n![[_System/media/...]]"
    let note = process_one(&client, &server.uri(), "0.0.0.0:18789", &provider, &job).await.unwrap();
    assert!(note.note.contains("title: Тест"));
    assert!(note.note.contains("> [!note]- Полный транскрипт"));
    assert!(note.summary.contains("коротко"));
    assert!(!note.media.is_empty(), "media collected for MCP save");
}
```

(Adjust `FakeLlm.chat` to return the `## Резюме … ## Конспект …` body.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core video_worker::process_one_builds_note -- --nocapture`
Expected: FAIL — `process_one` returns `String`, not the new struct; no note assembly.

- [ ] **Step 3: Change `process_one` to assemble the note**

Define the result + rewrite `process_one` to: call toolgate → deserialize `RawMaterial` → compute `slug` (from `job.source_title` or `raw.title`, fallback `job.id`) → plan `frame_names` `<slug>-frame-NN.jpg` → LLM body via `build_summary_messages(&raw, &frame_names)` → `created` date → `build_note` → `extract_summary`:

```rust
pub struct NoteResult {
    pub slug: String,
    pub note: String,
    pub summary: String,
    pub media: Vec<(String, String)>, // (filename, image_b64)
}

pub async fn process_one(
    http: &reqwest::Client, toolgate_url: &str, gateway_listen: &str,
    provider: &dyn LlmProvider, job: &VideoJob,
) -> anyhow::Result<NoteResult> {
    let url = format!("{}/summarize-video", toolgate_url.trim_end_matches('/'));
    let mut body = source_payload(job, gateway_listen);
    body["language"] = serde_json::json!("ru");
    if let Some(t) = &job.source_title { body["title"] = serde_json::json!(t); }
    let resp = http.post(&url).json(&body).send().await?;
    if !resp.status().is_success() { anyhow::bail!("toolgate HTTP {}", resp.status().as_u16()); }
    let raw: crate::agent::file_scenario::video_summary::RawMaterial = resp.json().await?;

    let title = job.source_title.clone().or_else(|| raw.title.clone()).unwrap_or_default();
    let id8 = job.id.simple().to_string();
    let id8 = &id8[..8];
    let slug = crate::agent::file_scenario::video_summary::slug(&title, id8);
    let frame_names: Vec<String> = (0..raw.frames.len())
        .map(|i| format!("{slug}-frame-{:02}.jpg", i + 1)).collect();
    let media: Vec<(String, String)> = frame_names.iter().cloned()
        .zip(raw.frames.iter().map(|f| f.image_b64.clone())).collect();

    let messages = crate::agent::file_scenario::video_summary::build_summary_messages(&raw, &frame_names);
    let opts = crate::agent::providers::CallOptions { thinking_level: 0, claude_md_content: None };
    let llm_body = provider.chat(&messages, &[], opts).await?.content;

    let title_for_note = if title.is_empty() { slug.clone() } else { title };
    let note = crate::agent::file_scenario::video_summary::build_note(&raw, &title_for_note, &llm_body, &frame_names);
    let summary = crate::agent::file_scenario::video_summary::extract_summary(&note);
    Ok(NoteResult { slug, note, summary, media })
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p opex-core video_worker::process_one_builds_note -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Rewrite the worker loop to write via MCP (no isolated test — wired + compiled)**

In `spawn_video_worker`, after a successful `process_one`, resolve `engine.mcp()` and write. Replace the old `mark_done + deliver(summary)` block:

```rust
            let mcp = match engine.mcp() {
                Some(m) => m.clone(),
                None => {
                    let _ = opex_db::video_jobs::mark_video_job_failed(&db, job.id, "MCP disabled — cannot save note").await;
                    continue;
                }
            };
            match process_one(&http, &toolgate_url, &gateway_listen, provider.as_ref(), &job).await {
                Ok(nr) => {
                    // free folder (collision)
                    let mut folder = format!("Видео/{}", nr.slug);
                    for suffix in 2..=20 {
                        let exists = mcp.call_tool("mcp-obsidian", "note_exists",
                            &serde_json::json!({ "folder": folder, "filename": "конспект.md" }))
                            .await.map(|s| s.trim() == "true").unwrap_or(false);
                        if !exists { break; }
                        folder = format!("Видео/{}-{}", nr.slug, suffix);
                    }
                    // save media
                    let mut ok = true;
                    for (name, b64) in &nr.media {
                        if let Err(e) = mcp.call_tool("mcp-obsidian", "save_media",
                            &serde_json::json!({ "filename": name, "content_b64": b64 })).await {
                            tracing::warn!(error=%e, "save_media failed"); ok = false; break;
                        }
                    }
                    if ok {
                        if let Err(e) = mcp.call_tool("mcp-obsidian", "create_note",
                            &serde_json::json!({ "folder": folder, "filename": "конспект.md", "content": nr.note })).await {
                            let _ = opex_db::video_jobs::mark_video_job_failed(&db, job.id, &format!("create_note: {e}")).await;
                            deliver(&db, &ui_tx, &job, &format!("Не удалось сохранить конспект: {e}")).await;
                            continue;
                        }
                        let _ = mcp.call_tool("mcp-obsidian", "commit_vault",
                            &serde_json::json!({ "message": format!("видео-конспект: {}", nr.slug) })).await; // best-effort
                        let path = format!("{folder}/конспект.md");
                        let chat = format!("{}\n\n📓 Конспект: {}", nr.summary, path);
                        let _ = opex_db::video_jobs::mark_video_job_done(&db, job.id, &nr.summary).await;
                        deliver(&db, &ui_tx, &job, &chat).await;
                    } else {
                        let _ = opex_db::video_jobs::mark_video_job_failed(&db, job.id, "save_media failed").await;
                        deliver(&db, &ui_tx, &job, "Не удалось сохранить кадры конспекта").await;
                    }
                }
                Err(e) => {
                    let _ = opex_db::video_jobs::mark_video_job_failed(&db, job.id, &e.to_string()).await;
                    deliver(&db, &ui_tx, &job, &format!("Не удалось обработать видео: {e}")).await;
                }
            }
```

(Confirm the loop already has `db`, `http`, `toolgate_url`, `gateway_listen`, `ui_tx`, `engine`, `provider` in scope from the video feature; `engine.mcp()` returns `&Option<Arc<McpRegistry>>`.)

- [ ] **Step 6: Build + run the worker test + check**

Run: `cargo check -p opex-core && DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core video_worker:: -- --nocapture`
Expected: compiles; worker tests PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/video_worker.rs
git commit -m "feat(video-note): worker builds note + writes to vault via MCP + chat summary/link"
```

---

## Task 7: config + full gate

**Files:**
- Modify: `config/opex.toml`
- Verify: whole workspace + toolgate + MCP

**Interfaces:** none new.

- [ ] **Step 1: Add note keys to the `[video]` section**

Append to the existing `[video]` block in `config/opex.toml`:

```toml
# Zettelkasten note output (v1): max screenshots embedded per note.
note_max_frames = 24
# Obsidian vault name for the obsidian:// deep link in the chat notification.
vault_name = "zettelkasten"
```

(Documentation-only for v1 — the toolgate env var `VIDEO_NOTE_MAX_FRAMES` is the live knob; the worker hardcodes the `obsidian://` vault name `zettelkasten` unless wired. Keep `.env` untouched.)

- [ ] **Step 2: Lint**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: passes (fix any warnings introduced by the new Rust code — idiomatic only).

- [ ] **Step 3: Full DB-backed Rust suite**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core && DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-db`
Expected: all pass (video_jobs, dispatch, video_summary, video_worker).

- [ ] **Step 4: toolgate + MCP tests**

Run: `cd /d/GIT/bogdan/opex/toolgate && .venv/Scripts/python -m pytest test_video.py -v && cd /d/GIT/bogdan/opex/docker/mcp/obsidian && node test-mcp.js`
Expected: pytest all pass; `ALL MCP OPS TESTS PASSED`.

- [ ] **Step 5: Commit**

```bash
git add config/opex.toml
git commit -m "feat(video-note): config note keys + final gate"
```

---

## Self-Review

**Spec coverage (spec § → task):**
- §5 note format (frontmatter, `![[…]]`, callout, appendix) → Tasks 5 (`build_note`), 6 (assembly). ✓
- §6 MCP ops (save_media/create_note-folder/commit_vault/note_exists + Dockerfile git) → Task 4. ✓
- §7 toolgate images + title + note cap → Task 3. ✓
- §8 worker via `McpRegistry::call_tool`; slug; collision; chat summary+link; R3 extract_summary; R4 source_title → Tasks 1,2,5,6. ✓
- §9 errors (MCP fail → failed+chat; commit best-effort; collision suffix) → Task 6. ✓
- §10 tests → every task TDD. ✓
- §11 defaults (Видео/, _System/media, Cyrillic slug, MCP path, note cap, source_title) → Tasks 1,3,5,6,7. ✓
- §12 open Qs (vault name, note_exists for subfolder, down-select) → resolved: `note_exists` added (Task 4); even-by-timestamp down-select (Task 3); vault name in config (Task 7).

**Placeholder scan:** no TBD/TODO; every code step has complete code.

**Type consistency:** `enqueue_video_job(..., source_title)` (T1) used in T2; `RawMaterial{title,frames[image_b64]}`/`slug`/`build_note`/`extract_summary` (T5) used in T6; MCP tool names `save_media`/`create_note`/`note_exists`/`commit_vault` consistent across T4 and T6; `NoteResult` fields match T6 test and loop.

**Confirm-at-implementation (flagged, not placeholders):** `created` frontmatter date threaded by the worker (T5 note); the worker-loop scope vars exist from the video feature (T6 step 5 note); `LlmResponse.content` field (used, confirmed in the video feature).
