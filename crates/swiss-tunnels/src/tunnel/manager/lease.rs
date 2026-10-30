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

//! One operation's reference on an SSH client (SPEC §remote.transport), and the remote
//! capability's way to take one together with the client.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::tunnel::types::{FailureKind, TunnelError};
use super::client::SshLike;
use super::TunnelManager;

/// One operation's reference on an SSH client, released when the operation ends
/// (SPEC §remote.transport).
///
/// Born as the shell session's guard (SPEC §terminal.remote) and generalized when remote exec and
/// SFTP needed the same discipline: whatever the operation — a rule, a PTY, an exec, a
/// file transfer — it counts its reference through the ONE path (`put_ref`) that knows
/// "the last one out ends the client". Dropping the lease is the release; the task that
/// owns it drops it when the work is over. `ShellSessionGuard` remains as an alias so
/// existing imports keep their meaning.
pub struct ConnectionLease {
    pub(super) manager: Arc<TunnelManager>,
    pub(super) conn: Arc<dyn SshLike>,
}

/// The pre-remote name of [ConnectionLease], kept so the shell-side vocabulary stays
/// readable where only shells are meant.
pub type ShellSessionGuard = ConnectionLease;

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let conn = self.conn.clone();
        // Drop cannot await, and the release ends the client when this was the last
        // reference — a network round trip. Hand it to the runtime instead.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { manager.put_ref(&conn).await });
            }
            // Only reachable if a guard outlives the runtime during shutdown. Return the
            // count anyway so the ledger stays honest; the transport dies with the
            // runtime regardless, and panicking here would abort the process.
            Err(_) => {
                conn.refs().fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

impl TunnelManager {
    /// Dial (or reuse) one connection and hand back the client TOGETHER with its
    /// operation-scoped lease (SPEC §remote.transport): the remote capability's entry point, used
    /// by exec and by every SFTP operation. The caller moves the lease into whatever
    /// owns the operation — the exec future or the file reader/writer — so the reference
    /// lives exactly as long as the work does.
    pub async fn lease_connection(
        self: &Arc<Self>,
        conn_id: &str,
    ) -> Result<(Arc<dyn SshLike>, ConnectionLease), TunnelError> {
        let Some(def) = self.with_store(|s| s.connection(conn_id)) else {
            return Err(TunnelError::new(
                format!("unknown ssh connection: {conn_id}"),
                FailureKind::Config,
            ));
        };
        let c = self.dial(&def).await?;
        c.refs().fetch_add(1, Ordering::SeqCst);
        Ok((
            c.clone(),
            ConnectionLease {
                manager: self.clone(),
                conn: c,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration as StdDuration;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, echo_server, eventually, free_port, manager, nth, rule_input, scratch,
    };
    use crate::tunnel::sshtest;
    use crate::tunnel::store::{ConnInput, TunnelStore};
    use crate::tunnel::types::{AuthType, SshConnDef};
    use swiss_host::services::action::CancelSource;
    use swiss_host::services::remote::{
        RemoteError, RemoteExecEvent, RemoteExecRequest, RemoteExecResult,
        RemoteTransportProvider,
    };

    // --- remote execution leases + provider (SPEC §remote) --------------------------------------

    /// A minimal request helper: argv only, the shape every lease test needs.
    fn exec_req(argv: &[&str]) -> RemoteExecRequest {
        RemoteExecRequest {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            cwd: None,
            stdin: None,
        }
    }

    fn collect(mut rx: tokio::sync::mpsc::Receiver<RemoteExecEvent>) -> (String, String) {
        let (mut out, mut err) = (String::new(), String::new());
        loop {
            match rx.try_recv() {
                Ok(RemoteExecEvent::Stdout(b)) => out.push_str(&String::from_utf8_lossy(&b)),
                Ok(RemoteExecEvent::Stderr(b)) => err.push_str(&String::from_utf8_lossy(&b)),
                Err(_) => break, // empty or the sender side dropped: both mean "done"
            }
        }
        (out, err)
    }

    /// Real-transport helper: store + connection row against a spawned test server port.
    fn real_store_with(dir: &Path, port: u16) -> (Arc<Mutex<TunnelStore>>, SshConnDef) {
        swiss_core::secure::key::use_test_master_key();
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        let input = ConnInput {
            name: "srv".into(),
            host: "127.0.0.1".into(),
            port: port as f64,
            username: "deploy".into(),
            auth_type: AuthType::Password,
            password: Some("pw".into()),
            ..Default::default()
        };
        let conn = store.lock().unwrap().add_connection(&input).expect("add");
        (store, conn)
    }

    #[tokio::test]
    async fn a_remote_exec_shares_the_rule_client_and_releases_its_lease() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        let echo = echo_server().await;
        let rule = add_rule(&store, rule_input("r1", &conn.id, free_port().await, echo));
        m.start_rule(&rule.id).await.expect("starts");
        let refs_with_rule = nth(&built, 0).ref_count();

        // The provider leases while the exec runs and gives the reference back.
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        *nth(&built, 0).exec_output.lock().unwrap() = b"hello\nworld\n".to_vec();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let result = provider
            .exec(
                &conn.id,
                "remote.exec",
                exec_req(&["make"]),
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("execs");
        assert_eq!(result.exit_code, 0);
        let (out, _) = collect(rx);
        assert_eq!(out, "hello\nworld\n");
        assert_eq!(nth(&built, 0).execs.load(Ordering::SeqCst), 1);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == refs_with_rule).await,
            "the lease returned to the rule's reference count"
        );
        assert_eq!(
            nth(&built, 0).dials(),
            1,
            "no second dial: the client is shared"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_result_not_an_error() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        // No rule here: a setup lease dials the client and holds it while the exec
        // rides the same connection, which is the interesting half of the contract.
        let (_client, lease) = m.lease_connection(&conn.id).await.expect("dial");
        nth(&built, 0).exec_exit.store(7, Ordering::SeqCst);
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let result = provider
            .exec(
                &conn.id,
                "remote.exec",
                exec_req(&["false"]),
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("a nonzero exit is still a completed exec");
        assert_eq!(result.exit_code, 7);
        // The lease release is a spawned task (Drop cannot await): poll for it.
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 1).await,
            "back to the setup lease only"
        );
        drop(lease);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 0).await,
            "no leaked lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_canceled_exec_stops_and_releases_the_lease() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        let (_client, lease) = m.lease_connection(&conn.id).await.expect("dial");
        // Enough output chunks that the cancel lands mid-stream.
        *nth(&built, 0).exec_output.lock().unwrap() = vec![b'x'; 7 * 200];
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        let source = CancelSource::new();
        let (tx, rx) = tokio::sync::mpsc::channel(4); // small: the pump parks, cancel wins
        let handle = source.handle();
        let exec = provider.exec(
            &conn.id,
            "remote.exec",
            exec_req(&["long-build"]),
            tx,
            handle,
        );
        // Cancel while the first chunks are still in flight: the fake checks the
        // cancel flag between every chunk, so a mid-stream cancel is deterministic.
        tokio::time::sleep(StdDuration::from_millis(5)).await;
        source.cancel();
        match exec.await {
            Err(RemoteError::Canceled(m)) => assert!(m.contains("canceled"), "{m}"),
            Ok(_) => panic!("canceled, not a result"),
            Err(other) => panic!("wrong error: {other}"),
        }
        drop(rx);
        // The exec's own reference is back even though the command "never finished";
        // the setup lease then unwinds the client itself.
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 1).await,
            "canceled released the exec lease"
        );
        drop(lease);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 0 && nth(&built, 0).ended() == 1).await,
            "the client unwound after its last lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_exec_on_an_unknown_endpoint_is_named() {
        let (dir, store) = scratch();
        let (m, _) = manager(&store);
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        match provider
            .exec(
                "ghost",
                "remote.exec",
                exec_req(&["make"]),
                tx,
                CancelSource::new().handle(),
            )
            .await
        {
            Err(RemoteError::Unknown(m)) => assert!(m.contains("ghost"), "{m}"),
            Ok(_) => panic!("unknown endpoint, got a result"),
            Err(other) => panic!("wrong error: {other}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn file_operations_round_trip_and_release_their_leases() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        let (_client, lease) = m.lease_connection(&conn.id).await.expect("dial");
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        provider
            .mkdir_p(&conn.id, "sync", "/data/ws/proj/src")
            .await
            .expect("mkdir");
        let mut writer = provider
            .create(&conn.id, "sync", "/data/ws/proj/src/main.c")
            .await
            .expect("create");
        // The lease is held WHILE the writer is open: refs on the client are >= 2
        // (the setup lease plus the writer's own).
        assert!(
            nth(&built, 0).ref_count() >= 2,
            "an open file holds its lease on top of the setup lease"
        );
        writer.write_chunk(b"int main(){}\n").await.expect("write");
        writer.finish().await.expect("finish");
        drop(writer);
        let stat = provider
            .stat(&conn.id, "sync", "/data/ws/proj/src/main.c")
            .await
            .expect("stat")
            .expect("present");
        assert_eq!(stat.size, 13, "the written byte count");
        let mut reader = provider
            .open_read(&conn.id, "pull", "/data/ws/proj/src/main.c")
            .await
            .expect("read");
        let mut back = Vec::new();
        while let Some(chunk) = reader.read_chunk().await.expect("chunk") {
            back.extend_from_slice(&chunk);
        }
        drop(reader);
        assert_eq!(back, b"int main(){}\n");
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 1).await,
            "every file lease returned to the setup lease's count"
        );
        drop(lease);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 0).await,
            "the client unwound after its last lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The full russh stack against the fake exec server (sshtest): connect, session
    /// channel, exec request, streaming Data/ExtendedData, exit status.
    #[tokio::test]
    async fn the_real_client_execs_against_a_real_russh_server() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-ssh-exec-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        let port = sshtest::spawn_exec_ssh_server(sshtest::ExecScript {
            stdout: b"hello\nworld\n".to_vec(),
            stderr: b"warn".to_vec(),
            exit: Some(0),
            stdin: sshtest::StdinMode::Ignore,
        })
        .await;
        let (store, conn) = real_store_with(&dir, port);
        let m = TunnelManager::new(store.clone(), None);
        let provider = crate::tunnel::remote::TunnelRemote::new(m);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let result = provider
            .exec(
                &conn.id,
                "remote.exec",
                RemoteExecRequest {
                    argv: vec!["echo".into(), "one two".into(), "it's".into()],
                    env: vec![("BOARD".into(), "a;b".into())],
                    cwd: Some("/tmp/x y".into()),
                    stdin: None,
                },
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("exec over the real transport");
        assert_eq!(result.exit_code, 0);
        let (stdout, stderr) = collect(rx);
        assert_eq!(stdout, "hello\nworld\n");
        assert_eq!(stderr, "warn");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancellation against the real transport: the server stalls between its output
    /// and the exit status; the client closes the channel and reports canceled.
    #[tokio::test]
    async fn the_real_client_cancels_a_stalled_exec() {
        let dir =
            std::env::temp_dir().join(format!("swiss-ssh-cxl-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        // A wedged command: output, then nothing — no exit status, no close. The
        // only ways out are the client's cancel or its deadline.
        let port = sshtest::spawn_exec_ssh_server(sshtest::ExecScript {
            stdout: b"partial".to_vec(),
            stderr: Vec::new(),
            exit: None,
            stdin: sshtest::StdinMode::Ignore,
        })
        .await;
        let (store, conn) = real_store_with(&dir, port);
        let m = TunnelManager::new(store.clone(), None);
        let provider = crate::tunnel::remote::TunnelRemote::new(m);
        let source = CancelSource::new();
        let handle = source.handle();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        // SPAWN the exec: an unpinned future is never polled, and on the current_thread
        // runtime only a spawned task can stream while the test waits for the first
        // bytes.
        let exec = tokio::spawn(async move {
            provider
                .exec(&conn.id, "remote.exec", exec_req(&["yocto"]), tx, handle)
                .await
        });
        // Park until the first stdout arrives, then cancel into the stall.
        let streamed = loop {
            match rx.recv().await {
                Some(RemoteExecEvent::Stdout(b)) if !b.is_empty() => break true,
                Some(_) => continue,
                None => break false,
            }
        };
        // Cancel only matters while the exec still runs; if it already ended, the
        // await below returns immediately and the assert names what it returned.
        if streamed {
            source.cancel();
        }
        let outcome = exec.await.expect("the exec task finished");
        assert!(
            streamed,
            "the stalled exec streamed its output first; it finished instead: {outcome:?}"
        );
        match outcome {
            Err(RemoteError::Canceled(_)) => {}
            Ok(_) => panic!("a stalled exec must not finish"),
            Err(other) => panic!("wrong error: {other}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One exec against a command that reads its stdin (a `cat` or a `wc -c`, per the
    /// mode), the events drained WHILE it runs: an echo is as big as the input, so
    /// collecting only after the end would park the exec on a full queue.
    async fn exec_reading_stdin(
        mode: sshtest::StdinMode,
        stdin: Option<String>,
    ) -> (Vec<u8>, Result<RemoteExecResult, RemoteError>) {
        let dir =
            std::env::temp_dir().join(format!("swiss-ssh-cat-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let port = sshtest::spawn_exec_ssh_server(sshtest::ExecScript {
            stdin: mode,
            ..Default::default()
        })
        .await;
        let (store, conn) = real_store_with(&dir, port);
        let m = TunnelManager::new(store.clone(), None);
        let provider = crate::tunnel::remote::TunnelRemote::new(m);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut request = exec_req(&["cat"]);
        request.stdin = stdin;
        let exec = tokio::spawn(async move {
            provider
                .exec(&conn.id, "remote.exec", request, tx, CancelSource::new().handle())
                .await
        });
        let mut out = Vec::new();
        while let Some(event) = rx.recv().await {
            if let RemoteExecEvent::Stdout(bytes) = event {
                out.extend_from_slice(&bytes);
            }
        }
        let result = exec.await.expect("the exec task finished");
        let _ = std::fs::remove_dir_all(&dir);
        (out, result)
    }

    /// A command that reads stdin sees it end (SPEC §remote.transport). An exec used to leave
    /// stdin open, so `cat`, a `psql` without -c or a `read` sat until the run's deadline -
    /// two hours by default.
    #[tokio::test]
    async fn an_exec_without_stdin_closes_it_at_once() {
        let (out, result) =
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                exec_reading_stdin(sshtest::StdinMode::Echo, None),
            )
                .await
                .expect("the command saw EOF on its stdin and ended");
        assert_eq!(result.expect("a clean exit").exit_code, 0);
        assert!(out.is_empty(), "nothing was sent, nothing comes back: {out:?}");
    }

    /// Stdin text reaches the command byte for byte, quotes and all: nothing quotes it,
    /// and its end is the command's EOF.
    #[tokio::test]
    async fn stdin_reaches_the_command_verbatim_then_ends() {
        let sql = "SELECT username FROM users WHERE id = '1gn6daai3jng5ddas3uyxctkmy';\n";
        let (out, result) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            exec_reading_stdin(sshtest::StdinMode::Echo, Some(sql.to_string())),
        )
            .await
            .expect("the command got its stdin and its EOF");
        assert_eq!(result.expect("a clean exit").exit_code, 0);
        assert_eq!(String::from_utf8_lossy(&out), sql);
    }

    /// A stdin larger than the SSH window arrives whole and in order before its EOF: the
    /// feed waits out window after window while the exec keeps reading. The command
    /// counts what it got, like `wc -c` - an echo this size wedges the russh test pair
    /// itself (sshtest StdinMode::Count says why).
    #[tokio::test]
    async fn a_stdin_larger_than_the_window_arrives_whole() {
        let line = "0123456789abcdef'\"$(x)\n";
        let text = line.repeat((4 << 20) / line.len());
        let (out, result) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            exec_reading_stdin(sshtest::StdinMode::Count, Some(text.clone())),
        )
            .await
            .expect("a stdin of several windows finished");
        assert_eq!(result.expect("a clean exit").exit_code, 0);
        assert_eq!(String::from_utf8_lossy(&out), format!("{}\n", text.len()));
    }
}
