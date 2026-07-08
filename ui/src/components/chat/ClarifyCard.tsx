"use client";

import { memo, useState, useCallback } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { ClarifyPart } from "@/stores/chat-store";
import { submitClarify } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Loader2 } from "lucide-react";
import { ApprovalCountdown } from "./ApprovalCountdown";

interface ClarifyCardProps {
  part: ClarifyPart;
}

function ClarifyCardImpl({ part }: ClarifyCardProps) {
  const { t } = useTranslation();
  const [otherText, setOtherText] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submittingChoice, setSubmittingChoice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState<string | null>(part.response);

  const handleSubmit = useCallback(
    async (response: string, choiceKey?: string) => {
      if (!response.trim()) return;
      setIsSubmitting(true);
      if (choiceKey !== undefined) setSubmittingChoice(choiceKey);
      setError(null);
      try {
        const result = await submitClarify(part.clarifyId, response.trim());
        if (result.ok) {
          setSubmitted(response.trim());
        } else {
          setError(result.error ?? t("chat.clarify_error"));
        }
      } catch {
        setError(t("chat.clarify_error"));
      } finally {
        setIsSubmitting(false);
        setSubmittingChoice(null);
      }
    },
    [part.clarifyId, t],
  );

  // ── Resolved state ────────────────────────────────────────────────────────

  if (submitted !== null) {
    return (
      <div
        role="status"
        aria-label={t("chat.clarify_answered")}
        className="rounded-lg border border-border/50 bg-card/50 px-4 py-2 flex items-start gap-2"
      >
        <div className="w-2 h-2 rounded-full shrink-0 bg-success mt-1.5" />
        <div className="flex-1 min-w-0">
          <p className="text-xs text-muted-foreground leading-relaxed">{part.question}</p>
          <p className="text-xs font-medium mt-1 truncate">{submitted}</p>
        </div>
        <span className="ml-auto font-mono text-3xs font-bold uppercase tracking-widest text-success shrink-0">
          {t("chat.clarify_answered")}
        </span>
      </div>
    );
  }

  // ── Pending state ─────────────────────────────────────────────────────────

  return (
    <div
      className="rounded-lg border border-primary/30 bg-card/50 p-4"
      role="status"
      aria-label={t("chat.clarify_awaiting")}
    >
      {/* Header */}
      <div className="flex items-start gap-2">
        <div
          className="w-2 h-2 rounded-full bg-primary animate-pulse shadow-lg shadow-primary/30 shrink-0 mt-1"
          aria-hidden="true"
        />
        <p className="text-sm leading-relaxed flex-1">{part.question}</p>
        <span className="ml-auto font-mono text-3xs font-bold uppercase tracking-widest text-primary shrink-0">
          {t("chat.clarify_awaiting")}
        </span>
      </div>

      {/* Countdown */}
      <div className="mt-2">
        <ApprovalCountdown
          timeoutMs={part.timeoutMs}
          receivedAt={part.receivedAt}
          status="pending"
        />
      </div>

      {/* Error */}
      {error && <p className="text-destructive text-xs mt-2">{error}</p>}

      {/* Choice buttons */}
      {part.choices.length > 0 && (
        <div className="flex flex-wrap gap-2 mt-3">
          {part.choices.map((choice) => (
            <Button
              key={choice}
              variant="outline"
              size="sm"
              className="text-xs"
              onClick={() => handleSubmit(choice, choice)}
              disabled={isSubmitting}
            >
              {submittingChoice === choice ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                choice
              )}
            </Button>
          ))}
        </div>
      )}

      {/* Other — free-text input */}
      <div className="mt-3 flex items-center gap-2">
        <input
          type="text"
          value={otherText}
          onChange={(e) => setOtherText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              handleSubmit(otherText);
            }
          }}
          placeholder={t("chat.clarify_other_placeholder")}
          disabled={isSubmitting}
          className="flex-1 rounded-md border border-input bg-background px-3 py-1.5 text-xs placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
          aria-label={t("chat.clarify_other_placeholder")}
        />
        <Button
          variant="default"
          size="sm"
          className="text-xs shrink-0"
          onClick={() => handleSubmit(otherText)}
          disabled={isSubmitting || !otherText.trim()}
        >
          {isSubmitting ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            t("chat.clarify_submit")
          )}
        </Button>
      </div>
    </div>
  );
}

export const ClarifyCard = memo(ClarifyCardImpl);
