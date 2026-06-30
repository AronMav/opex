import { create } from "zustand";

// Bridges the voice-reply trigger (ChatComposer, which knows a voice-initiated
// turn just finished) to the inline AudioPlayer that actually renders the
// synthesize_speech audio in the chat. ChatComposer requests autoplay of a URL;
// the matching AudioPlayer plays it on its VISIBLE element (so the user sees the
// player start), and reports `playing` so the hands-free loop waits before
// re-arming the mic (otherwise it would record the agent's own TTS).
interface VoicePlaybackState {
  /** URL the inline player should auto-play once, or null. */
  autoplayUrl: string | null;
  /** True while a voice reply is auto-playing (gates hands-free re-arm). */
  playing: boolean;
  /** ChatComposer: ask the inline player for `url` to auto-play. Optimistically
   *  marks `playing` so the re-arm gate closes before the element starts. */
  requestAutoplay: (url: string) => void;
  /** AudioPlayer: consume the request so it fires exactly once. */
  consumeAutoplay: () => void;
  /** AudioPlayer: report play start/stop (ended, paused, or blocked). */
  setPlaying: (playing: boolean) => void;
}

export const useVoicePlaybackStore = create<VoicePlaybackState>((set) => ({
  autoplayUrl: null,
  playing: false,
  requestAutoplay: (url) => set({ autoplayUrl: url, playing: true }),
  consumeAutoplay: () => set({ autoplayUrl: null }),
  setPlaying: (playing) => set({ playing }),
}));
