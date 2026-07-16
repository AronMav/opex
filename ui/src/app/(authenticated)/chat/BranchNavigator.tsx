"use client";

import { ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useChatStore } from "@/stores/chat-store";
import { useTranslation } from "@/hooks/use-translation";

interface BranchNavigatorProps {
  parentMessageId: string;
  siblings: { id: string }[];  // ordered by created_at
  currentIndex: number;        // 0-based index of current sibling
  /** Fix L: disable both arrows while a turn is active for the session — a
   * mid-turn branch switch re-walks the active path and blends two branches. */
  disabled?: boolean;
}

export function BranchNavigator({ parentMessageId, siblings, currentIndex, disabled = false }: BranchNavigatorProps) {
  const switchBranch = useChatStore((s) => s.switchBranch);
  const { t } = useTranslation();

  if (siblings.length <= 1) return null;

  return (
    <div className="flex items-center gap-0.5 text-xs text-muted-foreground">
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => switchBranch(parentMessageId, siblings[currentIndex - 1].id)}
        disabled={disabled || currentIndex === 0}
        className="h-5 w-5 rounded-full"
        aria-label={t("chat.branch_previous")}
      >
        <ChevronLeft className="h-4 w-4" />
      </Button>
      <span className="tabular-nums min-w-8 text-center">
        {currentIndex + 1}/{siblings.length}
      </span>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => switchBranch(parentMessageId, siblings[currentIndex + 1].id)}
        disabled={disabled || currentIndex === siblings.length - 1}
        className="h-5 w-5 rounded-full"
        aria-label={t("chat.branch_next")}
      >
        <ChevronRight className="h-4 w-4" />
      </Button>
    </div>
  );
}
