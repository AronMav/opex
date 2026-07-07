"use client";

import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Field } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { getToken } from "@/lib/api";
import { useAgents } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import type { ConfigFieldDescriptor } from "./handler-descriptor";

/**
 * Per-agent operator settings ("valves") for a file handler. The FIELD
 * definitions come from the handler's `<config>` descriptor block (passed in);
 * the VALUES are loaded/saved per agent via `/api/handlers/{id}/config?agent=`.
 */
export function HandlerConfigForm({
  handlerId,
  fields,
}: {
  handlerId: string;
  fields: ConfigFieldDescriptor[];
}) {
  const { t } = useTranslation();
  const { data: agentsData } = useAgents();
  const agents = agentsData ?? [];

  const [agent, setAgent] = useState<string>("");
  const [values, setValues] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Default the selector to the first agent once the list loads.
  useEffect(() => {
    if (!agent && agents.length > 0) setAgent(agents[0].name);
  }, [agents, agent]);

  // Load saved values for the selected agent (falling back to field defaults).
  useEffect(() => {
    if (!agent) return;
    let cancelled = false;
    setLoading(true);
    setSaved(false);
    setError(null);
    fetch(
      `/api/handlers/${encodeURIComponent(handlerId)}/config?agent=${encodeURIComponent(agent)}`,
      { headers: { Authorization: `Bearer ${getToken()}` } },
    )
      .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
      .then((body: { values?: Record<string, unknown> }) => {
        if (cancelled) return;
        const stored = body.values ?? {};
        const next: Record<string, string> = {};
        for (const f of fields) {
          const v = stored[f.name];
          next[f.name] =
            v != null ? String(v) : f.default != null ? String(f.default) : "";
        }
        setValues(next);
      })
      .catch(() => {
        if (!cancelled) setError(t("tools.handler_config_load_error"));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [agent, handlerId, fields, t]);

  async function save() {
    if (!agent) return;
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      const res = await fetch(
        `/api/handlers/${encodeURIComponent(handlerId)}/config?agent=${encodeURIComponent(agent)}`,
        {
          method: "PUT",
          headers: {
            "Content-Type": "application/json",
            Authorization: `Bearer ${getToken()}`,
          },
          body: JSON.stringify({ values }),
        },
      );
      if (!res.ok) {
        setError(`HTTP ${res.status}`);
        return;
      }
      setSaved(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  if (fields.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("tools.handler_config_none")}
      </p>
    );
  }

  return (
    <div className="space-y-4">
      <p className="text-xs text-muted-foreground">{t("tools.handler_config_hint")}</p>

      <Field label={t("tools.handler_config_agent")}>
        <Select value={agent} onValueChange={setAgent}>
          <SelectTrigger>
            <SelectValue placeholder="—" />
          </SelectTrigger>
          <SelectContent>
            {agents.map((a) => (
              <SelectItem key={a.name} value={a.name}>
                {a.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>

      <div className="grid grid-cols-1 gap-4">
        {fields.map((f) =>
          f.type === "bool" ? (
            <Field key={f.name} label={f.label || f.name} hint={f.description}>
              <Switch
                checked={values[f.name] === "true"}
                onCheckedChange={(v) =>
                  setValues((s) => ({ ...s, [f.name]: v ? "true" : "false" }))
                }
              />
            </Field>
          ) : (
            <Field key={f.name} label={f.label || f.name} hint={f.description}>
              <Input
                type={f.type === "int" || f.type === "number" ? "number" : "text"}
                value={values[f.name] ?? ""}
                placeholder={f.default != null ? String(f.default) : ""}
                disabled={loading}
                onChange={(e) =>
                  setValues((s) => ({ ...s, [f.name]: e.target.value }))
                }
              />
            </Field>
          ),
        )}
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}

      <Button onClick={save} disabled={saving || loading || !agent}>
        {saving
          ? t("tools.handler_config_saving")
          : saved
            ? t("tools.handler_config_saved")
            : t("tools.handler_config_save")}
      </Button>
    </div>
  );
}
