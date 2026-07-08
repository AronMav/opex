---
name: media-processing
description: How to process an uploaded file or a shared link (image, document, audio/voice, video, URL) — the model-driven file_handler menu, not auto-dispatch.
triggers:
  - "sent a video"
  - "sent a file"
  - видео
  - обработать файл
  - обработать ссылку
tools_required:
  - file_handler
priority: 10
state: active
---

When the user sends a file or a link, the engine adds a context hint listing the
matching handlers. Processing is **model-driven via the `file_handler` tool** —
there is NO auto-dispatch (the old File Scenario Engine is retired). Do NOT
transcribe / describe / summarize on your own; drive the handlers:

1. **List** the options for the source (the tool renders an interactive menu of
   buttons — web card + Telegram inline buttons — on its own):

   ```
   file_handler(action="list", source_url="https://…")     # for a link
   file_handler(action="list", upload_id="<uuid>")          # for an uploaded file
   ```

   Do NOT re-print the handler list as text — the menu is already shown. Just
   wait for the user's choice (they click a button, or name a handler).

2. **Run** the chosen handler when the user picks one:

   ```
   file_handler(action="run", handler_id="summarize_video",
                source_url="…" | upload_id="…")
   ```

   The result (transcript / description / extracted text / summary, or a saved
   note) appears in the chat when it finishes; async jobs show a live progress
   indicator (fetch → transcribe → digest → saving).

Built-in handlers: `transcribe` (speech→text), `summarize_video` (transcript +
structured note), `describe` (image), `extract_document` (PDF/doc text), `save`
(store the file). A link may offer several — let the user choose.

Need a NEW kind of processing that no handler covers? Create one — see the
`file-handler-guide` skill (descriptor + `run(ctx, file, params)`, dropped into
`workspace/file_handlers/`, hot-reloaded).
