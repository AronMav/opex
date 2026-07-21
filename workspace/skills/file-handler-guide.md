---
name: file-handler-guide
description: Complete reference for creating toolgate FILE HANDLERS — the model-driven buttons that process an uploaded file or a link (transcribe, describe, summarize, extract, save). Covers the XML descriptor, the run(ctx, file, params) function, the ctx API, sync vs async, matching, valves, and post_action note-writing.
triggers:
  - create file handler
  - new file handler
  - file handler
  - handler descriptor
  - process a file
  - process a link
  - transcribe handler
  - summarize video
  - describe image
  - toolgate handler
tools_required:
  - workspace_read
  - workspace_write
priority: 5
state: active
---

# Creating File Handlers

A **file handler** is a small self-describing Python module that processes ONE
uploaded file or ONE link and returns a result. Handlers are what power the
"choose an action" menu the user sees after they send a video/image/document/
link (transcribe, summarize video, describe, extract text, save…). They are the
CURRENT mechanism — the old File Scenario Engine is retired.

## Where handlers live & how they load

- **Builtin** (system, read-only reference): `toolgate/handlers/builtin/*.py` —
  `describe`, `transcribe`, `summarize_video`, `extract_document`, `save`.
- **Yours** (agent-authored): drop a `.py` file into
  **`workspace/file_handlers/`**. Toolgate hot-reloads it (watchfiles) within
  ~1s — no restart. Read the builtins first; copy the closest one.
- Discovery: core does a conditional `GET /handlers` (ETag, ~30s) and, when the
  user sends a matching file/link, offers your handler as a button. Verify yours
  appears with the `file_handler` tool: `file_handler(action="list", …)`.

**Trust (v1):** workspace-tier handlers run in the toolgate process and are
allowed by default (trusted author). Builtin handlers are gated by the global
`fse.allowlist`. Keep handler code safe — it runs with toolgate's privileges.

## Anatomy — two parts in one file

### 1. The XML descriptor (a comment block at the top)

The descriptor is a `# <handler> … # </handler>` comment. It declares identity,
what the handler matches, and its parameters/valves. Full field set:

```python
# <handler>
#   <id>my_handler</id>                      <!-- unique, [a-z0-9_], = filename stem -->
#   <label lang="ru">Моя обработка</label>   <!-- button text; add each language -->
#   <label lang="en">My action</label>
#   <description lang="ru">Что делает</description>
#   <description lang="en">What it does</description>
#   <icon>file</icon>                        <!-- lucide-ish name: image, mic, file, video… -->
#   <match>
#     <mime>image/*</mime>                   <!-- one or more; globs allowed (audio/*, */*) -->
#     <max_size_mb>20</max_size_mb>
#     <domain>youtube.com</domain>           <!-- URL handlers: match link hosts (optional) -->
#   </match>
#   <capability>vision</capability>          <!-- optional hint: stt|vision|tts|imagegen|… -->
#   <execution>sync</execution>              <!-- sync (fast, inline) | async (long, queued) -->
#   <output>text</output>                    <!-- text | file | card -->
#   <params>                                 <!-- values the MODEL may pass at run time -->
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <config>                                 <!-- VALVES: per-agent settings the OPERATOR sets in UI -->
#     <field name="max_tokens" type="int" default="2000" label="Макс. токенов" description="…"/>
#   </config>
#   <order>10</order>                        <!-- button sort order -->
#   <enabled>true</enabled>
# </handler>
```

- **`params`** = runtime args the model chooses (e.g. `language`). Read via
  `params.get("language", "ru")`.
- **`config`** = OpenWebUI-style **valves**: per-agent settings an operator sets
  in the UI ("Agent settings"). Read via `ctx.config.get("max_tokens")`. Values
  arrive as strings — coerce them. ALWAYS fall back to your own default when a
  valve is empty/unset.

### 2. `async def run(ctx, file, params)`

Always `async def run(ctx, file, params)`. Return a `ctx.result.*`.

```python
async def run(ctx, file, params):
    language = params.get("language", "ru")
    text = await ctx.vision.describe(
        file.bytes, content_type=file.mime, prompt="Опиши изображение", max_tokens=2000
    )
    return ctx.result.text(text)
```

## The `file` object

- `file.bytes` — RAW bytes of the upload (already downloaded for you; NEVER fetch
  a loopback URL — toolgate hard-blocks loopback).
- `file.mime`, `file.filename`, `file.size`.
- `file.source_url` — for URL/link handlers (`execution=async`, matched by
  `<domain>`), the link; `file.bytes` is empty in that case. Download it yourself
  (see summarize_video / transcribe using `video_helpers.download_video`).

## The `ctx` API (the ONLY sanctioned surface)

Provider calls auto-resolve the active provider and inject the http client:

- `await ctx.stt.transcribe(audio_bytes, filename=…, language="ru")` → text
- `await ctx.vision.describe(img_bytes, content_type=…, prompt=…, max_tokens=2000)` → text
- `await ctx.tts.synthesize(text, voice=…, response_format="mp3")` → bytes
- `await ctx.imagegen.generate(prompt, size="1024x1024")` → bytes
- `await ctx.embed.embed([texts])` → list[list[float]]
- `await ctx.search.search(query, max_results=5)` → list[dict]
- `await ctx.llm.complete(messages, provider=None, model=None)` → text (raw LLM via core)
- `await ctx.http.get(url) / ctx.http.post(url, …)` — SSRF-safe client for EXTERNAL
  fetches (validates the URL, blocks private/loopback hosts). Use this for any
  URL derived from user/file input.
- `await ctx.progress(phase, pct)` — progress ticks for async jobs (`"fetch"/
  "transcribe"/"digest"/"saving"` are localized in the UI; a no-op in sync).
- `ctx.config` — the valves dict (see above). `ctx.log` — logger.

## Result — return exactly one

- `ctx.result.text("…")` — the common case (shown in chat / sent to the channel).
- `ctx.result.file(bytes, mime)` — return a binary artifact (image, audio).
- `ctx.result.card(card_type, data)` — a structured rich card.
- `ctx.result.failed("reason")` — a clean failure (also `.unsupported`, `.too_large`).

The wire shape core consumes is exactly 4 keys (status/summary_text/
artifact_urls/reason). Don't hand-build dicts — use `ctx.result.*`.

## sync vs async

- **sync**: fast, runs inline, result returns immediately. No `job_id` → `ctx.
  progress` is a no-op, and `ctx.llm` requires a job runner (available in async).
  Use for quick single-provider calls (describe an image, extract text).
- **async**: queued on `handler_jobs`, runs out-of-process, can post `ctx.progress`
  and take minutes. Use for downloads (URL/link handlers) and multi-step
  pipelines (summarize a video: download → STT → LLM digest → save). URL handlers
  MUST be async (only async handlers receive `source_url`).

### Writing a note/file when done (async `post_action`)

To persist output to the workspace (e.g. an Obsidian note) attach a `post_action`
to the result — the runner writes the file (no mcp-obsidian dependency):

```python
result = ctx.result.text(short_summary)
result.post_action = {
    "kind": "write_file",
    "dir": ctx.config.get("output_dir") or "",   # abs path, or "" → workspace/<vault>
    "subfolder": "Summary",
    "filename": f"{safe_slug}.md",                # ^[A-Za-z0-9 _.-]{1,128}$ (path-guarded by core)
    "content": full_markdown_note,
}
return result
```

## Checklist

1. Read the closest builtin in `toolgate/handlers/builtin/`.
2. `workspace_write` your handler to `workspace/file_handlers/<id>.py` with a
   descriptor + `async def run`.
3. Match precisely (`<mime>` / `<max_size_mb>` / `<domain>`); pick `sync` for fast,
   `async` for downloads/long work.
4. Expose operator knobs as `<config>` valves; read via `ctx.config.get(...)`
   with a fallback.
5. Verify with `file_handler(action="list", …)` on a matching file/link, then run
   it. Iterate — the file hot-reloads.
