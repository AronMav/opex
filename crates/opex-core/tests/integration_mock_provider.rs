//! Smoke integration test for TEST-02 — MockProvider determinism.
//!
//! Exercises the LOCAL `MockLlmProvider` trait against the in-process `MockProvider`:
//! - replays scripted turns deterministically
//! - streams text via chat_stream()
//! - returns a named error ("no more scripted turns") when over-consumed
//!
//! Does NOT require Docker or any network — safe on every CI runner.

mod support;

use std::time::Duration;
use support::{MockLlmProvider, MockProvider};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Build an empty messages/tools slice — Phase 61 mock does not inspect them
/// beyond recording the messages vec.
fn empty_inputs() -> (
    Vec<opex_core::opex_types::Message>,
    Vec<opex_core::opex_types::ToolDefinition>,
) {
    (Vec::new(), Vec::new())
}

#[tokio::test]
async fn test_test_02_mock_provider_replays_scripted_turns() {
    timeout(Duration::from_secs(5), async {
        let mock = MockProvider::new()
            .expect_text("hello", "stop")
            .expect_text("world", "stop");
        let (msgs, tools) = empty_inputs();

        let r1 = mock.chat(&msgs, &tools).await.expect("first turn");
        assert_eq!(r1.content, "hello");
        assert_eq!(r1.finish_reason.as_deref(), Some("stop"));

        let r2 = mock.chat(&msgs, &tools).await.expect("second turn");
        assert_eq!(r2.content, "world");

        assert_eq!(mock.invocations(), 2);
        assert_eq!(mock.recorded_messages().len(), 2);
    })
    .await
    .expect("scripted-turns test exceeded 5s — mock should be instant");
}

#[tokio::test]
async fn test_test_02_mock_provider_streams_text_chunks() {
    timeout(Duration::from_secs(5), async {
        let mock = MockProvider::new().expect_text("streamed", "stop");
        let (msgs, tools) = empty_inputs();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let resp = mock.chat_stream(&msgs, &tools, tx).await.expect("stream");
        assert_eq!(resp.content, "streamed");

        // Drain the channel — should yield exactly one chunk equal to the scripted content.
        let chunk = rx.recv().await.expect("expected one streamed chunk");
        assert_eq!(chunk, "streamed");
    })
    .await
    .expect("streaming test exceeded 5s");
}

#[tokio::test]
async fn test_test_02_mock_provider_errors_when_over_consumed() {
    timeout(Duration::from_secs(5), async {
        let mock = MockProvider::new().expect_text("only-turn", "stop");
        let (msgs, tools) = empty_inputs();

        mock.chat(&msgs, &tools).await.expect("first ok");
        let err = mock.chat(&msgs, &tools).await.unwrap_err();
        let display = format!("{err:#}");
        assert!(
            display.contains("no more scripted turns"),
            "error must explicitly say 'no more scripted turns', got: {display}"
        );
    })
    .await
    .expect("over-consumption test exceeded 5s");
}
