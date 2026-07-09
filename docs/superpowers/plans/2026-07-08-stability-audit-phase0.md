# Stability Audit — Phase 0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce a ranked, adversarially-verified backlog of stability/security findings across the whole OPEX system (Rust core, toolgate, channels, UI, infra) — read-only, no code changes.

**Architecture:** Seed cheap deterministic signals (clippy / cargo audit / grep / tsc / npm audit / gen-types drift), feed them to a multi-agent Workflow (finders per `layer × axis` cell → 3-lens adversarial verify per finding → JS dedup+rank → completeness critic), then assemble the backlog markdown. The backlog is the sole deliverable; fixing happens in later waves (each its own plan).

**Tech Stack:** Workflow tool (JS orchestration), Grep/Read/Glob (finders), ripgrep, cargo clippy/audit, node/tsc/npm, ssh to the deploy server for Rust signals.

## Global Constraints

Copied verbatim from the spec (`docs/superpowers/specs/2026-07-08-stability-hardening-program-design.md` §9). Phase 0 is **read-only**; the deploy/fix constraints bind waves 1..N.

- Rust + rustls-tls only, no OpenSSL.
- Deploy target: `aronmav@188.246.224.118`, `SERVER_DIR=~/opex`, build on server (`make remote-deploy`). Pi retired.
- Backward compat: fixes must not break API contracts or migrations.
- No push/deploy without explicit user confirmation.
- CI-triple before any push: `cargo test --workspace` + `tsc` + gen-types drift.
- Vitest/`npm test` only from `ui/`; local Windows does not run Rust tests reliably → Rust-signal authority is the server.
- No Co-Authored-By in commits; incremental deploy only.
- Avoid `scp` into `~/opex-src` without commit — it blocks `git pull --ff-only`.

---

### Task 1: Scaffold audit output + collect local seed signals

**Files:**
- Create: `docs/audits/2026-07-08-stability-audit-findings.md` (placeholder, filled in Task 4)
- Create: scratchpad `stability-seed-leads.md` (intermediate leads bundle)

**Interfaces:**
- Produces: a leads bundle file at `%SCRATCH%/stability-seed-leads.md` where `%SCRATCH%` = `C:\Users\AronMav\AppData\Local\Temp\claude\d--GIT-bogdan-opex\<session>\scratchpad`. Task 3 passes its contents as `args.leads`.

- [ ] **Step 1: Create the audits directory + placeholder**

```bash
mkdir -p d:/GIT/bogdan/opex/docs/audits
printf '# OPEX Stability Audit — Findings (Phase 0)\n\n_Backlog assembled by the Phase 0 audit workflow — see plan 2026-07-08-stability-audit-phase0.md._\n' > d:/GIT/bogdan/opex/docs/audits/2026-07-08-stability-audit-findings.md
```

- [ ] **Step 2: Inventory the Rust crash-surface (panic sites)**

Use the Grep tool (output_mode "count") across `crates/` for `\.unwrap\(\)|\.expect\(|panic!\(|unreachable!\(|\[\s*0\s*\]|todo!\(` — record the per-file counts. This is a lead, not a verdict; finders confirm reachability.

Expected: a ranked list of files by panic-marker density (the earlier whole-repo count was ~2034 non-test hits).

- [ ] **Step 3: Inventory silent-failure markers**

Grep `crates/` and `toolgate/` and `channels/src/` for `let _ =|\.ok\(\);|catch\s*\(|except\s*:|unwrap_or_default\(\)` — record hotspots. Lead only.

- [ ] **Step 4: Run local TypeScript + npm signals**

```bash
cd d:/GIT/bogdan/opex/ui && npx tsc --noEmit 2>&1 | head -60
cd d:/GIT/bogdan/opex/ui && npm audit --omit=dev 2>&1 | tail -30
```

Expected: tsc clean or a short list of type errors; npm audit advisory summary. Capture both into the leads bundle.

- [ ] **Step 5: Write the leads bundle**

Write all of the above (panic hotspots, silent-failure hotspots, tsc output, npm-audit summary) into `%SCRATCH%/stability-seed-leads.md` under clear headings (`## panic-sites`, `## silent-failures`, `## tsc`, `## npm-audit`). Leave room to append server signals in Task 2.

- [ ] **Step 6: Verify the bundle exists and is non-empty**

```bash
wc -l "$SCRATCH/stability-seed-leads.md"
```

Expected: > 20 lines.

---

### Task 2: Collect server-side Rust seed signals (best-effort)

**Files:**
- Modify: `%SCRATCH%/stability-seed-leads.md` (append `## clippy` and `## cargo-audit` sections)

**Interfaces:**
- Consumes: the leads bundle from Task 1.
- Produces: the same bundle, enriched. If the server is unreachable, the bundle is used as-is (spec §6: seed is best-effort, does not block the audit).

- [ ] **Step 1: Run clippy on the server**

```bash
ssh aronmav@188.246.224.118 'cd ~/opex-src && ~/.cargo/bin/cargo clippy --all-targets --workspace 2>&1 | grep -E "^(warning|error)" | sort | uniq -c | sort -rn | head -60'
```

Expected: aggregated clippy warning/error counts by message. If ssh fails or times out, note "clippy: unavailable" in the bundle and continue.

- [ ] **Step 2: Run cargo audit on the server**

```bash
ssh aronmav@188.246.224.118 'cd ~/opex-src && ~/.cargo/bin/cargo audit 2>&1 | grep -A3 -E "^(Crate|ID|Warning|Error):" | head -80'
```

Expected: RustSec advisories (recall `.cargo/audit.toml` ignores dev-only RUSTSEC-2026-0205). If unavailable, note it and continue.

- [ ] **Step 3: Append both to the leads bundle**

Append `## clippy` and `## cargo-audit` sections (or their "unavailable" notes) to `%SCRATCH%/stability-seed-leads.md`.

- [ ] **Step 4: Verify**

Grep the bundle for the four-plus section headers.

Expected: `## panic-sites`, `## silent-failures`, `## tsc`, `## npm-audit`, `## clippy`, `## cargo-audit` all present.

---

### Task 3: Author the audit Workflow script

**Files:**
- Create: `%SCRATCH%/stability-audit.workflow.js`

**Interfaces:**
- Consumes: leads bundle (passed as `args.leads` at run time in Task 4).
- Produces: on run, returns `{ confirmed: Finding[], plausible: Finding[], refutedCount: number, gaps: Gap[] }` where `Finding` matches `FINDING_SCHEMA` plus a `verdict` and `score`, and `Gap = {area, axes, why}`.

- [ ] **Step 1: Write the complete Workflow script**

Write this file verbatim (it is plain JS — no TypeScript, no `Date.now`/`Math.random`):

```javascript
export const meta = {
  name: 'opex-stability-audit-phase0',
  description: 'Read-only stability/security audit: finders per layer×axis, 3-lens adversarial verify, dedup+rank, completeness critic',
  phases: [
    { title: 'Find' },
    { title: 'Verify' },
    { title: 'Critic' },
  ],
}

// ── Axes (see spec §4) ───────────────────────────────────────────────
const AXES = {
  1: 'crash-surface / panics (unwrap/expect/panic!, str byte-offset slicing, indexing, overflow, unhandled exceptions)',
  2: 'error-handling & silent failures (swallowed errors, masking fallbacks, silently-dying background tasks)',
  3: 'concurrency, races, leaks (TOCTOU, lock scope, reqwest without timeout, task/handle leaks, cancellation correctness)',
  4: 'security authz/isolation (IDOR/scope, SSRF, secret leakage, SQL/path-traversal/provenance injection)',
  5: 'API contracts & data integrity (SSE type drift, gen-types drift, unsafe/irreversible migrations, serde-rename mismatch)',
  6: 'resource exhaustion / DoS (unbounded input, rate-limit correctness, memory blowup, runaway loops)',
  7: 'cross-layer seams (channel-WS session correctness, core↔toolgate multipart/HMAC, deploy seams)',
  8: 'dead code & config drift (unused modules/handlers, deprecated tables still queried, dead config keys, .env policy)',
}
const ax = (...ns) => ns.map(n => `  - axis ${n}: ${AXES[n]}`).join('\n')

// ── Finder grid (spec §5): ~25 curated layer×area cells ──────────────
const CELLS = [
  { id: 'core-pipeline', layer: 'rust-core', area: 'crates/opex-core/src/agent/pipeline/ (execute, bootstrap, finalize, behaviour, sink, handlers)', axes: [1,2,3,6] },
  { id: 'core-providers', layer: 'rust-core', area: 'crates/opex-core/src/agent/providers/ (openai, anthropic, google, http, routing, factory, registry) — note recent version-aware URL churn', axes: [1,2,3,5] },
  { id: 'core-chat-sse', layer: 'rust-core', area: 'crates/opex-core/src/gateway/handlers/chat/ (sse, sse_converter, resume, streaming_db, openai_compat, misc)', axes: [1,2,5,6] },
  { id: 'core-channel-ws', layer: 'rust-core', area: 'crates/opex-core/src/gateway/handlers/channel_ws/ (reader, writer, dispatcher, handshake, inline, session_locks)', axes: [3,7] },
  { id: 'core-files', layer: 'rust-core', area: 'crates/opex-core/src/gateway/handlers/files.rs + agent/file_handler_worker.rs + agent/handler_registry.rs + agent/provenance.rs', axes: [2,3,4,7] },
  { id: 'core-sessions', layer: 'rust-core', area: 'crates/opex-core/src/gateway/handlers/sessions.rs + db session lifecycle (claim/reentry/timeline)', axes: [3,4] },
  { id: 'core-tools-yaml', layer: 'rust-core', area: 'crates/opex-core/src/tools/ (yaml loader, engine_dispatch, ssrf)', axes: [4,6] },
  { id: 'core-tools-system', layer: 'rust-core', area: 'crates/opex-core/src/agent/tool_registry.rs + pipeline/handlers (workspace_write/edit/read, code_exec, agent, memory)', axes: [1,4] },
  { id: 'core-memory', layer: 'rust-core', area: 'crates/opex-core/src/memory.rs + memory/watcher.rs (hybrid search, embedding calls, watcher task)', axes: [1,2,3] },
  { id: 'core-secrets', layer: 'rust-core', area: 'crates/opex-core/src/secrets.rs (ChaCha20Poly1305, scoped resolution, leakage)', axes: [4] },
  { id: 'core-db', layer: 'rust-core', area: 'crates/opex-core/src/db/ (raw sqlx queries — dynamic SQL, assumptions on nullability)', axes: [4,5] },
  { id: 'core-procman', layer: 'rust-core', area: 'crates/opex-core/src/process_manager/ + containers/ (child kill_on_drop, restart, leaks)', axes: [2,3] },
  { id: 'core-workspace', layer: 'rust-core', area: 'crates/opex-core/src/agent/workspace.rs (is_read_only, path validation, traversal)', axes: [4] },
  { id: 'core-middleware', layer: 'rust-core', area: 'crates/opex-core/src/gateway/ auth + rate-limit + CORS middleware + uploads signed-URL', axes: [4,6] },
  { id: 'watchdog', layer: 'rust-sat', area: 'crates/opex-watchdog/src/ (inactivity monitor, alert routing)', axes: [2,3] },
  { id: 'memworker', layer: 'rust-sat', area: 'crates/opex-memory-worker/src/ (task queue poll, reqwest timeout, stuck-task recovery)', axes: [2,3] },
  { id: 'types-db', layer: 'rust-sat', area: 'crates/opex-types/ + crates/opex-db/ (contract types, gen-types drift source)', axes: [5] },
  { id: 'toolgate-handlers', layer: 'toolgate', area: 'toolgate/handlers/ (builtin + workspace) + job runner (HMAC X-Job-Token, tempfile bytes, exceptions)', axes: [1,2,4] },
  { id: 'toolgate-ctx', layer: 'toolgate', area: 'toolgate/ ctx API + endpoints + normalize.py + providers/', axes: [2,4,6] },
  { id: 'channels-ws', layer: 'channels', area: 'channels/src/ adapters + WS client + handshake (Ready/Config ordering, Config-wait, message drop)', axes: [2,3,7] },
  { id: 'channels-upload', layer: 'channels', area: 'channels/src/ upload path + formatting (multipart Content-Length, proxy-upload)', axes: [2,7] },
  { id: 'ui-streaming', layer: 'ui', area: 'ui/src/stores/ streaming-renderer.ts + chat-store + chat/* + sse-events.ts (reconnect, overlay dedup, event drift)', axes: [2,3,5] },
  { id: 'ui-auth', layer: 'ui', area: 'ui/src/stores/auth-store.ts + api layer (token handling, unauthorized flow, ws-ticket)', axes: [4,2] },
  { id: 'infra-migrations', layer: 'infra', area: 'migrations/ (destructive/irreversible ops, ordering, nullable assumptions, constraint gaps)', axes: [5] },
  { id: 'infra-deploy', layer: 'infra', area: 'scripts/server-deploy.sh + scripts/deploy-ui.sh + docker/ + config/opex.toml + systemd units (deploy seams, .env policy)', axes: [7,8] },
]

// ── Schemas ──────────────────────────────────────────────────────────
const FINDING_SCHEMA = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          axis: { type: 'integer' },
          file: { type: 'string' },
          line: { type: 'integer' },
          summary: { type: 'string' },
          failure_scenario: { type: 'string' },
          evidence: { type: 'string' },
          severity: { type: 'string', enum: ['S0', 'S1', 'S2', 'S3'] },
          blast_radius: { type: 'string', enum: ['whole-system', 'subsystem', 'single-flow'] },
          proposed_fix: { type: 'string' },
        },
        required: ['axis', 'file', 'summary', 'failure_scenario', 'severity', 'blast_radius'],
      },
    },
  },
  required: ['findings'],
}
const VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    refuted: { type: 'boolean' },
    reasoning: { type: 'string' },
  },
  required: ['refuted', 'reasoning'],
}
const GAP_SCHEMA = {
  type: 'object',
  properties: {
    gaps: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          area: { type: 'string' },
          axes: { type: 'array', items: { type: 'integer' } },
          why: { type: 'string' },
        },
        required: ['area', 'why'],
      },
    },
  },
  required: ['gaps'],
}

// ── Prompt builders ──────────────────────────────────────────────────
const REPO = 'd:/GIT/bogdan/opex'
const finderPrompt = (cell, leads) => `You are a stability & security auditor for the OPEX codebase (repo root ${REPO}).
Inspect ONLY this area: ${cell.area}
Hunt these axes:
${ax(...cell.axes)}

Seed leads from tooling (clippy/audit/grep/tsc — use as hints, not gospel; confirm in the real code):
${leads}

Use Read/Grep/Glob to inspect the ACTUAL code before reporting. Report only real, specific defects with a concrete file, line, and a plausible production failure scenario. Do NOT report style nits, formatting, or hypotheticals you cannot ground in the code. Prefer fewer, high-signal findings. For each, fill the schema; set severity honestly (S0 = reachable-in-prod crash/data-loss/security; S1 = stability degradation under realistic load; S2 = bounded correctness/contract; S3 = dead code/hygiene).`

const LENSES = {
  reachability: 'Can production inputs actually reach this code path? Trace callers/entry points.',
  guards: 'Is this already guarded/validated upstream (middleware, earlier checks, type invariants)?',
  reproduction: 'Given the real types and control flow, does the described failure actually occur?',
}
const verifyPrompt = (f, lens) => `You are an ADVERSARIAL verifier. A prior auditor claims this defect in ${REPO}:
  file: ${f.file}:${f.line || '?'}
  axis ${f.axis}: ${f.summary}
  failure scenario: ${f.failure_scenario}
  evidence: ${f.evidence || '(none given)'}

Your job is to REFUTE it. Lens for this pass: ${LENSES[lens]}
Read the actual code and surrounding context. If you cannot positively confirm the defect is real AND reachable, return refuted=true. Only return refuted=false when you have concrete evidence the defect holds.`

// ── Ranking (pure JS) ────────────────────────────────────────────────
const SEV = { S0: 4, S1: 3, S2: 2, S3: 1 }
const BLAST = { 'whole-system': 3, subsystem: 2, 'single-flow': 1 }
const scoreOf = (f) => SEV[f.severity] * 100 + (f.verdict === 'CONFIRMED' ? 20 : 10) + (BLAST[f.blast_radius] || 1)
const keyOf = (f) => `${(f.file || '').toLowerCase()}:${f.line || 0}:${f.axis}`

// ── Run ──────────────────────────────────────────────────────────────
const leads = (args && args.leads) ? args.leads : '(no seed leads provided)'

// Stage 1+2: find then 3-lens verify, pipelined per cell
const perCell = await pipeline(
  CELLS,
  (cell) => agent(finderPrompt(cell, leads), { label: `find:${cell.id}`, phase: 'Find', schema: FINDING_SCHEMA }),
  (res, cell) => {
    const findings = (res && res.findings) ? res.findings : []
    return parallel(findings.map((f) => () =>
      parallel(Object.keys(LENSES).map((lens) => () =>
        agent(verifyPrompt(f, lens), { label: `verify:${cell.id}:${lens}`, phase: 'Verify', schema: VERDICT_SCHEMA })
      )).then((votes) => {
        const refuted = votes.filter(Boolean).filter((v) => v.refuted).length
        const alive = refuted < 2 // majority (>=2 of 3) refute kills it
        const confirmed = votes.filter(Boolean).every((v) => v.refuted === false)
        return { ...f, layer: cell.layer, cell: cell.id, alive, verdict: confirmed ? 'CONFIRMED' : 'PLAUSIBLE' }
      })
    ))
  }
)

// Stage 3: flatten, drop refuted, dedup cross-cell, rank
const all = perCell.flat().filter(Boolean)
const refutedCount = all.filter((f) => !f.alive).length
const seen = new Set()
const kept = []
for (const f of all.filter((f) => f.alive).sort((a, b) => scoreOf(b) - scoreOf(a))) {
  const k = keyOf(f)
  if (seen.has(k)) continue
  seen.add(k)
  kept.push({ ...f, score: scoreOf(f) })
}
const confirmed = kept.filter((f) => f.verdict === 'CONFIRMED')
const plausible = kept.filter((f) => f.verdict === 'PLAUSIBLE')
log(`finders done: ${all.length} raw, ${refutedCount} refuted, ${kept.length} kept (${confirmed.length} confirmed / ${plausible.length} plausible)`)

// Stage 4: completeness critic
const coveredSummary = CELLS.map((c) => `${c.id} [${c.axes.join(',')}]`).join('; ')
const critic = await agent(
  `Audit coverage review for OPEX (${REPO}). These cells were covered: ${coveredSummary}. ${kept.length} findings survived verification. Identify GAPS: any layer×area or axis (see spec §4/§5) that was NOT meaningfully covered, or a cross-layer seam left unverified. Return concrete gaps only; empty if coverage is complete.`,
  { label: 'critic:coverage', phase: 'Critic', schema: GAP_SCHEMA }
)
const gaps = (critic && critic.gaps) ? critic.gaps : []

// Optional gap-fill round (one extra finder per gap)
let gapFindings = []
if (gaps.length) {
  log(`critic found ${gaps.length} gaps — running gap-fill finders`)
  const extra = await parallel(gaps.map((g, i) => () =>
    agent(finderPrompt({ id: `gap${i}`, area: g.area, axes: (g.axes && g.axes.length ? g.axes : [1,2,3,4]) }, leads),
      { label: `find:gap${i}`, phase: 'Find', schema: FINDING_SCHEMA })
  ))
  gapFindings = extra.filter(Boolean).flatMap((r) => (r.findings || []).map((f) => ({ ...f, layer: 'gap', cell: 'gap', alive: true, verdict: 'PLAUSIBLE', score: scoreOf({ ...f, verdict: 'PLAUSIBLE' }) })))
}

return { confirmed, plausible, gapFindings, refutedCount, gaps, coveredCells: CELLS.length }
```

- [ ] **Step 2: Sanity-check the script parses**

```bash
node --check "$SCRATCH/stability-audit.workflow.js"
```

Expected: no output (exit 0) — valid JS syntax. (Note: `node --check` only validates syntax; the Workflow globals `agent`/`pipeline`/`parallel`/`log`/`args` are injected by the Workflow runtime, not Node.)

---

### Task 4: Run the audit workflow and assemble the backlog

**Files:**
- Modify: `docs/audits/2026-07-08-stability-audit-findings.md` (fill with the ranked backlog)

**Interfaces:**
- Consumes: `%SCRATCH%/stability-audit.workflow.js` + the leads bundle.
- Produces: the finished backlog markdown.

- [ ] **Step 1: Run the workflow**

Invoke the Workflow tool with `scriptPath: "%SCRATCH%/stability-audit.workflow.js"` and `args: { leads: "<full contents of stability-seed-leads.md>" }`. It runs in the background; wait for the completion notification.

Expected: a returned object `{ confirmed, plausible, gapFindings, refutedCount, gaps, coveredCells }`.

- [ ] **Step 2: Inspect the run journal if the result looks thin**

If `confirmed.length + plausible.length` is surprisingly low, Read `<transcriptDir>/journal.jsonl` to confirm finders actually returned findings (per Workflow resume guidance) before trusting an empty result.

- [ ] **Step 3: Assemble the backlog markdown**

Write `docs/audits/2026-07-08-stability-audit-findings.md` with:
1. A header (date, scope, method one-liner, `coveredCells` count, `refutedCount`).
2. A **ranked summary table**: `| # | id | severity | confidence | blast | axis | layer | file:line | summary |`, sorted by `score` desc (confirmed above plausible at equal severity).
3. **Per-finding detail** sections (id, axis, layer, `file:line`, severity, confidence, blast, failure scenario, evidence, proposed-fix sketch).
4. A **gaps** section listing anything the completeness critic flagged (and whether the gap-fill round covered it).
5. A short **"suggested wave grouping"** (S0/S1 first, grouped by subsystem) — a *suggestion* for the user to accept/reorder, not a commitment.

- [ ] **Step 4: Verify the backlog is complete and non-placeholder**

```bash
grep -cE "^### " d:/GIT/bogdan/opex/docs/audits/2026-07-08-stability-audit-findings.md
grep -nE "TBD|TODO|FIXME|<placeholder>" d:/GIT/bogdan/opex/docs/audits/2026-07-08-stability-audit-findings.md || echo "no placeholders — good"
```

Expected: detail-section count equals `confirmed.length + plausible.length (+ gapFindings)`; zero placeholders.

---

### Task 5: Publish the backlog and hand off to prioritization

**Files:**
- Commit: `docs/audits/2026-07-08-stability-audit-findings.md`

- [ ] **Step 1: Commit the backlog locally (no push)**

```bash
cd d:/GIT/bogdan/opex && git add -f docs/audits/2026-07-08-stability-audit-findings.md docs/superpowers/plans/2026-07-08-stability-audit-phase0.md && git commit -q -m "docs(audit): Phase 0 stability findings backlog + plan"
```

Expected: one commit; nothing pushed (push requires explicit approval).

- [ ] **Step 2: Offer an Artifact rendering (optional)**

If the user wants a hosted, skimmable version for prioritization, load the `artifact-design` skill, render the backlog to an Artifact, and share the URL.

- [ ] **Step 3: Hand off**

Present the ranked backlog to the user. Ask them to mark which findings/subsystems become Wave 1 (and reorder as they see fit). Each approved wave then gets its own plan via `writing-plans` (spec §8). **Do not start fixing** until the user prioritizes.

---

## Self-Review

**Spec coverage:**
- §4 axes (1–8) → encoded in `AXES` + assigned across `CELLS`. ✓
- §5 perimeter map → 25 `CELLS` spanning rust-core, rust-sat, toolgate, channels, ui, infra. ✓
- §6 Workflow (Stage 0 seed → 1 finders → 2 verify → 3 dedup/rank → 4 critic) → Tasks 1–2 (seed), Task 3 script (stages 1–4). ✓
- §7 severity rubric + backlog format → `FINDING_SCHEMA`, `scoreOf`, Task 4 Step 3. ✓
- §7 optional Artifact → Task 5 Step 2. ✓
- §8 fix-wave process → Task 5 Step 3 handoff (waves are out of Phase 0 scope by design). ✓
- §9 constraints → Global Constraints block; server-authority for Rust signals in Task 2; no-push in Task 5. ✓
- §11 done-criteria → Task 4 (all findings verdicted + backlog written) + Task 3 Stage 4 critic. ✓

**Placeholder scan:** No TBD/TODO/"handle edge cases" in steps; the backlog placeholder file in Task 1 is explicitly filled in Task 4 and checked in Task 4 Step 4. ✓

**Type consistency:** `FINDING_SCHEMA` fields (axis/file/line/summary/failure_scenario/severity/blast_radius) are the same fields consumed by `scoreOf`/`keyOf`/`finderPrompt` and written in Task 4 Step 3. `verdict` values `CONFIRMED`/`PLAUSIBLE` are produced in Stage 2 and consumed in ranking + backlog. ✓
