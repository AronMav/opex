"use client";

import { Suspense } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { ArrowLeft, Target, Lightbulb, ListChecks, Check, X } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";
import { relativeTime } from "@/lib/format";
import { useAgentPlan, useApproveProposal, useDismissProposal } from "@/lib/queries";
import { PageContainer } from "@/components/ui/page-container";
import { PageHeader } from "@/components/ui/page-header";
import { SectionHeader } from "@/components/ui/section-header";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { StatusBadge } from "@/components/ui/status-badge";
import { EmptyState } from "@/components/ui/empty-state";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Skeleton } from "@/components/ui/skeleton";
import { CircularLoader } from "@/components/ui/loader";
import type { AgentPlanProposal } from "@/types/api";
import type { TranslationKey } from "@/i18n/types";

// Static export (`output: "export"`) can't pre-render a `[name]` dynamic
// segment for an open-ended, runtime-configurable agent set, so this page is
// addressed via `?agent=` query param instead — same pattern as the
// `/monitor/?tab=` tabs. Kept as its own route (not a monitor tab) since it's
// agent-scoped, not a global cross-agent view.
const PROPOSAL_STATUS_KEY: Record<string, TranslationKey> = {
  pending: "agent_plan.status_pending",
  approved: "agent_plan.status_approved",
  dismissed: "agent_plan.status_dismissed",
};

function AgentPlanPageInner() {
  const { t, locale } = useTranslation();
  const router = useRouter();
  const searchParams = useSearchParams();
  const agent = searchParams.get("agent") ?? "";

  const { data: plan, isLoading, error } = useAgentPlan(agent || null);
  const approveProposal = useApproveProposal();
  const dismissProposal = useDismissProposal();

  const acting = approveProposal.isPending || dismissProposal.isPending;

  return (
    <PageContainer>
      <PageHeader
        title={t("agent_plan.title", { agent })}
        description={t("agent_plan.subtitle")}
        actions={
          <Button variant="outline" size="sm" onClick={() => router.push("/agents")}>
            <ArrowLeft className="h-4 w-4 mr-2" />
            {t("agent_plan.back")}
          </Button>
        }
      />

      {error && <ErrorBanner error={`${error}`} />}

      {isLoading ? (
        <div className="space-y-6">
          <Skeleton className="h-24 rounded-xl" />
          <Skeleton className="h-48 rounded-xl" />
          <Skeleton className="h-32 rounded-xl" />
        </div>
      ) : plan ? (
        <div className="space-y-8">
          {/* Current focus */}
          <div>
            <SectionHeader icon={Target} title={t("agent_plan.current_focus")} />
            {plan.current_focus ? (
              <Card className="p-4">
                <p className="text-sm text-foreground/90 whitespace-pre-wrap break-words">
                  {plan.current_focus}
                </p>
              </Card>
            ) : (
              <EmptyState icon={Target} text={t("agent_plan.no_focus")} height="h-24" />
            )}
          </div>

          {/* Proposals */}
          <div>
            <SectionHeader
              icon={Lightbulb}
              title={t("agent_plan.proposals")}
              count={plan.proposals.length}
            />
            {plan.proposals.length === 0 ? (
              <EmptyState icon={Lightbulb} text={t("agent_plan.no_proposals")} height="h-24" />
            ) : (
              <div className="grid gap-4">
                {plan.proposals.map((p: AgentPlanProposal) => (
                  <Card key={p.id} className="flex flex-col gap-3 p-4">
                    <div className="flex items-start justify-between gap-3 flex-wrap">
                      <p className="min-w-0 flex-1 text-sm text-foreground/90 whitespace-pre-wrap break-words">
                        {p.text}
                      </p>
                      <StatusBadge status={p.status}>
                        {t(PROPOSAL_STATUS_KEY[p.status] ?? "agent_plan.status_pending")}
                      </StatusBadge>
                    </div>
                    <span className="text-2xs text-muted-foreground-subtle font-mono tabular-nums">
                      {relativeTime(p.created_at, locale)}
                    </span>
                    {p.status === "pending" && (
                      <div className="grid grid-cols-2 md:flex md:items-center md:justify-end gap-2 border-t border-border/50 pt-3">
                        <Button
                          variant="outline-success"
                          size="sm"
                          onClick={() => approveProposal.mutate({ agent, id: p.id })}
                          disabled={acting}
                          className="text-xs font-medium"
                        >
                          <Check className="h-4 w-4 mr-2" />
                          {t("agent_plan.approve")}
                        </Button>
                        <Button
                          variant="outline-destructive"
                          size="sm"
                          onClick={() => dismissProposal.mutate({ agent, id: p.id })}
                          disabled={acting}
                          className="text-xs font-medium"
                        >
                          <X className="h-4 w-4 mr-2" />
                          {t("agent_plan.dismiss")}
                        </Button>
                      </div>
                    )}
                  </Card>
                ))}
              </div>
            )}
          </div>

          {/* Active goals */}
          <div>
            <SectionHeader
              icon={ListChecks}
              title={t("agent_plan.active_goals")}
              count={plan.active_goals.length}
            />
            {plan.active_goals.length === 0 ? (
              <EmptyState icon={ListChecks} text={t("agent_plan.no_active_goals")} height="h-24" />
            ) : (
              <div className="grid gap-3">
                {plan.active_goals.map((g, i) => (
                  <Card key={i} className="flex items-center justify-between gap-3 p-4">
                    <p className="min-w-0 flex-1 text-sm text-foreground/90 whitespace-pre-wrap break-words">
                      {g.goal}
                    </p>
                    <Badge variant="outline" className="font-mono shrink-0">
                      {t("agent_plan.turns", { count: g.turns })}
                    </Badge>
                  </Card>
                ))}
              </div>
            )}
          </div>
        </div>
      ) : null}
    </PageContainer>
  );
}

export default function AgentPlanPage() {
  return (
    <Suspense fallback={<div className="flex h-full items-center justify-center"><CircularLoader size="lg" /></div>}>
      <AgentPlanPageInner />
    </Suspense>
  );
}
