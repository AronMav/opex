# `stream_options.include_usage` for OpenAI-compatible streaming

## Goal

Report real token usage on sessions backed by OpenAI-compatible providers
(Ollama, vLLM, SGLang, LiteLLM, DeepSeek, Moonshot, Groq, …). Currently
`messages.input_tokens` / `output_tokens` are 0 or NULL for these sessions,
breaking cost attribution and the context-usage indicator.

## Root cause

`providers_openai.rs::chat_stream` builds the streaming request without
`stream_options.include_usage: true`. Per the OpenAI Chat Completions spec,
a streaming response omits the `usage` block by default; the field is only
emitted when the caller explicitly requests it.

The response parser (`providers_openai.rs:567-569`) already captures
`chunk_json.usage` when it is present in a stream chunk, so only the
request side needs the change.

## Change

One line in the JSON body built by `chat_stream`:

```rust
let mut body = serde_json::json!({
    "model": effective_model,
    "messages": messages_to_openai_format(messages),
    "temperature": self.temperature,
    "stream": true,
    "stream_options": { "include_usage": true },   // NEW
});
```

## Compatibility

`stream_options.include_usage` is part of the OpenAI public API since 2024
and is respected by every mainstream OpenAI-compatible server (Ollama,
vLLM, SGLang, LiteLLM, DeepSeek, Moonshot, Groq, Together, OpenRouter).
Servers that do not recognize it are expected to ignore unknown request
fields per the OpenAI contract; this is the same robustness assumption
we already rely on for `tool_choice`, `parallel_tool_calls`, etc.

A misbehaving provider that 4xx's on unknown fields would have been
broken for us long before this change, since we already send several
OpenAI-spec fields not in the original 2023 API.

## Verification

Before:

```sql
SELECT agent_id, input_tokens, output_tokens, created_at
FROM messages
WHERE agent_id IN ('Alma','Hyde','Arty') AND role='assistant'
ORDER BY created_at DESC LIMIT 5;
```

Expected: all zeros (or NULL) for Ollama-backed agents.

After deploy + one fresh session per agent:

Expected: non-zero `input_tokens` and `output_tokens`.

## Non-goals

- Rewriting any parser logic — already handles the field.
- Adding a config flag to disable this — the field is universally safe;
  optionality would be dead code.
- Backfilling existing messages. Only new sessions after deploy get the
  accurate counts.

## Scope

Single file (`crates/hydeclaw-core/src/agent/providers_openai.rs`),
single function (`chat_stream`), one line added. No tests gated on
this (the streaming test harness mocks responses).
