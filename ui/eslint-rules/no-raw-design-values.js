/**
 * Forbids raw design values in page className strings. Keeps them confined to
 * tokens (globals.css) and primitives (components/ui). Scope via flat-config
 * `files` — do NOT enable globally until all pages are migrated.
 */
const FORBIDDEN = [
  { re: /\btext-\[(?:10|11)px\]/, msg: "Use text-2xs/text-3xs instead of an arbitrary font size." },
  { re: /\bneu-(?:card|flat)\b/, msg: "Use the <Card> primitive instead of the .neu-card/.neu-flat utility." },
  {
    re: /\b(?:bg|text|border|ring|fill|stroke|from|to|via)-(?:gray|slate|zinc|neutral|stone|blue|emerald|green|amber|yellow|purple|violet|cyan|teal|orange|sky|rose|indigo|pink|red)-\d{2,3}\b/,
    msg: "Use semantic/chart tokens instead of raw Tailwind palette colors.",
  },
  { re: /-\[\d+(?:\.\d+)?(?:px|rem)\]/, msg: "Use a design token instead of an arbitrary px/rem value." },
];

/** @type {import('eslint').Rule.RuleModule} */
const rule = {
  meta: {
    type: "problem",
    docs: { description: "Forbid raw design values in app pages; keep them in tokens/primitives." },
    schema: [],
    messages: {},
  },
  create(context) {
    function check(node, raw) {
      if (typeof raw !== "string") return;
      for (const { re, msg } of FORBIDDEN) {
        if (re.test(raw)) {
          context.report({ node, message: msg });
          break;
        }
      }
    }
    return {
      Literal(node) {
        check(node, node.value);
      },
      TemplateElement(node) {
        check(node, node.value.raw);
      },
    };
  },
};

module.exports = rule;
