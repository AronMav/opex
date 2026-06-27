import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const FM_RE = /^---\n[\s\S]*?\n---/;

export function frontmatterRange(doc: string): { from: number; to: number } | null {
  const m = FM_RE.exec(doc);
  if (!m || m.index !== 0) return null;
  return { from: 0, to: m[0].length };
}

export function frontmatterDecorations(): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) { if (u.docChanged) this.decorations = this.build(u.view); }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        const r = frontmatterRange(view.state.doc.toString());
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
    ".cm-frontmatter": { background: "rgba(128,128,128,0.10)", color: "#9aa5b1", fontStyle: "italic" },
  });
  return [plugin, theme];
}
