use sqlx::PgPool;
use chrono::{DateTime, Utc};

// ── Row type ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct CuratorDecisionRow {
    pub id:         i32,
    pub skill_name: String,
    pub action:     String,
    pub reason:     Option<String>,
    pub decided_at: DateTime<Utc>,
}

// ── Write ─────────────────────────────────────────────────────────────────────

pub async fn save_decision(
    db: &PgPool,
    skill_name: &str,
    action: &str,
    reason: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO curator_decisions (skill_name, action, reason) VALUES ($1, $2, $3)"
    )
    .bind(skill_name)
    .bind(action)
    .bind(reason)
    .execute(db)
    .await
    .map(|_| ())
}

// ── Read ──────────────────────────────────────────────────────────────────────

/// Return the last `limit` decisions for a specific skill, newest-first.
pub async fn list_decisions(
    db: &PgPool,
    skill_name: &str,
    limit: i64,
) -> sqlx::Result<Vec<CuratorDecisionRow>> {
    sqlx::query_as::<_, CuratorDecisionRow>(
        "SELECT * FROM curator_decisions \
         WHERE skill_name = $1 ORDER BY decided_at DESC LIMIT $2"
    )
    .bind(skill_name)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Return the most recent decision per skill (DISTINCT ON).
/// Skills with no decisions are absent from the result.
pub async fn list_recent(db: &PgPool) -> sqlx::Result<Vec<CuratorDecisionRow>> {
    sqlx::query_as::<_, CuratorDecisionRow>(
        "SELECT DISTINCT ON (skill_name) id, skill_name, action, reason, decided_at \
         FROM curator_decisions ORDER BY skill_name, decided_at DESC"
    )
    .fetch_all(db)
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curator_decision_row_derives_serialize() {
        let row = CuratorDecisionRow {
            id: 1,
            skill_name: "daily-reflection".into(),
            action: "reject".into(),
            reason: Some("from_section not found".into()),
            decided_at: chrono::Utc::now(),
        };
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("daily-reflection"));
        assert!(s.contains("reject"));
        assert!(s.contains("from_section not found"));
    }

    #[test]
    fn curator_decision_row_null_reason_serializes() {
        let row = CuratorDecisionRow {
            id: 2,
            skill_name: "verification".into(),
            action: "archive".into(),
            reason: None,
            decided_at: chrono::Utc::now(),
        };
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("\"reason\":null"));
    }
}
