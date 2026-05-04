use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Recursively walk a JSON value tree, replacing unsigned `/uploads/` URLs
/// with HMAC-signed equivalents. Returns the number of replacements made.
///
/// A URL is considered unsigned when it matches the pattern but does NOT
/// already contain `?sig=` immediately after the extension.
pub fn sign_uploads_in_value(val: &mut Value, re: &Regex, key: &[u8; 32]) -> usize {
    match val {
        Value::String(s) => {
            if !s.contains("/uploads/") {
                return 0;
            }
            let original = s.clone();
            let mut count = 0usize;
            let result = re.replace_all(&original, |caps: &regex::Captures| {
                let end = caps.get(0).unwrap().end();
                // Skip if already signed (match is immediately followed by ?sig=)
                if original[end..].starts_with("?sig=") {
                    return caps.get(0).unwrap().as_str().to_string();
                }
                count += 1;
                crate::uploads::mint_signed_url(
                    "",
                    &caps[1],
                    key,
                    crate::uploads::HISTORICAL_URL_TTL_SECS,
                )
            });
            if count > 0 {
                *s = result.into_owned();
            }
            count
        }
        Value::Array(arr) => arr
            .iter_mut()
            .map(|v| sign_uploads_in_value(v, re, key))
            .sum(),
        Value::Object(map) => map
            .values_mut()
            .map(|v| sign_uploads_in_value(v, re, key))
            .sum(),
        _ => 0,
    }
}

/// One-shot startup migration: sign all unsigned `/uploads/` URLs stored in
/// `messages.content`. Guarded by a `system_flags` key so it runs exactly
/// once per installation.
///
/// Returns the number of message rows updated.
pub async fn run_upload_signature_migration(
    db: &PgPool,
    upload_key: &[u8; 32],
) -> Result<usize> {
    // Gate check — returns early if migration already ran
    let already_done: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM system_flags \
         WHERE key = 'upload_sigs_migrated_v1')",
    )
    .fetch_one(db)
    .await?;

    if already_done {
        return Ok(0);
    }

    let re = Regex::new(r"/uploads/([a-f0-9\-]+\.[a-z0-9.]+)")
        .expect("hardcoded regex is valid");

    let mut total_updated = 0usize;
    let mut processed = 0usize;
    let mut last_id: Option<Uuid> = None;

    loop {
        // Cursor-based pagination using the primary key — stable even as rows
        // are updated (signed URLs still contain '/uploads/' so still match).
        let rows: Vec<(Uuid, Value)> = if let Some(cursor) = last_id {
            sqlx::query_as(
                "SELECT id, content FROM messages \
                 WHERE content::text LIKE '%/uploads/%' \
                 AND id > $1 \
                 ORDER BY id LIMIT 500",
            )
            .bind(cursor)
            .fetch_all(db)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, content FROM messages \
                 WHERE content::text LIKE '%/uploads/%' \
                 ORDER BY id LIMIT 500",
            )
            .fetch_all(db)
            .await?
        };

        if rows.is_empty() {
            break;
        }

        last_id = rows.last().map(|(id, _)| *id);

        for (id, mut content) in rows {
            let replacements = sign_uploads_in_value(&mut content, &re, upload_key);
            if replacements > 0 {
                if let Err(e) = sqlx::query(
                    "UPDATE messages SET content = $1 WHERE id = $2",
                )
                .bind(&content)
                .bind(id)
                .execute(db)
                .await
                {
                    tracing::warn!(
                        message_id = %id,
                        error = %e,
                        "upload migration: skipped row"
                    );
                    continue;
                }
                total_updated += 1;
            }
            processed += 1;
            if processed % 1_000 == 0 {
                tracing::info!(processed, "upload migration in progress");
            }
        }
    }

    // Write gate — idempotent via ON CONFLICT
    sqlx::query(
        "INSERT INTO system_flags (key, value) \
         VALUES ('upload_sigs_migrated_v1', 'true'::jsonb) \
         ON CONFLICT (key) DO UPDATE \
         SET value = 'true'::jsonb, updated_at = now()",
    )
    .execute(db)
    .await?;

    Ok(total_updated)
}
