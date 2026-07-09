//! Signed-URL endpoint for workspace artifacts created by workspace_write,
//! workspace_edit, and the code-execution tool. No Bearer auth — security
//! is the HMAC sig + expiry mediated by `mint_workspace_file_url`.

use std::path::Path as StdPath;

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::gateway::clusters::InfraServices;
use crate::gateway::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/workspace-files/{*path}", get(serve_workspace_file))
}

#[derive(serde::Deserialize)]
pub(crate) struct SignedQuery {
    sig: String,
    exp: u64,
}

pub(crate) async fn serve_workspace_file(
    State(infra): State<InfraServices>,
    Path(rel_decoded): Path<String>,
    Query(q): Query<SignedQuery>,
) -> Response {
    // axum 0.8's Path extractor already percent-decodes the {*path} capture.
    // A second manual decode here turned a literal '%2F' in a legitimate filename
    // into '/', so verify_workspace_file_url signed a different string than mint
    // did → 403 for any %-containing filename (F128). Consume the extractor value
    // directly, exactly once — consistent with mint's single encode.
    let key = infra.secrets.get_upload_hmac_key();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if crate::uploads::verify_workspace_file_url(&rel_decoded, &q.sig, q.exp, &key, now).is_err() {
        return (StatusCode::FORBIDDEN, "invalid or expired signature").into_response();
    }

    let workspace_root = StdPath::new(crate::config::WORKSPACE_DIR);
    let workspace_canon = match workspace_root.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "workspace not found").into_response(),
    };
    let abs = match workspace_root.join(&rel_decoded).canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    if !abs.starts_with(&workspace_canon) {
        return (StatusCode::FORBIDDEN, "path escapes workspace").into_response();
    }

    let mime = crate::uploads::guess_mime_from_extension(&rel_decoded);
    // F092: this endpoint is PUBLIC (auth-exempt) and can serve agent-written
    // .html/.svg. Without hardening, an admin opening such a signed URL top-level
    // executes same-origin script that can exfiltrate the bearer token from
    // localStorage. Mirror uploads_serve: never MIME-sniff, and force any
    // non-inlineable type (html, svg, pdf, text, application/*) to download.
    let inlineable = super::uploads_serve::is_inlineable_mime(mime);
    // F093: stream the file instead of buffering the whole thing into RAM. The
    // workspace serves arbitrary-size binaries (mp4/webm/zip/tar dropped by the
    // video pipeline / code_exec / backups); a `tokio::fs::read` of a multi-GB
    // file — or several concurrent large loads — drove OOM on the single-binary
    // home-lab box. ReaderStream reads in bounded chunks.
    let file = match tokio::fs::File::open(&abs).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", mime)
        .header("Cache-Control", "private, max-age=900")
        .header("X-Content-Type-Options", "nosniff");
    if !inlineable {
        let filename = StdPath::new(&rel_decoded)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        builder = builder.header(
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        );
    }
    builder
        .body(body)
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build response").into_response())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    fn write_file(dir: &std::path::Path, rel: &str, content: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
    }

    #[test]
    fn verify_accepts_valid_signature() {
        let key = [42u8; 32];
        let url = crate::uploads::mint_workspace_file_url("a.md", &key, 3600);
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        crate::uploads::verify_workspace_file_url("a.md", sig, exp, &key, now()).unwrap();
    }

    #[test]
    fn verify_rejects_expired() {
        let key = [42u8; 32];
        let url = crate::uploads::mint_workspace_file_url("a.md", &key, 1);
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        let result = crate::uploads::verify_workspace_file_url("a.md", sig, exp, &key, exp + 1);
        assert!(matches!(result, Err(crate::uploads::UploadSignatureError::Expired)));
    }

    #[test]
    fn verify_rejects_tampered_sig() {
        let key = [42u8; 32];
        let exp = now() + 60;
        let bogus = "AAAA";
        assert!(crate::uploads::verify_workspace_file_url("a.md", bogus, exp, &key, now()).is_err());
    }

    #[test]
    fn path_traversal_attempt_canonicalizes_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        write_file(workspace.path(), "ok.csv", b"x");
        let outside = tempfile::tempdir().unwrap();
        write_file(outside.path(), "secret.txt", b"don't read");

        let workspace_canon = workspace.path().canonicalize().unwrap();

        let attempt = workspace.path().join("../").join(
            outside.path().file_name().unwrap()
        ).join("secret.txt");
        let attempt_canon = attempt.canonicalize().unwrap();
        assert!(!attempt_canon.starts_with(&workspace_canon),
                "escape attempt must canonicalize outside workspace");

        let inside = workspace.path().join("ok.csv").canonicalize().unwrap();
        assert!(inside.starts_with(&workspace_canon));
    }

    #[test]
    fn signed_url_roundtrips_for_percent_in_filename() {
        // F128: a filename with a literal '%' must round-trip through mint→verify
        // using the RAW name. The handler now passes axum's single-decoded path
        // straight to verify (no second percent-decode), so the string mint signed
        // and the string verify checks are identical. Previously the extra decode
        // turned '%CD' / '%2F' into other bytes → HMAC mismatch → 403.
        let key = [7u8; 32];
        let rel = "AB%CD.log";
        let url = crate::uploads::mint_workspace_file_url(rel, &key, 60);
        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();
        crate::uploads::verify_workspace_file_url(rel, sig, exp, &key, now()).unwrap();
    }

    /// Full round-trip: mint URL with the same path that `handle_workspace_write`
    /// resolves bare filenames to (`agents/{name}/x.md`), then verify sig +
    /// canonicalize + read body — same sequence as `serve_workspace_file`.
    /// Catches the C-2 bug class (marker URL not pointing where the file landed).
    #[test]
    fn roundtrip_mint_verify_resolve_for_agent_file() {
        let workspace = tempfile::tempdir().unwrap();
        write_file(workspace.path(), "agents/Aria/note.md", b"hello world");

        let key = [99u8; 32];
        let rel = "agents/Aria/note.md";
        let url = crate::uploads::mint_workspace_file_url(rel, &key, 60);
        assert!(url.starts_with("/workspace-files/agents/Aria/note.md?"), "{url}");

        let sig = url.split("sig=").nth(1).unwrap().split('&').next().unwrap();
        let exp: u64 = url.split("exp=").nth(1).unwrap().parse().unwrap();

        crate::uploads::verify_workspace_file_url(rel, sig, exp, &key, now()).unwrap();

        let workspace_canon = workspace.path().canonicalize().unwrap();
        let abs = workspace.path().join(rel).canonicalize().unwrap();
        assert!(abs.starts_with(&workspace_canon), "must resolve inside workspace");

        let body = std::fs::read(&abs).unwrap();
        assert_eq!(body, b"hello world");
    }
}
