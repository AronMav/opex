"use client";

import { useState, useEffect } from "react";
import { useSearchParams } from "next/navigation";
import { apiGet, apiPost, apiDelete } from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useAuthStore } from "@/stores/auth-store";
import { useTranslation } from "@/hooks/use-translation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useOAuthAccounts, useOAuthBindings, qk } from "@/lib/queries";
import { Copy, Check, Mail, Unlink, Link2, X, Plus, Trash2 } from "lucide-react";
import type { GitHubRepoInfo, OAuthAccount } from "@/types/api";

/* ── Provider metadata (display only) ───────────────────────────────────── */

const PROVIDERS = ["github", "google"] as const;
type Provider = (typeof PROVIDERS)[number];

const PROVIDER_LABEL: Record<Provider, string> = {
  github: "GitHub",
  google: "Google",
};

/* ── Icon components ────────────────────────────────────────────────────── */

function GoogleIcon({ size = 18 }: { size?: number }) {
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} aria-hidden>
      <path d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92c-.26 1.37-1.04 2.53-2.21 3.31v2.77h3.57c2.08-1.92 3.28-4.74 3.28-8.09z" fill="#4285F4" />
      <path d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84C3.99 20.53 7.7 23 12 23z" fill="#34A853" />
      <path d="M5.84 14.09c-.22-.66-.35-1.36-.35-2.09s.13-1.43.35-2.09V7.07H2.18C1.43 8.55 1 10.22 1 12s.43 3.45 1.18 4.93l2.85-2.22.81-.62z" fill="#FBBC05" />
      <path d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.07l3.66 2.84c.87-2.6 3.3-4.53 6.16-4.53z" fill="#EA4335" />
    </svg>
  );
}

function GitHubIcon({ size = 18, className }: { size?: number; className?: string }) {
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} fill="currentColor" aria-hidden className={className}>
      <path d="M12 2C6.477 2 2 6.477 2 12c0 4.42 2.865 8.17 6.839 9.49.5.092.682-.217.682-.482 0-.237-.008-.866-.013-1.7-2.782.603-3.369-1.34-3.369-1.34-.454-1.156-1.11-1.462-1.11-1.462-.908-.62.069-.608.069-.608 1.003.07 1.531 1.03 1.531 1.03.892 1.529 2.341 1.087 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.11-4.555-4.943 0-1.091.39-1.984 1.029-2.683-.103-.253-.446-1.27.098-2.647 0 0 .84-.269 2.75 1.025A9.578 9.578 0 0112 6.836c.85.004 1.705.115 2.504.337 1.909-1.294 2.747-1.025 2.747-1.025.546 1.377.202 2.394.1 2.647.64.699 1.028 1.592 1.028 2.683 0 3.842-2.339 4.687-4.566 4.935.359.309.678.919.678 1.852 0 1.336-.012 2.415-.012 2.741 0 .267.18.578.688.48C19.138 20.167 22 16.418 22 12c0-5.523-4.477-10-10-10z" />
    </svg>
  );
}

function ProviderIcon({ provider }: { provider: string }) {
  if (provider === "google") {
    return (
      <span className="flex h-9 w-9 items-center justify-center rounded-xl neu-inset shrink-0">
        <GoogleIcon size={18} />
      </span>
    );
  }
  return (
    <span className="flex h-9 w-9 items-center justify-center rounded-xl neu-inset text-foreground/80 shrink-0">
      <GitHubIcon size={18} />
    </span>
  );
}

function CopyButton({ text }: { text: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors shrink-0"
      onClick={() => {
        copyText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      }}
    >
      {copied ? <Check className="h-3 w-3 text-success" /> : <Copy className="h-3 w-3" />}
      <span>{copied ? t("chat.copied") : t("common.copy")}</span>
    </button>
  );
}

/* ── Status badge helper ────────────────────────────────────────────────── */

function StatusBadge({ status }: { status: OAuthAccount["status"] }) {
  const { t } = useTranslation();
  if (status === "connected") {
    return (
      <Badge className="border-success/40 text-success bg-success/10 gap-1.5 font-mono text-[11px]">
        <span className="h-1.5 w-1.5 rounded-full bg-success inline-block" />
        {t("integrations.status_connected")}
      </Badge>
    );
  }
  if (status === "expired") {
    return (
      <Badge variant="outline" className="border-warning/40 text-warning bg-warning/10 font-mono text-[11px]">
        {t("integrations.status_expired")}
      </Badge>
    );
  }
  return (
    <Badge variant="outline" className="text-muted-foreground font-mono text-[11px]">
      {t("integrations.status_disconnected")}
    </Badge>
  );
}

/* ── Gmail types & section ──────────────────────────────────────────────── */

interface GmailTrigger {
  id: string;
  agent_id: string;
  email_address: string;
  watch_expiry: string | null;
  pubsub_topic: string;
  enabled: boolean;
}

function GmailSection({
  selectedAgent,
  callbackUrl,
}: {
  selectedAgent: string;
  callbackUrl: string;
}) {
  const { t, locale } = useTranslation();
  const [pubsubTopic, setPubsubTopic] = useState("");
  const [gmailError, setGmailError] = useState("");

  const { data: gmailTriggers = [], refetch: refetchTriggers } = useQuery({
    queryKey: ["gmail-triggers"],
    queryFn: () =>
      apiGet<{ triggers: GmailTrigger[] }>("/api/triggers/email").then((r) => r.triggers),
  });

  const agentTriggers = gmailTriggers.filter((t) => t.agent_id === selectedAgent);

  const handleGmailEnable = async (topic: string) => {
    setGmailError("");
    try {
      await apiPost("/api/triggers/email", {
        agent_id: selectedAgent,
        email_address: "",
        pubsub_topic: topic,
      });
      refetchTriggers();
      setPubsubTopic("");
    } catch (e) {
      setGmailError(`${e}`);
    }
  };

  return (
    <div className="px-5 py-4">
      <div className="flex items-center gap-2 mb-3">
        <Mail className="h-3.5 w-3.5 text-muted-foreground" />
        <span className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
          {t("integrations.gmail_notifications")}
        </span>
      </div>

      {agentTriggers.length > 0 && (
        <div className="flex flex-col gap-2 mb-3">
          {agentTriggers.map((trigger) => (
            <div
              key={trigger.id}
              className="flex items-center justify-between neu-inset rounded-lg px-3 py-2"
            >
              <div className="min-w-0">
                <p className="font-mono text-xs font-semibold truncate">{trigger.email_address}</p>
                {trigger.watch_expiry && (
                  <p className="text-[11px] text-muted-foreground">
                    {t("integrations.watch_expiry", { date: new Date(trigger.watch_expiry).toLocaleDateString(locale) })}
                  </p>
                )}
              </div>
              <button
                type="button"
                className="ml-3 shrink-0 text-muted-foreground hover:text-destructive transition-colors"
                onClick={async () => {
                  try {
                    await apiDelete(`/api/triggers/email/${trigger.id}`);
                    refetchTriggers();
                  } catch (e) {
                    setGmailError(`${e}`);
                    console.error(e);
                  }
                }}
                title={t("integrations.stop")}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="flex flex-col gap-2">
        <p className="text-[11px] text-muted-foreground">
          {t("integrations.pubsub_hint")}{" "}
          <span className="font-mono">
            {callbackUrl.replace("/oauth/callback", "/triggers/email/push")}
          </span>
        </p>
        <div className="flex gap-2">
          <Input
            className="h-8 font-mono text-xs flex-1 min-w-0"
            placeholder="projects/my-project/topics/gmail-push"
            value={pubsubTopic}
            onChange={(e) => setPubsubTopic(e.target.value)}
          />
          <Button
            size="sm"
            className="h-8 shrink-0"
            disabled={!pubsubTopic || !selectedAgent}
            onClick={() => handleGmailEnable(pubsubTopic)}
          >
            {t("integrations.enable")}
          </Button>
        </div>
        {gmailError && <p className="text-xs text-destructive">{gmailError}</p>}
      </div>
    </div>
  );
}

/* ── GitHub repos inline ────────────────────────────────────────────────── */

function GitHubReposInline({ agent }: { agent: string }) {
  const { t } = useTranslation();
  const [newOwner, setNewOwner] = useState("");
  const [newRepo, setNewRepo] = useState("");
  const [adding, setAdding] = useState(false);
  const [repoError, setRepoError] = useState("");

  const { data: repos = [], refetch } = useQuery<GitHubRepoInfo[]>({
    queryKey: ["github-repos", agent],
    queryFn: () =>
      apiGet<{ repos: GitHubRepoInfo[] }>(`/api/agents/${agent}/github/repos`).then(
        (r) => r?.repos ?? []
      ),
    enabled: !!agent,
  });

  const handleAdd = async () => {
    if (!newOwner.trim() || !newRepo.trim()) return;
    setAdding(true);
    setRepoError("");
    try {
      await apiPost(`/api/agents/${agent}/github/repos`, {
        owner: newOwner.trim(),
        repo: newRepo.trim(),
      });
      setNewOwner("");
      setNewRepo("");
      refetch();
    } catch (e) {
      setRepoError(`${e}`);
      console.error(e);
    } finally {
      setAdding(false);
    }
  };

  const handleDelete = async (id: string) => {
    setRepoError("");
    try {
      await apiDelete(`/api/agents/${agent}/github/repos/${id}`);
      refetch();
    } catch (e) {
      setRepoError(`${e}`);
      console.error(e);
    }
  };

  return (
    <>
      <div className="border-t border-border/60 mx-5" />
      <div className="px-5 py-4">
        <div className="flex items-center gap-2 mb-3">
          <GitHubIcon size={14} className="text-muted-foreground" />
          <span className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
            {t("integrations.allowed_repos")}
          </span>
        </div>

        {repos.length > 0 && (
          <div className="flex flex-col gap-2 mb-3">
            {repos.map((r) => (
              <div
                key={r.id}
                className="flex items-center justify-between neu-inset rounded-lg px-3 py-2"
              >
                <span className="font-mono text-xs font-semibold truncate min-w-0">
                  {r.owner}/{r.repo}
                </span>
                <button
                  type="button"
                  className="ml-3 shrink-0 text-muted-foreground hover:text-destructive transition-colors"
                  onClick={() => handleDelete(r.id)}
                  title={t("integrations.remove")}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}
          </div>
        )}

        {repos.length === 0 && (
          <p className="text-[11px] text-muted-foreground mb-3">
            {t("integrations.no_repos")}
          </p>
        )}

        <div className="flex gap-2">
          <Input
            className="h-8 font-mono text-xs flex-1 min-w-0"
            placeholder="owner/repo"
            value={newOwner ? `${newOwner}/${newRepo}` : newRepo}
            onChange={(e) => {
              const val = e.target.value;
              const slash = val.indexOf("/");
              if (slash >= 0) {
                setNewOwner(val.substring(0, slash));
                setNewRepo(val.substring(slash + 1));
              } else {
                setNewOwner(val);
                setNewRepo("");
              }
            }}
            onKeyDown={(e) => e.key === "Enter" && handleAdd()}
          />
          <Button
            size="sm"
            className="h-8 shrink-0"
            disabled={!newOwner.trim() || !newRepo.trim() || adding}
            onClick={handleAdd}
          >
            {t("common.add")}
          </Button>
        </div>
        {repoError && <p className="text-xs text-destructive mt-2">{repoError}</p>}
      </div>
    </>
  );
}

/* ── Main page ──────────────────────────────────────────────────────────── */

export default function IntegrationsPage() {
  const { t, locale } = useTranslation();
  const agents = useAuthStore((s) => s.agents);
  const [selectedAgent, setSelectedAgent] = useState<string>("");
  const [toast, setToast] = useState<{ msg: string; type: "ok" | "err" } | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [addForm, setAddForm] = useState({ provider: "github" as Provider, displayName: "", clientId: "", clientSecret: "" });
  const [addSaving, setAddSaving] = useState(false);
  const queryClient = useQueryClient();
  const searchParams = useSearchParams();

  const callbackUrl =
    typeof window !== "undefined"
      ? `${window.location.origin}/api/oauth/callback`
      : "/api/oauth/callback";

  // Auto-select first agent
  useEffect(() => {
    if (agents.length > 0 && !selectedAgent) setSelectedAgent(agents[0]);
  }, [agents, selectedAgent]);

  // Handle OAuth callback params
  useEffect(() => {
    const connected = searchParams.get("connected");
    const error = searchParams.get("error");
    if (connected) {
      const label = PROVIDER_LABEL[connected as Provider] ?? connected;
      setToast({ msg: t("integrations.connected_success", { provider: label }), type: "ok" });
      queryClient.invalidateQueries({ queryKey: qk.oauthAccounts });
    } else if (error) {
      setToast({ msg: t("integrations.oauth_error", { error: decodeURIComponent(error) }), type: "err" });
    }
  }, [searchParams, queryClient]);

  // Data queries
  const { data: accounts = [], isLoading: accountsLoading } = useOAuthAccounts();
  const { data: bindings = [] } = useOAuthBindings(selectedAgent);

  /* ── Account actions ──────────────────────────────────────────────────── */

  const handleAddAccount = async () => {
    if (!addForm.displayName.trim() || !addForm.clientId.trim() || !addForm.clientSecret.trim()) return;
    setAddSaving(true);
    try {
      await apiPost<{ ok: boolean; id: string }>("/api/oauth/accounts", {
        provider: addForm.provider,
        display_name: addForm.displayName.trim(),
        client_id: addForm.clientId.trim(),
        client_secret: addForm.clientSecret.trim(),
      });
      queryClient.invalidateQueries({ queryKey: qk.oauthAccounts });
      setAddForm({ provider: "github", displayName: "", clientId: "", clientSecret: "" });
      setAddOpen(false);
      setToast({ msg: t("integrations.account_added"), type: "ok" });
    } catch (e) {
      setToast({ msg: t("integrations.add_failed", { error: `${e}` }), type: "err" });
    } finally {
      setAddSaving(false);
    }
  };

  const handleConnect = async (accountId: string) => {
    try {
      const res = await apiPost<{ auth_url: string }>(
        `/api/oauth/accounts/${accountId}/connect?agent=${selectedAgent}`,
        {}
      );
      window.location.href = res.auth_url;
    } catch (e) {
      setToast({ msg: t("integrations.connection_error", { error: `${e}` }), type: "err" });
    }
  };

  const handleRevoke = async (accountId: string) => {
    try {
      await apiPost(`/api/oauth/accounts/${accountId}/revoke`, {});
      queryClient.invalidateQueries({ queryKey: qk.oauthAccounts });
      queryClient.invalidateQueries({ queryKey: qk.oauthBindings(selectedAgent) });
      setToast({ msg: t("integrations.account_revoked"), type: "ok" });
    } catch (e) {
      setToast({ msg: t("integrations.revoke_error", { error: `${e}` }), type: "err" });
    }
  };

  const handleDeleteAccount = async (accountId: string) => {
    try {
      await apiDelete(`/api/oauth/accounts/${accountId}`);
      queryClient.invalidateQueries({ queryKey: qk.oauthAccounts });
      queryClient.invalidateQueries({ queryKey: qk.oauthBindings(selectedAgent) });
      setToast({ msg: t("integrations.account_deleted"), type: "ok" });
    } catch (e) {
      setToast({ msg: t("integrations.delete_failed", { error: `${e}` }), type: "err" });
    }
  };

  /* ── Binding actions ──────────────────────────────────────────────────── */

  const handleBindingChange = async (provider: string, accountId: string | "none") => {
    try {
      if (accountId === "none") {
        await apiDelete(`/api/agents/${selectedAgent}/oauth/bindings/${provider}`);
      } else {
        await apiPost(`/api/agents/${selectedAgent}/oauth/bindings`, {
          provider,
          account_id: accountId,
        });
      }
      queryClient.invalidateQueries({ queryKey: qk.oauthBindings(selectedAgent) });
      setToast({ msg: t("integrations.binding_updated"), type: "ok" });
    } catch (e) {
      setToast({ msg: t("integrations.binding_failed", { error: `${e}` }), type: "err" });
    }
  };

  /* ── Derived data ─────────────────────────────────────────────────────── */

  const getBinding = (provider: string) =>
    bindings.find((b) => b.provider === provider);

  const connectedAccountsForProvider = (provider: string) =>
    accounts.filter((a) => a.provider === provider && a.status === "connected");

  const hasBoundConnectedAccount = (provider: string) => {
    const binding = getBinding(provider);
    return binding && binding.status === "connected";
  };

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8">
      {/* Page header */}
      <div className="mb-8 flex flex-col gap-1">
        <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
          {t("integrations.title")}
        </h2>
        <span className="text-sm text-muted-foreground">
          {t("integrations.subtitle")}
        </span>
      </div>

      {/* Toast */}
      {toast && (
        <div
          className={`mb-5 flex items-center justify-between gap-3 px-4 py-3 rounded-xl border text-sm font-medium ${
            toast.type === "ok"
              ? "bg-success/10 text-success border-success/30"
              : "bg-destructive/10 text-destructive border-destructive/30"
          }`}
        >
          <span>{toast.msg}</span>
          <button className="opacity-60 hover:opacity-100 transition-opacity" onClick={() => setToast(null)}>
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {/* Agent selector */}
      <div className="mb-6 flex items-center gap-3">
        <label className="text-xs font-medium text-muted-foreground uppercase tracking-wider shrink-0">
          {t("integrations.agent_label")}
        </label>
        <Select value={selectedAgent} onValueChange={setSelectedAgent}>
          <SelectTrigger className="h-8 w-48 text-xs font-mono">
            <SelectValue placeholder={t("integrations.select_agent")} />
          </SelectTrigger>
          <SelectContent>
            {agents.map((a) => (
              <SelectItem key={a} value={a} className="font-mono text-xs">
                {a}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* ── OAuth Accounts ──────────────────────────────────────────────── */}
      <div className="mb-6">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-display font-bold text-sm tracking-tight">{t("integrations.oauth_accounts")}</h3>
          <Button size="sm" variant="outline" className="gap-1.5 h-7 text-xs" onClick={() => setAddOpen((v) => !v)}>
            <Plus className="h-3 w-3" />
            {t("integrations.add_account")}
          </Button>
        </div>

        {/* Add Account form */}
        {addOpen && (
          <div className="neu-card mb-4 px-5 py-4">
            <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-3">
              {t("integrations.new_oauth_account")}
            </p>
            <div className="grid gap-3">
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="text-xs text-muted-foreground mb-1 block">{t("integrations.provider")}</label>
                  <Select
                    value={addForm.provider}
                    onValueChange={(v) => setAddForm((f) => ({ ...f, provider: v as Provider }))}
                  >
                    <SelectTrigger className="h-9 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {PROVIDERS.map((p) => (
                        <SelectItem key={p} value={p} className="text-xs">
                          {PROVIDER_LABEL[p]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div>
                  <label className="text-xs text-muted-foreground mb-1 block">{t("integrations.display_name")}</label>
                  <Input
                    className="h-9 text-xs"
                    placeholder={t("integrations.display_name_placeholder")}
                    value={addForm.displayName}
                    onChange={(e) => setAddForm((f) => ({ ...f, displayName: e.target.value }))}
                  />
                </div>
              </div>
              <div>
                <label className="text-xs text-muted-foreground mb-1 block font-mono">{t("integrations.client_id")}</label>
                <Input
                  className="h-9 font-mono text-xs"
                  placeholder={t("integrations.client_id")}
                  value={addForm.clientId}
                  onChange={(e) => setAddForm((f) => ({ ...f, clientId: e.target.value }))}
                />
              </div>
              <div>
                <label className="text-xs text-muted-foreground mb-1 block font-mono">{t("integrations.client_secret")}</label>
                <Input
                  type="password"
                  className="h-9 font-mono text-xs"
                  placeholder={t("integrations.client_secret")}
                  value={addForm.clientSecret}
                  onChange={(e) => setAddForm((f) => ({ ...f, clientSecret: e.target.value }))}
                />
              </div>
              <div>
                <p className="text-xs text-muted-foreground mb-1.5">{t("integrations.redirect_uri_hint")}</p>
                <div className="flex items-center gap-2 neu-inset rounded-lg px-3 py-2">
                  <span className="flex-1 font-mono text-xs text-foreground/80 truncate min-w-0">{callbackUrl}</span>
                  <CopyButton text={callbackUrl} />
                </div>
              </div>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  disabled={
                    !addForm.displayName.trim() ||
                    !addForm.clientId.trim() ||
                    !addForm.clientSecret.trim() ||
                    addSaving
                  }
                  onClick={handleAddAccount}
                >
                  {addSaving ? t("common.saving") : t("common.create")}
                </Button>
                <Button size="sm" variant="outline" onClick={() => setAddOpen(false)}>
                  {t("common.cancel")}
                </Button>
              </div>
            </div>
          </div>
        )}

        {/* Accounts list */}
        {accountsLoading && (
          <p className="text-xs text-muted-foreground">{t("integrations.loading_accounts")}</p>
        )}

        {!accountsLoading && accounts.length === 0 && !addOpen && (
          <div className="neu-card px-5 py-6 text-center">
            <p className="text-sm text-muted-foreground">{t("integrations.no_accounts")}</p>
            <p className="text-xs text-muted-foreground mt-1">
              {t("integrations.no_accounts_hint")}
            </p>
          </div>
        )}

        <div className="grid gap-3">
          {accounts.map((account) => (
            <div key={account.id} className="neu-card overflow-hidden">
              <div className="flex items-center gap-3 px-5 py-4">
                <ProviderIcon provider={account.provider} />
                <div className="flex-1 min-w-0">
                  <span className="font-display font-bold text-sm tracking-tight">
                    {account.display_name}
                  </span>
                  <div className="flex items-center gap-2">
                    <span className="text-[11px] text-muted-foreground capitalize">{PROVIDER_LABEL[account.provider as Provider] ?? account.provider}</span>
                    {account.user_email && (
                      <span className="text-[11px] text-muted-foreground font-mono truncate">
                        {account.user_email}
                      </span>
                    )}
                  </div>
                </div>
                <div className="flex items-center gap-2 shrink-0">
                  <StatusBadge status={account.status} />
                  {account.status === "disconnected" && (
                    <Button
                      size="sm"
                      className="gap-1.5 text-xs h-7"
                      disabled={!selectedAgent}
                      onClick={() => handleConnect(account.id)}
                    >
                      <Link2 className="h-3 w-3" />
                      {t("integrations.connect")}
                    </Button>
                  )}
                  {account.status === "connected" && (
                    <Button
                      variant="outline"
                      size="sm"
                      className="border-destructive/30 text-destructive hover:bg-destructive/10 gap-1.5 text-xs h-7"
                      onClick={() => handleRevoke(account.id)}
                    >
                      <Unlink className="h-3 w-3" />
                      {t("integrations.revoke")}
                    </Button>
                  )}
                  {account.status === "expired" && (
                    <Button
                      size="sm"
                      variant="outline"
                      className="gap-1.5 text-xs h-7"
                      disabled={!selectedAgent}
                      onClick={() => handleConnect(account.id)}
                    >
                      <Link2 className="h-3 w-3" />
                      {t("integrations.reconnect")}
                    </Button>
                  )}
                  <button
                    type="button"
                    className="text-muted-foreground hover:text-destructive transition-colors p-1"
                    onClick={() => handleDeleteAccount(account.id)}
                    title={t("integrations.delete_account")}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
              {account.connected_at && (
                <div className="border-t border-border/60 mx-5 px-0 py-2">
                  <span className="text-[11px] text-muted-foreground">
                    {t("integrations.connected_at", { date: new Date(account.connected_at).toLocaleDateString(locale) })}
                    {account.expires_at && (
                      <> · {t("integrations.expires_at", { date: new Date(account.expires_at).toLocaleDateString(locale) })}</>
                    )}
                  </span>
                </div>
              )}
            </div>
          ))}
        </div>
      </div>

      {/* ── Agent Bindings ──────────────────────────────────────────────── */}
      {selectedAgent && (
        <div className="mb-6">
          <h3 className="font-display font-bold text-sm tracking-tight mb-4">
            {t("integrations.agent_bindings")}
            <span className="ml-2 font-mono text-xs text-primary font-normal">{selectedAgent}</span>
          </h3>

          <div className="grid gap-3">
            {PROVIDERS.map((provider) => {
              const binding = getBinding(provider);
              const available = connectedAccountsForProvider(provider);
              const currentValue = binding?.account_id ?? "none";

              return (
                <div key={provider} className="neu-card px-5 py-4">
                  <div className="flex items-center gap-3">
                    <ProviderIcon provider={provider} />
                    <div className="flex-1 min-w-0">
                      <span className="font-display font-bold text-sm tracking-tight">
                        {PROVIDER_LABEL[provider]}
                      </span>
                      {binding && (
                        <p className="text-[11px] text-muted-foreground font-mono truncate">
                          {binding.display_name}
                          {binding.user_email && ` (${binding.user_email})`}
                        </p>
                      )}
                    </div>
                    <Select
                      value={currentValue}
                      onValueChange={(v) => handleBindingChange(provider, v)}
                    >
                      <SelectTrigger className="h-8 w-52 text-xs font-mono">
                        <SelectValue placeholder={t("integrations.none")} />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="none" className="text-xs">
                          {t("integrations.none")}
                        </SelectItem>
                        {available.map((acc) => (
                          <SelectItem key={acc.id} value={acc.id} className="font-mono text-xs">
                            {acc.display_name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* ── Provider-specific sections ──────────────────────────────────── */}

      {/* GitHub repos — when agent has bound + connected GitHub account */}
      {selectedAgent && hasBoundConnectedAccount("github") && (
        <div className="neu-card overflow-hidden mb-4">
          <GitHubReposInline agent={selectedAgent} />
        </div>
      )}

      {/* Gmail triggers — when agent has bound + connected Google account */}
      {selectedAgent && hasBoundConnectedAccount("google") && (
        <div className="neu-card overflow-hidden mb-4">
          <GmailSection selectedAgent={selectedAgent} callbackUrl={callbackUrl} />
        </div>
      )}
    </div>
  );
}
