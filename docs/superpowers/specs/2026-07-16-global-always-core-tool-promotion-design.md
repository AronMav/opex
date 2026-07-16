# Global `always_core` — promote extension tools into the native `tools[]`

**Date:** 2026-07-16
**Status:** Design approved; full spec review complete (F1 subagent coverage added, F2 config placement fixed, F3/F4 folded in); pending final user sign-off
**Author:** brainstorming session

## Problem

Weak-prompt-adherence models (in prod: `glm-5.2`) sometimes "invoke" MCP/extension
tools as free-form assistant **content** instead of going through the `tool_use`
dispatcher indirection, e.g.:

```text
sequentialthinking
{"thought": "..."}
```

or `<sequentialthinking>...</sequentialthinking>`. Because no native function of
that name exists in the per-turn `tools[]` schema, the OpenAI-compatible provider
streams it as `delta.content` and the user sees raw JSON/XML. Existing mitigations
(`HallucinatedToolFilter` live suppressor + post-hoc strip + hardened tool hint)
only **hide** the leak — they do not make the tool actually run.

Root cause is two-sided:

- **Primary — the model.** Strong models follow the `tool_use` indirection reliably;
  weak open-weight models revert to inline text/XML tool calls.
- **Contributing — the architecture.** OPEX deliberately hides extension/MCP tools
  behind the `tool_use` dispatcher (token economy). That indirection is exactly what
  weak models fumble.

## Goal

Let an operator promote specific extension tools (initially `sequentialthinking`)
into the native `tools[]` array for **every** agent running in dispatcher mode —
current and future, **including dispatcher-mode subagents** — via a single global
config knob. A promoted tool becomes a real function schema (no indirection for the
model to fumble) and is simultaneously **excluded** from the dispatcher catalogue,
tool hint, and hallucination suppressor so there is no "native but the prompt says
use `tool_use`" contradiction.

Subagents matter here: a non-base subagent inherits its parent's
`tool_dispatcher.enabled` by default
([`dispatch_for_subagent_decision`](../../../crates/opex-core/src/agent/engine/tool_executor.rs#L166)),
so the very agents this feature targets spawn dispatcher-mode subagents on the same
weak model with the same hallucination exposure. Subagents build tools on a
**separate** path ([`subagent_runner.rs`](../../../crates/opex-core/src/agent/pipeline/subagent_runner.rs))
that does not touch the `context_builder` retain, so they must be covered
explicitly (§4). Base subagents always get full native tools (dispatcher off for
them), so they are unaffected either way.

Non-goals: changing per-agent `[agent.tool_dispatcher] core_extra` semantics;
changing the auto-promotion (`promotion_max`) mechanism; adding defense-in-depth
that keeps a promoted tool in the suppressor list (explicitly deferred — see below).

## Why not the alternatives

- **Per-agent config only (edit each server TOML's `core_extra`).** Zero code, but
  manual, per-agent, and fragile: future dispatcher agents are not covered, and the
  configs live only on the prod server. Rejected given the "all + future agents"
  requirement.
- **Hardcoded Rust constant.** Same global reach as the config knob but requires a
  rebuild to change the set and runs against the project's config-driven culture.
  Rejected.

## Existing mechanism this builds on

When `[agent.tool_dispatcher] enabled = true`, the native `tools[]` array is
partitioned to `static_core ∪ core_extra ∪ promoted`
([`context_builder.rs:706`](../../../crates/opex-core/src/agent/context_builder.rs#L706)).
`core_extra` already promotes named tools natively, but it is **not** excluded from
the dispatcher catalogue/hint (a pre-existing double-listing). This design adds a
*global* promotion list that IS excluded, leaving per-agent `core_extra` untouched.

Key enabler: the runtime `AgentConfig` already holds `app_config: Arc<AppConfig>`
([`agent_config.rs:30`](../../../crates/opex-core/src/agent/agent_config.rs#L30)), so
the global list is reachable at every call site via `cfg().app_config` — no new
trait/struct threading required.

## Design

### 1. Config

New top-level section in `opex.toml` (parsed into `AppConfig`). The section name
`[tool_dispatcher]` is distinct from the per-agent `[agent.tool_dispatcher]`, so
there is no collision.

```toml
[tool_dispatcher]
always_core = ["sequentialthinking"]
```

New struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GlobalToolDispatcherConfig {
    /// Extension tool names always promoted into the native tools[] array for
    /// EVERY dispatcher-enabled agent, and excluded from the dispatcher
    /// catalogue/hint/suppressor. Subject to per-agent deny-list and
    /// required_base at context-build time. Empty by default (no behaviour
    /// change).
    #[serde(default)]
    pub always_core: Vec<String>,
}
```

Field on `AppConfig`: `#[serde(default)] pub tool_dispatcher: GlobalToolDispatcherConfig`.
Empty default ⇒ backward compatible.

### 2. Native promotion — `context_builder.rs` retain

Extend the partition `retain` (currently `core_names ∪ core_extra`) to also keep
names in `app_config.tool_dispatcher.always_core`:

```rust
all_tools.retain(|t| {
    core_names.contains(t.name.as_str())
        || core_extra.contains(&t.name)
        || always_core.contains(&t.name)
});
```

`retain` runs **after** `filter_tools_by_policy`, so deny-list and `required_base`
are still honoured (a filtered-out tool is absent from `all_tools`; promotion cannot
resurrect it). If the tool is not configured for an agent (e.g. MCP absent), the set
simply keeps nothing — a safe no-op.

`always_core` is read in `DefaultContextBuilder` via the engine-based deps, which
can reach `self.cfg().app_config.tool_dispatcher.always_core`. A new deps trait
method `fn dispatcher_always_core(&self) -> &[String]` mirrors the existing
`agent_core_extra()`.

### 3. Exclusion from catalogue / hint / suppressor — `dispatcher/lookup.rs`

Add an explicit `always_core: &[String]` parameter to `build_extension_tool_list`
(and its `find_extension_tool` wrapper) and filter those names the same way static
`core` is filtered. Centralising the filter in `lookup.rs` (one place) is cleaner
than overloading the currently-always-empty `promoted` set at each call site.

All four consumers get the effect at once:

| Call site | Consumer | Effect of exclusion |
| --- | --- | --- |
| `execute.rs:185` | suppressor `known_extension_tools` | promoted tool drops out — it is native, arrives as `tool_calls` not `content`, nothing to suppress |
| `tool_use.rs:70` | `search` catalogue | no `describe` offered for an already-native tool |
| `tool_use.rs:145` | `describe` lookup | consistency (native tool not surfaced via dispatcher) |
| `context_builder.rs:480` | tool hint (top-1) | the "NOT directly callable, use `tool_use`" hint cannot fire on a native tool (would contradict the schema) |

Each call site passes `&cfg().app_config.tool_dispatcher.always_core`
(`context_builder.rs:480` via the new deps method).

### 4. Subagent coverage — `subagent_runner.rs`

Subagents do not go through `DefaultContextBuilder`; they assemble `available_tools`
in `run_subagent_with_session`. When `dispatch_for_subagent` is **true**, the current
code keeps `available_tools` at static-core only (the `if !dispatch_for_subagent`
block that injects YAML/MCP is skipped), so an `always_core` MCP/YAML tool is not
native for the subagent.

Add, for the `dispatch_for_subagent == true` case, an injection of the `always_core`
subset drawn from the already-loaded `yaml_tools` and `mcp_defs`, filtered by
`denied_for_subagent` (the subagent deny-list is never weakened) and by
`filter_tools_by_policy` (which already runs at
[`subagent_runner.rs:183`](../../../crates/opex-core/src/agent/pipeline/subagent_runner.rs#L183)).
Read the list via `executor.cfg().app_config.tool_dispatcher.always_core`.

The subagent's own dispatcher catalogue must exclude the promoted names too: the
subagent path reaches the catalogue through the same `tool_use` handler
(`build_extension_tool_list`), so the §3 `always_core` exclusion parameter already
covers it — no separate subagent catalogue change is needed beyond passing the list.

Base subagents (`dispatch_for_subagent == false`) already receive full native
YAML/MCP tools, so the promoted tool is native for them without any change.

### Explicit decision: suppressor exclusion, not defense-in-depth

A promoted tool is **removed** from the suppressor's `known_extension_tools`. Once
native, the tool schema is the correct signal; the suppressor was a workaround for
*non-native* tools. Keeping the tool in both native `tools[]` and the suppressor
list is muddy. Defense-in-depth (retain in suppressor so a still-hallucinating weak
model's inline text is also swallowed) is deferred — it is a trivial follow-up if
prod shows the model still leaks the tool as text despite the native schema.

## Data flow

```text
opex.toml [tool_dispatcher].always_core
        │  (parsed at startup; live-reload propagation unverified — see Deploy)
        ▼
AppConfig.tool_dispatcher.always_core ── Arc<AppConfig> ──► every AgentConfig
        │                                                          │
        ├── context_builder retain (main agent) ─────► native tools[]  (promotion)
        ├── subagent_runner inject (dispatcher-mode subagent) ─► native tools[]  (§4)
        └── build_extension_tool_list(always_core) ──► catalogue/hint/suppressor
                                                        (exclusion, shared by both)
```

## Error handling / edge cases

- **Empty `always_core` (default):** every added branch is a no-op; byte-for-byte
  prior behaviour. This is the backward-compat guarantee.
- **Name in `always_core` but denied / not `required_base` for an agent:** absent
  from `all_tools` → not promoted; absent from catalogue anyway. Consistent.
- **Name in `always_core` that no provider/MCP supplies:** never appears in
  `all_tools` or the catalogue → no-op.
- **Overlap with per-agent `core_extra`:** both branches keep it; `retain` is a
  boolean OR, so no duplication in `tools[]` (single ToolDefinition per name).
- **Interaction with per-session auto-promotion (`promotion_max`):** an
  `always_core` tool is native from turn 1, so the per-session promotion counter is
  simply never consulted for it — no conflict, the two mechanisms are independent.
- **Typo / unmatched name (F3):** an `always_core` entry that matches no tool from
  any source (typo, or the MCP/YAML tool is absent for every agent) silently
  no-ops. To avoid silent misconfiguration, log a `warn` at startup for each
  `always_core` name not found in the full internal + YAML + MCP tool universe.

## Testing

1. `build_extension_tool_list` unit: a name in `always_core` is absent from the
   returned list (and therefore from the suppressor-derived `known_extension_tools`).
2. Retain-partition unit: an `always_core` tool present in `all_tools` survives the
   partition into native `tools[]`; a deny-listed one does not.
3. Config parse: `AppConfig` with no `[tool_dispatcher]` section yields empty
   `always_core` (backward compatibility); with the section, the list parses.
4. Subagent injection unit: with `dispatch_for_subagent == true` and an
   `always_core` name present in `yaml_tools`/`mcp_defs`, that tool appears in the
   subagent's `available_tools`; a `denied_for_subagent` name in `always_core` does
   NOT (deny-list not weakened). Base subagent (`dispatch_for_subagent == false`)
   already has it via the full-tools path.
5. Existing `hallucinated_tool.rs` tests remain untouched and green.

## Affected files

- `crates/opex-core/src/config/mod.rs` — `GlobalToolDispatcherConfig` struct + `AppConfig` field.
- `crates/opex-core/src/agent/context_builder.rs` — retain branch + hint call-site + deps trait method.
- `crates/opex-core/src/agent/engine/context_builder.rs` — implement `dispatcher_always_core()`.
- `crates/opex-core/src/agent/dispatcher/lookup.rs` — `always_core` param + filter.
- `crates/opex-core/src/agent/pipeline/execute.rs` — pass `always_core` at call site.
- `crates/opex-core/src/agent/tool_handlers/tool_use.rs` — pass `always_core` at 2 call sites.
- `crates/opex-core/src/agent/pipeline/subagent_runner.rs` — inject `always_core` subset natively in the `dispatch_for_subagent` case (§4).
- `crates/opex-core/src/main.rs` (or config-load path) — F3 startup `warn` for unmatched `always_core` names.
- `config/opex.toml` — set `always_core = ["sequentialthinking"]`.

## Deploy

Binary rebuild (`make remote-deploy`); the promotion takes effect on restart, which
the code change requires anyway. Whether a *later* edit to the `always_core` list
(value only, no code change) hot-applies without a restart depends on whether the
`opex.toml` config-watcher reload rebuilds per-agent engines with a fresh
`Arc<AppConfig>` snapshot — the runtime `AgentConfig.app_config` is a snapshot taken
at engine construction. This is **unverified**; confirm the reload-propagation path
during implementation, and if engines are not rebuilt on reload, document that list
changes need a core restart (not just a config save).
