export interface DescriptorFields {
  id: string;
  labels: Record<string, string>;
  descriptions: Record<string, string>;
  icon: string;
  mime: string[];
  max_size_mb: number | null;
  execution: "sync" | "async";
  order: number;
  enabled: boolean;
}

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** Render the `# <handler> … # </handler>` comment block from descriptor fields. */
export function renderDescriptorBlock(f: DescriptorFields): string {
  const L: string[] = ["# <handler>", `#   <id>${esc(f.id)}</id>`];
  for (const [lang, txt] of Object.entries(f.labels)) L.push(`#   <label lang="${esc(lang)}">${esc(txt)}</label>`);
  for (const [lang, txt] of Object.entries(f.descriptions)) if (txt) L.push(`#   <description lang="${esc(lang)}">${esc(txt)}</description>`);
  if (f.icon) L.push(`#   <icon>${esc(f.icon)}</icon>`);
  L.push("#   <match>");
  for (const m of f.mime) L.push(`#     <mime>${esc(m)}</mime>`);
  if (f.max_size_mb != null) L.push(`#     <max_size_mb>${f.max_size_mb}</max_size_mb>`);
  L.push("#   </match>");
  L.push(`#   <execution>${f.execution}</execution>`);
  L.push(`#   <order>${f.order}</order>`);
  L.push(`#   <enabled>${f.enabled}</enabled>`);
  L.push("# </handler>");
  return L.join("\n");
}

/** Replace an existing leading descriptor block (or prepend one) in `source`. */
export function spliceDescriptor(source: string, f: DescriptorFields): string {
  const block = renderDescriptorBlock(f);
  const re = /# <handler>[\s\S]*?# <\/handler>/;
  return re.test(source) ? source.replace(re, block) : `${block}\n${source}`;
}
