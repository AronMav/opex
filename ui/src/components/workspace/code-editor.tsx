"use client";

import { useCallback, useRef, useMemo, useEffect } from "react";
import { useTheme } from "next-themes";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { yaml } from "@codemirror/lang-yaml";
import { StreamLanguage } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { keymap } from "@codemirror/view";
import type { Extension } from "@codemirror/state";

interface CodeEditorProps {
  value: string;
  onChange: (value: string) => void;
  onSave?: () => void;
  language?: string;
}

function getExtension(lang: string | undefined): Extension[] {
  switch (lang) {
    case "json":
      return [json()];
    case "yaml":
    case "yml":
      return [yaml()];
    case "toml":
      return [StreamLanguage.define(toml)];
    case "md":
    case "markdown":
      return [markdown()];
    default:
      // Unknown types render as plain text (no language extension).
      return [];
  }
}

function getLangFromFilename(filename: string): string | undefined {
  const ext = filename.split(".").pop()?.toLowerCase();
  switch (ext) {
    case "json":
      return "json";
    case "toml":
      return "toml";
    case "yaml":
    case "yml":
      return "yaml";
    case "md":
      return "md";
    default:
      return undefined;
  }
}

export { getLangFromFilename };

export function CodeEditor({ value, onChange, onSave, language }: CodeEditorProps) {
  const { resolvedTheme } = useTheme();
  const handleChange = useCallback(
    (val: string) => {
      onChange(val);
    },
    [onChange],
  );

  const onSaveRef = useRef(onSave);
  useEffect(() => { onSaveRef.current = onSave; }, [onSave]);
  // The `run` callback only fires on user keypress (never during render), so
  // reading onSaveRef.current here is the intended ref pattern — the lint
  // heuristic cannot prove that and would false-positive without the disable.
  // eslint-disable-next-line react-hooks/refs
  const saveKeymap = useMemo(() => keymap.of([{
    key: "Mod-s",
    run: () => { onSaveRef.current?.(); return true; },
  }]), []);

  return (
    <div className="flex-1 min-h-0 overflow-hidden">
      <CodeMirror
        value={value}
        onChange={handleChange}
        theme={resolvedTheme === "dark" ? oneDark : "light"}
        extensions={[...getExtension(language), saveKeymap]}
        basicSetup={{
          lineNumbers: true,
          foldGutter: true,
          highlightActiveLine: true,
          bracketMatching: true,
          indentOnInput: true,
          tabSize: 2,
        }}
        className="h-full [&_.cm-editor]:h-full [&_.cm-scroller]:overflow-auto"
        height="100%"
      />
    </div>
  );
}
