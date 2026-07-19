"use client";

import React, { memo } from "react";
import { cleanContent } from "@/lib/format";
import { MessageContent } from "@/components/ui/message";
import { StreamingCaret } from "@/components/ui/loader";
import { useSmoothedText } from "@/hooks/use-smoothed-text";

export interface HighlightRange {
  start: number;
  end: number;
}

interface TextPartProps {
  text: string;
  /** Optional ranges to highlight within the raw text (before cleanContent). */
  highlightRanges?: HighlightRange[];
  /** When true, the active match gets a stronger highlight color. */
  isActive?: boolean;
  /**
   * True when THIS part's message is still actively streaming (message.status
   * === "streaming", threaded from MessageItem's renderAllParts). Distinct
   * from the internal isStreaming below (global connectionPhase, used only
   * for the smoothed-text buffering): this one drives the inline
   * StreamingCaret, which must appear only at the end of the message that is
   * actually still receiving deltas.
   */
  streaming?: boolean;
}

/**
 * Render raw text with optional search highlight spans.
 * Highlights are applied to the raw text and wrapped in inline <mark> elements.
 * When highlightRanges is empty/undefined, falls back to the normal smoothed markdown path.
 */
function HighlightedText({ text, ranges, isActive }: { text: string; ranges: HighlightRange[]; isActive?: boolean }) {
  if (ranges.length === 0) return <span>{text}</span>;

  const parts: React.ReactNode[] = [];
  let pos = 0;
  for (const range of ranges) {
    if (range.start > pos) {
      parts.push(<React.Fragment key={`t-${pos}`}>{text.slice(pos, range.start)}</React.Fragment>);
    }
    parts.push(
      <mark
        key={`h-${range.start}`}
        className={isActive ? "bg-highlight-active text-foreground rounded-sm" : "bg-highlight text-foreground rounded-sm"}
      >
        {text.slice(range.start, range.end)}
      </mark>,
    );
    pos = range.end;
  }
  if (pos < text.length) {
    parts.push(<React.Fragment key={`t-end`}>{text.slice(pos)}</React.Fragment>);
  }
  return <span>{parts}</span>;
}

export const TextPart = memo(function TextPart({ text, highlightRanges, isActive, streaming }: TextPartProps) {
  // H3 fix: drive the smoothed-text path off the PER-MESSAGE `streaming` prop
  // (threaded from MessageItem, which checks message.status === "streaming")
  // instead of the GLOBAL `connectionPhase` subscription. The global
  // subscription caused every TextPart instance in the tree (including
  // completed historical messages) to re-render on every phase transition —
  // 200–400 extra re-renders per turn on a typical session. Historical
  // messages never have `streaming=true`, so they short-circuit cheaply
  // (useSmoothedText returns the text directly when `isStreaming=false`).
  const isStreaming = !!streaming;

  // Hooks must run before any early return: toggling highlightRanges on/off
  // (search ↔ clear-search) otherwise changes the hook count between renders
  // and React crashes with "Rendered {more,fewer} hooks than expected".
  // When isStreaming is false (the highlight/search context) useSmoothedText
  // returns the text immediately with no timer, so this is side-effect-free.
  const cleaned = cleanContent(text);
  const smoothed = useSmoothedText(cleaned, isStreaming);

  // When highlight ranges are provided, render plain text with inline marks.
  // Skip cleanContent + markdown rendering to avoid losing character offsets.
  if (highlightRanges && highlightRanges.length > 0) {
    return (
      <p className="leading-relaxed text-foreground text-message whitespace-pre-wrap">
        <HighlightedText text={text} ranges={highlightRanges} isActive={isActive} />
      </p>
    );
  }

  if (!smoothed) return null;
  return (
    <>
      <MessageContent
        markdown
        isStreaming={isStreaming}
        className="prose prose-sm dark:prose-invert max-w-none bg-transparent p-0 overflow-x-auto
          [&_p]:leading-relaxed [&_p]:text-foreground [&_p]:text-message
          [&_pre]:my-4 [&_pre]:border [&_pre]:border-border [&_pre]:bg-muted/50 [&_pre]:shadow-inner [&_pre]:rounded-lg
          [&_table]:block [&_table]:overflow-x-auto [&_table]:w-full
          [&_a]:text-primary [&_a]:font-bold [&_a]:no-underline hover:[&_a]:underline
          [&_li]:text-foreground [&_strong]:text-foreground [&_strong]:font-bold"
      >
        {smoothed}
      </MessageContent>
      {streaming && <StreamingCaret />}
    </>
  );
});
