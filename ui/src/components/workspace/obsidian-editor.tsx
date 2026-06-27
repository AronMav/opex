"use client";

import { useCallback, useRef, useEffect, useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { markdown } from "@codemirror/lang-markdown";
import { keymap, EditorView } from "@codemirror/view";
import { StateEffect, StateField } from "@codemirror/state";
import { imageDecorations, resolveAssetPath, findImageMatches } from "./md-decorations/images";
import { signWorkspacePaths } from "@/lib/api";

// ── URL cache state ──────────────────────────────────────────────────────────
const setUrls = StateEffect.define<Record<string, string>>();
const urlField = StateField.define<Record<string, string>>({
  create: () => ({}),
  update(value, tr) {
    for (const e of tr.effects) if (e.is(setUrls)) value = { ...value, ...e.value };
    return value;
  },
});

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

  const urlCacheRef = useRef<Record<string, string>>({});
  const viewRef = useRef<EditorView | null>(null);

  const ensureSigned = useCallback(async (doc: string) => {
    const need = new Set<string>();
    for (const m of findImageMatches(doc)) {
      const p = resolveAssetPath(noteDir, m.src);
      if (p && !urlCacheRef.current[p]) need.add(p);
    }
    if (!need.size) return;
    const map = await signWorkspacePaths([...need]);
    urlCacheRef.current = { ...urlCacheRef.current, ...map };
    viewRef.current?.dispatch({ effects: setUrls.of(map) });
  }, [noteDir]);

  useEffect(() => { ensureSigned(value); }, [value, ensureSigned]);

  const saveKeymap = useMemo(() => keymap.of([{
    key: "Mod-s",
    run: () => { onSaveRef.current?.(); return true; },
  }]), []);

  const extensions = useMemo(
    () => [
      markdown(),
      saveKeymap,
      EditorView.lineWrapping,
      urlField,
      imageDecorations({ noteDir, getUrl: (p) => urlCacheRef.current[p] }),
    ],
    // noteDir change rebuilds the decoration extension with updated resolver
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [saveKeymap, noteDir],
  );

  const handleChange = useCallback((v: string) => onChange(v), [onChange]);

  // onNavigate is consumed by wiki-link decoration extension (Task 12).
  void onNavigate;

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
        onCreateEditor={(view) => { viewRef.current = view; ensureSigned(value); }}
      />
    </div>
  );
}
