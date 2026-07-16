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

**Additional cross-field rules** enforced in `AgentConfig::load()`
(`config/mod.rs:2171-2203`), beyond the per-section `validate()`:

- `initiative.daily_plan = true` requires a configured `[agent.heartbeat]`
  (heartbeat drives day-plan generation/advancement).
- `initiative.daily_plan = true` requires `initiative.enabled = true`.
- `emotion.enabled = true` requires `soul.enabled = true`.

Gate: initiative endpoints are **non-base only** (M3 gate,
`agents/initiative.rs:58,178,203,296,315` → 403 "initiative is non-base only").
The editor disables the initiative section for base agents.

**Budget disambiguation:** `InitiativeConfig.daily_token_budget` (the auto-approve
day-plan ceiling, this feature) is a DIFFERENT field from
`AgentSettings.daily_budget_tokens` (`config/mod.rs:1040`, the whole-agent daily
budget already surfaced on the general tab as `field_daily_budget`). Do not
conflate them; the Soul tab edits only `daily_token_budget`.

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
  `Option<T>`, matching the existing optional `compaction` / `tool_loop`
  payload sub-structs in `AgentCreatePayload`).
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

**Server-side validation — NEW code required (spec correction).** The
create/update handlers do NOT currently validate config sections: `crud.rs`
calls `cfg.to_toml()` and writes to disk (`crud.rs:787-803`, and the create path
~490) with only `validate_agent_name()` run — never the section `validate()`
functions or the cross-field checks. `AgentConfig::load()` runs those checks only
at startup/reload; the handler builds the running engine from the in-memory
(unvalidated) `cfg`, so an invalid soul/emotion/drift/initiative PUT would be
written to disk, returned as 200, and only rejected on the next restart (where
`load_agent_configs` warns and **skips** the agent — `config/mod.rs:2230`).

Fix: extract the load-time validation (per-section `validate()` + the four
cross-field `bail!`s at `config/mod.rs:2129-2203`) into a reusable
`AgentConfig::validate_sections() -> Vec<String>`. Call it from `load()` (replacing
the inline block) AND from both the create and update handlers **before writing
TOML**; on non-empty errors return `400` with the messages. Client-side gating
(below) reduces but cannot eliminate invalid PUTs, so this server barrier is
required for correctness, not optional.

### 3. UI — "Soul" tab

**Form-state ownership (spec correction).** The dialog form model does NOT live
in `AgentEditDialog.tsx` — it lives in the parent
`ui/src/app/(authenticated)/agents/page.tsx`. `AgentEditDialog` receives `form`
and `upd` as props (`AgentEditDialog.tsx:139-140`). The pieces to extend are all
in `page.tsx`:

- `FormState` interface — flat, ~70 fields today; numbers held as **strings**,
  bools as `boolean` (`page.tsx:61-133` region). Add ~22 flat fields:
  `soulEnabled: boolean`, `soulReflectionThreshold: string`, `driftEnabled`,
  `driftThreshold: string`, `driftCorrect: boolean`, `driftAnchor: string`,
  `initiativeEnabled`, `initiativeDailyProposalCap: string`,
  `initiativeDecompose`, `initiativeDailyPlan`, `initiativeAutoApprove`,
  `initiativeDailyTokenBudget: string`, `emotionEnabled`, `emotionK: string`,
  `emotionBlendRate: string`, `emotionHalfLife: string`, etc.
- `emptyForm` (`page.tsx:56`) — defaults for the new fields (matching config
  defaults from the table above).
- `detailToForm(d: AgentDetail)` (`page.tsx:115`) — read the new DTO sections.
- `formToPayload(f)` (`page.tsx:184`) — serialize to the new payload sub-structs;
  `parseInt/parseFloat` the string number fields; map empty `driftAnchor` → `null`
  (precedent: `tool_matcher` empty→null at `page.tsx:1002`).

`upd()` is generic (`Partial<FormState>`, `page.tsx:518`) — it does **not** need
changing.

`AgentEditDialog.tsx` (presentation only):

- Add `"soul"` to the `AgentTab` union (`:162`) and an `AGENT_TABS` entry
  `{ id: "soul", icon: Sparkles, labelKey: "agents.tab_soul" }` (`:164-171`).
  **`Sparkles` must be added to the lucide import** (`:41`) — not currently
  imported.
- Add a new tab panel `<div ... activeTab === "soul" ...>` inside the tab grid
  (mirroring the existing panels at `:216-601`).
- Four `SwitchSection` blocks (Soul / Drift / Initiative / Emotion). `SwitchSection`
  (`:742-768`) already renders `toggle + {enabled && children}`.
- **NEW component work:** the "Дополнительно" collapsible inside each section is
  not an existing pattern. Build it on `ui/src/components/ui/collapsible.tsx`
  (exists, unused in `agents/`). Fine-grained fields use `<Input type="number">`
  with `min`/`max` from the ranges table. `driftAnchor` needs a **textarea**
  (no textarea exists in this dialog today — add one).

`ui/src/types/api.generated.ts` is **generated from the Rust DTOs** via
`make gen-types` (`cargo run --features ts-gen --bin gen_ts_types`). The real
source edit is `dto_structs.rs` (§1); after that, run `make gen-types` to
regenerate — never hand-edit the generated file (CI checks drift). `types/api.ts`
is a re-export facade; touch it only to add friendly aliases if desired.

i18n: new keys in `ui/src/i18n/locales/en.json` and `ru.json` (flat keys like
`agents.tab_soul`). Budget ~60 keys **per language** (tab + 4 section titles +
~22 field labels + number-field hints + 4 gating notes), duplicated en/ru.

#### Cross-field gating (NEW UI code — no existing precedent)

The current editor has no reusable "disabled control + explanatory note" pattern:
`editingBase` today only appends a text paragraph without disabling anything
(`AgentEditDialog.tsx:415-421`). All four gates below are new UI code, mirroring
the server cross-field rules:

- **Emotion** section disabled while `soulEnabled === false`; note explains it.
- **Drift `correct`** toggle disabled while `driftEnabled === false`.
- **Initiative `auto_approve_day_plan`** disabled unless `initiativeDailyPlan`;
  when on, `daily_token_budget` is required (> 0) and gated.
- **Initiative `daily_plan`** requires a configured heartbeat (schedule tab) AND
  `initiative.enabled` — surface a note (backend rejects otherwise). Gating on
  heartbeat-presence is a note/warning since heartbeat lives on another tab.
- **Initiative** section disabled for base agents (`editingBase` prop, `:159,189`)
  with a "non-base only" note.

Field `min`/`max` come directly from the ranges table. `daily_token_budget` is
`u64`; cap the input `max` (e.g. 10^12) to stay within JS `number` safe-integer
range — it is stored as a string and JSON-serialized as a number.

### README

`README.md` and `README.ru.md`: add one factual row to the "What's actually
different" table (English) / its Russian mirror. Framing: autobiographical
memory + periodic reflection + proactive initiative, **opt-in and per-agent,
default-off**. No "soul"/"consciousness" marketing language (per positioning
note). `docs/README.md` (28-line index) is untouched.

## Error handling

Client-side gating catches the common cross-field / range errors before the
request. The **new** handler-side `validate_sections()` (see Backend §2) is the
authoritative barrier — it returns `400` with the section error messages, which
surface via the dialog's existing error banner. Relying on `AgentConfig::load()`
alone is insufficient because the handler does not reload from disk.

## Testing (TDD)

**Rust:**

- `build_agent_config` maps a populated soul/drift/initiative/emotion payload
  into the config (not default).
- `build_agent_config` with absent soul sections yields defaults (create path).
- Merge: payload section **absent** → preserved from disk; payload section
  **present** → UI value wins over disk (regression guard on the `a44a4a53`
  fix — this is the highest-risk interaction).
- delegation still unconditionally preserved.
- `validate_sections()` returns the same errors as the old inline `load()` block
  (extraction is behavior-preserving), incl. the emotion-requires-soul and
  daily_plan-requires-heartbeat cross-checks.
- create/update handler returns `400` (not `200` + bad write) for an invalid
  section payload (e.g. `emotion.enabled` without `soul.enabled`).

**UI (vitest):**

- `detailToForm` ↔ `formToPayload` round-trip for all four sections (incl.
  string↔number and `driftAnchor` empty↔null).
- Gating: emotion disabled without soul; drift.correct disabled without drift;
  initiative auto-approve disabled without daily_plan; initiative disabled for
  base agent.

## Files touched

Backend: `config/mod.rs` (extract `validate_sections()`), `agents/dto_structs.rs`,
`agents/dto.rs`, `agents/schema.rs`, `agents/crud.rs` (payload-gated merge +
call `validate_sections()` before write in create & update).
UI: **`agents/page.tsx`** (`FormState`, `emptyForm`, `detailToForm`,
`formToPayload` — the actual form logic), `agents/AgentEditDialog.tsx` (tab +
panels + gating + Sparkles import), `components/ui/collapsible.tsx` (reuse),
`types/api.generated.ts` (regen via `make gen-types`), optionally `types/api.ts`
(aliases), `i18n/locales/{en,ru}.json`.
Docs: `README.md`, `README.ru.md`.
