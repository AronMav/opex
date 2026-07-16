# Persona-Drift v2 — Batch D-U (UI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two small Soul-tab drift fixes that pair with the v2 metric backend: (1) new agents created via the UI get the correct `baseline_turns` default (8, not the stale 3); (2) the now-inert `threshold` input is visibly disabled so an operator doesn't "raise the threshold" during the canary and see nothing happen.

**Architecture:** One literal bump in `page.tsx` (new-agent form default) + disabling/greying the `threshold` `Input` in `AgentEditDialog.tsx` with an explanatory tooltip. The full `z_fire`/`z_release` UI inputs are a SEPARATE later task — this batch only prevents the two concrete UX regressions the audit flagged.

**Tech Stack:** Next.js 16 / React 19 / TypeScript, vitest.

## Global Constraints

- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push, do NOT deploy — controller runs vitest + `deploy-ui.sh` after review, on explicit user approval.
- vitest runs ONLY from `ui/` (`cd ui && npm test`); also `cd ui && npx tsc --noEmit` + `cd ui && npm run build`.
- **NO `Co-Authored-By` / Claude attribution trailer in the commit** — user forbids it. Subject line only.
- Do NOT add the full `z_fire`/`z_release` inputs (deferred). Do NOT change `formToPayload`'s drift serialization shape (it still sends `threshold`/`baseline_turns` etc.; the backend defaults the z fields).
- Source spec: `docs/superpowers/specs/2026-07-17-persona-drift-v2-zscore-design.md` §7.

## File Structure

- `ui/src/app/(authenticated)/agents/page.tsx` — new-agent default literal.
- `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` — disable the inert `threshold` input.

---

### Task 1: Baseline-turns default bump + inert threshold input

**Files:**
- Modify: `ui/src/app/(authenticated)/agents/page.tsx` (new-agent form defaults ~line 119-123)
- Modify: `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` (drift-section `threshold` input; grep `driftThreshold`)
- Test: the existing agent-form test (`grep -rl "formToPayload" ui/src`)

**Background:** `formToPayload` always sends the full `drift: {...}` object, so the new-agent form's hardcoded `driftBaselineTurns: "3"` bakes the OLD default into any UI-created agent's TOML (never reaching the backend's new default of 8). And the `threshold` numeric input still renders live (step 0.01, min 0, max 2) though v2 ignores `threshold` entirely — an operator raising it to "fix" the always-fire sees the PUT succeed and nothing change. Existing agents are unaffected by the default bump (their GET round-trips the on-disk value).

- [ ] **Step 1: Write the failing test**

In the agent-form test file, add:

```ts
  it("new-agent form default for driftBaselineTurns is 8 (matches backend v2 default)", () => {
    // emptyForm / defaultForm is the new-agent baseline the form starts from.
    const p = formToPayload(emptyForm);
    expect(p.drift.baseline_turns).toBe(8);
  });
```

(Use the real baseline-form constant the existing tests use — `emptyForm` or similar; the assertion is the contract.)

- [ ] **Step 2: Run to verify failure**

Run: `cd ui && npm test -- agent-form`
Expected: FAIL — `baseline_turns` is 3.

- [ ] **Step 3: Bump the new-agent default literal**

In `page.tsx` (~119-123), change `driftBaselineTurns: "3"` → `driftBaselineTurns: "8"`. (Leave `driftThreshold: "0.15"` as-is — the field is inert but still round-trips harmlessly; do NOT remove it.)

- [ ] **Step 4: Run the test**

Run: `cd ui && npm test -- agent-form` → PASS.

- [ ] **Step 5: Disable the inert `threshold` input**

In `AgentEditDialog.tsx`, find the drift-section `threshold` `Input` (grep `driftThreshold` — it's a `<Input type="number" ... value={form.driftThreshold} .../>`). Add `disabled` and a muted-note/tooltip explaining it's superseded. Minimal change — add `disabled` to the Input and a small note beside it:

```tsx
                        <Input type="number" step={0.01} min={0} max={2} disabled className="bg-background border-border font-mono text-sm h-8 opacity-50" value={form.driftThreshold} onChange={(e) => upd({ driftThreshold: e.target.value })} />
                        <p className="text-xs text-muted-foreground">{t("agents.drift_threshold_deprecated")}</p>
```

Add the i18n key `agents.drift_threshold_deprecated` to BOTH `en.json` and `ru.json` (find the agents i18n block):
- en: `"drift_threshold_deprecated": "Superseded by self-calibrating z-score (config-only for now)."`
- ru: `"drift_threshold_deprecated": "Заменён само-калибрующимся z-score (пока только через конфиг)."`

(Match the exact JSX structure around the existing threshold input — keep its `value`/`onChange` so form state still round-trips; only add `disabled` + `opacity-50` + the note. If the design system has a dedicated disabled style, use it instead of `opacity-50` per the project's `no-raw-design-values` ESLint rule — check how other disabled inputs in this file are styled and match them.)

- [ ] **Step 6: Type check + build**

Run: `cd ui && npx tsc --noEmit` → clean. `cd ui && npm run build` → success. `cd ui && npm run lint` (if the project lints in CI) → clean (watch the `no-raw-design-values` rule on the opacity/style).

- [ ] **Step 7: Commit** (NO trailer)

```bash
git add ui/src/app/\(authenticated\)/agents/page.tsx ui/src/app/\(authenticated\)/agents/AgentEditDialog.tsx ui/src/i18n/locales/en.json ui/src/i18n/locales/ru.json ui/src/app/\(authenticated\)/agents/__tests__/agent-form.test.tsx
git commit -m "fix(agents-ui): drift baseline_turns default 8; grey the inert threshold input (v2)"
```

(Adjust the i18n + test paths to the real ones.)

---

## Post-implementation (controller, after review + user approval)

- vitest (`cd ui && npm test`) + `tsc --noEmit` + `npm run build` green.
- Deploy via `scripts/deploy-ui.sh` (local build + scp + atomic symlink flip — NOT server-deploy.sh).
- Manual check: create a new agent via the UI → its drift `baseline_turns` is 8; the `threshold` input is greyed with the deprecation note.

## Pairs with

- Rust batch (`2026-07-17-persona-drift-v2-batch-r-rust.md`) — the metric + config + DTO + wiring. Independent deploy; either order. The full `z_fire`/`z_release` UI inputs are a later follow-up, not in either batch.
