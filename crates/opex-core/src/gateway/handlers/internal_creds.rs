//! `GET /api/internal/its-credentials` — resolve ИТС 1C login/password from the
//! secrets vault, for the `toolgate` `its/creds.py` browser-login flow.
//!
//! **Authentication:** standard Bearer-token auth middleware — this endpoint
//! is NOT in `PUBLIC_EXACT` / `PUBLIC_PREFIX` / `LOOPBACK_EXACT` in
//! `gateway/middleware.rs`, so an auth header is required for every caller,
//! including loopback (same shape as `POST /api/llm/complete`, see
//! `gateway/handlers/llm.rs`). `toolgate` already holds `OPEX_AUTH_TOKEN` for
//! this kind of call-back into core.
//!
//! Secret `ITS_CREDENTIALS` is stored as a JSON string `{"login","password"}`
//! under the global scope (`""`) in the vault. Returns 404 if unset, 500 if
//! the stored value is not valid JSON in the expected shape. The secret value
//! is never logged.

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get, Json};
use serde::{Deserialize, Serialize};

use crate::gateway::clusters::AuthServices;
use crate::gateway::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ItsCreds {
    pub login: String,
    pub password: String,
}

async fn get_its_credentials(State(auth): State<AuthServices>) -> impl IntoResponse {
    // Global scope only ("") — no per-agent scoping, no env-var fallback
    // surprise for a credential this sensitive (mirrors `get_secret`'s
    // scope-less path in handlers/secrets.rs).
    match auth.secrets.get_strict("ITS_CREDENTIALS").await {
        Some(raw) => match serde_json::from_str::<ItsCreds>(&raw) {
            Ok(creds) => Json(creds).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("ITS_CREDENTIALS malformed: {e}"),
            )
                .into_response(),
        },
        None => (StatusCode::NOT_FOUND, "ITS_CREDENTIALS not set").into_response(),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/internal/its-credentials", get(get_its_credentials))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_credentials_json() {
        let v: ItsCreds = serde_json::from_str(r#"{"login":"u","password":"p"}"#).unwrap();
        assert_eq!(v.login, "u");
        assert_eq!(v.password, "p");
    }
}
