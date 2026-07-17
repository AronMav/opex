"use client";

import { useState, useCallback } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { ApprovalPart } from "@/stores/chat-store";
import { useChatStore } from "@/stores/chat-store";
import { decideApproval, addApprovalAllowlist } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { ChevronRight, Loader2 } from "lucide-react";
import { ApprovalCountdown } from "./ApprovalCountdown";
import { ApprovalArgsEditor } from "./ApprovalArgsEditor";

interface ApprovalCardProps {
  part: ApprovalPart;
}

export function ApprovalCard({ part }: ApprovalCardProps) {
  const { t } = useTranslation();
  const currentAgent = useChatStore((s) => s.currentAgent);
  const [isEditing, setIsEditing] = useState(false);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [payloadExpanded, setPayloadExpanded] = useState(false);

  const handleApprove = useCallback(async () => {
    setIsSubmitting(true);
    setError(null);
    try {
      const result = await decideApproval(part.approvalId, "approved");
      if (!result.ok) {
        setError(result.error ?? t("chat.approval_error"));
      }
    } catch {
      setError(t("chat.approval_error"));
    } finally {
      setIsSubmitting(false);
    }
  }, [part.approvalId, t]);

  const handleAlwaysAllow = useCallback(async () => {
    setIsSubmitting(true);
    setError(null);
    try {
      // Persist the durable grant first; only approve if it stuck, so the user
      // isn't misled into thinking future calls are whitelisted when they aren't.
      if (currentAgent) {
        const grant = await addApprovalAllowlist(currentAgent, part.toolName);
        if (!grant.ok) {
          setError(grant.error ?? t("chat.approval_error"));
          return;
        }
      }
      const result = await decideApproval(part.approvalId, "approved");
      if (!result.ok) {
        setError(result.error ?? t("chat.approval_error"));
      }
    } catch {
      setError(t("chat.approval_error"));
    } finally {
      setIsSubmitting(false);
    }
  }, [part.approvalId, part.toolName, currentAgent, t]);

  const handleReject = useCallback(async () => {
    setIsSubmitting(true);
    setError(null);
    try {
      const result = await decideApproval(part.approvalId, "rejected");
      if (!result.ok) {
        setError(result.error ?? t("chat.approval_error"));
      }
    } catch {
      setError(t("chat.approval_error"));
    } finally {
      setIsSubmitting(false);
    }
  }, [part.approvalId, t]);

  const handleApproveWithModified = useCallback(
    async (modified: Record<string, unknown>) => {
      setIsSubmitting(true);
      setError(null);
      try {
        const result = await decideApproval(part.approvalId, "approved", modified);
        if (!result.ok) {
          setError(result.error ?? t("chat.approval_error"));
        } else {
          setIsEditing(false);
        }
      } catch {
        setError(t("chat.approval_error"));
      } finally {
        setIsSubmitting(false);
      }
    },
    [part.approvalId, t],
  );

  // ── Resolved states (approved / rejected / timeout_rejected) ──────────────

  if (part.status !== "pending") {
    const isApproved = part.status === "approved";
    const statusLabel =
      part.status === "approved"
        ? t("chat.approval_approved")
        : part.status === "rejected"
          ? t("chat.approval_rejected")
          : t("chat.approval_timed_out");
    const statusColor = isApproved ? "text-success" : "text-destructive";
    const dotColor = isApproved ? "bg-success" : "bg-destructive";

    return (
      <div
        role="status"
        aria-label={`${part.toolName} ${statusLabel}`}
        className="rounded-lg border border-border/50 bg-card/50 px-4 py-2 flex items-center gap-2"
      >
        <div className={`w-2 h-2 rounded-full shrink-0 ${dotColor}`} />
        <span className="font-mono text-xs">{part.toolName}</span>
        {part.modifiedInput && (
          <span className="text-muted-foreground text-xs">
            {t("chat.approval_modified")}
          </span>
        )}
        <span className={`ml-auto font-mono text-3xs font-bold uppercase tracking-widest ${statusColor}`}>
          {statusLabel}
        </span>
      </div>
    );
  }

  // ── Pending state ─────────────────────────────────────────────────────────

  const inputDisplay = JSON.stringify(part.toolInput, null, 2);

  return (
    <div className="rounded-lg border border-warning/40 bg-card/50 p-4" role="status" aria-label={t("chat.approval_awaiting")}>
      {/* Header row */}
      <Collapsible open={payloadExpanded} onOpenChange={setPayloadExpanded}>
        <div className="flex items-center gap-2">
          <div className="w-2 h-2 rounded-full bg-warning animate-pulse shadow-lg shadow-warning/30 shrink-0" aria-hidden="true" />
          <span className="font-mono text-xs font-semibold tracking-tight text-foreground truncate">
            {part.toolName}
          </span>
          <span className="ml-auto font-mono text-3xs font-bold uppercase tracking-widest text-warning">
            {t("chat.approval_awaiting")}
          </span>
          <CollapsibleTrigger asChild>
            <button
              type="button"
              className="p-0.5 text-muted-foreground-subtle hover:text-foreground transition-colors group rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              aria-label={t("common.expand")}
            >
              <ChevronRight className="h-4 w-4 transition-transform duration-300 group-data-[state=open]:rotate-90" />
            </button>
          </CollapsibleTrigger>
        </div>

        {/* Always-visible payload preview — admin must see args before approving, without clicking.
            Hidden while expanded to avoid showing the same JSON twice. */}
        {!payloadExpanded && (
          <div className="mt-2">
            <span className="font-mono text-3xs font-bold uppercase tracking-wider text-primary/80">
              {t("chat.approval_input")}
            </span>
            <pre className="bg-muted/30 rounded p-2 text-xs font-mono overflow-x-auto line-clamp-3 mt-1 whitespace-pre-wrap">
              {inputDisplay}
            </pre>
          </div>
        )}

        {/* Collapsible: full, unclamped INPUT section */}
        <CollapsibleContent>
          <div className="mt-2">
            <span className="font-mono text-3xs font-bold uppercase tracking-wider text-primary/80">
              {t("chat.approval_input")}
            </span>
            <pre className="bg-muted/30 rounded p-2 text-xs font-mono overflow-x-auto max-h-50 mt-1 whitespace-pre-wrap">
              {inputDisplay}
            </pre>
          </div>
        </CollapsibleContent>
      </Collapsible>

      {/* Countdown timer */}
      <div className="mt-2">
        <ApprovalCountdown
          timeoutMs={part.timeoutMs}
          receivedAt={part.receivedAt}
          status={part.status}
        />
      </div>

      {/* Args editor (when editing) */}
      {isEditing && (
        <div className="mt-3">
          <ApprovalArgsEditor
            initialInput={part.toolInput}
            onSubmit={handleApproveWithModified}
            onCancel={() => setIsEditing(false)}
          />
        </div>
      )}

      {/* Error display */}
      {error && (
        <p className="text-destructive text-xs mt-2">{error}</p>
      )}

      {/* Button row — stacks vertically on mobile, single row on sm+ */}
      {!isEditing && (
        <div className="flex flex-col sm:flex-row sm:items-center gap-2 mt-3">
          <Button
            variant="ghost"
            size="sm"
            className="text-primary text-xs w-full sm:w-auto justify-start sm:justify-center"
            onClick={() => setIsEditing(true)}
            disabled={isSubmitting}
          >
            {t("chat.approval_edit_args")}
          </Button>
          {currentAgent && (
            <Button
              variant="ghost"
              size="sm"
              className="text-muted-foreground text-xs w-full sm:w-auto justify-start sm:justify-center"
              title={t("chat.approval_always_allow_tooltip")}
              aria-label={`${t("chat.approval_always_allow")} ${part.toolName}`}
              onClick={handleAlwaysAllow}
              disabled={isSubmitting}
            >
              {t("chat.approval_always_allow")}
            </Button>
          )}
          <div className="flex items-center gap-2 sm:ml-auto">
            <Button
              variant="outline"
              size="sm"
              className="flex-1 sm:flex-none text-destructive border-destructive/40 hover:bg-destructive/10"
              aria-label={`${t("chat.approval_reject")} ${part.toolName}`}
              onClick={handleReject}
              disabled={isSubmitting}
            >
              {isSubmitting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                t("chat.approval_reject")
              )}
            </Button>
            <Button
              variant="success"
              size="sm"
              className="flex-1 sm:flex-none"
              aria-label={`${t("chat.approval_approve")} ${part.toolName}`}
              onClick={handleApprove}
              disabled={isSubmitting}
            >
              {isSubmitting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                t("chat.approval_approve")
              )}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
