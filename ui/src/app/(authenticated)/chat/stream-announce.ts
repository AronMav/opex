const TERMINAL = /[.!?…]/;

// Split into sentence-like runs: a run ending in terminal punctuation (with any
// trailing whitespace), or a final unterminated run. A regex segmenter is used
// rather than Intl.Segmenter because the latter does not treat "…" as a sentence
// boundary in several locales (e.g. Russian) that this announcer must honour;
// for streaming chunk announcements the regex is sufficient.
function segment(text: string): string[] {
  return text.match(/[^.!?…]*[.!?…]+\s*|[^.!?…]+$/g) ?? [text];
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
