import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import { type Extension, type Text, RangeSetBuilder } from "@codemirror/state";

const FM_RE = /^---\r?\n[\s\S]*?\r?\n---/;

export function frontmatterRange(doc: string): { from: number; to: number } | null {
  const m = FM_RE.exec(doc);
  if (!m || m.index !== 0) return null;
  return { from: 0, to: m[0].length };
}

/**
 * Given an array of line texts (already stripped of line endings, as CM's
 * `line(n).text` returns), find the 1-based line number of the closing `---`
 * of a leading frontmatter block.
 *
 * Returns null when:
 *   - line 1 is not `---`
 *   - no closing `---` is found within the first `maxLines` lines
 */
export function frontmatterEndLine(lines: string[], maxLines = 50): number | null {
  if (lines.length === 0 || lines[0].trim() !== "---") return null;
  for (let i = 1; i < Math.min(lines.length, maxLines); i++) {
    if (lines[i].trim() === "---") return i + 1; // convert 0-based index to 1-based line number
  }
  return null;
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
        // Collect line texts directly from the CM document (CM already strips \r).
        // This is CRLF-safe: we never mix a reconstructed-string byte-offset with
        // real-doc positions — everything is expressed in line numbers.
        const maxLines = 50;
        const totalLines = view.state.doc.lines;
        const cap = Math.min(totalLines, maxLines);
        const lineTexts: string[] = [];
        for (let n = 1; n <= cap; n++) {
          lineTexts.push(view.state.doc.line(n).text);
        }
        const endLine = frontmatterEndLine(lineTexts, maxLines);
        if (endLine !== null) {
          for (let n = 1; n <= endLine; n++) {
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
