"use client";

import { Avatar, AvatarImage, AvatarFallback } from "@/components/ui/avatar";
import { Bot, User } from "lucide-react";
import { GenerativeUISlot } from "@/components/ui/card-registry";
import { useTranslation } from "@/hooks/use-translation";

// ── Avatar colors & hashing ──────────────────────────────────────────────────

// Agent avatar palette maps onto the theme-aware chart tokens (chart-1..8),
// which already resolve per light/dark — no dark: overrides needed.
export const AGENT_COLORS = [
  "bg-chart-1/15 text-chart-1 border-chart-1/25",
  "bg-chart-5/15 text-chart-5 border-chart-5/25",
  "bg-chart-2/15 text-chart-2 border-chart-2/25",
  "bg-chart-3/15 text-chart-3 border-chart-3/25",
  "bg-chart-4/15 text-chart-4 border-chart-4/25",
  "bg-chart-6/15 text-chart-6 border-chart-6/25",
  "bg-chart-7/15 text-chart-7 border-chart-7/25",
  "bg-chart-8/15 text-chart-8 border-chart-8/25",
];

export function hashAgentName(name: string): number {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = ((hash << 5) - hash + name.charCodeAt(i)) | 0;
  }
  return Math.abs(hash);
}

// ── Avatar ───────────────────────────────────────────────────────────────────

export function RoleAvatar({
  role,
  iconUrl,
  agentName,
}: {
  role: "user" | "assistant" | "agent-sender";
  iconUrl?: string | null;
  agentName?: string;
}) {
  const { t } = useTranslation();
  const isUser = role === "user";
  const isAgentSender = role === "agent-sender";

  if (isUser && !isAgentSender) {
    return (
      <Avatar className="h-9 w-9 rounded-xl shadow-sm">
        <AvatarFallback className="rounded-xl bg-primary/10 border border-primary/30 text-primary">
          <User className="h-4 w-4" />
        </AvatarFallback>
      </Avatar>
    );
  }

  const colorIdx = agentName ? hashAgentName(agentName) % AGENT_COLORS.length : 0;
  return (
    <Avatar className="h-9 w-9 rounded-xl shadow-sm">
      {iconUrl && <AvatarImage key={iconUrl} src={iconUrl} alt={agentName || t("common.agent")} className="rounded-xl object-cover" />}
      <AvatarFallback className={`rounded-xl text-sm font-semibold border ${agentName ? AGENT_COLORS[colorIdx] : "bg-muted/50 border-border text-muted-foreground"}`}>
        {agentName ? agentName[0].toUpperCase() : <Bot className="h-4 w-4" />}
      </AvatarFallback>
    </Avatar>
  );
}

// ── Part renderers (exported for MessageItem.tsx) ───────────────────────────

export function RichCardDataPartView({ data }: { data: Record<string, unknown> }) {
  const { cardType, ...rest } = data;
  if (cardType === "agent-turn") {
    return null; // Deprecated: async delegation replaced agent-turn cards
  }
  return <GenerativeUISlot cardType={String(cardType ?? "unknown")} data={rest} />;
}
