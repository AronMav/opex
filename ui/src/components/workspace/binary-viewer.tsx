"use client";

import { useTranslation } from "@/hooks/use-translation";
import type { WorkspaceFile } from "@/types/api";

type BinaryFile = Extract<WorkspaceFile, { is_binary: true }>;

export function BinaryViewer({ file }: { file: BinaryFile }) {
  const { t } = useTranslation();

  if (file.mime.startsWith("image/")) {
    return (
      <div className="flex-1 min-h-0 flex items-center justify-center overflow-auto bg-background p-4">
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img src={file.url} alt={file.path} className="max-h-full max-w-full object-contain" />
      </div>
    );
  }
  if (file.mime === "application/pdf") {
    return <iframe src={file.url} title={file.path} className="flex-1 min-h-0 w-full border-0" />;
  }
  return (
    <div className="flex-1 flex flex-col items-center justify-center gap-3 text-muted-foreground">
      <span className="font-mono text-sm">{file.path}</span>
      <span className="text-xs">{(file.size / 1024).toFixed(1)} KB · {file.mime}</span>
      <a href={file.url} download className="text-primary underline text-sm">{t("workspace.download")}</a>
    </div>
  );
}
