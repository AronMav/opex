import { IncrementalParser } from "@/lib/message-parser";
import { uuid } from "../chat-types";
import type { MessagePart } from "../chat-types";

export class StreamBuffer {
  /** Finalized parts: tools, files, rich-cards, flushed text/reasoning blocks. */
  parts: MessagePart[] = [];

  /** In-progress text accumulator — handles <think> / reasoning blocks. */
  readonly parser: IncrementalParser = new IncrementalParser();

  /** Raw tool input chunks waiting for tool-input-available. */
  readonly toolInputChunks = new Map<string, string[]>();

  /** Which sub-agent is currently producing the response. */
  currentRespondingAgent: string | null;

  /** Message ID for the current LLM iteration. */
  assistantId: string;

  /** Timestamp for the current LLM iteration. */
  assistantCreatedAt: string;

  constructor(initialAgent: string | null) {
    this.currentRespondingAgent = initialAgent;
    this.assistantId = uuid();
    this.assistantCreatedAt = new Date().toISOString();
  }

  /**
   * Move completed text/reasoning from parser into parts[] and close the
   * current text scope. Call before pushing a tool, file, or rich-card
   * part — parts pushed here are preserved by snapshot() (NOT filtered by
   * type, fixing the bug where reasoning disappeared after a tool call).
   *
   * Uses parser.endTextBlock() (not plain flush) so insideThink is reset.
   * Without that reset an unclosed <thinking> tag would leak into the
   * next text block and route it into reasoning by mistake.
   */
  flushText(): void {
    const flushed = this.parser.endTextBlock();
    if (flushed.length > 0) this.parts.push(...flushed);
  }

  /**
   * Closes the current text block on `text-end` SSE event. Identical to
   * flushText() now (both go through parser.endTextBlock); kept as a named
   * alias so the call site in stream-processor reads as semantic intent
   * ("text-end → close text block") rather than implementation detail.
   */
  endTextBlock(): void {
    this.flushText();
  }

  /**
   * Full snapshot of current message content — no side-effects.
   * Returns [...finalizedParts, ...liveParserSnapshot].
   */
  snapshot(): MessagePart[] {
    return [...this.parts, ...this.parser.snapshot()];
  }

  /**
   * Begin a new LLM iteration (called on `start` event).
   * Resets parts, parser, toolInputChunks, generates new assistantId/createdAt.
   * Preserves currentRespondingAgent — caller overrides if agentName is in event.
   */
  reset(): void {
    this.parts = [];
    this.parser.reset();
    this.toolInputChunks.clear();
    this.assistantId = uuid();
    this.assistantCreatedAt = new Date().toISOString();
  }
}
