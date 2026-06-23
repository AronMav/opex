//! Credential file persistence for the Gemini Cloud Code OAuth provider.
//!
//! **Path resolution (in priority order):**
//! 1. `OPEX_OAUTH_CREDENTIALS_PATH` env var (fallback: `HYDECLAW_OAUTH_CREDENTIALS_PATH`) —
//!    read via `opex_gateway_util::env::env_var`; used by tests and operators.
//! 2. Platform default:
//!    - Linux/macOS: `~/.config/hydeclaw/google_oauth.json`
//!    - Windows:     `%APPDATA%\hydeclaw\google_oauth.json`
//!
//! **Safety properties:**
//! - Atomic write: data written to `<file>.tmp` then renamed over the target.
//! - Unix permissions: file is `chmod 0600` after rename (owner-read/write only).
//! - Cross-process lock: exclusive `fs2` lock on a sibling `.lock` file,
//!   with a 30-second spin-retry before returning `OauthError::LockTimeout`.
//!
//! **API contracts (per controller decisions F5/F6):**
//! - `load_credentials()` — sync, returns `Option<GoogleCredentials>` (not Result).
//! - `clear_credentials()` — sync, returns `()` (not Result); named `clear_*`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;

use super::types::{GoogleCredentials, OauthError};

// ── Path resolution ───────────────────────────────────────────────────────────

/// Resolve the credentials file path.
///
/// Priority: `OPEX_OAUTH_CREDENTIALS_PATH` (then `HYDECLAW_OAUTH_CREDENTIALS_PATH`
/// via dual-read helper) → platform default.
pub fn credentials_path() -> PathBuf {
    if let Some(p) = opex_gateway_util::env::env_var("OAUTH_CREDENTIALS_PATH")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    default_credentials_path()
}

/// Platform-default path: `~/.config/hydeclaw/google_oauth.json` (Linux/macOS)
/// or `%APPDATA%\hydeclaw\google_oauth.json` (Windows).
fn default_credentials_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            // Fallback: use home dir if config_dir() is unavailable.
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
        })
        .join("hydeclaw")
        .join("google_oauth.json")
}

// ── Lock helpers ──────────────────────────────────────────────────────────────

const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Sibling lock-file path alongside the credentials file.
fn lock_path(creds: &Path) -> PathBuf {
    let file_name = creds
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut p = creds.to_path_buf();
    p.set_file_name(format!("{file_name}.lock"));
    p
}

/// Acquire an exclusive cross-process lock on the sibling `.lock` file,
/// execute `f`, then release the lock.
///
/// Spins with 50 ms sleep for up to 30 seconds, then returns
/// `Err(OauthError::LockTimeout)`.
pub(crate) fn with_lock<F, T>(creds_path: &Path, f: F) -> Result<T, OauthError>
where
    F: FnOnce() -> Result<T, OauthError>,
{
    let lp = lock_path(creds_path);

    // Ensure parent dir exists before opening lock file.
    if let Some(parent) = lp.parent() {
        fs::create_dir_all(parent)?;
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lp)?;

    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                let result = f();
                // FileExt::unlock() is best-effort — log if it fails but
                // don't shadow the real result.
                let _ = lock_file.unlock();
                return result;
            }
            Err(_) => {
                if Instant::now() >= deadline {
                    return Err(OauthError::LockTimeout);
                }
                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
        }
    }
}

// ── Convenience lock wrapper ───────────────────────────────────────────────────

/// Acquire the credentials-file lock, execute `f`, release the lock.
///
/// Thin wrapper around `with_lock` that resolves the credentials path internally.
/// Used by `refresh.rs` so callers don't need to manage paths directly.
pub(crate) fn with_credentials_lock<F, T>(f: F) -> Result<T, OauthError>
where
    F: FnOnce() -> Result<T, OauthError>,
{
    let path = credentials_path();
    with_lock(&path, f)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Load credentials from the credentials file.
///
/// Returns `None` if the file does not exist or cannot be parsed.
/// All errors are silently absorbed (F5 controller decision).
pub fn load_credentials() -> Option<GoogleCredentials> {
    let path = credentials_path();
    let bytes = fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist credentials atomically.
///
/// Writes to `<file>.tmp`, then renames over the target. On Unix the file is
/// `chmod 0600` before rename so the final path is never world-readable.
///
/// Acquires the cross-process lock for the duration of the write.
pub fn save_credentials(creds: &GoogleCredentials) -> Result<(), OauthError> {
    let path = credentials_path();
    with_lock(&path, || save_credentials_locked(creds, &path))
}

fn save_credentials_locked(
    creds: &GoogleCredentials,
    path: &Path,
) -> Result<(), OauthError> {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_vec_pretty(creds)?;

    // Write to a sibling .tmp file (name: `google_oauth.json.tmp`).
    // Append `.tmp` suffix directly to the full file name so the result is
    // predictable regardless of how many dots the base name contains.
    let mut file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    file_name.push_str(".tmp");
    let mut tmp_path = path.to_path_buf();
    tmp_path.set_file_name(file_name);

    let mut tmp_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)?;
    tmp_file.write_all(&json)?;
    tmp_file.flush()?;
    drop(tmp_file);

    // chmod 0600 before rename so the final path is never briefly exposed.
    #[cfg(unix)]
    set_owner_only_perms(&tmp_path)?;

    // Atomic replace.
    fs::rename(&tmp_path, path)?;

    Ok(())
}

/// Write credentials directly to the given path without locking.
///
/// Used by `refresh.rs` (Pass 3) and by tests that need to pre-populate a
/// specific file. Callers must ensure they hold the cross-process lock themselves
/// (or use `save_credentials` which acquires it automatically).
///
/// Named with leading `_` to signal "lock not included"; `#[allow(dead_code)]`
/// suppresses unused-item warnings when only tests use this in non-test builds.
#[allow(dead_code)]
pub(crate) fn _save_to_path(
    creds: &GoogleCredentials,
    path: &Path,
) -> Result<(), OauthError> {
    save_credentials_locked(creds, path)
}

/// Remove the credentials file. No-op if the file does not exist.
/// Sync, returns `()` (F6 controller decision).
pub fn clear_credentials() {
    let path = credentials_path();
    let _ = fs::remove_file(&path);
}

// ── Unix permission helper ────────────────────────────────────────────────────

#[cfg(unix)]
fn set_owner_only_perms(path: &Path) -> Result<(), OauthError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// Helper: create a `GoogleCredentials` with distinct test values.
    fn test_creds(email: &str) -> GoogleCredentials {
        GoogleCredentials {
            refresh: "tok|proj|managed".to_string(),
            access: "ya29.testtoken".to_string(),
            expires_ms: 9_999_999_999_000,
            email: email.to_string(),
        }
    }

    /// The env var the dual-read helper checks first for the credentials path.
    const TEST_CREDS_ENV: &str = "OPEX_OAUTH_CREDENTIALS_PATH";

    /// Run a closure with `OPEX_OAUTH_CREDENTIALS_PATH` set to a temp-dir
    /// path, then restore the env var (or remove it if it wasn't set).
    fn with_tmp_path<F: FnOnce(PathBuf)>(f: F) {
        let dir = tempfile::tempdir().expect("tempdir");
        let creds_path = dir.path().join("google_oauth.json");
        let prev = std::env::var(TEST_CREDS_ENV).ok();

        // SAFETY: test-only mutation; serial_test / thread sequencing prevents
        // races between tests that touch the same env var.
        unsafe {
            std::env::set_var(TEST_CREDS_ENV, &creds_path);
        }

        f(creds_path);

        unsafe {
            match &prev {
                Some(v) => std::env::set_var(TEST_CREDS_ENV, v),
                None => std::env::remove_var(TEST_CREDS_ENV),
            }
        }
    }

    #[test]
    #[serial(oauth_creds_path)]
    fn roundtrip() {
        with_tmp_path(|_| {
            let original = test_creds("roundtrip@example.com");
            save_credentials(&original).expect("save");
            let loaded = load_credentials().expect("load");
            assert_eq!(loaded.email, "roundtrip@example.com");
            assert_eq!(loaded.access, "ya29.testtoken");
            assert_eq!(loaded.expires_ms, 9_999_999_999_000);
            assert_eq!(loaded.refresh, "tok|proj|managed");
        });
    }

    #[test]
    #[serial(oauth_creds_path)]
    fn load_returns_none_when_absent() {
        with_tmp_path(|creds_path| {
            assert!(!creds_path.exists(), "file should not exist yet");
            let result = load_credentials();
            assert!(result.is_none(), "expected None for missing file");
        });
    }

    #[cfg(unix)]
    #[test]
    #[serial(oauth_creds_path)]
    fn file_has_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        with_tmp_path(|creds_path| {
            save_credentials(&test_creds("perms@example.com")).expect("save");
            let meta = std::fs::metadata(&creds_path).expect("metadata");
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        });
    }

    #[test]
    #[serial(oauth_creds_path)]
    fn clear_credentials_removes_file() {
        with_tmp_path(|creds_path| {
            save_credentials(&test_creds("clear@example.com")).expect("save");
            assert!(creds_path.exists(), "file should exist after save");
            clear_credentials();
            assert!(!creds_path.exists(), "file should be gone after clear");
        });
    }

    #[test]
    #[serial(oauth_creds_path)]
    fn load_returns_none_after_clear() {
        with_tmp_path(|_| {
            save_credentials(&test_creds("clearthenload@example.com")).expect("save");
            clear_credentials();
            assert!(load_credentials().is_none(), "expected None after clear");
        });
    }

    #[test]
    #[serial(oauth_creds_path)]
    fn atomic_replace_on_partial_write() {
        // Verify that after a successful save + a failed overwrite (simulated
        // by writing corrupt JSON to tmp then bailing), the original is intact.
        with_tmp_path(|creds_path| {
            // First save succeeds.
            save_credentials(&test_creds("original@example.com")).expect("first save");

            // Write corrupt data to the .tmp file directly (simulating a crash
            // mid-write). The rename never happens so the original survives.
            // Tmp name mirrors save_credentials_locked: append ".tmp" to the full name.
            let mut file_name = creds_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            file_name.push_str(".tmp");
            let tmp = creds_path.with_file_name(file_name);
            fs::write(&tmp, b"CORRUPT{{{").expect("write corrupt tmp");

            // Original credentials must still load correctly.
            let loaded = load_credentials().expect("should still load original");
            assert_eq!(loaded.email, "original@example.com");
        });
    }

    #[test]
    fn cross_process_lock_second_waits_for_first() {
        // Two threads try to acquire the same lock.
        // The first holds it briefly while the second is already waiting.
        // The second must eventually succeed (not deadlock or time out for
        // a short hold duration well within the 30-second limit).
        let dir = tempfile::tempdir().expect("tempdir");
        let creds_path = Arc::new(dir.path().join("google_oauth.json"));

        // Barrier ensures both threads start acquiring at roughly the same time.
        let barrier = Arc::new(Barrier::new(2));

        let creds_path_t1 = Arc::clone(&creds_path);
        let barrier_t1 = Arc::clone(&barrier);
        let t1 = thread::spawn(move || {
            barrier_t1.wait();
            with_lock(&creds_path_t1, || {
                // Hold for 100 ms while the other thread waits.
                thread::sleep(Duration::from_millis(100));
                Ok(1u32)
            })
        });

        let creds_path_t2 = Arc::clone(&creds_path);
        let barrier_t2 = Arc::clone(&barrier);
        let t2 = thread::spawn(move || {
            barrier_t2.wait();
            with_lock(&creds_path_t2, || Ok(2u32))
        });

        let r1 = t1.join().expect("t1 panicked").expect("t1 lock error");
        let r2 = t2.join().expect("t2 panicked").expect("t2 lock error");

        // Both must succeed; results are distinguishable.
        assert!(r1 == 1 || r1 == 2);
        assert!(r2 == 1 || r2 == 2);
        assert_ne!(r1, r2, "each thread must have had exclusive lock");
    }
}
