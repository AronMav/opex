//! Deterministic in-process LLM provider mock for integration tests.
//!
//! Phase 61 design: this mock implements a LOCAL trait (`MockLlmProvider`)
//! mirroring the engine's `LlmProvider` signature. The bridge to the real
//! trait (`crate::agent::providers::LlmProvider`) is DEFERRED — see the
//! plan-level <deferred> block in 61-02-PLAN.md for rationale (cascading
//! lib.rs re-exports exceed the 10-module cap).
//!
//! NEVER add an HTTP client to this file — the entire point is offline determinism.
//! CI checks `grep -E "use reqwest|use wiremock|use hyper"` must stay empty.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use hydeclaw_core::hydeclaw_types::{LlmResponse, Message, ToolCall, ToolDefinition};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Local trait mirror of `crate::agent::providers::LlmProvider`.
/// Method signatures MUST stay identical so a future blanket impl
/// (Phase 66+ once engine.rs is split) is mechanical.
#[async_trait]
pub trait MockLlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse>;

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<LlmResponse>;

    fn name(&self) -> &str;
}

/// One scripted turn the mock will return.
#[derive(Debug, Clone)]
pub struct MockTurn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

impl MockTurn {
    pub fn text(content: impl Into<String>, finish: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
            finish_reason: Some(finish.into()),
        }
    }

    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: hydeclaw_types::ids::ToolCallId::from(id.into()),
                name: name.into(),
                arguments,
                thought_signature: None,
            }],
            finish_reason: Some("tool_calls".to_string()),
        }
    }
}

#[derive(Debug, Default)]
struct MockState {
    turns: Vec<MockTurn>,
    cursor: usize,
    recorded_messages: Vec<Vec<Message>>,
}

pub struct MockProvider {
    name: String,
    state: Mutex<MockState>,
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            name: "mock".to_string(),
            state: Mutex::new(MockState::default()),
        }
    }

    /// Append a text-only turn. Builder pattern.
    pub fn expect_text(
        mut self,
        content: impl Into<String>,
        finish: impl Into<String>,
    ) -> Self {
        self.state
            .get_mut()
            .unwrap()
            .turns
            .push(MockTurn::text(content, finish));
        self
    }

    pub fn expect_tool_call(
        mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        self.state
            .get_mut()
            .unwrap()
            .turns
            .push(MockTurn::tool_call(id, name, args));
        self
    }

    pub fn invocations(&self) -> usize {
        self.state.lock().unwrap().cursor
    }

    pub fn recorded_messages(&self) -> Vec<Vec<Message>> {
        self.state.lock().unwrap().recorded_messages.clone()
    }

    fn next_turn(&self, messages: &[Message]) -> Result<MockTurn> {
        let mut s = self.state.lock().unwrap();
        s.recorded_messages.push(messages.to_vec());
        let idx = s.cursor;
        if idx >= s.turns.len() {
            return Err(anyhow!(
                "MockProvider: no more scripted turns (cursor={}, scripted={})",
                idx,
                s.turns.len()
            ));
        }
        let turn = s.turns[idx].clone();
        s.cursor += 1;
        Ok(turn)
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MockLlmProvider for MockProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let turn = self.next_turn(messages)?;
        // NOTE: `hydeclaw_types::LlmResponse` does NOT implement `Default`, so every
        // field must be listed explicitly. Keep this aligned with the struct shape in
        // `crates/hydeclaw-types/src/lib.rs`. If a new field is added there, add it
        // here with a sensible default (typically `None` / `Vec::new()` / `0`).
        Ok(LlmResponse {
            content: turn.content,
            tool_calls: turn.tool_calls,
            usage: None,
            finish_reason: turn.finish_reason,
            model: None,
            provider: Some("mock".to_string()),
            fallback_notice: None,
            tools_used: Vec::new(),
            iterations: 0,
            thinking_blocks: Vec::new(),
        })
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<LlmResponse> {
        let response = self.chat(messages, tools).await?;
        if response.tool_calls.is_empty() && !response.content.is_empty() {
            chunk_tx.send(response.content.clone()).ok();
        }
        Ok(response)
    }

    fn name(&self) -> &str {
        &self.name
    }
}
