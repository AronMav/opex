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

/// `subagent.rs` (the enrich seam) must no longer enqueue via the legacy `video_jobs`
/// queue — that path is gone (Python handler_jobs queue owns video summarization now,
/// R13). The URL-detection helpers (`detect_video_links` / `is_supported_video_host`)
/// and the `handler_jobs` enqueue are LEGITIMATELY KEPT for the "paste a YouTube
/// link → auto-summarize" auto-trigger (Phase 5 Task 6d, R13).
#[test]
fn subagent_has_no_legacy_video_enqueue() {
    let src = include_str!("../src/agent/pipeline/subagent.rs");
    // Legacy path must be gone.
    assert!(
        !src.contains("video_jobs::enqueue_video_job"),
        "subagent.rs still enqueues via the legacy video_jobs queue"
    );
    // The new-queue auto-trigger must be present (R13 preservation).
    assert!(
        src.contains("handler_jobs::insert_handler_job"),
        "subagent.rs must enqueue via the new handler_jobs queue (R13 auto-trigger)"
    );
    assert!(
        src.contains("detect_video_links"),
        "subagent.rs must keep detect_video_links for the URL auto-trigger (R13)"
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

/// The closed in-core dispatch builtin set must no longer contain SummarizeVideo
/// (video moved to the Python handler tier), and the EnqueueCtx plumbing must be
/// fully removed (R15 — clean, no dead_code attrs). dispatch.rs / dispatch_seam.rs
/// themselves are KEPT — only the video arm + enqueue plumbing is cut.
#[test]
fn dispatch_has_no_summarize_video_or_enqueue_plumbing() {
    let dispatch = include_str!("../src/agent/file_scenario/dispatch.rs");
    assert!(
        !dispatch.contains("SummarizeVideo"),
        "dispatch.rs still declares the SummarizeVideo builtin arm"
    );
    assert!(
        !dispatch.contains("run_summarize_video"),
        "dispatch.rs still defines run_summarize_video"
    );
    assert!(
        !dispatch.contains("EnqueueCtx"),
        "dispatch.rs still declares the EnqueueCtx plumbing (must be removed cleanly per R15)"
    );
    // The kept sync arms must survive the cull.
    assert!(dispatch.contains("BuiltinAction::Transcribe"), "Transcribe arm kept");
    assert!(dispatch.contains("BuiltinAction::Save"), "Save arm kept");

    let seam = include_str!("../src/agent/file_scenario/dispatch_seam.rs");
    assert!(
        !seam.contains("video_jobs"),
        "dispatch_seam.rs still references the deprecated video_jobs table"
    );
    assert!(
        !seam.contains("EnqueueCtx"),
        "dispatch_seam.rs still threads EnqueueCtx (must be removed cleanly per R15)"
    );
    // The kept sync seam must survive.
    assert!(
        seam.contains("PendingAlternative"),
        "dispatch_seam.rs must keep PendingAlternative (legacy chips path, R2)"
    );

    // The wire field stays (R9) but its only-caller constructor is gone (R15).
    let outcome = include_str!("../src/agent/file_scenario/outcome.rs");
    assert!(
        outcome.contains("pub video_accepted"),
        "ScenarioOutcome.video_accepted wire field must be kept (R9)"
    );
    assert!(
        !outcome.contains("pub fn video_accepted"),
        "the orphaned ScenarioOutcome::video_accepted constructor must be removed (R15)"
    );
}

/// The in-core async-video modules must be gone from the file_scenario tree, the
/// mod facade must not declare them, and lib.rs must not mount video_summary.
#[test]
fn video_modules_are_deleted() {
    let mod_rs = include_str!("../src/agent/file_scenario/mod.rs");
    assert!(
        !mod_rs.contains("pub mod video_summary") && !mod_rs.contains("pub mod video_worker"),
        "file_scenario/mod.rs still declares video_summary / video_worker"
    );
    let lib_rs = include_str!("../src/lib.rs");
    assert!(
        !lib_rs.contains("video_summary") && !lib_rs.contains("video_worker"),
        "lib.rs still mounts the deleted video_summary/video_worker module"
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/agent/file_scenario");
    assert!(!dir.join("video_summary.rs").exists(), "video_summary.rs must be deleted");
    assert!(!dir.join("video_worker.rs").exists(), "video_worker.rs must be deleted");
    // Kept shell must survive.
    assert!(dir.join("dispatch.rs").exists(), "dispatch.rs must be kept (R11)");
    assert!(dir.join("dispatch_seam.rs").exists(), "dispatch_seam.rs must be kept (R11)");
    assert!(dir.join("owner_gate.rs").exists(), "owner_gate.rs must be kept");
}

/// opex-db must no longer expose the video_jobs module, and the deprecation
/// migration must be non-destructive (no DROP TABLE).
#[test]
fn video_jobs_module_removed_and_migration_non_destructive() {
    let dblib = include_str!("../../opex-db/src/lib.rs");
    assert!(
        !dblib.contains("pub mod video_jobs"),
        "opex-db lib still declares video_jobs"
    );
    let mig = include_str!("../../../migrations/068_video_jobs_deprecate.sql");
    assert!(
        !mig.to_uppercase().contains("DROP TABLE"),
        "068 must NOT drop video_jobs (history-preserving deprecation only)"
    );
    assert!(
        mig.to_lowercase().contains("video_jobs"),
        "068 should reference video_jobs in its deprecation note"
    );
}
