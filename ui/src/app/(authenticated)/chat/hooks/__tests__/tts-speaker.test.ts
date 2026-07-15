import { describe, it, expect, vi } from "vitest";
import { createSentenceSplitter, createSpeakerQueue, type SpeakerDeps } from "../tts-speaker";

describe("createSentenceSplitter", () => {
  it("splits on sentence boundaries", () => {
    const s = createSentenceSplitter();
    // NOTE: long enough to clear the default minLen=20 gate on its own —
    // the brief's original fixture ("Привет мир.", 11 chars) would have been
    // held by the min-length rule exercised below, so it was lengthened here
    // to keep this test consistent with that rule instead of contradicting it.
    expect(s.push("Это готовое предложение для проверки границ. ")).toEqual([
      "Это готовое предложение для проверки границ.",
    ]);
  });

  it("holds short fragments until min length", () => {
    const s = createSentenceSplitter();
    expect(s.push("Да. ")).toEqual([]); // <20 симв — копится
    expect(s.push("И ещё длинное предложение тут. ")).toEqual([
      "Да. И ещё длинное предложение тут.",
    ]);
  });

  it("flush returns remainder", () => {
    const s = createSentenceSplitter();
    s.push("Хвост без точки");
    expect(s.flush()).toEqual(["Хвост без точки"]);
  });

  it("flush returns [] when nothing is left", () => {
    const s = createSentenceSplitter();
    s.push("Достаточно длинное предложение с точкой. ");
    expect(s.flush()).toEqual([]);
  });

  it("strips markdown before emit", () => {
    const s = createSentenceSplitter();
    const out = s.push("## Заголовок с достаточной длиной текста. ");
    expect(out).toHaveLength(1);
    expect(out[0]).not.toContain("#");
  });

  it("code fences become placeholder", () => {
    const s = createSentenceSplitter();
    s.push("```js\ncode\n```");
    expect(s.flush().join(" ")).toContain("код");
  });

  it("strips list markers and turns markdown links into plain text", () => {
    const s = createSentenceSplitter();
    const out = s.push("- Смотри [тут](https://example.com) — предложение длинное. ");
    expect(out).toHaveLength(1);
    expect(out[0]).not.toMatch(/^-/);
    expect(out[0]).toContain("тут");
    expect(out[0]).not.toContain("https://example.com");
  });
});

describe("createSpeakerQueue", () => {
  function fakeDeps() {
    const played: string[] = [];
    const deps: SpeakerDeps = {
      synth: vi.fn(async (s: string) => new Blob([s])),
      play: vi.fn(async (b: Blob) => {
        played.push(await b.text());
      }),
      onStateChange: vi.fn(),
      onDrain: vi.fn(),
    };
    return { deps, played };
  }

  it("plays sentences in order", async () => {
    const { deps, played } = fakeDeps();
    const q = createSpeakerQueue(deps);
    q.enqueue("first.");
    q.enqueue("second.");
    await vi.waitFor(() => expect(played).toEqual(["first.", "second."]));
    expect(q.idle).toBe(true);
  });

  it("emits speaking/idle state transitions and fires onDrain once empty", async () => {
    const { deps } = fakeDeps();
    const q = createSpeakerQueue(deps);
    expect(q.idle).toBe(true);
    q.enqueue("only.");
    expect(q.idle).toBe(false);
    await vi.waitFor(() => expect(deps.onDrain).toHaveBeenCalledTimes(1));
    expect(deps.onStateChange).toHaveBeenCalledWith("speaking");
    expect(deps.onStateChange).toHaveBeenCalledWith("idle");
    expect(q.idle).toBe(true);
  });

  it("skips a sentence when synth resolves null (e.g. 409) without throwing", async () => {
    const played: string[] = [];
    const synth = vi.fn(async (s: string) => (s === "bad." ? null : new Blob([s])));
    const play = vi.fn(async (b: Blob) => {
      played.push(await b.text());
    });
    const q = createSpeakerQueue({ synth, play });
    q.enqueue("bad.");
    q.enqueue("good.");
    await vi.waitFor(() => expect(played).toEqual(["good."]));
  });

  it("cancel aborts pending synth and stops — no play happens afterward", async () => {
    const { deps, played } = fakeDeps();
    const q = createSpeakerQueue(deps);
    q.enqueue("a.");
    q.cancel();
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(played).toEqual([]);
    expect(deps.play).not.toHaveBeenCalled();
    expect(q.idle).toBe(true);
  });

  it("takeoverAudio clears queue and plays given blob", async () => {
    const { deps, played } = fakeDeps();
    const q = createSpeakerQueue(deps);
    q.enqueue("text-sentence.");
    q.takeoverAudio(new Blob(["AGENT_AUDIO"]));
    await vi.waitFor(() => expect(played).toContain("AGENT_AUDIO"));
    expect(played).not.toContain("text-sentence.");
  });

  it("idle is false while playing and true again after drain", async () => {
    const { deps } = fakeDeps();
    const q = createSpeakerQueue(deps);
    q.enqueue("one.");
    expect(q.idle).toBe(false);
    await vi.waitFor(() => expect(q.idle).toBe(true));
  });
});
