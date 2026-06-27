"use client";

import { useCallback, useRef, useEffect, useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { markdown } from "@codemirror/lang-markdown";
import { keymap, EditorView } from "@codemirror/view";

export interface ObsidianEditorProps {
  value: string;
  onChange: (v: string) => void;
  onSave?: () => void;
  /** Folder of the open note, workspace-relative — used to resolve relative image paths. */
  noteDir: string;
  /** Called when a [[wiki-link]] is clicked. */
  onNavigate?: (target: string) => void;
}

export function ObsidianEditor({ value, onChange, onSave, noteDir, onNavigate }: ObsidianEditorProps) {
  const onSaveRef = useRef(onSave);
  useEffect(() => { onSaveRef.current = onSave; }, [onSave]);

  const saveKeymap = useMemo(() => keymap.of([{
    key: "Mod-s",
    run: () => { onSaveRef.current?.(); return true; },
  }]), []);

  // Live Preview decoration extensions are appended here in Tasks 11-14.
  const extensions = useMemo(
    () => [markdown(), saveKeymap, EditorView.lineWrapping],
    [saveKeymap],
  );

  const handleChange = useCallback((v: string) => onChange(v), [onChange]);

  // noteDir / onNavigate are consumed by decoration extensions (Tasks 11-12).
  void noteDir; void onNavigate;

  return (
    <div className="flex-1 overflow-hidden">
      <CodeMirror
        value={value}
        onChange={handleChange}
        theme={oneDark}
        extensions={extensions}
        basicSetup={{ lineNumbers: false, foldGutter: false, highlightActiveLine: false }}
        className="h-full [&_.cm-editor]:h-full [&_.cm-scroller]:overflow-auto"
        height="100%"
      />
    </div>
  );
}
