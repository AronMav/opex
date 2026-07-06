"use client"

import React, { Component, ErrorInfo, ReactNode } from "react"
import { AlertCircle } from "lucide-react"

interface Props {
  children: ReactNode
  fallback?: ReactNode
}

interface State {
  hasError: boolean
  error: Error | null
}

export class ErrorBoundary extends Component<Props, State> {
  public state: State = {
    hasError: false,
    error: null,
  }

  public static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error }
  }

  public componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("Uncaught error in ErrorBoundary:", error, errorInfo)
  }

  public render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback
      }

      return (
        <div className="flex flex-col items-center justify-center p-4 border border-destructive/20 bg-destructive/5 rounded-lg text-destructive text-sm my-2">
          <AlertCircle className="h-5 w-5 mb-2" />
          <p className="font-medium text-center">Failed to render content</p>
          <pre className="mt-2 text-3xs overflow-auto max-w-full opacity-70">
            {this.state.error?.message}
          </pre>
        </div>
      )
    }

    return this.props.children
  }
}
