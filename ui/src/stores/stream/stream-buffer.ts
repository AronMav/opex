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
   * Move completed text/reasoning from parser into parts[].
   * Call before pushing a tool, file, or rich-card part.
   * Parts pushed here are preserved by snapshot() — NOT filtered by type,
   * fixing the bug where reasoning disappeared after a tool call.
   */
  flushText(): void {
    const flushed = this.parser.flush();
    if (flushed.length > 0) this.parts.push(...flushed);
  }

  /**
   * Closes the current text block on `text-end` SSE event: flushes the
   * accumulator into parts and clears insideThink so the next iteration's
   * text-deltas start with a fresh parser state. Without this, parser keeps
   * accumulating across LLM iterations and old text bleeds into new ones.
   */
  endTextBlock(): void {
    const flushed = this.parser.endTextBlock();
    if (flushed.length > 0) this.parts.push(...flushed);
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
