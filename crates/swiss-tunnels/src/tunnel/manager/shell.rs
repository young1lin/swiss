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

//! Interactive shells (SPEC §terminal.remote): the host list and live state for the target
//! list, and a PTY that shares the connection's client with the rules on it.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::tunnel::types::{ConnState, FailureKind, SshConnDef, TunnelError};
use super::lease::ConnectionLease;
use super::TunnelManager;
use swiss_host::services::shell::{PtyEndpoint, PtySize};

impl TunnelManager {
    /// The connection definitions, for a capability that needs the host list without
    /// reaching through the store lock itself.
    pub fn connections(&self) -> Vec<SshConnDef> {
        self.with_store(|s| s.connections())
    }

    /// The live state of one connection, for the shell target list. A definition with no
    /// client yet is `Idle` — it has never been dialed, which is different from failing.
    pub fn conn_state(&self, id: &str) -> ConnState {
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|c| c.state())
            .unwrap_or(ConnState::Idle)
    }

    /// Open an interactive PTY on one connection, dialing it if nothing holds it yet.
    ///
    /// The session takes a reference the same way a rule does — through `put_ref`, the
    /// one place that knows "the last one out ends the client". A terminal on a host that
    /// already has tunnels up therefore shares their session rather than dialing a second
    /// one, and closing the terminal does not take their tunnels down with it.
    ///
    /// The reference travels INTO the pump (see [SshLike::open_shell]) rather than coming
    /// back to the caller: the pump is the only thing that knows when the session is
    /// really over, and a guard handed back to the provider would have nowhere to live.
    pub async fn open_shell(
        self: &Arc<Self>,
        conn_id: &str,
        size: PtySize,
        endpoint: PtyEndpoint,
    ) -> Result<(), TunnelError> {
        let Some(def) = self.with_store(|s| s.connection(conn_id)) else {
            return Err(TunnelError::new(
                format!("unknown ssh connection: {conn_id}"),
                FailureKind::Config,
            ));
        };
        let c = self.dial(&def).await?;
        c.refs().fetch_add(1, Ordering::SeqCst);
        let hold = ConnectionLease {
            manager: self.clone(),
            conn: c.clone(),
        };
        // On failure the guard drops here and gives the reference straight back, so a
        // refused PTY cannot leave a connection pinned open with nothing using it.
        c.open_shell(size, endpoint, Box::new(hold)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration as StdDuration;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, built_len, echo_server, eventually, free_port, manager, nth, rule_input,
        scratch, shells, state_of,
    };
    use swiss_host::services::shell::{
        drain_sessions, PtyEvent, ShellError, ShellProvider, ShellRegistry, OUTPUT_QUEUE_CHUNKS,
    };

    // --- interactive shells (SPEC §terminal.remote) ------------------------------------------------------
    //
    // These live here rather than beside `TunnelShells` because the harness does: a provider is
    // only interesting over a manager whose clients can be driven, and FakeConn is this module's.

    fn pty_size() -> PtySize {
        PtySize::new(80, 24).expect("in range")
    }

    #[tokio::test]
    async fn a_shell_takes_a_reference_and_gives_it_back_when_the_session_drops() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, built) = manager(&store);
        let provider = shells(&m);

        let mut session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("the shell opens");
        let conn = nth(&built, 0);
        assert_eq!(conn.shells(), 1, "one PTY, on the one client");
        assert_eq!(conn.ref_count(), 1, "the session holds a reference");
        assert_eq!(conn.ended(), 0);
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"80x24$ ".to_vec())),
            "the far side was told the size it was opened with"
        );

        drop(session);
        // Dropping the session is the whole release path: the pump notices, drops the guard,
        // the guard returns the reference through put_ref, and the last one out ends the client.
        assert!(
            eventually(|| conn.ref_count() == 0 && conn.ended() == 1).await,
            "refs={} ended={}",
            conn.ref_count(),
            conn.ended()
        );
    }

    #[tokio::test]
    async fn a_terminal_shares_a_rules_connection_and_does_not_close_it() {
        // The invariant a user feels: opening a terminal on a host that already has tunnels up
        // must not dial twice, and closing the terminal must not take the tunnels down.
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let port = echo_server().await;
        let rule = add_rule(&store, rule_input("db", &def.id, free_port().await, port));
        let (m, built) = manager(&store);
        m.start_rule(&rule.id).await.expect("the rule starts");
        let conn = nth(&built, 0);
        assert_eq!(conn.ref_count(), 1, "the rule holds one");

        let provider = shells(&m);
        let session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("the shell opens");
        assert_eq!(
            built_len(&built),
            1,
            "one client, shared - not a second dial"
        );
        assert_eq!(conn.ref_count(), 2, "and now two holders");

        drop(session);
        assert!(
            eventually(|| conn.ref_count() == 1).await,
            "the terminal reference came back"
        );
        assert_eq!(conn.ended(), 0, "the rule tunnel is untouched");
        assert_eq!(state_of(&m, &rule.id), "up");
    }

    #[tokio::test]
    async fn a_refused_pty_returns_the_reference_instead_of_pinning_the_client() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, built) = manager(&store);
        let provider = shells(&m);
        // Dial once so the client exists, then make the next PTY request fail.
        let first = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        let conn = nth(&built, 0);
        conn.refuse_shell.store(true, Ordering::SeqCst);

        match provider.open(&def.id, "terminal", pty_size()).await {
            Err(ShellError::Unavailable(msg)) => assert!(msg.contains("refused"), "{msg}"),
            Ok(_) => panic!("the server refused; the open must fail"),
            Err(other) => panic!("wrong error: {other}"),
        }
        // The guard drops on the failure path, and its release is a spawned task (Drop
        // cannot await): the reference comes back promptly, not synchronously.
        assert!(
            eventually(|| conn.ref_count() == 1).await,
            "the refused open gave its reference back; refs={}",
            conn.ref_count()
        );
        assert_eq!(
            conn.ended(),
            0,
            "the first session still holds the client open"
        );
        drop(first);
        assert!(eventually(|| conn.ref_count() == 0 && conn.ended() == 1).await);
    }

    #[tokio::test]
    async fn an_unknown_target_is_refused_without_minting_a_lease() {
        let (_dir, store) = scratch();
        add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        match provider.open("no-such-conn", "terminal", pty_size()).await {
            Err(ShellError::Unknown(msg)) => {
                assert!(msg.contains("no-such-conn"), "{msg}");
                assert!(
                    msg.contains("tunnels"),
                    "the message names the plugin: {msg}"
                );
            }
            Ok(_) => panic!("unknown target, got a session"),
            Err(other) => panic!("wrong error: {other}"),
        }
        assert_eq!(
            provider.ledger().outstanding(),
            0,
            "a typo must not show up in the drain count"
        );
    }

    #[tokio::test]
    async fn withdrawing_refuses_new_shells_and_names_the_plugin() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        provider.ledger().begin_withdraw();
        match provider.open(&def.id, "terminal", pty_size()).await {
            Err(ShellError::Withdrawing(msg)) => {
                assert!(
                    msg.contains("tunnels"),
                    "the message names the plugin: {msg}"
                )
            }
            Ok(_) => panic!("a withdrawing provider must not open a session"),
            Err(other) => panic!("wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn the_drain_reports_a_terminal_the_stop_had_to_close_over() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        provider.ledger().begin_withdraw();
        assert_eq!(
            drain_sessions(provider.ledger(), StdDuration::from_millis(50), "tunnels").await,
            1,
            "an attached terminal does not hand itself back"
        );
        drop(session);
        assert_eq!(
            drain_sessions(provider.ledger(), StdDuration::from_secs(5), "tunnels").await,
            0
        );
    }

    #[tokio::test]
    async fn the_target_list_reports_the_live_connection_state() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let before = provider.list();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].id, def.id);
        assert_eq!(before[0].username, "deploy");
        assert_eq!(
            before[0].state, "idle",
            "never dialed is idle, which is not the same as failing"
        );
        let session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        assert_eq!(provider.list()[0].state, "connected");
        drop(session);
    }

    #[tokio::test]
    async fn typing_reaches_the_far_side_and_a_resize_follows_it_in_order() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let mut session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"80x24$ ".to_vec()))
        );
        session.write(b"ls\n".to_vec()).await.expect("typed");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"ls\n".to_vec()))
        );
        let bigger = PtySize::new(132, 43).expect("in range");
        session.resize(bigger).await.expect("resized");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"132x43".to_vec()))
        );
        assert_eq!(session.size(), bigger);
    }

    #[tokio::test]
    async fn the_registry_hands_the_terminal_a_session_without_it_knowing_about_ssh() {
        // End to end through the host contract, which is the only path the terminal plugin
        // will ever use: register the provider, open through the registry, withdraw, clear.
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, built) = manager(&store);
        let provider = shells(&m);
        let registry = ShellRegistry::new();
        registry
            .register(provider.clone(), "tunnels")
            .expect("registers");
        assert!(registry.has_provider());
        assert_eq!(registry.list().len(), 1);

        let mut session = registry
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opened through the registry");
        assert!(session.next_event().await.is_some());
        assert_eq!(nth(&built, 0).ref_count(), 1);

        registry.begin_withdraw();
        provider.ledger().begin_withdraw();
        assert!(matches!(
            registry.open(&def.id, "terminal", pty_size()).await,
            Err(ShellError::Withdrawing(_))
        ));
        drop(session);
        assert_eq!(
            drain_sessions(provider.ledger(), StdDuration::from_secs(5), "tunnels").await,
            0,
            "the drain is clean once the tab is gone"
        );
        registry.clear();
        assert!(!registry.has_provider());
        assert!(eventually(|| nth(&built, 0).ended() == 1).await);
    }

    #[tokio::test]
    async fn a_typed_interrupt_gets_through_while_the_output_queue_is_full() {
        // SPEC §terminal.verify item 7, at this layer: the provider pumps the two directions in
        // separate tasks, so a terminal that is flooding still accepts Ctrl-C.
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let mut session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        // Never read the greeting: the fake echoes, so typing is what fills the queue.
        let typed = OUTPUT_QUEUE_CHUNKS + 8;
        for i in 0..typed {
            session
                .write(vec![b'a' + (i % 26) as u8])
                .await
                .expect("typed");
        }
        session.write(vec![0x03]).await.expect("Ctrl-C is accepted");
        // Drain: the greeting, then every byte typed, in order, none lost.
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"80x24$ ".to_vec()))
        );
        for i in 0..typed {
            assert_eq!(
                session.next_event().await,
                Some(PtyEvent::Data(vec![b'a' + (i % 26) as u8])),
                "byte {i} arrived out of order or was dropped"
            );
        }
        assert_eq!(session.next_event().await, Some(PtyEvent::Data(vec![0x03])));
    }
}
