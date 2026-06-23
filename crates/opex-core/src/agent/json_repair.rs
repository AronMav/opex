//! Repair malformed JSON from LLM tool call arguments.
//!
//! LLMs (especially `MiniMax`) sometimes wrap JSON in markdown fences,
//! prefix with explanatory text, or include trailing commas.

use std::sync::LazyLock;
use regex::Regex;

/// Attempt to parse `raw` as JSON. If it fails, apply repairs and retry.
/// Returns the parsed value on success, or the parse error if all repairs fail.
pub fn repair_json(raw: &str) -> Result<serde_json::Value, serde_json::Error> {
    // Fast path: valid JSON as-is
    if let Ok(v) = serde_json::from_str(raw) {
        return Ok(v);
    }

    let cleaned = strip_markdown_fences(raw);
    let cleaned = extract_json_object(&cleaned);
    let cleaned = fix_trailing_commas(&cleaned);

    serde_json::from_str(&cleaned)
}

/// Strip ```json ... ``` or ``` ... ``` fences.
fn strip_markdown_fences(s: &str) -> String {
    static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)```(?:json)?\s*(\{.*\})\s*```").unwrap()
    });
    if let Some(caps) = FENCE_RE.captures(s) {
        return caps[1].to_string();
    }
    s.to_string()
}

/// Extract the first top-level JSON object from text that may have prefix/suffix.
fn extract_json_object(s: &str) -> String {
    let Some(start) = s.find('{') else { return s.to_string() };
    let mut depth = 0i32;
    let mut end = start;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in s[start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    end = start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }

    if depth == 0 && end > start {
        s[start..end].to_string()
    } else {
        s.to_string()
    }
}

/// Remove trailing commas before } or ].
fn fix_trailing_commas(s: &str) -> String {
    static TRAILING_COMMA_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r",\s*([}\]])").unwrap()
    });
    TRAILING_COMMA_RE.replace_all(s, "$1").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_json_passthrough() {
        let v = repair_json(r#"{"query": "hello"}"#).unwrap();
        assert_eq!(v["query"], "hello");
    }

    #[test]
    fn markdown_fenced_json() {
        let raw = "Here is the result:\n```json\n{\"query\": \"test\"}\n```\n";
        let v = repair_json(raw).unwrap();
        assert_eq!(v["query"], "test");
    }

    #[test]
    fn prefixed_json() {
        let raw = "I'll search for that. {\"query\": \"bitcoin price\"}";
        let v = repair_json(raw).unwrap();
        assert_eq!(v["query"], "bitcoin price");
    }

    #[test]
    fn trailing_comma() {
        let raw = r#"{"query": "test", "limit": 5,}"#;
        let v = repair_json(raw).unwrap();
        assert_eq!(v["limit"], 5);
    }

    #[test]
    fn combined_issues() {
        let raw = "Sure!\n```json\n{\"a\": 1, \"b\": [1, 2,],}\n```\nDone.";
        let v = repair_json(raw).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn garbage_returns_error() {
        assert!(repair_json("not json at all").is_err());
    }

    #[test]
    fn nested_braces_in_strings() {
        let raw = r#"prefix {"key": "value with {braces}"} suffix"#;
        let v = repair_json(raw).unwrap();
        assert_eq!(v["key"], "value with {braces}");
    }

    #[test]
    fn prefix_with_braces_before_fence() {
        // Regression: prefix containing { before markdown fence should still extract fenced JSON
        let raw = "I processed {input}.\n```json\n{\"query\": \"test\"}\n```";
        let v = repair_json(raw).unwrap();
        assert_eq!(v["query"], "test");
    }

    #[test]
    fn multiline_json_in_fence() {
        let raw = "```json\n{\n  \"query\": \"test\",\n  \"limit\": 10\n}\n```";
        let v = repair_json(raw).unwrap();
        assert_eq!(v["query"], "test");
        assert_eq!(v["limit"], 10);
    }

    #[test]
    fn empty_object() {
        let v = repair_json("{}").unwrap();
        assert!(v.as_object().unwrap().is_empty());
    }

    #[test]
    fn deeply_nested_json() {
        let raw = r#"here: {"a": {"b": {"c": 42}}}"#;
        let v = repair_json(raw).unwrap();
        assert_eq!(v["a"]["b"]["c"], 42);
    }
}
