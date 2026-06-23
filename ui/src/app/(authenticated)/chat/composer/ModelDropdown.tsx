"use client";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useChatStore } from "@/stores/chat-store";
import { useAgents, useProviders, useProviderModels } from "@/lib/queries";

export function ModelDropdown({ agent }: { agent: string }) {
  const modelOverride = useChatStore(s => s.agents[agent]?.modelOverride ?? null);
  const { data: allAgents } = useAgents();
  const { data: allProviders = [] } = useProviders();
  const agentInfo = allAgents?.find(a => a.name === agent);
  const providerConnection = agentInfo?.provider_connection;
  const selectedProvider = allProviders.filter(p => p.type === "text").find(p => p.name === providerConnection);
  const defaultModel = agentInfo?.model ?? "";
  const { data: models } = useProviderModels(selectedProvider?.id ?? null);

  const currentModel = modelOverride ?? defaultModel;
  const shortModel = currentModel.split("/").pop()?.split(":")[0] ?? currentModel;

  if (!models || models.length <= 1) return null;

  return (
    <Select
      value={currentModel}
      onValueChange={(val) => {
        useChatStore.getState().setModelOverride(agent, val === defaultModel ? null : val);
      }}
    >
      <SelectTrigger className="h-6 border-0 bg-transparent text-[10px] font-mono uppercase tracking-wide text-muted-foreground hover:text-foreground px-1 gap-1 w-auto max-w-[130px]">
        <SelectValue>{shortModel}</SelectValue>
      </SelectTrigger>
      <SelectContent className="border-border text-xs">
        {(models as string[]).map((m) => (
          <SelectItem key={m} value={m} className="font-mono text-xs">
            {m === defaultModel ? `${m} ★` : m}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
