//! 3-tier OAuth client credential resolution for Gemini Code Assist.
//!
//! Resolution order (first match wins):
//!  1. Environment variables `HYDECLAW_GEMINI_CLIENT_ID` /
//!     `HYDECLAW_GEMINI_CLIENT_SECRET`.
//!  2. Scrape the `~/.npm-global/lib/node_modules/@google/gemini-cli/...` bundle
//!     (best-effort; silently skipped if the file is absent or unreadable).
//!  3. The published public OAuth client credentials bundled with gemini-cli.
//!
//! Design decision F7: `resolve_client_creds` is a sync, infallible function
//! (never returns an error — public defaults are always the last-resort fallback).

// Fields used only by later tasks; suppress until wired up.
#![allow(dead_code)]

/// Published public OAuth client credentials bundled with gemini-cli.
/// Used as fallback when no machine-local install or env override is present.
const DEFAULT_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const DEFAULT_CLIENT_SECRET: &str = "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";

/// Env var names for operator-supplied client credentials.
const ENV_CLIENT_ID: &str = "HYDECLAW_GEMINI_CLIENT_ID";
const ENV_CLIENT_SECRET: &str = "HYDECLAW_GEMINI_CLIENT_SECRET";

/// Resolved OAuth client ID and secret.
///
/// These are the credentials used when initiating the authorization-code
/// or device flow with Google's OAuth2 endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthClientCreds {
    pub client_id: String,
    pub client_secret: String,
}

impl OauthClientCreds {
    fn public_default() -> Self {
        Self {
            client_id: DEFAULT_CLIENT_ID.to_string(),
            client_secret: DEFAULT_CLIENT_SECRET.to_string(),
        }
    }
}

/// Resolve OAuth client credentials using the 3-tier lookup.
///
/// Never fails — falls through to the published public defaults if both
/// env-var and scrape tiers are unavailable.
pub fn resolve_client_creds() -> OauthClientCreds {
    // Tier 1: explicit env override.
    if let (Ok(id), Ok(secret)) = (
        std::env::var(ENV_CLIENT_ID),
        std::env::var(ENV_CLIENT_SECRET),
    ) && !id.is_empty() && !secret.is_empty()
    {
        return OauthClientCreds {
            client_id: id,
            client_secret: secret,
        };
    }

    // Tier 2: scrape local gemini-cli npm installation (best-effort).
    if let Some(creds) = scrape_npm_install() {
        return creds;
    }

    // Tier 3: published public defaults.
    OauthClientCreds::public_default()
}

/// Attempt to extract client credentials from the gemini-cli npm bundle.
///
/// Looks for the `oauth2.js` or similar bundle under:
///   `~/.npm-global/lib/node_modules/@google/gemini-cli/`
///
/// Returns `None` silently on any failure (missing file, parse error, etc.).
fn scrape_npm_install() -> Option<OauthClientCreds> {
    let home = home_dir()?;
    let base = home.join(".npm-global/lib/node_modules/@google/gemini-cli");
    if !base.exists() {
        return None;
    }

    // Walk candidate JS bundle files looking for the credential strings.
    let candidates = [
        base.join("bundle/gemini.js"),
        base.join("dist/index.js"),
        base.join("build/index.js"),
    ];

    for path in &candidates {
        if let Ok(src) = std::fs::read_to_string(path)
            && let Some(creds) = extract_from_source(&src)
        {
            return Some(creds);
        }
    }

    None
}

/// Extract client_id and client_secret from a JavaScript bundle source.
///
/// Looks for patterns like:
///   `clientId:"<id>"` / `clientId: "<id>"` and
///   `clientSecret:"<secret>"` / `clientSecret: "<secret>"`
fn extract_from_source(src: &str) -> Option<OauthClientCreds> {
    let id = extract_js_string_value(src, "clientId")?;
    let secret = extract_js_string_value(src, "clientSecret")?;
    if id.contains(".apps.googleusercontent.com") && secret.starts_with("GOCSPX-") {
        Some(OauthClientCreds {
            client_id: id,
            client_secret: secret,
        })
    } else {
        None
    }
}

/// Extract the string value of a JS property like `key:"value"` or `key: "value"`.
fn extract_js_string_value(src: &str, key: &str) -> Option<String> {
    // Find `key:` or `key: ` (with optional space) followed by a quoted string.
    let marker = format!("{key}:\"");
    let marker_space = format!("{key}: \"");

    let start = src
        .find(&marker)
        .map(|i| i + marker.len())
        .or_else(|| src.find(&marker_space).map(|i| i + marker_space.len()))?;

    let rest = &src[start..];
    let end = rest.find('"')?;
    let value = &rest[..end];
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Cross-platform home directory lookup.
///
/// Uses `dirs::home_dir()` when the `gemini-cloudcode` feature is active,
/// which pulls in the `dirs = "5"` optional dependency.
fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(feature = "gemini-cloudcode")]
    {
        dirs::home_dir()
    }
    #[cfg(not(feature = "gemini-cloudcode"))]
    {
        // Fallback for compilation outside the feature (e.g. doc tests).
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial(gemini_env)]
    fn env_override_wins() {
        // Arrange
        unsafe {
            std::env::set_var(ENV_CLIENT_ID, "custom-id");
            std::env::set_var(ENV_CLIENT_SECRET, "custom-secret");
        }

        // Act
        let creds = resolve_client_creds();

        // Cleanup before assert so panics still clean up.
        unsafe {
            std::env::remove_var(ENV_CLIENT_ID);
            std::env::remove_var(ENV_CLIENT_SECRET);
        }

        assert_eq!(creds.client_id, "custom-id");
        assert_eq!(creds.client_secret, "custom-secret");
    }

    #[test]
    #[serial(gemini_env)]
    fn public_default_returned_when_no_env_no_scrape() {
        // Ensure env vars are absent.
        unsafe {
            std::env::remove_var(ENV_CLIENT_ID);
            std::env::remove_var(ENV_CLIENT_SECRET);
        }

        let creds = resolve_client_creds();

        // When no local gemini-cli install present, must fall through to defaults.
        // (Scrape tier is best-effort and will silently skip on CI/dev machines.)
        // We accept either scraped-or-default; the important invariant is that
        // client_id is non-empty and is a Google OAuth client ID.
        assert!(
            !creds.client_id.is_empty(),
            "client_id must not be empty"
        );
        assert!(
            !creds.client_secret.is_empty(),
            "client_secret must not be empty"
        );
    }

    #[test]
    #[serial(gemini_env)]
    fn empty_env_vars_fall_through_to_default() {
        // Empty strings should NOT be treated as a valid override.
        unsafe {
            std::env::set_var(ENV_CLIENT_ID, "");
            std::env::set_var(ENV_CLIENT_SECRET, "");
        }

        let creds = resolve_client_creds();

        unsafe {
            std::env::remove_var(ENV_CLIENT_ID);
            std::env::remove_var(ENV_CLIENT_SECRET);
        }

        // Should NOT return empty credentials — falls through to scrape/default.
        assert!(!creds.client_id.is_empty());
        assert!(!creds.client_secret.is_empty());
    }

    #[test]
    fn extract_from_source_happy_path() {
        let src = r#"const x={clientId:"681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",clientSecret:"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"}"#;
        let creds = extract_from_source(src).expect("should parse");
        assert_eq!(
            creds.client_id,
            "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"
        );
        assert_eq!(creds.client_secret, "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");
    }

    #[test]
    fn extract_from_source_with_spaces() {
        let src = r#"{ clientId: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com", clientSecret: "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" }"#;
        let creds = extract_from_source(src).expect("should parse spaced format");
        assert!(creds.client_id.contains("googleusercontent.com"));
    }

    #[test]
    fn extract_from_source_rejects_wrong_format() {
        // No googleusercontent.com or GOCSPX- prefix → reject.
        let src = r#"clientId:"not-a-google-id",clientSecret:"not-a-secret""#;
        assert!(extract_from_source(src).is_none());
    }

    #[test]
    fn public_default_has_expected_client_id() {
        let creds = OauthClientCreds::public_default();
        assert_eq!(
            creds.client_id,
            "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"
        );
        assert!(creds.client_secret.starts_with("GOCSPX-"));
    }
}
