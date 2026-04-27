use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn check_repo_access(db: &PgPool, agent_id: &str, owner: &str, repo: &str) -> Result<bool> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM agent_github_repos WHERE agent_id = $1 AND lower(owner) = lower($2) AND lower(repo) = lower($3)"
    )
    .bind(agent_id)
    .bind(owner)
    .bind(repo)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct GitHubRepo {
    pub id: Uuid,
    pub agent_id: String,
    pub owner: String,
    pub repo: String,
    pub added_at: chrono::DateTime<chrono::Utc>,
}
crate::register_ts_dto!(GitHubRepo);

pub async fn list_repos(db: &PgPool, agent_id: &str) -> Result<Vec<GitHubRepo>> {
    let rows = sqlx::query_as::<_, GitHubRepo>(
        "SELECT id, agent_id, owner, repo, added_at FROM agent_github_repos WHERE agent_id = $1 ORDER BY owner, repo"
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn add_repo(db: &PgPool, agent_id: &str, owner: &str, repo: &str) -> Result<GitHubRepo> {
    let row = sqlx::query_as::<_, GitHubRepo>(
        "INSERT INTO agent_github_repos (agent_id, owner, repo) VALUES ($1, $2, $3) \
         ON CONFLICT (agent_id, owner, repo) DO UPDATE SET added_at = agent_github_repos.added_at \
         RETURNING id, agent_id, owner, repo, added_at"
    )
    .bind(agent_id)
    .bind(owner)
    .bind(repo)
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn remove_repo(db: &PgPool, id: Uuid, agent_id: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM agent_github_repos WHERE id = $1 AND agent_id = $2")
        .bind(id)
        .bind(agent_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}
