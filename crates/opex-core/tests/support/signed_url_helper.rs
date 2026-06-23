//! Signed-URL mint/verify helper for Phase 64 SEC-03 upload-signature tests.
//!
//! Mints URLs of the shape `/uploads/{uuid}.{ext}?sig=<b64url>&exp=<unix>`
//! so Wave 1 Plan 04 can:
//!   * assert verifier accepts freshly minted URLs,
//!   * assert verifier rejects expired / tampered signatures,
//!   * reuse a single HMAC contract (payload = `"{filename}:{exp_unix}"`)
//!     matching the production verifier.
//!
//! NOTE on encoding: the signature is encoded with **base64url (no padding)**
//! — `URL_SAFE_NO_PAD` — so the string is safe to drop directly into a query
//! parameter without further percent-encoding. The production verifier MUST
//! decode with the same alphabet (`URL_SAFE_NO_PAD`); standard base64 will
//! fail on `+` / `/` / `=` handling.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Parsed components of a signed URL, extracted by `parse_signed_url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignedUrl {
    /// `{uuid}.{ext}` portion of the path (no leading slash, no query).
    pub filename: String,
    /// Raw base64url-encoded signature as it appeared in the URL.
    pub sig_b64: String,
    /// Unix-epoch seconds at which the URL expires.
    pub exp_unix: u64,
}

/// Mint a signed URL.
///
/// HMAC payload is `"{filename}:{exp_unix}"` where `filename = "{uuid}.{ext}"`.
/// Key is a 32-byte slice (Plan 04 derives it via HKDF-SHA256 from
/// `HYDECLAW_MASTER_KEY`; tests pass a deterministic array for reproducibility).
pub fn mint_signed_url(
    base: &str,
    uuid: &str,
    ext: &str,
    key: &[u8; 32],
    ttl_secs: u64,
) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs();
    let exp = now + ttl_secs;
    let filename = format!("{uuid}.{ext}");
    let payload = format!("{filename}:{exp}");

    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts 32-byte key");
    mac.update(payload.as_bytes());
    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    let base = base.trim_end_matches('/');
    format!("{base}/uploads/{filename}?sig={sig}&exp={exp}")
}

/// Parse `filename`, `sig_b64`, and `exp_unix` out of a URL produced by
/// `mint_signed_url`. Returns a String error describing the first problem.
///
/// Forgiving: accepts absolute or path-only URLs, as long as they contain
/// `/uploads/…?sig=…&exp=…`. Does NOT verify the signature — that's the
/// verifier-under-test's job.
pub fn parse_signed_url(url: &str) -> Result<ParsedSignedUrl, String> {
    let (path_and_query, _) = match url.split_once('#') {
        Some(s) => s,
        None => (url, ""),
    };
    let (path, query) = path_and_query
        .split_once('?')
        .ok_or_else(|| "url has no query string".to_string())?;

    let prefix = "/uploads/";
    let idx = path
        .find(prefix)
        .ok_or_else(|| format!("path does not contain {prefix:?}"))?;
    let filename = path[idx + prefix.len()..].to_string();
    if filename.is_empty() {
        return Err("empty filename after /uploads/".into());
    }

    let mut sig_b64: Option<String> = None;
    let mut exp_unix: Option<u64> = None;
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        match k {
            "sig" => sig_b64 = Some(v.to_string()),
            "exp" => {
                exp_unix = Some(
                    v.parse::<u64>()
                        .map_err(|e| format!("exp not a u64: {e}"))?,
                );
            }
            _ => {}
        }
    }

    Ok(ParsedSignedUrl {
        filename,
        sig_b64: sig_b64.ok_or_else(|| "missing sig= param".to_string())?,
        exp_unix: exp_unix.ok_or_else(|| "missing exp= param".to_string())?,
    })
}

/// Return a clone of `url` with a single-bit flip applied to the base64url
/// `sig=` parameter. Useful for "tampered signature must be rejected" tests.
///
/// Strategy: flip the very first signature character between its lower-case
/// and upper-case form (URL_SAFE_NO_PAD alphabet contains both halves, so
/// either direction stays within the alphabet and decodes to a different
/// byte — guaranteed MAC mismatch).
pub fn tampered_sig(url: &str) -> String {
    let sig_param = "sig=";
    let Some(start) = url.find(sig_param) else {
        return url.to_string();
    };
    let value_start = start + sig_param.len();
    let bytes = url.as_bytes();
    if value_start >= bytes.len() {
        return url.to_string();
    }
    let first = bytes[value_start];
    let replacement = match first {
        b'a'..=b'z' => first - (b'a' - b'A'),
        b'A'..=b'Z' => first + (b'a' - b'A'),
        b'0'..=b'8' => first + 1,
        b'9' => b'0',
        b'-' => b'_',
        b'_' => b'-',
        other => other ^ 0x01,
    };
    let mut out = Vec::with_capacity(url.len());
    out.extend_from_slice(&bytes[..value_start]);
    out.push(replacement);
    out.extend_from_slice(&bytes[value_start + 1..]);
    String::from_utf8(out).expect("ascii in, ascii out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_and_parse_round_trip() {
        let key = [7u8; 32];
        let url = mint_signed_url(
            "http://host",
            "5d6f0b60-0000-4000-8000-000000000001",
            "png",
            &key,
            86_400,
        );

        assert!(url.starts_with("http://host/uploads/"), "unexpected: {url}");
        assert!(url.contains("?sig="), "missing sig: {url}");
        assert!(url.contains("&exp="), "missing exp: {url}");

        let parsed = parse_signed_url(&url).expect("parse round-trip");
        assert_eq!(parsed.filename, "5d6f0b60-0000-4000-8000-000000000001.png");
        assert!(!parsed.sig_b64.is_empty());
        // Signature MUST NOT contain standard-b64 padding or unsafe chars.
        assert!(
            !parsed.sig_b64.contains('='),
            "URL_SAFE_NO_PAD must omit padding: {}",
            parsed.sig_b64
        );
        assert!(!parsed.sig_b64.contains('+'));
        assert!(!parsed.sig_b64.contains('/'));
        assert!(parsed.exp_unix > 0);
    }

    #[test]
    fn trailing_slash_in_base_is_tolerated() {
        let key = [1u8; 32];
        let url = mint_signed_url("http://host/", "abc", "jpg", &key, 60);
        // Should not produce a double slash.
        assert!(!url.contains("//uploads"), "double slash: {url}");
        assert!(url.contains("/uploads/abc.jpg?"), "unexpected path: {url}");
    }

    #[test]
    fn tampered_sig_changes_signature_byte() {
        let key = [0u8; 32];
        let url = mint_signed_url("http://host", "abc", "jpg", &key, 60);
        let tampered = tampered_sig(&url);

        assert_ne!(url, tampered, "tampered URL must differ");
        assert_eq!(url.len(), tampered.len(), "tamper is single-byte flip");

        let orig = parse_signed_url(&url).unwrap();
        let new = parse_signed_url(&tampered).unwrap();
        assert_eq!(orig.exp_unix, new.exp_unix, "exp must be untouched");
        assert_eq!(orig.filename, new.filename, "filename must be untouched");
        assert_ne!(orig.sig_b64, new.sig_b64, "signature must differ");
        // Differ by exactly one character.
        let diffs = orig
            .sig_b64
            .chars()
            .zip(new.sig_b64.chars())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(diffs, 1, "tamper flips exactly one char");
    }

    #[test]
    fn parse_rejects_urls_with_no_query() {
        let err = parse_signed_url("http://host/uploads/abc.jpg").unwrap_err();
        assert!(err.contains("no query string"), "got: {err}");
    }

    #[test]
    fn parse_rejects_urls_missing_sig_param() {
        let err = parse_signed_url("http://host/uploads/abc.jpg?exp=1").unwrap_err();
        assert!(err.contains("sig="), "got: {err}");
    }
}
