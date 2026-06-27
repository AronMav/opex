"use client";

import { useCallback, useRef, useEffect, useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { markdown } from "@codemirror/lang-markdown";
import { keymap, EditorView } from "@codemirror/view";
import { imageDecorations, resolveAssetPath, findImageMatches, setUrls, urlField } from "./md-decorations/images";
import { wikiLinkDecorations } from "./md-decorations/wikilinks";
import { calloutDecorations } from "./md-decorations/callouts";
import { frontmatterDecorations } from "./md-decorations/frontmatter";
import { signWorkspacePaths } from "@/lib/api";

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
  // Monotonic counter to drop stale concurrent fetch results (Fix 2).
  const ensureSignedSeqRef = useRef(0);

  // Fix 1: clear cache when noteDir changes so expired entries from a previous
  // note do not bleed into the new note and so memory is bounded per note.
  useEffect(() => {
    urlCacheRef.current = {};
    ensureSignedSeqRef.current = 0;
  }, [noteDir]);

  const ensureSigned = useCallback(async (doc: string) => {
    // Fix 2: capture sequence before the async boundary.
    const seq = ++ensureSignedSeqRef.current;
    const need = new Set<string>();
    for (const m of findImageMatches(doc)) {
      const p = resolveAssetPath(noteDir, m.src);
      if (p && !urlCacheRef.current[p]) need.add(p);
    }
    if (need.size) {
      const map = await signWorkspacePaths([...need]);
      // Fix 2: discard if a newer call has already superseded this one.
      if (ensureSignedSeqRef.current !== seq) return;
      urlCacheRef.current = { ...urlCacheRef.current, ...map };
    }
    // Always push the current cache so the plugin rebuilds with URLs — covers the
    // mount case where the earlier call ran before viewRef was captured.
    viewRef.current?.dispatch({ effects: setUrls.of(urlCacheRef.current) });
  }, [noteDir]);

  // Fix 3: on image load error, evict the stale URL and re-sign so the image recovers.
  const onImageError = useCallback((assetPath: string) => {
    delete urlCacheRef.current[assetPath];
    ensureSigned(viewRef.current?.state.doc.toString() ?? "");
  }, [ensureSigned]);

  useEffect(() => { ensureSigned(value); }, [value, ensureSigned]);

  const saveKeymap = useMemo(() => keymap.of([{
    key: "Mod-s",
    run: () => { onSaveRef.current?.(); return true; },
  }]), []);

  const onNavigateRef = useRef(onNavigate);
  useEffect(() => { onNavigateRef.current = onNavigate; }, [onNavigate]);

  const extensions = useMemo(
    () => [
      markdown(),
      saveKeymap,
      EditorView.lineWrapping,
      urlField,
      imageDecorations({ noteDir, getUrl: (p) => urlCacheRef.current[p], onImageError }),
      wikiLinkDecorations((t) => onNavigateRef.current?.(t)),
      calloutDecorations(),
      frontmatterDecorations(),
    ],
    // noteDir change rebuilds the decoration extension with updated resolver
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [saveKeymap, noteDir, onImageError],
  );

  const handleChange = useCallback((v: string) => onChange(v), [onChange]);

  return (
    <div className="flex-1 min-h-0 overflow-hidden">
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
