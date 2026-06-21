"use client";

import * as React from "react";
import { RotateCcw, X, AlertCircle, WifiOff, Clock, AlertTriangle } from "lucide-react";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/hooks/use-translation";

type ErrorSeverity = "error" | "warning";

type ErrorKind = "api_error" | "connection_lost" | "timeout";

export function classifyError(error: string): ErrorKind {
  const lower = error.toLowerCase();
  if (
    lower.includes("connection lost") ||
    lower.includes("failed to fetch") ||
    lower.includes("network") ||
    lower.includes("disconnected") ||
    lower.includes("aborted")
  ) {
    return "connection_lost";
  }
  if (lower.includes("timeout") || lower.includes("timed out")) {
    return "timeout";
  }
  return "api_error";
}

interface ErrorBannerProps {
  error: string;
  severity?: ErrorSeverity;
  className?: string;
  hasMessages?: boolean;
  onRetry?: () => void;
  onClear?: () => void;
  retryLabel?: string;
}

export function ErrorBanner({
  error,
  severity,
  className,
  hasMessages,
  onRetry,
  onClear,
  retryLabel,
}: ErrorBannerProps) {
  const { t } = useTranslation();
  if (!error) return null;

  const kind = classifyError(error);
  const isWarning = severity === "warning" || kind === "connection_lost" || kind === "timeout";

  const containerClass = isWarning
    ? "border-warning/30 bg-warning/5 dark:bg-warning/15 text-warning"
    : "border-destructive/30 bg-destructive/10 text-destructive";

  const Icon = kind === "connection_lost" ? WifiOff : kind === "timeout" ? Clock : isWarning ? AlertTriangle : AlertCircle;

  const label = kind === "connection_lost"
    ? t("chat.error_connection_lost")
    : kind === "timeout"
      ? t("chat.error_timeout")
      : isWarning
        ? t("chat.error_generic_warning")
        : null;

  const buttonHover = isWarning
    ? "hover:bg-warning/10 text-warning"
    : "text-destructive hover:bg-destructive/10";
  const closeHover = isWarning
    ? "hover:bg-warning/20 text-warning/60 hover:text-warning"
    : "hover:bg-destructive/20 text-destructive/60 hover:text-destructive";

  return (
    <div
      data-testid="error-banner"
      data-severity={isWarning ? "warning" : "error"}
      className={cn(
        "flex items-center gap-3 rounded-lg border p-4 text-sm font-medium",
        containerClass,
        className ?? "mb-8"
      )}
    >
      <Icon className="h-4 w-4 shrink-0" />
      {label && <span className="shrink-0 font-semibold">{label}</span>}
      <span className="flex-1 line-clamp-2">{t("common.error_prefix", { error })}</span>
      {hasMessages && onRetry && (
        <button
          type="button"
          onClick={onRetry}
          className={cn("shrink-0 rounded-md px-2 py-1 text-xs font-medium transition-colors", buttonHover)}
        >
          <RotateCcw className="h-3 w-3 mr-1 inline" />
          {retryLabel ?? t("error.retry")}
        </button>
      )}
      {onClear && (
        <button
          type="button"
          onClick={onClear}
          aria-label={t("common.close")}
          className={cn("shrink-0 rounded p-0.5 transition-colors", closeHover)}
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
