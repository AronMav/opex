//! Pipeline step: sandbox — code_exec tool execution (migrated from engine_sandbox.rs).
//!
//! All functions take explicit dependencies instead of `&self` on `AgentEngine`.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::engine::BgProcess;
use crate::containers::sandbox::CodeSandbox;

// ── Code execution (sandbox or host) ────────────────────────────────────────

/// Top-level code_exec dispatcher: runs in Docker sandbox, or falls back to
/// host execution for privileged agents without a sandbox.
/// Snapshots workspace before/after to emit `__file__:` markers for any
/// files the script creates or modifies.
pub async fn handle_code_exec(
    args: &serde_json::Value,
    agent_name: &str,
    is_base: bool,
    sandbox: &Option<Arc<CodeSandbox>>,
    workspace_dir: &str,
    secrets: &crate::secrets::SecretsManager,
    ttl_secs: u64,
) -> String {
    let code = args.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if code.is_empty() {
        return "Error: 'code' is required".to_string();
    }
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("python");
    let packages: Vec<String> = args
        .get("packages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    // Snapshot BEFORE so we can diff after.
    let workspace_path = std::path::Path::new(workspace_dir);
    let before = crate::agent::pipeline::artifact_hook::snapshot(workspace_path);

    // Run the sandbox / host fallback path.
    let host_fn = execute_host_code;
    let outcome: Result<crate::containers::sandbox::ExecResult, anyhow::Error> =
        if is_base && sandbox.is_none() {
            let host_out = host_fn(code, language, &packages).await;
            Ok(crate::containers::sandbox::ExecResult {
                stdout: host_out,
                stderr: String::new(),
                exit_code: 0,
            })
        } else {
            let sb = match sandbox {
                Some(s) => s.clone(),
                None => return "Error: Docker sandbox unavailable.".to_string(),
            };
            let host_path = std::fs::canonicalize(workspace_dir)
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            sb.execute(agent_name, code, language, &packages, &host_path, is_base).await
        };

    // Snapshot AFTER (always, even on Err — script may have written partial files).
    let after = crate::agent::pipeline::artifact_hook::snapshot(workspace_path);
    let changes = crate::agent::pipeline::artifact_hook::diff(&before, &after);

    let key = secrets.get_upload_hmac_key();
    let format_markers =
        |changes: &[crate::agent::pipeline::artifact_hook::ArtifactChange]| -> String {
            let mut s = String::new();
            for change in changes {
                let rel = change.rel_path.to_string_lossy();
                let url = crate::uploads::mint_workspace_file_url(&rel, &key, ttl_secs);
                let mime = crate::uploads::guess_mime_from_extension(&rel);
                let json =
                    serde_json::json!({"url": url, "mediaType": mime}).to_string();
                s.push_str(&format!("\n{}{}", crate::agent::engine::FILE_PREFIX, json));
            }
            s
        };

    match outcome {
        Ok(result) => {
            let mut out = result.stdout;
            if !result.stderr.is_empty() {
                out.push_str("\n--- stderr ---\n");
                out.push_str(&result.stderr);
            }
            if out.is_empty() {
                out = format!("Exit code: {}", result.exit_code);
            }
            out.push_str(&format_markers(&changes));
            out
        }
        Err(e) => {
            let mut out = format!("Error: {}", e);
            out.push_str(&format_markers(&changes));
            out
        }
    }
}

// ── Host code execution (base agents only) ──────────────────────────────────

/// Execute code directly on host (base agents only, no Docker sandbox).
/// Runs in the hydeclaw working directory with full host access.
async fn execute_host_code(code: &str, language: &str, packages: &[String]) -> String {
    use tokio::process::Command;

    let timeout = std::time::Duration::from_secs(120);

    // Install packages if requested (avoid shell to prevent command injection via package names)
    if !packages.is_empty() && language == "python" {
        let valid = packages
            .iter()
            .all(|p| p.chars().all(|c| c.is_alphanumeric() || "-_.[]<>=!,".contains(c)));
        if !valid {
            return "Error: invalid characters in package name".to_string();
        }
        let mut cmd = Command::new("pip");
        cmd.args(["install", "-q"]);
        for p in packages {
            cmd.arg(p);
        }
        let _ = cmd.output().await;
    }

    let (cmd, args) = match language {
        "python" => ("python3", vec!["-c".to_string(), code.to_string()]),
        "bash" | "sh" => ("bash", vec!["-c".to_string(), code.to_string()]),
        _ => {
            return format!(
                "Error: unsupported language '{}' for host execution",
                language
            )
        }
    };

    match tokio::time::timeout(timeout, Command::new(cmd).args(&args).output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let mut result = stdout;
            if !stderr.is_empty() {
                result.push_str("\n--- stderr ---\n");
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = format!("Exit code: {}", output.status.code().unwrap_or(-1));
            }
            // Truncate to prevent LLM context overflow
            if result.len() > 16000 {
                result.truncate(16000);
                result.push_str("\n... (truncated)");
            }
            result
        }
        Ok(Err(e)) => format!("Error executing on host: {}", e),
        Err(_) => "Error: host execution timed out (120s)".to_string(),
    }
}

// ── Background process tools (base agents only) ─────────────────────────────

/// Start a background process, returning a handle ID.
pub async fn handle_process_start(
    args: &serde_json::Value,
    agent_name: &str,
    bg_processes: &Arc<Mutex<HashMap<String, BgProcess>>>,
) -> String {
    use rand::Rng;
    use tokio::process::Command;

    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => return "Error: 'command' is required".to_string(),
    };

    let process_id = format!("{:08x}", rand::rng().random::<u32>());
    let log_dir = format!("/tmp/hydeclaw-bg/{}", agent_name);
    let log_path = format!("{}/{}.log", log_dir, process_id);

    if let Err(e) = tokio::fs::create_dir_all(&log_dir).await {
        return format!("Error creating log dir: {}", e);
    }

    let log_file = match tokio::fs::File::create(&log_path).await {
        Ok(f) => f,
        Err(e) => return format!("Error creating log file: {}", e),
    };
    let log_file_std = log_file.into_std().await;

    let mut cmd = Command::new("bash");
    cmd.args(["-c", &command]);
    if let Some(wd) = args
        .get("working_directory")
        .and_then(|v| v.as_str())
        .filter(|d| !d.is_empty())
    {
        cmd.current_dir(wd);
    }
    let mut child = match cmd
        .stdout(std::process::Stdio::from(
            log_file_std.try_clone().expect("clone stdout"),
        ))
        .stderr(std::process::Stdio::from(log_file_std))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("Error spawning process: {}", e),
    };

    let pid = child.id();

    {
        let mut procs = bg_processes.lock().await;
        procs.insert(
            process_id.clone(),
            BgProcess {
                process_id: process_id.clone(),
                command: command.clone(),
                log_path: log_path.clone(),
                pid,
                started_at: std::time::Instant::now(),
            },
        );
    }

    // Detach: wait in background so the child doesn't become a zombie
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    format!(
        "Started background process.\nprocess_id: {}\nlog: {}\ncommand: {}",
        process_id, log_path, command
    )
}

/// Query status of a background process (cleans up finished ones).
pub async fn handle_process_status(
    args: &serde_json::Value,
    bg_processes: &Arc<Mutex<HashMap<String, BgProcess>>>,
) -> String {
    // Clean up finished processes on access
    {
        let mut procs = bg_processes.lock().await;
        procs.retain(|_id, p| {
            p.pid
                .is_some_and(|pid| std::path::Path::new(&format!("/proc/{}", pid)).exists())
        });
    }

    let process_id = match args.get("process_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return "Error: 'process_id' is required".to_string(),
    };

    let (pid, log_path) = {
        let procs = bg_processes.lock().await;
        match procs.get(&process_id) {
            Some(p) => (p.pid, p.log_path.clone()),
            None => return format!("Error: process '{}' not found", process_id),
        }
    };

    let running = if let Some(pid) = pid {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    } else {
        false
    };

    let log_content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    let lines: Vec<&str> = log_content.lines().collect();
    let tail: Vec<&str> = lines.iter().rev().take(20).copied().collect();
    let tail_str = tail.into_iter().rev().collect::<Vec<_>>().join("\n");

    format!(
        "process_id: {}\nstatus: {}\n\n--- last 20 log lines ---\n{}",
        process_id,
        if running { "running" } else { "done" },
        tail_str
    )
}

/// Read log output from a background process.
pub async fn handle_process_logs(
    args: &serde_json::Value,
    bg_processes: &Arc<Mutex<HashMap<String, BgProcess>>>,
) -> String {
    let process_id = match args.get("process_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return "Error: 'process_id' is required".to_string(),
    };
    let tail_lines = args
        .get("tail_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;

    let log_path = {
        let procs = bg_processes.lock().await;
        match procs.get(&process_id) {
            Some(p) => p.log_path.clone(),
            None => return format!("Error: process '{}' not found", process_id),
        }
    };

    let log_content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    let lines: Vec<&str> = log_content.lines().collect();
    let tail: Vec<&str> = lines.iter().rev().take(tail_lines).copied().collect();
    let tail_str = tail.into_iter().rev().collect::<Vec<_>>().join("\n");

    format!(
        "process_id: {}\n--- last {} lines ---\n{}",
        process_id, tail_lines, tail_str
    )
}

/// Kill a background process by sending SIGTERM.
pub async fn handle_process_kill(
    args: &serde_json::Value,
    bg_processes: &Arc<Mutex<HashMap<String, BgProcess>>>,
) -> String {
    use tokio::process::Command;

    let process_id = match args.get("process_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return "Error: 'process_id' is required".to_string(),
    };

    let pid = {
        let procs = bg_processes.lock().await;
        match procs.get(&process_id) {
            Some(p) => p.pid,
            None => return format!("Error: process '{}' not found", process_id),
        }
    };

    match pid {
        Some(pid) => {
            let result = Command::new("kill").arg(pid.to_string()).output().await;
            match result {
                Ok(_) => format!("Sent SIGTERM to process {} (pid {})", process_id, pid),
                Err(e) => format!("Error killing process: {}", e),
            }
        }
        None => format!("Error: process '{}' has no known PID", process_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_secrets() -> crate::secrets::SecretsManager {
        crate::secrets::SecretsManager::new_noop()
    }

    fn write_file(dir: &std::path::Path, rel: &str, content: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
    }

    /// Smoke test that the marker formatting produces one __file__: per change.
    /// Bypasses the sandbox by invoking the underlying snapshot/diff +
    /// asserting the marker construction.
    #[tokio::test]
    async fn format_markers_produces_one_per_change() {
        use crate::agent::pipeline::artifact_hook::{snapshot, diff};

        let dir = tempfile::tempdir().unwrap();
        let before = snapshot(dir.path());
        write_file(dir.path(), "out.csv", b"data");
        write_file(dir.path(), "chart.png", b"\x89PNG");
        let after = snapshot(dir.path());
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 2);

        let secrets = test_secrets();
        let key = secrets.get_upload_hmac_key();
        let mut markers = String::new();
        for change in &changes {
            let rel = change.rel_path.to_string_lossy();
            let url = crate::uploads::mint_workspace_file_url(&rel, &key, 3600);
            let mime = crate::uploads::guess_mime_from_extension(&rel);
            let json = serde_json::json!({"url": url, "mediaType": mime}).to_string();
            markers.push_str(&format!("
{}{}", crate::agent::engine::FILE_PREFIX, json));
        }
        assert!(markers.contains("/workspace-files/out.csv?sig="));
        assert!(markers.contains("/workspace-files/chart.png?sig="));
        assert!(markers.contains("\"mediaType\":\"text/csv\""));
        assert!(markers.contains("\"mediaType\":\"image/png\""));
    }
}
