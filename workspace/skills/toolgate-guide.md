---
name: toolgate-guide
description: Guide for creating toolgate providers and routers (FastAPI, media hub, STT/TTS/Vision/ImageGen)
triggers:
  - toolgate
  - create provider
  - new provider
  - create router
  - new router
  - media provider
  - fastapi
tools_required:
  - code_exec
  - web_fetch
priority: 5
state: active
---

Toolgate is a FastAPI service (port 9011), the media hub. STT, Vision, TTS, ImageGen via swappable providers + custom routers.

## Provider Protocols (base.py)

| Capability | Method signature |
|---|---|
| STT | `transcribe(http, audio_bytes, filename, language, model?) -> str` |
| Vision | `describe(http, image_bytes, content_type, prompt, max_tokens?) -> str` |
| TTS | `synthesize(http, text, voice, model?, response_format?) -> bytes` |
| ImageGen | `generate(http, prompt, size?, model?, quality?) -> bytes` |

Constructor always: `(base_url, api_key, model, options)`.

## Creating a Provider — 3 steps

**1)** Create `toolgate/providers/{type}_{name}.py` via code_exec
**2)** Register in `registry.py`: import + driver map + DRIVER_INFO
**3)** Restart toolgate via Core API

## Creating a Router

File at `~/hydeclaw/toolgate/routers/name.py` via code_exec. Required: `router = APIRouter()`. Secrets via `workspace_helpers.get_secret(name)`. Register in `app.py`: import + `app.include_router(name.router)`. Restart toolgate.

## Secrets Access

```python
from workspace_helpers import get_secret, core_api
token = await get_secret("MY_API_KEY")
data = await core_api("GET", "/api/agents")
```

## What Requires Restart

| Change | Action |
|---|---|
| routers, providers, registry.py | Restart toolgate |
| Media provider config (DB) | Auto-reload (no restart) |
| YAML tools | Hot-reload in Core (not toolgate) |
