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

/// TTL for upload URLs re-signed during the historical migration — 50 years.
pub const HISTORICAL_URL_TTL_SECS: u64 = 1_576_800_000;

/// TTL for per-job callback tokens minted by the file-handler worker.
/// Sized for long async jobs (6-hour+ video transcription + summarisation),
/// with generous headroom. Decoupled from `uploads.signed_url_ttl_secs` so
/// adjusting the upload URL cache window doesn't shorten callback validity.
pub const JOB_CALLBACK_TTL_SECS: u64 = 24 * 3600;

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

/// Mint a signed URL for an upload row: `{base}/api/uploads/{id}?sig=...&exp=...`.
///
/// HMAC namespace stays `"uploads"` (preserves the
/// `cross_namespace_forgery_rejected` test invariant). Signed payload bytes:
/// `"uploads:{id}:{exp_unix}"`. Internally reuses `mint_namespaced_url` and
/// rewrites the `/uploads/` path prefix to `/api/uploads/` so the read
/// endpoint is the id-based one. The HMAC payload is unchanged (URL path
/// rewriting is purely cosmetic; the signature is over `"uploads:{id}:{exp}"`).
pub fn mint_uploads_url(base: &str, id: uuid::Uuid, key: &[u8; 32], ttl_secs: u64) -> String {
    let id_str = id.to_string();
    // mint_namespaced_url produces "{base}/uploads/{id}?sig=...&exp=...".
    // Swap the path segment to "/api/uploads/" while keeping the same signed
    // payload format ("uploads:{id}:{exp}") so the HMAC namespace tag is
    // unchanged.
    let url = mint_namespaced_url(base, "uploads", &id_str, key, ttl_secs);
    url.replacen("/uploads/", "/api/uploads/", 1)
}

/// Base prefix for upload URLs that are rendered **in the same-origin web UI**
/// (chat `__file__:` markers, `*_ready` notifications, client-upload and
/// agent-icon responses). Always empty, so the minted URL is **root-relative**
/// (`/api/uploads/{id}?sig=…&exp=…`).
///
/// A root-relative URL resolves against whatever origin served the page, so it
/// works no matter how (or whether) `gateway.public_url` is configured. Using
/// the configured absolute base here was the root cause of a mixed-content /
/// `ERR_NAME_NOT_RESOLVED` failure: when `public_url` was left at its
/// placeholder (`http://your-server:18789`) on an HTTPS deployment, the browser
/// blocked the cross-origin insecure request and audio/image playback silently
/// failed. The HMAC signature is over `"uploads:{id}:{exp}"` (host-independent),
/// so dropping the host does not affect verification.
///
/// `public_url` is still required where an **absolute** URL is mandatory —
/// OAuth redirect URIs and CORS origins (see `oauth.rs`, `gateway/mod.rs`).
pub fn web_uploads_base() -> &'static str {
    ""
}

/// Verify a signed `/api/uploads/{id}` URL. Inputs: the id parsed from the
/// path, the signature and expiry from the query, and the same key used to
/// mint. Returns `Ok(())` on success.
///
/// Implemented by delegating to `verify_signed_url` with the id-as-string as
/// the filename token; the signed payload `"uploads:{id}:{exp}"` is identical
/// to what `mint_uploads_url` produces. `now_unix` is sourced from the system
/// clock here so the caller doesn't need to plumb it through (this matches
/// what production callers want; tests that need clock-injection should call
/// `verify_signed_url` directly).
pub fn verify_uploads_url(
    id: uuid::Uuid,
    sig_b64: &str,
    exp_unix: u64,
    key: &[u8; 32],
) -> Result<(), UploadSignatureError> {
    let q = SignedUploadQuery {
        sig: Some(sig_b64.to_string()),
        exp: Some(exp_unix),
    };
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    verify_signed_url(&id.to_string(), &q, key, now_unix)
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

// ── Per-job HMAC callback tokens ──────────────────────────────────────────────

/// Namespace for per-job callback tokens. Must differ from all upload namespaces
/// so a job token cannot be replayed on an upload endpoint (domain separation).
const JOB_CB_NS: &str = "jobcb";

/// Mint a per-job HMAC-SHA256 callback token bound to `job_id`.
///
/// Token format: `"{exp}.{hex-HMAC}"`.
/// HMAC payload: `"jobcb:{job_id}:{exp}"`.
///
/// The same 32-byte upload HMAC key is reused, but the `"jobcb:"` namespace
/// prefix domain-separates this token from all `/uploads/` and
/// `/workspace-files/` signatures — a job token cannot be replayed elsewhere.
///
/// This function is `pub` so the async-job worker (`file_handler_worker.rs`)
/// can call it when dispatching handler_jobs to toolgate.
pub fn mint_job_callback_token(key: &[u8; 32], job_id: uuid::Uuid, ttl_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    let exp = now + ttl_secs;
    let payload = format!("{JOB_CB_NS}:{job_id}:{exp}");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC-SHA256 accepts 32-byte key");
    mac.update(payload.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    format!("{exp}.{sig}")
}

/// Verify a per-job callback token. Returns `true` iff the token is structurally
/// valid, not expired (exp >= now), and the HMAC matches.
///
/// Constant-time comparison via `subtle::ConstantTimeEq`.
///
/// This function is `pub` so the gateway handler can call it from
/// `gateway/handlers/files.rs`.
pub fn verify_job_callback_token(key: &[u8; 32], job_id: uuid::Uuid, token: &str) -> bool {
    // Parse "{exp}.{hex_sig}".
    let Some((exp_str, hex_sig)) = token.split_once('.') else {
        return false;
    };
    let Ok(exp) = exp_str.parse::<u64>() else {
        return false;
    };
    let Ok(submitted) = hex::decode(hex_sig) else {
        return false;
    };

    // Reject expired tokens (exp < now).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    if now > exp {
        return false;
    }

    // Recompute HMAC and constant-time compare.
    let payload = format!("{JOB_CB_NS}:{job_id}:{exp}");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC-SHA256 accepts 32-byte key");
    mac.update(payload.as_bytes());
    let expected = mac.finalize().into_bytes();
    submitted.ct_eq(expected.as_slice()).into()
}

// ── Codemode capability tokens ────────────────────────────────────────────────

/// Namespace for codemode capability tokens. Domain-separated from job tokens
/// (`"jobcb"`) and upload/workspace-file signatures so a codemode token cannot
/// be replayed on any other endpoint.
const CODEMODE_NS: &str = "codemode";

/// Mint a per-execution HMAC-SHA256 capability token for codemode (tools-as-code).
///
/// Token format: `"{exp}.{hex-HMAC}"` (same shape as `mint_job_callback_token`).
/// HMAC payload: `"codemode:{session_id}:{agent_name}:{tools_hash}:{exp}"`.
///
/// The token is bound to:
/// - `session_id` — the session the script runs in (replay protection).
/// - `agent_name` — the agent that minted it (cross-agent replay protection).
/// - `tools_hash` — a hash of the sorted allowed-tools list, so a token minted
///   for one tool set cannot call tools outside that set even if the token is
///   stolen.
/// - `exp` — expiry (TTL scoped to one codemode run, typically sandbox.timeout * 3).
///
/// Reuses the upload HMAC key (HKDF-derived from `OPEX_MASTER_KEY`) — the
/// `"codemode:"` namespace prefix provides domain separation.
#[allow(dead_code)]
pub fn mint_codemode_token(
    key: &[u8; 32],
    session_id: uuid::Uuid,
    agent_name: &str,
    tools_hash: u64,
    ttl_secs: u64,
) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    let exp = now + ttl_secs;
    let payload = format!("{CODEMODE_NS}:{session_id}:{agent_name}:{tools_hash}:{exp}");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC-SHA256 accepts 32-byte key");
    mac.update(payload.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    format!("{exp}.{sig}")
}

/// Verify a codemode capability token. Returns `true` iff the token is
/// structurally valid, not expired, and the HMAC matches the given
/// `session_id` + `agent_name` + `tools_hash`.
///
/// Constant-time compare via `subtle::ConstantTimeEq`.
pub fn verify_codemode_token(
    key: &[u8; 32],
    session_id: uuid::Uuid,
    agent_name: &str,
    tools_hash: u64,
    token: &str,
) -> bool {
    let Some((exp_str, hex_sig)) = token.split_once('.') else {
        return false;
    };
    let Ok(exp) = exp_str.parse::<u64>() else {
        return false;
    };
    let Ok(submitted) = hex::decode(hex_sig) else {
        return false;
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    if now > exp {
        return false;
    }

    let payload = format!("{CODEMODE_NS}:{session_id}:{agent_name}:{tools_hash}:{exp}");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC-SHA256 accepts 32-byte key");
    mac.update(payload.as_bytes());
    let expected = mac.finalize().into_bytes();
    submitted.ct_eq(expected.as_slice()).into()
}

/// Compute a stable hash of the sorted allowed-tools list for codemode token
/// binding. Uses a simple FNV-1a hash (deterministic, no hash-randomization).
pub fn codemode_tools_hash(allowed_tools: &[String]) -> u64 {
    let mut sorted: Vec<&String> = allowed_tools.iter().collect();
    sorted.sort_unstable();
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a 64-bit offset basis
    for tool in &sorted {
        for &b in tool.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3); // FNV-1a 64-bit prime
        }
        h ^= 0x2f; // '/' separator between tool names
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Guess MIME type from filename extension (no external dep).
/// Used by handlers in the binary tree (`workspace_write/edit`, code-exec
/// sandbox, `/workspace-files/` endpoint). The lib facade doesn't expose
/// these handlers, so this fn appears dead in the lib target — allow it.
#[allow(dead_code)]
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

    #[test]
    fn historical_url_ttl_secs_is_50_years() {
        // 50 * 365 * 24 * 3600
        assert_eq!(HISTORICAL_URL_TTL_SECS, 1_576_800_000u64);
    }

    // ── Per-job callback token tests ─────────────────────────────────────────

    #[test]
    fn job_callback_token_roundtrip() {
        let key = [11u8; 32];
        let id = uuid::Uuid::new_v4();
        let token = mint_job_callback_token(&key, id, 300);
        assert!(verify_job_callback_token(&key, id, &token), "valid token must verify");
    }

    #[test]
    fn job_callback_token_tampered_sig_rejected() {
        let key = [22u8; 32];
        let id = uuid::Uuid::new_v4();
        let token = mint_job_callback_token(&key, id, 300);
        let (exp_part, _sig) = token.split_once('.').unwrap();
        let bogus = format!("{exp_part}.{}", "00".repeat(32));
        assert!(!verify_job_callback_token(&key, id, &bogus), "tampered sig must be rejected");
    }

    #[test]
    fn job_callback_token_expired_rejected() {
        let key = [33u8; 32];
        let id = uuid::Uuid::new_v4();
        // Mint with exp already in the past: ttl=0 means exp=now, and the
        // verify check is now > exp. We produce a token with exp = now - 1.
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(1);
        let payload = format!("{JOB_CB_NS}:{id}:{exp}");
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).unwrap();
        mac.update(payload.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        let token = format!("{exp}.{sig}");
        assert!(!verify_job_callback_token(&key, id, &token), "expired token must be rejected");
    }

    #[test]
    fn job_callback_token_wrong_job_id_rejected() {
        let key = [44u8; 32];
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let token = mint_job_callback_token(&key, id1, 300);
        assert!(!verify_job_callback_token(&key, id2, &token), "token for id1 must not verify for id2");
    }

    #[test]
    fn job_callback_token_cannot_forge_upload_sig() {
        // A job token format must not accidentally verify as an upload URL sig.
        // They share the same key but different namespaces ("jobcb:" vs "uploads:").
        let key = [55u8; 32];
        let id = uuid::Uuid::new_v4();
        let token = mint_job_callback_token(&key, id, 300);
        // Parse exp from token and attempt to use it as an upload sig.
        let (_exp_part, hex_sig) = token.split_once('.').unwrap();
        let b64_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(hex::decode(hex_sig).unwrap());
        let token_exp: u64 = token.split('.').next().unwrap().parse().unwrap();
        let result = verify_uploads_url(id, &b64_sig, token_exp, &key);
        assert!(result.is_err(), "job token must not verify as upload URL sig");
    }

    // ── Codemode capability token tests ────────────────────────────────────────

    #[test]
    fn codemode_token_roundtrip() {
        let key = [11u8; 32];
        let session = uuid::Uuid::new_v4();
        let agent = "base";
        let tools_hash = codemode_tools_hash(&["workspace_read".into(), "workspace_write".into()]);
        let token = mint_codemode_token(&key, session, agent, tools_hash, 300);
        assert!(
            verify_codemode_token(&key, session, agent, tools_hash, &token),
            "valid token must verify"
        );
    }

    #[test]
    fn codemode_token_tampered_sig_rejected() {
        let key = [22u8; 32];
        let session = uuid::Uuid::new_v4();
        let tools_hash = 12345;
        let token = mint_codemode_token(&key, session, "base", tools_hash, 300);
        let (exp_part, _sig) = token.split_once('.').unwrap();
        let bogus = format!("{exp_part}.{}", "00".repeat(32));
        assert!(
            !verify_codemode_token(&key, session, "base", tools_hash, &bogus),
            "tampered sig must be rejected"
        );
    }

    #[test]
    fn codemode_token_wrong_session_rejected() {
        let key = [33u8; 32];
        let s1 = uuid::Uuid::new_v4();
        let s2 = uuid::Uuid::new_v4();
        let tools_hash = 99;
        let token = mint_codemode_token(&key, s1, "base", tools_hash, 300);
        assert!(
            !verify_codemode_token(&key, s2, "base", tools_hash, &token),
            "token for session1 must not verify for session2"
        );
    }

    #[test]
    fn codemode_token_wrong_agent_rejected() {
        let key = [44u8; 32];
        let session = uuid::Uuid::new_v4();
        let tools_hash = 99;
        let token = mint_codemode_token(&key, session, "base", tools_hash, 300);
        assert!(
            !verify_codemode_token(&key, session, "other", tools_hash, &token),
            "token for 'base' must not verify for 'other'"
        );
    }

    #[test]
    fn codemode_token_wrong_tools_hash_rejected() {
        let key = [55u8; 32];
        let session = uuid::Uuid::new_v4();
        let token = mint_codemode_token(&key, session, "base", 100, 300);
        assert!(
            !verify_codemode_token(&key, session, "base", 200, &token),
            "token with tools_hash=100 must not verify for tools_hash=200"
        );
    }

    #[test]
    fn codemode_token_expired_rejected() {
        let key = [66u8; 32];
        let session = uuid::Uuid::new_v4();
        let tools_hash = 1;
        // Mint with exp in the past.
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(1);
        let payload = format!("{CODEMODE_NS}:{session}:base:{tools_hash}:{exp}");
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).unwrap();
        mac.update(payload.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        let token = format!("{exp}.{sig}");
        assert!(
            !verify_codemode_token(&key, session, "base", tools_hash, &token),
            "expired token must be rejected"
        );
    }

    #[test]
    fn codemode_token_cannot_forge_job_callback() {
        // A codemode token must not accidentally verify as a job callback token
        // (different namespaces: "codemode:" vs "jobcb:").
        let key = [77u8; 32];
        let session = uuid::Uuid::new_v4();
        let tools_hash = codemode_tools_hash(&["workspace_read".into()]);
        let token = mint_codemode_token(&key, session, "base", tools_hash, 300);
        assert!(
            !verify_job_callback_token(&key, session, &token),
            "codemode token must not verify as job callback token"
        );
    }

    #[test]
    fn codemode_tools_hash_is_order_independent() {
        let h1 = codemode_tools_hash(&["b".into(), "a".into(), "c".into()]);
        let h2 = codemode_tools_hash(&["c".into(), "a".into(), "b".into()]);
        assert_eq!(h1, h2, "tools hash must be order-independent");
    }

    #[test]
    fn codemode_tools_hash_different_lists_differ() {
        let h1 = codemode_tools_hash(&["workspace_read".into()]);
        let h2 = codemode_tools_hash(&["workspace_read".into(), "workspace_write".into()]);
        assert_ne!(h1, h2, "different tool lists must have different hashes");
    }

    #[test]
    fn codemode_tools_hash_excluding_one_tool_differs() {
        // Regression for C1-v2: mint/verify must use the SAME tool list. If
        // one side includes `code_orchestrate` and the other excludes it, the
        // hashes won't match and every tool call is rejected with 401.
        let full = vec![
            "workspace_read".to_string(),
            "workspace_write".to_string(),
            "code_orchestrate".to_string(),
        ];
        let filtered: Vec<String> = full
            .iter()
            .filter(|n| *n != "code_orchestrate")
            .cloned()
            .collect();
        let h_full = codemode_tools_hash(&full);
        let h_filtered = codemode_tools_hash(&filtered);
        assert_ne!(
            h_full, h_filtered,
            "including vs excluding code_orchestrate must produce different hashes"
        );
    }

    fn parse_url_qs(url: &str) -> (String, u64) {
        let qs = url.split('?').nth(1).unwrap();
        let mut sig = String::new();
        let mut exp = 0u64;
        for kv in qs.split('&') {
            let (k, v) = kv.split_once('=').unwrap();
            match k {
                "sig" => sig = v.to_string(),
                "exp" => exp = v.parse().unwrap(),
                _ => {}
            }
        }
        (sig, exp)
    }

    #[test]
    fn mint_and_verify_uploads_url_roundtrip() {
        let key = [42u8; 32];
        let id = uuid::Uuid::new_v4();
        let url = mint_uploads_url("http://h", id, &key, 60);
        assert!(url.starts_with(&format!("http://h/api/uploads/{id}?sig=")), "{url}");
        let (sig, exp) = parse_url_qs(&url);
        assert!(verify_uploads_url(id, &sig, exp, &key).is_ok());
    }

    #[test]
    fn web_uploads_base_yields_root_relative_verifiable_url() {
        // Regression guard for the mixed-content bug: media rendered in the web
        // UI must use a root-relative URL (no scheme/host) so it resolves
        // against the page origin regardless of gateway.public_url. The signed
        // payload is host-independent, so verification still succeeds.
        let key = [13u8; 32];
        let id = uuid::Uuid::new_v4();
        let url = mint_uploads_url(web_uploads_base(), id, &key, 60);
        assert!(url.starts_with("/api/uploads/"), "must be root-relative: {url}");
        assert!(!url.contains("://"), "must carry no scheme/host: {url}");
        let (sig, exp) = parse_url_qs(&url);
        assert!(verify_uploads_url(id, &sig, exp, &key).is_ok());
    }

    #[test]
    fn verify_uploads_url_rejects_tampered_id() {
        let key = [7u8; 32];
        let id = uuid::Uuid::new_v4();
        let url = mint_uploads_url("http://h", id, &key, 60);
        let (sig, exp) = parse_url_qs(&url);
        let other_id = uuid::Uuid::new_v4();
        assert!(verify_uploads_url(other_id, &sig, exp, &key).is_err());
    }

    #[test]
    fn verify_uploads_url_rejects_expired() {
        let key = [1u8; 32];
        let id = uuid::Uuid::new_v4();
        // Mint with ttl=1, sleep past expiry, then verify.
        let url = mint_uploads_url("http://h", id, &key, 1);
        let (sig, exp) = parse_url_qs(&url);
        std::thread::sleep(std::time::Duration::from_secs(2));
        let result = verify_uploads_url(id, &sig, exp, &key);
        assert!(result.is_err(), "expired URL must be rejected, got {result:?}");
    }

    #[test]
    fn uploads_url_namespace_cannot_forge_workspace_files() {
        // Mint with the uploads HMAC + try to verify against the workspace-files
        // namespace via verify_workspace_file_url. Must fail because the signed
        // payload starts with "uploads:" not "workspace-files:".
        let key = [9u8; 32];
        let id = uuid::Uuid::new_v4();
        let url = mint_uploads_url("http://h", id, &key, 60);
        let (sig, exp) = parse_url_qs(&url);
        let result = verify_workspace_file_url(&id.to_string(), &sig, exp, &key, now());
        assert!(result.is_err(), "cross-namespace forgery must be rejected, got {result:?}");
    }
}
