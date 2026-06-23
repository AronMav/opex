import { test, expect } from '@playwright/test';

/**
 * Basic E2E test suite for OPEX Chat.
 * These tests focus on core functionality and user flows.
 */
test.describe('Chat Core Flows', () => {

  test.beforeEach(async ({ page }) => {
    // Inject a test token to bypass the setup wizard and login
    await page.addInitScript(() => {
      window.localStorage.setItem('auth-storage', JSON.stringify({
        state: { token: 'test-token', isAuthenticated: true }
      }));
    });
    await page.goto('/chat');
  });

  test('should display chat interface and sidebar', async ({ page }) => {
    // Check if the sidebar is visible
    const sidebar = page.locator('aside');
    await expect(sidebar).toBeVisible();

    // Check if the chat input is present
    const composer = page.locator('textarea');
    await expect(composer).toBeVisible();
  });

  test('should allow sending a message and seeing assistant response', async ({ page }) => {
    const composer = page.locator('textarea');
    const sendButton = page.locator('button[title="Send message"]');

    // Type and send
    await composer.fill('Hello Opex, tell me about yourself.');
    await sendButton.click();

    // Verify user message appears
    await expect(page.getByText('Hello Opex, tell me about yourself.')).toBeVisible();

    // Wait for assistant response (streamed)
    // We expect at least some assistant message container to appear
    const assistantMessage = page.locator('[data-role="assistant"]').last();
    await expect(assistantMessage).toBeVisible({ timeout: 10000 });
  });

  test('should transition to history mode after stream finish', async ({ page }) => {
    // 1. Send message
    await page.locator('textarea').fill('Echo test.');
    await page.locator('button[title="Send message"]').click();

    // 2. Wait for streaming to finish (connectionPhase should become idle)
    // We can check if the stop button disappears or just wait for the SSE stream to close
    const stopButton = page.locator('button[title="Stop generation"]');
    await expect(stopButton).not.toBeVisible({ timeout: 15000 });

    // 3. Verify we can still see the message after "reload" (simulated by state)
    // Or just check if the session title appeared in the sidebar
    const sessionItem = page.locator('aside >> text=Echo test').first();
    await expect(sessionItem).toBeVisible();
  });
});
