# Video → Zettelkasten Notes — Design

- **Date:** 2026-06-26
- **Status:** Approved (brainstorm complete) — ready for implementation planning.
- **Related:**
  - [`2026-06-26-fse-video-summarization-design.md`](2026-06-26-fse-video-summarization-design.md) — the video-summary feature this extends. That feature delivers a short text summary into the chat session; THIS work changes the final stage to produce a full Obsidian note (with screenshots) in the Zettelkasten vault and post only a short summary + link to chat.
  - `D:\GIT\telesumbot\src\summary\generator.rs` — the reference for full-note formatting: transcript + inline `![Кадр N [MM:SS]](images/…)` screenshots, with an appendix for unplaced frames.
  - `docker/mcp/obsidian/app.js` — the Obsidian/Zettelkasten MCP server this extends with media + subfolder + commit operations.
  - Real vault sample: `~/opex/workspace/zettelkasten/Beatmaking/Конспекты/1 Этап.md` — confirmed format: `<Тема>/Конспекты/` subfolder, `## headings`, `![[_System/media/<name>.png]]` embeds; `.obsidian/app.json` has `attachmentFolderPath: "_System/media"`.

---

## 1. Problem

The shipped video-summary feature produces a **short text digest** delivered into the chat session. The user wants the telesumbot experience: a **full, richly-formatted Obsidian note** with **screenshots of key frames**, stored in the Zettelkasten vault — one note per video in its own folder, screenshots in the shared `_System/media/`, chat gets only a short summary + a link.

Current gaps (verified):
- toolgate `/summarize-video` extracts frames, gets Vision descriptions, and **discards the JPEGs** (`video.py`: `describe(ts, jpeg)` returns `{timestamp, description}` only). The pictures are lost.
- `FrameDesc { timestamp, description }` has no image data; the digest (`video_summary.rs`) is text-only.
- The Obsidian MCP (`app.js`) cannot help: `create_note` does `path.basename` (strips subfolders) and there is no media-write or commit operation.
- The worker delivers the digest as a session message; there is no vault write.

## 2. Goals / Non-goals

### Goals
- One Obsidian note per video at `Видео/<slug>/конспект.md`, screenshots in `_System/media/<slug>-frame-NN.jpg`.
- **Hybrid content:** LLM-written digest (Резюме + thesis sections with screenshots placed by timestamp) **followed by the full transcript** in a collapsed Obsidian callout.
- Reuse the running pipeline (ffmpeg → STT → Vision); only the final stage changes (return frame images + title; build note; write to vault; notify chat).
- Write to vault through an **extended Obsidian MCP** (`save_media`, subfolder `create_note`, `commit_vault`).
- Chat receives the `## Резюме` section + a link to the note.
- No screenshot lost: frames the LLM doesn't place inline go to a «Дополнительные кадры» appendix.

### Non-goals (deferred)
- Timestamp-segmented transcript / per-segment inline placement (telesumbot's word-timestamps). The hybrid uses a flat collapsed transcript, so STT segments are NOT needed (`/transcribe` keeps returning plain text).
- Agents creating rich notes via MCP (the new MCP ops make it possible, but wiring an agent tool is out of scope — only the worker uses them in v1).
- Non-video Zettelkasten note types.
- Telegram delivery (the video feature is web-only in v1; chat notification rides the same path).

## 3. Decisions (from brainstorm)
- **Content:** hybrid (chosen over full-transcript-inline and LLM-only).
- **Mechanism:** extend the Obsidian MCP (chosen over the worker writing the vault filesystem directly). The worker calls the MCP over HTTP; `commit_vault` is an MCP op since writes go through the container.
- **Chat:** short Резюме + link (not the full note, not silent).
- **Defaults baked in:** top-level subfolder `Видео/`; slug keeps Cyrillic; `commit_vault` is best-effort (never fails the job).

## 4. Architecture & data flow

```text
toolgate /summarize-video  (CHANGED: returns frame images + title)
  RawMaterial { title, duration, transcript,
                frames: [{ timestamp, description, image_b64 }] }
        ▼
in-core video worker  (CHANGED final stage)
  1. Build hybrid note via LLM (video_summary.rs):
       frontmatter + ## Резюме + ## Конспект (sections, ![[…]] by timestamp)
       + «Дополнительные кадры» for unplaced frames + collapsed full transcript
  2. Ensure mcp-obsidian container is up (services API), then over HTTP /mcp:
       save_media(<slug>-frame-NN.jpg, image_b64)  → _System/media/   (×N)
       create_note("Видео/<slug>", "конспект.md", markdown)
       commit_vault("видео-конспект: <title>")       (best-effort)
  3. Deliver to chat: «## Резюме» text + path + obsidian:// link
        ▼
  video_jobs.status = done (summary column stores the short Резюме)
```

**Separation:** toolgate = media (now also returns frame pictures), worker = orchestration + LLM digest + chat, MCP = vault writes (note + media + commit). No worker→vault filesystem coupling.

## 5. Note format (`конспект.md`)

```markdown
---
title: <Название видео>
source: <YouTube URL | имя файла>
created: <YYYY-MM-DD>
duration: <Nм Nс>
tags: [видео, конспект]
---

# <Название видео>

## Резюме
<3-5 предложений>

## Конспект
### <Раздел по смыслу/таймкоду>
<тезисы>
![[_System/media/<slug>-frame-03.jpg]]
<подпись к кадру>

### <Следующий раздел>
…

## Дополнительные кадры
![[_System/media/<slug>-frame-07.jpg]]

> [!note]- Полный транскрипт
> <весь транскрипт>
```

- **Frontmatter:** Obsidian YAML properties.
- **Images:** `![[_System/media/<file>]]` embed-wikilink (matches the vault's `attachmentFolderPath`).
- **Collapsed transcript:** `> [!note]-` callout (the `-` collapses it).
- **Frame safety:** the LLM is given each frame as `{timestamp, description, filename}` with the EXACT target filename and told to embed `![[_System/media/<filename>]]` where relevant. After generation the worker scans the produced markdown for each `filename`; any not present is appended under «Дополнительные кадры». No frame is dropped even if the LLM ignores some.

## 6. MCP extension (`docker/mcp/obsidian/app.js`)

Three new tools (added to `MCP_TOOLS` + the `tools/call` switch):

```text
save_media(filename, content_b64)
  dest = path.join(ZK_PATH, "_System/media", path.basename(filename))
  guard: extension ∈ {.jpg,.jpeg,.png,.webp}; decoded size ≤ cap (e.g. 10 MB)
  write decoded bytes; return saved path

create_note(folder, filename, content)     # folder is NEW, optional (BWC)
  safeFolder = normalized(folder) with NO ".." and staying inside ZK_PATH
  dir = path.join(ZK_PATH, safeFolder); mkdir -p
  file = path.join(dir, basename(filename) ending .md)
  refuse if exists (return message); else write; return path

commit_vault(message)
  spawn git with args (NOT shell): 
    git -C ZK_PATH add -A
    git -C ZK_PATH -c user.name=opex -c user.email=opex@local commit -m <message>
  treat "nothing to commit" as success; return short status
```

- **Security:** `save_media` and `create_note` reject path traversal (basename / normalized-folder inside vault), extension/size allowlist on media. `commit_vault` passes `message` as a git arg (argv, no shell) — no command injection.
- **Dockerfile:** `node:22-slim` lacks `git`; add `RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*`. The vault `.git` is already mounted via `../workspace`.
- **MCP.md:** document the three new tools.

## 7. toolgate changes (`routers/video.py`, `video_helpers.py`)

- In the frame loop, keep the JPEG bytes: each frame result becomes `{timestamp, description, image_b64}` where `image_b64 = base64(jpeg_bytes)`.
- Add `title` to the response: for `page_url`, `yt-dlp --print title --skip-download <url>` (or `--no-download --print "%(title)s"`); for `video_url` (upload), derive from the upload filename. Fall back to empty (worker uses a date-based slug).
- `FRAME_CEILING` already bounds frame count → bounds base64 payload size.

## 8. Worker changes (`video_worker.rs`, `video_summary.rs`)

- `RawMaterial`/`FrameDesc` gain `image_b64: String` (serde) and `RawMaterial.title: Option<String>`.
- `build_summary_messages` → hybrid prompt: emit frontmatter, `## Резюме`, `## Конспект` with screenshot embeds the LLM places, and the collapsed transcript. The frame list passed to the LLM carries the **planned filenames**.
- New module (or worker helpers): `slug(title) -> String` (strip `/\:*?"<>|`, spaces→`-`, keep Cyrillic, fallback `видео-<date>-<id8>`); `build_note(raw, slug, llm_markdown) -> String` (assembles frontmatter + LLM body + appends unplaced frames + collapsed transcript); `extract_summary(note) -> String` (the `## Резюме` section for chat).
- Worker final stage replaces the session-message delivery:
  1. compute `slug`, plan frame filenames `<slug>-frame-NN.jpg`
  2. LLM digest → assemble note
  3. ensure `mcp-obsidian` up; `save_media` each frame; `create_note("Видео/"+slug, "конспект.md", note)`; `commit_vault(...)`
  4. deliver chat: Резюме + path `Видео/<slug>/конспект.md` + `obsidian://open?vault=zettelkasten&file=<url-encoded path without .md>`
  5. `mark_video_job_done(summary = Резюме)`
- **MCP HTTP:** the worker POSTs `{"method":"tools/call","params":{"name":…,"arguments":…}}` to the mcp-obsidian endpoint (port 9005 on the host / container DNS on the docker net). Confirm the in-core reachable URL during planning.

## 9. Error handling

- **mcp-obsidian not reachable / save_media / create_note fail** → job `failed` + chat «не удалось сохранить конспект: <reason>». (Don't half-write: if `create_note` fails after media saved, the orphaned media is acceptable; log it.)
- **`commit_vault` fails** → `tracing::warn`, NOT fatal — files are written; the Zettelkasten heartbeat git step will pick them up.
- **slug collision** (folder exists) → `create_note` refuses; worker retries with `-2`, `-3` suffix.
- **empty title** → date-based slug.
- **toolgate failure** (no frames / no STT) unchanged from the base feature (degraded → transcript-only note; honest fail if no STT).

## 10. Testing (TDD)

- **MCP (Node):** add a minimal test runner (none today). `save_media` decodes base64 → file in `_System/media`, rejects traversal + bad extension; `create_note` creates the subfolder + refuses `..`; `commit_vault` commits and treats "nothing to commit" as success.
- **`video_summary.rs`:** hybrid prompt contains frontmatter keys, `## Резюме`, `![[_System/media/…]]` with the planned filename, `> [!note]-` callout; `build_note` appends unplaced frames to «Дополнительные кадры»; `extract_summary` returns only the Резюме section.
- **slug:** strips special chars, keeps Cyrillic, empty→date fallback, collision→suffix.
- **worker:** assembles the note and issues `save_media`×N → `create_note` → `commit_vault` in order against a mock MCP; chat payload carries Резюме + link; `image_b64` threaded from RawMaterial.
- **toolgate:** `frames[].image_b64` present and decodes to the JPEG; `title` returned (mock yt-dlp/ filename path).

## 11. Resolved defaults
- Subfolder: top-level **`Видео/`**, one folder per video (`Видео/<slug>/конспект.md`).
- Screenshots: shared **`_System/media/`**, `<slug>-frame-NN.jpg`.
- slug keeps **Cyrillic**.
- `commit_vault`: **best-effort** (never fails the job).
- Vault write path: **extended MCP** (not direct filesystem).
- Chat: short **Резюме + link**.

## 12. Open questions (for the plan, not blockers)
- Exact in-core URL to reach mcp-obsidian (host `127.0.0.1:9005` vs docker-network DNS) and how the worker ensures the on-demand container is started (services API endpoint) — confirm against `services.rs` during planning.
- `obsidian://` vault name — confirm the vault's registered name (`zettelkasten` assumed) so the deep link opens correctly.
- Whether to also store the note path on the `video_jobs` row (nice for diagnostics) — optional column, decide in planning.
