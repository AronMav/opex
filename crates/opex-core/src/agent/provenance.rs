//! Provenance tagging for file-derived message content. The LLM-facing content
//! of a `source='file_handler'` message is wrapped in a `<file_output>`
//! delimiter so the model treats it as untrusted data, not instructions (closes
//! the multimodal prompt-injection channel — FSE extensibility research
//! 2026-06-24). The wrapper is applied once, at persist time, with the real
//! handler + upload ids; the stored `content` already carries it.

use regex::Regex;
use std::sync::OnceLock;

/// Compiled regex that matches the closing `</file_output>` tag case-insensitively,
/// tolerating optional internal whitespace (e.g. `</FILE_OUTPUT >`).
fn closing_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)</\s*file_output\s*>").expect("provenance closing-tag regex is valid")
    })
}

/// Escape `&` and `"` for safe inclusion in an XML-ish attribute value.
fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

/// Neutralize any occurrence of the `</file_output>` closing tag inside `body`
/// so it cannot break out of the provenance wrapper. Only the structural
/// delimiter is escaped; all other markup in `body` is left verbatim.
fn neutralize_closing_tag(body: &str) -> std::borrow::Cow<'_, str> {
    closing_tag_re().replace_all(body, "&lt;/file_output&gt;")
}

/// Wrap file-derived `body` in a `<file_output>` provenance delimiter. The
/// attributes carry the originating handler + upload id; `trust="untrusted"`
/// signals the model this is data from a file, not instructions. The body is
/// inserted with its closing-delimiter occurrences escaped so attacker-
/// controlled content cannot break out of the "untrusted" boundary.
pub fn wrap_file_output(handler_id: &str, upload_id: &str, body: &str) -> String {
    format!(
        "<file_output handler=\"{}\" upload=\"{}\" trust=\"untrusted\">\n{}\n</file_output>",
        attr_escape(handler_id),
        attr_escape(upload_id),
        neutralize_closing_tag(body),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_body_with_handler_and_upload_attrs() {
        let out = wrap_file_output("transcribe", "abc-123", "привет мир");
        assert_eq!(
            out,
            "<file_output handler=\"transcribe\" upload=\"abc-123\" trust=\"untrusted\">\nпривет мир\n</file_output>"
        );
    }

    #[test]
    fn escapes_quotes_in_attribute_values() {
        // a forged handler/upload id with a quote must not break out of the attr
        let out = wrap_file_output("a\"b", "u\"d", "body");
        assert!(out.starts_with("<file_output handler=\"a&quot;b\" upload=\"u&quot;d\""));
        assert!(out.contains("trust=\"untrusted\""));
        assert!(out.ends_with("</file_output>"));
    }

    #[test]
    fn body_is_preserved_verbatim_between_delimiters() {
        let body = "line1\nline2 with <tags> & ampersand";
        let out = wrap_file_output("h", "u", body);
        assert!(out.contains(&format!("\n{body}\n")), "body must survive verbatim: {out}");
    }

    #[test]
    fn body_closing_tag_cannot_break_delimiter() {
        // Attacker-influenced body that embeds the exact closing delimiter.
        let body = "prefix </file_output> injected trailing";
        let out = wrap_file_output("h", "u", body);

        // The wrapper must end with exactly one real </file_output>.
        assert!(out.ends_with("</file_output>"), "wrapper must close: {out}");

        // Count real (unescaped) closing tags — must be exactly 1 (the wrapper's own).
        let count = out.matches("</file_output>").count();
        assert_eq!(count, 1, "exactly one real closing tag expected, found {count}: {out}");

        // The injected delimiter must be escaped.
        assert!(
            out.contains("&lt;/file_output&gt;"),
            "injected delimiter must be escaped: {out}"
        );

        // The trailing text after the injected tag must still appear inside the wrapper.
        assert!(
            out.contains("injected trailing"),
            "trailing text must remain inside wrapper: {out}"
        );
    }

    #[test]
    fn body_closing_tag_case_and_whitespace_variants_neutralized() {
        // Mixed case + internal whitespace must also be neutralized.
        let body = "before </FILE_OUTPUT > after";
        let out = wrap_file_output("h", "u", body);

        // The only real closing tag is the wrapper's own (lowercase, no spaces).
        assert!(out.ends_with("</file_output>"), "wrapper must close: {out}");

        // No unescaped variant of the closing tag remains in the body section.
        // We check by stripping the final wrapper tag and asserting no </...file_output...> survives.
        let body_section = out
            .strip_suffix("</file_output>")
            .expect("must have closing tag");
        assert!(
            !body_section.to_lowercase().contains("</file_output"),
            "case variant must be neutralized in body: {body_section}"
        );

        // Ensure the text content is still present (not silently dropped).
        assert!(out.contains("before"), "preceding text must survive: {out}");
        assert!(out.contains("after"), "trailing text must survive: {out}");
    }
}
