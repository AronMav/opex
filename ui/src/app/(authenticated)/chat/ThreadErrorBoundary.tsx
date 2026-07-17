"use client";

import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { Button } from "@/components/ui/button";

interface ThreadErrorBoundaryProps {
  children: ReactNode;
  onRetry?: () => void;
  retryLabel: string;
}
interface ThreadErrorBoundaryState {
  error: string | null;
}

export class ThreadErrorBoundary extends Component<ThreadErrorBoundaryProps, ThreadErrorBoundaryState> {
  state: ThreadErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error: error.message };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.warn("[ThreadErrorBoundary]", error.message, info.componentStack?.slice(0, 200));
  }

  private handleRetry = () => {
    // Clear the boundary AND let the parent re-drive the failed operation (B1);
    // clearing alone just re-mounts the same failing subtree.
    this.setState({ error: null });
    this.props.onRetry?.();
  };

  render() {
    if (this.state.error) {
      return (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
          <p role="alert" className="text-sm text-muted-foreground font-mono">{this.state.error}</p>
          <Button variant="outline" size="sm" onClick={this.handleRetry}>
            {this.props.retryLabel}
          </Button>
        </div>
      );
    }
    return this.props.children;
  }
}
