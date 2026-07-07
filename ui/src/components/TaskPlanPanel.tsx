"use client";

import React, { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import {
  Loader,
  CheckCircle,
  XCircle,
  Circle,
  ChevronDown,
  ChevronRight,
  ListTodo,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { useAgentTasks } from "@/lib/queries";
import type { AgentTask, TaskStep } from "@/types/api";

// ── Step Status Badge ────────────────────────────────────────────────────────

function StepBadge({ step }: { step: TaskStep }) {
  const { t } = useTranslation();
  switch (step.status) {
    case "in_progress":
      return (
        <Badge variant="secondary" className="gap-1">
          <Loader className="h-4 w-4 animate-spin" />
          {t("common.running")}
        </Badge>
      );
    case "done":
      return (
        <Badge variant="success" className="gap-1">
          <CheckCircle className="h-4 w-4" />
          {t("common.done")}
        </Badge>
      );
    case "error":
      return (
        <Badge variant="destructive" className="gap-1">
          <XCircle className="h-4 w-4" />
          {t("common.error")}
        </Badge>
      );
    case "pending":
    default:
      return (
        <Badge variant="outline" className="gap-1">
          <Circle className="h-4 w-4" />
          {t("common.pending")}
        </Badge>
      );
  }
}

// ── Task Card ────────────────────────────────────────────────────────────────

function TaskCard({ task }: { task: AgentTask }) {
  const [expanded, setExpanded] = useState(true);

  const doneCount = task.steps.filter((s) => s.status === "done").length;
  const totalCount = task.steps.length;

  return (
    <div className="py-2 px-4">
      <button
        className="flex w-full items-center gap-1.5 text-left text-sm font-medium text-foreground/80 hover:text-foreground transition-colors"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        )}
        <span className="flex-1 truncate min-w-0">{task.title}</span>
        {totalCount > 0 && (
          <span className="shrink-0 text-xs text-muted-foreground-subtle">
            {doneCount}/{totalCount}
          </span>
        )}
      </button>

      {expanded && task.steps.length > 0 && (
        <ul className="mt-1.5 ml-5 space-y-1.5">
          {task.steps.map((step) => (
            <li key={step.id} className="flex flex-col gap-0.5">
              <div className="flex items-center gap-2">
                <span className="flex-1 text-xs text-muted-foreground truncate min-w-0">
                  {step.title}
                </span>
                <StepBadge step={step} />
              </div>
              {step.status === "error" && step.error && (
                <p className="ml-0 text-xs text-destructive/80 truncate" title={step.error}>
                  {step.error}
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ── TaskPlanPanel ─────────────────────────────────────────────────────────────

interface TaskPlanPanelProps {
  agentName: string | null;
  isStreaming?: boolean;
}

export function TaskPlanPanel({ agentName, isStreaming = false }: TaskPlanPanelProps) {
  const { t } = useTranslation();
  const [showCompleted, setShowCompleted] = useState(false);
  const { data: tasks } = useAgentTasks(agentName, isStreaming);

  if (!tasks || tasks.length === 0) return null;

  const activeTasks = tasks.filter((t) => t.status !== "done");
  const completedTasks = tasks.filter((t) => t.status === "done");

  const visibleTasks = showCompleted ? tasks : activeTasks;

  if (visibleTasks.length === 0 && completedTasks.length === 0) return null;
  if (activeTasks.length === 0 && !showCompleted) {
    // All done — show nothing (panel is invisible when idle)
    return null;
  }

  return (
    <div className="border-b border-border/50 bg-muted/20">
      <div className="flex items-center gap-1.5 px-4 py-2 text-xs font-semibold text-muted-foreground uppercase tracking-wider">
        <ListTodo className="h-3.5 w-3.5" />
        <span className="flex-1">{t("tasks.plan_title")}</span>
        {completedTasks.length > 0 && (
          <button
            className="text-xs font-normal normal-case text-muted-foreground/60 hover:text-muted-foreground transition-colors tracking-normal"
            onClick={() => setShowCompleted((v) => !v)}
          >
            {showCompleted ? t("tasks.hide_done") : t("tasks.done_count", { count: completedTasks.length })}
          </button>
        )}
      </div>
      {visibleTasks.map((task) => (
        <TaskCard key={task.task_id} task={task} />
      ))}
    </div>
  );
}
