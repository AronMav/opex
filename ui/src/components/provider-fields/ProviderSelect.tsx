"use client";

import { Link2 } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useProviders } from "@/lib/queries";

const NONE = "__none__";

export interface ProviderSelectProps {
  value: string;
  onChange: (name: string) => void;
  /** Provider `type` categories to offer (e.g. ["text","llm"] for LLM slots —
   *  `llm` is the legacy alias for `text`). */
  categories: string[];
  /** Adds a "—" item that maps to "" (routing rules use it to unset the rule). */
  allowNone?: boolean;
  placeholder?: string;
  disabled?: boolean;
  size?: "sm" | "default";
  className?: string;
  id?: string;
}

/** Unified provider picker: name + default_model secondary label, filtered by
 *  capability categories. Data comes from useProviders (React Query). */
export function ProviderSelect({
  value,
  onChange,
  categories,
  allowNone = false,
  placeholder,
  disabled,
  size = "default",
  className,
  id,
}: ProviderSelectProps) {
  const { t } = useTranslation();
  const { data: providers = [] } = useProviders();
  const options = providers.filter((p) => categories.includes(p.type));

  return (
    <Select
      value={value === "" ? (allowNone ? NONE : "") : value}
      onValueChange={(v) => onChange(v === NONE ? "" : v)}
      disabled={disabled}
    >
      <SelectTrigger id={id} size={size} className={className}>
        <SelectValue placeholder={placeholder ?? t("profiles.provider_placeholder")} />
      </SelectTrigger>
      <SelectContent>
        {allowNone && (
          <SelectItem value={NONE} className="text-xs text-muted-foreground">
            <span className="text-muted-foreground">&mdash;</span>
          </SelectItem>
        )}
        {options.map((p) => (
          <SelectItem key={p.name} value={p.name} className="text-xs">
            <span className="flex min-w-0 items-center gap-2">
              <Link2 className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate">{p.name}</span>
              {p.default_model && (
                <span className="truncate text-2xs text-muted-foreground-subtle">{p.default_model}</span>
              )}
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
