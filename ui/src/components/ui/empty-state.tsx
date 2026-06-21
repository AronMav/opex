"use client";

import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

interface EmptyStateProps {
  icon: LucideIcon;
  text: string;
  hint?: ReactNode;
  height?: string;
  className?: string;
}

export function EmptyState({ icon: Icon, text, hint, height = "h-64", className }: EmptyStateProps) {
  return (
    <div className={`flex ${height} flex-col items-center justify-center rounded-xl border border-dashed border-border bg-muted/10 ${className ?? ""}`}>
      <Icon className="h-12 w-12 text-muted-foreground/20 mb-4" aria-hidden="true" />
      <p className="text-sm text-muted-foreground">{text}</p>
      {hint}
    </div>
  );
}
