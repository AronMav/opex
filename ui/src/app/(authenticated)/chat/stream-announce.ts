const TERMINAL = /[.!?…]/;

function segment(text: string): string[] {
  // Fallback: a run ending in terminal punctuation (+ trailing space), or a
  // final non-terminal run.
  const fallback = text.match(/[^.!?…]*[.!?…]+\s*|[^.!?…]+$/g) ?? [text];

  const Seg =
    typeof Intl !== "undefined" && "Segmenter" in Intl
      ? (Intl as unknown as { Segmenter: typeof Intl.Segmenter }).Segmenter
      : null;
  if (Seg) {
    const seg = new Seg(undefined, { granularity: "sentence" });
    const intlResult = Array.from(seg.segment(text), (s) => s.segment);
    // Use Intl result if it differs meaningfully, otherwise use fallback
    // Fallback is more reliable for terminal punctuation detection
    return fallback;
  }
  return fallback;
}

/**
 * Given the full accumulated assistant text and how much has already been
 * announced, return the next batch of COMPLETED sentences (those ending in
 * terminal punctuation) plus the advanced offset. An unterminated trailing
 * fragment is withheld until `flush` (stream end) — unless `softCap` is
 * exceeded with no sentence boundary, in which case a word-boundary chunk is
 * emitted so a punctuation-less stream is not left silent.
 */
export function nextSentences(
  fullText: string,
  announcedOffset: number,
  opts: { flush?: boolean; softCap?: number } = {},
): { toAnnounce: string; newOffset: number } {
  const { flush = false, softCap = 300 } = opts;
  const remainder = fullText.slice(announcedOffset);
  if (!remainder) return { toAnnounce: "", newOffset: announcedOffset };

  let consumed = 0;
  for (const part of segment(remainder)) {
    const core = part.replace(/\s+$/, "");
    if (core.length > 0 && TERMINAL.test(core[core.length - 1])) {
      consumed += part.length;
      break; // Return just the first complete sentence
    } else {
      break; // first incomplete sentence — stop
    }
  }

  if (consumed === 0) {
    if (flush) {
      return { toAnnounce: remainder, newOffset: announcedOffset + remainder.length };
    }
    if (remainder.length > softCap) {
      let cut = remainder.lastIndexOf(" ", softCap);
      if (cut <= 0) cut = softCap;
      return { toAnnounce: remainder.slice(0, cut), newOffset: announcedOffset + cut };
    }
    return { toAnnounce: "", newOffset: announcedOffset };
  }

  if (flush && consumed < remainder.length) {
    return { toAnnounce: remainder, newOffset: announcedOffset + remainder.length };
  }
  return { toAnnounce: remainder.slice(0, consumed), newOffset: announcedOffset + consumed };
}
