import { test, expect, vi, beforeEach, afterEach } from "vitest";
import { createStreamingRenderer } from "../streaming-renderer";
import type { StoreAccess } from "../streaming-renderer";

// Minimal StoreAccess — construction only reads .agents (none) and wires the
// visibilitychange listener; no network calls happen at build time.
const store = {
  get: () => ({ agents: {} }),
  set: () => {},
} as unknown as StoreAccess;

let addSpy: ReturnType<typeof vi.spyOn>;
let removeSpy: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
  addSpy = vi.spyOn(document, "addEventListener");
  removeSpy = vi.spyOn(document, "removeEventListener");
});
afterEach(() => {
  addSpy.mockRestore();
  removeSpy.mockRestore();
});

// Regression: the renderer registered a document visibilitychange listener at
// construction with no way to remove it. dispose() must detach the exact same
// handler so a re-instantiated renderer (HMR / re-init) can't leak a stale
// listener that closes over a dead store.
test("dispose() removes the exact visibilitychange handler it registered", () => {
  const r = createStreamingRenderer(store);

  const addCall = addSpy.mock.calls.find((c: unknown[]) => c[0] === "visibilitychange");
  expect(addCall).toBeTruthy();
  const handler = addCall![1];

  r.dispose();

  const removeCall = removeSpy.mock.calls.find((c: unknown[]) => c[0] === "visibilitychange");
  expect(removeCall).toBeTruthy();
  expect(removeCall![1]).toBe(handler); // same reference — actually detaches
});

test("dispose() is idempotent", () => {
  const r = createStreamingRenderer(store);
  r.dispose();
  removeSpy.mockClear();
  r.dispose(); // second call must be a no-op
  expect(removeSpy).not.toHaveBeenCalled();
});
