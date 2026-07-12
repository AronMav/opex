"use client";

import type { MouseEvent } from "react";
import { Button } from "@/components/ui/button";
import { useResolveInfraDecision } from "@/lib/queries";
import type { NotificationRow } from "@/types/api";

/**
 * Actionable body for `infra_decision` notifications (self-healing infra —
 * Task 5 backend `POST /api/infra/decisions/{id}/resolve`). Renders inline
 * "Выполнить"/"Отклонить" buttons instead of a plain body line — same
 * inline-render convention as `MediaNotificationBody`, including
 * `stopPropagation` so a button click doesn't also trigger the row's
 * navigate-on-click handler in `notification-bell.tsx`.
 */
export function NotificationInfraBody({ n }: { n: NotificationRow }) {
  const resolve = useResolveInfraDecision();
  const decisionId = n.data?.decision_id;
  const id = typeof decisionId === "string" ? decisionId : null;
  if (!id) return null;

  const stop = (e: MouseEvent) => e.stopPropagation();

  return (
    <div className="mt-2 flex gap-2" onClick={stop}>
      <Button
        size="sm"
        variant="default"
        disabled={resolve.isPending}
        onClick={() => resolve.mutate({ id, approved: true })}
      >
        Выполнить
      </Button>
      <Button
        size="sm"
        variant="outline"
        disabled={resolve.isPending}
        onClick={() => resolve.mutate({ id, approved: false })}
      >
        Отклонить
      </Button>
    </div>
  );
}
