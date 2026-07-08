// ui/src/components/chat/ParentBadge.tsx
import { CornerUpLeft } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";

interface ParentBadgeProps {
  /** Title of the parent session. Null renders as "previous session". */
  parentTitle: string | null;
  onNavigate: () => void;
}

export function ParentBadge({ parentTitle, onNavigate }: ParentBadgeProps) {
  const { t } = useTranslation();
  return (
    <button
      onClick={onNavigate}
      className="inline-flex items-center gap-1 text-3xs text-muted-foreground hover:text-foreground transition-colors mt-0.5 rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <CornerUpLeft className="h-4 w-4 shrink-0" />
      <span className="truncate max-w-[160px]">
        {parentTitle ?? t("chat.previous_session")}
      </span>
    </button>
  );
}
