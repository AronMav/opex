"use client";

import { useCallback, useRef, useMemo, useEffect } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { keymap } from "@codemirror/view";

interface CodeEditorProps {
  value: string;
  onChange: (value: string) => void;
  onSave?: () => void;
  language?: string;
}

function getExtension(lang: string | undefined) {
  switch (lang) {
    case "json":
      return json();
    case "md":
    case "markdown":
      return markdown();
    default:
      return markdown();
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
  const handleChange = useCallback(
    (val: string) => {
      onChange(val);
    },
    [onChange],
  );

  const onSaveRef = useRef(onSave);
  useEffect(() => { onSaveRef.current = onSave; }, [onSave]);
  const saveKeymap = useMemo(() => keymap.of([{
    key: "Mod-s",
    run: () => { onSaveRef.current?.(); return true; },
  }]), []);

  return (
    <div className="flex-1 min-h-0 overflow-hidden">
      <CodeMirror
        value={value}
        onChange={handleChange}
        theme={oneDark}
        extensions={[getExtension(language), saveKeymap]}
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
