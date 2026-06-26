//! Host subprocess transport for language servers.

use std::path::Path;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[allow(dead_code)]
pub async fn spawn_server(
    command: &[String],
    cwd: &Path,
) -> anyhow::Result<(Child, ChildStdout, ChildStdin)> {
    let (bin, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!("spawn {bin}: {e} (is the language server installed on PATH?)")
        })?;
    let out = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let inp = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    Ok((child, out, inp))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn spawns_and_pipes_stdio() {
        let (mut child, mut out, mut inp) =
            spawn_server(&["cat".into()], std::path::Path::new("."))
                .await
                .unwrap();
        inp.write_all(b"hi").await.unwrap();
        inp.shutdown().await.unwrap();
        let mut s = String::new();
        out.read_to_string(&mut s).await.unwrap();
        assert_eq!(s, "hi");
        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn missing_binary_errors() {
        assert!(
            spawn_server(
                &["definitely-not-a-real-bin-xyz".into()],
                std::path::Path::new(".")
            )
            .await
            .is_err()
        );
    }
}
