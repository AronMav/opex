//! Session shares — durable, read-only shareable links (Tier-3 #6).
//!
//! One active share per session. The unguessable `token` is the security
//! boundary: `GET /api/shares/{token}` is auth-exempt (like the HMAC-signed
//! `/api/uploads/*` reads), so knowing the token is what grants read access.
//! Revoking deletes the row.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Create a share for `session_id`, or return the existing token if one is
/// already active (idempotent — re-sharing yields the same link). The caller
/// supplies the random `token` (generated in core, which owns the RNG).
pub async fn create_or_get_share(
    db: &PgPool,
    session_id: Uuid,
    token: &str,
    created_by: &str,
) -> Result<String> {
    // ON CONFLICT keeps the existing row (stable link); RETURNING gives us
    // whichever token is now current.
    let row: (String,) = sqlx::query_as(
        "INSERT INTO session_shares (token, session_id, created_by)
         VALUES ($1, $2, $3)
         ON CONFLICT (session_id) DO UPDATE SET session_id = EXCLUDED.session_id
         RETURNING token",
    )
    .bind(token)
    .bind(session_id)
    .bind(created_by)
    .fetch_one(db)
    .await?;
    Ok(row.0)
}

/// Resolve a share token to its session id (`None` if unknown/revoked).
pub async fn session_for_token(db: &PgPool, token: &str) -> Result<Option<Uuid>> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT session_id FROM session_shares WHERE token = $1")
            .bind(token)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0))
}

/// The current share token for a session, if shared.
pub async fn token_for_session(db: &PgPool, session_id: Uuid) -> Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT token FROM session_shares WHERE session_id = $1")
            .bind(session_id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0))
}

/// Revoke (delete) a session's share. Returns true if a row was removed.
pub async fn delete_share_for_session(db: &PgPool, session_id: Uuid) -> Result<bool> {
    let res = sqlx::query("DELETE FROM session_shares WHERE session_id = $1")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}
