# Soul Audit Fix-Wave — Batch U (UI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the two confirmed Soul-tab UI defects from the 2026-07-16 audit: (1) toggling a parent section OFF no longer traps a now-disabled dependent switch in a checked+disabled state that can't be saved (emotion vs soul, drift.correct vs drift.enabled, auto-approve vs daily_plan) or send a stale dependent value that 400s; (2) entering `0` in `drift.threshold` / `emotion.intensity_importance_k` is no longer silently clobbered to the default.

**Architecture:** Two edits in `ui/src/app/(authenticated)/agents/`: parent-section `onToggle`/`onCheckedChange` handlers clear their dependent child field when turned off (`AgentEditDialog.tsx`); `formToPayload` uses a zero-preserving `numOr` for the two zero-valid float fields (`page.tsx`).

**Tech Stack:** Next.js 16 / React 19 / TypeScript, vitest.

## Global Constraints

- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push, do NOT deploy — controller runs vitest + `deploy-ui.sh` after review, on explicit user approval.
- **vitest runs ONLY from `ui/`** (`cd ui && npm test`). Do not run it from repo root. Also run `cd ui && npx tsc --noEmit` (type check) and `cd ui && npm run build` if changing types.
- **NO `Co-Authored-By` / Claude attribution trailer in the commit** — user forbids it. Subject line only.
- No design-value literals in raw form (project ESLint `no-raw-design-values`) — but these fixes touch logic/handlers, not styles.
- Source: soul audit UI findings Important #1 (disabled-trap / parent-off doesn't clear dependent) and Important #3 (zero-clobber).

## File Structure

- `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` — parent-off clears dependent child (soul→emotion, drift→correct, daily_plan→auto_approve).
- `ui/src/app/(authenticated)/agents/page.tsx` — `numOr` helper + zero-preserving `formToPayload` for `drift.threshold` and `emotion.intensity_importance_k`.
- `ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx` (or wherever the existing agent-form round-trip test lives — find it) — add zero-preservation + clearing assertions.

---

### Task 1: Parent-off clears dependent child + zero-preserving numeric parse

**Files:**
- Modify: `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` (soul SwitchSection `onToggle` ~line 665; drift SwitchSection `onToggle` ~line 685; daily_plan `Switch` `onCheckedChange` ~line 714)
- Modify: `ui/src/app/(authenticated)/agents/page.tsx` (`formToPayload` — `drift.threshold` line 338, `emotion.intensity_importance_k` line 354; add `numOr` helper near `formToPayload` ~line 228)
- Test: the existing agent-form vitest (find it: `grep -rl "formToPayload\|reflection_cooldown" ui/src/**/*.test.tsx`)

**Background:**
- `soulGating()` returns `emotionDisabled: !soulEnabled`, `driftCorrectDisabled: !driftEnabled`, `autoApproveDisabled: !daily_plan || budget<=0`. The dependent switches render `disabled={g.*Disabled}` but their `form.*` value is NOT cleared when the parent goes off. Because a disabled Radix `Switch` blocks BOTH directions, an emotion switch left `checked && disabled` cannot be unchecked from the UI, and `formToPayload` still serializes `emotion.enabled=true` alongside `soul.enabled=false` → server 400 (`emotion.enabled requires soul.enabled`) with no UI recovery path. `drift.correct` (rendered inside `{driftEnabled && ...}`) is hidden-but-still-true after toggling drift off → serializes `correct:true, enabled:false` → 400 about an invisible field. Root fix for all three: when the parent toggles OFF, clear the dependent child field in the same `upd(...)`.
- `formToPayload` uses `parseFloat(f.driftThreshold) || 0.15` and `parseFloat(f.emotionK) || 3`. Since `parseFloat("0") === 0` and `0 || d === d`, a user-entered `0` is replaced by the default. Both fields' valid ranges include 0 (`drift.threshold ∈ [0,2]`, `emotion.intensity_importance_k ∈ [0,5]`). This is the exact bug already fixed for `reflection_cooldown_minutes` (page.tsx ~329-331) — apply the same zero-preserving pattern.

- [ ] **Step 1: Write the failing tests**

Find the existing agent-form test (`grep -rl "formToPayload" ui/src`) and add:

```ts
  it("preserves an explicit 0 for drift.threshold and emotion.intensity_importance_k", () => {
    const f = { ...defaultForm, driftEnabled: true, driftThreshold: "0", emotionEnabled: true, soulEnabled: true, emotionK: "0" };
    const p = formToPayload(f);
    expect(p.drift.threshold).toBe(0);              // not 0.15
    expect(p.emotion.intensity_importance_k).toBe(0); // not 3
  });

  it("blank numeric field still falls back to the default", () => {
    const f = { ...defaultForm, driftEnabled: true, driftThreshold: "", emotionEnabled: true, soulEnabled: true, emotionK: "" };
    const p = formToPayload(f);
    expect(p.drift.threshold).toBe(0.15);
    expect(p.emotion.intensity_importance_k).toBe(3);
  });
```

(`defaultForm` = whatever the existing tests use as a baseline `FormState` — reuse it. If the test file constructs its form differently, match that; the assertions are the contract.)

- [ ] **Step 2: Run to verify failure**

Run: `cd ui && npm test -- agent-form` (or the test file name)
Expected: FAIL — `drift.threshold` is `0.15` and `intensity_importance_k` is `3` (the clobber).

- [ ] **Step 3: Add the `numOr` helper + use it**

In `page.tsx`, above `formToPayload` (~line 228):

```ts
// Parse a numeric form field, preserving a legitimate 0 (unlike `parseFloat(x) || d`,
// which clobbers 0 to the default). Falls back to `dflt` only for empty/NaN input.
function numOr(s: string, dflt: number): number {
  if (s.trim() === "") return dflt;
  const n = parseFloat(s);
  return Number.isFinite(n) ? n : dflt;
}
```

Change line 338: `threshold: parseFloat(f.driftThreshold) || 0.15,` → `threshold: numOr(f.driftThreshold, 0.15),`
Change line 354: `intensity_importance_k: parseFloat(f.emotionK) || 3,` → `intensity_importance_k: numOr(f.emotionK, 3),`

(Leave the other `parseFloat(...) || d` lines UNCHANGED — the audit confirmed only these two fields have a 0-valid range where the clobber matters; do not broaden scope.)

- [ ] **Step 4: Clear dependent child on parent-off (AgentEditDialog.tsx)**

Soul SwitchSection (~line 665) — clear `emotionEnabled` when soul turns off:

```tsx
                    <SwitchSection title={t("agents.section_soul")} enabled={form.soulEnabled} onToggle={(v) => upd(v ? { soulEnabled: v } : { soulEnabled: v, emotionEnabled: false })}>
```

Drift SwitchSection (~line 685) — clear `driftCorrect` when drift turns off:

```tsx
                    <SwitchSection title={t("agents.section_drift")} enabled={form.driftEnabled} onToggle={(v) => upd(v ? { driftEnabled: v } : { driftEnabled: v, driftCorrect: false })}>
```

Daily-plan `Switch` (~line 714) — clear `initiativeAutoApprove` when daily_plan turns off:

```tsx
                        <Switch checked={form.initiativeDailyPlan} disabled={g.dailyPlanDisabled} onCheckedChange={(v) => upd(v ? { initiativeDailyPlan: v } : { initiativeDailyPlan: v, initiativeAutoApprove: false })} className="data-[state=checked]:bg-primary" />
```

(These are the exact three dependent-field relationships from `soulGating`: `emotionDisabled← soul`, `driftCorrectDisabled← drift`, `autoApproveDisabled← daily_plan`. Do not change the `disabled=` props or any other handler.)

- [ ] **Step 5: (optional) component test for the clearing**

If the existing test harness renders `AgentEditDialog` (testing-library), add a test that toggling soul off sets `emotionEnabled` false in the resulting payload. If the harness only unit-tests `formToPayload` (no render), SKIP this — the clearing is in the handler and is covered by review + manual E2E; do NOT fabricate a render test if the file has no rendering setup. State which you did in the report.

- [ ] **Step 6: Run tests + type check + build**

Run: `cd ui && npm test -- agent-form` → PASS. Then `cd ui && npx tsc --noEmit` → clean. Then `cd ui && npm run build` → success.

- [ ] **Step 7: Commit** (NO trailer)

```bash
git add ui/src/app/\(authenticated\)/agents/AgentEditDialog.tsx ui/src/app/\(authenticated\)/agents/page.tsx ui/src/app/\(authenticated\)/agents/__tests__/agent-form.test.tsx
git commit -m "fix(agents-ui): clear dependent soul field on parent-off; preserve explicit 0 for drift.threshold/emotion.k"
```

(Adjust the test path to the real one you found.)

---

## Post-implementation (controller, after whole-branch review + user approval)

- vitest (`cd ui && npm test`), `tsc --noEmit`, `npm run build` all green.
- Deploy via `scripts/deploy-ui.sh` (local build + scp + atomic symlink flip — UI does NOT ship via server-deploy.sh).
- Post-deploy manual E2E: open an agent with emotion+soul on → toggle soul off → confirm the emotion switch is not stuck checked and the agent saves without a 400; enter `0` in drift threshold → save → reopen → confirm it persisted as `0` not `0.15`.
