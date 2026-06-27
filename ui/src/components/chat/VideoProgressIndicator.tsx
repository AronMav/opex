"use client";
import { useChatStore } from "@/stores/chat-store";
import { Loader2 } from "lucide-react";

export function VideoProgressIndicator({ sessionId }: { sessionId: string | null }) {
  const entry = useChatStore((s) => (sessionId ? s.videoProgress[sessionId] : undefined));
  if (!entry) return null;
  return (
    <div className="flex items-center gap-2 px-4 py-2 text-sm text-muted-foreground">
      <Loader2 className="h-4 w-4 animate-spin" />
      <span>{entry.text}</span>
    </div>
  );
}
