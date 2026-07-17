"use client";

import { useState, useCallback, useEffect, useRef } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { CodeEditor } from "@/components/workspace/code-editor";
import { Button } from "@/components/ui/button";

interface ApprovalArgsEditorProps {
  initialInput: Record<string, unknown>;
  onSubmit: (modified: Record<string, unknown>) => void;
  onCancel: () => void;
}

export function ApprovalArgsEditor({ initialInput, onSubmit, onCancel }: ApprovalArgsEditorProps) {
  const { t } = useTranslation();
  const [value, setValue] = useState(() => JSON.stringify(initialInput, null, 2));
  const [isValid, setIsValid] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const handleChange = useCallback(
    (newValue: string) => {
      setValue(newValue);
      try {
        JSON.parse(newValue);
        setIsValid(true);
        setError(null);
      } catch {
        setIsValid(false);
        setError(t("chat.approval_invalid_json"));
      }
    },
    [t],
  );

  const handleSubmit = useCallback(() => {
    if (!isValid) return;
    try {
      const parsed = JSON.parse(value);
      onSubmit(parsed);
    } catch {
      setIsValid(false);
      setError(t("chat.approval_invalid_json"));
    }
  }, [isValid, value, onSubmit, t]);

  // Escape key cancels editing (only when focus is inside this editor)
  const containerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape" && containerRef.current?.contains(document.activeElement)) {
        e.preventDefault();
        onCancel();
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onCancel]);

  return (
    <div ref={containerRef} aria-label={t("chat.edit_tool_args")}>
      <div className="min-h-30 max-h-75 overflow-hidden rounded border border-border/50">
        <CodeEditor
          value={value}
          onChange={handleChange}
          language="json"
        />
      </div>
      {error && (
        <p className="text-destructive text-xs mt-1">{error}</p>
      )}
      <div className="flex items-center justify-end gap-2 mt-2">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="default"
          size="sm"
          className={`bg-primary ${!isValid ? "opacity-50 cursor-not-allowed" : ""}`}
          disabled={!isValid}
          onClick={handleSubmit}
        >
          {t("chat.approval_submit_modified")}
        </Button>
      </div>
    </div>
  );
}
