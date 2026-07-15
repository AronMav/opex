// ── hooks/tts-speaker.ts ─────────────────────────────────────────────────────
// Pure, DOM-free TTS speaking pipeline: SentenceSplitter turns streaming text
// deltas into complete, markdown-cleaned sentences; SpeakerQueue synthesizes
// and plays those sentences strictly in order, with one-ahead prefetch,
// agent-audio takeover, and cancellation. No fetch/Audio/document/window here
// — everything I/O-shaped is injected via `deps` so this is fully unit
// testable, mirroring the vad.ts pure-state-machine pattern.

// ── SentenceSplitter ─────────────────────────────────────────────────────────

/** One or more sentence-ending marks, optional closing quote/bracket, then whitespace. */
const SENTENCE_BOUNDARY_RE = /[.!?…]+["»)\]]*\s/;
const FENCED_CODE_RE = /```[\s\S]*?```/g;
const MD_LINK_RE = /\[([^\]]*)\]\([^)]*\)/g;
const HEADING_RE = /^#{1,6}\s+/;
const LIST_MARKER_RE = /^\s*[-*]\s+/;

/** Strip markdown noise that shouldn't be read aloud by TTS. */
function cleanMarkdown(text: string): string {
  let t = text;
  t = t.replace(FENCED_CODE_RE, "(код)");
  t = t.replace(MD_LINK_RE, "$1");
  t = t.replace(HEADING_RE, "");
  t = t.replace(LIST_MARKER_RE, "");
  return t.trim();
}

export interface SentenceSplitter {
  /** Append a text delta; returns any complete sentences ready to speak. */
  push(delta: string): string[];
  /** Flush the remaining accumulator as a final sentence (or []). */
  flush(): string[];
}

/**
 * Streams text deltas into complete sentences. A sentence boundary is
 * `[.!?…]` (one or more), optionally followed by closing quotes/brackets,
 * then whitespace. A boundary candidate is only emitted once its trimmed
 * length reaches `minLen` (default 20) — shorter fragments keep accumulating
 * across further boundaries until the target length is met.
 */
export function createSentenceSplitter(opts: { minLen?: number } = {}): SentenceSplitter {
  const minLen = opts.minLen ?? 20;
  const boundaryRe = new RegExp(SENTENCE_BOUNDARY_RE.source, "g");
  let acc = "";

  function push(delta: string): string[] {
    acc += delta;
    const results: string[] = [];
    let searchFrom = 0;

    while (searchFrom <= acc.length) {
      boundaryRe.lastIndex = searchFrom;
      const m = boundaryRe.exec(acc);
      if (!m) break;

      const end = m.index + m[0].length;
      const candidate = acc.slice(0, end).trim();
      if (candidate.length >= minLen) {
        results.push(cleanMarkdown(candidate));
        acc = acc.slice(end);
        searchFrom = 0;
      } else {
        // Not long enough yet — keep this boundary's text and look further
        // ahead for the next one, merging fragments until minLen is reached.
        searchFrom = end;
      }
    }

    return results;
  }

  function flush(): string[] {
    const remainder = acc.trim();
    acc = "";
    return remainder ? [cleanMarkdown(remainder)] : [];
  }

  return { push, flush };
}

// ── SpeakerQueue ─────────────────────────────────────────────────────────────

export interface SpeakerDeps {
  /** Synthesize one sentence into audio. `null` (e.g. 409/error) means skip it. */
  synth(sentence: string, signal: AbortSignal): Promise<Blob | null>;
  /** Play one already-synthesized (or takeover) audio blob to completion. */
  play(blob: Blob): Promise<void>;
  onStateChange?(s: "idle" | "speaking"): void;
  onDrain?(): void;
}

export interface SpeakerQueue {
  /** Append a sentence to the playback queue. */
  enqueue(sentence: string): void;
  /** Abort in-flight synths, clear the queue, and play this blob instead. */
  takeoverAudio(blob: Blob): void;
  /** Abort in-flight synths, clear the queue, and stop. */
  cancel(): void;
  /** True when the queue is empty and nothing is playing. */
  readonly idle: boolean;
}

/**
 * Plays sentences strictly in order on a single output. Prefetch=1: while the
 * current sentence plays, the next one's synth may run in parallel (at most
 * one ready-but-not-yet-played blob is held). A sentence whose synth resolves
 * `null` is skipped without interrupting the rest of the queue.
 */
export function createSpeakerQueue(deps: SpeakerDeps): SpeakerQueue {
  const queue: string[] = [];
  const controllers = new Set<AbortController>();

  let epoch = 0;
  let pumping = false;
  let playing = false;
  let state: "idle" | "speaking" = "idle";

  function setState(next: "idle" | "speaking") {
    if (state === next) return;
    state = next;
    deps.onStateChange?.(next);
  }

  function abortAll() {
    for (const c of controllers) c.abort();
    controllers.clear();
  }

  /** Synth one sentence; resolves to null on abort/error, or if superseded. */
  function synthOne(sentence: string, myEpoch: number): Promise<Blob | null> {
    const controller = new AbortController();
    controllers.add(controller);
    return deps
      .synth(sentence, controller.signal)
      .catch(() => null)
      .then((blob) => {
        controllers.delete(controller);
        return myEpoch === epoch ? blob : null;
      });
  }

  async function pump(myEpoch: number) {
    if (pumping) return;
    pumping = true;
    setState("speaking");

    let prefetch: Promise<Blob | null> | null = null;

    while (myEpoch === epoch) {
      let blob: Blob | null;
      if (prefetch) {
        blob = await prefetch;
        prefetch = null;
      } else if (queue.length > 0) {
        blob = await synthOne(queue.shift()!, myEpoch);
      } else {
        break;
      }
      if (myEpoch !== epoch) break;

      // Kick off the next sentence's synth in parallel with this playback.
      if (queue.length > 0) {
        prefetch = synthOne(queue.shift()!, myEpoch);
      }

      if (blob) {
        playing = true;
        try {
          await deps.play(blob);
        } finally {
          playing = false;
        }
      }
      if (myEpoch !== epoch) break;
    }

    if (myEpoch === epoch) {
      pumping = false;
      setState("idle");
      deps.onDrain?.();
    }
  }

  function enqueue(sentence: string) {
    queue.push(sentence);
    if (!pumping) {
      void pump(epoch);
    }
  }

  function stopInternal() {
    epoch += 1;
    queue.length = 0;
    abortAll();
    pumping = false;
  }

  function cancel() {
    stopInternal();
    playing = false;
    setState("idle");
  }

  function takeoverAudio(blob: Blob) {
    stopInternal();
    const myEpoch = epoch;
    playing = true;
    setState("speaking");
    void deps
      .play(blob)
      .catch(() => {})
      .then(() => {
        if (myEpoch !== epoch) return;
        playing = false;
        setState("idle");
      });
  }

  return {
    enqueue,
    takeoverAudio,
    cancel,
    get idle() {
      return queue.length === 0 && !playing && !pumping;
    },
  };
}
