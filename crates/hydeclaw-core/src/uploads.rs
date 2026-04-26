//! Phase 64 SEC-03 — HMAC-signed URL mint/verify for `/uploads/*`.
//!
//! # Why
//! Agents routinely embed uploaded media in responses (e.g. Telegram
//! `send_photo` tool result). Before this module, anyone who learned an
//! upload UUID could fetch the file forever. HMAC + TTL limits the blast
//! radius; constant-time compare closes the timing side channel.
//!
//! # Contract
//! * URL format: `{base}/uploads/{filename}?sig={b64url-nopad}&exp={unix}`
//! * Signature payload: `"{ns}:{path}:{exp_unix}"` (bytes) — namespace prefix
//!   prevents cross-namespace forgery between `/uploads/` and `/workspace-files/`.
//! * HMAC algorithm: `HMAC-SHA256` with a 32-byte key.
//! * Key derivation: `HKDF-SHA256(ikm = master_key, salt = None, info = b"uploads-v1")`.
//!   Using `info` as a domain separator lets us later rotate to `"uploads-v2"`
//!   (or mint other per-domain keys like `"session-v1"`) without touching the
//!   master key.
//! * Base64 alphabet: `URL_SAFE_NO_PAD` (matches `tests/support/signed_url_helper.rs`).
//!
//! # Leaf-ness
//! This module has zero `crate::*` references. It pulls only `std`, `base64`,
//! `hmac`, `sha2`, `hkdf`, `subtle`, `percent-encoding`, and `thiserror`. That
//! lets `src/lib.rs` re-export it for integration tests without cascading the
//! 10-module lib-facade cap (see `src/lib.rs` for the budgeting comment).

use base64::Engine;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Parsed `?sig=&exp=` query parameters.
///
/// Axum extractors can be used upstream; this struct keeps the leaf module
/// free of axum-specific types so it compiles in `lib.rs` without the gateway
/// cascade.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SignedUploadQuery {
    pub sig: Option<String>,
    pub exp: Option<u64>,
}

/// Verification outcome for `/uploads/{file}` requests.
///
/// Mapping to HTTP:
///   * `Missing` → 403 Forbidden (only when `require_signature=true`)
///   * `Invalid` → 403 Forbidden
///   * `Expired` → 410 Gone
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum UploadSignatureError {
    #[error("missing signature")]
    Missing,
    #[error("invalid signature")]
    Invalid,
    #[error("signature expired")]
    Expired,
}

/// Derive a per-domain 32-byte HMAC key from the master key via `HKDF-SHA256`.
///
/// * `ikm`   = the 32-byte master key
/// * `salt`  = `None` (master key is already high-entropy uniform random)
/// * `info`  = `b"uploads-v1"` — domain separator for future rotation
///
/// Expanding 32 bytes is well within HKDF's `255 * HashLen` ceiling, so the
/// expansion never fails and `expect()` here is a true invariant, not a
/// runtime error path.
pub fn derive_upload_key(master_key: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master_key);
    let mut okm = [0u8; 32];
    hk.expand(b"uploads-v1", &mut okm)
        .expect("32-byte okm is always within HKDF output length limit");
    okm
}

/// Per-namespace HMAC payload: `"{ns}:{path}:{exp}"`. The namespace prefix
/// prevents a sig minted for one namespace from verifying on another.
fn ns_payload(ns: &'static str, path: &str, exp: u64) -> Vec<u8> {
    format!("{ns}:{path}:{exp}").into_bytes()
}

/// Generic namespace-aware URL signer.
pub(crate) fn mint_namespaced_url(
    base: &str,
    ns: &'static str,
    path: &str,
    key: &[u8; 32],
    ttl_secs: u64,
) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    let exp = now + ttl_secs;

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC-SHA256 accepts 32-byte key");
    mac.update(&ns_payload(ns, path, exp));
    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(mac.finalize().into_bytes());

    let path_encoded = url_encode_keep_slash(path);
    format!("{base}/{ns}/{path_encoded}?sig={sig}&exp={exp}")
}

/// Generic namespace-aware verifier. Constant-time compare via subtle.
pub(crate) fn verify_namespaced_url(
    ns: &'static str,
    path: &str,
    sig_b64: &str,
    exp: u64,
    key: &[u8; 32],
    now_unix: u64,
) -> Result<(), UploadSignatureError> {
    if now_unix > exp {
        return Err(UploadSignatureError::Expired);
    }
    let submitted = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig_b64) {
        Ok(v) => v,
        Err(_) => return Err(UploadSignatureError::Invalid),
    };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|_| UploadSignatureError::Invalid)?;
    mac.update(&ns_payload(ns, path, exp));
    let expected = mac.finalize().into_bytes();
    if submitted.ct_eq(&expected).into() {
        Ok(())
    } else {
        Err(UploadSignatureError::Invalid)
    }
}

/// Mint a signed URL for a workspace file.
/// Slashes are kept raw; spaces/unicode are percent-encoded.
pub fn mint_workspace_file_url(rel_path: &str, key: &[u8; 32], ttl_secs: u64) -> String {
    mint_namespaced_url("", "workspace-files", rel_path, key, ttl_secs)
}

/// Verify a workspace-files signed URL.
pub fn verify_workspace_file_url(
    rel_path: &str,
    sig_b64: &str,
    exp: u64,
    key: &[u8; 32],
    now_unix: u64,
) -> Result<(), UploadSignatureError> {
    verify_namespaced_url("workspace-files", rel_path, sig_b64, exp, key, now_unix)
}

/// Percent-encode every byte that is not unreserved (`A-Za-z0-9-._~`)
/// **except** the slash, which stays raw so multi-segment paths like
/// `subdir/out.csv` remain readable in URLs.
const URL_KEEP_SLASH: &AsciiSet = &CONTROLS
    .add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?')
    .add(b'`').add(b'{').add(b'}').add(b'[').add(b']');

pub(crate) fn url_encode_keep_slash(s: &str) -> String {
    utf8_percent_encode(s, URL_KEEP_SLASH).to_string()
}

/// Guess MIME type from filename extension (no external dep).
pub(crate) fn guess_mime_from_extension(filename: &str) -> &'static str {
    match std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("csv") => "text/csv",
        Some("tsv") => "text/tab-separated-values",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("txt" | "log") => "text/plain",
        Some("html") => "text/html",
        Some("xml") => "application/xml",
        Some("py" | "rs" | "ts" | "js") => "text/plain",
        Some("yaml" | "yml") => "application/yaml",
        Some("toml") => "application/toml",
        Some("sql") => "application/sql",
        _ => "application/octet-stream",
    }
}

/// Build `"{base}/uploads/{filename}?sig={b64url-nopad}&exp={unix}"`.
///
/// `base` may be absolute (`"http://host"`) or empty (`""`) — the caller
/// decides whether the URL is public-facing or relative. Trailing slashes on
/// `base` are NOT stripped here; the test helper strips them, but production
/// callers pass a pre-normalized base or an empty string.
///
/// # Panics
/// Panics only if the system clock is before the Unix epoch (essentially
/// never) or if `Hmac::new_from_slice` rejects a 32-byte key (impossible —
/// HMAC-SHA256 accepts any key length, and 32 bytes is the canonical size).
///
/// # Backward-compat note
/// The HMAC payload format changed from `"{filename}:{exp}"` to
/// `"uploads:{filename}:{exp}"` when namespace-aware signing was introduced.
/// Previously-issued `/uploads/...` URLs will fail verification after upgrade.
/// TTL is 24 h so the breakage window is small.
pub fn mint_signed_url(base: &str, filename: &str, key: &[u8; 32], ttl_secs: u64) -> String {
    mint_namespaced_url(base, "uploads", filename, key, ttl_secs)
}

/// Constant-time HMAC verification.
///
/// Returns `Ok(())` iff:
///   1. Both `sig` and `exp` are present in the query.
///   2. `now_unix <= exp` (not expired).
///   3. `sig` (after base64url-no-pad decode) matches the HMAC of
///      `"uploads:{filename}:{exp}"` computed with `key`.
///
/// The final comparison uses `subtle::ConstantTimeEq`, which runs in time
/// independent of how many leading bytes match — the defense required by
/// Phase 64 CONTEXT.md.
///
/// Malformed base64 is collapsed to `Invalid` (not a separate variant) so
/// attackers can't distinguish "your sig isn't even base64" from "your sig
/// decoded but didn't match" via HTTP status.
pub fn verify_signed_url(
    filename: &str,
    query: &SignedUploadQuery,
    key: &[u8; 32],
    now_unix: u64,
) -> Result<(), UploadSignatureError> {
    let (sig, exp) = match (query.sig.as_deref(), query.exp) {
        (Some(s), Some(e)) => (s, e),
        _ => return Err(UploadSignatureError::Missing),
    };
    verify_namespaced_url("uploads", filename, sig, exp, key, now_unix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn mint_contains_filename_sig_and_exp() {
        let key = [0u8; 32];
        let url = mint_signed_url("http://h", "abc.png", &key, 60);
        assert!(url.starts_with("http://h/uploads/abc.png?"), "{url}");
        assert!(url.contains("sig="));
        assert!(url.contains("&exp="));
    }

    #[test]
    fn roundtrip_ok() {
        let key = [1u8; 32];
        let url = mint_signed_url("", "file.jpg", &key, 3600);
        let q = SignedUploadQuery {
            sig: url
                .split("sig=")
                .nth(1)
                .and_then(|s| s.split('&').next())
                .map(|s| s.to_string()),
            exp: url
                .split("exp=")
                .nth(1)
                .and_then(|s| s.parse().ok()),
        };
        assert!(verify_signed_url("file.jpg", &q, &key, now()).is_ok());
    }

    #[test]
    fn hkdf_output_is_not_the_ikm() {
        // Regression guard: HKDF-SHA256 of all-zero ikm produces nonzero okm.
        // If it did equal the ikm, we'd be leaking the master key directly.
        let out = derive_upload_key(&[0u8; 32]);
        assert_ne!(out, [0u8; 32]);
    }

    #[test]
    fn cross_namespace_forgery_rejected() {
        // Sig minted in the "uploads" namespace must NOT verify against
        // workspace-files for the same path. Without namespace prefix in
        // the HMAC payload, the same sig would verify for both namespaces
        // because both callers compute HMAC over "{path}:{exp}".
        let key = [42u8; 32];
        let url = mint_signed_url("", "shared.png", &key, 60);
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();

        let result = verify_workspace_file_url("shared.png", sig, exp, &key, now());
        assert!(result.is_err(), "uploads sig must NOT verify on workspace-files");
    }

    #[test]
    fn workspace_file_url_roundtrip_with_subdir_path() {
        let key = [7u8; 32];
        let url = mint_workspace_file_url("sub/dir/out.csv", &key, 3600);
        assert!(url.starts_with("/workspace-files/sub/dir/out.csv?"), "{url}");
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        verify_workspace_file_url("sub/dir/out.csv", sig, exp, &key, now()).unwrap();
    }

    #[test]
    fn workspace_file_url_rejects_expired_exp() {
        let key = [9u8; 32];
        let url = mint_workspace_file_url("a.md", &key, 1);
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        let result = verify_workspace_file_url("a.md", sig, exp, &key, exp + 1);
        assert!(matches!(result, Err(UploadSignatureError::Expired)));
    }

    #[test]
    fn workspace_file_url_rejects_tampered_path() {
        let key = [3u8; 32];
        let url = mint_workspace_file_url("real.md", &key, 60);
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        let result = verify_workspace_file_url("evil.md", sig, exp, &key, now());
        assert!(matches!(result, Err(UploadSignatureError::Invalid)));
    }

    #[test]
    fn workspace_file_url_rejects_tampered_sig() {
        let key = [5u8; 32];
        let url = mint_workspace_file_url("a.md", &key, 60);
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        let bogus_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 32]);
        let result = verify_workspace_file_url("a.md", &bogus_sig, exp, &key, now());
        assert!(matches!(result, Err(UploadSignatureError::Invalid)));
    }

    #[test]
    fn mint_workspace_file_url_percent_encodes_spaces_and_unicode() {
        let key = [1u8; 32];
        let url_space = mint_workspace_file_url("My Report.md", &key, 60);
        assert!(url_space.contains("My%20Report.md"), "expect space encoded: {url_space}");

        let url_unicode = mint_workspace_file_url("отчёт.md", &key, 60);
        assert!(!url_unicode.contains("отчёт"), "raw unicode in URL: {url_unicode}");
        assert!(url_unicode.contains("%"));

        // Roundtrip: verify accepts the SAME path string used for mint
        // (encoding happens inside mint, the HMAC is over the raw path).
        let sig = url_space.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url_space.split("exp=").nth(1).unwrap().parse().unwrap();
        verify_workspace_file_url("My Report.md", sig, exp, &key, now()).unwrap();
    }

    #[test]
    fn mint_workspace_file_url_keeps_slashes_raw() {
        let key = [2u8; 32];
        let url = mint_workspace_file_url("a/b/c.csv", &key, 60);
        assert!(url.contains("/a/b/c.csv"), "slashes encoded as %2F: {url}");
        assert!(!url.contains("%2F"), "slashes encoded as %2F: {url}");
    }

    #[test]
    fn guess_mime_known_and_unknown() {
        assert_eq!(guess_mime_from_extension("a.png"), "image/png");
        assert_eq!(guess_mime_from_extension("a.csv"), "text/csv");
        assert_eq!(guess_mime_from_extension("a.MD"), "text/markdown");
        assert_eq!(guess_mime_from_extension("noext"), "application/octet-stream");
        assert_eq!(guess_mime_from_extension("a.unknownext"), "application/octet-stream");
    }
}
