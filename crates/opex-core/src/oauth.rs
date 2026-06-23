use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use anyhow::{Context, Result};
use sqlx::FromRow;
use uuid::Uuid;
use crate::secrets::SecretsManager;

// ---------------------------------------------------------------------------
// Provider config (static registry)
// ---------------------------------------------------------------------------

pub struct OAuthProviderCfg {
    pub name: &'static str,
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    pub userinfo_url: Option<&'static str>,
    /// If set, this provider offers git CLI access via HTTPS token auth.
    /// Value is the hostname (e.g. "github.com", "gitlab.com").
    pub git_host: Option<&'static str>,
}

pub const PROVIDERS: &[OAuthProviderCfg] = &[
    OAuthProviderCfg {
        name: "google",
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        scopes: &[
            "https://www.googleapis.com/auth/gmail.readonly",
            "https://www.googleapis.com/auth/gmail.send",
            "https://www.googleapis.com/auth/calendar",
            "openid", "email",
        ],
        userinfo_url: Some("https://www.googleapis.com/oauth2/v3/userinfo"),
        git_host: None,
    },
    OAuthProviderCfg {
        name: "github",
        auth_url: "https://github.com/login/oauth/authorize",
        token_url: "https://github.com/login/oauth/access_token",
        scopes: &["repo", "user:email"],
        userinfo_url: Some("https://api.github.com/user"),
        git_host: Some("github.com"),
    },
];

pub fn find_provider(name: &str) -> Option<&'static OAuthProviderCfg> {
    PROVIDERS.iter().find(|p| p.name == name)
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct PendingState {
    account_id: Uuid,
    provider: String,
    agent_id: String,
    created_at: std::time::Instant,
}

#[derive(FromRow)]
struct OAuthAccountRow {
    id: Uuid,
    provider: String,
    display_name: Option<String>,
    user_email: Option<String>,
    scope: Option<String>,
    status: String,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    connected_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(FromRow)]
struct BindingRow {
    agent_id: String,
    provider: String,
    account_id: Uuid,
    bound_at: chrono::DateTime<chrono::Utc>,
    // joined from oauth_accounts
    display_name: Option<String>,
    user_email: Option<String>,
    status: String,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    connected_at: Option<chrono::DateTime<chrono::Utc>>,
}

// Vault key constants
const VAULT_CREDENTIALS: &str = "OAUTH_CREDENTIALS";
const VAULT_TOKENS: &str = "OAUTH_TOKENS";

// ---------------------------------------------------------------------------
// OAuthManager
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OAuthManager {
    pub db: sqlx::PgPool,
    pub secrets: Arc<SecretsManager>,
    pub client: reqwest::Client,
    pending: Arc<RwLock<HashMap<String, PendingState>>>,
    pub public_url: String,
}

impl OAuthManager {
    pub fn new(
        db: sqlx::PgPool,
        secrets: Arc<SecretsManager>,
        client: reqwest::Client,
        public_url: String,
    ) -> Self {
        Self { db, secrets, client, pending: Default::default(), public_url }
    }

    /// Create a no-op OAuthManager for unit tests (never issues real requests).
    #[cfg(test)]
    pub fn new_noop() -> Self {
        let db = sqlx::PgPool::connect_lazy("postgres://invalid").expect("lazy pool");
        let secrets = Arc::new(crate::secrets::SecretsManager::new_noop());
        let client = reqwest::Client::new();
        Self { db, secrets, client, pending: Default::default(), public_url: String::new() }
    }

    // -----------------------------------------------------------------------
    // Shared helpers
    // -----------------------------------------------------------------------

    /// Load provider name and static config for an account by its ID.
    async fn load_account_provider(&self, account_id: Uuid) -> Result<(String, &'static OAuthProviderCfg)> {
        let provider_name: String = sqlx::query_scalar("SELECT provider FROM oauth_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&self.db)
            .await?;
        let cfg = find_provider(&provider_name)
            .ok_or_else(|| anyhow::anyhow!("unknown provider: {provider_name}"))?;
        Ok((provider_name, cfg))
    }

    /// Load OAuth client credentials from vault for the given account.
    async fn load_credentials(&self, account_id: Uuid) -> Result<(String, String)> {
        let scope = account_id.to_string();
        let raw = self.secrets.get_scoped(VAULT_CREDENTIALS, &scope).await
            .ok_or_else(|| anyhow::anyhow!("no credentials for account {account_id}"))?;
        let val: serde_json::Value = serde_json::from_str(&raw)?;
        let client_id = val["client_id"].as_str().unwrap_or_default().to_string();
        let client_secret = val["client_secret"].as_str().unwrap_or_default().to_string();
        Ok((client_id, client_secret))
    }

    /// Store token blob in vault for an account.
    async fn store_tokens(&self, account_id: Uuid, access: &str, refresh: Option<&str>, expiry: Option<&str>) -> Result<()> {
        let scope = account_id.to_string();
        let tokens = serde_json::json!({
            "access_token": access,
            "refresh_token": refresh,
            "expiry": expiry,
        });
        self.secrets.set_scoped(VAULT_TOKENS, &scope, &tokens.to_string(), None).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Account CRUD
    // -----------------------------------------------------------------------

    /// Create an OAuth account with per-account credentials stored in vault.
    pub async fn create_account(
        &self,
        provider: &str,
        display_name: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Uuid> {
        find_provider(provider)
            .ok_or_else(|| anyhow::anyhow!("unsupported provider: {provider}"))?;

        let id = Uuid::new_v4();
        let scope_str = find_provider(provider)
            .map(|p| p.scopes.join(" "))
            .unwrap_or_default();

        sqlx::query(
            "INSERT INTO oauth_accounts (id, provider, display_name, scope, status, created_at)
             VALUES ($1, $2, $3, $4, 'disconnected', now())",
        )
        .bind(id)
        .bind(provider)
        .bind(display_name)
        .bind(&scope_str)
        .execute(&self.db)
        .await
        .context("failed to insert oauth_accounts row")?;

        // Store credentials in vault; rollback row on failure
        let creds = serde_json::json!({
            "client_id": client_id,
            "client_secret": client_secret,
        });
        if let Err(e) = self
            .secrets
            .set_scoped(VAULT_CREDENTIALS, &id.to_string(), &creds.to_string(), None)
            .await
        {
            let _ = sqlx::query("DELETE FROM oauth_accounts WHERE id = $1")
                .bind(id)
                .execute(&self.db)
                .await;
            return Err(e).context("failed to store credentials in vault");
        }

        Ok(id)
    }

    /// Delete an OAuth account (CASCADE removes bindings).
    pub async fn delete_account(&self, account_id: Uuid) -> Result<()> {
        let scope = account_id.to_string();
        sqlx::query("DELETE FROM oauth_accounts WHERE id = $1")
            .bind(account_id)
            .execute(&self.db)
            .await?;
        let _ = self.secrets.delete_scoped(VAULT_CREDENTIALS, &scope).await;
        let _ = self.secrets.delete_scoped(VAULT_TOKENS, &scope).await;
        Ok(())
    }

    /// List accounts, optionally filtered by provider.
    pub async fn list_accounts(
        &self,
        provider: Option<&str>,
    ) -> Result<Vec<serde_json::Value>> {
        let rows: Vec<OAuthAccountRow> = match provider {
            Some(prov) => sqlx::query_as::<_, OAuthAccountRow>(
                "SELECT id, provider, display_name, user_email, scope, status,
                        expires_at, connected_at, created_at
                 FROM oauth_accounts WHERE provider = $1
                 ORDER BY created_at",
            )
            .bind(prov)
            .fetch_all(&self.db)
            .await?,
            None => sqlx::query_as::<_, OAuthAccountRow>(
                "SELECT id, provider, display_name, user_email, scope, status,
                        expires_at, connected_at, created_at
                 FROM oauth_accounts ORDER BY provider, created_at",
            )
            .fetch_all(&self.db)
            .await?,
        };

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "provider": r.provider,
                    "display_name": r.display_name,
                    "user_email": r.user_email,
                    "scope": r.scope,
                    "status": r.status,
                    "expires_at": r.expires_at.map(|t| t.to_rfc3339()),
                    "connected_at": r.connected_at.map(|t| t.to_rfc3339()),
                    "created_at": r.created_at.to_rfc3339(),
                })
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Binding CRUD
    // -----------------------------------------------------------------------

    /// Bind an agent to an OAuth account (one account per provider per agent).
    pub async fn bind_account(
        &self,
        agent_id: &str,
        provider: &str,
        account_id: Uuid,
    ) -> Result<()> {
        // Verify account exists and provider matches
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT provider FROM oauth_accounts WHERE id = $1",
        )
        .bind(account_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("account {account_id} not found"))?;

        if row.0 != provider {
            anyhow::bail!(
                "account {} is for provider '{}', not '{}'",
                account_id, row.0, provider
            );
        }

        sqlx::query(
            "INSERT INTO agent_oauth_bindings (agent_id, provider, account_id, bound_at)
             VALUES ($1, $2, $3, now())
             ON CONFLICT (agent_id, provider) DO UPDATE
             SET account_id = EXCLUDED.account_id, bound_at = now()",
        )
        .bind(agent_id)
        .bind(provider)
        .bind(account_id)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    /// Remove an agent's binding for a provider.
    pub async fn unbind_account(&self, agent_id: &str, provider: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM agent_oauth_bindings WHERE agent_id = $1 AND provider = $2",
        )
        .bind(agent_id)
        .bind(provider)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// List all bindings for an agent with joined account details.
    pub async fn list_bindings(
        &self,
        agent_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let rows = sqlx::query_as::<_, BindingRow>(
            "SELECT b.agent_id, b.provider, b.account_id, b.bound_at,
                    a.display_name, a.user_email, a.status, a.expires_at, a.connected_at
             FROM agent_oauth_bindings b
             JOIN oauth_accounts a ON a.id = b.account_id
             WHERE b.agent_id = $1
             ORDER BY b.provider",
        )
        .bind(agent_id)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "agent_id": r.agent_id,
                    "provider": r.provider,
                    "account_id": r.account_id,
                    "bound_at": r.bound_at.to_rfc3339(),
                    "display_name": r.display_name,
                    "user_email": r.user_email,
                    "status": r.status,
                    "expires_at": r.expires_at.map(|t| t.to_rfc3339()),
                    "connected_at": r.connected_at.map(|t| t.to_rfc3339()),
                })
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // OAuth flow
    // -----------------------------------------------------------------------

    /// Start the OAuth authorization flow for a specific account.
    pub async fn init_flow(
        &self,
        account_id: Uuid,
        agent_id: &str,
    ) -> Result<String> {
        let (provider_name, p) = self.load_account_provider(account_id).await?;
        let (client_id, _) = self.load_credentials(account_id).await?;

        let state = Uuid::new_v4().to_string();

        // Evict stale pending states (>10 min) and insert the new one
        {
            let mut pending = self.pending.write().await;
            pending.retain(|_, v| v.created_at.elapsed() < std::time::Duration::from_secs(600));
            pending.insert(
                state.clone(),
                PendingState {
                    account_id,
                    provider: provider_name.clone(),
                    agent_id: agent_id.to_string(),
                    created_at: std::time::Instant::now(),
                },
            );
        }

        let redirect_uri = format!("{}/api/oauth/callback", self.public_url);
        let scope = p.scopes.join(" ");

        let mut url = url::Url::parse(p.auth_url)?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &client_id);
            q.append_pair("redirect_uri", &redirect_uri);
            q.append_pair("response_type", "code");
            q.append_pair("scope", &scope);
            q.append_pair("state", &state);
            if provider_name == "google" {
                q.append_pair("access_type", "offline");
                q.append_pair("prompt", "consent");
            }
        }

        Ok(url.to_string())
    }

    /// Handle the OAuth callback: exchange code for tokens, update DB + vault.
    pub async fn handle_callback(
        &self,
        code: String,
        state_token: String,
    ) -> Result<(String, String)> {
        let pending = self
            .pending
            .write()
            .await
            .remove(&state_token)
            .ok_or_else(|| anyhow::anyhow!("invalid or expired OAuth state"))?;

        let p = find_provider(&pending.provider)
            .ok_or_else(|| anyhow::anyhow!("unknown provider: {}", pending.provider))?;

        let (client_id, client_secret) = self.load_credentials(pending.account_id).await?;

        let redirect_uri = format!("{}/api/oauth/callback", self.public_url);

        let resp = self
            .client
            .post(p.token_url)
            .form(&[
                ("code", code.as_str()),
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("redirect_uri", &redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        let access = resp["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no access_token in token response"))?
            .to_string();
        let refresh = resp["refresh_token"].as_str();
        let expires_in = resp["expires_in"].as_i64();
        let expires_at =
            expires_in.map(|s| chrono::Utc::now() + chrono::Duration::seconds(s));

        self.store_tokens(
            pending.account_id,
            &access,
            refresh,
            expires_at.map(|t| t.to_rfc3339()).as_deref(),
        ).await?;

        // Fetch user email if provider supports it
        let user_email = self.fetch_user_email(p, &access).await;

        // Update account row
        sqlx::query(
            "UPDATE oauth_accounts
             SET status = 'connected', user_email = $1, expires_at = $2, connected_at = now()
             WHERE id = $3",
        )
        .bind(user_email.as_deref())
        .bind(expires_at)
        .bind(pending.account_id)
        .execute(&self.db)
        .await?;

        Ok((pending.agent_id, pending.provider))
    }

    // -----------------------------------------------------------------------
    // Token access
    // -----------------------------------------------------------------------

    /// Get a valid access token for the agent's bound account of the given provider.
    pub async fn get_token(&self, provider: &str, agent_id: &str) -> Result<String> {
        // Find binding
        let (account_id, ): (Uuid, ) = sqlx::query_as(
            "SELECT account_id FROM agent_oauth_bindings
             WHERE agent_id = $1 AND provider = $2",
        )
        .bind(agent_id)
        .bind(provider)
        .fetch_optional(&self.db)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no OAuth binding for {provider}/{agent_id} \u{2014} connect via /integrations"
            )
        })?;

        let scope = account_id.to_string();
        let tokens_json = self
            .secrets
            .get_scoped(VAULT_TOKENS, &scope)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("no OAuth tokens for account {account_id} \u{2014} reconnect")
            })?;
        let tokens: serde_json::Value = serde_json::from_str(&tokens_json)?;

        // Check expiry
        if let Some(expiry_str) = tokens["expiry"].as_str()
            && let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(expiry_str)
                && expiry < chrono::Utc::now() + chrono::Duration::seconds(60) {
                    return self.refresh_token(account_id, provider).await;
                }

        tokens["access_token"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("access_token missing in vault"))
    }

    /// Refresh the access token for a given account.
    async fn refresh_token(&self, account_id: Uuid, provider_name: &str) -> Result<String> {
        let p = find_provider(provider_name)
            .ok_or_else(|| anyhow::anyhow!("unknown provider: {provider_name}"))?;

        let (client_id, client_secret) = self.load_credentials(account_id).await?;

        // Load current refresh token
        let scope = account_id.to_string();
        let tokens_json = self
            .secrets
            .get_scoped(VAULT_TOKENS, &scope)
            .await
            .ok_or_else(|| anyhow::anyhow!("no tokens for account {account_id}"))?;
        let old_tokens: serde_json::Value = serde_json::from_str(&tokens_json)?;
        let refresh = old_tokens["refresh_token"]
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!("no refresh_token for account {account_id}")
            })?;

        let resp = self
            .client
            .post(p.token_url)
            .form(&[
                ("refresh_token", refresh),
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        let new_access = resp["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no access_token in refresh response"))?
            .to_string();
        let new_refresh = resp["refresh_token"]
            .as_str()
            .or_else(|| old_tokens["refresh_token"].as_str());
        let expires_in = resp["expires_in"].as_i64();
        let expires_at =
            expires_in.map(|s| chrono::Utc::now() + chrono::Duration::seconds(s));

        self.store_tokens(
            account_id,
            &new_access,
            new_refresh,
            expires_at.map(|t| t.to_rfc3339()).as_deref(),
        ).await?;

        // Update expires_at in DB
        if let Some(exp) = expires_at {
            let _ = sqlx::query(
                "UPDATE oauth_accounts SET expires_at = $1 WHERE id = $2",
            )
            .bind(Some(exp))
            .bind(account_id)
            .execute(&self.db)
            .await;
        }

        Ok(new_access)
    }

    /// Revoke tokens for an account (set status to disconnected).
    pub async fn revoke(&self, account_id: Uuid) -> Result<()> {
        let scope = account_id.to_string();
        let _ = self.secrets.delete_scoped(VAULT_TOKENS, &scope).await;
        sqlx::query(
            "UPDATE oauth_accounts SET status = 'disconnected', expires_at = NULL
             WHERE id = $1",
        )
        .bind(account_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Backward-compatible listing
    // -----------------------------------------------------------------------

    /// List connections for an agent (backward-compatible JSON shape).
    pub async fn list_connections(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>> {
        let Some(aid) = agent_id else {
            // No agent filter — query all bindings (rare, backward compat)
            let rows = sqlx::query_as::<_, BindingRow>(
                "SELECT b.agent_id, b.provider, b.account_id, b.bound_at,
                        a.display_name, a.user_email, a.status, a.expires_at, a.connected_at
                 FROM agent_oauth_bindings b
                 JOIN oauth_accounts a ON a.id = b.account_id
                 ORDER BY b.agent_id, b.provider",
            )
            .fetch_all(&self.db)
            .await?;

            return Ok(rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "provider": r.provider,
                        "agent_id": r.agent_id,
                        "user_email": r.user_email,
                        "expires_at": r.expires_at.map(|t| t.to_rfc3339()),
                        "connected_at": r.connected_at.map(|t| t.to_rfc3339()),
                    })
                })
                .collect());
        };

        let bindings = self.list_bindings(aid).await?;
        Ok(bindings.iter().map(|b| serde_json::json!({
            "provider": b.get("provider"),
            "agent_id": b.get("agent_id"),
            "user_email": b.get("user_email"),
            "expires_at": b.get("expires_at"),
            "connected_at": b.get("connected_at"),
        })).collect())
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    async fn fetch_user_email(
        &self,
        p: &OAuthProviderCfg,
        access: &str,
    ) -> Option<String> {
        let url = p.userinfo_url?;
        let resp = self
            .client
            .get(url)
            .bearer_auth(access)
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;
        resp["email"].as_str().map(str::to_string)
    }
}

// ---------------------------------------------------------------------------
// Startup migration: oauth_connections -> oauth_accounts + agent_oauth_bindings
// ---------------------------------------------------------------------------

/// Migrate legacy `oauth_connections` table to the new account-based schema.
/// Idempotent: no-op if `oauth_connections` does not exist.
pub async fn migrate_oauth_vault(
    db: &sqlx::PgPool,
    secrets: &SecretsManager,
) -> Result<()> {
    // Check if old table exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'oauth_connections'
        )",
    )
    .fetch_one(db)
    .await?;

    if !exists {
        return Ok(());
    }

    tracing::info!("migrating oauth_connections to oauth_accounts...");

    #[derive(FromRow)]
    struct LegacyRow {
        provider: String,
        agent_id: String,
        user_email: Option<String>,
        scope: Option<String>,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
        connected_at: chrono::DateTime<chrono::Utc>,
    }

    let rows = sqlx::query_as::<_, LegacyRow>(
        "SELECT provider, agent_id, user_email, scope, expires_at, connected_at
         FROM oauth_connections",
    )
    .fetch_all(db)
    .await?;

    for row in &rows {
        let account_id = Uuid::new_v4();
        let scope = account_id.to_string();
        let display_name = format!(
            "{} ({})",
            row.provider,
            row.user_email.as_deref().unwrap_or(&row.agent_id)
        );

        // Insert account
        sqlx::query(
            "INSERT INTO oauth_accounts
                (id, provider, display_name, user_email, scope, status, expires_at, connected_at, created_at)
             VALUES ($1, $2, $3, $4, $5, 'connected', $6, $7, $7)",
        )
        .bind(account_id)
        .bind(&row.provider)
        .bind(&display_name)
        .bind(row.user_email.as_deref())
        .bind(row.scope.as_deref())
        .bind(row.expires_at)
        .bind(row.connected_at)
        .execute(db)
        .await?;

        // Insert binding
        sqlx::query(
            "INSERT INTO agent_oauth_bindings (agent_id, provider, account_id, bound_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (agent_id, provider) DO NOTHING",
        )
        .bind(&row.agent_id)
        .bind(&row.provider)
        .bind(account_id)
        .bind(row.connected_at)
        .execute(db)
        .await?;

        // Migrate tokens from old vault format
        let old_prefix = format!(
            "OAUTH_{}_{}",
            row.provider.to_uppercase(),
            row.agent_id.to_uppercase()
        );

        let access = secrets
            .get_scoped(&format!("{old_prefix}_ACCESS"), &row.agent_id)
            .await;
        let refresh = secrets
            .get_scoped(&format!("{old_prefix}_REFRESH"), &row.agent_id)
            .await;
        let expiry = secrets
            .get_scoped(&format!("{old_prefix}_EXPIRY"), &row.agent_id)
            .await;

        if let Some(at) = &access {
            let tokens = serde_json::json!({
                "access_token": at,
                "refresh_token": refresh,
                "expiry": expiry,
            });
            secrets
                .set_scoped(VAULT_TOKENS, &scope, &tokens.to_string(), None)
                .await?;
        }

        // Migrate global credentials
        let provider_upper = row.provider.to_uppercase();
        let client_id = secrets
            .get_scoped(&format!("{provider_upper}_CLIENT_ID"), "")
            .await;
        let client_secret = secrets
            .get_scoped(&format!("{provider_upper}_CLIENT_SECRET"), "")
            .await;

        if let (Some(cid), Some(csec)) = (&client_id, &client_secret) {
            let creds = serde_json::json!({
                "client_id": cid,
                "client_secret": csec,
            });
            secrets
                .set_scoped(VAULT_CREDENTIALS, &scope, &creds.to_string(), None)
                .await?;
        }

        // Delete old vault entries
        for suffix in &["ACCESS", "REFRESH", "EXPIRY"] {
            let _ = secrets
                .delete_scoped(
                    &format!("{old_prefix}_{suffix}"),
                    &row.agent_id,
                )
                .await;
        }

        tracing::info!(
            "migrated oauth connection: {}/{} -> account {}",
            row.provider,
            row.agent_id,
            account_id
        );
    }

    // Drop old table after successful migration
    sqlx::query("DROP TABLE oauth_connections")
        .execute(db)
        .await?;

    tracing::info!("oauth migration complete, dropped oauth_connections table");
    Ok(())
}
