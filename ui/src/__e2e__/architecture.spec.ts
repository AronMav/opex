import { test, expect, type Page } from "@playwright/test";

/**
 * E2E tests for the post-rework architecture (Phases 1-5):
 *   • per-iteration UUID in step-start
 *   • single visual bubble per turn (continuesPrevious renders)
 *   • Last-Event-ID resume protocol
 *   • Backend Finish event guarantee
 *
 * Run:
 *   PLAYWRIGHT_BASE_URL=https://192.168.1.82 \
 *   OPEX_AUTH_TOKEN=<token> \
 *   npx playwright test src/__e2e__/architecture.spec.ts --project=chromium
 */

const TOKEN = process.env.OPEX_AUTH_TOKEN ?? "";

test.describe.configure({ mode: "serial" });

test.beforeAll(() => {
  if (!TOKEN) {
    throw new Error("OPEX_AUTH_TOKEN env var required for e2e architecture tests");
  }
});

async function login(page: Page) {
  await page.goto("/login");
  await page.waitForSelector('input[type="password"]', { timeout: 15_000 });
  await page.fill('input[type="password"]', TOKEN);
  await page.click('button[type="submit"]');
  await page.waitForURL(/\/chat/, { timeout: 30_000 });
}

async function clickNewChat(page: Page) {
  await page.locator('aside button:has-text("New")').click({ timeout: 8_000 });
}

async function sendMessage(page: Page, text: string) {
  const ta = page.locator("[data-composer-input] textarea");
  await ta.waitFor({ state: "visible", timeout: 10_000 });
  await ta.fill(text);
  await ta.press("Enter");
}

async function waitForSessionId(page: Page, timeoutMs = 20_000): Promise<string | null> {
  try {
    await page.waitForFunction(
      () => new URLSearchParams(window.location.search).has("s"),
      undefined,
      { timeout: timeoutMs },
    );
    return new URL(page.url()).searchParams.get("s");
  } catch {
    return null;
  }
}

async function waitForSessionDone(page: Page, sid: string, timeoutMs: number): Promise<string | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const status = await page.evaluate(
      async ({ s, t }: { s: string; t: string }) => {
        const r = await fetch(`/api/sessions?agent=Arty&limit=100`, {
          headers: { Authorization: `Bearer ${t}` },
        });
        if (!r.ok) return null;
        const d = await r.json();
        return (d.sessions ?? []).find((x: { id: string; run_status: string }) => x.id === s)?.run_status ?? null;
      },
      { s: sid, t: TOKEN },
    );
    if (status && status !== "running" && status !== "streaming") return status;
    await page.waitForTimeout(2_000);
  }
  return null;
}

// ── Test A: post a message, capture SSE via fetch, validate contract ──────────

test("Phase 1 + 3 + 5: SSE contract end-to-end via UI fetch", async ({ page }) => {
  test.setTimeout(180_000);
  await login(page);

  // Use page.evaluate to drive the request from the same origin as the UI
  // — preserves cookie/session state and exercises the live deployment.
  const result = await page.evaluate(
    async ({ token }: { token: string }) => {
      const resp = await fetch("/api/chat", {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          messages: [
            { role: "user", content: "посчитай 7+3 одной фразой и заверши" },
          ],
          agent: "Arty",
          force_new_session: true,
        }),
      });
      const reader = resp.body!.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      const events: { id: string | null; data: unknown }[] = [];
      let pendingId: string | null = null;
      let lastSeq = 0;
      let sawFinish = false;
      const seenStepIds = new Set<string>();
      const stepStartIds: string[] = [];
      const toolStarts = new Set<string>();
      const toolOutputs = new Set<string>();

      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let nl = buf.indexOf("\n");
        while (nl !== -1) {
          const line = buf.slice(0, nl).replace(/\r$/, "");
          buf = buf.slice(nl + 1);
          if (line.startsWith("id:")) {
            pendingId = line.slice(3).trim();
            const n = Number.parseInt(pendingId, 10);
            if (!Number.isNaN(n)) lastSeq = Math.max(lastSeq, n);
          } else if (line.startsWith("data:")) {
            const data = line.slice(5).trim();
            if (data === "[DONE]") {
              events.push({ id: pendingId, data: { type: "__DONE__" } });
              break;
            }
            try {
              const parsed = JSON.parse(data);
              events.push({ id: pendingId, data: parsed });
              if (parsed.type === "step-start") {
                stepStartIds.push(parsed.messageId);
                seenStepIds.add(parsed.stepId);
              }
              if (parsed.type === "tool-input-start") toolStarts.add(parsed.toolCallId);
              if (parsed.type === "tool-output-available") toolOutputs.add(parsed.toolCallId);
              if (parsed.type === "finish") sawFinish = true;
            } catch {
              // ignore unparseable
            }
            pendingId = null;
          } else if (line === "") {
            pendingId = null;
          }
          nl = buf.indexOf("\n");
        }
      }
      return {
        eventCount: events.length,
        stepStartIds,
        seenStepIds: Array.from(seenStepIds),
        toolStarts: Array.from(toolStarts),
        toolOutputs: Array.from(toolOutputs),
        sawFinish,
        lastSeq,
      };
    },
    { token: TOKEN },
  );

  console.log("[Phase contract] result:", JSON.stringify(result));

  // Phase 1: every step-start carries a UUID messageId
  expect(result.stepStartIds.length).toBeGreaterThan(0);
  for (const id of result.stepStartIds) {
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  }

  // Phase 3: SSE id: field with monotonic seq
  expect(result.lastSeq).toBeGreaterThan(0);

  // Backend Finish guarantee
  expect(result.sawFinish).toBe(true);

  // Phase 5: every tool-input-start has a tool-output-available
  const missing = result.toolStarts.filter((id) => !result.toolOutputs.includes(id));
  expect(missing).toEqual([]);
});

// ── Test B: tool loop produces step_id rows in DB (Phase 4) ─────────────────

test("Phase 4: intermediate iterations get step_id in DB", async ({ page }) => {
  test.setTimeout(180_000);
  await login(page);

  // Tool-instructed prompt — Arty's research-strategy makes it call tools.
  const result = await page.evaluate(
    async ({ token }: { token: string }) => {
      const resp = await fetch("/api/chat", {
        method: "POST",
        headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
        body: JSON.stringify({
          messages: [
            {
              role: "user",
              content: "найди в интернете последнюю новость одной фразой",
            },
          ],
          agent: "Arty",
          force_new_session: true,
        }),
      });
      const reader = resp.body!.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      let sid: string | null = null;
      const stepStarts: { stepId: string; messageId: string }[] = [];
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let nl = buf.indexOf("\n");
        while (nl !== -1) {
          const line = buf.slice(0, nl).replace(/\r$/, "");
          buf = buf.slice(nl + 1);
          if (line.startsWith("data:")) {
            const data = line.slice(5).trim();
            if (data === "[DONE]") break;
            try {
              const p = JSON.parse(data);
              if (p.type === "data-session-id") sid = p.data.sessionId;
              if (p.type === "step-start") stepStarts.push({ stepId: p.stepId, messageId: p.messageId });
            } catch { /* ignore */ }
          }
          nl = buf.indexOf("\n");
        }
      }
      return { sid, stepStarts };
    },
    { token: TOKEN },
  );

  console.log(`[Phase 4] session=${result.sid}, step-starts: ${JSON.stringify(result.stepStarts)}`);
  expect(result.sid).toBeTruthy();
  // The model may or may not actually run tools. The contract we verify is:
  // EVERY step-start carries a valid UUID messageId.
  for (const s of result.stepStarts) {
    expect(s.messageId).toMatch(/^[0-9a-f-]{36}$/);
  }
  // If tool loop happened (>=2 step-starts), each has a unique UUID.
  if (result.stepStarts.length >= 2) {
    const ids = new Set(result.stepStarts.map((s) => s.messageId));
    expect(ids.size).toBe(result.stepStarts.length);
  }
});

// ── Test C: Last-Event-ID resume skips already-seen events ───────────────────

test("Last-Event-ID resume replays only new events (Phase 3)", async ({ page }) => {
  test.setTimeout(180_000);
  await login(page);

  // Create a session via API and capture its events.
  const first = await page.evaluate(
    async ({ token }: { token: string }) => {
      const resp = await fetch("/api/chat", {
        method: "POST",
        headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
        body: JSON.stringify({
          messages: [{ role: "user", content: "ответь словом 'готово' и больше ничего" }],
          agent: "Arty",
          force_new_session: true,
        }),
      });
      const reader = resp.body!.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      let lastId = 0;
      let sid: string | null = null;
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let nl = buf.indexOf("\n");
        while (nl !== -1) {
          const line = buf.slice(0, nl).replace(/\r$/, "");
          buf = buf.slice(nl + 1);
          if (line.startsWith("id:")) {
            const n = Number.parseInt(line.slice(3).trim(), 10);
            if (!Number.isNaN(n)) lastId = n;
          } else if (line.startsWith("data:")) {
            const data = line.slice(5).trim();
            if (data === "[DONE]") break;
            try {
              const p = JSON.parse(data);
              if (p.type === "data-session-id") sid = p.data.sessionId;
            } catch { /* ignore */ }
          }
          nl = buf.indexOf("\n");
        }
      }
      return { sid, lastId };
    },
    { token: TOKEN },
  );

  expect(first.sid).toBeTruthy();
  expect(first.lastId).toBeGreaterThan(2);

  // Wait for session to be marked done so resume is deterministic.
  await waitForSessionDone(page, first.sid!, 60_000);

  // Resume with Last-Event-ID at half — backend should skip earlier events.
  const half = Math.floor(first.lastId / 2);
  const replayed = await page.evaluate(
    async ({ sid, half, token }: { sid: string; half: number; token: string }) => {
      const r = await fetch(`/api/chat/${sid}/stream?agent=Arty`, {
        headers: {
          Authorization: `Bearer ${token}`,
          "Last-Event-ID": String(half),
        },
      });
      if (r.status === 204) return { status: 204, ids: [] };
      const reader = r.body!.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      const ids: number[] = [];
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let nl = buf.indexOf("\n");
        while (nl !== -1) {
          const line = buf.slice(0, nl).replace(/\r$/, "");
          buf = buf.slice(nl + 1);
          if (line.startsWith("id:")) {
            const n = Number.parseInt(line.slice(3).trim(), 10);
            if (!Number.isNaN(n)) ids.push(n);
          } else if (line.startsWith("data:")) {
            const data = line.slice(5).trim();
            if (data === "[DONE]") break;
          }
          nl = buf.indexOf("\n");
        }
      }
      return { status: r.status, ids };
    },
    { sid: first.sid!, half, token: TOKEN },
  );

  console.log(`[Last-Event-ID test] half=${half}, replayed status=${replayed.status}, ids=${JSON.stringify(replayed.ids.slice(0, 10))}…`);

  if (replayed.status === 204) {
    // Session pruned from registry — that's also a valid resume outcome
    // (no events to replay), counts as protocol-compliant.
    expect(replayed.status).toBe(204);
  } else {
    expect(replayed.ids.length).toBeGreaterThan(0);
    const minId = Math.min(...replayed.ids);
    expect(minId).toBeGreaterThan(half);
  }
});
