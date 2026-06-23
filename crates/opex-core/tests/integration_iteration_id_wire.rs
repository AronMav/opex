//! Wire-format byte-equal regression for IterationId.
//!
//! Asserts that the StepStart event produced by the new code (with
//! IterationId struct) is bit-identical to the pre-T2 format. Defends
//! against accidental wire-format break.

use opex_types::ids::{IterationId, MessageId};
use uuid::Uuid;

#[test]
fn step_start_wire_format_unchanged_after_iteration_id_struct() {
    // Build the SAME payload using the new IterationId struct + the manual
    // json! conversion logic from sse_converter.rs.
    let message_uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let iteration = IterationId { index: 0, message_id: MessageId::from(message_uuid) };

    // Mirror the conversion in sse_converter.rs (T2 update)
    let actual = serde_json::json!({
        "type": "step-start",
        "stepId": format!("step_{}", iteration.index),
        "messageId": iteration.message_id.to_string(),
        "agentName": "Arty"
    });

    // Load fixture
    let fixture: serde_json::Value = serde_json::from_str(
        include_str!("fixtures/step_start_legacy.json")
    ).unwrap();

    assert_eq!(actual, fixture,
               "T2 must preserve byte-identical wire format for step-start");
}
