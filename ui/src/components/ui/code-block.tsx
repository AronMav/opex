"use client"

import { cn } from "@/lib/utils"
import { Check, Copy } from "lucide-react"
import { useTheme } from "next-themes"
import React, { useCallback, useEffect, useRef, useState } from "react"
import DOMPurify from "dompurify"
import { copyText } from "@/lib/clipboard"
import { useTranslation } from "@/hooks/use-translation"

// ── Code Block Header Bar ───────────────────────────────────────────────────

function CodeBlockHeader({ language, code }: { language?: string; code: string }) {
  const [copied, setCopied] = useState(false)
  const { t } = useTranslation()

  const handleCopy = useCallback(async () => {
    try {
      await copyText(code)
    } catch {
      // Both clipboard API and execCommand fallback failed — silent fail
      // (legacy behaviour; existing consumers already swallowed this).
    }
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }, [code])

  return (
    <div className="flex items-center justify-between px-4 py-1.5 border-b border-border/50 bg-muted/30 text-xs text-muted-foreground">
      <span className="font-mono rounded bg-muted/50 px-1.5 py-0.5">{language || "text"}</span>
      <button
        onClick={handleCopy}
        aria-label={copied ? t("common.copied") : t("common.copy_code")}
        className="flex items-center gap-1 rounded px-2 py-1.5 md:px-1.5 md:py-0.5 min-h-[44px] md:min-h-0 hover:bg-muted/50 hover:text-foreground transition-colors"
      >
        {copied ? (
          <Check className="h-3.5 w-3.5 text-green-500" />
        ) : (
          <><Copy className="h-3.5 w-3.5" /> <span>{t("common.copy")}</span></>
        )}
      </button>
    </div>
  )
}

// ── Code Block Wrapper ──────────────────────────────────────────────────────

export type CodeBlockProps = {
  children?: React.ReactNode
  className?: string
  language?: string
  showLineNumbers?: boolean
  isStreaming?: boolean
} & React.HTMLProps<HTMLDivElement>

function CodeBlock({ children, className, language, isStreaming = false, ...props }: CodeBlockProps) {
  const codeRef = useRef<HTMLDivElement>(null)
  const [codeText, setCodeText] = useState("")

  useEffect(() => {
    if (isStreaming) return // Skip during streaming — no copy button needed for partial code
    if (codeRef.current) {
      setCodeText(codeRef.current.textContent || "")
    }
  }, [isStreaming, children])

  return (
    <div
      className={cn(
        "group/codeblock relative not-prose flex w-full flex-col overflow-clip border",
        "border-border bg-card text-card-foreground rounded-xl",
        className
      )}
      {...props}
    >
      {codeText && !isStreaming && <CodeBlockHeader language={language} code={codeText} />}
      <div ref={codeRef}>{children}</div>
    </div>
  )
}

// ── Syntax-highlighted Code ─────────────────────────────────────────────────

export type CodeBlockCodeProps = {
  code: string
  language?: string
  theme?: string
  showLineNumbers?: boolean
  className?: string
  isStreaming?: boolean
} & React.HTMLProps<HTMLDivElement>

function CodeBlockCode({
  code,
  language = "tsx",
  theme: themeProp,
  showLineNumbers,
  className,
  isStreaming = false,
  ...props
}: CodeBlockCodeProps) {
  const { resolvedTheme } = useTheme()
  const theme = themeProp ?? (resolvedTheme === "dark" ? "github-dark" : "github-light")
  const [highlightedHtml, setHighlightedHtml] = useState<string | null>(null)

  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    if (!code) {
      setHighlightedHtml("<pre><code></code></pre>")
      return
    }

    // When streaming (fence unclosed): cancel any pending timer, show plain pre
    if (isStreaming) {
      if (debounceRef.current) clearTimeout(debounceRef.current)
      setHighlightedHtml(null) // null → renders existing plain <pre><code> fallback
      return
    }

    // Not streaming: run 150ms debounce + Shiki (unchanged behavior)
    if (debounceRef.current) clearTimeout(debounceRef.current)
    debounceRef.current = setTimeout(async () => {
      try {
        const { codeToHtml } = await import("shiki")
        const html = await codeToHtml(code, { lang: language, theme })
        // Content is sanitized with DOMPurify before rendering
        setHighlightedHtml(DOMPurify.sanitize(html))
      } catch {
        setHighlightedHtml("")
      }
    }, 150)

    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current)
    }
  }, [code, language, theme, isStreaming])

  const classNames = cn(
    "w-full overflow-x-auto text-[13px] [&>pre]:px-4 [&>pre]:py-4 [&>pre]:bg-transparent",
    showLineNumbers && "code-line-numbers",
    className
  )

  // SSR fallback: render plain code if not hydrated yet
  // Note: highlightedHtml is sanitized via DOMPurify.sanitize() above
  return highlightedHtml ? (
    <div
      className={classNames}
      dangerouslySetInnerHTML={{ __html: highlightedHtml }}
      {...props}
    />
  ) : (
    <div className={classNames} {...props}>
      <pre>
        <code>{code}</code>
      </pre>
    </div>
  )
}

export type CodeBlockGroupProps = React.HTMLAttributes<HTMLDivElement>

function CodeBlockGroup({
  children,
  className,
  ...props
}: CodeBlockGroupProps) {
  return (
    <div
      className={cn("flex items-center justify-between", className)}
      {...props}
    >
      {children}
    </div>
  )
}

export { CodeBlockGroup, CodeBlockCode, CodeBlock }
