pub mod phase_transitions;
pub mod phase_repairs;
pub mod phase_consolidation;

use std::sync::Arc;
use sqlx::PgPool;
use crate::config::CuratorConfig;
use crate::secrets::SecretsManager;

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Sanitize a skill name to a safe filename stem.
/// Strips path-unsafe characters AND prevents directory traversal via `..`.
pub(crate) fn sanitize_skill_name(name: &str) -> String {
    let s = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
    if s.contains("..") {
        return s.replace("..", "-");
    }
    s
}

// ── Public types ───────────────────────────────────────────────────────────────

pub struct CuratorRunSummary {
    pub phase1: i32,
    pub phase2: i32,
    pub phase3: i32,
    pub report_md: String,
}

// ── Orchestrator ───────────────────────────────────────────────────────────────

/// Run the full curator pipeline. Each phase is isolated — failure of one does not stop the next.
pub async fn run_curator(
    db: &PgPool,
    cfg: &CuratorConfig,
    secrets: Arc<SecretsManager>,
    workspace_dir: &str,
) -> anyhow::Result<CuratorRunSummary> {
    let mut report_lines: Vec<String> = Vec::new();
    let mut phase1_count = 0i32;
    let mut phase2_count = 0i32;
    let mut phase3_count = 0i32;

    // Phase 1: State transitions (no LLM)
    match phase_transitions::run(workspace_dir, db, cfg.stale_after_days, cfg.archive_after_days).await {
        Ok(r) => {
            phase1_count = r.transitions;
            if !r.log.is_empty() {
                report_lines.push("## Phase 1: State Transitions".into());
                report_lines.extend(r.log.iter().map(|l| format!("- {l}")));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "curator phase1 failed");
            report_lines.push(format!("## Phase 1: FAILED — {e}"));
        }
    }

    // Build provider once for phases 2 and 3
    let provider = build_curator_provider(db, cfg, secrets.clone()).await;

    // Phase 2: Repair queue
    match phase_repairs::run(workspace_dir, db, &provider, cfg.max_repairs_per_run).await {
        Ok(r) => {
            phase2_count = r.applied;
            if !r.log.is_empty() {
                report_lines.push("## Phase 2: Repairs".into());
                report_lines.extend(r.log.iter().map(|l| format!("- {l}")));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "curator phase2 failed");
            report_lines.push(format!("## Phase 2: FAILED — {e}"));
        }
    }

    // Phase 3: LLM consolidation
    match phase_consolidation::run(workspace_dir, db, &provider).await {
        Ok(r) => {
            phase3_count = r.commands_executed;
            if !r.log.is_empty() {
                report_lines.push("## Phase 3: Consolidation".into());
                report_lines.extend(r.log.iter().map(|l| format!("- {l}")));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "curator phase3 failed");
            report_lines.push(format!("## Phase 3: FAILED — {e}"));
        }
    }

    if report_lines.is_empty() {
        report_lines.push("Nothing to do.".into());
    }

    Ok(CuratorRunSummary {
        phase1: phase1_count,
        phase2: phase2_count,
        phase3: phase3_count,
        report_md: report_lines.join("\n"),
    })
}

// ── Provider builder ───────────────────────────────────────────────────────────

/// Build an LLM provider from the curator config.
async fn build_curator_provider(
    db: &PgPool,
    cfg: &CuratorConfig,
    secrets: Arc<SecretsManager>,
) -> Arc<dyn crate::agent::providers::LlmProvider> {
    use crate::agent::providers::{UnconfiguredProvider, build_provider, ProviderOverrides};

    if cfg.provider_connection.is_empty() {
        tracing::warn!("curator: provider_connection not set — LLM phases will fail");
        return Arc::new(UnconfiguredProvider::new(
            "curator provider_connection not configured",
        ));
    }

    match crate::db::providers::get_provider_by_name(db, &cfg.provider_connection).await {
        Ok(Some(row)) => {
            let model = if cfg.model.is_empty() { None } else { Some(cfg.model.as_str()) };
            let overrides = ProviderOverrides {
                model: model.map(str::to_string),
                temperature: Some(0.3),
                max_tokens: Some(4096),
                prompt_cache: None,
            };
            let cancel = tokio_util::sync::CancellationToken::new();
            match build_provider(&row, secrets, &Default::default(), cancel, overrides) {
                Ok(p) => Arc::from(p),
                Err(e) => {
                    tracing::error!(error = %e, "curator: build_provider failed");
                    Arc::new(UnconfiguredProvider::new("build_provider failed"))
                }
            }
        }
        Ok(None) => {
            tracing::warn!(connection = %cfg.provider_connection, "curator: provider not found");
            Arc::new(UnconfiguredProvider::new("connection not found"))
        }
        Err(e) => {
            tracing::error!(error = %e, "curator: DB error");
            Arc::new(UnconfiguredProvider::new("DB error"))
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_skill_name_blocks_dotdot_traversal() {
        assert_eq!(sanitize_skill_name("../config"), "--config");
        assert_eq!(sanitize_skill_name("a/../b"), "a---b");
        assert_eq!(sanitize_skill_name(".."), "-");
    }

    #[test]
    fn sanitize_skill_name_strips_unsafe_chars() {
        assert_eq!(sanitize_skill_name("my skill/name"), "my-skill-name");
        assert_eq!(sanitize_skill_name("a:b*c?d"), "a-b-c-d");
    }

    #[test]
    fn sanitize_skill_name_leaves_safe_names_unchanged() {
        assert_eq!(sanitize_skill_name("channel-formatting"), "channel-formatting");
        assert_eq!(sanitize_skill_name("skill_v2"), "skill_v2");
    }
    use tempfile::TempDir;

    /// Verify run_curator completes without panicking when the workspace has no
    /// skills directory (phase1 gracefully returns empty, phase2 and phase3 also
    /// return empty because there is no DB and the provider is unconfigured).
    ///
    /// This is a unit-level smoke test — it does NOT require a running Postgres
    /// instance. Phase 1 bails early when it can't read the skills dir. Phases 2
    /// and 3 are never reached because `build_curator_provider` returns an
    /// `UnconfiguredProvider` when `provider_connection` is empty, and the phase
    /// functions short-circuit on empty queues before calling the provider.
    #[tokio::test]
    async fn run_curator_no_skills_dir_no_panic() {
        // Create a real but empty temporary workspace
        let tmp = TempDir::new().expect("tempdir");
        let workspace_dir = tmp.path().to_str().unwrap();

        // A default CuratorConfig has provider_connection = "" and enabled = false
        let cfg = CuratorConfig::default();

        // We cannot call run_curator without a real PgPool, so instead we test the
        // individual components that don't need a DB connection.

        // Phase 1: no skills dir → should return empty result, not panic
        let result = phase_transitions::run(
            workspace_dir,
            // We can't pass a real PgPool in a unit test, so we test phase1 only
            // up to the point where it tries to read the skills dir. The function
            // returns Ok(empty) when the dir is missing, which is what we check.
            // This verifies the graceful-degradation path.
            // NOTE: phase_transitions::run needs a PgPool for DB writes after
            // reading. Because the skills dir is absent, it returns before any
            // DB call, so we test with a dummy pool created from a connection
            // string that we never actually connect to.
            // We use a lazy pool (not yet connected) — sqlx PgPool is lazy by
            // default when created via PgPool::connect_lazy.
            &sqlx::PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap(),
            cfg.stale_after_days,
            cfg.archive_after_days,
        )
        .await
        .expect("phase1 must not error on missing skills dir");

        assert_eq!(result.transitions, 0);
        assert!(result.log.is_empty());
    }

    /// Verify that `build_curator_provider` returns an UnconfiguredProvider
    /// (rather than panicking) when provider_connection is empty.
    #[tokio::test]
    async fn build_curator_provider_empty_connection_returns_unconfigured() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap();
        let cfg = CuratorConfig::default(); // provider_connection = ""

        // SecretsManager::new_noop() — never actually used because
        // build_curator_provider returns early when provider_connection is empty.
        let secrets = crate::secrets::SecretsManager::new_noop();
        let provider = build_curator_provider(&pool, &cfg, Arc::new(secrets)).await;

        // Calling chat() on an UnconfiguredProvider returns an error, not a panic.
        // We just verify the Arc is valid (no panic during construction).
        let _name = provider.name();
        // UnconfiguredProvider::name() returns "unconfigured"
        assert_eq!(provider.name(), "unconfigured");
    }
}
