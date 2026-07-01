//! Phase 6 deletion gate: the in-core video pipeline is replaced by the Python
//! `summarize_video` async handler + the universal `handler_jobs` queue
//! (Phase 5). This pure source-text guard (no DB, no toolgate) asserts the live
//! consumers no longer reference the legacy video symbols, so deleting
//! `video_worker.rs` / `video_summary.rs` / `db/video_jobs.rs` in later Phase 6
//! tasks cannot strand a live call site.
//!
//! NOTE: the legacy sync chips / Telegram path (`dispatch_seam`, `dispatch.rs`
//! transcribe/describe/extract/save arms, `file_scenarios/run.rs`) is KEPT and
//! deliberately NOT asserted-against here (R2).

/// `subagent.rs` (the enrich seam) must no longer enqueue legacy video jobs from
/// detected YouTube/Yandex links — that path is gone (Python owns video now).
#[test]
fn subagent_has_no_legacy_video_enqueue() {
    let src = include_str!("../src/agent/pipeline/subagent.rs");
    assert!(
        !src.contains("video_jobs::enqueue_video_job"),
        "subagent.rs still enqueues legacy video_jobs"
    );
    assert!(
        !src.contains("detect_video_links"),
        "subagent.rs still has the dead detect_video_links helper"
    );
    assert!(
        !src.contains("is_supported_video_host"),
        "subagent.rs still has the dead is_supported_video_host helper"
    );
    // The legacy sync attachment dispatch (chips/Telegram, R2) must survive.
    assert!(
        src.contains("dispatch_attachments"),
        "subagent.rs must keep the sync dispatch_attachments enrich seam (R2)"
    );
}

/// `main.rs` must no longer spawn the in-core video worker nor recover stuck
/// video_jobs — Phase 5 replaced both with the universal file_handler_worker +
/// handler_jobs recovery.
#[test]
fn main_has_no_video_worker_or_recovery() {
    let src = include_str!("../src/main.rs");
    assert!(
        !src.contains("spawn_video_worker"),
        "main.rs still spawns the legacy video_worker"
    );
    assert!(
        !src.contains("recover_stuck_video_jobs"),
        "main.rs still recovers legacy video_jobs"
    );
    assert!(
        !src.contains("shutdown_video"),
        "main.rs still has the dead shutdown_video token alias"
    );
}
