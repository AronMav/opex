---
name: channel-formatting
description: Per-channel output formatting rules — messenger brevity, cron/heartbeat terseness, inter-agent structured data, API/webhook freedom
triggers:
  - formatting
  - channel format
  - output format
  - messenger message
  - telegram message
  - discord message
  - how to format
  - форматирование
  - формат ответа
  - как форматировать
  - короткий ответ
priority: 5
state: active
---

# Channel Output Formatting

Adapt response style to the current channel. The `Runtime > Channel` line in
the system context tells you which channel you are responding on.

## Rules per channel

- **Messenger** (`telegram`, `discord`, `whatsapp`, `matrix`, `irc`, `slack`):
  - Concise, mobile-friendly. Split long responses into multiple messages.
  - Use the channel's native markdown (Telegram MarkdownV2, Discord md, Slack mrkdwn).
  - Bold key conclusions; avoid ASCII tables — use bullet lists.
  - Emojis sparingly — only if the user uses them or the channel culture invites it.

- **Scheduled tasks** (`cron`, `heartbeat`):
  - Data and conclusions only, no filler phrases.
  - If nothing to report: `HEARTBEAT_OK`.
  - No greetings, no apologies, no meta-commentary.

- **Inter-agent** (`agent` tool delegation, `inter-agent`):
  - Structured data — JSON blocks, numbered lists, tables.
  - No personality, no pleasantries. Task-focused output.
  - Include what downstream agent needs to act on directly.

- **API / webhook** (`api`, `webhook`):
  - Adapt freely to question complexity. Full markdown allowed.
  - Code blocks with language hints. Tables OK.

- **UI chat** (`ui`, default):
  - Full markdown: headings, lists, code blocks, tables, math (KaTeX), mermaid diagrams.
  - Bold key conclusions. Keep code snippets focused.

## Universal rules (all channels)

1. Match response length to question complexity — short question = short answer.
2. Bold key conclusions with `**text**`.
3. Use lists for multi-part answers.
4. Keep code snippets focused on what the user asked — don't paste entire files.
5. Never output Chinese characters (汉字) in a non-Chinese session — that is a hallucination; stop and retry in the session language.
