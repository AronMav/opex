//! Backup-payload synth helper for Phase 64 SEC-04 size-cap tests.
//!
//! Produces a JSON byte buffer shaped like
//! `crate::gateway::handlers::backup::BackupFile` — enough fields for
//! `serde_json::from_slice::<serde_json::Value>` to succeed — with
//! `workspace` entries padded so that the total serialized size is
//! approximately `size_mb * 1_048_576` bytes.
//!
//! Wave 1 Plan 05 (SEC-04) uses this to stream a payload at or above the
//! configured `[limits] max_restore_size_mb` threshold and assert the
//! restore endpoint rejects with `413 Payload Too Large` BEFORE the whole
//! body lands on disk.

/// Generate a JSON BackupFile blob whose total byte length is roughly
/// `size_mb` megabytes. The exact byte count is within about 1 MiB of the
/// requested size — enough for "is under cap?" / "is over cap?" assertions
/// where the cap itself is an integer-MB threshold.
pub fn synthesize_backup_bytes(size_mb: usize) -> Vec<u8> {
    let target_bytes: usize = size_mb.saturating_mul(1_048_576);

    // ── Skeleton that satisfies BackupFile's non-default fields. ────────────
    // We emit a canonical JSON object with stable field order so the size
    // math is deterministic. All array fields beyond `workspace` stay empty
    // because `BackupFile` marks them `#[serde(default)]`.
    //
    // `config` is an empty JSON object; `created_at` is a fixed RFC 3339
    // timestamp so we don't depend on the system clock.
    let header: &str = concat!(
        "{",
        "\"version\":1,",
        "\"created_at\":\"2026-01-01T00:00:00Z\",",
        "\"config\":{},",
        "\"workspace\":["
    );
    let footer: &str = concat!(
        "],",
        "\"secrets\":[],",
        "\"memory\":[],",
        "\"cron\":[]",
        "}"
    );

    let mut out = Vec::with_capacity(target_bytes.saturating_add(4 * 1024));
    out.extend_from_slice(header.as_bytes());

    // Each workspace entry looks like:
    //   {"path":"pad/<n>.txt","content":"<filler>"}
    // We pick a filler size that makes each entry ~64 KiB — big enough that
    // overhead is negligible, small enough that we overshoot by <1 MiB.
    const ENTRY_CONTENT_BYTES: usize = 64 * 1024;
    let filler = ".".repeat(ENTRY_CONTENT_BYTES);

    let mut index: u64 = 0;
    let mut first = true;
    // Reserve ~1 KiB of headroom so the final entry + footer fit inside the
    // target without overflowing the estimate by much.
    let limit = target_bytes.saturating_sub(footer.len() + 1024);
    while out.len() < limit {
        if !first {
            out.push(b',');
        }
        first = false;

        out.extend_from_slice(b"{\"path\":\"pad/");
        out.extend_from_slice(index.to_string().as_bytes());
        out.extend_from_slice(b".txt\",\"content\":\"");
        // Truncate the last entry so we don't massively overshoot the target.
        let remaining = limit.saturating_sub(out.len());
        let take = ENTRY_CONTENT_BYTES.min(remaining.max(1));
        out.extend_from_slice(&filler.as_bytes()[..take]);
        out.extend_from_slice(b"\"}");

        index += 1;
    }

    out.extend_from_slice(footer.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_mb_produces_valid_json_in_size_band() {
        let bytes = synthesize_backup_bytes(1);

        // Must be in roughly the 1 MiB band (±1 MiB tolerance is OK — the
        // contract is "approximately size_mb MB", not exact).
        assert!(
            bytes.len() >= 1_000_000,
            "too small: {} bytes",
            bytes.len()
        );
        assert!(
            bytes.len() <= 2_000_000,
            "too large: {} bytes",
            bytes.len()
        );

        // Must be valid JSON with the expected top-level keys.
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        let obj = v.as_object().expect("top-level is an object");
        assert_eq!(obj.get("version").and_then(|x| x.as_u64()), Some(1));
        assert!(obj.contains_key("workspace"));
        assert!(obj.contains_key("secrets"));
        assert!(obj.contains_key("created_at"));

        let workspace = obj["workspace"].as_array().expect("workspace is array");
        assert!(!workspace.is_empty(), "workspace must have filler entries");
    }

    #[test]
    fn zero_mb_still_valid_json() {
        // size_mb = 0 must not panic — just produces the bare skeleton.
        let bytes = synthesize_backup_bytes(0);
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(v["version"].as_u64(), Some(1));
    }
}
