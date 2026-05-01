---
name: media-processing
description: Automatic processing of media attachments from the user — photos, documents, audio
triggers:
  - "attached an image"
  - "attached a document"
  - "voice message"
  - "sent a video"
  - прикреплено изображение
  - прикреплён документ
  - голосовое сообщение
  - видео
tools_required:
  - analyze_image
  - extract_document
  - transcribe_audio
priority: 10
state: active
---

When the user attaches a file to a message — process it immediately without asking permission:

- **Image** `[User attached an image: URL]` → call `analyze_image` with the URL, describe what is in the image and respond to the user's message
- **Document (PDF, DOCX, TXT, etc.)** `[User attached a document: URL]` → call `extract_document` with the URL, use the content to respond
- **Voice / audio** `[User sent a voice message: URL]` → call `transcribe_audio` with the URL, respond to the transcription content
- **Video** `[User sent a video: URL]` → describe what is available, call `analyze_image` with the preview URL if present

Do not ask the user whether they want processing — just process and respond.
