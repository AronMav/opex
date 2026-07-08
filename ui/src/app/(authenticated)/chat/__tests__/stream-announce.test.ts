import { describe, it, expect } from "vitest";
import { nextSentences } from "../stream-announce";

describe("nextSentences", () => {
  it("withholds an unterminated trailing fragment", () => {
    const { toAnnounce, newOffset } = nextSentences("Hello world. Second", 0);
    expect(toAnnounce.trim()).toBe("Hello world.");
    expect(newOffset).toBeGreaterThan(0);
    expect(newOffset).toBeLessThan("Hello world. Second".length);
  });

  it("is idempotent once caught up (nothing new to say)", () => {
    const first = nextSentences("Hello world. Second", 0);
    const again = nextSentences("Hello world. Second", first.newOffset);
    expect(again.toAnnounce).toBe("");
    expect(again.newOffset).toBe(first.newOffset);
  });

  it("emits completed sentences as a batch on first call, then empty on subsequent calls at end", () => {
    const first = nextSentences("Hello world. Second sentence.", 0);
    expect(first.toAnnounce.trim()).toBe("Hello world. Second sentence.");
    expect(first.newOffset).toBe("Hello world. Second sentence.".length);
    const second = nextSentences("Hello world. Second sentence.", first.newOffset);
    expect(second.toAnnounce).toBe("");
    expect(second.newOffset).toBe(first.newOffset);
  });

  it("flush speaks the trailing fragment", () => {
    const { toAnnounce, newOffset } = nextSentences("Hello world. Tail", 0, { flush: true });
    expect(toAnnounce).toContain("Tail");
    expect(newOffset).toBe("Hello world. Tail".length);
  });

  it("handles Cyrillic and ellipsis", () => {
    const { toAnnounce } = nextSentences("Привет мир… Второе", 0);
    expect(toAnnounce.trim()).toBe("Привет мир…");
  });

  it("softCap emits a word-boundary chunk when there is no sentence end", () => {
    const long = "word ".repeat(80); // 400 chars, no terminal punctuation
    const { toAnnounce, newOffset } = nextSentences(long, 0, { softCap: 300 });
    expect(toAnnounce.length).toBeGreaterThan(0);
    expect(toAnnounce.length).toBeLessThanOrEqual(300);
    expect(toAnnounce.endsWith(" ") || toAnnounce.endsWith("word")).toBe(true);
    expect(newOffset).toBe(toAnnounce.length);
  });

  it("returns empty for an empty remainder", () => {
    expect(nextSentences("", 0)).toEqual({ toAnnounce: "", newOffset: 0 });
    expect(nextSentences("abc", 3)).toEqual({ toAnnounce: "", newOffset: 3 });
  });

  it("batches multiple completed sentences in one call and withholds the tail", () => {
    const { toAnnounce } = nextSentences("A. B. C", 0);
    expect(toAnnounce.trim()).toBe("A. B.");
  });
});
