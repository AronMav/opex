use std::collections::HashMap;
use std::sync::Arc;

use crate::gateway::state::AccessGuardMap;
use crate::secrets::SecretsManager;

// ── Google OAuth pending-session types ───────────────────────────────────────

/// Distinguishes which OAuth2 flow created the pending session.
/// Retained for diagnostics and future UI work (D16); not used in current
/// poll logic.
// allow(dead_code): variants consumed by OAuth route handlers in a later task.
#[cfg(feature = "gemini-cloudcode")]
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum OAuthFlowKind {
    Code,
    Device,
}

/// Result written into the watch channel when the background OAuth task
/// finishes (success or failure).
// allow(dead_code): variants consumed by OAuth route handlers in a later task.
#[cfg(feature = "gemini-cloudcode")]
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum OAuthPollResult {
    Ok { email: String },
    Err { message: String },
}

/// State held for one in-flight Google OAuth login attempt.
// allow(dead_code): fields consumed by OAuth poll handlers in a later task.
#[cfg(feature = "gemini-cloudcode")]
#[allow(dead_code)]
#[derive(Clone)]
pub struct PendingOAuthSession {
    /// Creation time used by the reaper task to evict sessions older than
    /// 10 minutes.
    pub created_at: std::time::Instant,
    /// Sender side — written exactly once by the background OAuth task on
    /// completion or error.
    pub result_tx: Arc<tokio::sync::watch::Sender<Option<OAuthPollResult>>>,
    /// Receiver side — cloned per poll request so each caller sees the same
    /// notification.
    pub result_rx: tokio::sync::watch::Receiver<Option<OAuthPollResult>>,
    /// Stored for diagnostics and future UI extensions (e.g., active-sessions
    /// audit page D16). Not used in current poll logic.
    pub flow_kind: OAuthFlowKind,
}

/// Process-wide map of pending Google OAuth sessions, keyed by state token
/// (authorization-code flow) or device_code (device flow).
/// Entries are reaped by a background task 10 minutes after insertion.
#[cfg(feature = "gemini-cloudcode")]
pub type GoogleOAuthSessions = dashmap::DashMap<String, PendingOAuthSession>;

// ── AuthServices cluster ─────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuthServices {
    pub secrets:       Arc<SecretsManager>,
    pub access_guards: AccessGuardMap,
    pub oauth:         Arc<crate::oauth::OAuthManager>,
    pub ws_tickets:    Arc<tokio::sync::Mutex<HashMap<String, std::time::Instant>>>,
    /// Pending Google OAuth sessions for the `/api/auth/google/*` endpoints.
    /// Keyed by state token (code flow) or device_code (device flow).
    /// A background reaper removes entries older than 10 minutes.
    // allow(dead_code): consumed by OAuth route handlers added in a later task.
    #[cfg(feature = "gemini-cloudcode")]
    #[allow(dead_code)]
    pub google_oauth_sessions: Arc<GoogleOAuthSessions>,
}

impl AuthServices {
    pub fn new(
        secrets: Arc<SecretsManager>,
        access_guards: AccessGuardMap,
        oauth: Arc<crate::oauth::OAuthManager>,
        ws_tickets: Arc<tokio::sync::Mutex<HashMap<String, std::time::Instant>>>,
    ) -> Self {
        Self {
            secrets,
            access_guards,
            oauth,
            ws_tickets,
            #[cfg(feature = "gemini-cloudcode")]
            google_oauth_sessions: Arc::new(dashmap::DashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn test_new() -> Self {
        Self {
            secrets:       Arc::new(SecretsManager::new_noop()),
            access_guards: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            oauth:         Arc::new(crate::oauth::OAuthManager::new_noop()),
            ws_tickets:    Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(feature = "gemini-cloudcode")]
            google_oauth_sessions: Arc::new(dashmap::DashMap::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auth_services_ws_tickets_empty_on_new() {
        let auth = AuthServices::test_new();
        let tickets = auth.ws_tickets.lock().await;
        assert!(tickets.is_empty());
    }

    #[cfg(feature = "gemini-cloudcode")]
    #[tokio::test]
    async fn google_oauth_sessions_empty_on_new() {
        let auth = AuthServices::test_new();
        assert!(auth.google_oauth_sessions.is_empty());
    }

    #[cfg(feature = "gemini-cloudcode")]
    #[test]
    fn pending_oauth_session_result_roundtrip() {
        use tokio::sync::watch;
        let (tx, rx) = watch::channel(None::<OAuthPollResult>);
        let session = PendingOAuthSession {
            created_at: std::time::Instant::now(),
            result_tx: Arc::new(tx),
            result_rx: rx.clone(),
            flow_kind: OAuthFlowKind::Code,
        };
        assert_eq!(session.flow_kind, OAuthFlowKind::Code);
        assert!(session.result_rx.borrow().is_none());
    }
}
