"use client";

import { memo } from "react";
import { cleanContent } from "@/lib/format";
import { MessageContent } from "@/components/ui/message";
import { useChatStore } from "@/stores/chat-store";
import { useSmoothedText } from "@/hooks/use-smoothed-text";

export const TextPart = memo(function TextPart({ text }: { text: string }) {
  const isStreaming = useChatStore(
    (s) => s.agents[s.currentAgent]?.connectionPhase === "streaming"
  );
  const cleaned = cleanContent(text);
  const smoothed = useSmoothedText(cleaned, isStreaming);
  if (!smoothed) return null;
  return (
    <MessageContent
      markdown
      isStreaming={isStreaming}
      className="prose prose-sm dark:prose-invert max-w-none bg-transparent p-0 overflow-x-auto
        [&_p]:leading-relaxed [&_p]:text-foreground [&_p]:text-[15px]
        [&_pre]:my-4 [&_pre]:border [&_pre]:border-border [&_pre]:bg-muted/50 [&_pre]:shadow-inner [&_pre]:rounded-lg
        [&_table]:block [&_table]:overflow-x-auto [&_table]:w-full
        [&_a]:text-primary [&_a]:font-bold [&_a]:no-underline hover:[&_a]:underline
        [&_li]:text-foreground [&_strong]:text-foreground [&_strong]:font-bold"
    >
      {smoothed}
    </MessageContent>
  );
});
