---
name: canvas-usage
description: Display rich visual content in the Canvas panel — HTML dashboards, charts, markdown, JSON tables, embedded URLs
triggers:
  - визуализ
  - график
  - диаграмм
  - dashboard
  - canvas
  - покажи визуально
  - нарисуй
  - отобрази
  - chart
  - graph
  - таблиц
  - выведи в canvas
  - покажи на canvas
  - интерактивн
  - дашборд
tools_required:
  - canvas
---

# Canvas

Canvas is a dedicated UI panel in HydeClaw for displaying visual content: HTML pages, charts, dashboards, markdown documents, tables, and embedded URLs.

## When to Use Canvas

- Data visualization (charts, graphs, diagrams)
- Dashboards and analytics
- Tables with formatting or styling
- Interactive content (forms, filters, animations)
- HTML pages or widgets
- Embedded URL (iframe)
- User says "show in canvas", "visualize", "draw a chart"

## When NOT to Use Canvas

- Plain text — write directly in chat
- Short lists — markdown in chat is sufficient
- Images from tools — displayed inline automatically
- Unstyled tabular data — use `rich_card` (card_type="table") for inline chat tables

## Actions

### present — display content

Primary action. Opens the Canvas panel and renders content.

```json
{
  "action": "present",
  "content_type": "html",
  "title": "Panel title",
  "content": "<full HTML with inline CSS and JS>"
}
```

### push_data — send JSON data

Displays structured data. Automatically sets `content_type: "json"`.

```json
{
  "action": "push_data",
  "content": "{\"columns\": [\"Name\", \"Value\"], \"rows\": [[\"CPU\", \"42%\"], [\"RAM\", \"8GB\"]]}",
  "title": "System Metrics"
}
```

JSON with `{columns, rows}` structure renders as a table (TableCard).

### clear — clear canvas

```json
{
  "action": "clear"
}
```

### run_js — execute JS in current canvas

Runs JavaScript in the context of the current canvas content via browser-renderer. Requires a prior `present` call with content.

```json
{
  "action": "run_js",
  "code": "document.querySelector('#counter').textContent = '42'"
}
```

### snapshot — screenshot canvas

Takes a PNG screenshot of the current canvas via browser-renderer. Requires a prior `present` call with content.

```json
{
  "action": "snapshot"
}
```

## Content Types

| Type | Description | Rendering |
|------|-------------|-----------|
| `html` | Full HTML page with inline CSS/JS | Sandboxed iframe (allow-scripts) |
| `markdown` | Markdown text | `<Markdown>` component with prose styles |
| `url` | URL to embed | iframe via `sanitizeUrl()` |
| `json` | JSON string | Formatted JSON or TableCard (`{columns, rows}`) |

Default: `markdown`.

## HTML Design Rules

Follow these rules strictly when creating HTML content.

### Required

- Fully self-contained HTML page with **inline** CSS and JS
- Dark theme: deep colors (`#1a1a2e`, `#0a192f`, `#2d1b33`), not flat black
- SVG icons or CSS shapes instead of emoji
- Warm tones, teals, ambers, or monochrome — no purple/indigo/violet gradients
- Asymmetric layouts: varied element sizes, left-aligned text
- Contrasting font-weight (200 vs 800), mix serif + sans-serif
- Depth: layered shadows, subtle borders, glassmorphism
- Life: CSS transitions on hover, staggered @keyframe fade-ins, subtle transforms

### Forbidden

- Emoji as icons (🌤️☁️🌡️💧💨🚀📊✨ etc.)
- Purple/indigo/violet gradients
- Three identical cards in a row — use asymmetric grid
- Centering everything — left-align text, varied whitespace

### CDN Libraries

- **Chart.js**: `<script src="https://cdn.jsdelivr.net/npm/chart.js"></script>`
- **D3.js**: `<script src="https://cdn.jsdelivr.net/npm/d3"></script>`
- **Mermaid**: `<script src="https://cdn.jsdelivr.net/npm/mermaid/dist/mermaid.min.js"></script>`
- Any CDN visualization library

## Limits

- **Max content size**: 5 MB
- HTML renders in `<iframe sandbox="allow-scripts">` — no access to parent window
- `run_js` and `snapshot` require a running browser-renderer
- URL content filtered through `sanitizeUrl()` for security

## API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/canvas/{agent}` | GET | Current canvas state for agent |
| `/api/canvas/{agent}` | DELETE | Clear canvas for agent |

WebSocket event `canvas_update` — real-time canvas updates in UI.

## Important

**Always** write a text summary in chat after calling canvas. The user may not see the Canvas panel (mobile device, channel without UI). An empty reply is a failure.
