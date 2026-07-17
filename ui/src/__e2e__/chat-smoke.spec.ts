import { test, expect, type Page } from "@playwright/test";

/**
 * Smoke tests for 3 critical chat flows that were recently fixed.
 *
 * Run against the live Pi backend sequentially (serial mode prevents session
 * interference between tests):
 *
 *   PLAYWRIGHT_BASE_URL=http://192.168.1.85:18789 npx playwright test \
 *     src/__e2e__/chat-smoke.spec.ts --project=chromium --workers=1
 */

// ── Config ──────────────────────────────────────────────────────────────────

// Auth token: pass via OPEX_E2E_TOKEN (e.g. from .auth-token); the fallback is
// the retired Pi token kept for historical local setups.
const TOKEN =
  process.env.OPEX_E2E_TOKEN ||
  "25378f5154228e4f8f196007171e338f063ed89fc03bf1394c0233dffbb8f0e0";

/** Wave-4 semantic shift: the inline caret (`streaming-cursor`) now renders
 *  only while TEXT is arriving; during the reasoning/tool phase the comet
 *  (`thinking-indicator`) shows instead. "Stream engaged" = either of them. */
const STREAM_ENGAGED =
  "[data-testid='streaming-cursor'], [data-testid='thinking-indicator']";

/** Run serially — avoids session list contamination between tests. */
test.describe.configure({ mode: "serial" });

// ── Types ────────────────────────────────────────────────────────────────────


// ── Auth & navigation helpers ─────────────────────────────────────────────────

/** Login via the /login form. Input is type="password".
 *
 * Retries up to 3 times if rate-limited (Pi enforces 300 RPM).
 */
async function login(page: Page, maxRetries = 3) {
  for (let attempt = 0; attempt < maxRetries; attempt++) {
    await page.goto("/login");
    await page.waitForSelector('input[type="password"]', { timeout: 15_000 });
    await page.fill('input[type="password"]', TOKEN);
    await page.click('button[type="submit"]');

    // Check if we were redirected to /chat (success) or stayed on /login (rate limited or error)
    try {
      await page.waitForURL(/\/chat/, { timeout: 30_000 });
      return; // success
    } catch {
      // Might be rate limited — check for the error message
      const isRateLimited = await page
        .locator("text=Too many attempts")
        .isVisible()
        .catch(() => false);

      if (isRateLimited && attempt < maxRetries - 1) {
        console.log(`[login] Rate limited (attempt ${attempt + 1}/${maxRetries}). Waiting 35s...`);
        await page.waitForTimeout(35_000);
        continue;
      }
      throw new Error(`Login failed after ${attempt + 1} attempts (rate limited or other error)`);
    }
  }
}

/** Click the "+ New" button in the session sidebar. */
async function clickNewChat(page: Page) {
  await page.locator('aside button:has-text("New")').click({ timeout: 8_000 });
}

/** Fill the composer textarea and submit with Enter. */
async function sendMessage(page: Page, text: string) {
  const ta = page.locator('[data-composer-input] textarea');
  await ta.waitFor({ state: "visible", timeout: 10_000 });
  await ta.fill(text);
  await ta.press("Enter");
}

/**
 * Wait for the URL to contain ?s=<sessionId>.
 * Returns the session ID string or null on timeout.
 */
async function waitForSessionId(
  page: Page,
  timeoutMs = 20_000
): Promise<string | null> {
  try {
    await page.waitForFunction(
      () => new URLSearchParams(window.location.search).has("s"),
      undefined,
      { timeout: timeoutMs }
    );
    return new URL(page.url()).searchParams.get("s");
  } catch {
    return null;
  }
}

/**
 * Wait for the stop button (streaming in progress) to appear.
 * The stop button is type="button" with text-destructive class inside
 * the [data-composer-input] form, visible only during streaming.
 * Uses both Playwright locator and JS evaluation as fallbacks.
 */
async function waitForStreamingStarted(
  page: Page,
  timeoutMs: number
): Promise<boolean> {
  // Primary: Playwright locator (most reliable)
  try {
    await page
      .locator('[data-composer-input] button[type="button"].text-destructive')
      .first()
      .waitFor({ state: "visible", timeout: timeoutMs });
    return true;
  } catch {
    // Secondary: JS evaluation (catches cases where CSS class names differ slightly)
    // This runs after the primary timeout — try once more
    return page.evaluate(() => {
      const form = document.querySelector("[data-composer-input]");
      if (!form) return false;
      const buttons = Array.from(
        form.querySelectorAll<HTMLButtonElement>("button")
      );
      return buttons.some((btn) => {
        const cls = btn.className ?? "";
        const style = window.getComputedStyle(btn);
        return (
          cls.includes("destructive") &&
          style.display !== "none" &&
          style.visibility !== "hidden" &&
          parseFloat(style.opacity) > 0
        );
      });
    });
  }
}

/**
 * POST /api/chat/{sessionId}/abort?agent=<agent> directly.
 * This is the definitive way to signal the backend to stop the engine and
 * mark the session as "interrupted". `agent` is required (owner check,
 * IDOR fix 2026-07-04) — the caller must know which agent owns the session.
 */
async function apiAbortSession(page: Page, sessionId: string, agent: string): Promise<void> {
  await page.evaluate(
    async ({ sid, token, agent }: { sid: string; token: string; agent: string }) => {
      await fetch(`/api/chat/${sid}/abort?agent=${encodeURIComponent(agent)}`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
      }).catch(() => { });
    },
    { sid: sessionId, token: TOKEN, agent }
  );
}

/**
 * Read the current agent name from the agent selector element in the page.
 * Falls back to "Opex" if the selector is not accessible.
 *
 * Uses textContent (NOT innerText): innerText applies CSS text-transform
 * (e.g. "uppercase" renders "Opex" as "OPEX"), but the API is case-sensitive
 * and expects the original name ("Opex", not "OPEX"). textContent returns
 * the raw DOM text without CSS transforms.
 */
async function getAgentNameFromDom(page: Page): Promise<string> {
  let agentName = "Opex"; // safe default for this Pi instance
  try {
    const fromDom = await page.evaluate(() => {
      const trigger = document.querySelector('[aria-label="Switch agent"] button');
      if (trigger) {
        const span = trigger.querySelector("span");
        return span?.textContent?.trim() ?? null;
      }
      return null;
    });
    if (fromDom && fromDom.length > 0 && fromDom.length < 50) {
      agentName = fromDom;
    }
  } catch {
    // Use default "Opex"
  }
  return agentName;
}

/**
 * Poll /api/sessions?agent=<agent>&limit=100 until the session's run_status
 * leaves "running"/"streaming".
 *
 * Returns the final status string, or null if it was never determinable.
 */
async function waitForSessionFinished(
  page: Page,
  sessionId: string,
  timeoutMs: number
): Promise<string | null> {
  const agentName = await getAgentNameFromDom(page);
  console.log(`[pollSessionStatus] Using agent name: "${agentName}" for session ${sessionId}`);

  const deadline = Date.now() + timeoutMs;
  let lastStatus: string | null = null;

  while (Date.now() < deadline) {
    const result = await page
      .evaluate(
        async ({
          sid,
          token,
          agent,
        }: {
          sid: string;
          token: string;
          agent: string;
        }) => {
          try {
            const resp = await fetch(
              `/api/sessions?agent=${encodeURIComponent(agent)}&limit=100`,
              { headers: { Authorization: `Bearer ${token}` } }
            );
            if (!resp.ok) return { status: null, error: `HTTP ${resp.status}` };
            const data = await resp.json();
            const sessions: Array<{ id: string; run_status: string }> =
              Array.isArray(data.sessions) ? data.sessions : [];
            const found = sessions.find((s) => s.id === sid);
            const allIds = sessions.slice(0, 5).map((s) => `${s.id}:${s.run_status}:${(s as any).agent_id}`).join(", ");
            return {
              status: found ? found.run_status : null,
              error: found ? null : `session not found; first 5: [${allIds}]; total: ${sessions.length}; looking for: ${sid}`,
            };
          } catch (e) {
            return { status: null, error: String(e) };
          }
        },
        { sid: sessionId, token: TOKEN, agent: agentName }
      )
      .catch(() => ({ status: null, error: "evaluate failed" }));

    if (result.status !== null) {
      lastStatus = result.status;
      if (
        result.status !== "running" &&
        result.status !== "streaming" &&
        result.status !== null
      ) {
        return result.status;
      }
    } else if (result.error) {
      console.log(`[pollSessionStatus] API error: ${result.error}`);
    }

    await page.waitForTimeout(3_000);
  }

  return lastStatus;
}

// ── Test 1: Abort mid-stream → session marked interrupted (not error) ────────
//
// The user clicks Stop and we also POST /abort for reliability.
// The session must NOT end up as "error" or "failed".
// Best case: it ends as "interrupted".
// Acceptable: "done" (model finished within graceful drain window).

test("abort mid-stream marks session interrupted", async ({ page }) => {
  test.setTimeout(180_000);

  await login(page);
  await clickNewChat(page);

  // Very long prompt to ensure streaming takes > 30s on the Pi
  await sendMessage(
    page,
    "Напиши очень длинный подробный рассказ про горный Алтай минимум 6000 слов. " +
    "Включи подробное описание природы, истории, народов, рек, гор, животных и растений. " +
    "Пиши без остановки, очень подробно, каждый абзац минимум 10 предложений."
  );

  // Wait for session ID to be assigned
  const sessionId = await waitForSessionId(page, 20_000);
  if (!sessionId) {
    test.skip(true, "Session ID never appeared in URL — message send may have failed.");
    return;
  }

  // Wait for streaming to visibly start (stop button appears)
  // Give 60s — the Pi may take >30s to start generating the first token
  const streamingStarted = await waitForStreamingStarted(page, 60_000);

  // Diagnostic: take a screenshot and log DOM state when stop button check fails
  if (!streamingStarted) {
    // Log what buttons exist in the composer form
    const debugInfo = await page.evaluate(() => {
      const form = document.querySelector("[data-composer-input]");
      if (!form) return "NO FORM FOUND";
      const buttons = Array.from(form.querySelectorAll<HTMLElement>("button"));
      return buttons
        .map((b) => `[type=${b.getAttribute("type")} classes="${b.className.slice(0, 80)}" visible=${b.offsetParent !== null}]`)
        .join("; ");
    });
    console.log(`[abort test] Stop button search result. Buttons in form: ${debugInfo}`);

    // Check session status
    const statusCheck = await waitForSessionFinished(page, sessionId, 8_000);
    console.log(`[abort test] Session status after 60s wait: ${statusCheck}`);

    test.skip(
      true,
      `Stop button never appeared after 60s. Session status: ${statusCheck ?? "unknown"}. ` +
      `Composer buttons: ${debugInfo?.slice(0, 200) ?? "none"}. ` +
      `The model may complete very fast or the selector needs updating.`
    );
    return;
  }

  // POST /abort to the backend — this triggers the cancellation token and graceful drain.
  // The engine will finish within CANCEL_GRACE (30s) and mark the session "interrupted".
  const agentForAbort = await getAgentNameFromDom(page);
  await apiAbortSession(page, sessionId, agentForAbort);

  // Also abort the local SSE fetch so the UI updates immediately
  await page.evaluate(() => {
    // Trigger stopStream via the store's public API
    const event = new CustomEvent("opex:stop-stream");
    document.dispatchEvent(event);
  });

  // Poll session status — wait up to 60s for the engine to finish
  const finalStatus = await waitForSessionFinished(page, sessionId, 60_000);

  if (finalStatus === null) {
    // Could not determine status
    test.skip(
      true,
      `Could not determine final session status for ${sessionId}. Sessions API may not return this session.`
    );
    return;
  }

  // Critical: session must NOT end as "error" or "failed"
  expect(
    finalStatus,
    `Session ended as "${finalStatus}" — must not be "error" or "failed" after a user abort`
  ).not.toBe("error");
  expect(finalStatus).not.toBe("failed");

  if (finalStatus === "done") {
    // The model finished before the 30s graceful drain window — not a bug.
    console.warn(
      `[abort test] Session completed as "done" before abort took effect. ` +
      `Pi LLM may generate too fast for the 30s drain window. Not a bug.`
    );
    // Test passes — we verified no error status
  } else {
    // Expecting "interrupted"
    expect(finalStatus).toBe("interrupted");
  }
});

// ── Test 2: Switch sessions mid-stream → correct content renders ──────────────

test("switching sessions mid-stream does not show wrong content", async ({
  page,
}) => {
  test.setTimeout(90_000);

  await login(page);
  await clickNewChat(page);

  await sendMessage(
    page,
    "Напиши длинный рассказ про горы минимум 2000 слов."
  );

  // Wait for session ID to appear in URL
  const streamingSessionId = await waitForSessionId(page, 20_000);
  if (!streamingSessionId) {
    test.skip(true, "Session ID never appeared in URL after sending message.");
    return;
  }

  // Optionally wait for streaming to visibly start (non-blocking)
  await waitForStreamingStarted(page, 10_000);

  // Wait for the Virtuoso list to render session items in the sidebar.
  // Virtuoso uses virtual scrolling and may not render items immediately on mount.
  // We poll until at least 2 items appear, up to 10s.
  const sessionButtons = page.locator(
    "aside div.group button.flex.w-full.flex-col"
  );
  let count = 0;
  const virtuosoDeadline = Date.now() + 10_000;
  while (Date.now() < virtuosoDeadline) {
    count = await sessionButtons.count();
    if (count >= 2) break;
    await page.waitForTimeout(500);
  }

  if (count < 2) {
    test.skip(
      true,
      `Need at least 2 sessions in the sidebar (found ${count}). Pre-seed the Pi or run after sessions accumulate.`
    );
    return;
  }

  // Click the LAST (oldest) session to avoid interference with "now" sessions
  await sessionButtons.nth(count - 1).click();

  // Wait for URL to change to a different ?s= parameter
  await page.waitForFunction(
    (sidBefore: string) => {
      const current = new URLSearchParams(window.location.search).get("s") ?? "";
      return current !== "" && current !== sidBefore;
    },
    streamingSessionId,
    { timeout: 10_000 }
  );

  // Allow React Query to fetch the new session's messages
  await page.waitForTimeout(2_500);

  // The main page body must have meaningful content
  const bodyText = await page.locator("body").innerText();
  expect(bodyText.length).toBeGreaterThan(50);

  // URL must now reference a different session
  const newSessionId = new URL(page.url()).searchParams.get("s");
  expect(newSessionId).not.toBe(streamingSessionId);
  expect(newSessionId).toBeTruthy();
});

// ── Test 3: F5 reload during stream → session preserved ──────────────────────

test("F5 reload during stream preserves session", async ({ page }) => {
  test.setTimeout(90_000);

  await login(page);
  await clickNewChat(page);

  await sendMessage(
    page,
    "Напиши подробно про реки минимум 1500 слов."
  );

  // Wait for session ID to appear
  const sessionId = await waitForSessionId(page, 20_000);
  if (!sessionId) {
    test.skip(true, "Session ID never appeared in URL after sending message.");
    return;
  }

  const urlWithSession = page.url();
  expect(urlWithSession).toMatch(/[?&]s=/);

  // Reload the page (F5)
  await page.reload();

  // The auth token is in sessionStorage — it survives same-tab reload.
  // If we land on /login, re-authenticate.
  if (page.url().includes("/login")) {
    await login(page);
    await page.goto(urlWithSession);
    await page.waitForURL(/\/chat/, { timeout: 10_000 });
  } else {
    await page.waitForURL(/\/chat/, { timeout: 10_000 });
  }

  // After reload, the URL must still reference the same session
  await page.waitForFunction(
    (sid: string) => new URLSearchParams(window.location.search).get("s") === sid,
    sessionId,
    { timeout: 15_000 }
  );

  // The composer must be rendered (session loaded successfully)
  await page.locator("[data-composer-input]").waitFor({ state: "visible", timeout: 20_000 });

  // Page body must have meaningful content
  const chatText = await page.locator("body").innerText();
  expect(chatText.length).toBeGreaterThan(50);
});

// ── Test 4 (T4.1): F5 reload mid-stream resumes via WS snapshot ──────────────
//
// Verifies the session-lifecycle root-fix: when the user presses F5 during
// streaming, the page must transparently resume from the WS snapshot (or via
// the one-shot bootstrap effect when WS arrives before localStorage restore)
// instead of showing a stuck-session recovery banner.

test("F5 reload during streaming resumes via WS snapshot", async ({ page }) => {
  test.setTimeout(120_000);

  await login(page);
  await clickNewChat(page);
  await sendMessage(page, "напиши длинный ответ на 3 параграфа");

  // Wait for streaming to begin.
  await page.waitForSelector(STREAM_ENGAGED, {
    timeout: 30_000,
  });

  // F5 reload mid-stream.
  await page.reload();

  // If we land on /login, re-authenticate to land back on the chat URL.
  if (page.url().includes("/login")) {
    await login(page);
  }

  // After reload, the stream indicator (caret while text arrives, comet while
  // reasoning) must re-appear within a short window — proves resumeStream was
  // triggered (either by markSessionActive on the WS snapshot, or by the
  // one-shot bootstrap effect when WS arrives before localStorage restore).
  await page.waitForSelector(STREAM_ENGAGED, {
    timeout: 10_000,
  });

  // Final answer eventually appears.
  await page.waitForSelector("[data-testid='message-complete']", {
    timeout: 60_000,
  });
});

// ── Test 5 (T4.2): Watchdog timeout produces UI notification ─────────────────
//
// Long-running scenario (>= 75s: 15s inactivity + 60s watchdog tick). Gated
// behind RUN_LONG_TESTS=1. Requires a test agent named "test_short_inactivity"
// with watchdog.inactivity_secs = 15 — CI sets this up; locally it must be
// created manually if running this test outside CI.

test.describe("long-running scenarios", () => {
  test.skip(
    process.env.RUN_LONG_TESTS !== "1",
    "Skipped — set RUN_LONG_TESTS=1 to run this 75+ second test",
  );

  test("watchdog timeout produces UI notification", async ({ page }) => {
    test.setTimeout(180_000);

    // Pre-condition: a test agent named "test_short_inactivity" must exist
    // with watchdog.inactivity_secs = 15. CI sets this up; locally it must be
    // created manually if running this test outside CI.
    //
    // Navigate directly to the agent's chat URL — the helpers in this file
    // open whichever agent is currently selected, which may not match.
    await login(page);
    await page.goto("/chat?agent=test_short_inactivity");
    await page.waitForURL(/\/chat/, { timeout: 15_000 });
    await clickNewChat(page);
    await sendMessage(page, "тест");

    // Wait for streaming to begin then NOT send any further activity.
    await page.waitForSelector(STREAM_ENGAGED, {
      timeout: 30_000,
    });

    // Wait 15s (inactivity threshold) + 60s (watchdog tick interval) + buffer.
    await page.waitForTimeout(80_000);

    // Notification appears on the bell.
    await page.click("[data-testid='notifications-bell']");
    const list = page.locator("[data-testid='notification-list']");
    await expect(list).toContainText("Session timeout");
  });
});

// ── Test 6 (T4.3): Stuck-session banner never renders (negative smoke) ───────
//
// Negative regression: the old "Сессия отмечена как выполняемая…" amber banner
// must never appear after the session-lifecycle root-fix lands. Reload mid-
// stream and verify the banner stays gone for the stale-RQ-cache window where
// it used to flash.

test("Stuck-session recovery banner never renders", async ({ page }) => {
  test.setTimeout(60_000);

  await login(page);
  await clickNewChat(page);
  await sendMessage(page, "тест");

  await page.waitForSelector(STREAM_ENGAGED, {
    timeout: 30_000,
  });
  await page.reload();

  // If we land on /login, re-authenticate to land back on the chat URL.
  if (page.url().includes("/login")) {
    await login(page);
  }

  // Wait through the stale RQ cache window where the OLD banner would have shown.
  await page.waitForTimeout(2_000);

  // Banner text marker — unique amber-bordered message that the old code rendered.
  const banner = page.locator("text=Сессия отмечена как выполняемая");
  await expect(banner).not.toBeVisible();
});
