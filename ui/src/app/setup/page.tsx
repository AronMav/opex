"use client";

import { useState, useEffect, useRef, useId } from "react";
import { apiPost, apiGet, apiPut, apiDelete } from "@/lib/api";
import { toast } from "sonner";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Alert } from "@/components/ui/alert";
import { Stepper } from "@/components/ui/stepper";
import { AuthShell, AuthBrand } from "@/components/ui/auth-shell";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import {
  Key,
  User,
  MessageSquare,
  ArrowRight,
  Check,
  Loader2,
  Wifi,
  RefreshCw,
  ShieldCheck,
  CheckCircle2,
  AlertTriangle,
  XCircle,
} from "lucide-react";

// ── Provider type from API ────────────────────────────────────────────────

interface ProviderTypeInfo {
  id: string;
  name: string;
  default_base_url?: string;
  requires_api_key?: boolean;
  default_secret_name?: string;
}

// ── Requirements check types ──────────────────────────────────────────────

interface RequirementCheck {
  status: "ok" | "warn" | "error";
  message: string;
  fix_hint?: string | null;
}

interface CliToolCheck {
  name: string;
  status: "ok" | "not_found";
  version?: string;
  path?: string;
}

interface RequirementsResult {
  ok: boolean;
  checks: {
    docker: RequirementCheck;
    postgresql: RequirementCheck;
    disk_space: RequirementCheck;
  };
  cli_tools?: CliToolCheck[];
}

// ── Fallback popular models per provider_type ─────────────────────────────

const FALLBACK_MODELS: Record<string, string[]> = {
  minimax: ["MiniMax-M2.5", "MiniMax-M1"],
  anthropic: ["claude-sonnet-4-20250514", "claude-haiku-4-5-20251001", "claude-opus-4-20250514"],
  google: ["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.0-flash"],
  openai: ["gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano", "o4-mini", "o3"],
  deepseek: ["deepseek-chat", "deepseek-reasoner"],
  groq: ["llama-3.3-70b-versatile", "llama-3.1-8b-instant"],
  together: ["meta-llama/Llama-3.3-70B-Instruct-Turbo", "Qwen/Qwen2.5-72B-Instruct-Turbo"],
  openrouter: ["anthropic/claude-sonnet-4", "openai/gpt-4.1", "google/gemini-2.5-pro"],
  mistral: ["mistral-large-latest", "mistral-small-latest"],
  xai: ["grok-3", "grok-3-mini"],
  perplexity: ["sonar-pro", "sonar"],
  ollama: ["llama3.3", "qwen3", "gemma3"],
};

const LANGUAGES = [
  { value: "ru", label: "Русский" },
  { value: "en", label: "English" },
  { value: "es", label: "Español" },
  { value: "de", label: "Deutsch" },
  { value: "fr", label: "Français" },
  { value: "zh", label: "中文" },
  { value: "ja", label: "日本語" },
] as const;

interface NetworkAddresses {
  wan: { ip: string | null; is_cgnat: boolean | null; cgnat_warning: string | null } | null;
  tailscale: { connected: boolean; backend_state: string; ips: string[]; dns_name: string } | null;
  lan: { interface: string; ip: string; is_ipv6: boolean }[];
  mdns: { hostname: string } | null;
}

// ── localStorage key ──────────────────────────────────────────────────────

const WIZARD_STORAGE_KEY = "opex_wizard_progress";

type Step = "requirements" | "provider" | "agent" | "channel";

const STEPS: { key: Step; labelKey: TranslationKey; icon: typeof Key }[] = [
  { key: "requirements", labelKey: "setup.step_requirements", icon: ShieldCheck },
  { key: "provider", labelKey: "setup.step_provider", icon: Key },
  { key: "agent", labelKey: "setup.step_agent", icon: User },
  { key: "channel", labelKey: "setup.step_channel", icon: MessageSquare },
];

export default function SetupPage() {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>("requirements");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  // ── Stable ids for manually-associated (compound) fields ──────────────
  const providerTypeId = useId();
  const baseUrlId = useId();
  const modelId = useId();
  const langId = useId();

  // ── Provider types from API ───────────────────────────────────────────
  const [providerTypes, setProviderTypes] = useState<ProviderTypeInfo[]>([]);
  useEffect(() => {
    apiGet<{ provider_types: ProviderTypeInfo[] }>("/api/provider-types")
      .then((data) => setProviderTypes(data.provider_types ?? []))
      .catch(() => { /* fallback: manual input */ });
  }, []);

  // ── Step 0: Requirements ──────────────────────────────────────────────
  const [requirements, setRequirements] = useState<RequirementsResult | null>(null);
  const [requirementsLoading, setRequirementsLoading] = useState(false);
  const [detectedClis, setDetectedClis] = useState<string[]>([]);

  useEffect(() => {
    if (step !== "requirements") return;
    setRequirementsLoading(true);
    apiGet<RequirementsResult>("/api/setup/requirements")
      .then((data) => {
        setRequirements(data);
        const detected = (data.cli_tools ?? [])
          .filter((t) => t.status === "ok")
          .map((t) => t.name);
        setDetectedClis(detected);
      })
      .catch(() => setRequirements(null))
      .finally(() => setRequirementsLoading(false));
  }, [step]);

  // ── Step 1: Provider ──────────────────────────────────────────────────
  const [providerType, setProviderType] = useState("");
  const [apiKeyValue, setApiKeyValue] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [defaultModel, setDefaultModel] = useState("");
  // `providerName` was tracked here for a flow that no longer reads it; we
  // still call the API and treat the response, but the local mirror is
  // dead state.
  const [discoveredModels, setDiscoveredModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [testCallStatus, setTestCallStatus] = useState<"idle" | "testing" | "ok" | "fail">("idle");

  // ── Step 2: Agent ─────────────────────────────────────────────────────
  const [agentName, setAgentName] = useState("");
  const [agentLang, setAgentLang] = useState("ru");

  // ── Step 3: Channel ───────────────────────────────────────────────────
  const [botToken, setBotToken] = useState("");
  const [skipChannel, setSkipChannel] = useState(false);

  // ── Network info ──────────────────────────────────────────────────────
  const [networkData, setNetworkData] = useState<NetworkAddresses | undefined>(undefined);
  useEffect(() => {
    apiGet<NetworkAddresses>("/api/network/addresses")
      .then(setNetworkData)
      .catch(() => { });
  }, []);

  // ── localStorage: restore on mount ───────────────────────────────────
  const [restored, setRestored] = useState(false);
  useEffect(() => {
    try {
      const saved = localStorage.getItem(WIZARD_STORAGE_KEY);
      if (saved) {
        const p = JSON.parse(saved) as {
          step?: Step;
          providerType?: string;
          defaultModel?: string;
          baseUrl?: string;
          agentName?: string;
          agentLang?: string;
        };
        if (p.step) setStep(p.step);
        if (p.providerType) setProviderType(p.providerType);
        if (p.defaultModel) setDefaultModel(p.defaultModel);
        if (p.baseUrl) setBaseUrl(p.baseUrl);
        if (p.agentName) setAgentName(p.agentName);
        if (p.agentLang) setAgentLang(p.agentLang);
      }
    } catch { /* ignore parse errors */ }
    setRestored(true);
  }, []);

  // ── localStorage: save on every change (only after restore) ─────────
  useEffect(() => {
    if (!restored) return;
    try {
      localStorage.setItem(WIZARD_STORAGE_KEY, JSON.stringify({
        step, providerType, defaultModel, baseUrl, agentName, agentLang,
      }));
    } catch { /* ignore */ }
  }, [restored, step, providerType, defaultModel, baseUrl, agentName, agentLang]);

  const currentIdx = STEPS.findIndex((s) => s.key === step);

  // ── Provider type helpers ─────────────────────────────────────────────

  const selectedTypeInfo = providerTypes.find((pt) => pt.id === providerType);
  const fallbackModels = FALLBACK_MODELS[providerType] ?? [];
  const modelOptions = discoveredModels.length > 0 ? discoveredModels : fallbackModels;
  void modelOptions; // used via JSX chips below

  const handleProviderTypeChange = (v: string) => {
    setProviderType(v);
    setApiKeyValue("");
    setDefaultModel("");
    setDiscoveredModels([]);
    discoverGenRef.current++; // invalidate in-flight discover
    const info = providerTypes.find((pt) => pt.id === v);
    if (info?.default_base_url) setBaseUrl(info.default_base_url);
    else setBaseUrl("");
    const fb = FALLBACK_MODELS[v];
    if (fb && fb.length > 0) setDefaultModel(fb[0]);
  };

  const providerNameRef = useRef("");
  const discoverGenRef = useRef(0);
  const discoverModels = async () => {
    if (!providerType) return;
    const gen = ++discoverGenRef.current;
    setModelsLoading(true);
    try {
      const bUrl = baseUrl || undefined;
      const url = `/api/providers/${providerType}/models${bUrl ? `?base_url=${encodeURIComponent(bUrl)}` : ""}`;
      const data = await apiGet<{ models: { id: string }[] | string[] }>(url);
      if (gen !== discoverGenRef.current) return; // stale response
      const ids = data.models.map((m) => typeof m === "string" ? m : m.id);
      setDiscoveredModels(ids);
      if (ids.length > 0 && !defaultModel) setDefaultModel(ids[0]);
    } catch {
      // fallback to hardcoded models
    }
    if (gen === discoverGenRef.current) setModelsLoading(false);
  };

  // ── Step handlers ─────────────────────────────────────────────────────

  const doStep1 = async () => {
    if (!providerType || !defaultModel.trim()) return;
    setLoading(true);
    setError("");
    setTestCallStatus("idle");
    try {
      const name = `${providerType}-default`;
      const created = await apiPost<{ id: string; name: string }>("/api/providers", {
        name,
        type: "text",
        provider_type: providerType,
        default_model: defaultModel.trim(),
        api_key: apiKeyValue.trim() || undefined,
        base_url: baseUrl.trim() || undefined,
        enabled: true,
      });
      providerNameRef.current = created.name ?? name;

      // WIZ-02: validate key with test call
      setTestCallStatus("testing");
      try {
        const modelsData = await apiGet<{ models: Array<{ id: string } | string> }>(
          `/api/providers/${created.id}/models`
        );
        const modelList = modelsData.models ?? [];
        if (modelList.length === 0 && (selectedTypeInfo?.requires_api_key !== false)) {
          // No models returned and key is required — likely invalid key
          // Clean up orphaned provider record
          await apiDelete(`/api/providers/${created.id}`).catch(() => {
            toast.error("Warning: could not clean up test provider. You may need to remove it manually in Providers page.");
          });
          setTestCallStatus("fail");
          setError(t("setup.provider_test_fail"));
          setLoading(false);
          return; // do NOT advance to agent step
        }
        // If models returned, use first as defaultModel if none set
        if (modelList.length > 0 && !defaultModel.trim()) {
          const first = modelList[0];
          setDefaultModel(typeof first === "string" ? first : first.id);
        }
      } catch {
        // Test call failed (network error, auth error, etc.)
        // Clean up orphaned provider record
        await apiDelete(`/api/providers/${created.id}`).catch(() => {
          toast.error("Warning: could not clean up test provider. You may need to remove it manually in Providers page.");
        });
        setTestCallStatus("fail");
        setError(t("setup.provider_test_fail"));
        setLoading(false);
        return;
      }

      setTestCallStatus("ok");

      // profiles: seed the Default profile's text slot with the created provider
      try {
        const { profiles } = await apiGet<{ profiles: Array<{ id: string; name: string; slots: Record<string, unknown> }> }>("/api/profiles");
        const def = profiles.find((p) => p.name === "Default");
        if (def) {
          await apiPut(`/api/profiles/${def.id}`, {
            slots: { ...def.slots, text: [{ provider: providerNameRef.current, model: defaultModel.trim() }] },
          });
        }
      } catch { /* seed migration will create/fix Default on next startup; don't block the wizard */ }

      setStep("agent");
    } catch (e) {
      setError(`${e}`);
    } finally {
      setLoading(false);
    }
  };

  const doStep2 = async () => {
    if (!agentName.trim()) return;
    setLoading(true);
    setError("");
    try {
      await apiPost("/api/agents", {
        name: agentName.trim(),
        language: agentLang,
        profile: "Default",
        temperature: 1.0,
      });
      setStep("channel");
    } catch (e) {
      setError(`${e}`);
    }
    setLoading(false);
  };

  // Step 3: Channel
  const [botDisplayName, setBotDisplayName] = useState("");

  const finishSetup = async () => {
    // Mark setup as complete in DB — prevents redirect loop
    await apiPost("/api/setup/complete", {}).catch(() => { });
    localStorage.removeItem(WIZARD_STORAGE_KEY);
    // Use window.location instead of router.replace to avoid React suspense
    // boundary error (#185) when transitioning from unauthenticated to authenticated layout
    window.location.href = "/chat";
  };

  const doStep3 = async () => {
    if (skipChannel || !botToken.trim()) {
      await finishSetup();
      return;
    }
    setLoading(true);
    setError("");
    try {
      // Channel credentials are stored in vault automatically by the backend
      await apiPost(`/api/agents/${agentName.trim()}/channels`, {
        channel_type: "telegram",
        display_name: botDisplayName.trim() || `${agentName} Telegram`,
        config: { bot_token: botToken.trim() },
      });
      await finishSetup();
    } catch (e) {
      setError(`${e}`);
    }
    setLoading(false);
  };

  // ── Step 1 form validation ────────────────────────────────────────────
  const step1Valid = providerType && defaultModel.trim();

  // ── Requirements step helpers ─────────────────────────────────────────
  const reqChecks = requirements?.checks;
  const allChecks = reqChecks
    ? [
      { key: "docker", label: t("setup.req_docker"), check: reqChecks.docker },
      { key: "postgresql", label: t("setup.req_postgresql"), check: reqChecks.postgresql },
      { key: "disk_space", label: t("setup.req_disk_space"), check: reqChecks.disk_space },
    ]
    : [];

  const hasError = allChecks.some((c) => c.check.status === "error");
  const hasWarn = allChecks.some((c) => c.check.status === "warn");

  return (
    <AuthShell className="max-w-lg">
      {/* Logo */}
      <AuthBrand orientation="horizontal" className="mb-8" />

      {/* Step indicators */}
      <Stepper steps={STEPS} currentIndex={currentIdx} className="mb-8" />

      {/* Card */}
      <Card className="p-6">
        <h2 className="text-lg font-bold mb-1">{t(STEPS[currentIdx].labelKey)}</h2>

        {error && (
          <Alert variant="destructive" className="mt-3">
            {error}
          </Alert>
        )}

          {/* ── Step 0: Requirements ──────────────────────────────── */}
          {step === "requirements" && (
            <div className="mt-4 space-y-4">
              {requirementsLoading && (
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  {t("setup.requirements_title")}
                </div>
              )}

              {!requirementsLoading && allChecks.length > 0 && (
                <div className="space-y-2">
                  {allChecks.map(({ key, label, check }) => (
                    <div
                      key={key}
                      className="flex items-start gap-3 rounded-lg neu-inset p-3"
                    >
                      <div className="mt-0.5 shrink-0">
                        {check.status === "ok" && (
                          <CheckCircle2 className="h-4 w-4 text-success" />
                        )}
                        {check.status === "warn" && (
                          <AlertTriangle className="h-4 w-4 text-warning" />
                        )}
                        {check.status === "error" && (
                          <XCircle className="h-4 w-4 text-destructive" />
                        )}
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium">{label}</p>
                        <p className="text-xs text-muted-foreground mt-0.5">{check.message}</p>
                        {check.fix_hint && (
                          <p className="text-xs text-warning mt-1 font-mono">
                            {check.fix_hint}
                          </p>
                        )}
                      </div>
                    </div>
                  ))}

                  {/* Summary banner */}
                  {!hasError && !hasWarn && (
                    <Alert variant="success">{t("setup.requirements_pass")}</Alert>
                  )}
                  {!hasError && hasWarn && (
                    <Alert variant="warning">{t("setup.requirements_warn")}</Alert>
                  )}
                  {hasError && (
                    <Alert variant="destructive">{t("setup.requirements_fail")}</Alert>
                  )}

                  {/* CLI Tools detection */}
                  {requirements?.cli_tools && requirements.cli_tools.length > 0 && (
                    <div className="space-y-2">
                      <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
                        {t("setup.cli_tools_title")}
                      </p>
                      {requirements.cli_tools.map((tool) => (
                        <div
                          key={tool.name}
                          className={`flex items-start gap-3 rounded-lg neu-inset p-3 ${tool.status !== "ok" ? "opacity-50" : ""
                            }`}
                        >
                          <div className="mt-0.5 shrink-0">
                            {tool.status === "ok" ? (
                              <CheckCircle2 className="h-4 w-4 text-success" />
                            ) : (
                              <XCircle className="h-4 w-4 text-muted-foreground" />
                            )}
                          </div>
                          <div className="flex-1 min-w-0">
                            <p className="text-sm font-medium">{tool.name}</p>
                            <p className="text-xs text-muted-foreground mt-0.5">
                              {tool.status === "ok"
                                ? `${tool.version ? `v${tool.version}` : ""} ${tool.path ? `— ${tool.path}` : ""}`.trim()
                                : t("setup.cli_not_installed")}
                            </p>
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}

              {!requirementsLoading && allChecks.length === 0 && requirements === null && (
                <p className="text-sm text-muted-foreground">{t("setup.req_checking")}</p>
              )}

              <div className="flex gap-3">
                {hasError ? (
                  <>
                    <Button disabled className="flex-1">
                      <ArrowRight className="h-4 w-4 mr-2" />
                      {t("common.next")}
                    </Button>
                    <Button
                      variant="ghost"
                      onClick={() => setStep("provider")}
                      disabled={requirementsLoading}
                    >
                      {t("setup.req_proceed_anyway")}
                    </Button>
                  </>
                ) : (
                  <Button
                    onClick={() => setStep("provider")}
                    disabled={requirementsLoading}
                    className="w-full"
                  >
                    <ArrowRight className="h-4 w-4 mr-2" />
                    {t("common.next")}
                  </Button>
                )}
              </div>
            </div>
          )}

          {/* ── Step 1: Provider ──────────────────────────────────── */}
          {step === "provider" && (
            <div className="mt-4 space-y-4">
              <p className="text-sm text-muted-foreground">
                {t("setup.enter_llm_api_key")}
              </p>

              {/* Provider Type */}
              <div className="space-y-2">
                <label htmlFor={providerTypeId} className="text-sm font-medium text-muted-foreground">{t("setup.provider")}</label>
                {providerTypes.length > 0 ? (
                  <Select value={providerType} onValueChange={handleProviderTypeChange}>
                    <SelectTrigger id={providerTypeId} className="text-sm w-full">
                      <SelectValue placeholder={t("setup.select_provider")} />
                    </SelectTrigger>
                    <SelectContent>
                      {[...providerTypes].sort((a, b) => {
                        const aDetected = detectedClis.includes(a.id) ? 0 : 1;
                        const bDetected = detectedClis.includes(b.id) ? 0 : 1;
                        return aDetected - bDetected;
                      }).map((pt) => (
                        <SelectItem key={pt.id} value={pt.id}>
                          <span className="flex items-center gap-2">
                            {pt.name || pt.id}
                            {detectedClis.includes(pt.id) && (
                              <Badge variant="outline-success" size="xs">
                                {t("setup.cli_detected")}
                              </Badge>
                            )}
                          </span>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                ) : (
                  <Input
                    id={providerTypeId}
                    value={providerType}
                    onChange={(e) => setProviderType(e.target.value)}
                    className="font-mono text-sm"
                    placeholder="openai, anthropic, ollama..."
                  />
                )}
              </div>

              {/* API Key */}
              {providerType && (
                <Field label={t("setup.api_key")}>
                  <Input
                    type="password"
                    value={apiKeyValue}
                    onChange={(e) => setApiKeyValue(e.target.value)}
                    className="font-mono text-sm"
                    placeholder={selectedTypeInfo?.requires_api_key === false ? t("setup.optional_hint") : "sk-... / key-..."}
                  />
                </Field>
              )}

              {/* Base URL */}
              {providerType && (
                <div className="space-y-2">
                  <label htmlFor={baseUrlId} className="text-sm font-medium text-muted-foreground">{t("setup.base_url")} <span className="text-xs text-muted-foreground-subtle">({t("common.optional")})</span></label>
                  <Input
                    id={baseUrlId}
                    value={baseUrl}
                    onChange={(e) => setBaseUrl(e.target.value)}
                    className="font-mono text-sm"
                    placeholder={selectedTypeInfo?.default_base_url || "https://api.example.com"}
                  />
                </div>
              )}

              {/* Model */}
              {providerType && (
                <div className="space-y-1.5">
                  <label htmlFor={modelId} className="text-sm font-medium text-muted-foreground">
                    {t("setup.model")} <span className="text-destructive">*</span>
                  </label>
                  {discoveredModels.length > 0 ? (
                    <div className="flex gap-2">
                      <Select
                        value={discoveredModels.includes(defaultModel) ? defaultModel : ""}
                        onValueChange={setDefaultModel}
                      >
                        <SelectTrigger id={modelId} className="font-mono text-sm">
                          <SelectValue placeholder={t("setup.select_model")} />
                        </SelectTrigger>
                        <SelectContent>
                          {discoveredModels.map((m) => (
                            <SelectItem key={m} value={m} className="font-mono text-sm">{m}</SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <Button
                        variant="outline"
                        size="icon"
                        className="shrink-0 h-9 w-9"
                        onClick={discoverModels}
                        disabled={modelsLoading}
                      >
                        <RefreshCw className={`h-3.5 w-3.5 ${modelsLoading ? "animate-spin" : ""}`} />
                      </Button>
                    </div>
                  ) : (
                    <div className="flex gap-2">
                      <Input
                        id={modelId}
                        value={defaultModel}
                        onChange={(e) => setDefaultModel(e.target.value)}
                        className="font-mono text-sm"
                        placeholder={fallbackModels.length > 0 ? fallbackModels[0] : t("setup.model_placeholder")}
                      />
                      {selectedTypeInfo && (
                        <Button
                          variant="outline"
                          size="sm"
                          className="shrink-0 h-9 text-xs"
                          onClick={discoverModels}
                          disabled={modelsLoading}
                        >
                          {modelsLoading ? (
                            <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <RefreshCw className="h-3.5 w-3.5" />
                          )}
                          <span className="ml-1">{t("common.discover")}</span>
                        </Button>
                      )}
                    </div>
                  )}
                  {fallbackModels.length > 0 && discoveredModels.length === 0 && !defaultModel && (
                    <div className="flex flex-wrap gap-1.5 mt-1">
                      {fallbackModels.map((m) => (
                        <Button
                          key={m}
                          type="button"
                          variant="outline"
                          size="xs"
                          onClick={() => setDefaultModel(m)}
                          className="font-mono text-2xs"
                        >
                          {m}
                        </Button>
                      ))}
                    </div>
                  )}
                </div>
              )}

              {testCallStatus === "testing" && (
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  {t("setup.provider_test_call")}
                </div>
              )}

              <Button onClick={doStep1} disabled={loading || !step1Valid} className="w-full">
                {loading ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : <ArrowRight className="h-4 w-4 mr-2" />}
                {t("common.next")}
              </Button>
            </div>
          )}

          {/* ── Step 2: Agent ─────────────────────────────────────── */}
          {step === "agent" && (
            <div className="mt-4 space-y-4">
              <p className="text-sm text-muted-foreground">
                {t("setup.create_first_agent")}
              </p>
              <div className="grid grid-cols-2 gap-3">
                <Field label={t("setup.name")}>
                  <Input
                    value={agentName}
                    onChange={(e) => setAgentName(e.target.value)}
                    className="font-mono text-sm"
                    placeholder="Opex"
                  />
                </Field>
                <div className="space-y-2">
                  <label htmlFor={langId} className="text-sm font-medium text-muted-foreground">{t("setup.language")}</label>
                  <Select value={agentLang} onValueChange={setAgentLang}>
                    <SelectTrigger id={langId} className="text-sm w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {LANGUAGES.map((l) => (
                        <SelectItem key={l.value} value={l.value}>{l.label}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              </div>
              <div className="rounded-md bg-muted/50 p-2 text-xs text-muted-foreground">
                Provider: <span className="font-medium">{providerType}</span>
                {" / "}Model: <span className="font-mono">{defaultModel}</span>
              </div>
              <Button onClick={doStep2} disabled={loading || !agentName.trim()} className="w-full">
                {loading ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : <ArrowRight className="h-4 w-4 mr-2" />}
                {t("common.next")}
              </Button>
            </div>
          )}

          {/* ── Step 3: Channel ───────────────────────────────────── */}
          {step === "channel" && (
            <div className="mt-4 space-y-4">
              {networkData && (
                <div className="rounded-md border border-border bg-muted/30 p-3 space-y-2 text-sm">
                  <p className="font-medium text-xs text-muted-foreground flex items-center gap-1">
                    <Wifi className="h-4 w-4" />
                    {t("setup.network_info")}
                  </p>
                  {networkData.lan.length > 0 && (
                    <div>
                      <span className="text-xs text-muted-foreground">{t("setup.network_local")}:</span>
                      <span className="ml-2 font-mono text-xs">
                        {networkData.lan.filter(i => !i.is_ipv6).map(i => i.ip).join(", ")}
                      </span>
                    </div>
                  )}
                  {networkData.wan?.ip && (
                    <div>
                      <span className="text-xs text-muted-foreground">{t("setup.network_wan")}:</span>
                      <span className="ml-2 font-mono text-xs">{networkData.wan.ip}</span>
                      {networkData.wan.is_cgnat && (
                        <span className="ml-2 text-xs text-warning">
                          ({t("setup.network_cgnat")})
                        </span>
                      )}
                    </div>
                  )}
                  {networkData.tailscale?.connected && (
                    <div>
                      <span className="text-xs text-muted-foreground">{t("setup.network_tailscale")}:</span>
                      <span className="ml-2 font-mono text-xs">
                        {networkData.tailscale.dns_name || networkData.tailscale.ips[0] || "connected"}
                      </span>
                    </div>
                  )}
                  {networkData.mdns?.hostname && (
                    <div>
                      <span className="text-xs text-muted-foreground">{t("setup.network_mdns")}:</span>
                      <span className="ml-2 font-mono text-xs">{networkData.mdns.hostname}</span>
                    </div>
                  )}
                </div>
              )}
              <p className="text-sm text-muted-foreground">
                {t("setup.connect_telegram_bot")}
              </p>
              <Field label={t("setup.bot_name")}>
                <Input
                  value={botDisplayName}
                  onChange={(e) => setBotDisplayName(e.target.value)}
                  className="text-sm"
                  placeholder={`${agentName || "Agent"} Telegram`}
                  disabled={skipChannel}
                />
              </Field>
              <Field label={t("setup.bot_token")}>
                <Input
                  type="password"
                  value={botToken}
                  onChange={(e) => setBotToken(e.target.value)}
                  className="font-mono text-sm"
                  placeholder="123456789:ABCDEF..."
                  disabled={skipChannel}
                  onKeyDown={(e) => e.key === "Enter" && doStep3()}
                />
              </Field>
              <div className="flex gap-3">
                <Button onClick={doStep3} disabled={loading || (!botToken.trim() && !skipChannel)} className="flex-1">
                  {loading ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : <Check className="h-4 w-4 mr-2" />}
                  {skipChannel ? t("common.finish") : t("common.connect_and_finish")}
                </Button>
                <Button
                  variant="ghost"
                  onClick={async () => { setSkipChannel(true); await finishSetup(); }}
                  disabled={loading}
                >
                  {t("common.skip")}
                </Button>
              </div>
            </div>
          )}
      </Card>
    </AuthShell>
  );
}
