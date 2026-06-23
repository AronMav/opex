//! Strong-typed identity primitives for stream objects.
//!
//! Each ID kind is a distinct newtype around `Uuid` (or `String` where
//! externally supplied). Wire format is unchanged from pre-S2 — the newtype
//! exists for compile-time type-tagging ("can't pass a StepId where a
//! ToolCallId is expected") and boundary parsing (malformed UUIDs rejected
//! at SSE/DB deserialization).
//!
//! See ADR `docs/architecture/2026-05-05-id-based-dedup.md` (with the
//! 2026-05-07 amendment) and spec
//! `docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md`.

/// Generate a Uuid-backed newtype with type-tagging only (no encapsulation).
///
/// Public inner field is INTENTIONAL: this newtype exists for compile-time
/// type-tagging, not to enforce a value invariant beyond "is a valid Uuid".
/// Callers may read `id.0` directly when they need the inner Uuid.
///
/// `Default` is intentionally NOT generated. Constructing a fresh ID looks
/// like a side effect (random UUID); making that implicit via
/// `Default::default()` is surprising. Callers use `XxxId::new()` for a
/// fresh UUID or `XxxId::from(uuid)` to wrap a known value.
macro_rules! impl_id_newtype {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize,
        )]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        #[serde(transparent)]
        pub struct $name(pub uuid::Uuid);

        impl $name {
            // `Default` is intentionally omitted — see macro doc-comment.
            #[allow(clippy::new_without_default)]
            #[inline]
            pub fn new() -> Self { Self(uuid::Uuid::new_v4()) }

            #[inline]
            pub fn as_uuid(&self) -> uuid::Uuid { self.0 }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                uuid::Uuid::parse_str(s).map(Self)
            }
        }

        impl From<uuid::Uuid> for $name {
            #[inline]
            fn from(u: uuid::Uuid) -> Self { Self(u) }
        }

        impl From<$name> for uuid::Uuid {
            #[inline]
            fn from(id: $name) -> Self { id.0 }
        }
    };
}

/// Generate a String-backed newtype for IDs externally supplied (e.g.,
/// LLM-provider tool call IDs which can be arbitrary strings, not UUIDs).
macro_rules! impl_string_id_newtype {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize,
        )]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            #[inline]
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }

            #[inline]
            pub fn as_str(&self) -> &str { &self.0 }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            #[inline]
            fn from(s: String) -> Self { Self(s) }
        }

        impl From<&str> for $name {
            #[inline]
            fn from(s: &str) -> Self { Self(s.to_string()) }
        }
    };
}

impl_id_newtype!(
    /// Identity for an assistant message DB row.
    ///
    /// Pre-allocated by `pipeline::execute` before SSE `step-start` is emitted,
    /// then carried through to `messages.id` in DB. See ADR
    /// `docs/architecture/2026-05-05-id-based-dedup.md`.
    MessageId
);

impl_id_newtype!(
    /// Identity for a pending/resolved approval. DB-default-uuid via
    /// `pending_approvals.id RETURNING id` in `db::approvals::create_approval`.
    ApprovalId
);

impl_id_newtype!(
    /// Identity for a group of parallel tool calls executed in one batch.
    /// New in S2 (T3). Threads through `pipeline::parallel`, attaches to
    /// each parallel tool's persisted message row via
    /// `messages.parallel_batch_id` (m047) and SSE event field.
    ParallelBatchId
);

impl_string_id_newtype!(
    /// Identity for a single tool call within an LLM turn.
    ///
    /// Format is provider-specific:
    ///   - OpenAI: `"call_abc123"`
    ///   - Anthropic: `"toolu_..."`
    ///   - Other: arbitrary
    ///
    /// We MUST echo whatever the provider sends — wrapping it as a newtype
    /// gives compile-time type-tagging without altering the value.
    ToolCallId
);

/// Bundle of (iteration index, message id) replacing the implicit
/// (step_id, message_id) combo in `StreamEvent::StepStart`.
///
/// Note: the wire format keeps `stepId: String` (stringified integer) and
/// `messageId: String` (UUID-as-string) — conversion happens manually in
/// `sse_converter.rs`, not via Serde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IterationId {
    /// 0-indexed iteration counter within a turn.
    /// DB format: `INT` (`messages.step_id`, m046).
    pub index: u32,
    /// UUID of the assistant message DB row for this iteration.
    pub message_id: MessageId,
}

/// Compile-fail proof: passing one ID type where another is expected
/// is a compile error.
///
/// ```compile_fail
/// use opex_types::ids::{ToolCallId, MessageId};
///
/// fn requires_tool_call(_id: ToolCallId) {}
///
/// fn user() {
///     requires_tool_call(MessageId::new());
///     //                 ^^^^^^^^^^^^^^^^ expected `ToolCallId`, found `MessageId`
/// }
/// ```
#[allow(dead_code)]
fn _compile_fail_proof() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn newtype_serializes_as_bare_string() {
        let id = ApprovalId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"') && json.ends_with('"'));
        assert!(!json.contains("ApprovalId"));
    }

    #[test]
    fn newtype_round_trips() {
        let id = MessageId::new();
        let json = serde_json::to_string(&id).unwrap();
        let parsed: MessageId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn newtype_rejects_malformed_uuid() {
        let bad = "\"not-a-uuid\"";
        let result: Result<MessageId, _> = serde_json::from_str(bad);
        assert!(result.is_err(), "malformed UUID must be rejected");
    }

    #[test]
    fn newtype_display_matches_uuid_display() {
        let uuid = uuid::Uuid::new_v4();
        let id = ApprovalId::from(uuid);
        assert_eq!(format!("{id}"), format!("{uuid}"));
    }

    #[test]
    fn newtype_from_str_round_trips_with_display() {
        let id = ApprovalId::new();
        let s = format!("{id}");
        let parsed = ApprovalId::from_str(&s).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn string_newtype_accepts_arbitrary_value() {
        let id = ToolCallId::new("call_abc123");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"call_abc123\"");
    }
}
