<h1 align="center">
  <img src="docs/assets/opex-banner.png" alt="OPEX — a self-hosted AI gateway where everything is replaceable" width="820">
</h1>

<p align="center">
  <em>Pronounced "O-REKH" (walnut)</em>
</p>

<p align="center">
  <a href="https://github.com/AronMav/opex/actions/workflows/ci.yml?branch=master"><img src="https://img.shields.io/github/actions/workflow/status/AronMav/opex/ci.yml?branch=master&style=for-the-badge" alt="CI"></a>
  <a href="https://github.com/AronMav/opex/releases"><img src="https://img.shields.io/github/v/release/AronMav/opex?style=for-the-badge" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="MIT"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust&logoColor=white&style=for-the-badge" alt="Rust"></a>
  <a href="https://github.com/AronMav/opex/releases"><img src="https://img.shields.io/badge/platform-ARM64%20%7C%20x86__64-blue?logo=linux&logoColor=white&style=for-the-badge" alt="Platform"></a>
</p>

<p align="center">
  <a href="README.ru.md">Русский</a> ·
  <a href="docs/">Docs</a> ·
  <a href="docs/ARCHITECTURE.md">Architecture</a> ·
  <a href="docs/API.md">API</a> ·
  <a href="SECURITY.md">Security</a>
</p>

**OPEX is a self-hosted AI gateway in Rust, built around one idea: every layer is replaceable without touching the core.** Agent behavior lives in Markdown. Tools are YAML files. Providers swap with one line. Channels are a separate process. A single binary serves the HTTP API, agent lifecycle, LLM calls, tools, channels, memory and secrets — on a home server, ARM64 or x86_64, with no cloud lock-in. Talk to it from Telegram while it works on a remote machine.

Use any model — **150+ providers from the built-in catalog in one click**, any OpenAI-compatible endpoint, local Ollama/vLLM. Context windows for **5000+ models** are filled in automatically. Switching is one line of TOML — no code, no vendor lock-in.

<table>
<tr><td><b>Everything replaceable, nothing baked in</b></td><td>An agent's persona and memory are Markdown files. Tools are YAML. Skills are Markdown, loaded at runtime. Providers are a single registry editable from the UI. Channels are a separate process behind a protocol boundary. Change a file, change the behavior — no restart.</td></tr>
<tr><td><b>Built-in model catalog</b></td><td>Context windows, output limits, pricing and capabilities for 5000+ models from <a href="https://models.dev">models.dev</a> + OpenRouter, refreshed in the background. Auto-detected window for any model, add 150+ providers by preset (URL/type/models auto-filled), $ accounting at real prices, capability-aware request parameters.</td></tr>
<tr><td><b>Lives where you do</b></td><td>Telegram, Discord, Matrix, IRC, Slack — from a single gateway process. Voice-memo transcription, media handling, cross-platform conversation continuity.</td></tr>
<tr><td><b>Multi-agent orchestration</b></td><td>Agents collaborate in shared sessions, routed by @-mentions. Pools of session-scoped agents with a run / async / message / status / kill lifecycle — parallel workstreams without shared state.</td></tr>
<tr><td><b>Long-term memory</b></td><td>PostgreSQL + pgvector, hybrid search (semantic + FTS) with MMR reranking. Two tiers: raw with time decay, and pinned permanent. Key facts are extracted into memory during context compaction.</td></tr>
<tr><td><b>Scheduling & automations</b></td><td>Agent-level cron scheduler with timezones and jitter. Daily reports, nightly backups, audits — in natural language, unattended, delivered to any channel.</td></tr>
<tr><td><b>Extensible by standards</b></td><td>Any MCP server as an on-demand Docker container, tools auto-discovered. File handlers (STT / Vision / TTS / ImageGen / video) as self-describing Python plugins with hot-reload. LSP intelligence (pyright) for agents.</td></tr>
</table>

---

## Install

```bash
tar xzf opex-v<VERSION>.tar.gz
cd opex
./setup.sh
```

The installer sets up Docker, Bun, Python 3, PostgreSQL, generates `.env`, and creates systemd services. When done, open `http://your-server:18789`.

From source: clone the repo and run `./setup.sh` — it detects missing toolchains and compiles. Requires Rust 1.85+ (edition 2024), Node.js 22+, Docker, Bun 1.x, Python 3.

---

## Replaceable layers

OPEX is organized into independent layers — each can be changed, extended or replaced without affecting the others.

**Agent behavior — TOML + Markdown.** An agent is a TOML config and a folder of Markdown files in `workspace/agents/{Name}/`. Persona, memory, tone, background tasks — plain text. Edit a file = new behavior, no restart.

**Tools — YAML.** Drop a YAML into `workspace/tools/` and the tool is available instantly. Auth injection (Bearer / API key / header), JSONPath response transforms, binary responses (photo, voice), SSRF protection. No code.

**Skills — Markdown on demand.** Behavioral instructions injected at inference time rather than baked into the system prompt. The agent discovers them and loads on trigger match. Add a file — the skill appears; remove it — it's gone.

**Providers — a single registry + catalog.** All LLM and media services (STT, TTS, Vision, ImageGen, Embedding) go through a registry editable from the Web UI or API. Adding a provider = pick from 150+ catalog presets (URL, type and model list auto-filled). Any OpenAI-compatible endpoint works immediately.

**Channels — a separate process.** Telegram / Discord / Matrix / IRC / Slack adapters run as a TypeScript/Bun subprocess. The core knows no messaging protocol: adapters send `IncomingMessage` over an internal WebSocket. A new adapter needs no Rust changes.

---

## What changes without a restart

| Layer | Takes effect |
| --- | --- |
| SOUL.md / IDENTITY.md | Next message |
| Skill files | Next message |
| YAML tools | Next request (30s cache) |
| Agent TOML config | Hot-reload (file watcher) |
| Provider settings | Immediately via API |
| Model catalog | Background refresh (24h default) |
| Channel config | On adapter reconnect |

---

## Model catalog

OPEX pulls model metadata from external aggregators and makes it the single source of truth — instead of hardcoded tables.

- **Auto context window.** Resolution chain: manual override → provider native self-report (`/api/show`, `/v1/models`, `inputTokenLimit`) → **catalog** (models.dev ∪ OpenRouter) → name heuristic. 5000+ models resolve accurately; local and custom models via native probing.
- **150+ providers in one click.** The "add provider" picker fills base_url, type and model list. Most are OpenAI-compatible → added as `openai_compat` with no new code.
- **$ accounting.** `/api/usage` computes cost from real catalog prices, not a tiny built-in table.
- **Model capabilities.** `max_tokens` is clamped to the output limit; `temperature` is omitted for models that don't accept it (o1/reasoning).

---

## Architecture

Three Rust binaries + two managed child processes + Docker infrastructure.

```text
opex-core       — HTTP API, agent lifecycle, LLM calls, tool dispatch,
  │               memory, secrets, scheduler, model catalog
  ├── channels/ — chat adapters (TypeScript/Bun, managed process)
  └── toolgate/ — media hub: STT, TTS, Vision, ImageGen, Embeddings
                  (Python/FastAPI, managed process)

opex-watchdog        — external health monitor with channel alerting
opex-memory-worker   — background reindex via a PostgreSQL task queue

PostgreSQL 17 + pgvector — sessions, messages, memory, cron, secrets, usage
SearXNG                  — meta-search for web-search tools
browser-renderer         — headless browser for automation
MCP servers              — on-demand via the Docker API
code sandbox             — isolated containers for non-base agents' code
```

The Rust core knows no messaging protocol and ships no built-in provider SDK. Every external surface — channels, media services, LLM backends, MCP tools — is wired through a defined protocol boundary. That's what makes the layers replaceable. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

---

## Security

- **Workspace isolation** — path canonicalization and symlink resolution; an agent can't escape its directory.
- **SSRF protection** — DNS-level private-IP blocking (RFC 1918, link-local, CGNAT, Teredo, 6to4, IPv4-mapped) for outgoing YAML-tool requests; internal-service block-list.
- **Sandbox** — non-base agents run code in isolated Docker containers; base agents run on the host with explicit permission.
- **Tool approval** — per-tool human-in-the-loop; state in PostgreSQL.
- **Secrets** — ChaCha20Poly1305, per-agent scope, env fallback; credentials never touch config files.
- **PII redaction** and **prompt-injection detection** — keys/tokens filtered from code output; external content wrapped in boundary markers.

> [!IMPORTANT]
> Back up `OPEX_MASTER_KEY` — it decrypts the vault and cannot be recovered if lost.

---

## Configuration

Three variables in `.env`; everything else lives in the encrypted vault.

```bash
OPEX_AUTH_TOKEN=...   # API authentication
OPEX_MASTER_KEY=...   # ChaCha20Poly1305 vault key
DATABASE_URL=...      # PostgreSQL connection string
```

Agent config is `config/agents/{Name}.toml`, hot-reloaded on change:

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
make check           # cargo check --all-targets
make test            # cargo test (skips sqlx::test without a DB)
make lint            # cargo clippy --all-targets -- -D warnings
make remote-deploy   # build on the server → atomic swap + restart
make doctor          # GET /api/doctor
make logs            # journalctl --user -u opex-core -f
```

```text
opex/
├── crates/
│   ├── opex-core/          # Main binary
│   ├── opex-watchdog/      # Health monitor
│   ├── opex-memory-worker/ # Background jobs
│   └── opex-types/         # Shared types
├── channels/               # Channel adapters (TypeScript/Bun)
├── toolgate/               # Media hub (Python/FastAPI)
├── ui/                     # Web UI (Next.js 16)
├── workspace/              # Runtime: tools/, skills/, agents/
├── config/                 # Agent & system config (TOML)
├── migrations/             # PostgreSQL migrations (auto on startup)
└── docker/                 # Compose + Dockerfile
```

---

## Update

```bash
~/opex/update.sh opex-v<VERSION>.tar.gz
```

Preserves `.env`, `config/`, `workspace/` and the database. Then verify with `GET /api/doctor`.

---

## License

MIT — see [LICENSE](LICENSE).
