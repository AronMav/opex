"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useAuthStore } from "@/stores/auth-store";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { AuthShell, AuthBrand } from "@/components/ui/auth-shell";
import { Alert } from "@/components/ui/alert";
import { Card } from "@/components/ui/card";
import { Eye, EyeOff } from "lucide-react";

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
    <AuthShell glow className="max-w-sm">
      <AuthBrand orientation="vertical" subtitle={t("login.control_panel")} className="mb-10" />

      <Card className="p-8">
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
                className="h-12 border-border bg-background font-mono text-sm placeholder:text-muted-foreground-subtle focus:border-primary/30 rounded-xl neu-inset pr-12"
              />
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                onClick={() => setShowToken((v) => !v)}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                aria-label={t("login.show_token")}
              >
                {showToken ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              </Button>
            </div>
          </div>

          {error && <Alert variant="destructive">{error}</Alert>}

          <Button
            type="submit"
            className="w-full h-12 font-semibold text-sm rounded-xl transition-all duration-200 active:scale-[0.98]"
            disabled={loading || !token.trim()}
          >
            {loading ? t("login.checking") : t("login.submit")}
          </Button>
        </form>
      </Card>
    </AuthShell>
  );
}
