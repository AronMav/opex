//! `/api/watchdog/*` — read/write of `config/watchdog.toml`,
//! `watchdog_settings` DB rows, and `restart_cmd` execution for a
//! configured check.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::{Value, json};

use crate::gateway::clusters::InfraServices;

/// GET /api/watchdog/status
pub(crate) async fn api_watchdog_status() -> impl IntoResponse {
    match tokio::fs::read_to_string("/tmp/hydeclaw-watchdog.json").await {
        Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(v) => Json(v).into_response(),
            Err(_) => Json(json!({"error": "invalid status file"})).into_response(),
        },
        Err(_) => Json(json!({"error": "watchdog not running"})).into_response(),
    }
}

/// GET /api/watchdog/config
pub(crate) async fn api_watchdog_config() -> impl IntoResponse {
    match tokio::fs::read_to_string("config/watchdog.toml").await {
        Ok(text) => Json(json!({"config": text})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// POST /api/watchdog/restart/{name} — execute `restart_cmd` for a watchdog check
pub(crate) async fn api_watchdog_restart_check(
    Path(name): Path<String>,
) -> impl IntoResponse {
    let config_text = match tokio::fs::read_to_string("config/watchdog.toml").await {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let config: toml::Value = match toml::from_str(&config_text) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let checks = config.get("checks").and_then(|v| v.as_array());
    let restart_cmd = checks.and_then(|arr| {
        arr.iter().find(|c| c.get("name").and_then(|n| n.as_str()) == Some(&name))
            .and_then(|c| c.get("restart_cmd").and_then(|r| r.as_str()))
    });
    let Some(cmd) = restart_cmd else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": format!("no restart_cmd for check '{}'", name)}))).into_response();
    };
    tracing::info!(check = %name, cmd, "watchdog restart requested via API");
    let output = tokio::process::Command::new("bash").args(["-c", cmd]).output().await;
    match output {
        Ok(o) if o.status.success() => Json(json!({"ok": true, "check": name})).into_response(),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": err.to_string()}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

/// GET /api/watchdog/settings — read alerting settings from DB
pub(crate) async fn api_watchdog_settings(
    State(infra): State<InfraServices>,
) -> Json<Value> {
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT key, value FROM watchdog_settings",
    )
    .fetch_all(&infra.db)
    .await
    .unwrap_or_default();

    let mut settings = serde_json::Map::new();
    for (key, value) in rows {
        settings.insert(key, value);
    }
    Json(Value::Object(settings))
}

/// PUT /api/watchdog/settings — update alerting settings
pub(crate) async fn api_watchdog_settings_update(
    State(infra): State<InfraServices>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(obj) = body.as_object() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "expected JSON object"}))).into_response();
    };

    // Audit 2026-05-08: validate value shape per key BEFORE writing the
    // JSONB to DB. Without this, a `{"alert_channel_ids": "not-an-array"}`
    // body would store an invalid type that the watchdog binary then panics
    // on at deserialise time. Both keys are arrays of strings (UUIDs / event
    // names); the watchdog parses event strings into a known enum elsewhere.
    let allowed = ["alert_channel_ids", "alert_events"];
    for (key, value) in obj {
        if !allowed.contains(&key.as_str()) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("unknown key: {}", key)}))).into_response();
        }
        let arr = match value.as_array() {
            Some(a) => a,
            None => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("{key} must be a JSON array")}))).into_response();
            }
        };
        for item in arr {
            if !item.is_string() {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("{key} array elements must all be strings")}))).into_response();
            }
        }
        if key == "alert_channel_ids" {
            for item in arr {
                let s = item.as_str().unwrap_or_default();
                if uuid::Uuid::parse_str(s).is_err() {
                    return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("alert_channel_ids item '{s}' is not a valid UUID")}))).into_response();
                }
            }
        }
        if let Err(e) = sqlx::query(
            "INSERT INTO watchdog_settings (key, value, updated_at) VALUES ($1, $2, now())
             ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = now()",
        )
        .bind(key)
        .bind(value)
        .execute(&infra.db)
        .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    }

    Json(json!({"ok": true})).into_response()
}

/// PUT /api/watchdog/config
pub(crate) async fn api_watchdog_config_update(Json(req): Json<serde_json::Value>) -> impl IntoResponse {
    let text = match req.get("config").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "config field required"}))).into_response(),
    };
    if toml::from_str::<toml::Value>(text).is_err() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid TOML"}))).into_response();
    }
    match tokio::fs::write("config/watchdog.toml", text).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
