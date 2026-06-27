import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import { type Extension, type Text, RangeSetBuilder } from "@codemirror/state";

const FM_RE = /^---\r?\n[\s\S]*?\r?\n---/;

export function frontmatterRange(doc: string): { from: number; to: number } | null {
  const m = FM_RE.exec(doc);
  if (!m || m.index !== 0) return null;
  return { from: 0, to: m[0].length };
}

/** Read only the first `maxLines` lines of a CM Text object to a string. */
function readDocHead(doc: Text, maxLines: number): string {
  const lines: string[] = [];
  const iter = doc.iterLines();
  while (!iter.done && lines.length < maxLines) {
    iter.next();
    lines.push(iter.value);
  }
  return lines.join("\n");
}

export function frontmatterDecorations(): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) { if (u.docChanged) this.decorations = this.build(u.view); }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        // Read only the first 50 lines instead of stringifying the whole document.
        const head = readDocHead(view.state.doc, 50);
        const r = frontmatterRange(head);
        if (r) {
          const start = view.state.doc.lineAt(r.from).number;
          const end = view.state.doc.lineAt(r.to).number;
          for (let n = start; n <= end; n++) {
            const line = view.state.doc.line(n);
            b.add(line.from, line.from, Decoration.line({ class: "cm-frontmatter" }));
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
  const theme = EditorView.baseTheme({
    ".cm-frontmatter": { background: "color-mix(in srgb, var(--muted-foreground, #9aa5b1) 10%, transparent)", color: "var(--muted-foreground, #9aa5b1)", fontStyle: "italic" },
  });
  return [plugin, theme];
}
