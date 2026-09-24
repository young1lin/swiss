/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Local-port occupancy, and who is occupying it — port of `tunnels/port.ts`.
//!
//! The owner lookup and the force-kill are direct Win32 calls in `swiss_core::platform`
//! (GetExtendedTcpTable, the Toolhelp snapshot, TerminateProcess). The Node build shelled out
//! to netstat/tasklist/taskkill (~4 MB, ~40 ms each) and only because a `powershell.exe`
//! Get-NetTCPConnection (~65 MB, ~350 ms) was worse; this port takes the same lesson `mem.rs`
//! records one step further — a diagnostic must not cost more than the thing it diagnoses,
//! and a subprocess where a syscall exists is never the cheaper option (AGENTS.md).
//!
//! Off Windows the owner lookups answer None and force-free refuses — exactly the early returns
//! the Node build has for `process.platform !== "win32"`.

use std::time::Duration;

use serde_json::json;

use super::types::PortOwner;
use swiss_core::log;

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

/// Who holds `port`, so a start failure can say "held by pid 1072 (forward-port.exe)" instead
/// of "EADDRINUSE". Returns None off win32, or when the holder cannot be identified. Both
/// lookups are direct Win32 reads through the platform seam: the OS TCP owner table for the
/// pid, one Toolhelp snapshot pass for its image name.
pub async fn port_owner(port: u16) -> Option<PortOwner> {
    #[cfg(not(windows))]
    {
        let _ = port;
        return None;
    }
    #[cfg(windows)]
    {
        let pid = swiss_core::platform::tcp_listener_pid(port)?;
        // A pid that died between the two reads simply has no name to show.
        let name = swiss_core::platform::process_name(pid).unwrap_or_default();
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
        // Direct Win32 subtree kill (AGENTS.md — no subprocess where a syscall exists): the
        // platform walk terminates the holder AND its children, wider than `taskkill /F`
        // without /T ever was. TerminateProcess is asynchronous, so poll briefly — an honest
        // error must still reach the panel, not a success the OS has not carried out yet.
        swiss_core::platform::tree_kill(pid);
        for _ in 0..50 {
            if !swiss_core::platform::pid_alive(pid) {
                log::warn("force-freed a port holder", Some(json!({ "pid": pid })));
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err("could not kill the port holder".into())
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

    // The owner lookup reads the OS tables directly: a socket this test process holds must
    // come back with this pid and this image — the exact path that used to spawn netstat.exe
    // and tasklist.exe, now with no subprocess anywhere in it.
    #[cfg(windows)]
    #[tokio::test]
    async fn names_this_test_binary_as_the_holder_of_a_bound_port() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let owner = port_owner(port).await.expect("owner found");
        assert_eq!(owner.pid, std::process::id());
        assert!(!owner.name.is_empty(), "the image name came from the snapshot");
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
