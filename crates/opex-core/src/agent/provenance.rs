//! Provenance tagging for file-derived message content. The LLM-facing content
//! of a `source='file_handler'` message is wrapped in a `<file_output>`
//! delimiter so the model treats it as untrusted data, not instructions (closes
//! the multimodal prompt-injection channel — FSE extensibility research
//! 2026-06-24). The wrapper is applied once, at persist time, with the real
//! handler + upload ids; the stored `content` already carries it.

/// Escape `&` and `"` for safe inclusion in an XML-ish attribute value.
fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

/// Wrap file-derived `body` in a `<file_output>` provenance delimiter. The
/// attributes carry the originating handler + upload id; `trust="untrusted"`
/// signals the model this is data from a file, not instructions. The body is
/// inserted verbatim (only the attribute values are escaped) so the LLM sees
/// the exact processed text on its own line.
pub fn wrap_file_output(handler_id: &str, upload_id: &str, body: &str) -> String {
    format!(
        "<file_output handler=\"{}\" upload=\"{}\" trust=\"untrusted\">\n{}\n</file_output>",
        attr_escape(handler_id),
        attr_escape(upload_id),
        body
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
}
