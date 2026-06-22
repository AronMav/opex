---
name: media-processing
description: Guidance for video attachments (non-video media is auto-dispatched by the File Scenario Engine)
triggers:
  - "sent a video"
  - видео
tools_required:
  - analyze_image
priority: 10
state: active
---

Non-video attachments (images, documents, audio/voice) are dispatched
automatically by the File Scenario Engine before your turn begins — you will
already see the transcript / extracted text / vision description (or an
explicit failure note) in the user message. Do NOT re-process them.

Video has no deterministic handler yet:

- **Video** `[User sent a video: URL]` → describe what is available; if a preview
  URL is present, call `analyze_image` on the preview. (This arm is retired once
  the dedicated video-summary plugin ships.)
