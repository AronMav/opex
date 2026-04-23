# HydeClaw

Self-hosted AI gateway built to be changed.

[![CI](https://img.shields.io/github/actions/workflow/status/AronMav/hydeclaw/ci.yml?branch=master&label=CI)](https://github.com/AronMav/hydeclaw/actions)
[![Release](https://img.shields.io/github/v/release/AronMav/hydeclaw)](https://github.com/AronMav/hydeclaw/releases)
[![License: MIT](https://img.shields.io/github/license/AronMav/hydeclaw)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-Linux%20ARM64%20%7C%20x86__64-blue?logo=linux&logoColor=white)](https://github.com/AronMav/hydeclaw/releases)

HydeClaw is a self-hosted AI gateway designed around a single idea: every layer should be replaceable without touching the core. Agent behavior lives in Markdown. Tools are YAML files. Providers swap with one config line. Channels are a separate process. Nothing is baked in that doesn't have to be.

---

## Quick Start

```bash
tar xzf hydeclaw-v0.4.0.tar.gz && cd hydeclaw && ./setup.sh
```

The installer handles Docker, Bun, Python, PostgreSQL, `.env` generation, and systemd services. Open `http://your-server:18789` when done.

<details>
<summary>From source</summary>

```bash
git clone https://github.com/AronMav/hydeclaw.git && cd hydeclaw && ./setup.sh
```

`setup.sh` installs Rust and Node.js and compiles from source when no pre-built binaries are found.

</details>

---

## The Layers

HydeClaw is organized into independent layers. Each layer can be changed, extended, or replaced without modifying the others.

### Agent behavior — TOML + Markdown files

An agent is a TOML config and a folder of Markdown files. No code.

```toml
# config/agents/Assistant.toml
[agent]
name = "Assistant"
language = "en"
provider = "openai"
model = "gpt-4o-mini"
```

Agent personality, memory, and instructions live in `workspace/agents/{Name}/`:

```text
SOUL.md       — personality, principles, tone
IDENTITY.md   — name, language, style
MEMORY.md     — long-term facts the agent maintains
HEARTBEAT.md  — background task checklist
```

Change these files and the agent changes on the next request. No restart.

### Tools — YAML files

Drop a YAML file in `workspace/tools/` and the tool is immediately available to all agents:

```yaml
name: get_weather
description: "Get current weather for a location."
endpoint: "https://api.open-meteo.com/v1/forecast"
method: GET
parameters:
  latitude: { type: number, required: true, location: query }
  longitude: { type: number, required: true, location: query }
response_transform: "$.current"
```

Supports: Bearer/API key auth injection, JSONPath response transforms, binary responses (photos, voice), SSRF protection. No code, no restart.

### Skills — Markdown files

Skills are behavioral instructions loaded on demand at inference time — not baked into the system prompt. Agents call `skill_use(action="load", name="...")` when they need guidance.

```markdown
---
name: code-methodology
description: TDD, debugging, code review
triggers:
  - write code
  - there is a bug
---

## Debugging

1. Reproduce — can you repeat the error?
2. Isolate — narrow to file → function → line
3. Hypothesize — what could be the cause?
```

Add a skill file → agents start using it. Remove the file → gone. Per-agent skills go in `workspace/skills/{agent-name}/`.

### Providers — config + database registry

All LLM backends and media services (STT, TTS, Vision, ImageGen, Embedding) go through a unified provider registry. The registry is configured in the database and editable from the Web UI.

To switch an agent to a different LLM — one line change in `config/agents/{Name}.toml`. To add a new provider — register it via API or UI, no code changes needed.

Any OpenAI-compatible endpoint works as a provider out of the box. Built-in support covers major hosted APIs (OpenAI, Anthropic, Google, Mistral, and others) and local runtimes (Ollama, vLLM, SGLang, LiteLLM).

### Channels — separate process

Chat adapters (Telegram, Discord, Matrix, IRC, Slack) run as a TypeScript/Bun process spawned and supervised by core. They communicate over an internal WebSocket.

Adding or modifying a channel adapter doesn't require touching the Rust core. The adapter sends `IncomingMessage` objects; core sends back tool results and text. The protocol is the boundary.

### Infrastructure — Docker Compose

PostgreSQL (with pgvector), SearXNG, and the browser renderer run as Docker containers defined in `docker/docker-compose.yml`. MCP servers start on-demand via the Docker API when an agent uses them.

---

## What changes without a restart

| What you change        | Takes effect              |
| ---------------------- | ------------------------- |
| SOUL.md / IDENTITY.md  | Next message              |
| Skill files            | Next message              |
| YAML tools             | Next request (30s cache)  |
| Agent TOML config      | Hot-reload (file watcher) |
| Provider settings      | Immediately via API       |
| Channel configuration  | On adapter reconnect      |

---

## Architecture

Three Rust binaries + two managed child processes + Docker infrastructure.

```text
hydeclaw-core       — HTTP API, agent lifecycle, LLM calls, tool dispatch,
  │                   memory, secrets, scheduler
  ├── channels/     — chat adapters (TypeScript/Bun, managed child process)
  └── toolgate/     — media hub: STT, TTS, Vision, ImageGen, Embeddings
                      (Python/FastAPI, managed child process)

hydeclaw-watchdog        — external health monitor with channel alerting
hydeclaw-memory-worker   — background embedding reindex via PostgreSQL task queue

PostgreSQL 17+pgvector   — sessions, messages, memory, cron, secrets
SearXNG                  — web search
browser-renderer         — headless browser for automation
MCP servers              — started on-demand via Docker API
```

<details>
<summary>Production process inventory</summary>

**Systemd services:**

| Binary                            | Description                 |
| --------------------------------- | --------------------------- |
| `hydeclaw-core-{arch}`            | Main gateway                |
| `hydeclaw-watchdog-{arch}`        | Health monitor (optional)   |
| `hydeclaw-memory-worker-{arch}`   | Background tasks (optional) |

**Managed child processes (started by core):**

| Process  | Runtime        | Description                                     |
| -------- | -------------- | ----------------------------------------------- |
| channels | Bun            | Telegram, Discord, Matrix, IRC, Slack adapters  |
| toolgate | Python/uvicorn | STT, TTS, Vision, ImageGen, Embeddings          |

**Docker containers (always-on):**

| Container        | Description               |
| ---------------- | ------------------------- |
| postgres         | PostgreSQL 17 + pgvector  |
| searxng          | Meta-search               |
| browser-renderer | Headless browser          |

**Docker containers (on-demand):** MCP servers + code execution sandbox.

</details>

---

## Features

- **Multi-agent orchestration** — agents collaborate in shared sessions; @-mention routing; session-scoped agent pools with run/async/message/status/kill actions
- **Long-term memory** — pgvector hybrid search (semantic + FTS) with MMR reranking; two-tier: raw (time-decay) + pinned (permanent)
- **MCP protocol** — any MCP server runs as an on-demand Docker container; tools auto-discovered and injected into agent context
- **Cron scheduler** — per-agent scheduled tasks with timezone support and jitter; dynamic jobs creatable via API
- **Secrets vault** — ChaCha20Poly1305 encryption; per-agent scoping; env var fallback
- **Tool approval** — configurable human-in-the-loop for sensitive operations before execution
- **Web UI** — Next.js dashboard: multi-agent chat, agent/provider/tool management, workspace canvas, memory explorer, audit log
- **Compaction** — automatic context compression when conversations exceed model window; facts extracted to memory
- **Network discovery** — WAN IP, Tailscale, LAN addresses, mDNS registration (`hydeclaw.local`)
- **Doctor diagnostics** — `/api/doctor` with severity levels and actionable remediation hints

---

## Configuration

### Environment (`.env`) — only 3 keys

| Variable                | Description              |
| ----------------------- | ------------------------ |
| `HYDECLAW_AUTH_TOKEN`   | API authentication       |
| `HYDECLAW_MASTER_KEY`   | Vault encryption key     |
| `DATABASE_URL`          | PostgreSQL connection    |

Everything else (API keys, tokens, credentials) goes into the encrypted vault.

### Agent config (`config/agents/{Name}.toml`)

```toml
[agent]
name = "Assistant"
language = "en"
provider = "openai"
model = "gpt-4o-mini"
temperature = 0.7

[agent.tool_loop]
max_iterations = 50
detect_loops = true
```

Changes are hot-reloaded — no restart needed.

---

## Security

- **Secrets vault** — ChaCha20Poly1305; per-agent scoping; credentials never stored in config files
- **SSRF protection** — DNS-level private IP blocking on every outbound YAML tool request; internal service blocklist
- **Workspace isolation** — path canonicalization guard; agents cannot escape their directory or write to other agents' files
- **Sandbox** — non-base agents execute code in isolated Docker containers
- **Tool approval** — configurable per-tool approval workflow; human confirmation before execution
- **PII redaction** — automatic filtering of tokens, keys, passwords in code execution output

> [!IMPORTANT]
> Back up `HYDECLAW_MASTER_KEY`. It is required for vault decryption and cannot be recovered.

---

## Development

```bash
make check          # cargo check --all-targets
make test           # cargo test
make lint           # cargo clippy
make build-arm64    # cross-compile for Raspberry Pi / AWS Graviton
make deploy         # binary + UI + migrations to remote server
make doctor         # health check on remote server
make logs           # live logs
```

<details>
<summary>Project structure</summary>

```text
hydeclaw/
├── crates/
│   ├── hydeclaw-core/          # Main binary
│   ├── hydeclaw-watchdog/      # Health monitor
│   ├── hydeclaw-memory-worker/ # Background tasks
│   └── hydeclaw-types/         # Shared types
├── channels/                   # Channel adapters (TypeScript/Bun)
├── toolgate/                   # Media hub (Python/FastAPI)
├── ui/                         # Web UI (Next.js 16)
├── workspace/                  # Runtime: tools/, skills/, agents/
├── config/                     # Agent + system TOML configs
├── migrations/                 # PostgreSQL migrations (auto-applied)
├── docker/                     # Compose + Dockerfiles
├── setup.sh                    # Installer
├── update.sh                   # One-command updater
└── release.sh                  # Build release archive
```

</details>

<details>
<summary>Requirements (from source)</summary>

- Rust 1.85+ (edition 2024)
- Node.js 22+ (UI build)
- Docker
- Bun 1.x (channel adapters)
- Python 3 (toolgate)
- [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) (ARM64 cross-compilation only)

</details>

---

## Updating

```bash
~/hydeclaw/update.sh hydeclaw-v0.4.0.tar.gz
```

Preserves `.env`, `config/`, `workspace/`, and database.

---

## License

MIT — see [LICENSE](LICENSE).
