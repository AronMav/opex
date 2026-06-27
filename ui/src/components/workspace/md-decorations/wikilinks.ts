import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, WidgetType } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const WIKI_RE = /\[\[([^\]]+)\]\]/g;

export function findWikiLinks(text: string) {
  const out: { from: number; to: number; target: string; label: string }[] = [];
  for (const m of text.matchAll(WIKI_RE)) {
    const label = m[1];
    const target = label.split("#")[0].trim();
    out.push({ from: m.index!, to: m.index! + m[0].length, target, label });
  }
  return out;
}

class WikiWidget extends WidgetType {
  constructor(readonly label: string, readonly target: string, readonly onNavigate: (t: string) => void) { super(); }
  eq(o: WikiWidget) { return o.label === this.label; }
  toDOM() {
    const a = document.createElement("span");
    a.className = "cm-wikilink";
    a.textContent = this.label;
    a.style.cssText = "color:var(--primary,#7aa2f7);cursor:pointer;text-decoration:underline";
    a.onmousedown = (e) => { e.preventDefault(); this.onNavigate(this.target); };
    return a;
  }
}

export function wikiLinkDecorations(onNavigate: (target: string) => void): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged || u.selectionSet) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        const cursor = view.state.selection.main.head;
        for (const { from, to } of view.visibleRanges) {
          const text = view.state.doc.sliceString(from, to);
          for (const m of findWikiLinks(text)) {
            const mf = from + m.from, mt = from + m.to;
            if (cursor >= mf && cursor <= mt) continue;
            b.add(mf, mt, Decoration.replace({ widget: new WikiWidget(m.label, m.target, onNavigate) }));
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
}
