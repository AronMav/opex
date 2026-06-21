"use client";

import { Avatar, AvatarImage, AvatarFallback } from "@/components/ui/avatar";
import { Bot, User } from "lucide-react";
import { GenerativeUISlot } from "@/components/ui/card-registry";

// ── Avatar colors & hashing ──────────────────────────────────────────────────

export const AGENT_COLORS = [
  "bg-blue-500/15 text-blue-600 dark:text-blue-400 border-blue-500/25",
  "bg-purple-500/15 text-purple-600 dark:text-purple-400 border-purple-500/25",
  "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 border-emerald-500/25",
  "bg-amber-500/15 text-amber-600 dark:text-amber-400 border-amber-500/25",
  "bg-rose-500/15 text-rose-600 dark:text-rose-400 border-rose-500/25",
  "bg-cyan-500/15 text-cyan-600 dark:text-cyan-400 border-cyan-500/25",
  "bg-orange-500/15 text-orange-600 dark:text-orange-400 border-orange-500/25",
  "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400 border-indigo-500/25",
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
  const isUser = role === "user";
  const isAgentSender = role === "agent-sender";

  if (isUser && !isAgentSender) {
    return (
      <Avatar className="h-9 w-9 rounded-xl shadow-sm">
        <AvatarFallback className="rounded-xl bg-primary/10 border border-primary/20 text-primary">
          <User className="h-4 w-4" />
        </AvatarFallback>
      </Avatar>
    );
  }

  const colorIdx = agentName ? hashAgentName(agentName) % AGENT_COLORS.length : 0;
  return (
    <Avatar className="h-9 w-9 rounded-xl shadow-sm">
      {iconUrl && <AvatarImage key={iconUrl} src={iconUrl} alt={agentName || "agent"} className="rounded-xl object-cover" />}
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
