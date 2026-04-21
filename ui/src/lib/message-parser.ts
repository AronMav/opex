/**
 * Unified message parsing logic for HydeClaw.
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

/**
 * State-aware incremental parser for SSE streams.
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
    
    // We process the accumulator to find tags
    let remaining = this.accum;
    const thinkTags = ["think", "thinking", "thought", "antthinking"];
    
    while (remaining) {
      if (this.insideThink) {
        // Look for any closing tag
        let closestEndIdx = -1;
        let tagLen = 0;
        
        for (const tag of thinkTags) {
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
          this.accum = remaining; // update accum to what's left
        } else {
          // No end tag yet, but we can emit partial reasoning if it's long enough
          // or if we're sure it's not a partial closing tag.
          // To be safe, we keep at least 15 chars in accum to avoid splitting </think>
          if (remaining.length > 15) {
            const safeToEmit = remaining.slice(0, remaining.length - 15);
            this.appendToLast("reasoning", safeToEmit);
            remaining = remaining.slice(remaining.length - 15);
            this.accum = remaining;
          }
          break;
        }
      } else {
        // Look for any opening tag
        let closestStartIdx = -1;
        let tagLen = 0;
        
        for (const tag of thinkTags) {
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
          // No start tag, emit plain text safely
          if (remaining.length > 15) {
            const safeToEmit = remaining.slice(0, remaining.length - 15);
            this.appendToLast("text", safeToEmit);
            remaining = remaining.slice(remaining.length - 15);
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
