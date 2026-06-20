//! Session-scoped TODO list storage (table `session_todos`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

pub async fn list_todos(db: &PgPool, session_id: Uuid) -> Result<Vec<TodoItem>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT item_id, content, status FROM session_todos
         WHERE session_id = $1 ORDER BY position, created_at",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, content, status)| TodoItem { id, content, status })
        .collect())
}

pub async fn replace_todos(db: &PgPool, session_id: Uuid, items: &[TodoItem]) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM session_todos WHERE session_id = $1")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    for (i, it) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO session_todos (session_id, item_id, content, status, position)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(session_id)
        .bind(&it.id)
        .bind(&it.content)
        .bind(&it.status)
        .bind(i as i32)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn merge_todos(db: &PgPool, session_id: Uuid, items: &[TodoItem]) -> Result<()> {
    let mut tx = db.begin().await?;
    for it in items {
        sqlx::query(
            "INSERT INTO session_todos (session_id, item_id, content, status)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (session_id, item_id)
             DO UPDATE SET content = EXCLUDED.content, status = EXCLUDED.status, updated_at = now()",
        )
        .bind(session_id)
        .bind(&it.id)
        .bind(&it.content)
        .bind(&it.status)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

const MAX_ITEMS: usize = 256;
const MAX_CONTENT_CHARS: usize = 4000;
const VALID_STATUSES: &[&str] = &["pending", "in_progress", "done", "cancelled"];

/// Parse + validate the `items` array from a `todo` tool call.
pub fn parse_items(args: &serde_json::Value) -> std::result::Result<Vec<TodoItem>, String> {
    let arr = args
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "write mode requires an 'items' array".to_string())?;
    if arr.len() > MAX_ITEMS {
        return Err(format!("too many items ({}, max {MAX_ITEMS})", arr.len()));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, it) in arr.iter().enumerate() {
        let id = it
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("item {i}: missing 'id'"))?;
        let content = it
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("item {i}: missing 'content'"))?;
        if content.chars().count() > MAX_CONTENT_CHARS {
            return Err(format!("item {i}: content exceeds {MAX_CONTENT_CHARS} chars"));
        }
        let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
        if !VALID_STATUSES.contains(&status) {
            return Err(format!("item {i}: invalid status '{status}' (use {})", VALID_STATUSES.join("|")));
        }
        out.push(TodoItem {
            id: id.to_string(),
            content: content.to_string(),
            status: status.to_string(),
        });
    }
    Ok(out)
}

/// Render the TODO list as a system-prompt context block.
pub fn format_for_injection(items: &[TodoItem]) -> String {
    let mut s = String::from("## Active TODO\n");
    for it in items {
        let mark = match it.status.as_str() {
            "done" => "[x]",
            "in_progress" => "[~]",
            "cancelled" => "[-]",
            _ => "[ ]",
        };
        s.push_str(&format!("- {mark} {} (id: {})\n", it.content, it.id));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_items_reads_and_validates() {
        let v = serde_json::json!({"items": [
            {"id": "1", "content": "do x", "status": "pending"},
            {"id": "2", "content": "do y", "status": "done"}
        ]});
        let items = parse_items(&v).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].status, "done");
    }

    #[test]
    fn parse_items_rejects_bad_status() {
        let v = serde_json::json!({"items": [{"id": "1", "content": "x", "status": "wat"}]});
        assert!(parse_items(&v).is_err());
    }

    #[test]
    fn parse_items_enforces_limits() {
        let big = "x".repeat(4001);
        let v = serde_json::json!({"items": [{"id": "1", "content": big, "status": "pending"}]});
        assert!(parse_items(&v).is_err());
    }

    #[test]
    fn format_for_injection_renders_block() {
        let items = vec![
            TodoItem { id: "1".into(), content: "first".into(), status: "in_progress".into() },
            TodoItem { id: "2".into(), content: "second".into(), status: "done".into() },
        ];
        let s = format_for_injection(&items);
        assert!(s.contains("## Active TODO"));
        assert!(s.contains("first"));
        assert!(s.contains("[~]") && s.contains("[x]"));
    }

    async fn seed_session(pool: &PgPool) -> Uuid {
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'Test', 'u', 'ui')")
            .bind(sid)
            .execute(pool)
            .await
            .unwrap();
        sid
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn replace_then_list_roundtrip(pool: PgPool) -> sqlx::Result<()> {
        let sid = seed_session(&pool).await;
        let items = vec![
            TodoItem { id: "1".into(), content: "first".into(), status: "pending".into() },
            TodoItem { id: "2".into(), content: "second".into(), status: "in_progress".into() },
        ];
        replace_todos(&pool, sid, &items).await.unwrap();
        let got = list_todos(&pool, sid).await.unwrap();
        assert_eq!(got, items);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn merge_upserts_by_id(pool: PgPool) -> sqlx::Result<()> {
        let sid = seed_session(&pool).await;
        replace_todos(&pool, sid, &[TodoItem { id: "1".into(), content: "a".into(), status: "pending".into() }]).await.unwrap();
        merge_todos(&pool, sid, &[TodoItem { id: "1".into(), content: "a".into(), status: "done".into() }]).await.unwrap();
        let got = list_todos(&pool, sid).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].status, "done");
        Ok(())
    }
}
