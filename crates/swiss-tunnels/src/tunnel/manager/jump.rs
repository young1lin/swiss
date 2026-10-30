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

//! Jump chains (SPEC §tunnels.jump): a connection dialed through its jump hop holds a reference
//! on the hop for as long as the chain lives.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::tunnel::forward::ByteStream;
use crate::tunnel::types::{FailureKind, TunnelError};
use super::client::SshLike;
use super::TunnelManager;

/// One live chain's reference on its jump hop (SPEC §tunnels.jump) — ShellSessionGuard's
/// discipline for the jump case, so a chained connection counts references through the
/// SAME path a rule and a shell do. It travels inside the connection's Live; dropping it
/// with that Live is the release.
struct JumpHold {
    manager: Arc<TunnelManager>,
    conn: Arc<dyn SshLike>,
}

impl Drop for JumpHold {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let conn = self.conn.clone();
        // Drop cannot await, and the release ends the client when this was the last
        // reference — a network round trip. Hand it to the runtime instead.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { manager.put_ref(&conn).await });
            }
            // Only reachable if a hold outlives the runtime during shutdown. Return the
            // count anyway so the ledger stays honest; the transport dies with the
            // runtime regardless, and panicking here would abort the process.
            Err(_) => {
                conn.refs().fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

impl TunnelManager {
    /// SPEC §tunnels.jump — one hop of a live chain. The jump's own connection is dialed
    /// (or reused) through the SAME connection table a rule start goes through —
    /// single-flight, and shared with any rules riding that hop — then a direct-tcpip
    /// channel to this connection's host:port is opened on it and handed back for the
    /// caller's SSH handshake. The returned hold is the reference the caller's Live
    /// keeps on the hop; dropping it (a deliberate end or the transport watcher)
    /// releases the hop through put_ref, exactly like a shell session's reference.
    pub(super) async fn dial_jump(
        self: &Arc<Self>,
        jump_id: &str,
        host: &str,
        port: u16,
    ) -> Result<(ByteStream, Box<dyn Send + 'static>), TunnelError> {
        let Some(def) = self.with_store(|s| s.connection(jump_id)) else {
            // Save-time validation keeps jumps pointing at real rows and deletion is
            // refused while a dependent exists, so this only fires on a hand-edited
            // file; reporting it as config keeps it out of the retry loop.
            return Err(TunnelError::new(
                format!("jump connection not found: {jump_id}"),
                FailureKind::Config,
            ));
        };
        let name = def.name.clone();
        // SPEC §tunnels.jump: the hop's failure keeps its OWN kind (auth and hostkey are never
        // retried) while the message names the hop. The pair is constructed directly —
        // the prefix must never flow through classify_message, whose substring scan
        // would wash the kind away. The inner detail is dropped on purpose: a hop's
        // host-key mismatch is recorded against the HOP by dial above; carrying it
        // outward would offer "trust this key" for the wrong connection.
        let via =
            |err: TunnelError| TunnelError::new(format!("via {name}: {}", err.message), err.kind);
        let hop = self.dial(&def).await.map_err(via)?;
        hop.refs().fetch_add(1, Ordering::SeqCst);
        match hop.open_channel(host.to_string(), port).await {
            Ok(stream) => Ok((
                stream,
                Box::new(JumpHold {
                    manager: self.clone(),
                    conn: hop.clone(),
                }),
            )),
            Err(err) => {
                self.put_ref(&hop).await;
                Err(via(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;
    use std::time::Duration;

    use serde_json::json;

    use crate::tunnel::manager::client::ConnFactory;
    use crate::tunnel::manager::test_util::{
        add_rule, echo_server, free_port, port_is_free, round_trip, rule_input, scratch, state_of,
        wait_until,
    };
    use crate::tunnel::manager::OpError;
    use crate::tunnel::ssh::{SshConnection, SshHooks};
    use crate::tunnel::sshtest;
    use crate::tunnel::store::{ConnInput, RuleInput, TunnelStore};
    use crate::tunnel::types::{AuthType, ConnState, SshConnDef};

    // --- jump chains (SPEC §tunnels.jump) -------------------------------------------------------------

    /// Every REAL client this manager built, newest last — the jump tests' window into
    /// the shared connection table (refcounts, live state) without opening the manager up.
    type RealBuilt = Arc<Mutex<Vec<Arc<SshConnection>>>>;

    /// A manager over real russh clients, recorded as they are built: the fake-client
    /// harness cannot prove SSH-over-SSH, so the §3.4 tests run the genuine stack against
    /// the in-process servers and only observe through this list.
    fn real_manager(store: &Arc<Mutex<TunnelStore>>) -> (Arc<TunnelManager>, RealBuilt) {
        let built: RealBuilt = Arc::new(Mutex::new(Vec::new()));
        let sink = built.clone();
        let factory: ConnFactory = Arc::new(move |def: &SshConnDef, hooks: SshHooks| {
            let conn = SshConnection::new(def.clone(), hooks);
            sink.lock().unwrap().push(conn.clone());
            conn as Arc<dyn SshLike>
        });
        (
            TunnelManager::with_connections(store.clone(), None, factory),
            built,
        )
    }

    fn real_conns_with(built: &RealBuilt, id: &str) -> Vec<Arc<SshConnection>> {
        built
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.id() == id)
            .cloned()
            .collect()
    }

    /// The refcount of the one client built for this id — the hold assertions' shape.
    fn refs_of(built: &RealBuilt, id: &str) -> i64 {
        let conns = real_conns_with(built, id);
        assert_eq!(conns.len(), 1, "exactly one client for {id}");
        conns[0].refs.load(Ordering::SeqCst)
    }

    fn add_conn_with(store: &Arc<Mutex<TunnelStore>>, input: ConnInput) -> SshConnDef {
        store
            .lock()
            .unwrap()
            .add_connection(&input)
            .expect("add the connection")
    }

    /// A password-auth def pointing at a test server — the servers accept any
    /// credentials, and password auth keeps these tests off the filesystem (key paths).
    fn jump_pw_conn(name: &str, host: &str, port: u16, jump: Option<&str>) -> ConnInput {
        ConnInput {
            name: name.into(),
            host: host.into(),
            port: port as f64,
            username: "u".into(),
            auth_type: AuthType::Password,
            password: Some("pw".into()),
            jump: jump.map(str::to_string),
            ..Default::default()
        }
    }

    /// SPEC §tunnels.jump: connection A dials the jump server S1, connection B rides A's
    /// direct-tcpip channel to reach the target server S2, and the rule's traffic flows
    /// through both — the full SSH-over-SSH path, pinned by the jump's live client
    /// (built, connected, held) and the echoed bytes.
    #[tokio::test]
    async fn a_jump_connection_rides_the_jump_servers_channel_end_to_end() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_ssh_server().await;
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("bastion", "127.0.0.1", s1, None));
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &b.id, port, echo));

        m.start_rule(&r.id).await.expect("the chained start");
        assert_eq!(state_of(&m, &r.id), "up");
        // The jump really carried the target: its shared client is live, and exactly one
        // reference is out — the target's hold on its hop.
        let a_clients = real_conns_with(&built, &a.id);
        assert_eq!(a_clients.len(), 1, "the jump dialed once");
        assert_eq!(a_clients[0].state(), ConnState::Connected);
        assert_eq!(a_clients[0].refs.load(Ordering::SeqCst), 1);
        assert_eq!(round_trip(port, "hi").await, "HI");
        m.close_all().await;
    }

    /// SPEC §tunnels.jump: a two-level chain proves both the recursion (each hop dials
    /// through the one below it) and the hold discipline — stopping the target unwinds
    /// the whole chain, leaving every hop's refcount at zero.
    #[tokio::test]
    async fn a_two_level_chain_releases_every_hop_when_the_target_stops() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_ssh_server().await;
        let s2 = sshtest::spawn_ssh_server().await;
        let s3 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("hop-a", "127.0.0.1", s1, None));
        let b = add_conn_with(&store, jump_pw_conn("hop-b", "127.0.0.1", s2, Some(&a.id)));
        let c = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s3, Some(&b.id)));
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("the chained start");
        assert_eq!(round_trip(port, "chain").await, "CHAIN");
        // One reference per hop: hop-b's session holds a, db's session holds hop-b, and
        // the rule holds db.
        assert_eq!(refs_of(&built, &a.id), 1, "held by hop-b's session");
        assert_eq!(refs_of(&built, &b.id), 1, "held by db's session");
        assert_eq!(refs_of(&built, &c.id), 1, "held by the rule");

        m.stop_rule(&r.id, true, false)
            .await
            .expect("stop the target");
        // The unwind is async (each release ends the client that holds the next hop);
        // every hop must land at zero — a leaked hold anywhere leaves the hop below live.
        assert!(
            wait_until(Duration::from_secs(5), || {
                [a.id.as_str(), b.id.as_str(), c.id.as_str()]
                    .into_iter()
                    .all(|id| {
                        real_conns_with(&built, id)
                            .iter()
                            .all(|c| c.refs.load(Ordering::SeqCst) == 0)
                    })
            })
            .await,
            "a hop kept a reference after the chain unwound"
        );
        m.close_all().await;
    }

    /// SPEC §tunnels.jump: killing the jump server takes the whole chain down — the target's
    /// own watcher reports the loss and releases the local port — and a restarted jump
    /// on the same port lets auto-reconnect rebuild the chain.
    #[tokio::test]
    async fn a_lost_jump_releases_the_port_and_a_restarted_jump_recovers_the_chain() {
        let (_dir, store) = scratch();
        let (m, _built) = real_manager(&store);
        let s1 = sshtest::spawn_killable_ssh_server(sshtest::TestAuth::AcceptAll);
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(
            &store,
            jump_pw_conn("bastion", "127.0.0.1", s1.port(), None),
        );
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &b.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect("the chained start");
        assert_eq!(round_trip(port, "up").await, "UP");

        s1.kill().await;
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id)
                == "reconnecting")
            .await,
            "the chain stayed {} after the jump died",
            state_of(&m, &r.id)
        );
        assert!(
            port_is_free(port).await,
            "the port goes down with the chain"
        );

        s1.restart().await;
        assert!(
            wait_until(Duration::from_secs(15), || state_of(&m, &r.id) == "up").await,
            "the retry never rebuilt the chain"
        );
        assert_eq!(round_trip(port, "back").await, "BACK");
        m.close_all().await;
    }

    /// SPEC §tunnels.jump: a jump whose credentials are refused is an auth failure of the
    /// target — never retried — and the message names the hop it failed on.
    #[tokio::test]
    async fn a_bad_jump_password_is_an_auth_failure_named_after_the_hop_and_never_retried() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s_bad = sshtest::spawn_ssh_server_with(sshtest::TestAuth::RejectAll).await;
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("bastion", "127.0.0.1", s_bad, None));
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &b.id, port, echo)
            },
        );

        let err = m
            .start_rule(&r.id)
            .await
            .expect_err("the jump refuses the credentials");
        assert!(
            err.message().contains("via bastion:"),
            "names the hop: {}",
            err.message()
        );
        assert_eq!(state_of(&m, &r.id), "error", "auth is never retried");
        // The kind itself, pinned where it is still structured: the throwaway Test
        // client reports the same failure with kind == auth.
        let res = m.test_connection(&b.id).await.expect("the test call");
        assert_eq!(res["kind"], json!("auth"), "{res}");
        assert!(
            res["error"]
                .as_str()
                .unwrap_or_default()
                .contains("via bastion:"),
            "{res}"
        );
        // Past the reconnect interval: nothing may have retried the refused credentials.
        tokio::time::sleep(Duration::from_millis(1400)).await;
        assert_eq!(state_of(&m, &r.id), "error");
        assert_eq!(
            real_conns_with(&built, &a.id).len(),
            1,
            "the jump was dialed exactly once"
        );
        m.close_all().await;
    }

    /// SPEC §tunnels.jump: editing a jump reconnects everything that rides it, and deleting
    /// one is refused while a dependent exists — the error names the dependent.
    #[tokio::test]
    async fn editing_a_jump_reconnects_its_dependents_and_delete_lists_them() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_ssh_server().await;
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("bastion", "127.0.0.1", s1, None));
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &b.id, port, echo));
        m.start_rule(&r.id).await.expect("the chained start");

        m.apply_connection_update(
            &a.id,
            &ConnInput {
                username: "someone-else".into(),
                ..jump_pw_conn("bastion", "127.0.0.1", s1, None)
            },
        )
        .await
        .expect("edit the jump");

        assert_eq!(
            state_of(&m, &r.id),
            "up",
            "the dependent came back with the jump"
        );
        assert_eq!(round_trip(port, "after").await, "AFTER");
        // A fresh jump client was built and the old one ended; the dependent redialed
        // through the fresh one.
        let a_clients = real_conns_with(&built, &a.id);
        assert!(a_clients.len() >= 2, "the edit redialed the jump");
        assert!(!a_clients[0].connected(), "the pre-edit client is gone");
        assert!(a_clients.last().unwrap().connected());

        // Deleting the jump is refused while a dependent references it — a plain error
        // naming the dependent (no confirm/force path: the chain would dangle).
        match m.delete_connection(&a.id).await {
            Err(OpError::Msg(msg)) => assert!(msg.contains("db"), "{msg}"),
            other => panic!("expected a plain refusal naming the dependent, got {other:?}"),
        }
        // With the dependent gone the delete goes through.
        m.stop_rule(&r.id, true, true).await.expect("stop");
        m.delete_rule(&r.id, false).await.expect("delete the rule");
        m.delete_connection(&b.id)
            .await
            .expect("delete the dependent");
        m.delete_connection(&a.id).await.expect("delete the jump");
        assert!(store.lock().unwrap().is_empty());
    }

    /// SPEC §tunnels.jump: POST /connections/:id/test rides a PRIVATE throwaway chain — one
    /// one-off client per hop, nothing pinned in the shared table, no host key learned —
    /// and a failing hop is named in the error.
    #[tokio::test]
    async fn testing_a_jump_connection_uses_a_throwaway_chain_and_names_the_hop_on_failure() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_killable_ssh_server(sshtest::TestAuth::AcceptAll);
        let s2 = sshtest::spawn_ssh_server().await;
        let a = add_conn_with(
            &store,
            jump_pw_conn("bastion", "127.0.0.1", s1.port(), None),
        );
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));

        let res = m.test_connection(&b.id).await.expect("the test call");
        assert_eq!(res["ok"], json!(true), "{res}");
        assert_eq!(built.lock().unwrap().len(), 0, "no shared client was built");
        assert_eq!(m.conn_state(&b.id), ConnState::Idle);
        // A successful Test of a keyless hop stores nothing (the TOFU note, §3.2).
        assert!(store
            .lock()
            .unwrap()
            .connection(&a.id)
            .unwrap()
            .host_key
            .is_none());

        s1.kill().await;
        let res = m.test_connection(&b.id).await.expect("the test call");
        assert_eq!(res["ok"], json!(false), "{res}");
        assert_eq!(res["kind"], json!("network"), "{res}");
        assert!(
            res["error"]
                .as_str()
                .unwrap_or_default()
                .contains("via bastion:"),
            "{res}"
        );
    }
}
