//! Magic-byte + extension file-type sniffer for the File Scenario Engine.
//! Sniffed MIME wins; extension breaks ties; channel-declared type is last resort.

use crate::gateway::handlers::media::is_safe_client_upload_mime;
use hydeclaw_types::MediaType;

/// Where the effective MIME type was determined from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SniffSource {
    MagicBytes,
    Extension,
    Declared,
}

/// Resolved file-type information returned by [`sniff_bytes`].
#[derive(Debug, Clone)]
pub struct SniffedType {
    /// Effective MIME type (safe: never svg/html — forced to `application/octet-stream`).
    pub mime: String,
    /// Best-guess extension (from magic bytes or filename; `None` when only declared).
    #[allow(dead_code)] // Phase 4+: surfaced in binding-match UI and audit log
    pub extension: Option<String>,
    /// Which layer resolved the MIME.
    #[allow(dead_code)] // Phase 4+: surfaced in binding-match UI and audit log
    pub source: SniffSource,
}

/// Map a channel-declared coarse `MediaType` to a generic MIME family.
/// Last-resort only (used when both magic bytes and extension are inconclusive).
fn declared_family(mt: MediaType) -> &'static str {
    match mt {
        MediaType::Image => "image/*",
        MediaType::Audio => "audio/mpeg",
        MediaType::Video => "video/mp4",
        MediaType::Document => "application/octet-stream",
    }
}

/// Resolve the real file type from a bounded byte prefix + filename + declared hints.
///
/// Conflict rule (spec §4.3): sniffed MIME (magic bytes) wins; the extension
/// breaks ties only when magic-byte sniffing is inconclusive; the channel-declared
/// `media_type`/`mime` is the last resort. After resolving, the candidate mime is
/// re-checked with `is_safe_client_upload_mime`: an unsafe result (svg/html) is
/// forced to `application/octet-stream` (it is then handled by the `save` fallback).
pub fn sniff_bytes(
    prefix: &[u8],
    file_name: Option<&str>,
    declared_mime: Option<&str>,
    declared_media_type: MediaType,
) -> SniffedType {
    // 1. Magic bytes (authoritative when present).
    if let Some(t) = infer::get(prefix) {
        let mime = t.mime_type().to_string();
        return finalize(mime, Some(t.extension().to_string()), SniffSource::MagicBytes);
    }

    // 2. Extension fallback.
    if let Some(name) = file_name {
        let guess = mime_guess::from_path(name).first_raw();
        if let Some(m) = guess {
            let ext = name.rsplit('.').next().map(|e| e.to_string());
            return finalize(m.to_string(), ext, SniffSource::Extension);
        }
    }

    // 3. Declared mime, then declared media_type family (last resort).
    let last = declared_mime
        .map(|s| s.to_string())
        .unwrap_or_else(|| declared_family(declared_media_type).to_string());
    finalize(last, None, SniffSource::Declared)
}

/// Apply the unsafe-mime gate: anything `is_safe_client_upload_mime` rejects
/// (svg, html, …) collapses to `application/octet-stream` so dispatch never
/// auto-runs a built-in against script-bearing bytes.
fn finalize(mime: String, extension: Option<String>, source: SniffSource) -> SniffedType {
    let mime = if is_safe_client_upload_mime(&mime) {
        mime
    } else {
        "application/octet-stream".to_string()
    };
    SniffedType { mime, extension, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::MediaType;

    const PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn magic_bytes_override_misleading_declared_mime() {
        // Channel claims jpeg; bytes are PNG → sniff wins.
        let s = sniff_bytes(PNG, Some("photo.jpg"), Some("image/jpeg"), MediaType::Image);
        assert_eq!(s.mime, "image/png");
        assert_eq!(s.source, SniffSource::MagicBytes);
    }

    #[test]
    fn extension_fallback_when_sniff_inconclusive() {
        // Plain UTF-8 text has no magic signature → fall back to extension.
        let s = sniff_bytes(b"hello world", Some("notes.txt"), None, MediaType::Document);
        assert_eq!(s.mime, "text/plain");
        assert_eq!(s.source, SniffSource::Extension);
    }

    #[test]
    fn declared_is_last_resort() {
        // No magic, no usable extension → declared media_type maps to a family.
        let s = sniff_bytes(b"\x00\x01\x02", Some("blob"), None, MediaType::Audio);
        assert_eq!(s.source, SniffSource::Declared);
        assert!(s.mime.starts_with("audio/"));
    }

    #[test]
    fn unsafe_svg_forced_to_octet_stream() {
        // SVG sniffs to image/svg+xml which is_safe_client_upload_mime rejects.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        let s = sniff_bytes(svg, Some("x.svg"), Some("image/svg+xml"), MediaType::Image);
        assert_eq!(s.mime, "application/octet-stream");
    }

    #[test]
    fn unsafe_html_forced_to_octet_stream() {
        let s = sniff_bytes(b"<!doctype html><html></html>", Some("x.html"), Some("text/html"), MediaType::Document);
        assert_eq!(s.mime, "application/octet-stream");
    }
}
