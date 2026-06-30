import { describe, it, expect, vi, beforeEach } from "vitest";

import { autoPlayVoiceReply } from "./stream-processor";

// Capture every src that reaches Audio.play(). Module-level + cleared (not
// reassigned) so the lazily-created shared element keeps referencing it.
const playedSrcs: string[] = [];

class FakeAudio {
  src = "";
  play(): Promise<void> {
    playedSrcs.push(this.src);
    return Promise.resolve();
  }
}

beforeEach(() => {
  playedSrcs.length = 0;
  vi.stubGlobal("Audio", FakeAudio);
});

describe("autoPlayVoiceReply", () => {
  it("auto-plays a freshly-streamed voice URL", () => {
    const url = "/api/uploads/voice-1?sig=a";
    autoPlayVoiceReply(url);
    expect(playedSrcs).toContain(url);
  });

  it("de-dupes: the same URL never plays twice (reconciliation re-process safe)", () => {
    const url = "/api/uploads/voice-2?sig=b";
    autoPlayVoiceReply(url);
    expect(playedSrcs.filter((s) => s === url)).toHaveLength(1);
    autoPlayVoiceReply(url); // second arrival of the same audio → no-op
    expect(playedSrcs.filter((s) => s === url)).toHaveLength(1);
  });

  it("ignores empty URLs", () => {
    autoPlayVoiceReply("");
    expect(playedSrcs.filter((s) => s === "")).toHaveLength(0);
  });
});
