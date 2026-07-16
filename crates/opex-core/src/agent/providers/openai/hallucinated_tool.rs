//! Live-stream + post-hoc suppressor for hallucinated extension-tool "calls".
//!
//! MCP/extension tools (e.g. `sequentialthinking`) are NOT in the model's native
//! `tools[]` schema — they are reached via the `tool_use(action="describe"/"call",
//! name=...)` dispatcher. A weak-prompt-adherence model (e.g. glm-5.2) sometimes
//! IGNORES that indirection and "invokes" the tool as FREE-FORM assistant
//! `content`, e.g.
//!
//! ```text
//! sequentialthinking
//! {"thought": "..."}
//! ```
//!
//! or `<sequentialthinking>...</sequentialthinking>`. Because no native function of
//! that name exists, the OpenAI-compatible provider streams it as `delta.content`
//! and the client renders raw JSON/XML as message body text. There is no generic
//! DURING-STREAM detector (the MiniMax XML extractor runs post-hoc on the full
//! response, so it cannot stop the live leak).
//!
//! [`HallucinatedToolFilter`] closes that gap. It is deliberately CONSERVATIVE.
//! It suppresses text ONLY when, at the START of the assistant message or
//! immediately after a newline (a "boundary"), it sees a **known** extension-tool
//! name immediately followed by tool-invocation syntax:
//!
//! - `<toolname>` / `<toolname ...>` — XML shape (suppressed through `</toolname>`)
//! - `toolname{...}` / `toolname\n{...}` / `toolname {...}` — JSON shape (suppressed
//!   through the balanced `}`)
//!
//! Deliberately NOT suppressed: `toolname(...)` — a function-call paren shape.
//! Models do not actually hallucinate `name(json)` (the observed shapes are the
//! two above); meanwhile a short function-like tool name (`fetch`, `search`) is
//! common in legitimate code examples like `fetch('https://x')` at a line start,
//! so treating `(` as invocation punctuation was an over-suppression risk with
//! no matching real-world hallucination pattern to justify it.
//!
//! ### What it will NEVER suppress (conservatism guarantees)
//! - Prose that merely *mentions* a tool name mid-sentence
//!   ("what does sequentialthinking do?") — the name is not at a boundary and/or
//!   is not immediately followed by invocation punctuation.
//! - A known tool name followed by ordinary text
//!   ("sequentialthinking is a reasoning tool") — no `{`/`<...>` after the name.
//! - A known tool name used as a function call in code
//!   ("fetch('https://x')", "search(query)") — the paren shape is never treated
//!   as invocation punctuation (see above).
//! - Any content when the known-tool list is empty — the filter is a pure
//!   passthrough (zero behaviour change when no extension tools are configured).
//! - A real native `tool_use` call — those arrive in the SSE `tool_calls` array,
//!   never as `delta.content`, so they never reach this filter.
//! - An unclosed/ambiguous suppression that grows past [`MAX_SUPPRESS_BYTES`] —
//!   fail-open: the buffered bytes are flushed as text rather than swallowed.
//!
//! When uncertain the filter buffers a few bytes and, on resolution, forwards
//! them as text — it never swallows ambiguous content.

use std::sync::Arc;

/// Fail-open cap: if a detected call-start never resolves to a close delimiter
/// within this many buffered bytes, treat it as NOT a call and flush as text.
/// Prevents a mis-detection (or a genuinely unclosed structure) from
/// permanently swallowing legitimate assistant text.
const MAX_SUPPRESS_BYTES: usize = 16 * 1024;

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Decision for the text at a boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Boundary {
    /// Definitely not a hallucinated call at this boundary.
    NoMatch,
    /// The buffer is a live prefix of a possible call — wait for more bytes.
    NeedMore,
    /// XML call detected; suppress through this close tag (any case).
    Xml(String),
    /// `name{...}` call detected; suppress through the balanced `}`.
    Brace,
}

/// Classify the text at a message/line boundary. `seg` MUST start at a boundary.
// reviewed: `i` counts ASCII whitespace bytes only — a char boundary.
#[allow(clippy::string_slice)]
fn classify_boundary(seg: &str, known: &[String]) -> Boundary {
    let bytes = seg.as_bytes();
    // Allow leading horizontal whitespace (indentation) before the name.
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i >= bytes.len() {
        // Only whitespace so far — a name could still follow.
        return Boundary::NeedMore;
    }
    // `i` counts ASCII whitespace bytes only → valid char boundary.
    let rest = &seg[i..];
    let first = rest.as_bytes()[0];
    if first == b'<' {
        classify_xml(rest, known)
    } else if first.is_ascii_alphabetic() {
        classify_name(rest, known)
    } else {
        Boundary::NoMatch
    }
}

/// `rest` starts with `<`. Match `<knownname>` / `<knownname ...>`.
// reviewed: leading '<' is ASCII — slicing past it is a char boundary.
#[allow(clippy::string_slice)]
fn classify_xml(rest: &str, known: &[String]) -> Boundary {
    // Skip the leading '<' (ASCII → char boundary).
    let after = &rest[1..];
    let ab = after.as_bytes();
    let mut any_prefix = false;
    for name in known {
        let nb = name.as_bytes();
        if nb.is_empty() {
            continue;
        }
        if ab.len() < nb.len() {
            if !ab.is_empty() && nb[..ab.len()].eq_ignore_ascii_case(ab) {
                any_prefix = true;
            }
            continue;
        }
        if !ab[..nb.len()].eq_ignore_ascii_case(nb) {
            continue;
        }
        match ab.get(nb.len()) {
            None => any_prefix = true, // exact name so far; need the next byte
            Some(&c) => {
                if c == b'>' || c == b'/' || c == b' ' || c == b'\t' || c == b'\n' {
                    let mut close = String::with_capacity(name.len() + 3);
                    close.push_str("</");
                    close.push_str(name);
                    close.push('>');
                    return Boundary::Xml(close);
                }
                // Otherwise `name` is a prefix of a longer tag — not this tool.
            }
        }
    }
    if any_prefix {
        Boundary::NeedMore
    } else {
        Boundary::NoMatch
    }
}

/// `rest` starts with an ASCII letter. Match `knownname` as a full token
/// immediately followed by invocation punctuation.
// reviewed: `nb.len()` bytes matched an ASCII-cased known name — a char boundary.
#[allow(clippy::string_slice)]
fn classify_name(rest: &str, known: &[String]) -> Boundary {
    let rb = rest.as_bytes();
    let mut any_prefix = false;
    for name in known {
        let nb = name.as_bytes();
        if nb.is_empty() {
            continue;
        }
        if rb.len() < nb.len() {
            if nb[..rb.len()].eq_ignore_ascii_case(rb) {
                any_prefix = true;
            }
            continue;
        }
        if !rb[..nb.len()].eq_ignore_ascii_case(nb) {
            continue;
        }
        match rb.get(nb.len()) {
            None => {
                any_prefix = true; // exact name so far; need the next byte
                continue;
            }
            Some(&c) if is_name_byte(c) => continue, // longer identifier — not this tool
            Some(_) => {}
        }
        // `name.len()` bytes matched an ASCII-cased known name → char boundary.
        match scan_invocation(&rest[nb.len()..]) {
            InvPunct::Brace => return Boundary::Brace,
            InvPunct::NeedMore => any_prefix = true,
            InvPunct::No => {}
        }
    }
    if any_prefix {
        Boundary::NeedMore
    } else {
        Boundary::NoMatch
    }
}

enum InvPunct {
    Brace,
    NeedMore,
    No,
}

/// After a matched name, look for `{` allowing only horizontal whitespace and
/// at most one newline in between. Anything else — including `(`, deliberately
/// not treated as invocation punctuation (see module docs) — means "not a call".
fn scan_invocation(tail: &str) -> InvPunct {
    let b = tail.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i < b.len() && b[i] == b'\n' {
        i += 1;
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
    }
    match b.get(i) {
        None => InvPunct::NeedMore,
        Some(b'{') => InvPunct::Brace,
        Some(_) => InvPunct::No,
    }
}

/// Find the end (exclusive byte index) of a balanced `open`/`close` structure,
/// starting at the first `open` byte in `s`. String-aware: `open`/`close` bytes
/// inside a double-quoted JSON string (respecting `\` escapes) don't count.
/// Returns `None` if it never closes within `s`.
fn find_balanced_end(s: &str, open: u8, close: u8) -> Option<usize> {
    let b = s.as_bytes();
    let start = b.iter().position(|&x| x == open)?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1); // `close` is ASCII → char boundary
            }
        }
        i += 1;
    }
    None
}

/// Find the end (exclusive byte index) past `close` (an ASCII `</name>` tag),
/// matched case-insensitively. Returns `None` if not present.
fn find_xml_close(s: &str, close: &str) -> Option<usize> {
    let nb = close.as_bytes();
    if nb.is_empty() {
        return None;
    }
    s.as_bytes()
        .windows(nb.len())
        .position(|w| w.eq_ignore_ascii_case(nb))
        .map(|p| p + nb.len())
}

#[derive(Clone)]
enum Mode {
    /// `buffer` starts at a message/line boundary — classify it.
    Boundary,
    /// Mid-line in confirmed-safe text — pass through until the next newline.
    MidLine,
    /// Inside a detected XML call — drop bytes until the close tag.
    SuppressXml { close: String },
    /// Inside a detected `{...}` call — drop bytes until the balanced `}`.
    SuppressBrace,
}

/// Stateful streaming filter that suppresses hallucinated extension-tool calls
/// from an assistant text stream. Mirrors the buffered-scanner pattern of
/// `crate::agent::thinking::ThinkingFilter`.
pub(crate) struct HallucinatedToolFilter {
    known: Arc<Vec<String>>,
    buffer: String,
    mode: Mode,
}

impl HallucinatedToolFilter {
    pub(crate) fn new(known: Arc<Vec<String>>) -> Self {
        Self {
            known,
            buffer: String::new(),
            mode: Mode::Boundary,
        }
    }

    /// Feed a chunk; return the text safe to emit (may be empty while buffering
    /// or suppressing).
    // reviewed: all slice offsets from ASCII scans (whitespace, '\n', '<', '{',
    // '}', close-tag, and ASCII-cased known-name lengths) — char boundaries.
    #[allow(clippy::string_slice)]
    pub(crate) fn process(&mut self, chunk: &str) -> String {
        if self.known.is_empty() {
            // No extension tools → nothing to detect. Pure passthrough.
            return chunk.to_string();
        }
        self.buffer.push_str(chunk);
        let mut out = String::new();
        loop {
            match self.mode.clone() {
                Mode::Boundary => match classify_boundary(&self.buffer, &self.known) {
                    Boundary::NeedMore => {
                        // R5 fix 2: a boundary token (or leading whitespace)
                        // that never resolves into a definite call/no-match
                        // would otherwise buffer unboundedly (e.g. a known
                        // name followed by a very long run of whitespace).
                        // Apply the same fail-open cap as the two Suppress*
                        // modes: flush as text and resume passthrough.
                        if self.buffer.len() > MAX_SUPPRESS_BYTES {
                            out.push_str(&self.buffer);
                            self.buffer.clear();
                            self.mode = Mode::MidLine;
                        }
                        break;
                    }
                    Boundary::NoMatch => {
                        if let Some(p) = self.buffer.find('\n') {
                            out.push_str(&self.buffer[..=p]);
                            self.buffer.drain(..=p);
                            // Next line is itself a boundary — stay in Boundary.
                        } else {
                            out.push_str(&self.buffer);
                            self.buffer.clear();
                            self.mode = Mode::MidLine;
                            break;
                        }
                    }
                    Boundary::Xml(close) => self.mode = Mode::SuppressXml { close },
                    Boundary::Brace => self.mode = Mode::SuppressBrace,
                },
                Mode::MidLine => {
                    if let Some(p) = self.buffer.find('\n') {
                        out.push_str(&self.buffer[..=p]);
                        self.buffer.drain(..=p);
                        self.mode = Mode::Boundary;
                    } else {
                        out.push_str(&self.buffer);
                        self.buffer.clear();
                        break;
                    }
                }
                Mode::SuppressXml { close } => {
                    if let Some(end) = find_xml_close(&self.buffer, &close) {
                        self.buffer.drain(..end); // drop the suppressed call
                        self.mode = Mode::MidLine;
                    } else if self.buffer.len() > MAX_SUPPRESS_BYTES {
                        out.push_str(&self.buffer); // fail-open
                        self.buffer.clear();
                        self.mode = Mode::MidLine;
                        break;
                    } else {
                        break;
                    }
                }
                Mode::SuppressBrace => {
                    if let Some(end) = find_balanced_end(&self.buffer, b'{', b'}') {
                        self.buffer.drain(..end); // drop the suppressed call
                        self.mode = Mode::MidLine;
                    } else if self.buffer.len() > MAX_SUPPRESS_BYTES {
                        out.push_str(&self.buffer); // fail-open
                        self.buffer.clear();
                        self.mode = Mode::MidLine;
                        break;
                    } else {
                        break;
                    }
                }
            }
        }
        out
    }

    /// Flush at end of stream. Buffered non-call text (a `NeedMore`/boundary
    /// prefix that never became a call) is emitted; an in-progress suppression
    /// (a detected call-start that never closed) is dropped — consistent with
    /// the live suppression decision already made for it.
    pub(crate) fn finish(&mut self) -> String {
        let out = match self.mode {
            Mode::Boundary | Mode::MidLine => std::mem::take(&mut self.buffer),
            Mode::SuppressXml { .. } | Mode::SuppressBrace => String::new(),
        };
        self.buffer.clear();
        self.mode = Mode::Boundary;
        out
    }
}

/// Post-hoc cleanup of a COMPLETE assistant content string, using the same
/// conservative matcher as the live filter. Keeps the persisted message (and
/// therefore reloads) consistent with what was shown live. Returns the input
/// unchanged when nothing was suppressed.
pub(crate) fn strip_hallucinated_tool_calls(content: &str, known: &[String]) -> String {
    if known.is_empty() || content.is_empty() {
        return content.to_string();
    }
    let mut f = HallucinatedToolFilter::new(Arc::new(known.to_vec()));
    let mut out = f.process(content);
    out.push_str(&f.finish());
    // Passthrough reconstructs the input byte-for-byte, so equal length ⇒
    // nothing suppressed ⇒ preserve the original (don't trim legit whitespace).
    if out.len() == content.len() {
        content.to_string()
    } else {
        out.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Arc<Vec<String>> {
        Arc::new(vec![
            "sequentialthinking".to_string(),
            "brave_search".to_string(),
        ])
    }

    fn filter() -> HallucinatedToolFilter {
        HallucinatedToolFilter::new(known())
    }

    // ── (a) hallucinated XML at start → suppressed ──────────────────────────

    #[test]
    fn xml_call_at_start_suppressed() {
        let mut f = filter();
        let out = f.process("<sequentialthinking>{\"thought\":\"hi\"}</sequentialthinking>");
        assert_eq!(out, "");
        assert_eq!(f.finish(), "");
    }

    #[test]
    fn xml_call_then_trailing_text_preserved() {
        let mut f = filter();
        let mut out = f.process("<sequentialthinking>reason</sequentialthinking>\nHere is my answer.");
        out.push_str(&f.finish());
        assert_eq!(out, "\nHere is my answer.");
    }

    #[test]
    fn xml_call_split_across_chunks_suppressed() {
        let mut f = filter();
        let mut out = f.process("<sequential");
        out.push_str(&f.process("thinking>abc</sequ"));
        out.push_str(&f.process("entialthinking>done"));
        out.push_str(&f.finish());
        assert_eq!(out, "done");
    }

    // ── (b) hallucinated name+JSON at start → suppressed ────────────────────

    #[test]
    fn name_json_at_start_suppressed() {
        let mut f = filter();
        let out = f.process("sequentialthinking\n{\"thought\":\"deep\"}");
        assert_eq!(out, "");
        assert_eq!(f.finish(), "");
    }

    #[test]
    fn name_json_with_brace_containing_string_brace() {
        let mut f = filter();
        // A `}` inside a JSON string value must not close the object early.
        let mut out = f.process("brave_search {\"q\": \"a } b\"}\nresult text");
        out.push_str(&f.finish());
        assert_eq!(out, "\nresult text");
    }

    #[test]
    fn name_json_split_across_chunks() {
        let mut f = filter();
        let mut out = f.process("sequential");
        out.push_str(&f.process("thinking\n{\"a\":"));
        out.push_str(&f.process("1}rest"));
        out.push_str(&f.finish());
        assert_eq!(out, "rest");
    }

    #[test]
    fn hallucinated_call_after_prose_line_boundary() {
        let mut f = filter();
        let mut out = f.process("Let me think.\nsequentialthinking\n{\"t\":\"x\"}\ndone");
        out.push_str(&f.finish());
        assert_eq!(out, "Let me think.\n\ndone");
    }

    // ── (c) legitimate prose mentioning a tool name → NOT suppressed ─────────

    #[test]
    fn prose_mentioning_tool_midsentence_not_suppressed() {
        let mut f = filter();
        let s = "The sequentialthinking tool helps with reasoning.";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    #[test]
    fn prose_starting_with_tool_name_then_text_not_suppressed() {
        let mut f = filter();
        let s = "sequentialthinking is great for step-by-step reasoning.";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    #[test]
    fn question_about_tool_not_suppressed() {
        let mut f = filter();
        let s = "what does sequentialthinking do?";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    #[test]
    fn tool_name_prefix_of_longer_word_not_suppressed() {
        let mut f = filter();
        // `brave_searches` is a superset token — not the tool `brave_search`.
        let s = "brave_searches{not a call}";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    // Fix 1: the paren call-shape `name(...)` is deliberately NOT detected —
    // models don't actually hallucinate it, and a function-like known tool
    // name (`fetch`, `search`) is common in legitimate line-start code
    // examples. These must pass through byte-identical.

    #[test]
    fn fetch_paren_code_example_not_suppressed() {
        let mut f = HallucinatedToolFilter::new(std::sync::Arc::new(vec!["fetch".to_string()]));
        let s = "fetch('https://x')";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    #[test]
    fn search_paren_code_example_not_suppressed() {
        let mut f = HallucinatedToolFilter::new(std::sync::Arc::new(vec!["search".to_string()]));
        let s = "search(query)";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    // ── (d) normal text with no tool name → passthrough unchanged ────────────

    #[test]
    fn plain_text_passthrough() {
        let mut f = filter();
        let s = "Hello, here is a normal answer.\nWith two lines.";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    #[test]
    fn multichunk_plain_text_passthrough() {
        let mut f = filter();
        let mut out = f.process("Hel");
        out.push_str(&f.process("lo "));
        out.push_str(&f.process("wörld\nдальше"));
        out.push_str(&f.finish());
        assert_eq!(out, "Hello wörld\nдальше");
    }

    #[test]
    fn empty_known_list_is_pure_passthrough() {
        let mut f = HallucinatedToolFilter::new(Arc::new(vec![]));
        let s = "sequentialthinking\n{\"x\":1}";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    #[test]
    fn json_like_content_without_tool_name_not_suppressed() {
        let mut f = filter();
        let s = "{\"just\": \"some json the user asked for\"}";
        let mut out = f.process(s);
        out.push_str(&f.finish());
        assert_eq!(out, s);
    }

    // ── (e) real native tool_use is unaffected (never reaches this filter) ────
    // Native tool calls arrive in the SSE `tool_calls` array, not as
    // `delta.content`, so there is nothing to assert at this layer beyond the
    // passthrough guarantee above. The XML/JSON shapes here are the *hallucinated*
    // free-form variants; the real dispatch path does not stream `content`.

    // ── post-hoc strip ──────────────────────────────────────────────────────

    #[test]
    fn strip_posthoc_removes_xml_call() {
        let known = ["sequentialthinking".to_string()];
        let cleaned = strip_hallucinated_tool_calls(
            "<sequentialthinking>x</sequentialthinking>",
            &known,
        );
        assert_eq!(cleaned, "");
    }

    #[test]
    fn strip_posthoc_removes_name_json_keeps_answer() {
        let known = ["sequentialthinking".to_string()];
        let cleaned = strip_hallucinated_tool_calls(
            "sequentialthinking\n{\"t\":\"x\"}\nThe answer is 42.",
            &known,
        );
        assert_eq!(cleaned, "The answer is 42.");
    }

    #[test]
    fn strip_posthoc_preserves_plain_text() {
        let known = ["sequentialthinking".to_string()];
        let s = "A normal answer with no calls.";
        assert_eq!(strip_hallucinated_tool_calls(s, &known), s);
    }

    #[test]
    fn strip_posthoc_empty_known_passthrough() {
        let s = "sequentialthinking\n{\"x\":1}";
        assert_eq!(strip_hallucinated_tool_calls(s, &[]), s);
    }

    // ── fail-open safety valve ───────────────────────────────────────────────

    #[test]
    fn unclosed_call_over_cap_fails_open() {
        let mut f = filter();
        let big = "x".repeat(MAX_SUPPRESS_BYTES + 100);
        let payload = format!("sequentialthinking\n{{\"t\":\"{big}");
        let mut out = f.process(&payload);
        out.push_str(&f.finish());
        // Never closed → flushed as text rather than swallowed.
        assert!(out.contains(&big), "over-cap unclosed call must fail open to text");
    }

    // Fix 2: `NeedMore` (a matched boundary token, or leading whitespace, that
    // never resolves into a definite call/no-match) must be capped the same
    // way as the two `Suppress*` modes — otherwise a known name followed by an
    // unbounded run of whitespace before any terminator buffers forever.
    #[test]
    fn needmore_over_cap_fails_open() {
        let mut f = filter();
        let spaces = " ".repeat(MAX_SUPPRESS_BYTES + 100);
        let payload = format!("sequentialthinking{spaces}");
        let mut out = f.process(&payload);
        out.push_str(&f.finish());
        assert_eq!(
            out, payload,
            "unresolved NeedMore buffer over cap must fail open to text, byte-identical"
        );
    }

    // ── boundary helper units ────────────────────────────────────────────────

    #[test]
    fn find_balanced_end_respects_strings() {
        // `{"a":"}"}x` — the `}` at index 6 is inside a string; the real close
        // is the `}` at index 8, so the end (exclusive) is 9.
        assert_eq!(find_balanced_end("{\"a\":\"}\"}x", b'{', b'}'), Some(9));
    }

    #[test]
    fn classify_boundary_needmore_on_bare_name() {
        assert_eq!(classify_boundary("sequentialthinking", &known()), Boundary::NeedMore);
    }

    #[test]
    fn classify_boundary_nomatch_on_unknown_start() {
        assert_eq!(classify_boundary("hello world", &known()), Boundary::NoMatch);
    }
}
