import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, WidgetType } from "@codemirror/view";
import { type Extension, RangeSetBuilder, StateEffect, StateField } from "@codemirror/state";

export const setUrls = StateEffect.define<Record<string, string>>();
export const urlField = StateField.define<Record<string, string>>({
  create: () => ({}),
  update(value, tr) {
    for (const e of tr.effects) if (e.is(setUrls)) value = { ...value, ...e.value };
    return value;
  },
});

const IMG_RE = /!\[[^\]]*\]\(([^)\s]+)\)/g;

export function resolveAssetPath(noteDir: string, src: string): string | null {
  if (/^https?:\/\//i.test(src) || src.startsWith("/")) return null;
  return noteDir ? `${noteDir}/${src}` : src;
}

export function findImageMatches(text: string): { from: number; to: number; src: string }[] {
  const out: { from: number; to: number; src: string }[] = [];
  for (const m of text.matchAll(IMG_RE)) {
    out.push({ from: m.index!, to: m.index! + m[0].length, src: m[1] });
  }
  return out;
}

class ImageWidget extends WidgetType {
  constructor(readonly url: string | undefined, readonly alt: string) { super(); }
  eq(o: ImageWidget) { return o.url === this.url; }
  toDOM() {
    const wrap = document.createElement("div");
    wrap.className = "cm-md-image";
    wrap.style.display = "block";
    if (this.url) {
      const img = document.createElement("img");
      img.src = this.url; img.alt = this.alt;
      img.style.maxWidth = "100%"; img.style.borderRadius = "6px";
      wrap.appendChild(img);
    } else {
      wrap.textContent = "🖼 …"; // placeholder until signed URL arrives
      wrap.style.opacity = "0.5";
    }
    return wrap;
  }
}

export function imageDecorations(opts: {
  noteDir: string;
  getUrl: (path: string) => string | undefined;
}): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) { this.decorations = this.build(view); }
      update(u: ViewUpdate) {
        const urlsChanged = u.transactions.some((tr) => tr.effects.some((e) => e.is(setUrls)));
        if (u.docChanged || u.viewportChanged || u.selectionSet || urlsChanged) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        const cursor = view.state.selection.main.head;
        for (const { from, to } of view.visibleRanges) {
          const text = view.state.doc.sliceString(from, to);
          for (const m of findImageMatches(text)) {
            const mf = from + m.from, mt = from + m.to;
            if (cursor >= mf && cursor <= mt) continue; // show raw syntax on cursor line
            const resolved = resolveAssetPath(opts.noteDir, m.src);
            const url = resolved ? opts.getUrl(resolved) : m.src;
            // REPLACE the `![](...)` source range with the image widget (Live Preview:
            // hide markup, show render). NOT a trailing block widget — that would show
            // source AND image together.
            b.add(mf, mt, Decoration.replace({ widget: new ImageWidget(url, m.src) }));
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
}
