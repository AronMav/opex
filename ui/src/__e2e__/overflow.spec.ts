import { test, expect } from "@playwright/test";

/**
 * Self-contained overflow guard. Loads the /overflow-check harness (rendered
 * from the static export by the config's webServer, no backend) at three
 * viewport widths and asserts NO widget distends its fixed-width probe.
 *
 * Invariant is per-probe (scrollWidth <= clientWidth), NOT document-level:
 * globals.css has `html { overflow-x: hidden }`, which hides document overflow
 * and would mask clipping. A scrollable child (tab list) absorbs its own
 * overflow so its probe stays un-distended; a clipping/distending child pushes
 * the probe wider and fails here.
 */

const HARNESS = "http://localhost:4321/overflow-check";
const WIDTHS = [375, 768, 1280];

for (const width of WIDTHS) {
  test(`no widget overflows at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(HARNESS, { waitUntil: "networkidle" });

    const offenders = await page.$$eval("[data-overflow-check]", (nodes) =>
      nodes
        .filter((n) => n.scrollWidth - n.clientWidth > 1)
        .map((n) => ({
          id: n.getAttribute("data-overflow-check"),
          scrollWidth: n.scrollWidth,
          clientWidth: n.clientWidth,
        })),
    );

    expect(
      offenders,
      `Probes overflowing their container at ${width}px: ${JSON.stringify(offenders)}`,
    ).toEqual([]);
  });
}
