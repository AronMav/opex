"use client";

import { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Field } from "@/components/ui/field";
import type { Provider, RoutingRule } from "@/types/api";
import { Link2, Settings, Plus, ChevronDown, ChevronUp, X } from "lucide-react";

export const PROVIDERS = [
  { value: "minimax", label: "MiniMax" },
  { value: "anthropic", label: "Anthropic" },
  { value: "google", label: "Google Gemini" },
  { value: "openai", label: "OpenAI" },
  { value: "deepseek", label: "DeepSeek" },
  { value: "groq", label: "Groq" },
  { value: "together", label: "Together AI" },
  { value: "openrouter", label: "OpenRouter" },
  { value: "mistral", label: "Mistral" },
  { value: "xai", label: "xAI (Grok)" },
  { value: "perplexity", label: "Perplexity" },
  { value: "ollama", label: "Ollama (local)" },
  { value: "claude-cli", label: "Claude CLI" },
] as const;

export const ROUTING_CONDITIONS: { value: string; labelKey: TranslationKey }[] = [
  { value: "default", labelKey: "agents.routing_default" },
  { value: "short", labelKey: "agents.routing_short" },
  { value: "long", labelKey: "agents.routing_long" },
  { value: "with_tools", labelKey: "agents.routing_with_tools" },
  { value: "financial", labelKey: "agents.routing_financial" },
  { value: "analytical", labelKey: "agents.routing_analytical" },
  { value: "code", labelKey: "agents.routing_code" },
  { value: "fallback", labelKey: "agents.routing_fallback" },
];

export const FALLBACK_MODELS: Record<string, string[]> = {
  minimax: ["MiniMax-M2.5", "MiniMax-M1"],
  anthropic: ["claude-sonnet-4-20250514", "claude-haiku-4-5-20251001", "claude-opus-4-20250514"],
  google: ["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.0-flash"],
  openai: ["gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano", "o4-mini", "o3"],
  deepseek: ["deepseek-chat", "deepseek-reasoner"],
  groq: ["llama-3.3-70b-versatile", "llama-3.1-8b-instant", "gemma2-9b-it"],
  openrouter: [],
  mistral: ["mistral-large-latest", "mistral-small-latest", "codestral-latest"],
  xai: ["grok-3", "grok-3-mini"],
  perplexity: ["sonar-pro", "sonar"],
  ollama: [],
  "claude-cli": [],
  together: [],
};

function RoutingRuleRow({
  rule,
  llmProviders,
  discoveredModels,
  fetchModels,
  onChange,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  rule: RoutingRule;
  llmProviders: Provider[];
  discoveredModels: Record<string, string[]>;
  fetchModels: (connection: string) => void;
  onChange: (patch: Partial<RoutingRule>) => void;
  onRemove: () => void;
  onMoveUp?: () => void;
  onMoveDown?: () => void;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="rounded-lg border border-border bg-muted/20 p-3 space-y-2">
      <div className="flex flex-col sm:flex-row sm:items-center gap-2">
        <div className="flex-1 grid grid-cols-1 sm:grid-cols-3 gap-2">
          <Select
            value={rule.provider || "__none__"}
            onValueChange={(v) => {
              if (v === "__none__") { onChange({ provider: "", model: "" }); return; }
              const conn = llmProviders.find((p) => p.name === v);
              onChange({ provider: v, model: conn?.default_model ?? "" });
              fetchModels(v);
            }}
          >
            <SelectTrigger className="w-full bg-background border-border text-xs h-8">
              <SelectValue placeholder="Select provider..." />
            </SelectTrigger>
            <SelectContent className="border-border">
              <SelectItem value="__none__" className="text-xs text-muted-foreground">
                <span className="text-muted-foreground">&mdash;</span>
              </SelectItem>
              {llmProviders.map((conn) => (
                <SelectItem key={conn.name} value={conn.name} className="text-xs">
                  <span className="flex items-center gap-2">
                    <Link2 className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                    <span>{conn.name}</span>
                    <span className="text-muted-foreground/60 text-2xs">{conn.default_model}</span>
                  </span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input value={rule.model} placeholder={t("agents.model_placeholder")}
            className="bg-background border-border font-mono text-xs h-8"
            onChange={(e) => onChange({ model: e.target.value })} />
          <Select value={rule.condition} onValueChange={(v) => onChange({ condition: v })}>
            <SelectTrigger className="w-full bg-background border-border text-xs h-8">
              <SelectValue />
            </SelectTrigger>
            <SelectContent className="border-border">
              {ROUTING_CONDITIONS.map((c) => (
                <SelectItem key={c.value} value={c.value} className="text-xs">
                  {t(c.labelKey)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="flex items-center gap-0.5 shrink-0 self-end sm:self-auto">
          {onMoveUp && (
            <Button size="icon-sm" variant="ghost" className="text-muted-foreground hover:text-foreground" onClick={onMoveUp}>
              <ChevronUp className="h-3.5 w-3.5" />
            </Button>
          )}
          {onMoveDown && (
            <Button size="icon-sm" variant="ghost" className="text-muted-foreground hover:text-foreground" onClick={onMoveDown}>
              <ChevronDown className="h-3.5 w-3.5" />
            </Button>
          )}
          <Button
            size="icon-sm"
            variant="ghost"
            className="text-muted-foreground hover:text-foreground"
            onClick={() => setExpanded(!expanded)}
            title={t("agents.routing_advanced")}
          >
            <Settings className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            className="text-muted-foreground hover:text-destructive"
            onClick={onRemove}
          >
            <X className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
      {expanded && (
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-2 pt-1 animate-in fade-in duration-200">
          <Field label={t("agents.routing_field_temperature")} labelClassName="text-xs">
            <Input
              type="number"
              step="0.1"
              min="0"
              max="2"
              value={rule.temperature != null ? String(rule.temperature) : ""}
              placeholder={t("agents.routing_placeholder_none")}
              className="bg-background border-border font-mono text-xs h-8"
              onChange={(e) => onChange({ temperature: e.target.value ? parseFloat(e.target.value) : null })}
            />
          </Field>
        </div>
      )}
    </div>
  );
}

export interface RoutingRulesEditorProps {
  routing: RoutingRule[];
  llmProviders: Provider[];
  discoveredModels: Record<string, string[]>;
  fetchModels: (connection: string) => void;
  onChange: (routing: RoutingRule[]) => void;
}

export function RoutingRulesEditor({
  routing,
  llmProviders,
  discoveredModels,
  fetchModels,
  onChange,
}: RoutingRulesEditorProps) {
  const { t } = useTranslation();

  return (
    <div className="space-y-2 border-t border-border/30 pt-3">
      <div className="flex items-center justify-between">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-foreground">
          {t("agents.section_routing_rules")}
          {routing.length > 0 && (
            <span className="ml-2 text-muted-foreground font-normal">
              ({routing.length})
            </span>
          )}
        </h3>
        <Button
          size="sm"
          variant="outline"
          className="h-7 px-2 text-xs"
          onClick={() =>
            onChange([
              ...routing,
              { provider: llmProviders[0]?.name ?? "", model: llmProviders[0]?.default_model ?? "", condition: "default" },
            ])
          }
        >
          <Plus className="h-3 w-3 mr-1" /> {t("agents.add_rule")}
        </Button>
      </div>
      {routing.length === 0 ? (
        <p className="text-xs text-muted-foreground/60 py-2">
          {t("agents.no_routing")}
        </p>
      ) : (
        <div className="space-y-3">
          {routing.map((rule, idx) => (
            <RoutingRuleRow
              key={idx}
              rule={rule}
              llmProviders={llmProviders}
              discoveredModels={discoveredModels}
              fetchModels={fetchModels}
              onChange={(patch) => {
                const next = [...routing];
                next[idx] = { ...next[idx], ...patch };
                onChange(next);
              }}
              onRemove={() => {
                onChange(routing.filter((_, i) => i !== idx));
              }}
              onMoveUp={idx > 0 ? () => {
                const next = [...routing];
                [next[idx - 1], next[idx]] = [next[idx], next[idx - 1]];
                onChange(next);
              } : undefined}
              onMoveDown={idx < routing.length - 1 ? () => {
                const next = [...routing];
                [next[idx], next[idx + 1]] = [next[idx + 1], next[idx]];
                onChange(next);
              } : undefined}
            />
          ))}
        </div>
      )}
    </div>
  );
}
