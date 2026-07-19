"use client";

import { WalnutMark } from "@/components/ui/walnut-mark";
import { useTranslation } from "@/hooks/use-translation";
import { useChatStore } from "@/stores/chat-store";
import { Button } from "@/components/ui/button";
import { useAuthStore } from "@/stores/auth-store";
import { usePrompts, useAgentPrompts } from "@/lib/prompts";
import { fillComposer } from "./composer/draft";

// Static so Tailwind's content scanner (regex over source text) always finds
// these literal class names, even though they're applied by array index.
const SUGGESTION_DELAYS = ["delay-0", "delay-75", "delay-150"] as const;

export function ChatWelcomeScreen() {
  const { t } = useTranslation();
  const currentAgent = useChatStore((s) => s.currentAgent);
  const agentIcons = useAuthStore((s) => s.agentIcons);
  const agentIconUrl = currentAgent ? agentIcons[currentAgent] || null : null;

  // First 3 entries of the prompt library replace the hardcoded suggestion
  // chips when available — clicking one drops its text into the composer as an
  // editable draft (does NOT send), so the user can tweak before firing.
  // Resolution: the agent's own prompt library
  // (workspace/agents/{agent}/prompts.md) wins; else the shared
  // workspace/prompts.md; else the static suggestions (fail-soft).
  const { prompts: agentPrompts } = useAgentPrompts(currentAgent);
  const { prompts: sharedPrompts } = usePrompts();
  const prompts = agentPrompts.length > 0 ? agentPrompts : sharedPrompts;
  const suggestions = prompts.length > 0
    ? prompts.slice(0, 3).map((p) => ({ key: p.title, label: p.title, prompt: p.body }))
    : [
        { key: "chat.suggestion_news", label: t("chat.suggestion_news"), prompt: t("chat.suggestion_news") },
        { key: "chat.suggestion_search", label: t("chat.suggestion_search"), prompt: t("chat.suggestion_search") },
        { key: "chat.suggestion_tool", label: t("chat.suggestion_tool"), prompt: t("chat.suggestion_tool") },
      ];

  return (
    <div className="flex h-full flex-col items-center justify-center p-6 text-center">
      <div className="relative mb-8">
        <div className="absolute inset-0 rounded-2xl bg-primary/20 blur-2xl" />
        <div className="relative flex h-24 w-24 items-center justify-center rounded-2xl border border-border/50 bg-card shadow-xl overflow-hidden">
          <div className="absolute inset-0 rounded-2xl bg-gradient-to-br from-primary/5 to-transparent" />
          {agentIconUrl ? (
            <img src={agentIconUrl} alt={currentAgent} className="h-full w-full object-cover" />
          ) : (
            <WalnutMark size={35} className="text-primary/80" />
          )}
        </div>
        <div className="absolute -bottom-1 -right-1 h-4 w-4 rounded-full border-2 border-card bg-success animate-pulse" />
      </div>
      <h2 className="mb-2 font-display text-lg font-bold uppercase tracking-widest text-foreground/80">
        {currentAgent || t("chat.ready")}
      </h2>
      <p className="max-w-xs font-sans text-sm leading-relaxed text-muted-foreground-subtle">
        {t("chat.write_message_to_start")}
      </p>
      <div className="mt-6 flex flex-wrap gap-2 justify-center max-w-md">
        {suggestions.map((s, i) => (
          <Button
            key={s.key}
            variant="outline"
            size="sm"
            onClick={() => fillComposer(currentAgent ?? "", s.prompt)}
            className={`animate-in fade-in slide-in-from-bottom-1 duration-300 hover:bg-primary/10 hover:border-primary/30 hover:text-foreground ${SUGGESTION_DELAYS[i] ?? ""}`}
          >
            {s.label}
          </Button>
        ))}
      </div>
    </div>
  );
}
