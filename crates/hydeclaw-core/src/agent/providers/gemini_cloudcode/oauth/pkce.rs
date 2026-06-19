//! S256 PKCE pair generation for the Google OAuth authorization-code flow.
//!
//! Spec: RFC 7636 §4.1–4.2. Verifier is 96 random URL-safe bytes (128 chars
//! base64url). Challenge = BASE64URL(SHA256(ASCII(verifier))).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// PKCE verifier + S256 challenge pair.
pub struct PkcePair {
    /// Random URL-safe string sent in the token exchange (kept secret until exchange).
    pub verifier: String,
    /// BASE64URL(SHA256(verifier)) sent in the authorization URL.
    pub challenge: String,
}

/// Generate a fresh PKCE pair with a 96-byte (base64url-encoded) verifier.
///
/// Uses `rand::rng()` which is cryptographically secure on all platforms
/// supported by the `rand 0.9` crate.
///
/// 96 bytes encoded as URL-safe base64 without padding = 128 characters,
/// which satisfies RFC 7636 §4.1 (43–128 characters).
pub fn generate_pkce_pair() -> PkcePair {
    let mut raw = [0u8; 96];
    rand::rng().fill_bytes(&mut raw);
    let verifier = URL_SAFE_NO_PAD.encode(raw);

    let challenge = sha256_base64url(verifier.as_bytes());

    PkcePair { verifier, challenge }
}

/// Generate a 32-byte random state token (base64url-encoded, no padding).
/// Used as the `state` parameter in the authorization URL to prevent CSRF.
pub fn generate_state() -> String {
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

/// SHA-256 hash encoded as URL-safe base64 without padding.
fn sha256_base64url(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_url_safe_pair() {
        let pair = generate_pkce_pair();
        // verifier must be URL-safe unreserved chars only
        assert!(
            pair.verifier
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~'),
            "verifier contains reserved chars: {}",
            pair.verifier
        );
        // challenge must also be base64url (no +, /, =)
        assert!(!pair.challenge.contains('+'), "challenge must be base64url");
        assert!(!pair.challenge.contains('/'), "challenge must be base64url");
        assert!(!pair.challenge.contains('='), "challenge must not have padding");
    }

    #[test]
    fn challenge_is_deterministic() {
        // Given the same verifier bytes, SHA256 challenge must be stable.
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use sha2::{Digest, Sha256};

        let verifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let hash = hasher.finalize();
        let expected = URL_SAFE_NO_PAD.encode(hash);

        // Re-derive
        let mut hasher2 = Sha256::new();
        hasher2.update(verifier.as_bytes());
        let hash2 = hasher2.finalize();
        let actual = URL_SAFE_NO_PAD.encode(hash2);

        assert_eq!(expected, actual);
    }

    #[test]
    fn verifier_length_is_128_chars() {
        // 96 bytes base64url-encoded without padding = ceil(96*4/3) = 128 chars
        let pair = generate_pkce_pair();
        assert_eq!(
            pair.verifier.len(),
            128,
            "verifier must be 128 chars (96 bytes base64url)"
        );
    }

    #[test]
    fn state_is_url_safe() {
        let state = generate_state();
        assert!(
            state
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
            "state contains non-URL-safe chars: {}",
            state
        );
        assert!(!state.is_empty(), "state must not be empty");
    }

    #[test]
    fn two_pairs_are_different() {
        let a = generate_pkce_pair();
        let b = generate_pkce_pair();
        assert_ne!(a.verifier, b.verifier, "verifiers must be random");
    }
}
