"use client";

import * as React from "react";
import { cn } from "@/lib/utils";

interface FieldProps {
  label: string;
  hint?: React.ReactNode;
  htmlFor?: string;
  className?: string;
  labelClassName?: string;
  children: React.ReactElement;
}

export function Field({ label, hint, htmlFor, className, labelClassName, children }: FieldProps) {
  const autoId = React.useId();
  const childProps = children.props as { id?: string };
  const id = htmlFor ?? (childProps.id ?? autoId);
  return (
    <div className={cn("space-y-2", className)}>
      <label
        htmlFor={id}
        className={cn("text-sm font-medium text-muted-foreground ml-1", labelClassName)}
      >
        {label}
      </label>
      {React.cloneElement(children as React.ReactElement<Record<string, unknown>>, { id })}
      {hint && <p className="text-xs text-muted-foreground/60 ml-1">{hint}</p>}
    </div>
  );
}