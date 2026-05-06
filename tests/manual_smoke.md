# Manual Smoke Tests

Lightweight checklist for behaviors that are too coupled to the live runtime to
cover with isolated unit tests, but still need a documented repro path.

## Tool Dispatcher Partition (Task 16)

Verify that enabling the dispatcher actually shrinks what the LLM sees.

**Setup**

1. Pick an agent with a non-trivial tool surface (many YAML tools and/or MCP
   tools, e.g. the base agent).
2. With the dispatcher OFF, send any chat message and capture the
   `tools` array length the provider receives. Expected: 30+ entries
   (static core + every YAML/MCP tool that survives policy filtering, modulo
   `max_tools_in_context` top-K).

**Toggle**

3. In `config/agents/{Name}.toml`, set:

   ```toml
   [agent.tool_dispatcher]
   enabled = true
   # core_extra = ["foo_tool"]   # optional: pin extra names into per-turn core
   ```

4. Hot-reload the config (or restart the core). Send a fresh chat message in
   a NEW session (so `promoted` starts empty).

**Expected after toggle**

- Provider request `tools` array shrinks to ~10 entries (the static core list
  from `pipeline::tool_defs::static_core_tool_names()` plus any
  `core_extra` names that survive policy filtering).
- The system prompt now contains an `# Extension Tools (load on demand)`
  section with the `tool_use(action="search"|"describe"|"call")` workflow.
- Calling an extension tool via `tool_use(action="call", ...)` works; after
  `PROMOTION_THRESHOLD` successful direct calls in the same session, that
  tool name appears in the tools array on the next turn (auto-promotion).
- Disabling the dispatcher returns the tools array to the legacy size on the
  next turn.
