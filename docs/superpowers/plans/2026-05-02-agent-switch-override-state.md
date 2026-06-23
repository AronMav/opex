# Agent Switch URL Resolver Bounce — Override State Fix

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the "first switch after page load bounces back to previous agent" bug by introducing `overrideUrlSession` React state that blocks the cross-agent URL resolver synchronously during an agent switch.

**Architecture:** All changes in one file (`chat/page.tsx`). A new `overrideUrlSession` state (initially `undefined`, set to `null` in `switchAgent`) shadows `urlSessionId` via a derived `effectiveUrlSessionId`. All effects that drove the bounce use `effectiveUrlSessionId` instead of `urlSessionId`. A reset effect restores `undefined` whenever Next.js actually navigates (updating `searchParams`), preserving deep-link behaviour for real URL navigations.

**Tech Stack:** React 19, Next.js 16 App Router (`useSearchParams`, `useState`, `useEffect`, `useCallback`), Zustand (`useChatStore`).

---

## File Map

| File | Change |
|---|---|
| `ui/src/app/(authenticated)/chat/page.tsx` | Add state, update 4 effects, update switchAgent |
| `ui/src/stores/__tests__/agent-switching.test.ts` | Add 5 static-analysis contract tests |

---

### Task 1: Write Failing Static-Analysis Tests

**Files:**
- Modify: `ui/src/stores/__tests__/agent-switching.test.ts`

- [ ] **Step 1.1 — Add the failing test suite at the end of the file**

Open `ui/src/stores/__tests__/agent-switching.test.ts` and append the following block after the last `});`:

```ts
// ── 7. Override-state contract (static analysis of page.tsx) ─────────────────

describe("overrideUrlSession override-state contract", () => {
  async function getPageSrc() {
    const fs = await import("node:fs");
    const path = await import("node:path");
    return fs.readFileSync(
      path.resolve(__dirname, "../../app/(authenticated)/chat/page.tsx"),
      "utf8"
    );
  }

  it("declares overrideUrlSession state", async () => {
    const src = await getPageSrc();
    expect(src).toContain("overrideUrlSession");
    expect(src).toContain("setOverrideUrlSession");
  });

  it("declares effectiveUrlSessionId derived from override", async () => {
    const src = await getPageSrc();
    expect(src).toContain("effectiveUrlSessionId");
    expect(src).toContain(
      "overrideUrlSession !== undefined ? overrideUrlSession : urlSessionId"
    );
  });

  it("switchAgent sets override to null and does NOT call router.replace", async () => {
    const src = await getPageSrc();
    const switchBlock = src.slice(
      src.indexOf("const switchAgent = useCallback"),
      src.indexOf("}, []);", src.indexOf("const switchAgent = useCallback")) + "}, []);".length
    );
    expect(switchBlock).toContain("setOverrideUrlSession(null)");
    expect(switchBlock).not.toContain("router.replace");
  });

  it("cross-agent resolver uses effectiveUrlSessionId in body and deps", async () => {
    const src = await getPageSrc();
    const resolverBlock = src.slice(
      src.indexOf("urlResolveFetched = useRef"),
      src.indexOf("}, [effectiveUrlSessionId,") + "}, [effectiveUrlSessionId,".length
    );
    expect(resolverBlock).toContain("effectiveUrlSessionId");
    expect(resolverBlock).toContain("[effectiveUrlSessionId,");
  });

  it("URL-sync guard uses effectiveUrlSessionId", async () => {
    const src = await getPageSrc();
    // URL-sync effect is identified by the activeSessionId guard
    const syncBlock = src.slice(
      src.indexOf("// Sync activeSessionId → URL ?s= param"),
      src.indexOf("}, [activeSessionId, searchParams, sessions, effectiveUrlSessionId]);")
        + "}, [activeSessionId, searchParams, sessions, effectiveUrlSessionId]);".length
    );
    expect(syncBlock).toContain("effectiveUrlSessionId");
    expect(syncBlock).toContain("[activeSessionId, searchParams, sessions, effectiveUrlSessionId]");
  });
});
```

- [ ] **Step 1.2 — Run the new tests to confirm they all fail**

```bash
cd ui && npm test -- --run agent-switching 2>&1 | tail -20
```

Expected: `5 failed` in the new suite, all existing tests still pass.

- [ ] **Step 1.3 — Commit the failing tests**

```bash
cd ui && git add src/stores/__tests__/agent-switching.test.ts && git commit -m "test(ui): add failing contract tests for overrideUrlSession fix"
```

---

### Task 2: Add State, Derived Value and Reset Effect

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/page.tsx` (around line 78)

- [ ] **Step 2.1 — Add `overrideUrlSession` state and `effectiveUrlSessionId` after `urlSessionId`**

Find this line (≈ line 78):
```tsx
  const urlSessionId = searchParams.get("s");
```

Replace it with:
```tsx
  const urlSessionId = searchParams.get("s");
  // Override state: null = user switched agents (block resolver); undefined = use real searchParams.
  // Set synchronously in switchAgent so it batches with setCurrentAgent in the same render.
  const [overrideUrlSession, setOverrideUrlSession] = useState<string | null | undefined>(undefined);
  const effectiveUrlSessionId =
    overrideUrlSession !== undefined ? overrideUrlSession : urlSessionId;
```

- [ ] **Step 2.2 — Add the reset effect near the other session-restore effects**

Find the block that starts with (≈ line 133):
```tsx
  const sessionsReady = !sessionsLoading && sessionsData !== undefined;
```

Add the following effect immediately after that line:

```tsx
  // Reset override to undefined whenever Next.js router actually navigates
  // (e.g., user clicks a deep-link, router.push). window.history.replaceState
  // does NOT update useSearchParams, so this does not fire during a switch.
  useEffect(() => {
    setOverrideUrlSession(undefined);
  }, [searchParams]);
```

- [ ] **Step 2.3 — Verify TypeScript compiles**

```bash
cd ui && npx tsc --noEmit 2>&1
```

Expected: no output (clean).

- [ ] **Step 2.4 — Run tests (expect partial green)**

```bash
cd ui && npm test -- --run agent-switching 2>&1 | grep -E "passed|failed"
```

Expected: tests 1 and 2 of the new suite now pass (state and derived value declared); tests 3-5 still fail.

---

### Task 3: Update `switchAgent`

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/page.tsx` (≈ lines 411-424)

- [ ] **Step 3.1 — Replace the current `switchAgent` body**

Find the current `switchAgent` (look for `const switchAgent = useCallback`). Replace the entire callback including the comment block above it:

```tsx
  // Switch agent (including Group Chat virtual agent).
  // Override-state fix: set overrideUrlSession = null synchronously so
  // effectiveUrlSessionId is null in the same render as setCurrentAgent — the
  // cross-agent resolver sees !effectiveUrlSessionId and returns early before
  // useSearchParams can update from the now-stale ?s= param.
  // window.history.replaceState clears the physical URL so a hard reload won't
  // carry the previous agent's session ID into the resolver.
  const switchAgent = useCallback((target: string) => {
    restoredAgents.current.delete(target);
    setOverrideUrlSession(null);
    window.history.replaceState(null, "", window.location.pathname);
    useChatStore.getState().setCurrentAgent(target);
  }, []);
```

- [ ] **Step 3.2 — Remove unused `router` variable**

`router` was only used in the old `switchAgent`. After the replacement, search the file for any remaining `router.` usage:

```bash
grep -n "router\." ui/src/app/"(authenticated)"/chat/page.tsx
```

Expected: no output (no remaining usages). If no usages, delete the declaration near line 77:

```tsx
  const router = useRouter();   // ← delete this line
```

Also remove `useRouter` from the `next/navigation` import at the top of the file — change:

```tsx
import { useSearchParams, useRouter } from "next/navigation";
```

to:

```tsx
import { useSearchParams } from "next/navigation";
```

If `grep` shows other usages, leave both lines in place.

- [ ] **Step 3.3 — Verify TypeScript compiles**

```bash
cd ui && npx tsc --noEmit 2>&1
```

Expected: no output.

- [ ] **Step 3.4 — Run tests**

```bash
cd ui && npm test -- --run agent-switching 2>&1 | grep -E "passed|failed"
```

Expected: test 3 ("switchAgent sets override to null…") now passes; tests 4-5 still fail.

---

### Task 4: Update Cross-Agent Resolver

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/page.tsx` (≈ lines 151-176)

- [ ] **Step 4.1 — Replace the cross-agent resolver effect**

Find `const urlResolveFetched = useRef<string | null>(null);` and the `useEffect` immediately following it. Replace the entire effect (not the ref declaration) with:

```tsx
  useEffect(() => {
    if (!effectiveUrlSessionId || !sessionsReady || !currentAgent) return;
    const agentState = useChatStore.getState().agents[currentAgent];
    if (agentState?.activeSessionId === effectiveUrlSessionId) return;
    if (sessions.some((s) => s.id === effectiveUrlSessionId)) return; // restore effect handles this
    if (urlResolveFetched.current === effectiveUrlSessionId) return; // already tried
    urlResolveFetched.current = effectiveUrlSessionId;
    fetch(`/api/sessions/${effectiveUrlSessionId}`, {
      headers: { Authorization: `Bearer ${assertToken()}` },
    })
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { agent_id?: string } | null) => {
        if (!data?.agent_id) return;
        const targetAgent = data.agent_id;
        if (!agents.includes(targetAgent) || targetAgent === currentAgent) return;
        restoredAgents.current.add(targetAgent);
        useChatStore.getState().setCurrentAgent(targetAgent);
        useChatStore.getState().selectSession(effectiveUrlSessionId, targetAgent);
      })
      .catch(() => {});
  }, [effectiveUrlSessionId, sessionsReady, sessions, currentAgent, agents]);
```

- [ ] **Step 4.2 — Verify TypeScript compiles**

```bash
cd ui && npx tsc --noEmit 2>&1
```

Expected: no output.

- [ ] **Step 4.3 — Run tests**

```bash
cd ui && npm test -- --run agent-switching 2>&1 | grep -E "passed|failed"
```

Expected: test 4 now passes; test 5 still fails.

---

### Task 5: Update Restore Effect and URL-Sync Guard

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/page.tsx` (restore effect ≈ lines 178-247, URL-sync ≈ lines 255-271)

- [ ] **Step 5.1 — Update restore effect Priority 1 and its guard**

In the restore effect (starts `useEffect(() => { if (!currentAgent || !sessionsReady) return;`), find and replace the two occurrences of `urlSessionId` in Priority 1 and its guard:

Replace:
```tsx
    // Priority 1: URL ?s= param (deep link)
    if (urlSessionId && sessions.some((s) => s.id === urlSessionId)) {
      restoredAgents.current.add(currentAgent);
      const urlSession = sessions.find((s) => s.id === urlSessionId);
      useChatStore.getState().selectSession(urlSessionId, currentAgent);
      // If session is still running, mark it so ChatThread's auto-resume effect picks it up
      if (urlSession?.run_status === "running") {
        useChatStore.getState().markSessionActive(currentAgent, urlSessionId);
      }
      return;
    }

    // IMPORTANT: If urlSessionId exists but is NOT in current agent's sessions, it
    // likely belongs to a different agent. Do NOT fall through to Priority 2
    // (most-recent session) — selecting another session here triggers the URL-sync
    // effect to overwrite ?s= with the wrong session id, clobbering the deep link
    // before the cross-agent resolver effect has a chance to switch us to the
    // correct agent. Bail out and let the resolver handle it; deliberately do NOT
    // mark currentAgent as restored so a later visit (without deep link) still
    // restores normally.
    if (urlSessionId && !sessions.some((s) => s.id === urlSessionId)) {
      return;
    }
```

With:
```tsx
    // Priority 1: URL ?s= param (deep link)
    if (effectiveUrlSessionId && sessions.some((s) => s.id === effectiveUrlSessionId)) {
      restoredAgents.current.add(currentAgent);
      const urlSession = sessions.find((s) => s.id === effectiveUrlSessionId);
      useChatStore.getState().selectSession(effectiveUrlSessionId, currentAgent);
      // If session is still running, mark it so ChatThread's auto-resume effect picks it up
      if (urlSession?.run_status === "running") {
        useChatStore.getState().markSessionActive(currentAgent, effectiveUrlSessionId);
      }
      return;
    }

    // IMPORTANT: If effectiveUrlSessionId exists but is NOT in current agent's sessions,
    // it likely belongs to a different agent. Do NOT fall through to Priority 2
    // (most-recent session) — selecting another session here triggers the URL-sync
    // effect to overwrite ?s= with the wrong session id, clobbering the deep link
    // before the cross-agent resolver effect has a chance to switch us to the
    // correct agent. Bail out and let the resolver handle it; deliberately do NOT
    // mark currentAgent as restored so a later visit (without deep link) still
    // restores normally.
    if (effectiveUrlSessionId && !sessions.some((s) => s.id === effectiveUrlSessionId)) {
      return;
    }
```

Also update the restore effect deps array (last line of the effect):
```tsx
  }, [sessionsReady, sessions, currentAgent, effectiveUrlSessionId]);
```

- [ ] **Step 5.2 — Update the URL-sync guard to use `effectiveUrlSessionId`**

Find the URL-sync effect (starts with `// Sync activeSessionId → URL ?s= param`). Replace the guard block and update deps:

```tsx
  useEffect(() => {
    if (!activeSessionId) return;
    const currentUrlSession = searchParams.get("s");
    if (currentUrlSession === activeSessionId) return;

    if (
      effectiveUrlSessionId &&
      sessions.length > 0 &&
      !sessions.some((s) => s.id === effectiveUrlSessionId)
    ) {
      return; // resolver in flight — don't overwrite
    }

    const url = new URL(window.location.href);
    url.searchParams.set("s", activeSessionId);
    window.history.replaceState(null, "", url.pathname + url.search);
  }, [activeSessionId, searchParams, sessions, effectiveUrlSessionId]);
```

- [ ] **Step 5.3 — Verify TypeScript compiles**

```bash
cd ui && npx tsc --noEmit 2>&1
```

Expected: no output.

- [ ] **Step 5.4 — Run the full test suite**

```bash
cd ui && npm test -- --run 2>&1 | tail -8
```

Expected: all tests pass (`X passed`), including all 21 in `agent-switching.test.ts` (16 pre-existing + 5 added in Task 1).

- [ ] **Step 5.5 — Commit everything**

```bash
cd ui && git add src/app/"(authenticated)"/chat/page.tsx && git commit -m \
  "fix(ui): use overrideUrlSession to block cross-agent resolver during agent switch

Introduces overrideUrlSession React state (null when user switches agents)
that shadows urlSessionId via effectiveUrlSessionId. Because React batches
setOverrideUrlSession(null) + setCurrentAgent() into one render, the resolver
sees effectiveUrlSessionId = null immediately — before useSearchParams can
deliver the stale previous-agent session ID. Eliminates first-switch bounce."
```

---

### Task 6: Deploy to Pi

**Files:** none (deploy only)

- [ ] **Step 6.1 — Build UI**

```bash
cd ui && npm run build 2>&1 | tail -10
```

Expected: build succeeds, all 30 static pages generated.

- [ ] **Step 6.2 — Deploy to Pi**

```bash
ssh aronmav@192.168.1.85 "rm -rf ~/opex/ui/out"
cd ui && tar cf - out | ssh aronmav@192.168.1.85 "mkdir -p ~/opex/ui && cd ~/opex/ui && tar xf -"
```

Expected: `out/` transferred without errors.

- [ ] **Step 6.3 — Health check**

```bash
AUTH=$(cat .auth-token) && ssh aronmav@192.168.1.85 \
  "curl -sf -H 'Authorization: Bearer $AUTH' http://localhost:18789/api/doctor \
  | python3 -c 'import sys,json; d=json.load(sys.stdin); print(\"ok:\", d[\"ok\"])'"
```

Expected: `ok: True`.

- [ ] **Step 6.4 — Manual smoke test**

1. Open browser on Pi UI (`http://192.168.1.85` or local URL).
2. Note current agent (e.g., Arty).
3. Switch to Alma — confirm it stays on Alma (no bounce).
4. Hard reload (`Ctrl+Shift+R`) — confirm Alma is still selected.
5. Switch back to Arty — confirm no bounce.
