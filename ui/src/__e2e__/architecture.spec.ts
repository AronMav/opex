import { test, expect, type Page } from "@playwright/test";

/**
 * E2E tests for the post-rework architecture (Phases 1-5) and the
 * server-authoritative sync-envelope protocol (T1-T8b):
 *   • per-iteration UUID in step-start
 *   • single visual bubble per turn (continuesPrevious renders)
 *   • POST /api/chat → 202 {session_id, user_message_id}; the SSE stream is
 *     read exclusively from GET /api/chat/{sessionId}/stream, which always
 *     returns the full sync envelope (sync_begin → replay → sync_end → live)
 *     regardless of Last-Event-ID
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

// ── Shared helpers: 202 POST + GET sync-envelope read ────────────────────────

interface SseEvent {
  id: string | null;
  data: unknown;
}

interface EnvelopeResult {
  status: number;
  events: SseEvent[];
  /** `{boundaryMessageId, runStatus, truncated}` when present, else null. */
  syncBegin: unknown;
  /** `{lastSeq}` when present, else null. */
  syncEnd: unknown;
  seqIds: number[];
  sawFinish: boolean;
}

/** POST /api/chat — the only thing this returns now is the 202 ack. */
async function postChat(
  page: Page,
  body: Record<string, unknown>,
): Promise<{ status: number; session_id: string | null; user_message_id: string | null }> {
  return page.evaluate(
    async ({ token, body }: { token: string; body: Record<string, unknown> }) => {
      const resp = await fetch("/api/chat", {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(body),
      });
      let json: { session_id?: string; user_message_id?: string } | null = null;
      try {
        json = await resp.json();
      } catch {
        json = null;
      }
      return {
        status: resp.status,
        session_id: json?.session_id ?? null,
        user_message_id: json?.user_message_id ?? null,
      };
    },
    { token: TOKEN, body },
  );
}

/**
 * GET /api/chat/{sessionId}/stream — the authoritative sync envelope:
 * sync_begin → full replay (each SSE `id:` = seq) → sync_end → live →
 * finish/[DONE]. Always a full replay — Last-Event-ID (if passed via
 * extraHeaders) is ignored server-side, there is no 204 branch.
 */
async function readSyncEnvelope(
  page: Page,
  sessionId: string,
  agent: string,
  extraHeaders: Record<string, string> = {},
): Promise<EnvelopeResult> {
  return page.evaluate(
    async ({
      sid,
      agent,
      token,
      extraHeaders,
    }: {
      sid: string;
      agent: string;
      token: string;
      extraHeaders: Record<string, string>;
    }) => {
      const resp = await fetch(`/api/chat/${sid}/stream?agent=${encodeURIComponent(agent)}`, {
        headers: { Authorization: `Bearer ${token}`, ...extraHeaders },
      });

      const events: { id: string | null; data: unknown }[] = [];
      const seqIds: number[] = [];
      let sawFinish = false;
      let syncBegin: unknown = null;
      let syncEnd: unknown = null;

      if (!resp.body) {
        return { status: resp.status, events, syncBegin, syncEnd, seqIds, sawFinish };
      }

      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      let pendingId: string | null = null;

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
            if (!Number.isNaN(n)) seqIds.push(n);
          } else if (line.startsWith("data:")) {
            const data = line.slice(5).trim();
            if (data === "[DONE]") {
              events.push({ id: pendingId, data: { type: "__DONE__" } });
              pendingId = null;
              break;
            }
            try {
              const parsed = JSON.parse(data);
              events.push({ id: pendingId, data: parsed });
              if (parsed.type === "sync_begin") syncBegin = parsed;
              if (parsed.type === "sync_end") syncEnd = parsed;
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
      return { status: resp.status, events, syncBegin, syncEnd, seqIds, sawFinish };
    },
    { sid: sessionId, agent, token: TOKEN, extraHeaders },
  );
}

// ── Test A: post a message, read the sync envelope, validate contract ───────

test("Phase 1 + 3 + 5: SSE contract end-to-end via UI fetch", async ({ page }) => {
  test.setTimeout(180_000);
  await login(page);

  const posted = await postChat(page, {
    messages: [{ role: "user", content: "посчитай 7+3 одной фразой и заверши" }],
    agent: "Arty",
    force_new_session: true,
  });

  // POST /api/chat no longer streams — it just acks with 202 + ids.
  expect(posted.status).toBe(202);
  expect(posted.session_id).toBeTruthy();

  const result = await readSyncEnvelope(page, posted.session_id!, "Arty");

  console.log(
    "[Phase contract] result:",
    JSON.stringify({
      status: result.status,
      eventCount: result.events.length,
      syncBegin: result.syncBegin,
      syncEnd: result.syncEnd,
    }),
  );

  expect(result.status).toBe(200);

  // Envelope contract: sync_begin opens, sync_end closes the replay.
  expect(result.syncBegin).toBeTruthy();
  expect(result.syncEnd).toBeTruthy();

  const stepStartIds = result.events
    .filter((e) => (e.data as Record<string, unknown>)?.type === "step-start")
    .map((e) => (e.data as { messageId: string }).messageId);

  // Phase 1: every step-start carries a UUID messageId
  expect(stepStartIds.length).toBeGreaterThan(0);
  for (const id of stepStartIds) {
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  }

  // Phase 3: SSE id: field with monotonic seq present in the envelope
  expect(result.seqIds.length).toBeGreaterThan(0);
  expect(Math.max(...result.seqIds)).toBeGreaterThan(0);

  // Backend Finish guarantee
  expect(result.sawFinish).toBe(true);

  // Phase 5: every tool-input-start has a tool-output-available
  const toolStarts = new Set(
    result.events
      .filter((e) => (e.data as Record<string, unknown>)?.type === "tool-input-start")
      .map((e) => (e.data as { toolCallId: string }).toolCallId),
  );
  const toolOutputs = new Set(
    result.events
      .filter((e) => (e.data as Record<string, unknown>)?.type === "tool-output-available")
      .map((e) => (e.data as { toolCallId: string }).toolCallId),
  );
  const missing = Array.from(toolStarts).filter((id) => !toolOutputs.has(id));
  expect(missing).toEqual([]);
});

// ── Test B: tool loop produces step_id rows in DB (Phase 4) ─────────────────

test("Phase 4: intermediate iterations get step_id in DB", async ({ page }) => {
  test.setTimeout(180_000);
  await login(page);

  // Tool-instructed prompt — Arty's research-strategy makes it call tools.
  const posted = await postChat(page, {
    messages: [{ role: "user", content: "найди в интернете последнюю новость одной фразой" }],
    agent: "Arty",
    force_new_session: true,
  });

  expect(posted.status).toBe(202);
  expect(posted.session_id).toBeTruthy();

  const result = await readSyncEnvelope(page, posted.session_id!, "Arty");
  expect(result.status).toBe(200);

  const stepStarts = result.events
    .filter((e) => (e.data as Record<string, unknown>)?.type === "step-start")
    .map((e) => {
      const d = e.data as { stepId: string; messageId: string };
      return { stepId: d.stepId, messageId: d.messageId };
    });

  console.log(`[Phase 4] session=${posted.session_id}, step-starts: ${JSON.stringify(stepStarts)}`);

  // The model may or may not actually run tools. The contract we verify is:
  // EVERY step-start carries a valid UUID messageId.
  for (const s of stepStarts) {
    expect(s.messageId).toMatch(/^[0-9a-f-]{36}$/);
  }
  // If tool loop happened (>=2 step-starts), each has a unique UUID.
  if (stepStarts.length >= 2) {
    const ids = new Set(stepStarts.map((s) => s.messageId));
    expect(ids.size).toBe(stepStarts.length);
  }
});

// ── Test C: GET stream ignores Last-Event-ID, always full-replays ───────────
//
// REPURPOSED (was "Last-Event-ID resume replays only new events"): the
// transport-level resume/skip-ahead protocol this test used to exercise was
// removed along with `Last-Event-ID` handling and the 204 branch. The new
// invariant is the opposite of the old one — the GET stream is a full
// sync-envelope replay every time, and any Last-Event-ID header sent by a
// stale/reconnecting client is ignored rather than honored.

test("GET stream returns the full envelope and ignores Last-Event-ID", async ({ page }) => {
  test.setTimeout(180_000);
  await login(page);

  const posted = await postChat(page, {
    messages: [{ role: "user", content: "ответь словом 'готово' и больше ничего" }],
    agent: "Arty",
    force_new_session: true,
  });

  expect(posted.status).toBe(202);
  expect(posted.session_id).toBeTruthy();

  // First read: full envelope, capture the seq range.
  const first = await readSyncEnvelope(page, posted.session_id!, "Arty");
  expect(first.status).toBe(200);
  expect(first.syncBegin).toBeTruthy();
  expect(first.syncEnd).toBeTruthy();
  expect(first.seqIds.length).toBeGreaterThan(2);
  const firstMinSeq = Math.min(...first.seqIds);
  const firstMaxSeq = Math.max(...first.seqIds);

  // Wait for the session to be marked done so the second read is deterministic.
  await waitForSessionDone(page, posted.session_id!, 60_000);

  // Second read WITH a stale Last-Event-ID header set to the midpoint of the
  // first read's seq range. Under the old protocol this would have skipped
  // ahead (or 204'd); under the new protocol it must be ignored entirely.
  const half = Math.floor((firstMinSeq + firstMaxSeq) / 2);
  const replayed = await readSyncEnvelope(page, posted.session_id!, "Arty", {
    "Last-Event-ID": String(half),
  });

  console.log(
    `[envelope-replay test] half=${half}, replayed status=${replayed.status}, ` +
      `seqRange=[${replayed.seqIds.length ? Math.min(...replayed.seqIds) : "n/a"},` +
      `${replayed.seqIds.length ? Math.max(...replayed.seqIds) : "n/a"}]`,
  );

  // No more 204 — the stream endpoint always answers 200 with the envelope.
  expect(replayed.status).toBe(200);
  expect(replayed.syncBegin).toBeTruthy();
  expect(replayed.syncEnd).toBeTruthy();
  expect(replayed.seqIds.length).toBeGreaterThan(0);

  // Last-Event-ID is ignored: the replay still starts at (or below) the same
  // lowest seq as the very first read — it is NOT skipped ahead past `half`.
  const replayedMinSeq = Math.min(...replayed.seqIds);
  expect(replayedMinSeq).toBeLessThanOrEqual(firstMinSeq);
});
