---
name: mcp-docker-pattern
description: Use when deploying new MCP servers to HydeClaw via Docker containers and mcp-deploy.sh script
triggers:
  - mcp сервер
  - подключи mcp
  - deploy mcp
  - mcp server
tools_required:
  - code_exec
priority: 10
---

# MCP Server Deployment

How to connect new MCP servers to HydeClaw.

## Critical Rules

1. **`process(action="start")`** — for ALL docker commands and scripts. Runs ON THE HOST.
2. **Do NOT use `code_exec`** for docker operations — it runs in a sandbox WITHOUT Docker.
3. **Always use the `mcp-deploy.sh` script** — do NOT write Dockerfiles manually. The script detects the type (Python/Node.js), builds on top of the bridge image, and verifies.
4. **Always deploy**, even if the server requires an API token. The container is created first; the token is added later via `secret_set`. Never refuse deployment due to a missing token.
5. After `process(action="start")`, call `process(action="status", process_id=...)` and wait for completion. If the script returned FAIL — check logs via `process(action="start", command="docker logs mcp-NAME")`.

## Automated Deployment via Script

The Pi has `~/hydeclaw/scripts/mcp-deploy.sh` which handles everything automatically: build, container create, workspace YAML, verify.

### Type 1: Node.js stdio MCP (official mcp/* images)

```
process(action="start", command="~/hydeclaw/scripts/mcp-deploy.sh stdio-node mcp/fetch:latest fetch 9011")
```

The script automatically:
- Pulls the image (with --platform linux/amd64 for ARM compatibility)
- Detects the entrypoint from the image
- Creates a 3-line Dockerfile based on hydeclaw-mcp-bridge
- Builds, creates container, YAML, verifies

### Type 2: Python stdio MCP (pip package)

```
process(action="start", command="~/hydeclaw/scripts/mcp-deploy.sh stdio-python mcp-server-git git 9012 mcp-server-git")
```

Format: `stdio-python <pip-package> <name> <port> [command-name]`

### Type 3: External HTTP MCP (no Docker)

```
process(action="start", command="~/hydeclaw/scripts/mcp-deploy.sh url https://context7.com/mcp context7")
```

Only creates `workspace/mcp/name.yaml` with the URL.

### Removal

```
process(action="start", command="~/hydeclaw/scripts/mcp-deploy.sh remove fetch")
```

## Verifying the Result

After `process(action="start")`, call `process(action="status", process_id=...)` to check the result.
The script prints OK/FAIL and the number of tools discovered.

## Occupied Ports

| Port | Service |
|------|---------|
| 9002 | summarize |
| 9003 | stock-analysis |
| 9004 | weather |
| 9005 | obsidian |
| 9006 | github |
| 9007 | postgres |
| 9011 | toolgate (managed process, NOT MCP) |
| 9020 | browser-renderer |
| 9030 | browser-cdp |

Use ports starting from 9040, 9041, 9042... for new MCPs. Skip already occupied ones.

## Known MCP Servers

### Node.js stdio (mcp/* on Docker Hub)
- `mcp/fetch:latest` — HTTP fetch, web page loading
- `mcp/memory:latest` — key-value memory store
- `mcp/sequentialthinking:latest` — structured reasoning
- `mcp/filesystem:latest` — filesystem (requires volume mount)
- `mcp/puppeteer:latest` — Chromium browser automation
- `mcp/everart:latest` — image generation
- `mcp/time:latest` — timezone/datetime operations

### Python pip
- `mcp-server-git` — git operations (command: `mcp-server-git`)

### External HTTP (no Docker)
- `https://context7.com/mcp` — library documentation
- `https://mcp.deepwiki.com/mcp` — wiki/knowledge base

## MCP Servers with Env Variables (tokens)

For servers requiring API tokens, pass env vars as the 5th argument:

```
process(action="start", command="~/hydeclaw/scripts/mcp-deploy.sh stdio-node mcp/slack:latest slack 9047 'SLACK_BOT_TOKEN: ${SLACK_BOT_TOKEN}'")
```

The token is added later via `secret_set`. The server deploys but returns errors when called without a token — this is expected.

## Troubleshooting

If the script returned FAIL:
1. Check container logs: `process(action="start", command="docker logs mcp-NAME")`
2. Common cause: image doesn't exist for ARM64 — the script tries --platform linux/amd64
3. For Python MCP: verify the pip package exists
4. For external URL: check reachability via `process(action="start", command="curl -X POST URL ...")`
