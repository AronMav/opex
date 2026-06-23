---
name: toolgate-router
description: Create new toolgate routers (Python FastAPI endpoints) and YAML tools
triggers:
  - создай роутер
  - новый эндпоинт
  - toolgate
  - новый инструмент
tools_required:
  - code_exec
  - workspace_write
priority: 10
---

# Creating Toolgate Routers

Load the full guide first: `skill_use(action="load", name="toolgate-guide")`

## Workflow (strict order)

1. Check existing: `workspace_list("tools/")` + `code_exec("ls ~/opex/toolgate/routers/")`
2. If analog exists — report name, don't create duplicate
3. Write router file: `code_exec` to create `~/opex/toolgate/routers/name.py`
   - All imports, BaseModel, `router = APIRouter()` — self-contained
4. **Write YAML tool immediately** (before restart): `workspace_write("tools/name.yaml", ...)`
5. Register in app.py: `from routers import name` + `app.include_router(name.router)`
6. Install deps via venv pip (NOT system pip)
7. **Verify syntax**: run py_compile on the router file
8. Restart toolgate via Core API `POST /api/services/toolgate/restart`
9. Poll health until ready (up to 15s): retry `web_fetch("http://localhost:9011/health")` every 3s
10. If health check fails after 15s — check import errors (see below)
11. Add entry to TOOLS.md

## If toolgate doesn't respond after restart

Run `import app` via the toolgate venv python — this will show the import error. Fix and restart again.

## Rules

- **NEVER start toolgate manually** (uvicorn directly) — only via Core API restart
- **NEVER use system pip** — only the toolgate venv python -m pip
- **Create YAML before restart** — it gets lost during debugging if done last
- **Verify file exists before restart**
