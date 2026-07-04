"use client";

import { useTranslation } from "@/hooks/use-translation";
import type { SessionRow } from "@/types/api";

export function ReadOnlyFooter({ activeSession }: { activeSession?: SessionRow }) {
  const { t } = useTranslation();
  const label =
    activeSession?.channel === "heartbeat" ? t("chat.heartbeat_session") :
    activeSession?.channel === "cron" ? t("chat.cron_session") :
    activeSession?.channel === "group" ? t("chat.group_chat") :
    t("chat.inter_agent_session");

  return (
    <div className="shrink-0 w-full px-3 md:px-4 py-3 border-t border-primary/30 bg-primary/5">
      <div className="mx-auto max-w-4xl text-center text-sm text-primary/50 font-medium py-1">
        {label} — {t("chat.read_only")}
      </div>
    </div>
  );
}
