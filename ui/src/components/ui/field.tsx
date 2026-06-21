"use client";

import * as React from "react";
import { cn } from "@/lib/utils";

interface FieldProps {
  label: string;
  hint?: React.ReactNode;
  error?: React.ReactNode;
  htmlFor?: string;
  className?: string;
  labelClassName?: string;
  children: React.ReactElement;
}

export function Field({ label, hint, error, htmlFor, className, labelClassName, children }: FieldProps) {
  const autoId = React.useId();
  const childProps = children.props as { id?: string; "aria-describedby"?: string };
  const id = htmlFor ?? (childProps.id ?? autoId);
  const descId = `${id}-desc`;
  const hasError = Boolean(error);
  const describedBy = hasError
    ? descId
    : hint
      ? descId
      : childProps["aria-describedby"];

  return (
    <div className={cn("space-y-2", className)}>
      <label
        htmlFor={id}
        className={cn("text-sm font-medium text-muted-foreground ml-1", labelClassName)}
      >
        {label}
      </label>
      {React.cloneElement(children as React.ReactElement<Record<string, unknown>>, {
        id,
        "aria-invalid": hasError || undefined,
        "aria-describedby": describedBy,
      })}
      {hasError ? (
        <p id={descId} role="alert" className="text-xs text-destructive ml-1">
          {error}
        </p>
      ) : hint ? (
        <p id={descId} className="text-xs text-muted-foreground/60 ml-1">
          {hint}
        </p>
      ) : null}
    </div>
  );
}