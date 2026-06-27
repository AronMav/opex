import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const HEADER_RE = /^>\s*\[!(\w+)\]([-+]?)\s*(.*)$/;

export function parseCalloutHeader(line: string) {
  const m = HEADER_RE.exec(line);
  if (!m) return null;
  return { type: m[1].toLowerCase(), collapsible: m[2] === "-" || m[2] === "+", title: m[3].trim() };
}

const headerDeco = Decoration.line({ class: "cm-callout-header" });
const bodyDeco = Decoration.line({ class: "cm-callout-body" });

export function calloutDecorations(): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        for (const { from, to } of view.visibleRanges) {
          let pos = from;
          while (pos <= to) {
            const line = view.state.doc.lineAt(pos);
            const text = line.text;
            if (parseCalloutHeader(text)) b.add(line.from, line.from, headerDeco);
            else if (text.startsWith(">")) b.add(line.from, line.from, bodyDeco);
            pos = line.to + 1;
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
  const theme = EditorView.baseTheme({
    ".cm-callout-header": { borderLeft: "3px solid #7aa2f7", paddingLeft: "8px", fontWeight: "600", background: "rgba(122,162,247,0.08)" },
    ".cm-callout-body": { borderLeft: "3px solid #7aa2f7", paddingLeft: "8px", background: "rgba(122,162,247,0.04)" },
  });
  return [plugin, theme];
}
