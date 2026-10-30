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

//! One rule's lifecycle: start, stop, and the teardown both share. Each runs under the rule's
//! own op lock, and the port is bound only while the tunnel can carry traffic.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::json;

use crate::tunnel::forward::{ChannelOpener, Forward, ForwardTarget};
use crate::tunnel::port;
use crate::tunnel::types::{
    is_retryable, ConnState, FailureKind, PortOwner, RuleDef, RuleState, TunnelError,
};
use super::{OpError, RuleRuntime, TunnelManager};
use swiss_core::log;

impl TunnelManager {
    /// Close the forward (releasing the port) and drop the SSH reference.
    pub(super) async fn teardown(&self, rt: &Arc<RuleRuntime>) {
        let fwd = rt
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forward
            .take();
        if let Some(fwd) = fwd {
            fwd.close().await;
        }
        self.release(rt).await;
    }

    pub async fn start_rule(self: &Arc<Self>, id: &str) -> Result<(), OpError> {
        let Some(rule) = self.with_store(|s| s.rule(id)) else {
            return Err(OpError::msg(format!("unknown rule: {id}")));
        };
        let rt = self.rt(id);
        let _guard = rt.op.lock().await;
        self.do_start(rule, false)
            .await
            .map_err(|te| OpError::msg(te.message))
    }

    async fn do_start(
        self: &Arc<Self>,
        rule: RuleDef,
        retried_own_port: bool,
    ) -> Result<(), TunnelError> {
        let rt = self.rt(&rule.id);
        self.clear_retry(&rt);
        if self.closed.load(Ordering::SeqCst) {
            return Err(TunnelError::new(
                "tunnels are closed (the plugin is stopped or stopping)",
                FailureKind::Config,
            ));
        }
        let listening_on_live_client = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            let listening = data.forward.as_ref().is_some_and(|f| f.listening());
            let live = data
                .holding
                .as_ref()
                .is_some_and(|c| c.state() == ConnState::Connected);
            if listening && live {
                data.state = RuleState::Up;
                return Ok(());
            }
            listening
        };
        // A listener whose SSH client is gone forwards nothing: every accept fails to open a
        // channel. Rebuild it rather than report a port that is bound to a dead session.
        if listening_on_live_client {
            self.teardown(&rt).await;
        }
        let conn = self.with_store(|s| s.connection(&rule.connection_id));
        let Some(conn) = conn else {
            let reason = format!(
                "unknown SSH connection: {}",
                if rule.connection_id.is_empty() {
                    "(none)"
                } else {
                    rule.connection_id.as_str()
                }
            );
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Error;
            data.reason = Some(reason.clone());
            return Err(TunnelError::new(reason, FailureKind::Config));
        };
        {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Starting;
            data.reason = None;
            data.port_owner = None;
        }

        let started = async {
            let ssh = self.acquire(&conn, &rt).await?;
            let ssh_for_open = ssh.clone();
            let opener: ChannelOpener = Arc::new(move |host: &str, port: u16| {
                let ssh = ssh_for_open.clone();
                let host = host.to_string();
                Box::pin(async move { ssh.open_channel(host, port).await })
            });
            let fwd = Forward::new(
                ForwardTarget {
                    local_port: rule.local_port,
                    target_host: rule.target_host.clone(),
                    target_port: rule.target_port,
                },
                opener,
            );
            fwd.listen().await?;
            Ok::<Arc<Forward>, TunnelError>(fwd)
        }
        .await;

        match started {
            Ok(fwd) => {
                let now = swiss_core::log::iso_now();
                {
                    let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.forward = Some(fwd);
                    data.state = RuleState::Up;
                    data.retries = 0; // a successful connect resets the backoff for the next outage
                    if data.lost {
                        data.reconnected_at = Some(now);
                        data.lost = false;
                    } else {
                        data.started_at = Some(now);
                    }
                }
                // The tunnel is UP; failing to persist the enabled flag is a disk problem, not a
                // tunnel problem. Tearing down a working forward over a transient tunnels.json
                // write error trades a warning for a real outage (and a reconnect loop, since the
                // in-memory flag is already set).
                let persist = self.with_store(|s| s.set_enabled(&rule.id, true));
                if let Err(err) = persist {
                    log::warn(
                        "tunnel up, but persisting enabled failed",
                        Some(json!({ "rule": rule.name, "err": err })),
                    );
                }
                log::log(
                    "info",
                    "tunnel up",
                    Some(json!({
                        "rule": rule.name,
                        "local": rule.local_port,
                        "target": format!("{}:{}", rule.target_host, rule.target_port),
                    })),
                );
                Ok(())
            }
            Err(te) => {
                self.teardown(&rt).await;
                let mut owner: Option<PortOwner> = None;
                if te.kind == FailureKind::Port {
                    let found = port::port_owner(rule.local_port).await;
                    // Our own stale listener is the gateway's problem to clean up, not the user's:
                    // close it and try once more. Anything else is reported with the holder named,
                    // so the panel can offer Force free rather than a bare EADDRINUSE.
                    if found.as_ref().is_some_and(|o| o.pid == std::process::id())
                        && !retried_own_port
                    {
                        log::warn(
                            "reclaiming a port held by our own stale listener",
                            Some(json!({ "port": rule.local_port })),
                        );
                        self.close_stray_on(rule.local_port, &rule.id).await;
                        return Box::pin(self.do_start(rule, true)).await;
                    }
                    owner = found;
                }
                let reason = match &owner {
                    Some(o) if te.kind == FailureKind::Port => format!(
                        "local port {} is held by pid {} ({})",
                        rule.local_port, o.pid, o.name
                    ),
                    _ => te.message.clone(),
                };
                let retrying = rule.auto_reconnect && is_retryable(te.kind);
                {
                    let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.port_owner = owner;
                    data.state = if retrying {
                        RuleState::Reconnecting
                    } else {
                        RuleState::Error
                    };
                    data.reason = Some(reason.clone());
                }
                if retrying {
                    self.schedule_retry(&rule);
                }
                log::warn(
                    "tunnel start failed",
                    Some(json!({ "rule": rule.name, "kind": te.kind.as_str(), "reason": reason })),
                );
                Err(TunnelError::new(reason, te.kind))
            }
        }
    }

    /// Close any forward of ours that still holds `port` (state desync recovery). The teardown
    /// runs through each victim's OWN queue: the direct version bypassed the per-rule
    /// serialization every other mutation goes through, so a stop/lost op racing on that rule
    /// could interleave with it.
    async fn close_stray_on(self: &Arc<Self>, port_no: u16, except_rule_id: &str) {
        let victims: Vec<RuleDef> = self
            .with_store(|s| s.rules())
            .into_iter()
            .filter(|r| r.id != except_rule_id && r.local_port == port_no)
            .collect();
        for victim in victims {
            let rt = self.rt(&victim.id);
            if rt.data.lock().map(|d| d.forward.is_none()).unwrap_or(true) {
                continue;
            }
            let _guard = rt.op.lock().await;
            // Already torn down while this op waited in the queue.
            if rt.data.lock().map(|d| d.forward.is_none()).unwrap_or(true) {
                continue;
            }
            self.teardown(&rt).await;
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Stopped;
        }
    }

    /// Stop a rule. `persist: false` keeps its `enabled` flag, which is how shutdown leaves the
    /// set of rules that should come back up on the next boot.
    pub async fn stop_rule(
        self: &Arc<Self>,
        id: &str,
        persist: bool,
        force: bool,
    ) -> Result<(), OpError> {
        let Some(rule) = self.with_store(|s| s.rule(id)) else {
            return Err(OpError::msg(format!("unknown rule: {id}")));
        };
        if !force {
            let dependents = self.dependents_of(id);
            if !dependents.is_empty() {
                return Err(OpError::Dependents(dependents));
            }
        }
        let rt = self.rt(id);
        let _guard = rt.op.lock().await;
        self.clear_retry(&rt);
        self.teardown(&rt).await;
        {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Stopped;
            data.reason = None;
            data.lost = false;
            data.retries = 0;
            data.started_at = None;
            data.reconnected_at = None;
            data.port_owner = None;
        }
        if persist {
            self.with_store(|s| s.set_enabled(id, false))
                .map_err(OpError::msg)?;
        }
        log::log("info", "tunnel stopped", Some(json!({ "rule": rule.name })));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, built_len, conn_input, echo_server, field, free_port, manager, nth,
        port_is_free, round_trip, row_of, rule_input, scratch, squat, state_of, stored_rule,
    };
    use crate::tunnel::store::{ConnInput, TunnelStore};
    use crate::tunnel::types::AuthType;

    // --- start and stop ---------------------------------------------------------------------

    #[tokio::test]
    async fn binds_the_port_carries_traffic_and_frees_the_port_on_stop() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("start");
        assert_eq!(state_of(&m, &r.id), "up");
        assert_eq!(round_trip(port, "ping").await, "PING");
        assert!(stored_rule(&store, &r.id).enabled);

        m.stop_rule(&r.id, true, false).await.expect("stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
        assert!(port_is_free(port).await);
        assert!(!stored_rule(&store, &r.id).enabled);
    }

    /// SPEC §tunnels.proxy: a proxied connection through the REAL manager and the real russh
    /// client — start, one byte round trip riding proxy -> ssh server -> direct-tcpip ->
    /// echo, then stop releases the local port. The existing port-binding invariant,
    /// exercised on the proxied path.
    #[tokio::test]
    async fn a_proxied_connection_forwards_through_the_manager_lifecycle() {
        let ssh_port = crate::tunnel::sshtest::spawn_ssh_server().await;
        let (proxy_port, mut obs) =
            crate::tunnel::sshtest::spawn_socks5_proxy(crate::tunnel::sshtest::SocksAuth::None, 0)
                .await;
        let echo = echo_server().await;
        let (dir, store) = scratch();
        let mut input = ConnInput {
            host: "localhost".into(),
            port: ssh_port as f64,
            auth_type: AuthType::Password,
            password: Some("pw".into()),
            proxy: Some(format!("socks5://127.0.0.1:{proxy_port}")),
            ..conn_input()
        };
        input.key_path = None; // password auth has no key
        let c = store
            .lock()
            .unwrap()
            .add_connection(&input)
            .expect("add the proxied connection");
        let port = free_port().await;
        let r = add_rule(&store, rule_input("via-proxy", &c.id, port, echo));
        let m = TunnelManager::new(store.clone(), None);

        m.start_rule(&r.id).await.expect("start through the proxy");
        assert_eq!(state_of(&m, &r.id), "up");
        // The SSH session itself must have gone through the proxy — without this, a dialer
        // that silently ignored the proxy field would still pass on the direct route.
        let o = obs.recv().await.expect("the proxy saw the ssh dial");
        assert_eq!(
            (o.atyp, o.host.as_slice(), o.port),
            (0x03, b"localhost".as_slice(), ssh_port)
        );
        assert_eq!(round_trip(port, "proxied").await, "PROXIED");

        m.stop_rule(&r.id, true, false).await.expect("stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
        assert!(port_is_free(port).await);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[tokio::test]
    async fn reports_the_holder_when_the_local_port_is_taken_by_someone_else() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let (squatter, port) = squat().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        let err = m.start_rule(&r.id).await.expect_err("the port is taken");
        assert!(err.message().contains("local port"), "{}", err.message());
        let row = row_of(&m, &r.id);
        assert_eq!(field(&row, "state"), json!("error"));
        let reason = field(&row, "reason")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(reason.contains(&port.to_string()), "{reason}");
        // Only Windows can name the holder (port.rs has no POSIX lookup), and there the holder
        // is this very test process — so the manager first reclaims what it thinks is its own
        // stale listener, fails again, and reports the pid rather than a bare EADDRINUSE.
        #[cfg(windows)]
        {
            assert!(reason.contains("held by pid"), "{reason}");
            assert_eq!(
                field(&row, "portOwner").get("pid").cloned(),
                Some(json!(std::process::id()))
            );
        }
        drop(squatter);
    }

    #[tokio::test]
    async fn fails_a_rule_whose_connection_is_unknown_without_touching_the_port() {
        let (dir, seed) = scratch();
        let echo = echo_server().await;
        let c = add_conn(&seed);
        let port = free_port().await;
        let mut r = add_rule(&seed, rule_input("orphan", &c.id, port, echo));
        // Break the link the way a hand-edited tunnels.json does — `remove_connection` refuses
        // while a rule still points at one, so the file is written directly and read back. A
        // rule whose connection is unknown is KEPT on load: one bad hand-edit must not silently
        // discard the other 17 rules, so the manager has to report it rather than crash.
        r.connection_id = "gone".into();
        seed.lock()
            .unwrap()
            .replace_all(Vec::new(), vec![r.clone()])
            .expect("hand-edit the file");
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        assert_eq!(
            store.lock().unwrap().rules().len(),
            1,
            "the rule survived load"
        );
        let (m, built) = manager(&store);

        let err = m.start_rule(&r.id).await.expect_err("no connection");
        assert!(
            err.message().contains("unknown SSH connection"),
            "{}",
            err.message()
        );
        assert_eq!(state_of(&m, &r.id), "error");
        assert!(port_is_free(port).await);
        assert_eq!(built_len(&built), 0, "it must not dial anything");
    }

    #[tokio::test]
    async fn is_idempotent_a_second_start_is_a_no_op_and_so_is_a_second_stop() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("first start");
        m.start_rule(&r.id).await.expect("second start");
        assert_eq!(nth(&built, 0).dials(), 1);

        m.stop_rule(&r.id, true, false).await.expect("first stop");
        m.stop_rule(&r.id, true, false).await.expect("second stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
    }

    #[tokio::test]
    async fn serializes_a_start_and_a_stop_that_arrive_together() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        let (a, b, d) = tokio::join!(
            m.start_rule(&r.id),
            m.stop_rule(&r.id, true, false),
            m.start_rule(&r.id)
        );
        let _ = (a, b, d);
        // Whatever order they landed in, the reported state and the actual port agree.
        let up = state_of(&m, &r.id) == "up";
        assert_eq!(up, !port_is_free(port).await);
        m.close_all().await;
    }
}
