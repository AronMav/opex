import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, waitFor, screen, fireEvent } from "@testing-library/react";

// Task 6: streaming per-sentence TTS via hooks/tts-speaker. For a VOICE turn the
// composer feeds the assistant's streaming TEXT deltas through the sentence
// splitter and synthesises each complete sentence via /api/tts/synthesize,
// playing them in order on a single <audio> element. An audio/* file part on the
// reply (agent-produced voice) TAKES OVER — the per-sentence queue is aborted and
// the agent audio is played instead.

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "ru" }),
}));

vi.mock("@/lib/api", () => ({
  assertToken: () => "test-token",
  apiGet: vi.fn(),
  apiPost: vi.fn(),
}));

vi.mock("../ModelDropdown", () => ({ ModelDropdown: () => null }));

// current agent has both stt+tts capabilities → voice controls render.
vi.mock("@/lib/queries", () => ({
  useAgents: () => ({
    data: [{ name: "main", capabilities: { text: true, stt: true, tts: true, vision: false, imagegen: false, websearch: false } }],
  }),
  useProviders: () => ({ data: [] }),
  useProviderModels: () => ({ data: [] }),
  useProviderModelsDetailed: () => ({ data: [] }),
}));

vi.mock("@/hooks/use-commands", () => ({
  useCommands: () => ({ data: [] }),
}));

vi.mock("@/lib/prompts", () => ({
  usePrompts: () => ({ prompts: [], isLoading: false }),
}));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { agents: ["main"], token: "test-token" };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", currentAgent: "main" }) },
  ),
}));

// ── Mutable chat store ────────────────────────────────────────────────────────
type Part = { type: string; text?: string; url?: string; mediaType?: string };
type Msg = { id: string; role: "user" | "assistant"; parts: Part[] };
type Source = { mode: "live"; messages: Msg[] } | { mode: "new-chat" };

const chatState = {
  currentAgent: "main",
  agents: {
    main: {
      messageSource: { mode: "new-chat" } as Source,
      connectionPhase: "idle" as string,
      pendingMessage: null as unknown,
      voiceTurnPending: false,
    },
  },
  sendMessage: vi.fn(),
  queueMessage: vi.fn(),
  clearPending: vi.fn(),
  stopStream: vi.fn(),
  setVoiceTurnPending: vi.fn((pending: boolean) => {
    chatState.agents.main.voiceTurnPending = pending;
  }),
};

vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector?: (s: typeof chatState) => unknown) => (selector ? selector(chatState) : chatState),
    { getState: () => chatState },
  ),
  isActivePhase: (p?: string) => p === "streaming" || p === "submitted" || p === "reconnecting",
}));

vi.mock("../../hooks/use-voice-recorder", () => ({
  useVoiceRecorder: () => ({ state: "idle", start: vi.fn(), stop: vi.fn(), elapsed: 0, level: 0 }),
}));

// ── Fake <audio> element: play() resolves and fires `ended` so the speaker
//    pump advances to the next sentence. Records every distinct src played. ──
const playedSrcs: string[] = [];
class FakeAudio {
  private listeners: Record<string, Set<() => void>> = {};
  private _src = "";
  set src(v: string) { this._src = v; }
  get src() { return this._src; }
  addEventListener(ev: string, fn: () => void) { (this.listeners[ev] ??= new Set()).add(fn); }
  removeEventListener(ev: string, fn: () => void) { this.listeners[ev]?.delete(fn); }
  removeAttribute() { this._src = ""; }
  pause() {}
  play() {
    playedSrcs.push(this._src);
    // Resolve the play promise, then fire `ended` on the next microtask so the
    // speaker's playBlob promise resolves and the pump moves on.
    Promise.resolve().then(() => this.listeners["ended"]?.forEach((f) => f()));
    return Promise.resolve();
  }
}

import { ChatComposer } from "../ChatComposer";

const liveSource = (assistantParts: Part[]): Source => ({
  mode: "live",
  messages: [
    { id: "u1", role: "user", parts: [{ type: "text", text: "скажи что-нибудь" }] },
    { id: "a1", role: "assistant", parts: assistantParts },
  ],
});

let fetchMock: ReturnType<typeof vi.fn>;

describe("ChatComposer streaming TTS speaker (Task 6)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    playedSrcs.length = 0;
    chatState.agents.main.messageSource = { mode: "new-chat" };
    chatState.agents.main.connectionPhase = "idle";
    chatState.agents.main.voiceTurnPending = false;

    // @ts-expect-error jsdom lacks Audio
    global.Audio = FakeAudio;
    let n = 0;
    global.URL.createObjectURL = vi.fn(() => `blob:fake-${n++}`);
    global.URL.revokeObjectURL = vi.fn();

    fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      // Both /api/tts/synthesize and the agent-audio URL resolve to a blob.
      return {
        ok: true,
        status: 200,
        blob: async () => new Blob([new Uint8Array([1, 2, 3])], { type: "audio/mpeg" }),
        _url: url,
      } as unknown as Response;
    });
    global.fetch = fetchMock as unknown as typeof fetch;
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("synthesises the assistant's reply sentence-by-sentence for a voice turn", async () => {
    // Voice turn already streaming, assistant reply carries two complete
    // sentences (each ≥ minLen, trailing whitespace makes both emit on push).
    chatState.agents.main.voiceTurnPending = true;
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.messageSource = liveSource([
      { type: "text", text: "Привет, как твои дела сегодня? Всё отлично, спасибо большое! " },
    ]);

    render(<ChatComposer />);

    // Two sentences → two POSTs to /api/tts/synthesize, and two playbacks.
    await waitFor(() => {
      const synthCalls = fetchMock.mock.calls.filter((c) =>
        String(c[0]).startsWith("/api/tts/synthesize"),
      );
      expect(synthCalls.length).toBe(2);
    });

    const synthCalls = fetchMock.mock.calls.filter((c) =>
      String(c[0]).startsWith("/api/tts/synthesize"),
    );
    // URL carries the current agent.
    expect(String(synthCalls[0][0])).toContain("agent=main");
    // POST with a JSON {text} body + bearer auth.
    const init = synthCalls[0][1] as RequestInit;
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toHaveProperty("text");

    // Playback happened on the single <audio> element.
    await waitFor(() => expect(playedSrcs.length).toBeGreaterThanOrEqual(1));
  });

  it("takes over with the agent's own audio part (no per-sentence synth)", async () => {
    // A voice turn whose reply is a synthesize_speech audio part (no text).
    chatState.agents.main.voiceTurnPending = true;
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.messageSource = liveSource([
      { type: "file", url: "/api/uploads/abc?sig=x&exp=1", mediaType: "audio/mpeg" },
    ]);

    render(<ChatComposer />);

    // The agent-audio URL is fetched and played…
    await waitFor(() => {
      const audioFetches = fetchMock.mock.calls.filter((c) =>
        String(c[0]).includes("/api/uploads/abc"),
      );
      expect(audioFetches.length).toBe(1);
    });
    await waitFor(() => expect(playedSrcs.length).toBeGreaterThanOrEqual(1));

    // …and no per-sentence TTS synthesis was performed (there is no text).
    const synthCalls = fetchMock.mock.calls.filter((c) =>
      String(c[0]).startsWith("/api/tts/synthesize"),
    );
    expect(synthCalls.length).toBe(0);
  });

  it("does NOT synthesise for a non-voice turn", async () => {
    chatState.agents.main.voiceTurnPending = false;
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.messageSource = liveSource([
      { type: "text", text: "Обычный ответ без голоса, просто текст на экране. " },
    ]);

    render(<ChatComposer />);

    // Give effects + any microtasks a chance to run.
    await new Promise((r) => setTimeout(r, 20));

    const synthCalls = fetchMock.mock.calls.filter((c) =>
      String(c[0]).startsWith("/api/tts/synthesize"),
    );
    expect(synthCalls.length).toBe(0);
    expect(playedSrcs.length).toBe(0);
  });

  // ── Fix 1: Stop must NOT voice the trailing partial sentence ──────────────
  it("Stop during a voice turn does not voice the trailing partial sentence", async () => {
    // One COMPLETE sentence (spoken) + a trailing partial with no boundary (held
    // in the splitter accumulator, not yet emitted).
    chatState.agents.main.voiceTurnPending = true;
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.messageSource = liveSource([
      { type: "text", text: "Первая мысль полностью произнесена вслух. А это незаконченный хвост" },
    ]);

    const { rerender } = render(<ChatComposer />);

    // The one complete sentence is synthesised; the partial hangs in the splitter.
    await waitFor(() => {
      const calls = fetchMock.mock.calls.filter((c) =>
        String(c[0]).startsWith("/api/tts/synthesize"),
      );
      expect(calls.length).toBe(1);
    });
    const before = fetchMock.mock.calls.filter((c) =>
      String(c[0]).startsWith("/api/tts/synthesize"),
    ).length;

    // Press Stop (the square button rendered while streaming) → silenceVoiceTurn:
    // cancel speaker, clear voiceTurnPending, reset feed cursor + splitter.
    fireEvent.click(screen.getByLabelText("chat.stop_and_keep"));
    // stopStream→abortLocalOnly would set the phase idle — simulate the falling
    // edge so the effect that (previously) flushed the trailing partial runs.
    chatState.agents.main.connectionPhase = "idle";
    rerender(<ChatComposer />);

    // Let effects + microtasks settle: nothing more must be synthesised/played.
    await new Promise((r) => setTimeout(r, 20));

    const after = fetchMock.mock.calls.filter((c) =>
      String(c[0]).startsWith("/api/tts/synthesize"),
    ).length;
    expect(after).toBe(before); // NO trailing synth after Stop
    // The voice turn was cleanly ended (falling-edge gate now skips flush).
    expect(chatState.setVoiceTurnPending).toHaveBeenCalledWith(false, "main");
  });

  // ── Fix 2: upstream "(Empty response: …)" garbage must NOT be voiced ───────
  it("does NOT voice an upstream (Empty response: …) garbage turn", async () => {
    chatState.agents.main.voiceTurnPending = true;
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.messageSource = liveSource([
      { type: "text", text: "(Empty response: {'content': [...]})" },
    ]);

    const { rerender } = render(<ChatComposer />);
    await new Promise((r) => setTimeout(r, 20));
    // Falling edge (turn end) must not flush the garbage either.
    chatState.agents.main.connectionPhase = "idle";
    rerender(<ChatComposer />);
    await new Promise((r) => setTimeout(r, 20));

    const synthCalls = fetchMock.mock.calls.filter((c) =>
      String(c[0]).startsWith("/api/tts/synthesize"),
    );
    expect(synthCalls.length).toBe(0);
    expect(playedSrcs.length).toBe(0);
  });

  it("voices a normal reply that only CONTAINS (Empty response: mid-string", async () => {
    // Leading real text → start-only match returns false → the reply IS voiced.
    chatState.agents.main.voiceTurnPending = true;
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.messageSource = liveSource([
      { type: "text", text: "Смотри, вот пример: см. (Empty response:) в середине строки. " },
    ]);

    render(<ChatComposer />);

    await waitFor(() => {
      const synthCalls = fetchMock.mock.calls.filter((c) =>
        String(c[0]).startsWith("/api/tts/synthesize"),
      );
      expect(synthCalls.length).toBeGreaterThanOrEqual(1);
    });
  });
});
