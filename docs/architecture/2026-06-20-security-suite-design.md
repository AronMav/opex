# Security suite v1 — design

- **Date:** 2026-06-20
- **Status:** Approved (design); pending implementation plan
- **Branch:** `feat/security-suite`
- **Origin:** Hermes gap analysis (`reference_hermes_agent.md`, "Security tooling" gap). The prompt-injection **block** for identity files + C2/exfil/persistence patterns already shipped (Phase 1). This adds the remaining guardrails.

## Context & motivation

Three independent-but-cohesive guardrails, aligned with the project's core value (stability + security):

| # | Component | Hook | Behavior |
|---|-----------|------|----------|
| A | Pre-exec command scanner | `handle_code_exec` | block on host path, warn in sandbox |
| B | Post-write code-danger warnings | `handle_workspace_write` / `_edit` | non-blocking warning in tool result |
| C | User-configurable URL/domain blocklist | agent-controlled web fetches | refuse blocked hosts |

All three are pure-Rust pattern/policy checks (locally TDD-able). No DB / migration. One new config section (`[security]`) for C.

**Out of scope (deferred):** OSV/CVE advisory check before MCP launch (OPEX runs MCP as Docker images, not npx/uvx packages — the OSV-package model doesn't map); a downloaded external scanner binary (Hermes's Tirith) — we use in-process patterns instead; blocklist hot-reload (loaded at startup with the rest of the config).

## Cross-cutting principles

- TDD; rustls-only; clippy `-D warnings` clean; no `Co-Authored-By`; no push unless asked.
- Application-tree Rust tests run under `cargo test --bin opex-core`.
- Scanners reuse the `Severity` idea from `tools/content_security.rs` but live in focused new modules (shell-command threats and written-code smells are distinct concerns from context-injection).
- Verify scanners + policy locally with `cargo test`; the web-blocklist integration on the server.

---

## Component A — pre-exec command scanner

### Problem
`code_exec` is the most dangerous tool. For non-base agents it runs in a Docker sandbox (isolated); for **base agents it runs on the host with full filesystem access** (`execute_host_code`). Nothing scans the command before running.

### Design
New `crates/opex-core/src/tools/command_security.rs`:
- `pub enum CommandThreat { None, Dangerous(&'static str) }`
- `pub fn scan_command(code: &str) -> CommandThreat` — case-normalized substring/regex match against a curated list of high-confidence destructive / exfil / persistence shell patterns:
  - destructive: `rm -rf /`, `rm -rf ~`, `rm -rf /*`, `mkfs`, `dd ... of=/dev/`, `> /dev/sda`, `chmod -R 777 /`, fork bomb `:(){:|:&};:`
  - exfil / remote-exec: `curl ... | sh`, `curl ... | bash`, `wget ... | sh`, `wget ... | bash`
  - reverse shell: `bash -i >& /dev/tcp/`, `/dev/tcp/`, `nc -e`, `ncat -e`
  - persistence: `>> ~/.ssh/authorized_keys`, `>> /root/.ssh/authorized_keys`, crontab injection (`crontab -` piped)

Hook in `handle_code_exec` (`agent/pipeline/sandbox.rs`, right after `code` is read).
The host vs sandbox path is decided by `is_host = is_base && sandbox.is_none()` (the same
predicate the function already uses at the run site) — both `is_base` and `sandbox` are
parameters of `handle_code_exec`.
- Compute `scan_command(code)`.
- If `Dangerous(label)`:
  - **host path** (`is_host == true`): **block** — return `"⛔ code_exec blocked: dangerous command pattern '{label}'. Refusing to run on the host."` without executing.
  - **sandbox path** (`is_host == false`, Docker, isolated): **warn** — run as normal but prepend `"⚠ security: command matched '{label}' (ran in the isolated sandbox)."` to the result.
- `None`: unchanged.

### Tests (local, no IO)
`scan_command` flags each pattern class; benign commands (`ls -la`, `python script.py`, `rm -rf ./build`) are `None` (note: `rm -rf ./build` is relative — only absolute `/`/`~`/`/*` roots flagged); case-insensitivity; the host-vs-sandbox block/warn decision is a pure helper `command_action(threat, is_host) -> CmdAction { Allow, Warn(&str), Block(&str) }` unit-tested separately.

---

## Component B — post-write code-danger warnings

### Problem
When an agent writes code via `workspace_write`/`workspace_edit`, nothing flags dangerous constructs back to it. Hermes's security-guidance plugin appends non-blocking warnings so the model self-corrects.

### Design
New `crates/opex-core/src/tools/code_smell.rs`:
- `pub fn scan_written(filename: &str, content: &str) -> Vec<&'static str>` — extension-gated dangerous-code patterns (empty for non-code files like `.md`/`.txt`):
  - `.py`: `eval(`, `exec(`, `pickle.load`, `yaml.load(` (without `Loader=`), `os.system(`, `subprocess` with `shell=True`, `verify=False`, `pyyaml` unsafe
  - `.js`/`.ts`/`.tsx`/`.jsx`: `eval(`, `dangerouslySetInnerHTML`, `child_process.exec(`, `new Function(`
  - `.sh`/`.bash`: `curl ... | sh`, `eval ` , `rm -rf /`
  - `.yml`/`.yaml` (GitHub Actions): `${{ github.event` inside a `run:` (script injection)
- Returns matched labels (de-duplicated).

Hook in `handle_workspace_write` and `handle_workspace_edit` (`agent/pipeline/handlers.rs`): after a **successful** write, run `scan_written`; if non-empty, append to the success string:
`"\n\n⚠ Security note: this file contains potentially unsafe patterns ({labels}). Review before relying on it."` — **non-blocking** (the write already succeeded; this only informs the agent).

### Tests (local)
`scan_written` flags each language's patterns; `.md`/`.txt`/unknown extensions return empty; a clean `.py` returns empty; de-dup works.

---

## Component C — user-configurable URL/domain blocklist

### Problem
SSRF blocks private IPs, but there is no way for the operator to block the agent from fetching specific public domains (ads, known-malicious, data-exfil endpoints).

### Design
- New config: `SecurityConfig { #[serde(default)] blocked_domains: Vec<String> }` on `AppConfig` (`config/mod.rs`), `#[serde(default)] pub security: SecurityConfig`. TOML: `[security] blocked_domains = ["*.evil.tld", "ads.example.com"]`.
- New `crates/opex-core/src/tools/url_policy.rs`:
  - `pub fn host_blocked(host: &str, globs: &[String]) -> bool` — case-insensitive glob match (`*.evil.tld` matches `a.evil.tld` and `evil.tld`; exact match otherwise). A tiny dependency-free glob (prefix/suffix/`*`) — no new crate.
  - `pub fn url_blocked(url: &str, globs: &[String]) -> bool` — parse host from url, delegate to `host_blocked` (returns false on unparseable url — SSRF already guards scheme).
- Applied to **agent-controlled** web fetches only (NOT admin-set YAML endpoints).
  The check lives in the **tool handlers** (which have `deps.cfg.app_config.security` —
  the `ph::` functions do not receive config), checking `args["url"]` before delegating:
  - `BrowserActionHandler::handle` (`agent/tool_handlers/comms.rs`) — only when
    `args["url"]` is present (the `navigate` action).
  - `WebFetchHandler::handle` (`agent/tool_handlers/web.rs`) — `args["url"]`.
  - On block: return `"⛔ blocked by domain policy: {host}"` without delegating.
- **v1 limitation (noted, not fixed):** the `workspace/tools/browser.yaml` YAML alias
  reaches the browser-renderer via the internal-endpoint YAML path and bypasses this
  handler-level check; the primary `browser_action` system tool is covered.

### Tests (local)
`host_blocked`/`url_blocked`: `*.evil.tld` matches sub + apex; non-match passes; empty blocklist → never blocks; case-insensitive; unparseable url → false. Integration (server): with a configured blocklist, a browser_action/web_fetch to a blocked host is refused.

---

## File structure
- `crates/opex-core/src/tools/command_security.rs` (new) — A
- `crates/opex-core/src/tools/code_smell.rs` (new) — B
- `crates/opex-core/src/tools/url_policy.rs` (new) — C
- `crates/opex-core/src/tools/mod.rs` — declare the 3 modules
- `crates/opex-core/src/agent/pipeline/sandbox.rs` — A hook
- `crates/opex-core/src/agent/pipeline/handlers.rs` — B hook (write/edit)
- `crates/opex-core/src/agent/tool_handlers/comms.rs` — C hook (BrowserActionHandler)
- `crates/opex-core/src/agent/tool_handlers/web.rs` — C hook (WebFetchHandler)
- `crates/opex-core/src/config/mod.rs` — `SecurityConfig` + `AppConfig.security`

## Error handling
- All three fail-safe toward the existing behavior on parse issues (unparseable url → not blocked by policy but still SSRF-checked; empty patterns → no-op).
- A blocks only on the host path; sandbox stays permissive (isolated) with a warning.
- B never blocks a write (warning only).

## Testing & deploy
- Local: `cargo test --bin opex-core` (scanners + policy + decision helpers); `cargo clippy --bin opex-core --all-targets -- -D warnings`.
- Server: `make remote-deploy` (no migration) + `make doctor`; smoke: a base-agent `code_exec` with `rm -rf /` is refused; a `workspace_write` of a `.py` with `eval(` returns the warning; with `[security] blocked_domains` set, a browser_action to a blocked host is refused.
