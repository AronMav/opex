"use client";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useChatStore } from "@/stores/chat-store";
import { useAgents, useProviders, useProviderModelsDetailed } from "@/lib/queries";
import { ModelBadges } from "@/components/model-badges";

export function ModelDropdown({ agent }: { agent: string }) {
  const modelOverride = useChatStore(s => s.agents[agent]?.modelOverride ?? null);
  const { data: allAgents } = useAgents();
  const { data: allProviders = [] } = useProviders();
  const agentInfo = allAgents?.find(a => a.name === agent);
  const providerConnection = agentInfo?.provider_connection;
  const selectedProvider = allProviders.filter(p => p.type === "text").find(p => p.name === providerConnection);
  const defaultModel = agentInfo?.model ?? "";
  const { data: models } = useProviderModelsDetailed(selectedProvider?.id ?? null);

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
      <SelectTrigger className="h-6 border-0 bg-transparent text-3xs font-mono uppercase tracking-wide text-muted-foreground hover:text-foreground px-1 gap-1 w-auto max-w-32">
        <SelectValue>{shortModel}</SelectValue>
      </SelectTrigger>
      <SelectContent className="border-border text-xs">
        {models.map((pm) => (
          <SelectItem key={pm.id} value={pm.id} className="font-mono text-xs">
            <span className="flex items-center justify-between gap-3 w-full min-w-0">
              <span className="truncate">{pm.id === defaultModel ? `${pm.id} ★` : pm.id}</span>
              <ModelBadges m={pm} className="shrink-0" />
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
