import { describe, it, expect, beforeEach } from "vitest";

import { useVoicePlaybackStore } from "./voice-playback-store";

describe("voice-playback-store", () => {
  beforeEach(() => {
    useVoicePlaybackStore.setState({ autoplayUrl: null, playing: false });
  });

  it("requestAutoplay sets the url AND marks playing (closes the re-arm gate at once)", () => {
    useVoicePlaybackStore.getState().requestAutoplay("/api/uploads/v1?sig=x");
    const s = useVoicePlaybackStore.getState();
    expect(s.autoplayUrl).toBe("/api/uploads/v1?sig=x");
    expect(s.playing).toBe(true);
  });

  it("consumeAutoplay clears the url but keeps playing (the player is still sounding)", () => {
    useVoicePlaybackStore.getState().requestAutoplay("/u/x");
    useVoicePlaybackStore.getState().consumeAutoplay();
    const s = useVoicePlaybackStore.getState();
    expect(s.autoplayUrl).toBeNull();
    expect(s.playing).toBe(true);
  });

  it("setPlaying(false) releases the hands-free gate when the reply ends or is blocked", () => {
    useVoicePlaybackStore.getState().requestAutoplay("/u/x");
    useVoicePlaybackStore.getState().setPlaying(false);
    expect(useVoicePlaybackStore.getState().playing).toBe(false);
  });
});
