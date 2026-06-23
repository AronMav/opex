# Authority Model

How system prompt content is governed across layers.

## Hierarchy (highest to lowest priority)

### 1. Engine Hardcoded (workspace.rs -- `build_system_prompt()`)

**Controls:** Runtime mechanics, operating mode, tool usage rules, task completion behavior.

**Sections assembled in code:**

- `# Runtime` -- agent name, channel, model, datetime, language, connected channels
- `# Project Context` -- injects workspace file contents (SOUL.md, IDENTITY.md, MEMORY.md, TOOLS.md, AGENTS.md, USER.md)
- `# Available Tools & Capabilities` -- MCP tool schemas
- `# Operating Mode` -- reasoning approach, task completion rules, tool usage rules, output formatting, response guidelines
- `# Multi-Agent Session` -- participant list, agent tool rules (injected only in multi-agent sessions)

**Change process:** Rust code change, requires `cargo check && cargo test`, deploy binary.

**Budget:** ~2-3KB base + workspace content + MCP schemas. Not capped -- operator monitors via `system_prompt_size` log.

### 2. SOUL.md (per-agent, always loaded)

**Controls:** Agent identity, core principles, methodology mindset.

**What belongs here:**

- Who the agent is (name, role, personality)
- Capabilities and access level
- Hard rules (security boundaries, behavioral constraints)
- Core methodology principles (concise -- "what to think about", not "how to do it step by step")

**What does NOT belong here:**

- Detailed procedures (use skills instead)
- Tool schemas or runtime info (engine handles this)
- Operating mode rules (engine handles this)

**Change process:** Edit markdown file, takes effect on next message (no deploy).

**Budget:** ~2K tokens (~8KB). Keep SOUL.md lean before adding methodology.

### 3. Skills (on-demand, loaded via `skill_use` tool)

**Controls:** Detailed methodology, procedures, frameworks, protocols.

**What belongs here:**

- Step-by-step procedures (architecture design, code review, research strategy)
- Decision frameworks (discovery levels, task sizing, quality gates)
- Domain-specific knowledge (verification protocols, error recovery playbooks)

**What does NOT belong here:**

- Agent identity or personality (SOUL.md handles this)
- Runtime configuration (engine handles this)
- Core principles (SOUL.md handles this -- skills elaborate, not define)

**Change process:** Create/edit markdown in `workspace/skills/`, takes effect immediately.

**Budget:** Unbounded per skill, but only loaded when agent calls `skill_use("skill-name")`. Multiple skills can be loaded per session.

## Overlap Prevention Rules

1. **Identity** is SOUL.md only -- engine never defines who the agent is
2. **Operating mode** is engine only -- SOUL.md never redefines reasoning or tool usage rules
3. **Methodology principles** go in SOUL.md -- brief compass statements ("verify before trusting")
4. **Methodology procedures** go in skills -- detailed protocols ("Step 1: write failing test, Step 2: ...")
5. **If in doubt:** Can it fit in 1-2 sentences? SOUL.md. Does it need steps/examples? Skill.

## Token Budget Tracking

System prompt size is logged per agent per message:

```
tracing::info!(agent, prompt_bytes, prompt_approx_tokens, "system_prompt_size")
```

Visible in:

- UI Logs page (real-time)
- `journalctl --user -u opex-core` on Pi
- Grep: `grep system_prompt_size` in logs

Target budgets:

- SOUL.md: ~2K tokens (~8KB) across all phases
- Total system prompt: monitor per model context window (no hard limit)
