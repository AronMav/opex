# HydeClaw

<p align="center">
  <strong>Self-hosted AI gateway built to be changed.</strong>
</p>

<p align="center">
  <a href="https://github.com/AronMav/hydeclaw/actions/workflows/ci.yml?branch=master"><img src="https://img.shields.io/github/actions/workflow/status/AronMav/hydeclaw/ci.yml?branch=master&style=for-the-badge" alt="CI"></a>
  <a href="https://github.com/AronMav/hydeclaw/releases"><img src="https://img.shields.io/github/v/release/AronMav/hydeclaw?style=for-the-badge" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="MIT"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust&logoColor=white&style=for-the-badge" alt="Rust"></a>
  <a href="https://github.com/AronMav/hydeclaw/releases"><img src="https://img.shields.io/badge/platform-ARM64%20%7C%20x86__64-blue?logo=linux&logoColor=white&style=for-the-badge" alt="Platform"></a>
</p>

HydeClaw is a self-hosted AI gateway designed around one idea: every layer should be replaceable without touching the core. Agent behavior lives in Markdown. Tools are YAML files. Providers swap with one config line. Channels are a separate process. Nothing is baked in that doesn't have to be.

[Docs](docs/) · [API Reference](docs/API.md) · [Architecture](docs/ARCHITECTURE.md) · [Configuration](docs/CONFIGURATION.md) · [Security](SECURITY.md)

New install? Run `./setup.sh` — it handles everything.

---

## Install

```bash
tar xzf hydeclaw-v0.4.0.tar.gz
cd hydeclaw
./setup.sh
```

The installer handles Docker, Bun, Python 3, PostgreSQL, `.env` generation, and systemd services.
Open `http://your-server:18789` when done.

Building from source: clone the repo and run `./setup.sh` — it detects missing toolchains and compiles.

---

## The Layers

HydeClaw is organized into independent layers. Each layer can be changed, extended, or replaced without touching the others.

**Agent behavior — TOML + Markdown files.**
An agent is a TOML config and a folder of Markdown files. Personality, memory, tone, and background tasks are plain text files in `workspace/agents/{Name}/`. Change a file, change the agent — on the next request, no restart.

**Tools — YAML files.**
Drop a YAML file in `workspace/tools/` and the tool is live immediately. Supports auth injection (Bearer, API key, header), JSONPath response transforms, binary responses (photos, voice), and SSRF protection. No code, no restart.

**Skills — Markdown files loaded on demand.**
Skills are behavioral instructions injected at inference time, not baked into the system prompt. Agents discover them via `skill_use(action="list")` and load them when needed. Add a skill file and agents start using it. Remove it and it's gone.

**Providers — a unified registry.**
All LLM backends and media services (STT, TTS, Vision, ImageGen, Embedding) go through a provider registry editable from the Web UI or API. Switching an agent to a different model is one line in a TOML file. Any OpenAI-compatible endpoint works out of the box.

**Channels — a separate process.**
Telegram, Discord, Matrix, IRC, and Slack adapters run as a TypeScript/Bun subprocess. The core doesn't know or care about messaging protocols — adapters send `IncomingMessage` objects over an internal WebSocket and get results back. Add a new adapter without touching Rust.

---

## What changes without a restart

| Layer                  | Takes effect              |
| ---------------------- | ------------------------- |
| SOUL.md / IDENTITY.md  | Next message              |
| Skill files            | Next message              |
| YAML tools             | Next request (30 s cache) |
| Agent TOML config      | Hot-reload (file watcher) |
| Provider settings      | Immediately via API       |
| Channel configuration  | On adapter reconnect      |

---

## Highlights

- **Multi-agent orchestration** — agents collaborate in shared sessions with @-mention routing; session-scoped pools with run/async/message/status/kill lifecycle
- **Long-term memory** — PostgreSQL + pgvector hybrid search (semantic + FTS) with MMR reranking; two tiers: raw (time-decay) and pinned (permanent)
- **MCP protocol** — any MCP server runs as an on-demand Docker container; tools are auto-discovered and injected into agent context
- **Skills system** — Markdown-based instructions loaded at runtime; server-side trigger matching injects a hint into the system prompt when a user message matches a skill's keywords
- **Cron scheduler** — per-agent scheduled tasks with timezone support and jitter; jobs are creatable via API at runtime
- **Secrets vault** — ChaCha20Poly1305 encryption, per-agent scoping, env var fallback; credentials never touch config files
- **Tool approval** — configurable human-in-the-loop before execution of sensitive operations
- **Context compaction** — when conversation history exceeds the model window, the oldest turns are compressed and key facts are extracted to long-term memory
- **Web UI** — Next.js 16 dashboard: multi-agent chat, agent/provider/tool management, workspace canvas, memory explorer, audit log
- **Network discovery** — WAN IP, Tailscale status, LAN interfaces, mDNS (`hydeclaw.local`) for zero-config LAN access
- **Doctor diagnostics** — `GET /api/doctor` with severity levels and actionable remediation hints

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

PostgreSQL 17 + pgvector — sessions, messages, memory, cron, secrets
SearXNG                  — meta-search engine for web search tools
browser-renderer         — headless browser for automation
MCP servers              — started on-demand via Docker API
code sandbox             — isolated Docker containers for non-base agent code execution
```

The Rust core speaks no messaging protocol and has no provider SDK embedded. Every external surface — channels, media services, LLM backends, MCP tools — is connected through a defined protocol boundary. This is what makes individual layers swappable.

---

## Security

- **Workspace isolation** — path canonicalization guard with symlink resolution; agents cannot escape their directory or reach another agent's files
- **SSRF protection** — DNS-level private IP blocking (RFC 1918, link-local, CGNAT, Teredo, 6to4, IPv4-mapped) on every outbound YAML tool request; internal service blocklist
- **Sandbox** — non-base agents execute code in isolated Docker containers; base agents run on the host with explicit opt-in
- **Tool approval** — per-tool human confirmation workflow; approval state persisted in PostgreSQL
- **PII redaction** — automatic filtering of tokens, keys, passwords in code execution output
- **Prompt injection detection** — inbound content scanned for override patterns; external content wrapped in boundary markers

> [!IMPORTANT]
> Back up `HYDECLAW_MASTER_KEY`. It is required for vault decryption and cannot be recovered if lost.

---

## Configuration

Three variables in `.env`. Everything else goes into the encrypted vault.

```bash
HYDECLAW_AUTH_TOKEN=...   # API authentication
HYDECLAW_MASTER_KEY=...   # ChaCha20Poly1305 vault key
DATABASE_URL=...          # PostgreSQL connection string
```

Agent config lives in `config/agents/{Name}.toml` and hot-reloads on change:

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

---

## Development

```bash
make check          # cargo check --all-targets
make test           # cargo test
make lint           # cargo clippy -- -D warnings
make build-arm64    # cross-compile for Raspberry Pi / AWS Graviton
make deploy         # binary + UI + migrations → remote server
make doctor         # GET /api/doctor on remote
make logs           # journalctl tail on remote
```

Requirements from source: Rust 1.85+ (edition 2024), Node.js 22+, Docker, Bun 1.x, Python 3, [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) (ARM64 only).

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
├── migrations/                 # PostgreSQL migrations (auto-applied on start)
└── docker/                     # Compose + Dockerfiles
```

---

## Updating

```bash
~/hydeclaw/update.sh hydeclaw-v0.4.0.tar.gz
```

Preserves `.env`, `config/`, `workspace/`, and the database. Run `GET /api/doctor` after to verify.

---

## License

MIT — see [LICENSE](LICENSE).
