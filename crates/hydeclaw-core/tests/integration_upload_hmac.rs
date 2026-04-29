//! Phase 64 SEC-03 — HMAC-signed /uploads URL matrix.
//!
//! Covers:
//!   * valid signature accepted
//!   * tampered signature rejected (403 Invalid)
//!   * expired signature rejected (410 Expired)
//!   * missing signature rejected (Missing)
//!   * wrong filename rejected (prevents cross-file replay → Invalid)
//!   * derive_upload_key deterministic (HKDF-SHA256 stability contract)
//!   * constant-time compare: CI-runnable 100_000-iteration timing delta <5 %
//!     (NOT #[ignore] — ROADMAP success criterion #3 requires continuous validation)

mod support;

use hydeclaw_core::uploads::{
    derive_upload_key, mint_signed_url, verify_signed_url,
    SignedUploadQuery, UploadSignatureError,
};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Extract `?sig=&exp=` from a signed URL into a `SignedUploadQuery`.
/// Delegates to `support::parse_signed_url` so query-parsing logic stays in
/// one place (tests/support/signed_url_helper.rs).
fn parse_q(url: &str) -> SignedUploadQuery {
    let parsed = support::parse_signed_url(url)
        .expect("parse_q: url must be a well-formed signed upload URL");
    SignedUploadQuery {
        sig: Some(parsed.sig_b64),
        exp: Some(parsed.exp_unix),
    }
}

#[test]
fn valid_signature_accepts() {
    let key = [7u8; 32];
    let url = mint_signed_url("http://localhost", "abc.png", &key, 86_400);
    let q = parse_q(&url);
    assert!(verify_signed_url("abc.png", &q, &key, now_unix()).is_ok());
}

#[test]
fn tampered_signature_rejected() {
    let key = [7u8; 32];
    let url = mint_signed_url("http://localhost", "abc.png", &key, 86_400);
    let tampered = support::tampered_sig(&url);
    let q = parse_q(&tampered);
    assert_eq!(
        verify_signed_url("abc.png", &q, &key, now_unix()),
        Err(UploadSignatureError::Invalid)
    );
}

#[test]
fn expired_signature_rejected() {
    let key = [7u8; 32];
    let url = mint_signed_url("http://localhost", "abc.png", &key, 10);
    let q = parse_q(&url);
    let future = q.exp.unwrap() + 100;
    assert_eq!(
        verify_signed_url("abc.png", &q, &key, future),
        Err(UploadSignatureError::Expired)
    );
}

#[test]
fn missing_signature_rejected() {
    let q = SignedUploadQuery { sig: None, exp: None };
    assert_eq!(
        verify_signed_url("abc.png", &q, &[0u8; 32], now_unix()),
        Err(UploadSignatureError::Missing)
    );
}

#[test]
fn wrong_filename_rejected() {
    let key = [7u8; 32];
    let url = mint_signed_url("http://localhost", "a.png", &key, 86_400);
    let q = parse_q(&url);
    assert_eq!(
        verify_signed_url("b.png", &q, &key, now_unix()),
        Err(UploadSignatureError::Invalid)
    );
}

#[test]
fn derive_upload_key_is_deterministic() {
    let mk = [42u8; 32];
    assert_eq!(derive_upload_key(&mk), derive_upload_key(&mk));
    assert_ne!(derive_upload_key(&mk), derive_upload_key(&[43u8; 32]));
}

/// CI-runnable timing assertion. NOT #[ignore]. ROADMAP success criterion #3
/// requires continuous validation; ignoring it breaks the contract.
///
/// Budget: 100_000 verify calls total (50 batches × 2_000 iters each side),
/// targeted to run in <5 s on CI hardware including Pi 5 aarch64 runners.
///
/// Assertion: median nanos/verify for valid vs tampered inputs must differ by
/// less than 5 %. `subtle::ConstantTimeEq` provides that guarantee; a naive
/// `==` over the MAC bytes would typically short-circuit on the first mismatch
/// and blow this threshold by 10–100×.
#[test]
fn constant_time_compare_timing() {
    let key = [7u8; 32];
    let url = mint_signed_url("http://localhost", "abc.png", &key, 86_400);
    let q_valid = parse_q(&url);
    let q_bad = parse_q(&support::tampered_sig(&url));

    const BATCH: usize = 2_000;
    const BATCHES: usize = 50;
    let mut valid_ns = Vec::with_capacity(BATCHES);
    let mut bad_ns = Vec::with_capacity(BATCHES);
    let now = now_unix();
    for _ in 0..BATCHES {
        let t = std::time::Instant::now();
        for _ in 0..BATCH {
            let _ = verify_signed_url("abc.png", &q_valid, &key, now);
        }
        valid_ns.push(t.elapsed().as_nanos() as f64 / BATCH as f64);

        let t = std::time::Instant::now();
        for _ in 0..BATCH {
            let _ = verify_signed_url("abc.png", &q_bad, &key, now);
        }
        bad_ns.push(t.elapsed().as_nanos() as f64 / BATCH as f64);
    }
    valid_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    bad_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let vm = valid_ns[BATCHES / 2];
    let bm = bad_ns[BATCHES / 2];
    let delta = (vm - bm).abs() / vm.max(bm);
    eprintln!(
        "median valid={vm:.1}ns bad={bm:.1}ns delta={:.2}% (total iter={})",
        delta * 100.0,
        BATCH * BATCHES * 2
    );
    // 5% is the locked threshold from CONTEXT.md. Do NOT widen.
    assert!(delta < 0.05, "timing delta {} exceeds 5%", delta);
}
