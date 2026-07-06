"use client"

import { createContext, useContext } from "react"
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip"

// ── Footnote Extraction ───────────────────────────────────────────────────

/**
 * Extract footnote definitions from raw markdown.
 * Returns a Map of footnote id -> definition text.
 */
export function extractFootnotes(markdown: string): Map<string, string> {
  const re = /^\[\^([^\]]+)\]:\s*(.+)$/gm
  const map = new Map<string, string>()
  let match: RegExpExecArray | null
  while ((match = re.exec(markdown)) !== null) {
    map.set(match[1], match[2])
  }
  return map
}

// ── Footnote Context ──────────────────────────────────────────────────────

export const FootnoteContext = createContext<Map<string, string>>(new Map())

export function FootnoteProvider({
  footnotes,
  children,
}: {
  footnotes: Map<string, string>
  children: React.ReactNode
}) {
  return (
    <FootnoteContext.Provider value={footnotes}>
      {children}
    </FootnoteContext.Provider>
  )
}

// ── CitationRef ───────────────────────────────────────────────────────────

/**
 * Renders a footnote reference [^N] as a hoverable superscript with tooltip.
 * Reads footnote definition text from FootnoteContext.
 */
export function CitationRef({
  children,
  href,
  ...props
}: {
  children: React.ReactNode
  href?: string
  id?: string
  "data-footnote-ref"?: boolean
}) {
  const footnotes = useContext(FootnoteContext)

  // Extract footnote key from href like "#user-content-fn-1" -> "1"
  const fnKey = href?.replace(/^#user-content-fn-/, "") ?? ""
  const footnoteText = footnotes.get(fnKey)

  if (!footnoteText) {
    // Graceful degradation: just render superscript without tooltip
    return (
      <sup className="cursor-help">
        <span
          className="text-primary/70 text-xs font-semibold no-underline hover:text-primary cursor-help"
          {...props}
        >
          {children}
        </span>
      </sup>
    )
  }

  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <sup className="cursor-help">
            <span
              className="text-primary/70 text-xs font-semibold no-underline hover:text-primary cursor-help"
              {...props}
            >
              {children}
            </span>
          </sup>
        </TooltipTrigger>
        <TooltipContent side="top" className="max-w-xs text-sm">
          {footnoteText}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}

// ── CitationSection ───────────────────────────────────────────────────────

/**
 * Renders the footnote definitions section as visually hidden (sr-only).
 * Screen readers can still access it for accessibility.
 */
export function CitationSection({
  children,
  ...props
}: {
  children: React.ReactNode
  "data-footnotes"?: boolean
  className?: string
}) {
  return (
    <section className="sr-only" aria-label="Footnotes" {...props}>
      {children}
    </section>
  )
}

// ── Component Overrides for react-markdown ────────────────────────────────

// Minimal structural shape of a hast node — avoids importing the full hast
// type tree while keeping the override components fully type-safe.
type HastElement = {
  type?: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastElement[];
};

type OverrideProps = {
  children?: React.ReactNode;
  node?: HastElement;
  id?: string;
  href?: string;
  className?: string;
  [key: string]: unknown;
};

type OverrideComponent = React.ComponentType<OverrideProps>;

/**
 * Returns react-markdown component overrides for footnote elements.
 * - `sup`: wraps footnote refs in CitationRef (checks data-footnote-ref)
 * - `section`: wraps footnote defs in CitationSection (checks data-footnotes)
 */
export function createFootnoteComponents(): Record<string, OverrideComponent> {
  const SupOverride: OverrideComponent = ({ children, node, ...props }: OverrideProps) => {
    // Check if this sup contains a footnote ref anchor
    const firstChild = node?.children?.[0];
    const isFootnoteRef =
      firstChild?.tagName === "a" &&
      firstChild?.properties?.["dataFootnoteRef"] !== undefined;

    if (isFootnoteRef) {
      const anchor = firstChild.properties as { href?: string };
      return (
        <CitationRef
          href={anchor.href}
          id={props.id as string | undefined}
          data-footnote-ref
        >
          {children}
        </CitationRef>
      );
    }

    return <sup {...props}>{children}</sup>;
  };

  const SectionOverride: OverrideComponent = ({ children, node, ...props }: OverrideProps) => {
    const isFootnotes =
      node?.properties?.["dataFootnotes"] !== undefined;

    if (isFootnotes) {
      return (
        <CitationSection data-footnotes>
          {children}
        </CitationSection>
      );
    }

    return <section {...props}>{children}</section>;
  };

  return { sup: SupOverride, section: SectionOverride };
}
