use std::process::Output;
use std::time::Duration;
use tokio::process::Command;

/// Default hard timeout for watchdog subprocess probes.
pub const DEFAULT_PROC_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a command to completion with a hard timeout. On timeout the child is
/// killed (`kill_on_drop`) and `None` is returned; a spawn error also yields
/// `None`. Prevents a hung subprocess (e.g. `docker ps` against a wedged
/// daemon) from stalling the single-threaded watchdog loop (F003).
pub async fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<Output> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(o)) => Some(o),
        Ok(Err(_)) => None,
        Err(_) => None, // timed out — future dropped, child killed on drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slow_command_times_out_and_returns_none() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let out = output_with_timeout(&mut cmd, Duration::from_millis(500)).await;
        assert!(out.is_none(), "expected timeout to yield None");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not block for the full sleep duration"
        );
    }

    #[tokio::test]
    async fn fast_command_returns_output() {
        let mut cmd = Command::new("true");
        let out = output_with_timeout(&mut cmd, Duration::from_secs(5)).await;
        assert!(out.is_some_and(|o| o.status.success()));
    }
}
