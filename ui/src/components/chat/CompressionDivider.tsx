"use client";

import { useTranslation } from "@/hooks/use-translation";

interface Props {
  segmentIndex: number;
  totalSegments: number;
}

export function CompressionDivider({ segmentIndex, totalSegments }: Props) {
  const { t } = useTranslation();
  return (
    <div
      role="separator"
      aria-label={t("chat.compression_divider", { current: segmentIndex, total: totalSegments })}
      className="flex items-center gap-3 my-4 px-4 select-none"
    >
      <div className="flex-1 h-px bg-border" aria-hidden />
      <span className="text-xs text-muted-foreground whitespace-nowrap">
        ◈ {t("chat.compression_divider", { current: segmentIndex, total: totalSegments })}
      </span>
      <div className="flex-1 h-px bg-border" aria-hidden />
    </div>
  );
}
