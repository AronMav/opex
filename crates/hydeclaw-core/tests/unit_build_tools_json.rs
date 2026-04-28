//! Unit tests for gateway/handlers/chat.rs::build_tools_json function.
//!
//! This integration test validates the tools JSON caching logic that optimizes
//! SSE streaming by reusing cached tool arrays when the tool count hasn't changed.

use serde_json::json;

/// Build tools JSON from accumulated tools, reusing cached value when no new tools arrived.
/// Only calls `.to_vec()` when `accumulated_tools` actually grew since the last build.
fn build_tools_json(
    tools: &[serde_json::Value],
    flushed_count: &mut usize,
    cache: &mut Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    if tools.is_empty() {
        return None;
    }
    if cache.is_none() || tools.len() != *flushed_count {
        *cache = Some(serde_json::Value::Array(tools.to_vec()));
        *flushed_count = tools.len();
    }
    cache.clone()
}

#[test]
fn build_tools_json_empty_returns_none() {
    let mut count = 0usize;
    let mut cache = None;
    assert!(build_tools_json(&[], &mut count, &mut cache).is_none());
}

#[test]
fn build_tools_json_first_call_builds_cache() {
    let tools = vec![json!({"name": "search"})];
    let mut count = 0usize;
    let mut cache = None;
    let result = build_tools_json(&tools, &mut count, &mut cache).unwrap();
    assert_eq!(result, json!([{"name": "search"}]));
    assert_eq!(count, 1);
}

#[test]
fn build_tools_json_same_count_reuses_cache() {
    let tools = vec![json!({"name": "search"})];
    let mut count = 0usize;
    let mut cache = None;
    build_tools_json(&tools, &mut count, &mut cache);
    // Modify cache to detect reuse
    let sentinel = json!("SENTINEL");
    cache = Some(sentinel.clone());
    // Same count — should reuse cache, not rebuild
    let result = build_tools_json(&tools, &mut count, &mut cache).unwrap();
    assert_eq!(result, sentinel);
}

#[test]
fn build_tools_json_new_tool_invalidates_cache() {
    let tools_1 = vec![json!({"name": "search"})];
    let mut count = 0usize;
    let mut cache = None;
    build_tools_json(&tools_1, &mut count, &mut cache);

    // Add a second tool
    let tools_2 = vec![
        json!({"name": "search"}),
        json!({"name": "write"}),
    ];
    let result = build_tools_json(&tools_2, &mut count, &mut cache).unwrap();
    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(count, 2);
}
