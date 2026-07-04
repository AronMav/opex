//! Defence-in-depth: strip Core's own secrets from the environment of any
//! subprocess spawned directly on the host (no Docker isolation).
//!
//! Host-spawned subprocesses (`code_exec` host fallback, CLI-backend host
//! execution, provider CLI test-connection) inherit the Core process's full
//! environment by default — including `OPEX_MASTER_KEY` (vault encryption
//! key), `OPEX_AUTH_TOKEN` (API bearer token), and `DATABASE_URL` (Postgres
//! credentials). Model-generated code or an operator-triggered CLI test has
//! no legitimate need for these. Docker-sandboxed execution is unaffected
//! (containers never inherit host env — see `containers/sandbox.rs`); this
//! module only guards the host-execution paths that bypass the sandbox.

/// Env var names that must never be visible to a host-spawned subprocess.
pub(crate) const HOST_SPAWN_STRIP_KEYS: &[&str] =
    &["OPEX_MASTER_KEY", "OPEX_AUTH_TOKEN", "DATABASE_URL"];

/// Anything that exposes `.env_remove(&str) -> &mut Self`, i.e.
/// `std::process::Command` and `tokio::process::Command` both qualify.
pub(crate) trait EnvRemovable {
    fn env_remove(&mut self, key: &str) -> &mut Self;
}

impl EnvRemovable for std::process::Command {
    fn env_remove(&mut self, key: &str) -> &mut Self {
        std::process::Command::env_remove(self, key)
    }
}

impl EnvRemovable for tokio::process::Command {
    fn env_remove(&mut self, key: &str) -> &mut Self {
        tokio::process::Command::env_remove(self, key)
    }
}

/// Strip Core's own secrets (`HOST_SPAWN_STRIP_KEYS`) from a `Command` about
/// to be spawned on the host. Call this immediately before `.spawn()`/
/// `.output()`/`.status()` so nothing re-adds a stripped key afterwards.
pub(crate) fn strip_host_secrets<C: EnvRemovable>(cmd: &mut C) -> &mut C {
    for key in HOST_SPAWN_STRIP_KEYS {
        cmd.env_remove(key);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_all_configured_keys_std_command() {
        let mut cmd = std::process::Command::new("true");
        cmd.env("OPEX_MASTER_KEY", "secret-master")
            .env("OPEX_AUTH_TOKEN", "secret-token")
            .env("DATABASE_URL", "postgres://secret")
            .env("SOME_OTHER_VAR", "keep-me");
        strip_host_secrets(&mut cmd);

        // std::process::Command doesn't expose a getter for env vars, so we
        // rely on the #[cfg(unix)] spawn-based test below for end-to-end
        // proof. Here we just confirm the call compiles/chains for both
        // Command types (compile-time proof of the generic bound).
        let _ = cmd;
    }

    #[test]
    fn strips_all_configured_keys_tokio_command() {
        let mut cmd = tokio::process::Command::new("true");
        cmd.env("OPEX_MASTER_KEY", "secret-master")
            .env("OPEX_AUTH_TOKEN", "secret-token")
            .env("DATABASE_URL", "postgres://secret");
        strip_host_secrets(&mut cmd);
        let _ = cmd;
    }

    /// End-to-end proof on unix: spawn a shell that echoes the secret env
    /// var. Parent process has the key set (simulating Core's real env);
    /// after `strip_host_secrets`, the child must not see it.
    #[cfg(unix)]
    #[tokio::test]
    async fn child_process_does_not_inherit_stripped_secrets() {
        // SAFETY: test-only; no other test in this binary reads/depends on
        // these specific env var names concurrently.
        unsafe {
            std::env::set_var("OPEX_MASTER_KEY", "test-master-key-xyz");
            std::env::set_var("OPEX_AUTH_TOKEN", "test-auth-token-xyz");
            std::env::set_var("DATABASE_URL", "postgres://test-secret");
            std::env::set_var("KEEP_ME", "still-here");
        }

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("echo \"MK=[$OPEX_MASTER_KEY] AT=[$OPEX_AUTH_TOKEN] DB=[$DATABASE_URL] KEEP=[$KEEP_ME]\"")
            .stdout(std::process::Stdio::piped());
        strip_host_secrets(&mut cmd);

        let output = cmd.output().await.expect("failed to spawn sh");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(stdout.contains("MK=[]"), "stdout was: {stdout}");
        assert!(stdout.contains("AT=[]"), "stdout was: {stdout}");
        assert!(stdout.contains("DB=[]"), "stdout was: {stdout}");
        assert!(stdout.contains("KEEP=[still-here]"), "stdout was: {stdout}");

        unsafe {
            std::env::remove_var("OPEX_MASTER_KEY");
            std::env::remove_var("OPEX_AUTH_TOKEN");
            std::env::remove_var("DATABASE_URL");
            std::env::remove_var("KEEP_ME");
        }
    }
}
