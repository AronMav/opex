"use client"

import { cn } from "@/lib/utils"
import { marked } from "marked"
import { memo, useEffect, useId, useMemo, useState } from "react"
import type { Pluggable } from "unified"
import ReactMarkdown, { Components } from "react-markdown"
import remarkBreaks from "remark-breaks"
import remarkGfm from "remark-gfm"
import { CodeBlock, CodeBlockCode } from "./code-block"
import { extractFootnotes, FootnoteProvider, createFootnoteComponents } from "./citation-tooltip"
import { MermaidBlock } from "./mermaid-block"

// ── Math Detection ─────────────────────────────────────────────────────────

// Detect math content: $$...$$, \(...\), \[...\], and inline $...$ only when
// the content between $ signs contains at least one LaTeX operator (\, ^, _, {, })
// to avoid false positives on currency ($100) or Cyrillic text near $ signs.
const DISPLAY_MATH = /\$\$[\s\S]+?\$\$|\\[([]\s*[\s\S]*?\s*\\[\])]/
const INLINE_MATH = /\$[^\s$].*?[^\s$]\$/

function hasMathContent(content: string): boolean {
  if (DISPLAY_MATH.test(content)) return true
  const m = content.match(INLINE_MATH)
  if (!m) return false
  // Only treat inline $...$ as math if it contains a LaTeX operator
  const inner = m[0].slice(1, -1)
  return /[\\^_{}]/.test(inner)
}

// ── Block Key & Fence Detection ────────────────────────────────────────────

/** Fast djb2 hash of first 32 chars — sub-microsecond, no import needed */
export function blockKey(blockId: string, index: number, content: string): string {
  let hash = 5381
  const len = Math.min(content.length, 32)
  for (let i = 0; i < len; i++) {
    hash = ((hash << 5) + hash) ^ content.charCodeAt(i)
  }
  return `${blockId}-${index}-${(hash >>> 0).toString(36)}`
}

/**
 * Returns true if `raw` is an unclosed code fence (streaming partial block).
 * marked.lexer() emits `code` tokens even for unclosed fences — raw does NOT
 * end with closing backtick fence in that case.
 */
export function isUnclosedCodeBlock(raw: string): boolean {
  const trimmed = raw.trimEnd()
  return trimmed.startsWith('```') && !trimmed.endsWith('```')
}

// ── Types & Helpers ────────────────────────────────────────────────────────

export type MarkdownProps = {
  children: string
  id?: string
  className?: string
  components?: Partial<Components>
  isStreaming?: boolean
}

function parseMarkdownIntoBlocks(markdown: string): string[] {
  const tokens = marked.lexer(markdown)
  return tokens.map((token) => token.raw)
}

// File-handler results are persisted provenance-wrapped
// (`<file_output handler=… trust="untrusted">TEXT</file_output>`) so the LLM
// treats them as untrusted on the next turn. That wrapper is machine-only — strip
// the tags for human display, keeping the inner TEXT. Display-only: the raw
// content (with wrapper) stays in the DB for the model.
function stripProvenanceWrapper(s: string): string {
  if (!s.includes("<file_output")) return s
  return s.replace(/<\/?file_output\b[^>]*>/g, "").replace(/^\n+/, "")
}

function extractLanguage(className?: string): string {
  if (!className) return "plaintext"
  const match = className.match(/language-(\w+)/)
  return match ? match[1] : "plaintext"
}

// ── Components Factory ─────────────────────────────────────────────────────

function createComponents(isStreamingCode = false): Partial<Components> {
  return {
    code: function CodeComponent({ className, children, ...props }) {
      const isInline =
        !props.node?.position?.start.line ||
        props.node?.position?.start.line === props.node?.position?.end.line

      if (isInline) {
        return (
          <span
            className={cn(
              "bg-muted rounded-sm px-1 font-mono text-sm",
              className
            )}
            {...props}
          >
            {children}
          </span>
        )
      }

      const language = extractLanguage(className)

      if (language === "mermaid") {
        return <MermaidBlock code={String(children).trim()} />
      }

      const codeStr = children as string
      const lineCount = codeStr ? codeStr.split('\n').length : 0

      return (
        <CodeBlock className={className} language={language}>
          <CodeBlockCode
            code={codeStr}
            language={language}
            showLineNumbers={lineCount > 10}
            isStreaming={isStreamingCode}
          />
        </CodeBlock>
      )
    },
    pre: function PreComponent({ children }) {
      return <>{children}</>
    },
    table: function TableComponent({ children }) {
      return (
        <div className="overflow-x-auto my-2">
          <table className="w-full border-collapse">{children}</table>
        </div>
      )
    },
    img: function ImgComponent({ src, alt, ...props }) {
      return <img src={src} alt={alt} className="max-w-full h-auto rounded-lg" {...props} />
    },
  }
}

// Stable references for the common cases — avoids object recreation on each render
const FOOTNOTE_COMPONENTS = createFootnoteComponents()
const INITIAL_COMPONENTS = { ...createComponents(false), ...FOOTNOTE_COMPONENTS }
const STREAMING_COMPONENTS = { ...createComponents(true), ...FOOTNOTE_COMPONENTS }

// ── Standard Markdown Block (no math) ──────────────────────────────────────

const MemoizedMarkdownBlock = memo(
  function MarkdownBlock({
    content,
    isStreamingCode = false,
    components = INITIAL_COMPONENTS,
  }: {
    content: string
    isStreamingCode?: boolean
    components?: Partial<Components>
  }) {
    return (
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks]}
        components={components}
      >
        {content}
      </ReactMarkdown>
    )
  },
  function propsAreEqual(prevProps, nextProps) {
    return prevProps.content === nextProps.content
      && prevProps.isStreamingCode === nextProps.isStreamingCode
  }
)

MemoizedMarkdownBlock.displayName = "MemoizedMarkdownBlock"

// ── Math-aware Markdown Block (KaTeX loaded on demand) ─────────────────────

const MemoizedMathBlock = memo(
  function MathBlock({
    content,
    components = INITIAL_COMPONENTS,
  }: {
    content: string
    components?: Partial<Components>
  }) {
    const [mathPlugins, setMathPlugins] = useState<{ remarkMath: Pluggable; rehypeKatex: Pluggable } | null>(null)

    useEffect(() => {
      let cancelled = false
      ;(async () => {
        const [rm, rk] = await Promise.all([
          import("remark-math"),
          import("rehype-katex"),
        ])
        // Load KaTeX CSS on demand (webpack handles CSS dynamic imports at build time)
        // @ts-expect-error -- CSS import has no type declarations but webpack bundles it correctly
        await import("katex/dist/katex.min.css")
        if (!cancelled) setMathPlugins({ remarkMath: rm.default, rehypeKatex: rk.default })
      })()
      return () => { cancelled = true }
    }, [])

    if (!mathPlugins) {
      // Render without math until plugins load (plain text fallback)
      return (
        <ReactMarkdown
          remarkPlugins={[remarkGfm, remarkBreaks]}
          components={components}
        >
          {content}
        </ReactMarkdown>
      )
    }

    return (
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks, mathPlugins.remarkMath]}
        rehypePlugins={[mathPlugins.rehypeKatex]}
        components={components}
      >
        {content}
      </ReactMarkdown>
    )
  },
  function propsAreEqual(prevProps, nextProps) {
    return prevProps.content === nextProps.content
  }
)

MemoizedMathBlock.displayName = "MemoizedMathBlock"

// ── Main Markdown Component ────────────────────────────────────────────────

function MarkdownComponent({
  children,
  id,
  className,
  isStreaming = false,
  components,
}: MarkdownProps) {
  const generatedId = useId()
  const blockId = id ?? generatedId
  const cleaned = useMemo(() => stripProvenanceWrapper(children), [children])
  const blocks = useMemo(() => parseMarkdownIntoBlocks(cleaned), [cleaned])
  const footnoteMap = useMemo(() => extractFootnotes(cleaned), [cleaned])

  const content = (
    <div className={className}>
      {blocks.map((block, index) => {
        const isLastBlock = index === blocks.length - 1
        // Fence detection is authoritative — unclosed fence = streaming code regardless of isStreaming flag.
        // isStreaming is used by CodeBlockCode to decide whether to skip Shiki.
        // Here we only check fence state to determine which components object to pass.
        const isStreamingCode = isLastBlock && isUnclosedCodeBlock(block)

        // Use stable INITIAL_COMPONENTS for non-streaming blocks.
        // Use STREAMING_COMPONENTS only for the last block with an unclosed fence.
        // If caller provides custom components, always use those.
        const resolvedComponents = components ?? (isStreamingCode ? STREAMING_COMPONENTS : INITIAL_COMPONENTS)

        return hasMathContent(block) ? (
          <MemoizedMathBlock
            key={blockKey(blockId, index, block)}
            content={block}
            components={resolvedComponents}
          />
        ) : (
          <MemoizedMarkdownBlock
            key={blockKey(blockId, index, block)}
            content={block}
            isStreamingCode={isStreamingCode}
            components={resolvedComponents}
          />
        )
      })}
    </div>
  )

  return footnoteMap.size > 0
    ? <FootnoteProvider footnotes={footnoteMap}>{content}</FootnoteProvider>
    : content
}

const Markdown = memo(MarkdownComponent)
Markdown.displayName = "Markdown"

export { Markdown }
