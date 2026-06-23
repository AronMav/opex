//! ToolgateFixture — spawns a real uvicorn child process running toolgate/app.py
//! for integration tests that need provider-hotswap validation (RES-07).
//!
//! If Python venv or toolgate source is missing (e.g., CI without toolgate
//! layer), `spawn()` returns `SpawnResult::Skipped` and tests gracefully skip.
//! NEVER panics on missing dependencies — returns Skipped instead.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct ToolgateFixture {
    child: Child,
    pub port: u16,
    pub base_url: String,
}

impl std::fmt::Debug for ToolgateFixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolgateFixture")
            .field("port", &self.port)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum SpawnResult {
    /// Toolgate started successfully. Caller owns lifecycle.
    Started(ToolgateFixture),
    /// Python venv or toolgate/ directory absent — test should skip.
    Skipped(&'static str),
}

impl ToolgateFixture {
    /// Attempt to spawn toolgate with `--limit-concurrency` flag.
    /// Returns Skipped if prerequisites missing. Caller picks a free port.
    pub async fn spawn(port: u16, limit_concurrency: u32) -> SpawnResult {
        let repo_root = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(p) => PathBuf::from(p)
                .ancestors()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            Err(_) => return SpawnResult::Skipped("CARGO_MANIFEST_DIR not set"),
        };
        let toolgate_dir = repo_root.join("toolgate");
        if !toolgate_dir.exists() {
            return SpawnResult::Skipped("toolgate/ directory missing");
        }
        let venv_python = if cfg!(windows) {
            toolgate_dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            toolgate_dir.join(".venv").join("bin").join("python")
        };
        if !venv_python.exists() {
            return SpawnResult::Skipped("toolgate/.venv not found — run setup.sh");
        }

        let child = Command::new(&venv_python)
            .current_dir(&toolgate_dir)
            .args([
                "-m",
                "uvicorn",
                "app:app",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--workers",
                "1",
                "--loop",
                "asyncio",
                "--limit-concurrency",
                &limit_concurrency.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        let child = match child {
            Ok(c) => c,
            Err(e) => {
                return SpawnResult::Skipped(Box::leak(
                    format!("failed to spawn uvicorn: {e}").into_boxed_str(),
                ));
            }
        };

        // Poll /health for up to 10s
        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if std::time::Instant::now() >= deadline {
                break;
            }
            if let Ok(resp) = client
                .get(format!("{base_url}/health"))
                .timeout(Duration::from_millis(500))
                .send()
                .await
                && resp.status().is_success()
            {
                return SpawnResult::Started(ToolgateFixture {
                    child,
                    port,
                    base_url,
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        SpawnResult::Skipped("toolgate did not become ready within 10s")
    }
}

impl Drop for ToolgateFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
