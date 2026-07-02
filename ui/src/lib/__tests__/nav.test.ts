import { test, expect } from "vitest";
import { pageHasOwnHeader } from "../nav";

test.each(["/chat", "/chat/", "/workspace", "/workspace/"])(
  "%s has its own header (trailing slash tolerant)",
  (p) => {
    expect(pageHasOwnHeader(p)).toBe(true);
  },
);

test.each(["/agents/", "/webhooks/", "/"])("%s uses the shared header", (p) => {
  expect(pageHasOwnHeader(p)).toBe(false);
});
