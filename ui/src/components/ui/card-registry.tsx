"use client"

import React, { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { TableCard, MetricCard } from "@/components/ui/rich-card";

// ── Card component type ────────────────────────────────────────────────────

export type CardComponent = React.ComponentType<{ data: Record<string, unknown> }>;

// ── Registry ───────────────────────────────────────────────────────────────

export const CARD_REGISTRY = new Map<string, CardComponent>([
  ["table", TableCard],
  ["metric", MetricCard],
]);

// ── Error boundary ─────────────────────────────────────────────────────────

interface CardErrorBoundaryProps {
  resetKey: string;
  children: ReactNode;
}

interface CardErrorBoundaryState {
  hasError: boolean;
}

export class CardErrorBoundary extends Component<CardErrorBoundaryProps, CardErrorBoundaryState> {
  constructor(props: CardErrorBoundaryProps) {
    super(props);
    this.state = { hasError: false };
  }

  static getDerivedStateFromError(): CardErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: Error, _info: ErrorInfo) {
    console.error("[CardErrorBoundary]", error.message);
  }

  componentDidUpdate(prevProps: CardErrorBoundaryProps) {
    if (prevProps.resetKey !== this.props.resetKey && this.state.hasError) {
      this.setState({ hasError: false });
    }
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
          Card rendering error
        </div>
      );
    }
    return this.props.children;
  }
}

// ── Generative UI slot ─────────────────────────────────────────────────────

interface GenerativeUISlotProps {
  cardType: string;
  data: Record<string, unknown>;
}

export function GenerativeUISlot({ cardType, data }: GenerativeUISlotProps) {
  const CardComp = CARD_REGISTRY.get(cardType);

  return (
    <div style={{ contentVisibility: "auto", containIntrinsicSize: "0 200px" }}>
      {CardComp ? (
        <CardErrorBoundary resetKey={cardType}>
          <CardComp data={data} />
        </CardErrorBoundary>
      ) : (
        <pre className="rounded-lg border bg-muted/30 p-4 text-sm font-mono whitespace-pre-wrap overflow-auto">
          {JSON.stringify(data, null, 2)}
        </pre>
      )}
    </div>
  );
}
