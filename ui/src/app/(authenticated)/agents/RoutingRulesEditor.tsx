"use client";

import { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Field } from "@/components/ui/field";
import type { Provider, RoutingRule } from "@/types/api";
import { Settings, Plus, ChevronDown, ChevronUp, X } from "lucide-react";
import { ModelCombobox, ProviderSelect } from "@/components/provider-fields";

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

function RoutingRuleRow({
  rule,
  llmProviders,
  onChange,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  rule: RoutingRule;
  llmProviders: Provider[];
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
          <ProviderSelect
            value={rule.provider}
            allowNone
            categories={["text", "llm"]}
            className="w-full bg-background border-border text-xs h-9"
            onChange={(v) => {
              if (v === "") { onChange({ provider: "", model: "" }); return; }
              const conn = llmProviders.find((p) => p.name === v);
              onChange({ provider: v, model: conn?.default_model ?? "" });
            }}
          />
          <ModelCombobox
            value={rule.model}
            onChange={(m) => onChange({ model: m })}
            providerId={llmProviders.find((p) => p.name === rule.provider)?.id ?? null}
            disabled={!rule.provider}
            placeholder={t("agents.model_placeholder")}
            className="w-full"
          />
          <Select value={rule.condition} onValueChange={(v) => onChange({ condition: v })}>
            <SelectTrigger className="w-full bg-background border-border text-xs h-9">
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
  onChange: (routing: RoutingRule[]) => void;
}

export function RoutingRulesEditor({
  routing,
  llmProviders,
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
          <Plus className="h-4 w-4 mr-1" /> {t("agents.add_rule")}
        </Button>
      </div>
      {routing.length === 0 ? (
        <p className="text-xs text-muted-foreground-subtle py-2">
          {t("agents.no_routing")}
        </p>
      ) : (
        <div className="space-y-3">
          {routing.map((rule, idx) => (
            <RoutingRuleRow
              key={idx}
              rule={rule}
              llmProviders={llmProviders}
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
