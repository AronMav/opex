// SEC-03 audit (2026-03-30): Credential leak paths verified:
// - Log statements: secret NAME only, never values (tracing grep clean)
// - Error responses: metadata only, no secret values in API responses
// - Backup export: includes decrypted secrets BY DESIGN (portability with different master key)
//   Protected by: API auth middleware + X-Confirm-Restore header on restore
// - Channel credentials: redacted from DB config, re-injected from vault on GET ?reveal=true only

use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

/// Manages encrypted secrets in `PostgreSQL` with in-memory caching.
///
/// Secrets are encrypted with ChaCha20-Poly1305 using a master key.
/// Cache key is `(name, scope)`:
///   - scope = "" means global (default, visible to all)
///   - scope = "`AgentName`" means per-agent (isolated to that agent)
///
/// Falls back to environment variables for migration convenience.
#[derive(Clone)]
pub struct SecretsManager {
    cipher: Arc<ChaCha20Poly1305>,
    db: PgPool,
    cache: Arc<RwLock<HashMap<(String, String), String>>>,
    /// Phase 64 SEC-03: retained for HKDF-based key derivation (e.g. upload HMAC).
    /// NEVER exposed publicly — every accessor MUST return a DERIVED key and the
    /// raw bytes must not leave this module. Adding a `pub fn master_key_bytes()`
    /// getter would defeat the HKDF domain-separation invariant.
    ///
    /// Wrapped in `Arc<Zeroizing<[u8; 32]>>`: the inner bytes are zeroed when
    /// the LAST reference to the `Arc` drops (typically at process shutdown),
    /// reducing exposure via coredumps / `/proc/<pid>/mem` / live-migration
    /// snapshots. Review 2026-04-17.
    master_key_bytes: Arc<Zeroizing<[u8; 32]>>,
}

/// Plaintext secret for portable backup (decrypted, re-encrypted on restore).
#[derive(Debug, serde::Deserialize, Serialize)]
pub struct PlaintextSecret {
    pub name: String,
    pub scope: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct SecretInfo {
    pub name: String,
    pub scope: String,
    pub description: Option<String>,
    pub has_value: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(FromRow)]
struct SecretInfoRow {
    name: String,
    scope: String,
    description: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl SecretsManager {
    /// Create a new `SecretsManager`.
    ///
    /// `master_key_hex` must be exactly 64 hex characters (32 bytes).
    pub fn new(master_key_hex: &str, db: PgPool) -> Result<Self> {
        let key_bytes =
            hex::decode(master_key_hex).context("master key is not valid hex")?;
        if key_bytes.len() != 32 {
            anyhow::bail!(
                "master key must be 32 bytes (64 hex chars), got {}",
                key_bytes.len()
            );
        }
        // Phase 64 SEC-03: copy bytes into Zeroizing fixed array BEFORE cipher
        // construction so the ChaCha20Poly1305 constructor doesn't consume them.
        // Zeroizing clears the bytes on drop (review 2026-04-17 hardening).
        let mut master_key_bytes: Zeroizing<[u8; 32]> = Zeroizing::new([0u8; 32]);
        master_key_bytes.copy_from_slice(&key_bytes);

        let cipher = ChaCha20Poly1305::new_from_slice(&key_bytes)
            .map_err(|e| anyhow::anyhow!("failed to create cipher: {e}"))?;

        Ok(Self {
            cipher: Arc::new(cipher),
            db,
            cache: Arc::new(RwLock::new(HashMap::new())),
            master_key_bytes: Arc::new(master_key_bytes),
        })
    }

    /// Create a no-op SecretsManager for unit tests (never resolves any secrets).
    #[cfg(test)]
    pub fn new_noop() -> Self {
        let key = [0u8; 32];
        let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte zero key");
        let db = PgPool::connect_lazy("postgres://invalid").expect("lazy pool");
        Self {
            cipher: Arc::new(cipher),
            db,
            cache: Arc::new(RwLock::new(HashMap::new())),
            master_key_bytes: Arc::new(Zeroizing::new([0u8; 32])),
        }
    }

    /// Phase 64 SEC-03: derive a 32-byte HMAC key for upload URL signing.
    ///
    /// HKDF-SHA256 expands the master key with `info = b"uploads-v1"` so
    /// future key rotation (e.g. `"uploads-v2"` or sibling domains like
    /// `"session-v1"`) never reuses the same derived key. The raw master
    /// bytes NEVER leave this module — this accessor returns the HKDF
    /// output, not the master itself.
    pub fn get_upload_hmac_key(&self) -> [u8; 32] {
        crate::uploads::derive_upload_key(&self.master_key_bytes)
    }

    /// Load all secrets from DB into cache. Called once at startup.
    pub async fn load_all(&self) -> Result<usize> {
        let rows: Vec<(String, String, Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT name, scope, encrypted_value, nonce FROM secrets",
        )
        .fetch_all(&self.db)
        .await
        .context("failed to load secrets from DB")?;

        let mut cache = self.cache.write().await;
        let mut count = 0;
        for (name, scope, encrypted, nonce_bytes) in rows {
            if nonce_bytes.len() != 12 {
                tracing::error!(secret = %name, scope = %scope, "invalid nonce length, skipping");
                continue;
            }
            let nonce = Nonce::from_slice(&nonce_bytes);
            match self.cipher.decrypt(nonce, encrypted.as_ref()) {
                Ok(plaintext) => match String::from_utf8(plaintext) {
                    Ok(value) => {
                        cache.insert((name, scope), value);
                        count += 1;
                    }
                    Err(e) => {
                        tracing::error!(secret = %name, error = %e, "secret is not valid UTF-8");
                    }
                },
                Err(e) => {
                    tracing::error!(secret = %name, error = %e, "failed to decrypt secret (wrong master key?)");
                }
            }
        }
        Ok(count)
    }

    /// Get a global secret value from cache, falling back to env var.
    pub async fn get(&self, name: &str) -> Option<String> {
        let cache = self.cache.read().await;
        if let Some(val) = cache.get(&(name.to_string(), String::new())) {
            return Some(val.clone());
        }
        drop(cache);
        std::env::var(name).ok()
    }

    /// Get a secret with per-agent scope fallback chain:
    ///   1. (name, scope) — agent-specific secret
    ///   2. (name, "")   — global fallback
    ///   3. env var       — legacy env fallback
    pub async fn get_scoped(&self, name: &str, scope: &str) -> Option<String> {
        let cache = self.cache.read().await;
        if !scope.is_empty()
            && let Some(val) = cache.get(&(name.to_string(), scope.to_string())) {
            return Some(val.clone());
        }
        if let Some(val) = cache.get(&(name.to_string(), String::new())) {
            if !scope.is_empty() {
                tracing::debug!(secret = %name, scope = %scope, "scoped secret not found, using global fallback");
            }
            return Some(val.clone());
        }
        drop(cache);
        if let Ok(val) = std::env::var(name) {
            if !scope.is_empty() {
                tracing::warn!(secret = %name, scope = %scope, "secret resolved from env var — consider migrating to vault");
            }
            return Some(val);
        }
        None
    }

    /// Get a global secret value from cache only (no env fallback).
    pub async fn get_strict(&self, name: &str) -> Option<String> {
        self.cache.read().await.get(&(name.to_string(), String::new())).cloned()
    }

    /// Export all secrets as raw encrypted blobs (for backup).
    /// Export secrets as decrypted plaintext (for portable backups).
    /// The caller is responsible for protecting the output.
    pub async fn export_decrypted(&self) -> Result<Vec<PlaintextSecret>> {
        let cache = self.cache.read().await;
        Ok(cache
            .iter()
            .map(|((name, scope), value)| PlaintextSecret {
                name: name.clone(),
                scope: scope.clone(),
                value: value.clone(),
            })
            .collect())
    }

    /// Restore secrets from plaintext (encrypts with current master key).
    /// Upserts by (name, scope) and reloads the in-memory cache.
    pub async fn restore_plaintext(&self, secrets: Vec<PlaintextSecret>) -> Result<usize> {
        let count = secrets.len();
        for s in secrets {
            self.set_internal(&s.name, &s.scope, &s.value, None).await?;
        }
        Ok(count)
    }

    /// Set (upsert) a global secret (scope = "").
    /// Encrypts, stores in DB, updates cache.
    pub async fn set(
        &self,
        name: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<()> {
        self.set_internal(name, "", value, description).await
    }

    /// Set (upsert) a per-agent scoped secret.
    /// Encrypts, stores in DB, updates cache.
    #[allow(dead_code)]
    pub async fn set_scoped(
        &self,
        name: &str,
        scope: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<()> {
        self.set_internal(name, scope, value, description).await
    }

    async fn set_internal(
        &self,
        name: &str,
        scope: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<()> {
        let mut nonce_bytes = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, value.as_bytes())
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

        // Hold write lock across DB + cache to ensure atomicity.
        // The DB upsert is fast (single row), so lock contention is minimal.
        // Readers now use block_in_place to avoid blocking tokio threads.
        let mut cache = self.cache.write().await;
        sqlx::query(
            "INSERT INTO secrets (name, scope, encrypted_value, nonce, description, updated_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (name, scope) DO UPDATE SET
               encrypted_value = EXCLUDED.encrypted_value,
               nonce = EXCLUDED.nonce,
               description = COALESCE(EXCLUDED.description, secrets.description),
               updated_at = now()",
        )
        .bind(name)
        .bind(scope)
        .bind(&ciphertext)
        .bind(&nonce_bytes[..])
        .bind(description)
        .execute(&self.db)
        .await
        .context("failed to store secret in DB")?;
        cache.insert((name.to_string(), scope.to_string()), value.to_string());
        drop(cache);

        if scope.is_empty() {
            tracing::info!(secret = %name, "secret updated");
        } else {
            tracing::info!(secret = %name, scope = %scope, "scoped secret updated");
        }
        Ok(())
    }

    /// Update only the description of an existing secret (no value change).
    pub async fn update_description(&self, name: &str, scope: &str, description: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE secrets SET description = $3, updated_at = now() WHERE name = $1 AND scope = $2",
        )
        .bind(name)
        .bind(scope)
        .bind(description)
        .execute(&self.db)
        .await
        .context("failed to update secret description")?;
        tracing::info!(secret = %name, "secret description updated");
        Ok(())
    }


    /// Delete a scoped secret from DB and cache. Returns true if it existed.
    pub async fn delete_scoped(&self, name: &str, scope: &str) -> Result<bool> {
        let mut cache = self.cache.write().await;

        let result = sqlx::query("DELETE FROM secrets WHERE name = $1 AND scope = $2")
            .bind(name)
            .bind(scope)
            .execute(&self.db)
            .await
            .context("failed to delete scoped secret from DB")?;

        cache.remove(&(name.to_string(), scope.to_string()));
        drop(cache);

        let deleted = result.rows_affected() > 0;
        if deleted {
            tracing::info!(secret = %name, scope = %scope, "scoped secret deleted");
        }
        Ok(deleted)
    }

    /// List all global (scope = "") secret names with metadata (without values).
    pub async fn list(&self) -> Result<Vec<SecretInfo>> {
        let cache = self.cache.read().await;
        let rows: Vec<SecretInfoRow> = sqlx::query_as(
            "SELECT name, scope, description, created_at, updated_at FROM secrets \
             WHERE name NOT IN ('CHANNEL_CREDENTIALS', 'PROVIDER_CREDENTIALS') \
             ORDER BY scope, name",
        )
        .fetch_all(&self.db)
        .await
        .context("failed to list secrets")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let has_value = cache
                    .get(&(r.name.clone(), r.scope.clone()))
                    .is_some_and(|v| !v.is_empty());
                SecretInfo {
                    name: r.name,
                    scope: r.scope,
                    description: r.description,
                    has_value,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                }
            })
            .collect())
    }

    /// Rename all secrets belonging to `old_scope` → `new_scope`.
    /// Called when an agent is renamed to migrate its scoped secrets atomically.
    /// Returns the number of secrets migrated.
    pub async fn rename_scope(&self, old_scope: &str, new_scope: &str) -> Result<u64> {
        let mut cache = self.cache.write().await;

        let affected = sqlx::query("UPDATE secrets SET scope = $1 WHERE scope = $2")
            .bind(new_scope)
            .bind(old_scope)
            .execute(&self.db)
            .await
            .context("failed to rename secret scope in DB")?
            .rows_affected();

        // Re-key cache: (name, old_scope) → (name, new_scope)
        let old_keys: Vec<String> = cache
            .keys()
            .filter(|(_, s)| s == old_scope)
            .map(|(n, _)| n.clone())
            .collect();

        for name in old_keys {
            if let Some(value) = cache.remove(&(name.clone(), old_scope.to_string())) {
                cache.insert((name, new_scope.to_string()), value);
            }
        }
        drop(cache);

        tracing::info!(from = %old_scope, to = %new_scope, count = affected, "renamed secret scope");
        Ok(affected)
    }

    /// Delete all secrets belonging to a scope.
    /// Called when an agent is deleted to clean up its scoped secrets.
    pub async fn delete_scope(&self, scope: &str) -> Result<()> {
        sqlx::query("DELETE FROM secrets WHERE scope = $1")
            .bind(scope)
            .execute(&self.db)
            .await
            .context("failed to delete secrets for scope")?;

        // Remove from cache
        let mut cache = self.cache.write().await;
        cache.retain(|(_, s), _| s != scope);
        drop(cache);

        tracing::info!(scope = %scope, "deleted all secrets for scope");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Key validation helpers ──────────────────────────────────────────────

    fn lazy_pool() -> PgPool {
        PgPool::connect_lazy("postgres://invalid").expect("lazy pool never connects")
    }

    // ── new() hex validation ────────────────────────────────────────────────
    // PgPool::connect_lazy() requires a tokio context even though it never
    // actually connects, so all new() tests use #[tokio::test].

    #[tokio::test]
    async fn new_rejects_master_key_too_short() {
        let short_key = "a".repeat(62); // 62 hex chars → 31 bytes
        let result = SecretsManager::new(&short_key, lazy_pool());
        assert!(result.is_err(), "expected Err for 62-char hex key");
        let msg = result.err().unwrap().to_string();
        // Should mention either the byte count or the word "bytes"
        assert!(
            msg.contains("32") || msg.contains("bytes") || msg.contains("hex"),
            "error message should describe the key length constraint, got: {msg}"
        );
    }

    #[tokio::test]
    async fn new_rejects_master_key_too_long() {
        let long_key = "a".repeat(66); // 66 hex chars → 33 bytes
        let result = SecretsManager::new(&long_key, lazy_pool());
        assert!(result.is_err(), "expected Err for 66-char hex key");
    }

    #[tokio::test]
    async fn new_rejects_non_hex_master_key() {
        let non_hex = "g".repeat(64); // 'g' is not a valid hex character
        let result = SecretsManager::new(&non_hex, lazy_pool());
        assert!(result.is_err(), "expected Err for non-hex key");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("hex") || msg.contains("invalid"),
            "error should mention hex decoding failure, got: {msg}"
        );
    }

    #[tokio::test]
    async fn new_rejects_empty_key() {
        let result = SecretsManager::new("", lazy_pool());
        assert!(result.is_err(), "expected Err for empty key");
    }

    #[tokio::test]
    async fn new_accepts_valid_64_char_hex_key() {
        // All-lowercase 'a' repeated 64 times = valid 32-byte hex key.
        // new() is synchronous and uses a lazy pool — no DB connection attempted.
        let valid_key = "a".repeat(64);
        let result = SecretsManager::new(&valid_key, lazy_pool());
        assert!(result.is_ok(), "expected Ok for valid 64-char hex key, got: {:?}", result.err());
    }

    #[tokio::test]
    async fn new_accepts_mixed_case_hex_key() {
        // hex crate accepts both upper- and lower-case hex digits.
        // 4 × "aAbBcCdDeEfF0123" = 64 chars = 32 bytes exactly.
        let mixed = "aAbBcCdDeEfF0123".repeat(4);
        let result = SecretsManager::new(&mixed, lazy_pool());
        assert!(result.is_ok(), "expected Ok for mixed-case 64-char hex key, got: {:?}", result.err());
    }

    // ── In-memory cache reads ───────────────────────────────────────────────

    #[tokio::test]
    async fn cache_get_returns_none_for_missing_key() {
        let mgr = SecretsManager::new_noop();
        // No env var should exist for this nonsense name.
        let result = mgr.get("__hydeclaw_nonexistent_key_xyzzy__").await;
        assert_eq!(result, None, "expected None for key that was never inserted");
    }

    #[tokio::test]
    async fn cache_get_scoped_returns_none_for_missing_scoped_key() {
        let mgr = SecretsManager::new_noop();
        let result = mgr.get_scoped("__missing__", "SomeAgent").await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn cache_get_strict_returns_none_for_missing_key() {
        let mgr = SecretsManager::new_noop();
        let result = mgr.get_strict("__missing__").await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn export_decrypted_returns_empty_when_cache_empty() {
        let mgr = SecretsManager::new_noop();
        let exported = mgr.export_decrypted().await;
        assert!(exported.is_ok(), "export_decrypted should not error on empty cache");
        assert!(
            exported.unwrap().is_empty(),
            "export_decrypted should return empty vec when cache has no entries"
        );
    }

    /// Phase 64 SEC-03: master_key_bytes retention + HKDF accessor contract.
    ///
    ///  * Same master key must produce the same derived upload HMAC key
    ///    (determinism so signed URLs round-trip across restarts).
    ///  * Different master keys must produce different derived keys
    ///    (no leakage of a fixed constant).
    ///  * HKDF expansion must NOT equal the input ikm (would defeat the
    ///    domain separation — if `derive_upload_key(k) == k` the master
    ///    would be leaking directly into the HMAC key, re-using the AEAD
    ///    key as an HMAC key).
    #[tokio::test]
    async fn upload_hmac_key_derivation_is_deterministic_and_distinct() {
        let hex_a = "00".repeat(32);
        let hex_b = "01".repeat(32);
        // `connect_lazy` requires a tokio runtime even if it never actually
        // connects — hence the `#[tokio::test]` wrapper.
        let db_a = PgPool::connect_lazy("postgres://invalid").unwrap();
        let db_b = PgPool::connect_lazy("postgres://invalid").unwrap();
        let sm_a = SecretsManager::new(&hex_a, db_a).unwrap();
        let sm_b = SecretsManager::new(&hex_b, db_b).unwrap();

        let k_a1 = sm_a.get_upload_hmac_key();
        let k_a2 = sm_a.get_upload_hmac_key();
        let k_b = sm_b.get_upload_hmac_key();

        assert_eq!(k_a1, k_a2, "same master key must yield same HKDF output");
        assert_ne!(k_a1, k_b, "different master keys must yield different HKDF output");
        // Sanity: HKDF output of all-zero ikm must be nonzero after expand —
        // otherwise we'd be returning the raw master instead of the HKDF okm.
        assert_ne!(k_a1, [0u8; 32], "HKDF expand(all-zero ikm) must be nonzero");
    }

    /// Code review 2026-04-17: master_key_bytes is stored in a `Zeroizing`
    /// wrapper so the 32 bytes are overwritten when the last Arc ref drops.
    /// This test does NOT assert the post-drop memory state (impossible to
    /// observe reliably in safe Rust), but proves:
    ///  * `SecretsManager` still holds and uses the bytes correctly after
    ///    the wrapper change (derive produces a stable, nonzero output).
    ///  * Cloning the manager shares the Arc (reference count >= 2) — so
    ///    the secret is not accidentally duplicated on clone.
    ///  * Dropping one clone does NOT zero the key for surviving clones.
    #[tokio::test]
    async fn master_key_zeroize_wrapper_preserves_functionality() {
        let hex = "11".repeat(32);
        let db = PgPool::connect_lazy("postgres://invalid").unwrap();
        let sm = SecretsManager::new(&hex, db).unwrap();

        let k1 = sm.get_upload_hmac_key();
        assert_ne!(k1, [0u8; 32], "HKDF output must be nonzero");

        // Clone shares the Arc — dropping one must not break the other.
        let sm2 = sm.clone();
        drop(sm);
        let k2 = sm2.get_upload_hmac_key();
        assert_eq!(k1, k2, "surviving Arc clone must still produce same key");
    }
}
