"use client";

import { useEffect, useState, useCallback, useRef } from "react";
import { apiGet, apiPost, apiPut } from "@/lib/api";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Settings, Gauge, Box, GitBranch, Keyboard, Loader2, RotateCcw, Save, Timer } from "lucide-react";
import { toast } from "sonner";

interface ConfigData {
  [key: string]: unknown;
}

interface SubagentsSection {
  enabled: boolean;
  default_mode: string;
  max_concurrent_in_process: number;
  max_concurrent_docker: number;
  docker_timeout: string;
}

/**
 * Walk a JSON Schema object and return the description at the given property path.
 * Returns undefined if the path does not exist or has no description.
 *
 * @param schema - Root schema object from GET /api/config/schema
 * @param path - Dot-separated field path as an array, e.g. ["toolgate_url"] or ["limits", "max_requests_per_minute"]
 */
function getFieldDescription(
  schema: Record<string, unknown> | null,
  path: string[]
): string | undefined {
  if (!schema) return undefined;
  let node: unknown = schema;
  for (const key of path) {
    if (typeof node !== "object" || node === null) return undefined;
    const obj = node as Record<string, unknown>;
    // Walk into .properties[key]
    const props = obj["properties"];
    if (typeof props !== "object" || props === null) return undefined;
    node = (props as Record<string, unknown>)[key];
  }
  if (typeof node !== "object" || node === null) return undefined;
  const desc = (node as Record<string, unknown>)["description"];
  return typeof desc === "string" ? desc : undefined;
}

export default function ConfigPage() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<ConfigData | null>(null);
  const [error, setError] = useState("");

  const [restarting, setRestarting] = useState(false);
  const [subagentsToggling, setSubagentsToggling] = useState(false);
  const [editPublicUrl, setEditPublicUrl] = useState("");
  const [editMaxReqPerMin, setEditMaxReqPerMin] = useState("");
  const [editMaxToolConcurrency, setEditMaxToolConcurrency] = useState("");
  const [editMaxAgentTurns, setEditMaxAgentTurns] = useState("");
  const [editEmbedDimensions, setEditEmbedDimensions] = useState("");
  // [agent_tool] — multi-agent timeouts
  const [editAgentToolWaitForIdle, setEditAgentToolWaitForIdle] = useState("");
  const [editAgentToolResult, setEditAgentToolResult] = useState("");
  const [editAgentToolSafety, setEditAgentToolSafety] = useState("");
  const [savingAgentTool, setSavingAgentTool] = useState(false);
  const [savingFields, setSavingFields] = useState(false);
  const [schema, setSchema] = useState<Record<string, unknown> | null>(null);

  const restartPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const restartTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mountedRef = useRef(true);

  const loadConfig = useCallback(() => {
    apiGet<ConfigData>("/api/config")
      .then((d) => {
        setConfig(d);
        setError("");
        setEditPublicUrl((d.public_url as string) || "");
        const limits = d.limits as Record<string, unknown> | undefined;
        setEditMaxReqPerMin(String(limits?.max_requests_per_minute ?? ""));
        setEditMaxToolConcurrency(String(limits?.max_tool_concurrency ?? ""));
        setEditMaxAgentTurns(String(limits?.max_agent_turns ?? ""));
        const memory = d.memory as Record<string, unknown> | undefined;
        setEditEmbedDimensions(String(memory?.embed_dimensions ?? ""));
        const agentTool = d.agent_tool as Record<string, unknown> | undefined;
        setEditAgentToolWaitForIdle(String(agentTool?.message_wait_for_idle_secs ?? ""));
        setEditAgentToolResult(String(agentTool?.message_result_secs ?? ""));
        setEditAgentToolSafety(String(agentTool?.safety_timeout_secs ?? ""));
      })
      .catch((e) => setError(`${e}`));
  }, []);

  useEffect(() => { loadConfig(); }, [loadConfig]);

  useEffect(() => {
    apiGet<Record<string, unknown>>("/api/config/schema")
      .then(setSchema)
      .catch(() => {
        // Schema hints are non-critical — degrade gracefully if endpoint unavailable
      });
  }, []); // empty dep array: fetch once on mount

  const subagents = config?.subagents as SubagentsSection | undefined;
  const toggleSubagents = async (enabled: boolean) => {
    setSubagentsToggling(true);
    try {
      await apiPut("/api/config", { subagents_enabled: enabled });
      toast.success(enabled ? t("config.subagents_on") : t("config.subagents_off"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSubagentsToggling(false);
    }
  };

  const saveAgentToolFields = async () => {
    setSavingAgentTool(true);
    try {
      const payload: Record<string, unknown> = {};
      if (editAgentToolWaitForIdle.trim()) {
        payload.agent_tool_message_wait_for_idle_secs = Number(editAgentToolWaitForIdle);
      }
      if (editAgentToolResult.trim()) {
        payload.agent_tool_message_result_secs = Number(editAgentToolResult);
      }
      if (editAgentToolSafety.trim()) {
        payload.agent_tool_safety_timeout_secs = Number(editAgentToolSafety);
      }
      await apiPut("/api/config", payload);
      toast.success(t("config.saved"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSavingAgentTool(false);
    }
  };

  const saveEditableFields = async () => {
    setSavingFields(true);
    try {
      const payload: Record<string, unknown> = {};
      if (editPublicUrl.trim()) payload.public_url = editPublicUrl.trim();
      if (editMaxReqPerMin.trim()) payload.max_requests_per_minute = Number(editMaxReqPerMin);
      if (editMaxToolConcurrency.trim()) payload.max_tool_concurrency = Number(editMaxToolConcurrency);
      if (editMaxAgentTurns.trim()) payload.max_agent_turns = Number(editMaxAgentTurns);
      if (editEmbedDimensions.trim()) payload.embed_dimensions = Number(editEmbedDimensions);
      await apiPut("/api/config", payload);
      toast.success(t("config.saved"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSavingFields(false);
    }
  };

  // Cleanup restart polling on unmount
  useEffect(() => () => {
    mountedRef.current = false;
    if (restartPollRef.current) clearInterval(restartPollRef.current);
    if (restartTimeoutRef.current) clearTimeout(restartTimeoutRef.current);
  }, []);

  const restartCore = async () => {
    setRestarting(true);
    try {
      await apiPost("/api/restart", undefined, { "X-Confirm-Restart": "true" });
      toast.success(t("config.core_restarting"));
      // Poll until core comes back
      const poll = setInterval(async () => {
        if (!mountedRef.current) return;
        try {
          await apiGet("/health");
          clearInterval(poll);
          restartPollRef.current = null;
          if (restartTimeoutRef.current) { clearTimeout(restartTimeoutRef.current); restartTimeoutRef.current = null; }
          if (!mountedRef.current) return;
          setRestarting(false);
          loadConfig();
          toast.success(t("config.core_restarted"));
        } catch { /* still restarting */ }
      }, 2000);
      restartPollRef.current = poll;
      // Timeout after 60s
      restartTimeoutRef.current = setTimeout(() => { clearInterval(poll); restartPollRef.current = null; setRestarting(false); }, 60000);
    } catch (e) {
      toast.error(`${e}`);
      setRestarting(false);
    }
  };

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mb-8 md:mb-10">
          <div className="flex flex-col md:flex-row md:items-center justify-between gap-4">
            <div className="flex flex-col gap-1">
              <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("config.title")}</h2>
              <span className="text-sm text-muted-foreground">
                {t("config.subtitle")}
              </span>
            </div>
            <Button
              variant="destructive"
              onClick={restartCore}
              disabled={restarting}
              className="w-full md:w-auto shrink-0"
            >
              {restarting ? <Loader2 className="h-4 w-4 animate-spin" /> : <RotateCcw className="h-4 w-4" />}
              {t("config.restart_core")}
            </Button>
          </div>
        </div>

        {error && <ErrorBanner error={error} />}

        {!config && !error && (
          <div className="space-y-6">
            {[1, 2, 3].map((i) => (
              <div key={i} className="h-40 rounded-xl border border-border bg-muted/20 animate-pulse" />
            ))}
          </div>
        )}

        {config && (() => {
          const serviceKeys = new Set(["toolgate_url", "public_url", "tts_proxy_url", "searxng_url", "tools", "mcp", "tools_count", "mcp_count", "memory", "subagents", "database", "gateway", "discussion", "typing", "agent_tool"]);
          const sections: Record<string, Record<string, unknown>> = {};
          const topLevel: Record<string, unknown> = {};
          for (const [key, val] of Object.entries(config)) {
            if (serviceKeys.has(key)) continue;
            if (val !== null && typeof val === "object" && !Array.isArray(val)) {
              sections[key] = val as Record<string, unknown>;
            } else {
              topLevel[key] = val;
            }
          }
          return (
            <div className="space-y-6">
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                {subagents && (
                  <div className="neu-flat p-4 md:p-5">
                    <div className="mb-4 flex items-center gap-2 border-b border-border/50 pb-3">
                      <GitBranch className="h-4 w-4 text-primary" />
                      <h3 className="text-sm font-semibold text-foreground">{t("config.subagents")}</h3>
                      <div className="ml-auto flex items-center gap-2">
                        <Badge
                          variant={subagents.enabled ? "default" : "secondary"}
                          className={`text-xs ${subagents.enabled ? "bg-success/20 text-success border-success/30" : "bg-muted text-muted-foreground border-border"}`}
                        >
                          {subagents.enabled ? t("config.subagents_enabled") : t("config.subagents_disabled")}
                        </Badge>
                        <Switch
                          checked={subagents.enabled}
                          onCheckedChange={(v) => toggleSubagents(v)}
                          disabled={subagentsToggling}
                        />
                      </div>
                    </div>
                    <div className="space-y-1.5">
                      {Object.entries(subagents).filter(([k]) => k !== "enabled").map(([key, val]) => (
                        <div
                          key={key}
                          className="flex items-center justify-between gap-2 border-b border-border/20 py-1.5 last:border-0"
                        >
                          <span className="font-mono text-xs text-muted-foreground">{key}</span>
                          <div className="shrink-0">
                            {renderValue(val, t)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                )}
                <div className="neu-flat p-4 md:p-5">
                    <div className="mb-4 flex items-center gap-2 border-b border-border/50 pb-3">
                      <Gauge className="h-4 w-4 text-primary" />
                      <h3 className="text-sm font-semibold text-foreground">{t("config.editable_fields")}</h3>
                    </div>
                    <div className="space-y-4">
                      <div className="space-y-1.5">
                        <label className="font-mono text-xs text-muted-foreground">public_url</label>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Input
                              value={editPublicUrl}
                              onChange={(e) => setEditPublicUrl(e.target.value)}
                              placeholder="https://example.com"
                              className="font-mono text-sm h-9"
                            />
                          </TooltipTrigger>
                          {(() => { const d = getFieldDescription(schema, ["gateway", "public_url"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                        </Tooltip>
                      </div>
                      <div className="space-y-1.5">
                        <label className="font-mono text-xs text-muted-foreground">max_requests_per_minute</label>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Input
                              type="number"
                              min={1}
                              value={editMaxReqPerMin}
                              onChange={(e) => setEditMaxReqPerMin(e.target.value)}
                              placeholder="60"
                              className="font-mono text-sm h-9"
                            />
                          </TooltipTrigger>
                          {(() => { const d = getFieldDescription(schema, ["limits", "max_requests_per_minute"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                        </Tooltip>
                      </div>
                      <div className="space-y-1.5">
                        <label className="font-mono text-xs text-muted-foreground">max_tool_concurrency</label>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Input
                              type="number"
                              min={1}
                              value={editMaxToolConcurrency}
                              onChange={(e) => setEditMaxToolConcurrency(e.target.value)}
                              placeholder="4"
                              className="font-mono text-sm h-9"
                            />
                          </TooltipTrigger>
                          {(() => { const d = getFieldDescription(schema, ["limits", "max_tool_concurrency"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                        </Tooltip>
                      </div>
                      <div className="space-y-1.5">
                        <label className="font-mono text-xs text-muted-foreground">max_agent_turns</label>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Input
                              type="number"
                              value={editMaxAgentTurns}
                              onChange={(e) => setEditMaxAgentTurns(e.target.value)}
                              placeholder="5"
                              className="font-mono text-sm h-9"
                              min={1}
                              max={50}
                            />
                          </TooltipTrigger>
                          {(() => { const d = getFieldDescription(schema, ["limits", "max_agent_turns"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                        </Tooltip>
                        <p className="text-xs text-muted-foreground/60">
                          {t("config.max_agent_turns_description")}
                        </p>
                      </div>
                      <div className="space-y-1.5">
                        <label className="font-mono text-xs text-muted-foreground">embed_dimensions</label>
                        <Input
                          type="number"
                          value={editEmbedDimensions}
                          onChange={(e) => setEditEmbedDimensions(e.target.value)}
                          placeholder={t("config.embed_dimensions_placeholder")}
                          className="font-mono text-sm h-9"
                          min={0}
                          max={8192}
                        />
                        <p className="text-xs text-muted-foreground/60">
                          {t("config.embed_dimensions_description")}
                        </p>
                        <p className="text-[11px] text-muted-foreground/40 leading-relaxed">
                          {t("config.embed_dimensions_hint")}
                        </p>
                      </div>
                      <Button
                        size="sm"
                        onClick={saveEditableFields}
                        disabled={savingFields}
                        className="gap-1.5"
                      >
                        {savingFields ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                        {t("common.save")}
                      </Button>
                    </div>
                  </div>
                <div className="neu-flat p-4 md:p-5">
                  <div className="mb-4 flex items-center gap-2 border-b border-border/50 pb-3">
                    <Timer className="h-4 w-4 text-primary" />
                    <h3 className="text-sm font-semibold text-foreground">{t("config.agent_tool.title")}</h3>
                  </div>
                  <div className="space-y-4">
                    <p className="text-xs text-muted-foreground/70 leading-relaxed">
                      {t("config.agent_tool.description")}
                    </p>
                    <div className="space-y-1.5">
                      <label className="font-mono text-xs text-muted-foreground">message_wait_for_idle_secs</label>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Input
                            type="number"
                            min={1}
                            max={3600}
                            value={editAgentToolWaitForIdle}
                            onChange={(e) => setEditAgentToolWaitForIdle(e.target.value)}
                            placeholder="60"
                            className="font-mono text-sm h-9"
                          />
                        </TooltipTrigger>
                        {(() => { const d = getFieldDescription(schema, ["agent_tool", "message_wait_for_idle_secs"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                      </Tooltip>
                      <p className="text-xs text-muted-foreground/60">
                        {t("config.agent_tool.message_wait_for_idle")}
                      </p>
                    </div>
                    <div className="space-y-1.5">
                      <label className="font-mono text-xs text-muted-foreground">message_result_secs</label>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Input
                            type="number"
                            min={1}
                            max={3600}
                            value={editAgentToolResult}
                            onChange={(e) => setEditAgentToolResult(e.target.value)}
                            placeholder="300"
                            className="font-mono text-sm h-9"
                          />
                        </TooltipTrigger>
                        {(() => { const d = getFieldDescription(schema, ["agent_tool", "message_result_secs"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                      </Tooltip>
                      <p className="text-xs text-muted-foreground/60">
                        {t("config.agent_tool.message_result")}
                      </p>
                    </div>
                    <div className="space-y-1.5">
                      <label className="font-mono text-xs text-muted-foreground">safety_timeout_secs</label>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Input
                            type="number"
                            min={1}
                            max={3600}
                            value={editAgentToolSafety}
                            onChange={(e) => setEditAgentToolSafety(e.target.value)}
                            placeholder="600"
                            className="font-mono text-sm h-9"
                          />
                        </TooltipTrigger>
                        {(() => { const d = getFieldDescription(schema, ["agent_tool", "safety_timeout_secs"]); return d ? <TooltipContent>{d}</TooltipContent> : null; })()}
                      </Tooltip>
                      <p className="text-xs text-muted-foreground/60">
                        {t("config.agent_tool.safety_timeout")}
                      </p>
                    </div>
                    <Button
                      size="sm"
                      onClick={saveAgentToolFields}
                      disabled={savingAgentTool}
                      className="gap-1.5"
                    >
                      {savingAgentTool ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                      {t("common.save")}
                    </Button>
                  </div>
                </div>
                {Object.keys(topLevel).length > 0 && (
                  <div className="neu-flat p-4 md:p-5">
                    <div className="mb-4 flex items-center gap-2 border-b border-border/50 pb-3">
                      <Settings className="h-4 w-4 text-foreground/70" />
                      <h3 className="text-sm font-semibold text-foreground">{t("config.section_general")}</h3>
                    </div>
                    <div className="space-y-1.5">
                      {Object.entries(topLevel).map(([key, val]) => (
                        <div
                          key={key}
                          className="flex items-center justify-between gap-2 border-b border-border/20 py-1.5 last:border-0"
                        >
                          <span className="font-mono text-xs text-muted-foreground truncate">{key}</span>
                          <div className="shrink-0">
                            {renderValue(val, t)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {Object.entries(sections).map(([section, values]) => {
                  const sectionIcons: Record<string, React.ReactNode> = {
                    limits: <Gauge className="h-4 w-4 text-primary" />,
                    sandbox: <Box className="h-4 w-4 text-primary" />,
                    subagents: <GitBranch className="h-4 w-4 text-primary" />,
                    typing: <Keyboard className="h-4 w-4 text-primary" />,
                  };
                  return (
                  <div key={section} className="neu-flat p-4 md:p-5">
                    <div className="mb-4 flex items-center gap-2 border-b border-border/50 pb-3">
                      {sectionIcons[section] ?? <Settings className="h-4 w-4 text-primary" />}
                      <h3 className="text-sm font-semibold text-foreground">{section}</h3>
                    </div>
                    <div className="space-y-1.5">
                      {Object.entries(values).map(([key, val]) => (
                        <div
                          key={key}
                          className="flex items-start justify-between gap-2 border-b border-border/20 py-1.5 last:border-0"
                        >
                          <span className="font-mono text-xs text-muted-foreground pt-0.5 truncate">{key}</span>
                          <div className="shrink-0 max-w-[60%] flex justify-end overflow-x-auto scrollbar-none">
                            {renderValue(val, t)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                  ); })}
              </div>
            </div>
          );
        })()}
    </div>
  );
}

function renderValue(val: unknown, t: (key: TranslationKey, values?: Record<string, string | number>) => string): React.ReactNode {
  if (val === null || val === undefined) {
    return <span className="text-sm text-muted-foreground/60 italic">{t("config.value_null")}</span>;
  }
  if (typeof val === "boolean") {
    return (
      <Badge variant={val ? "default" : "secondary"} className={`text-xs ${val ? 'bg-success/20 text-success border-success/30' : 'bg-muted text-muted-foreground border-border'}`}>
        {val ? t("config.value_on") : t("config.value_off")}
      </Badge>
    );
  }
  if (typeof val === "number") {
    return <span className="font-mono text-sm font-bold text-primary tabular-nums">{val}</span>;
  }
  if (Array.isArray(val)) {
    const hasObjects = val.some((v) => v !== null && typeof v === "object");
    if (hasObjects) {
      return (
        <pre className="max-w-full overflow-x-auto whitespace-pre-wrap font-mono text-sm text-foreground/80 bg-muted/30 p-3 rounded-lg border border-border">
          {JSON.stringify(val, null, 2)}
        </pre>
      );
    }
    return (
      <div className="flex flex-wrap sm:justify-end gap-1.5 max-w-full">
        {val.map((v, i) => (
          <Badge key={i} variant="outline" className="font-mono text-xs border-primary/20 text-foreground/80 bg-primary/5 whitespace-normal break-all leading-relaxed">
            {String(v)}
          </Badge>
        ))}
      </div>
    );
  }
  if (val && typeof val === "object") {
    return (
      <pre className="max-w-full overflow-x-auto whitespace-pre-wrap font-mono text-sm text-foreground/80 bg-muted/30 p-3 rounded-lg border border-border">
        {JSON.stringify(val, null, 2)}
      </pre>
    );
  }
  return (
    <span className="max-w-full break-all font-mono text-sm text-foreground/90">{String(val)}</span>
  );
}
