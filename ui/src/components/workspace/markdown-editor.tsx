"use client";

import { useEffect, useRef, useCallback } from "react";
import { useEditor, EditorContent } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import Placeholder from "@tiptap/extension-placeholder";
import { Markdown } from "tiptap-markdown";

interface MarkdownStorage {
  markdown: { getMarkdown: () => string };
  [key: string]: unknown;
}

interface MarkdownEditorProps {
  value: string;
  onChange: (value: string) => void;
  onSave?: () => void;
}

export function MarkdownEditor({ value, onChange, onSave }: MarkdownEditorProps) {
  const onChangeRef = useRef(onChange);
  const onSaveRef = useRef(onSave);
  const suppressUpdate = useRef(false);

  useEffect(() => { onChangeRef.current = onChange; }, [onChange]);
  useEffect(() => { onSaveRef.current = onSave; }, [onSave]);

  const editor = useEditor({
    extensions: [
      StarterKit.configure({
        heading: { levels: [1, 2, 3, 4] },
        codeBlock: { HTMLAttributes: { class: "hljs" } },
      }),
      Placeholder.configure({ placeholder: "Start writing..." }),
      Markdown.configure({
        html: false,
        transformPastedText: true,
        transformCopiedText: true,
      }),
    ],
    content: value,
    editorProps: {
      attributes: {
        class:
          "prose prose-sm dark:prose-invert max-w-none min-h-full p-4 outline-none " +
          "[&_h1]:text-xl [&_h1]:font-bold [&_h1]:mb-3 [&_h1]:mt-6 " +
          "[&_h2]:text-lg [&_h2]:font-semibold [&_h2]:mb-2 [&_h2]:mt-5 " +
          "[&_h3]:text-base [&_h3]:font-semibold [&_h3]:mb-2 [&_h3]:mt-4 " +
          "[&_p]:mb-2 [&_p]:leading-relaxed " +
          "[&_ul]:list-disc [&_ul]:pl-6 [&_ul]:mb-2 " +
          "[&_ol]:list-decimal [&_ol]:pl-6 [&_ol]:mb-2 " +
          "[&_li]:mb-0.5 " +
          "[&_blockquote]:border-l-2 [&_blockquote]:border-primary/50 [&_blockquote]:pl-4 [&_blockquote]:italic [&_blockquote]:text-muted-foreground " +
          "[&_code]:rounded [&_code]:bg-muted [&_code]:px-1.5 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-code " +
          "[&_pre]:rounded-md [&_pre]:bg-muted [&_pre]:p-3 [&_pre]:font-mono [&_pre]:text-code [&_pre]:overflow-x-auto [&_pre]:mb-3 " +
          "[&_hr]:border-border [&_hr]:my-4 " +
          "[&_strong]:font-bold [&_em]:italic " +
          "[&_.is-editor-empty:first-child::before]:text-muted-foreground [&_.is-editor-empty:first-child::before]:float-left [&_.is-editor-empty:first-child::before]:content-[attr(data-placeholder)] [&_.is-editor-empty:first-child::before]:pointer-events-none [&_.is-editor-empty:first-child::before]:h-0",
      },
      handleKeyDown: (_view, event) => {
        if ((event.ctrlKey || event.metaKey) && event.key === "s") {
          event.preventDefault();
          onSaveRef.current?.();
          return true;
        }
        return false;
      },
    },
    onUpdate: ({ editor: e }) => {
      if (suppressUpdate.current) return;
      const md = (e.storage as unknown as MarkdownStorage).markdown.getMarkdown();
      onChangeRef.current(md);
    },
  });

  const setContent = useCallback(
    (md: string) => {
      if (!editor) return;
      suppressUpdate.current = true;
      editor.commands.setContent(md);
      suppressUpdate.current = false;
    },
    [editor],
  );

  useEffect(() => {
    if (!editor) return;
    const currentMd = (editor.storage as unknown as MarkdownStorage).markdown.getMarkdown();
    if (value !== currentMd) {
      setContent(value);
    }
  }, [value, editor, setContent]);

  return (
    <div className="flex-1 overflow-y-auto bg-background text-sm focus-within:ring-1 focus-within:ring-primary/40">
      <EditorContent editor={editor} className="min-h-full" />
    </div>
  );
}
