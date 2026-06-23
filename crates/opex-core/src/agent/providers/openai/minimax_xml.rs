//! MiniMax-specific XML tool-call extraction. The MiniMax variant of
//! the OpenAI-compatible API encodes tool calls inside an XML payload
//! within the `content` field rather than via the `tool_calls` array.
//! This module parses that payload.

/// Extract MiniMax `<minimax:tool_call>` XML blocks from a response body.
///
/// Returns `(cleaned_content, tool_calls)` where:
/// - `cleaned_content` is the original text with all `<minimax:tool_call>...</minimax:tool_call>`
///   blocks removed and surrounding whitespace trimmed.
/// - `tool_calls` is the list of parsed tool calls.
///
/// XML format:
/// ```xml
/// <minimax:tool_call>
/// <invoke name="tool_name">
/// <parameter name="param1">value</parameter>
/// </invoke>
/// </minimax:tool_call>
/// ```
pub(crate) fn extract_minimax_xml_tool_calls(
    content: &str,
) -> (String, Vec<opex_types::ToolCall>) {
    const OPEN: &str = "<minimax:tool_call>";
    const CLOSE: &str = "</minimax:tool_call>";

    if !content.contains(OPEN) {
        return (content.to_string(), vec![]);
    }

    let mut tool_calls: Vec<opex_types::ToolCall> = Vec::new();
    let mut cleaned = String::new();
    let mut rest = content;

    loop {
        match rest.find(OPEN) {
            None => {
                cleaned.push_str(rest);
                break;
            }
            Some(start) => {
                cleaned.push_str(&rest[..start]);
                let after_open = &rest[start + OPEN.len()..];
                match after_open.find(CLOSE) {
                    None => break, // unclosed block — discard rest
                    Some(close_pos) => {
                        let block = &after_open[..close_pos];
                        rest = &after_open[close_pos + CLOSE.len()..];
                        parse_xml_invoke_blocks(block, &mut tool_calls);
                    }
                }
            }
        }
    }

    (cleaned.trim().to_string(), tool_calls)
}

/// Parse `<invoke name="...">...</invoke>` elements and push them into `out`.
fn parse_xml_invoke_blocks(block: &str, out: &mut Vec<opex_types::ToolCall>) {
    const INV_OPEN: &str = "<invoke";
    const INV_CLOSE: &str = "</invoke>";

    let mut rest = block;
    while let Some(start) = rest.find(INV_OPEN) {
        let after_tag = &rest[start + INV_OPEN.len()..];

        let Some(name) = xml_extract_attr(after_tag, "name") else { break };

        // Skip to end of opening tag (`>`)
        let Some(gt) = after_tag.find('>') else { break };
        let body_and_rest = &after_tag[gt + 1..];

        let Some(close_pos) = body_and_rest.find(INV_CLOSE) else { break };
        let invoke_body = &body_and_rest[..close_pos];
        rest = &body_and_rest[close_pos + INV_CLOSE.len()..];

        let mut args = serde_json::Map::new();
        parse_xml_parameters(invoke_body, &mut args);

        out.push(opex_types::ToolCall {
            id: opex_types::ids::ToolCallId::new(format!(
                "xml-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            )),
            name,
            arguments: serde_json::Value::Object(args),
            thought_signature: None,
        });
    }
}

/// Parse `<parameter name="...">VALUE</parameter>` pairs into a JSON map.
fn parse_xml_parameters(body: &str, out: &mut serde_json::Map<String, serde_json::Value>) {
    const PARAM_OPEN: &str = "<parameter";
    const PARAM_CLOSE: &str = "</parameter>";

    let mut rest = body;
    while let Some(start) = rest.find(PARAM_OPEN) {
        let after_tag = &rest[start + PARAM_OPEN.len()..];

        let Some(name) = xml_extract_attr(after_tag, "name") else { break };

        let Some(gt) = after_tag.find('>') else { break };
        let val_and_rest = &after_tag[gt + 1..];

        let Some(close_pos) = val_and_rest.find(PARAM_CLOSE) else { break };
        let raw_val = val_and_rest[..close_pos].trim();
        rest = &val_and_rest[close_pos + PARAM_CLOSE.len()..];

        // Coerce numeric/bool values; everything else is a string.
        let json_val = if let Ok(n) = raw_val.parse::<i64>() {
            serde_json::Value::Number(n.into())
        } else if raw_val == "true" {
            serde_json::Value::Bool(true)
        } else if raw_val == "false" {
            serde_json::Value::Bool(false)
        } else {
            serde_json::Value::String(raw_val.to_string())
        };

        out.insert(name, json_val);
    }
}

/// Extract `attr="VALUE"` from an XML tag fragment (everything after the tag name).
fn xml_extract_attr(s: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = s.find(needle.as_str())? + needle.len();
    let end = s[start..].find('"')?;
    Some(s[start..start + end].to_string())
}

#[cfg(test)]
mod xml_tests {
    use super::*;

    #[test]
    fn test_extract_single_invoke() {
        let input = "Some text\n<minimax:tool_call>\n<invoke name=\"brave_search\">\n<parameter name=\"q\">test query</parameter>\n<parameter name=\"count\">5</parameter>\n</invoke>\n</minimax:tool_call>\nMore text";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        // Double newline is expected: text before ends with \n, text after starts with \n
        assert_eq!(content, "Some text\n\nMore text");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "brave_search");
        assert_eq!(calls[0].arguments["q"], "test query");
        assert_eq!(calls[0].arguments["count"], 5);
    }

    #[test]
    fn test_extract_multiple_invokes_in_one_block() {
        let input = "<minimax:tool_call><invoke name=\"search\">\n<parameter name=\"q\">foo</parameter>\n</invoke>\n<invoke name=\"web\">\n<parameter name=\"url\">https://example.com</parameter>\n</invoke>\n</minimax:tool_call>";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(content, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[1].name, "web");
    }

    #[test]
    fn test_extract_multiple_blocks() {
        let input = "<minimax:tool_call><invoke name=\"a\"><parameter name=\"x\">1</parameter></invoke></minimax:tool_call>\n<minimax:tool_call><invoke name=\"b\"><parameter name=\"y\">2</parameter></invoke></minimax:tool_call>";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(content, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn test_no_xml_passthrough() {
        let input = "Normal response with no XML";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(content, input);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_boolean_and_numeric_coercion() {
        let input = "<minimax:tool_call><invoke name=\"t\"><parameter name=\"n\">42</parameter><parameter name=\"b\">true</parameter><parameter name=\"s\">hello</parameter></invoke></minimax:tool_call>";
        let (_, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(calls[0].arguments["n"], 42);
        assert_eq!(calls[0].arguments["b"], true);
        assert_eq!(calls[0].arguments["s"], "hello");
    }
}

#[cfg(test)]
mod golden_fixtures {
    use super::*;

    /// Regression: MiniMax XML with two `<invoke>` blocks in one response
    /// must yield two tool calls.
    #[test]
    fn minimax_xml_two_invoke_blocks() {
        let content = r#"prefix <minimax:tool_call>
<invoke name="alpha"><parameter name="x">1</parameter></invoke>
<invoke name="beta"><parameter name="y">2</parameter></invoke>
</minimax:tool_call> suffix"#;
        let (_cleaned, calls) = extract_minimax_xml_tool_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "alpha");
        assert_eq!(calls[1].name, "beta");
    }

    /// Regression: parse_xml_parameters with no <parameter> tags returns
    /// an empty map rather than crashing.
    #[test]
    fn xml_parameters_empty_body() {
        let mut params = serde_json::Map::new();
        parse_xml_parameters("", &mut params);
        assert!(params.is_empty());
    }
}
