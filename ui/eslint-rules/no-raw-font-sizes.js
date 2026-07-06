/**
 * Forbids arbitrary font-size values (`text-[10px]`, `text-[0.875rem]`) across
 * the whole `src/` tree. Use the design-system font tokens instead:
 * `text-3xs` (10px), `text-2xs` (11px), `text-code` (13px), `text-message` (15px),
 * or the standard Tailwind scale (`text-xs`, `text-sm`, …).
 *
 * Unlike `no-raw-design-values` (which is page-scoped and also blocks raw palette
 * colors + arbitrary px/rem dims), this rule is narrow: it only governs font
 * sizes and is safe to apply to primitives and feature components alike.
 */
const FORBIDDEN_FONT = /\btext-\[\d+(?:\.\d+)?(?:px|rem)\]/;

/** @type {import('eslint').Rule.RuleModule} */
const rule = {
  meta: {
    type: "problem",
    docs: {
      description: "Forbid arbitrary font-size values; require design-system font tokens.",
    },
    schema: [],
    messages: {},
  },
  create(context) {
    function check(node, raw) {
      if (typeof raw !== "string") return;
      if (FORBIDDEN_FONT.test(raw)) {
        context.report({
          node,
          message:
            "Use a font-size token (text-3xs/text-2xs/text-code/text-message or the Tailwind scale) instead of an arbitrary text-[Npx]/text-[Nrem] value.",
        });
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
