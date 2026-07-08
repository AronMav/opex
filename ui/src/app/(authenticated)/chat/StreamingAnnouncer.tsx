"use client";

import { useShallow } from "zustand/react/shallow";
import { useChatStore } from "@/stores/chat-store";
import { selectLiveAssistantText } from "@/stores/chat-selectors";
import { useTranslation } from "@/hooks/use-translation";
import { useStreamAnnouncer } from "./hooks/use-stream-announcer";

/**
 * Visually-hidden polite live region that announces the current agent's
 * streaming assistant response sentence-by-sentence. Always mounted so the
 * region exists in the DOM before its text changes.
 */
export function StreamingAnnouncer({ agent }: { agent: string }) {
  const { t } = useTranslation();
  const { id, text } = useChatStore(useShallow((s) => selectLiveAssistantText(s, agent)));
  const streaming = useChatStore((s) => s.agents[agent]?.connectionPhase === "streaming");
  const delta = useStreamAnnouncer(id, text, streaming);

  return (
    <div className="sr-only" role="status" aria-live="polite" aria-atomic="true" aria-label={t("chat.response_announcer")}>
      {delta}
    </div>
  );
}
