export interface ParamDescriptor {
  name: string;
  type?: string;
  default?: string | null;
  required?: boolean;
}

/** An operator-configurable setting ("valve") declared in the handler's
 *  <config> descriptor block. Values are set per-agent in the settings tab. */
export interface ConfigFieldDescriptor {
  name: string;
  type?: string;
  default?: string | null;
  label?: string;
  description?: string;
}

export interface DescriptorFields {
  id: string;
  labels: Record<string, string>;
  descriptions: Record<string, string>;
  icon: string;
  mime: string[];
  domains: string[];
  max_size_mb: number | null;
  execution: "sync" | "async";
  order: number;
  enabled: boolean;
  /** Passthrough — not form-editable in v1, but must round-trip so editing a
   *  builtin's label doesn't silently strip <capability>/<output>/<params>. */
  capability?: string | null;
  output?: string | null;
  params?: ParamDescriptor[];
  /** Passthrough — operator-configurable field definitions. Must round-trip so
   *  editing a handler's descriptor doesn't strip its <config> block. */
  config?: ConfigFieldDescriptor[];
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
  for (const d of f.domains) L.push(`#     <domain>${esc(d)}</domain>`);
  L.push("#   </match>");
  if (f.capability) L.push(`#   <capability>${esc(f.capability)}</capability>`);
  L.push(`#   <execution>${f.execution}</execution>`);
  L.push(`#   <output>${esc(f.output ?? "text")}</output>`);
  if (f.params && f.params.length > 0) {
    L.push("#   <params>");
    for (const p of f.params) {
      let line = `#     <param name="${esc(p.name)}"`;
      if (p.type) line += ` type="${esc(p.type)}"`;
      if (p.default != null) line += ` default="${esc(p.default)}"`;
      line += ` required="${p.required === true ? "true" : "false"}"`;
      line += "/>";
      L.push(line);
    }
    L.push("#   </params>");
  }
  if (f.config && f.config.length > 0) {
    L.push("#   <config>");
    for (const c of f.config) {
      let line = `#     <field name="${esc(c.name)}"`;
      if (c.type) line += ` type="${esc(c.type)}"`;
      if (c.default != null) line += ` default="${esc(c.default)}"`;
      if (c.label) line += ` label="${esc(c.label)}"`;
      if (c.description) line += ` description="${esc(c.description)}"`;
      line += "/>";
      L.push(line);
    }
    L.push("#   </config>");
  }
  L.push(`#   <order>${f.order}</order>`);
  L.push(`#   <enabled>${f.enabled}</enabled>`);
  L.push("# </handler>");
  return L.join("\n");
}

/** Replace an existing leading descriptor block (or prepend one) in `source`.
 *  Tolerates `#<handler>` with zero or more spaces after `#` (matches
 *  descriptor.py's `_BLOCK_RE = re.compile(r"#\s*<handler>...")`). */
export function spliceDescriptor(source: string, f: DescriptorFields): string {
  const block = renderDescriptorBlock(f);
  const re = /#\s*<handler>[\s\S]*?#\s*<\/handler>/;
  return re.test(source) ? source.replace(re, block) : `${block}\n${source}`;
}
