"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { useAuthStore } from "@/stores/auth-store";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Bot, Eye, EyeOff } from "lucide-react";

export default function LoginPage() {
  const { t } = useTranslation();
  const router = useRouter();
  const login = useAuthStore((s) => s.login);
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [showToken, setShowToken] = useState(false);

  // URL-based token login removed for security:
  // tokens in URLs leak via browser history, nginx logs, and Referer headers.

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!token.trim()) return;
    setLoading(true);
    setError("");
    await new Promise(r => setTimeout(r, 400));
    const result = await login(token.trim());
    setLoading(false);
    if (result === true) {
      router.replace("/chat");
    } else if (result === "rate_limited") {
      setError(t("login.rate_limited"));
    } else {
      setError(t("login.invalid_token"));
    }
  };

  return (
    <div className="relative flex h-dvh w-full items-center justify-center overflow-hidden bg-background selection:bg-primary/30">
      <div className="absolute inset-0 pointer-events-none overflow-hidden">
        <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 h-[600px] w-[600px] bg-primary/5 rounded-full blur-[120px] opacity-50" />
      </div>

      <div className="relative z-10 w-full max-w-[400px] px-6">
        <div className="mb-10 flex flex-col items-center gap-3">
          <div className="flex h-14 w-14 items-center justify-center rounded-2xl bg-card border border-border neu-card">
            <Bot className="h-7 w-7 text-primary" />
          </div>
          <div className="flex flex-col items-center">
            <h1 className="font-display text-2xl font-bold tracking-wide text-foreground">
              OPEX
            </h1>
            <span className="text-sm text-muted-foreground mt-1">
              {t("login.control_panel")}
            </span>
          </div>
        </div>

        <div className="neu-card p-8">
          <div className="mb-6">
            <span className="text-sm font-semibold text-foreground">{t("login.sign_in")}</span>
          </div>

          <form onSubmit={handleSubmit} className="space-y-5">
            <div className="space-y-2">
              <div className="relative">
                <Input
                  type={showToken ? "text" : "password"}
                  placeholder={t("login.enter_token")}
                  value={token}
                  onChange={(e) => setToken(e.target.value)}
                  autoFocus
                  disabled={loading}
                  className="h-12 border-border bg-background font-mono text-sm placeholder:text-muted-foreground/50 focus:border-primary/40 rounded-xl neu-inset pr-12"
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  onClick={() => setShowToken((v) => !v)}
                  className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                  aria-label={t("login.show_token")}
                >
                  {showToken ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                </Button>
              </div>
            </div>

            {error && (
              <div className="rounded-lg border border-destructive/20 bg-destructive/10 p-3">
                <p className="text-sm text-destructive">{error}</p>
              </div>
            )}

            <Button
              type="submit"
              className="w-full h-12 font-semibold text-sm rounded-xl transition-all duration-200 active:scale-[0.98]"
              disabled={loading || !token.trim()}
            >
              {loading ? t("login.checking") : t("login.submit")}
            </Button>
          </form>
        </div>
      </div>
    </div>
  );
}
