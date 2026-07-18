import { MessageSquareShare } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";
import { useAuthStore } from "@/stores/auth-store";
import { displayAgentName } from "@/lib/agent-display";

/**
 * Divider rendered when switching between agents in a multi-agent session.
 * Replaces the legacy "Handoff" mechanism with the modern agent tool model.
 *
 * WS6: `agentName` (really `agentId`) may be a raw session UUID when a
 * session was silently recreated — never surface that to the user.
 */
export function AgentTransitionDivider({ agentName }: { agentName: string }) {
  const { t } = useTranslation();
  const knownAgents = useAuthStore((s) => s.agents);
  const display = displayAgentName(agentName, knownAgents, t);
  return (
    <div className="my-6 flex items-center gap-3 px-4 md:px-6" role="separator" aria-label={t("chat.agent_transition", { agent: display })}>
      <div className="h-px flex-1 bg-border/40" />
      <div className="flex items-center gap-2 rounded-full border border-border/50 bg-muted/30 px-3 py-1 text-2xs font-medium text-muted-foreground shadow-sm">
        <MessageSquareShare className="h-4 w-4" />
        <span className="uppercase tracking-wider">{display}</span>
      </div>
      <div className="h-px flex-1 bg-border/40" />
    </div>
  );
}
