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

  it("a rejecting play does not deadlock the pump — later sentences and enqueues still play", async () => {
    // Regression test for Fix 1: previously `await deps.play(blob)` was
    // wrapped only in try/finally with no catch, so a rejection propagated
    // out of the fire-and-forget `pump()`, skipping the `pumping = false`
    // reset. Every future sentence would silently queue and never play.
    const played: string[] = [];
    const synth = vi.fn(async (s: string) => new Blob([s]));
    const play = vi.fn(async (b: Blob) => {
      const text = await b.text();
      if (text === "first.") throw new Error("autoplay blocked");
      played.push(text);
    });
    const q = createSpeakerQueue({ synth, play });

    q.enqueue("first.");
    q.enqueue("second.");
    // "first." rejects but must not stop "second." from playing.
    await vi.waitFor(() => expect(played).toEqual(["second."]));
    await vi.waitFor(() => expect(q.idle).toBe(true));

    // The queue must still be usable afterward — pump must have been
    // released, not left permanently "pumping".
    q.enqueue("third.");
    await vi.waitFor(() => expect(played).toEqual(["second.", "third."]));
    expect(q.idle).toBe(true);
  });

  it("takeoverAudio mid-play: superseded play settling early must not report idle prematurely", async () => {
    // Regression test for Fix 2: previously the pump loop's `finally {
    // playing = false }` after `await deps.play(blob)` was unconditional.
    // In real life, when takeoverAudio interrupts an in-flight play on the
    // same shared <audio> element, the OLD play's promise commonly settles
    // (e.g. "interrupted by a new load request") *before* the new
    // (takeover) play settles. If that stale settlement clobbers `playing`
    // to false, the queue would wrongly report `idle: true` while the
    // takeover audio is still actually playing.
    const played: string[] = [];
    let releaseOld!: () => void;
    let releaseNew!: () => void;
    const oldGate = new Promise<void>((resolve) => {
      releaseOld = resolve;
    });
    const newGate = new Promise<void>((resolve) => {
      releaseNew = resolve;
    });

    const synth = vi.fn(async (s: string) => new Blob([s]));
    const play = vi.fn(async (b: Blob) => {
      const text = await b.text();
      if (text === "slow-sentence.") await oldGate;
      if (text === "TAKEOVER_AUDIO") await newGate;
      played.push(text);
    });
    const q = createSpeakerQueue({ synth, play });

    q.enqueue("slow-sentence.");
    await vi.waitFor(() => expect(play).toHaveBeenCalledTimes(1));

    // Takeover fires while the first play is still pending.
    q.takeoverAudio(new Blob(["TAKEOVER_AUDIO"]));
    await vi.waitFor(() => expect(play).toHaveBeenCalledTimes(2));

    // Release the superseded ("slow-sentence.") play FIRST — its stale
    // completion must not clobber the takeover's still-in-flight state.
    releaseOld();
    await vi.waitFor(() => expect(played).toContain("slow-sentence."));
    expect(q.idle).toBe(false); // takeover audio is still playing

    // Now let the takeover's own play settle — idle should reflect that.
    releaseNew();
    await vi.waitFor(() => expect(played).toContain("TAKEOVER_AUDIO"));
    expect(q.idle).toBe(true);
    expect(played).toEqual(["slow-sentence.", "TAKEOVER_AUDIO"]);
  });
});
