# Soul-layer UI editor — design

**Date:** 2026-07-16
**Status:** approved, pending implementation plan
**Related:** `docs/superpowers/specs/2026-07-09-agent-soul-foundation-design.md`,
`2026-07-14-agent-soul-emotion-layer-v1-design.md`,
`2026-07-11-agent-soul-stage-c-initiative-design.md`,
`2026-07-14-auto-approve-day-plan-budget-design.md`.
Follows the PUT-preserve fix in commit `a44a4a53`.

## Problem

The agent "soul" layer — `[agent.soul]`, `[agent.drift]`, `[agent.initiative]`,
`[agent.emotion]` — is configurable **only** by hand-editing
`config/agents/{Name}.toml` on the server. Three concrete gaps:

1. The agent detail GET response (`AgentDetailDto`) omits these four sections
   entirely — the UI never receives them.
2. `build_agent_config` (create/update payload → config) hardcodes all four to
   `::default()` (disabled), and `AgentCreatePayload` has no fields for them, so
   an agent created or edited through the API can never enable the soul layer.
3. Commit `a44a4a53` restores these sections from disk on PUT to prevent silent
   data loss — correct while they are no-payload sections, but it means UI edits
   would be impossible until the merge is reworked (see §Backend / preserve).

The README (`README.md`, `README.ru.md`) does not document the capability at all.

## Goals

- Make `soul` / `drift` / `initiative` / `emotion` editable from the agent
  editor, respecting the same validation the backend enforces.
- Preserve the disk-value fallback so a partial PUT (older client, omitted
  section) never wipes an operator's TOML config.
- Document the capability factually in the README.

## Non-goals (YAGNI)

- mood / drift-score / day-plan **dashboards** (separate audit items).
- `[agent.delegation]` UI (stays no-payload; partially covered by the tools tab).
- `SELF.md` viewer.
- Regenerating soul data or triggering reflection from the UI.

## Config surface (source of truth: `crates/opex-core/src/config/mod.rs`)

| Section | Field | Type | Range / rule (from `validate()`) | Default |
| --- | --- | --- | --- | --- |
| **soul** | `enabled` | bool | — | false |
| | `reflection_threshold` | f64 | > 0 | 150.0 |
| | `reflection_cooldown_minutes` | u64 | [0, 1440] | 60 |
| | `context_top_k` | usize | [1, 20] | 6 |
| | `context_budget_tokens` | u32 | [100, 4000] | 800 |
| | `max_events_per_session` | usize | [1, 30] | 10 |
| **drift** | `enabled` | bool | — | false |
| | `threshold` | f32 | [0.0, 2.0] | 0.15 |
| | `min_history` | usize | [2, 50] | 6 |
| | `baseline_turns` | usize | [1, 10] | 3 |
| | `correct` | bool | requires `enabled` | false |
| | `anchor` | Option\<String\> | — | none |
| **initiative** | `enabled` | bool | — | false |
| | `daily_proposal_cap` | u32 | [1, 10] | 1 |
| | `decompose` | bool | — | false |
| | `daily_plan` | bool | — | false |
| | `auto_approve_day_plan` | bool | requires `daily_plan` && `daily_token_budget > 0` | false |
| | `daily_token_budget` | u64 | > 0 when `auto_approve_day_plan` | 0 |
| **emotion** | `enabled` | bool | requires `soul.enabled` (cross-checked in `AgentConfig::load()`) | false |
| | `intensity_importance_k` | f32 | [0.0, 5.0] | 3.0 |
| | `blend_rate` | f32 | (0.0, 1.0] | 0.3 |
| | `decay_half_life_hours` | f32 | > 0 | 12.0 |

Gate: initiative endpoints are **non-base only** (M3 gate,
`agents/initiative.rs`). The editor disables the initiative section for base
agents.

## Architecture — three layers

### 1. Backend — GET DTO

`crates/opex-core/src/gateway/handlers/agents/dto_structs.rs`: add four nested
DTOs mirroring `AgentDetailCompactionDto` / `AgentDetailToolLoopDto`:
`AgentDetailSoulDto`, `AgentDetailDriftDto`, `AgentDetailInitiativeDto`,
`AgentDetailEmotionDto`, each a field on `AgentDetailDto`.

`dto.rs`: populate them from `a.soul` / `a.drift` / `a.initiative` / `a.emotion`.
These are always present on `AgentSettings` (not `Option`), so the DTO fields are
non-optional structs — no `.as_ref().map` wrapper needed.

### 2. Backend — payload + build + preserve

`schema.rs`:
- Add optional sub-structs to `AgentCreatePayload`: `soul`, `drift`,
  `initiative`, `emotion` (each `Option<...Payload>` with all fields
  `Option<T>`, matching the `AgentDetailCompactionDto` payload precedent).
- In `build_agent_config`, replace the four hardcoded `::default()` with
  `p.<section>.flatten().map(|s| Config { … }).unwrap_or_default()`.

`crud.rs` — **preserve rework (the delicate part):**
- Today `preserve_no_payload_sections` unconditionally copies delegation,
  emotion, soul, drift, initiative from disk. Once these four gain payload
  fields, unconditional copy would discard UI edits.
- New behavior: `delegation` stays unconditionally preserved (no payload). For
  `soul` / `drift` / `initiative` / `emotion`, follow the `payload_webhooks_present`
  precedent: capture presence of each payload sub-struct **before**
  `build_agent_config` consumes the payload; when a section is **absent**
  (`None`) → copy from `existing_cfg`; when **present** (`Some`) → keep the
  built value (UI wins).
- Concretely: rename/extend the helper to
  `preserve_no_payload_sections(new, existing)` (delegation only) plus a new
  `merge_soul_sections(new, existing, present: SoulSectionPresence)` where
  `SoulSectionPresence { soul, drift, initiative, emotion: bool }` is computed
  from the raw payload. This keeps the "one place to update" property.

Validation is already enforced by `AgentConfig::load()` after the TOML write
(covers ranges + cross-field), so the server remains the final barrier; no new
validation code is required backend-side.

### 3. UI — "Soul" tab

`ui/src/app/(authenticated)/agents/AgentEditDialog.tsx`:
- Add a 7th tab `soul` (Sparkles icon) to `AgentTab` and the tab list.
- Four `SwitchSection` blocks (Soul / Drift / Initiative / Emotion), each a
  `enabled` toggle plus a collapsible "Дополнительно" region holding the
  fine-grained fields with their ranges as `min`/`max`/hints.
- Extend the dialog form model + `upd()` with the ~22 new fields.
- Parse GET response (new DTO fields) → form; serialize form → PUT payload.

`ui/src/types/api.ts` + regenerate `ui/src/types/api.generated.ts` (gen-types
drift is checked in CI — must regenerate, not hand-edit the generated file).

i18n: new keys in `ui/src/i18n/locales/en.json` and `ru.json` (section titles,
field labels, hints, gating notes).

#### Cross-field gating (mirrors server `validate()`, prevents invalid PUT)

- **Emotion** section disabled while `soul.enabled === false`; note explains the
  dependency.
- **Drift `correct`** toggle disabled while `drift.enabled === false`.
- **Initiative `auto_approve_day_plan`** disabled unless `daily_plan === true`;
  when enabled, `daily_token_budget` field is required (> 0) and gated.
- **Initiative** section disabled for base agents with a "non-base only" note.

Field `min`/`max` come directly from the ranges table above so the UI cannot
submit an out-of-range value.

### README

`README.md` and `README.ru.md`: add one factual row to the "What's actually
different" table (English) / its Russian mirror. Framing: autobiographical
memory + periodic reflection + proactive initiative, **opt-in and per-agent,
default-off**. No "soul"/"consciousness" marketing language (per positioning
note). `docs/README.md` (28-line index) is untouched.

## Error handling

Client-side gating catches the common cross-field / range errors before the
request. The server's `AgentConfig::load()` remains the authority; its errors
already surface via the dialog's existing error banner.

## Testing (TDD)

**Rust (`schema.rs` / `crud.rs` unit tests):**
- `build_agent_config` maps a populated soul/drift/initiative/emotion payload
  into the config (not default).
- `build_agent_config` with absent soul sections yields defaults (create path).
- Merge: payload section **absent** → preserved from disk; payload section
  **present** → UI value wins over disk (regression guard on the `a44a4a53`
  fix — this is the highest-risk interaction).
- delegation still unconditionally preserved.

**UI (vitest):**
- Form ↔ payload round-trip for all four sections.
- Gating: emotion disabled without soul; drift.correct disabled without drift;
  initiative auto-approve disabled without daily_plan; initiative disabled for
  base agent.

## Files touched

Backend: `agents/dto_structs.rs`, `agents/dto.rs`, `agents/schema.rs`,
`agents/crud.rs`.
UI: `AgentEditDialog.tsx`, `types/api.ts`, `types/api.generated.ts` (regen),
`i18n/locales/{en,ru}.json`.
Docs: `README.md`, `README.ru.md`.
