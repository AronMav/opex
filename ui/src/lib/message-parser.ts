/**
 * Unified message parsing logic for OPEX.
 * Handles <think> blocks, cleaning artifacts, and merging consecutive parts.
 */

export interface TextPart {
  type: "text";
  text: string;
}

export interface ReasoningPart {
  type: "reasoning";
  text: string;
}

export type ParsedContentPart = TextPart | ReasoningPart;

/**
 * Normalizes a list of parts by merging consecutive parts of the same type
 * and trimming whitespace where appropriate.
 */
export function normalizeParts(parts: ParsedContentPart[]): ParsedContentPart[] {
  const result: ParsedContentPart[] = [];
  for (const part of parts) {
    const last = result[result.length - 1];
    if (last && last.type === part.type) {
      if (part.type === "text") {
        (last as TextPart).text += part.text;
      } else {
        (last as ReasoningPart).text += part.text;
      }
    } else {
      // Clone to avoid mutating original objects
      result.push({ ...part });
    }
  }
  // Filter out empty parts, but preserve significant whitespace in others
  return result.filter(p => p.text.length > 0);
}

/**
 * Cleans technical artifacts from LLM responses (e.g. MiniMax XML tool calls).
 */
export function cleanArtifacts(text: string): string {
  return text
    .replace(/<minimax:tool_call>[\s\S]*?(<\/minimax:tool_call>|$)\s*/g, "")
    .replace(/\[TOOL_CALL\][\s\S]*?(\[\/TOOL_CALL\]|$)\s*/g, "")
    .trim();
}

/**
 * Parses full content (history) into structured parts.
 */
export function parseContentParts(raw: string): ParsedContentPart[] {
  if (!raw) return [];
  const parts: ParsedContentPart[] = [];
  
  // Clean MiniMax and other artifacts first
  const cleaned = cleanArtifacts(raw);
  
  const thinkRegex = /<(?:think|thinking|thought|antthinking)>([\s\S]*?)<\/(?:think|thinking|thought|antthinking)>/gi;
  let lastIndex = 0;
  let match;

  while ((match = thinkRegex.exec(cleaned)) !== null) {
    const before = cleaned.slice(lastIndex, match.index);
    if (before) parts.push({ type: "text", text: before });
    
    const reasoning = match[1];
    if (reasoning) parts.push({ type: "reasoning", text: reasoning });
    
    lastIndex = match.index + match[0].length;
  }

  // Handle remaining text
  const after = cleaned.slice(lastIndex);
  if (after) {
    // Check for unclosed <think> tag at the very end
    const unclosedMatch = after.match(/<(think|thinking|thought|antthinking)>([\s\S]*)$/i);
    if (unclosedMatch) {
      const beforeUnclosed = after.slice(0, unclosedMatch.index);
      if (beforeUnclosed) parts.push({ type: "text", text: beforeUnclosed });
      
      const unclosedReasoning = unclosedMatch[2];
      if (unclosedReasoning) parts.push({ type: "reasoning", text: unclosedReasoning });
    } else {
      parts.push({ type: "text", text: after });
    }
  }

  return normalizeParts(parts);
}

const THINK_TAGS = ["think", "thinking", "thought", "antthinking"] as const;

/**
 * Returns the length of the longest suffix of `text` that could still grow
 * into a valid `<tag>` opening tag for any tag in THINK_TAGS.
 *
 * Examples (THINK_TAGS includes "think"):
 *   "...hello<th"       → 3   (could become "<think>")
 *   "...hello<thinki"   → 6   (could become "<thinking>")
 *   "...hello<"         → 1   (could be the start of any tag)
 *   "...hello"          → 0   (no partial tag)
 *
 * This replaces a previous magic-number holdback (15 chars) which both wasted
 * memory on text without partial tags and risked under-buffering for the
 * longest tag (`</antthinking>` is 14 chars). Tag matching is case-insensitive
 * to match the parser's regex behavior.
 */
function partialTagSuffixLen(text: string, isClosing: boolean): number {
  if (!text) return 0;
  const lower = text.toLowerCase();
  let max = 0;
  for (const tag of THINK_TAGS) {
    const full = isClosing ? `</${tag}>` : `<${tag}>`;
    const maxCheck = Math.min(full.length - 1, text.length);
    for (let len = maxCheck; len > max; len--) {
      if (lower.endsWith(full.slice(0, len).toLowerCase())) {
        max = len;
        break;
      }
    }
  }
  return max;
}

/**
 * State-aware incremental parser for SSE streams.
 *
 * Streams may split a tag like `</think>` across chunk boundaries. We never
 * emit the trailing chars that could still become a tag — see partialTagSuffixLen.
 * Once a chunk arrives whose suffix can no longer become a tag, the held-back
 * text is released. The buffer also drains on `flush()`/`reset()` and at
 * non-text events upstream (text-end, Finish, ToolCallStart, ...).
 */
export class IncrementalParser {
  private parts: ParsedContentPart[] = [];
  private insideThink = false;
  private accum = "";

  /**
   * Processes a new chunk of text and returns updated parts list.
   */
  processDelta(delta: string): ParsedContentPart[] {
    this.accum += delta;

    let remaining = this.accum;

    while (remaining) {
      if (this.insideThink) {
        // Look for the closest closing tag
        let closestEndIdx = -1;
        let tagLen = 0;

        for (const tag of THINK_TAGS) {
          const idx = remaining.toLowerCase().indexOf(`</${tag}>`);
          if (idx >= 0 && (closestEndIdx === -1 || idx < closestEndIdx)) {
            closestEndIdx = idx;
            tagLen = tag.length + 3; // </tag>
          }
        }

        if (closestEndIdx >= 0) {
          const text = remaining.slice(0, closestEndIdx);
          this.appendToLast("reasoning", text);
          this.insideThink = false;
          remaining = remaining.slice(closestEndIdx + tagLen);
          this.accum = remaining;
        } else {
          // Hold back only the suffix that could still become a closing tag
          const holdback = partialTagSuffixLen(remaining, true);
          if (remaining.length > holdback) {
            const safeToEmit = remaining.slice(0, remaining.length - holdback);
            this.appendToLast("reasoning", safeToEmit);
            remaining = remaining.slice(remaining.length - holdback);
            this.accum = remaining;
          }
          break;
        }
      } else {
        // Look for the closest opening tag
        let closestStartIdx = -1;
        let tagLen = 0;

        for (const tag of THINK_TAGS) {
          const idx = remaining.toLowerCase().indexOf(`<${tag}>`);
          if (idx >= 0 && (closestStartIdx === -1 || idx < closestStartIdx)) {
            closestStartIdx = idx;
            tagLen = tag.length + 2; // <tag>
          }
        }

        if (closestStartIdx >= 0) {
          const before = remaining.slice(0, closestStartIdx);
          if (before) this.appendToLast("text", before);
          this.insideThink = true;
          remaining = remaining.slice(closestStartIdx + tagLen);
          this.accum = remaining;
        } else {
          // Hold back only the suffix that could still become an opening tag
          const holdback = partialTagSuffixLen(remaining, false);
          if (remaining.length > holdback) {
            const safeToEmit = remaining.slice(0, remaining.length - holdback);
            this.appendToLast("text", safeToEmit);
            remaining = remaining.slice(remaining.length - holdback);
            this.accum = remaining;
          }
          break;
        }
      }
    }

    return [...this.parts];
  }

  /**
   * Resets all incremental parser state. Call between agent turns to prevent
   * reasoning context from leaking into the next agent's text output.
   */
  reset(): void {
    this.parts = [];
    this.insideThink = false;
    this.accum = "";
  }

  /**
   * Closes the current text block: flushes any accumulator content into parts
   * and clears the insideThink flag. Used on `text-end` SSE events so the next
   * text block (typically from the next LLM tool-loop iteration) starts fresh
   * — without this, the held-back accum bleeds into the next iteration's text
   * and any open <think> state leaks across the boundary.
   *
   * Returns the parts produced by this flush so callers can append to a buffer.
   */
  endTextBlock(): ParsedContentPart[] {
    const flushed = this.flush();
    this.insideThink = false;
    return flushed;
  }

  /**
   * Flushes remaining accumulator into parts.
   */
  flush(): ParsedContentPart[] {
    if (this.accum) {
      this.appendToLast(this.insideThink ? "reasoning" : "text", this.accum);
      this.accum = "";
    }
    const result = normalizeParts(this.parts);
    // Must clear parts after flush — otherwise the next flush() call re-returns
    // the same parts and StreamBuffer.flushText() pushes them to buffer.parts again,
    // causing text duplication when multiple tool calls occur in one agent turn.
    this.parts = [];
    return result;
  }

  /** Get snapshot of all parts including buffered accum, WITHOUT resetting state */
  snapshot(): ParsedContentPart[] {
    if (!this.accum) return [...this.parts];
    // Temporarily add accum to get complete picture
    const copy = this.parts.map(p => ({ ...p }));
    const last = copy[copy.length - 1];
    const type = this.insideThink ? "reasoning" : "text";
    if (last && last.type === type) {
      last.text += this.accum;
    } else {
      copy.push({ type, text: this.accum } as ParsedContentPart);
    }
    return copy;
  }

  private appendToLast(type: "text" | "reasoning", text: string) {
    const lastIdx = this.parts.length - 1;
    const last = this.parts[lastIdx];
    if (last && last.type === type) {
      // Create new object for immutable update compatibility (Fix IMM-01)
      this.parts[lastIdx] = { ...last, text: last.text + text };
    } else {
      this.parts.push({ type, text });
    }
  }
}
