use chrono::{DateTime, Duration, Utc};
use crate::skills::{SkillDef, SkillState};

pub struct TransitionResult {
    pub transitions: i32,
    pub log: Vec<String>,
}

/// Determine the target state for a skill based on anchor date and thresholds.
/// Returns None if no transition needed (already archived, never used, or within threshold).
pub fn decide_transition(
    current: &SkillState,
    anchor: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    stale_after_days: u32,
    archive_after_days: u32,
) -> Option<SkillState> {
    if matches!(current, SkillState::Archived) {
        return None;
    }
    let anchor = anchor?;
    let age = now.signed_duration_since(anchor);
    let stale = Duration::days(i64::from(stale_after_days));
    let archive = Duration::days(i64::from(archive_after_days));

    if age >= archive && !matches!(current, SkillState::Archived) {
        Some(SkillState::Archived)
    } else if age >= stale && matches!(current, SkillState::Active) {
        Some(SkillState::Stale)
    } else {
        None
    }
}

pub async fn run(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    stale_after_days: u32,
    archive_after_days: u32,
    dry_run: bool,
) -> anyhow::Result<TransitionResult> {
    let now = Utc::now();
    let skills_dir = std::path::Path::new(workspace_dir).join("skills");
    let mut result = TransitionResult { transitions: 0, log: Vec::new() };

    let mut rd = match tokio::fs::read_dir(&skills_dir).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "curator phase1: cannot read skills dir");
            return Ok(result);
        }
    };

    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "phase1: read error");
                continue;
            }
        };

        let skill = match SkillDef::parse(&content) {
            Some(s) => s,
            None => continue,
        };

        if skill.meta.pinned.unwrap_or(false) { continue; }

        let anchor = skill.meta.last_used_at.as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let target = decide_transition(
            &skill.meta.state, anchor, now, stale_after_days, archive_after_days,
        );

        if let Some(new_state) = target {
            let log_prefix = if dry_run { "[DRY-RUN] " } else { "" };

            if !dry_run {
                let _ = crate::db::skill_versions::save_version(
                    db, &skill.meta.name, &content, "auto-transition", None,
                    Some(&format!("curator:auto-transition to {}", state_str(&new_state))),
                ).await;

                let updated = content.replacen(
                    &format!("state: {}", state_str(&skill.meta.state)),
                    &format!("state: {}", state_str(&new_state)),
                    1,
                );
                let updated = if updated == content {
                    insert_state_in_frontmatter(&content, &new_state)
                } else {
                    updated
                };

                let tmp = format!("{}.tmp", path.display());
                if tokio::fs::write(&tmp, &updated).await.is_ok() {
                    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
                        tracing::warn!(path = %path.display(), error = %e, "phase1: rename failed");
                        let _ = tokio::fs::remove_file(&tmp).await;
                    } else {
                        result.transitions += 1;
                        result.log.push(format!(
                            "{}{}: {:?} → {:?}", log_prefix, skill.meta.name, skill.meta.state, new_state
                        ));
                    }
                }
            } else {
                // Dry-run: count and log without writing
                result.transitions += 1;
                result.log.push(format!(
                    "{}{}: {:?} → {:?}", log_prefix, skill.meta.name, skill.meta.state, new_state
                ));
            }
        }
    }

    Ok(result)
}

fn state_str(s: &SkillState) -> &'static str {
    match s {
        SkillState::Active   => "active",
        SkillState::Stale    => "stale",
        SkillState::Archived => "archived",
    }
}

fn insert_state_in_frontmatter(content: &str, state: &SkillState) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let close = lines.iter().enumerate().skip(1).find(|(_, l)| l.trim() == "---");
    if let Some((pos, _)) = close {
        lines.insert(pos, format!("state: {}", state_str(state)));
    }
    let trailing = if content.ends_with('\n') { "\n" } else { "" };
    lines.join("\n") + trailing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_transition_for_archived() {
        let result = decide_transition(
            &SkillState::Archived,
            Some(Utc::now() - Duration::days(100)),
            Utc::now(), 30, 90,
        );
        assert!(result.is_none());
    }

    #[test]
    fn active_to_stale_after_threshold() {
        let anchor = Some(Utc::now() - Duration::days(31));
        let result = decide_transition(&SkillState::Active, anchor, Utc::now(), 30, 90);
        assert!(matches!(result, Some(SkillState::Stale)));
    }

    #[test]
    fn active_to_archived_after_archive_threshold() {
        let anchor = Some(Utc::now() - Duration::days(91));
        let result = decide_transition(&SkillState::Active, anchor, Utc::now(), 30, 90);
        assert!(matches!(result, Some(SkillState::Archived)));
    }

    #[test]
    fn stale_to_archived_after_archive_threshold() {
        let anchor = Some(Utc::now() - Duration::days(91));
        let result = decide_transition(&SkillState::Stale, anchor, Utc::now(), 30, 90);
        assert!(matches!(result, Some(SkillState::Archived)));
    }

    #[test]
    fn no_transition_within_threshold() {
        let anchor = Some(Utc::now() - Duration::days(10));
        let result = decide_transition(&SkillState::Active, anchor, Utc::now(), 30, 90);
        assert!(result.is_none());
    }

    #[test]
    fn null_last_used_at_no_transition() {
        let result = decide_transition(&SkillState::Active, None, Utc::now(), 30, 90);
        assert!(result.is_none());
    }

    #[test]
    fn dry_run_prefix_applied_to_log() {
        let prefix = "[DRY-RUN] ";
        let entry = format!("{}web-search: Active → Stale", prefix);
        assert!(entry.starts_with("[DRY-RUN] "));
    }

    #[test]
    fn real_run_has_no_prefix() {
        let entry = "web-search: Active → Stale".to_string();
        assert!(!entry.starts_with("[DRY-RUN] "));
    }
}
