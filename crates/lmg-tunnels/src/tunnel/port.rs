//! Local-port occupancy, and who is occupying it — port of `tunnels/port.ts`.
//!
//! This is the only module here that spawns anything, and it deliberately uses netstat/tasklist
//! (~4 MB, ~40 ms) rather than a `powershell.exe` Get-NetTCPConnection (~65 MB, ~350 ms). The
//! same lesson `mem.rs` records: a diagnostic must not cost more than the thing it diagnoses.
//!
//! Off Windows the owner lookups answer None and force-free refuses — exactly the early returns
//! the Node build has for `process.platform !== "win32"`.

use std::time::Duration;

use serde_json::json;

use super::types::PortOwner;
use lmg_core::log;

/// Hide the console window of any diagnostic we spawn (Node's `windowsHide: true`).
#[cfg(windows)]
fn hide_window(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    // 0x08000000 = CREATE_NO_WINDOW
    cmd.creation_flags(0x0800_0000)
}
#[cfg(not(windows))]
fn hide_window(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd
}

/// True when nothing holds the port, tested by actually binding it.
pub async fn probe_port(port: u16, host: &str) -> bool {
    tokio::net::TcpListener::bind((host, port)).await.is_ok()
}

/// Wait until a port can be bound again, up to `budget_ms`.
///
/// A rule that stops and immediately restarts otherwise races its own release: the listener is
/// closed but the OS has not finished tearing down its accepted sockets, and the retry fails
/// with EADDRINUSE against nothing.
pub async fn wait_for_release(port: u16, host: &str) -> bool {
    let mut budget = Duration::from_millis(2000);
    let step = Duration::from_millis(50);
    loop {
        if probe_port(port, host).await {
            return true;
        }
        if budget.is_zero() {
            return false;
        }
        let sleep = budget.min(step);
        tokio::time::sleep(sleep).await;
        budget = budget.saturating_sub(sleep);
    }
}

/// Run a diagnostic command and take its stdout (empty on failure), bounded by a timeout —
/// Node's execFile with `windowsHide` + `timeout`.
async fn run(cmd: &str, args: &[&str], timeout: Duration) -> String {
    let mut command = tokio::process::Command::new(cmd);
    command.args(args);
    hide_window(&mut command);
    let out = tokio::time::timeout(timeout, async {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await
    })
    .await;
    match out {
        Ok(Ok(output)) if !output.stdout.is_empty() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => String::new(),
    }
}

/// The pid listening on `port`, from `netstat -ano`. Hand-rolled parsing (ADR-007): the
/// netstat table is whitespace columns, no regex needed.
async fn listener_pid(port: u16) -> Option<u32> {
    let out = run(
        "netstat.exe",
        &["-ano", "-p", "TCP"],
        Duration::from_secs(5),
    )
    .await;
    if out.is_empty() {
        return None;
    }
    for line in out.lines() {
        // "  TCP    127.0.0.1:6380   0.0.0.0:0   LISTENING   1072"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 || parts[3] != "LISTENING" {
            continue;
        }
        // Match on the port only after the last colon, so ::1:6380 and 127.0.0.1:6380 both work
        // and 16380 never matches 6380.
        let local = parts[1];
        let Some(colon) = local.rfind(':') else {
            continue;
        };
        if local[colon + 1..].parse::<u16>().ok() != Some(port) {
            continue;
        }
        if let Ok(pid) = parts[4].parse::<u32>() {
            if pid > 0 {
                return Some(pid);
            }
        }
    }
    None
}

/// The image name of a pid, from `tasklist` CSV output.
async fn process_name(pid: u32) -> String {
    let filter = format!("PID eq {pid}");
    let out = run(
        "tasklist.exe",
        &["/FI", &filter, "/FO", "CSV", "/NH"],
        Duration::from_secs(5),
    )
    .await;
    // The first CSV row is `\"name\",\"pid\",…' — take the first quoted field.
    for line in out.lines() {
        let line = line.trim();
        if !line.starts_with('"') {
            continue;
        }
        // Split on `","` the way the Node build did, then strip the leading quote.
        let first = line.split("\",\"").next().unwrap_or("");
        return first.trim_start_matches('"').to_string();
    }
    String::new()
}

/// Who holds `port`, so a start failure can say "held by pid 1072 (forward-port.exe)" instead
/// of "EADDRINUSE". Returns None off win32, or when the holder cannot be identified.
pub async fn port_owner(port: u16) -> Option<PortOwner> {
    #[cfg(not(windows))]
    {
        let _ = port;
        return None;
    }
    #[cfg(windows)]
    {
        let pid = listener_pid(port).await?;
        let name = process_name(pid).await;
        Some(PortOwner {
            pid,
            name: if name.is_empty() {
                format!("pid {pid}")
            } else {
                name
            },
        })
    }
}

/// Force-release a port by killing its holder. Only ever called after an explicit confirmation
/// in the panel — this can kill a process that is doing real work, so it is never automatic.
pub async fn force_free(pid: u32) -> Result<(), String> {
    if pid == 0 || pid == std::process::id() {
        return Err("refusing to kill the gateway itself".into());
    }
    #[cfg(not(windows))]
    {
        // The Node build had a process.kill() path for POSIX; without a libc dependency (and
        // with the owner lookup being Windows-only anyway), refuse instead of pretending.
        let _ = pid;
        return Err("force-free is only supported on Windows".into());
    }
    #[cfg(windows)]
    {
        let out = tokio::time::timeout(Duration::from_secs(10), async {
            let mut command = tokio::process::Command::new("taskkill.exe");
            command.args(["/PID", &pid.to_string(), "/F"]);
            hide_window(&mut command);
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
        })
        .await;
        match out {
            Ok(Ok(o)) if o.status.success() => {
                log::warn("force-freed a port holder", Some(json!({ "pid": pid })));
                Ok(())
            }
            Ok(Ok(o)) => {
                let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
                Err(if msg.is_empty() {
                    "taskkill failed".into()
                } else {
                    msg
                })
            }
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("taskkill timed out".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_detects_a_live_listener() {
        // A listener we hold makes the probe false; the moment it drops, true again.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!probe_port(port, "127.0.0.1").await);
        drop(listener);
        assert!(wait_for_release(port, "127.0.0.1").await);
    }

    #[tokio::test]
    async fn force_free_refuses_the_gateway_itself() {
        assert_eq!(
            force_free(std::process::id()).await.unwrap_err(),
            "refusing to kill the gateway itself"
        );
        assert_eq!(
            force_free(0).await.unwrap_err(),
            "refusing to kill the gateway itself"
        );
    }
}
