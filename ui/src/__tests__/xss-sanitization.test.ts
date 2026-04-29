import { describe, it, expect } from "vitest";
import { sanitizeUrl } from "@/lib/sanitize-url";

describe("sanitizeUrl", () => {
  // ── Allowed protocols ────────────────────────────────────────────────────

  it("allows relative URLs starting with /", () => {
    expect(sanitizeUrl("/uploads/file.png")).toBe("/uploads/file.png");
  });

  it("allows https URLs", () => {
    expect(sanitizeUrl("https://example.com/img.jpg")).toBe("https://example.com/img.jpg");
  });

  it("allows http URLs", () => {
    expect(sanitizeUrl("http://example.com/img.jpg")).toBe("http://example.com/img.jpg");
  });

  it("allows data:image/png for inline images", () => {
    expect(sanitizeUrl("data:image/png;base64,iVBOR")).toBe("data:image/png;base64,iVBOR");
  });

  it("allows data:image/jpeg", () => {
    expect(sanitizeUrl("data:image/jpeg;base64,/9j/4AAQ")).toBe("data:image/jpeg;base64,/9j/4AAQ");
  });

  it("allows data:image/svg+xml", () => {
    expect(sanitizeUrl("data:image/svg+xml;base64,PHN2Zz4=")).toBe("data:image/svg+xml;base64,PHN2Zz4=");
  });

  it("allows data:image/gif", () => {
    expect(sanitizeUrl("data:image/gif;base64,R0lGOD")).toBe("data:image/gif;base64,R0lGOD");
  });

  // ── Blocked XSS vectors ──────────────────────────────────────────────────

  it("blocks javascript: protocol", () => {
    expect(sanitizeUrl("javascript:alert(1)")).toBe("#");
  });

  it("blocks JAVASCRIPT: (case-insensitive)", () => {
    expect(sanitizeUrl("JAVASCRIPT:alert(1)")).toBe("#");
  });

  it("blocks JavaScript: (mixed case)", () => {
    expect(sanitizeUrl("JavaScript:alert(1)")).toBe("#");
  });

  it("blocks vbscript: protocol", () => {
    expect(sanitizeUrl("vbscript:MsgBox(1)")).toBe("#");
  });

  it("blocks data:text/html", () => {
    expect(sanitizeUrl("data:text/html,<script>alert(1)</script>")).toBe("#");
  });

  it("blocks data:text/javascript", () => {
    expect(sanitizeUrl("data:text/javascript,alert(1)")).toBe("#");
  });

  it("blocks data:application/javascript", () => {
    expect(sanitizeUrl("data:application/javascript,alert(1)")).toBe("#");
  });

  it("blocks data:application/xml", () => {
    expect(sanitizeUrl("data:application/xml,<x/>")).toBe("#");
  });

  // ── Edge / boundary cases ─────────────────────────────────────────────────

  it("returns # for empty string", () => {
    expect(sanitizeUrl("")).toBe("#");
  });

  it("trims whitespace before checking", () => {
    expect(sanitizeUrl("  javascript:alert(1)  ")).toBe("#");
  });

  it("trims whitespace from safe URLs", () => {
    expect(sanitizeUrl("  /uploads/file.png  ")).toBe("/uploads/file.png");
  });

  it("returns # for whitespace-only input", () => {
    expect(sanitizeUrl("   ")).toBe("#");
  });

  it("returns # for non-parseable string", () => {
    expect(sanitizeUrl("not a url at all %%invalid")).toBe("#");
  });

  it("returns # for file: protocol (server-side path leak)", () => {
    expect(sanitizeUrl("file:///etc/passwd")).toBe("#");
  });

  it("returns # for ftp: protocol (not in allowlist)", () => {
    expect(sanitizeUrl("ftp://files.example.com/pub/file.zip")).toBe("#");
  });

  it("allows data:image/webp", () => {
    expect(sanitizeUrl("data:image/webp;base64,AAAA")).toBe("data:image/webp;base64,AAAA");
  });
});
