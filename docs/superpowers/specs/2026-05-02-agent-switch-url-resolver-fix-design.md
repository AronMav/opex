# Agent Switch URL Resolver Bounce — Fix Design

**Date:** 2026-05-02
**Status:** Approved

## Problem

Switching agents in the chat UI bounces back to the previous agent on the first switch after page load.

**Root cause chain:**

1. Page loads with URL `?s=arty-session` (Arty was last active).
2. `currentAgent = "Alma"` from localStorage; URL has `?s=arty-session`.
3. User clicks Alma in the dropdown → `switchAgent("Alma")`.
4. Previous fix called `router.replace("/chat")` to clear `?s=`, but Next.js `useSearchParams` does NOT update synchronously from `router.replace`. The URL change is scheduled as an async navigation.
5. React renders with `currentAgent = "Alma"` AND `urlSessionId = "arty-session"` (stale).
6. `urlResolveFetched.current` is `null` (first switch since page load).
7. Cross-agent resolver fires: "arty-session" not in Alma's sessions → fetches `/api/sessions/arty-session` → gets `agent_id = "Arty"` → calls `setCurrentAgent("Arty")` → bounce.
8. After first bounce, `urlResolveFetched.current = "arty-session"` → resolver never fires again (explains "only on first switch").

## Solution: Override State

Introduce a `overrideUrlSession` React state that shadows `urlSessionId` synchronously. Setting it to `null` in `switchAgent` batches with `setCurrentAgent` in the same render, so the resolver sees `effectiveUrlSessionId = null` immediately — before any async URL update.

## Design

### 1. New State

```tsx
const [overrideUrlSession, setOverrideUrlSession] =
  useState<string | null | undefined>(undefined);

const effectiveUrlSessionId =
  overrideUrlSession !== undefined ? overrideUrlSession : urlSessionId;
```

| Value | Meaning |
|---|---|
| `undefined` | Use real `searchParams` (initial state; restored after Next.js navigation) |
| `null` | User switched agents — block the resolver |

**Reset effect** — restores `undefined` when Next.js actually navigates (deep-link, router.push, page transition), so URL-sharing deep-links continue to work after a switch:

```tsx
useEffect(() => {
  setOverrideUrlSession(undefined);
}, [searchParams]);
```

`window.history.replaceState` (used in `switchAgent` for reload safety) does NOT update `useSearchParams`, so this effect does not fire prematurely during a switch.

### 2. `switchAgent` Callback

```tsx
const switchAgent = useCallback((target: string) => {
  restoredAgents.current.delete(target);
  setOverrideUrlSession(null);                                     // syncs in same render
  window.history.replaceState(null, "", window.location.pathname); // clears URL for reload
  useChatStore.getState().setCurrentAgent(target);
}, []);
```

- Remove `router.replace` and `[router, searchParams]` deps from the previous fix.
- `setOverrideUrlSession(null)` + `setCurrentAgent(target)` batch into one render.
- In that render: `effectiveUrlSessionId = null` → resolver returns early.
- `window.replaceState` clears the physical URL so a hard reload has no `?s=`.

### 3. Cross-Agent Resolver

Replace `urlSessionId` with `effectiveUrlSessionId` everywhere in the effect body and deps:

```tsx
useEffect(() => {
  if (!effectiveUrlSessionId || !sessionsReady || !currentAgent) return;
  const agentState = useChatStore.getState().agents[currentAgent];
  if (agentState?.activeSessionId === effectiveUrlSessionId) return;
  if (sessions.some((s) => s.id === effectiveUrlSessionId)) return;
  if (urlResolveFetched.current === effectiveUrlSessionId) return;
  urlResolveFetched.current = effectiveUrlSessionId;
  fetch(`/api/sessions/${effectiveUrlSessionId}`, { ... })
    .then(...);
}, [effectiveUrlSessionId, sessionsReady, sessions, currentAgent, agents]);
```

### 4. Restore Effect

Two substitutions of `urlSessionId` → `effectiveUrlSessionId`:

```tsx
// Priority 1 — honour URL deep-link session
if (effectiveUrlSessionId && sessions.some((s) => s.id === effectiveUrlSessionId)) {
  ...selectSession(effectiveUrlSessionId, currentAgent)...
}

// Guard — don't fall to Priority 2 while resolver is still in flight
if (effectiveUrlSessionId && !sessions.some((s) => s.id === effectiveUrlSessionId)) return;
```

Deps array: replace `urlSessionId` with `effectiveUrlSessionId`.

### 5. URL-Sync Guard

The URL-sync effect reads `searchParams.get("s")` directly (not via `urlSessionId`). Make it override-aware:

```tsx
const currentUrlSession = overrideUrlSession !== undefined
  ? overrideUrlSession
  : searchParams.get("s");

if (currentUrlSession && sessions.length > 0 && !sessions.some((s) => s.id === currentUrlSession)) {
  return; // resolver in flight — don't overwrite
}
```

When `overrideUrlSession = null`: `currentUrlSession = null` → guard doesn't block → URL-sync updates to `?s=alma-session` correctly.

## Scenario Validation

| Scenario | Behaviour |
|---|---|
| First switch after page load (Arty→Alma) | `override = null` batches with agent switch → resolver sees `effectiveUrlSessionId = null` → returns early ✓ |
| Hard reload after switch | `window.replaceState` cleared URL → no `?s=` on reload → resolver doesn't fire ✓ |
| Subsequent switches (Alma→Hyde) | `override` already null; `replaceState` keeps URL clean ✓ |
| Deep-link on page load (`?s=foreign`) | `override = undefined` (fresh mount) → resolver uses real `urlSessionId` → works ✓ |
| Deep-link via Next.js navigation | `searchParams` updates → reset effect sets `override = undefined` → resolver works ✓ |
| Deep-link via hard reload | Fresh mount → `override = undefined` → resolver works ✓ |

## Files Changed

- `ui/src/app/(authenticated)/chat/page.tsx` — all changes in one file:
  1. Add `overrideUrlSession` state + `effectiveUrlSessionId` derived value
  2. Add reset `useEffect([searchParams])`
  3. Update `switchAgent` (remove `router`, add `setOverrideUrlSession`, `replaceState`)
  4. Update cross-agent resolver deps + body
  5. Update restore effect Priority 1 + guard
  6. Update URL-sync guard

No store changes, no new dependencies, no new files.

## Testing

Existing `agent-switching.test.ts` covers store-level behaviour. The `switchAgent` change (remove router) removes `[router, searchParams]` from deps — test coverage for the new pattern via static analysis tests in the same file, checking for `setOverrideUrlSession` and absence of `router.replace` in `switchAgent`.
