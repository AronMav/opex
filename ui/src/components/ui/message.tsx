"use client"

import { cn } from "@/lib/utils"
import { Markdown } from "./markdown"
import { ErrorBoundary } from "./error-boundary"
import { useTranslation } from "@/hooks/use-translation"

export type MessageContentProps = {
  children: React.ReactNode
  markdown?: boolean
  className?: string
  isStreaming?: boolean
} & React.ComponentProps<typeof Markdown> &
  React.HTMLProps<HTMLDivElement>

export function MessageContent({
  children,
  markdown = false,
  className,
  isStreaming,
  components,
  ...props
}: MessageContentProps) {
  const { t } = useTranslation()
  const classNames = cn(
    "rounded-lg p-2 text-foreground bg-secondary prose break-words whitespace-normal [&_a]:break-all",
    className
  )

  return markdown ? (
    <ErrorBoundary fallback={<div className={cn(classNames, "border-destructive text-destructive")}>{t("chat.render_error")}</div>}>
      <Markdown className={classNames} isStreaming={isStreaming} components={components} {...props}>
        {children as string}
      </Markdown>
    </ErrorBoundary>
  ) : (
    <div className={classNames} {...props}>
      {children}
    </div>
  )
}