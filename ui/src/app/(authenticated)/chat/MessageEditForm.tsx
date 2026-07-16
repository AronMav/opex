"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { X, Send } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";

export function MessageEditForm({
  initialText,
  onSubmit,
  onCancel,
}: {
  initialText: string;
  onSubmit: (text: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState(initialText);

  return (
    <div className="flex w-full flex-col gap-2">
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") { e.preventDefault(); onCancel(); }
          if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); onSubmit(text); }
        }}
        className="min-h-20 w-full resize-none rounded-lg border border-border bg-background px-3 py-2 text-sm text-foreground outline-none focus:border-primary/50"
        autoFocus
      />
      <div className="flex items-center justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          <X className="h-4 w-4 mr-1" />
          {t("common.cancel")}
        </Button>
        <Button variant="ghost" size="sm" onClick={() => onSubmit(text)} className="text-primary">
          <Send className="h-4 w-4 mr-1" />
          {t("common.save")}
        </Button>
      </div>
    </div>
  );
}
