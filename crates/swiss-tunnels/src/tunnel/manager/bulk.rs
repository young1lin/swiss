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

//! Many rules at once: boot (`start_enabled`), start-all and stop-all, and the shutdown sweep
//! (`close_all`).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::tunnel::types::{RuleDef, RuleState};
use super::client::SshLike;
use super::{OpResult, RuleRuntime, TunnelManager};

impl TunnelManager {
    /// Start every rule marked enabled. Never throws: one bad rule must not hold up the gateway.
    pub async fn start_enabled(self: &Arc<Self>) -> Vec<OpResult> {
        self.reopen();
        let rules: Vec<RuleDef> = self
            .with_store(|s| s.rules())
            .into_iter()
            .filter(|r| r.enabled)
            .collect();
        if rules.is_empty() {
            return Vec::new();
        }
        self.run_many(rules, RunOp::Start).await
    }

    pub async fn start_all(self: &Arc<Self>) -> Vec<OpResult> {
        let rules = self.with_store(|s| s.rules());
        self.run_many(rules, RunOp::Start).await
    }

    pub async fn stop_all(self: &Arc<Self>, force: bool) -> Result<Vec<OpResult>, Vec<String>> {
        let running: Vec<RuleDef> = self
            .with_store(|s| s.rules())
            .into_iter()
            .filter(|r| {
                let state = self
                    .runtimes
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&r.id).cloned())
                    .and_then(|rt| rt.data.lock().ok().map(|d| d.state));
                matches!(
                    state,
                    Some(RuleState::Up)
                        | Some(RuleState::Starting)
                        | Some(RuleState::Reconnecting)
                        | Some(RuleState::Error)
                )
            })
            .collect();
        if !force {
            let mut blocked: Vec<String> = Vec::new();
            for r in &running {
                for d in self.dependents_of(&r.id) {
                    if !blocked.contains(&d) {
                        blocked.push(d);
                    }
                }
            }
            if !blocked.is_empty() {
                return Err(blocked);
            }
        }
        Ok(self.run_many(running, RunOp::StopForce).await)
    }

    /// Run an operation over many rules, grouped so rules sharing a connection are serialized
    /// behind one dial rather than racing 14 handshakes at the same host. (The groups run
    /// concurrently; within a group, sequentially.)
    async fn run_many(self: &Arc<Self>, rules: Vec<RuleDef>, op: RunOp) -> Vec<OpResult> {
        #[derive(Clone, Copy)]
        enum Which {
            Start,
            Stop,
        }
        let which = match op {
            RunOp::Start => Which::Start,
            RunOp::StopForce => Which::Stop,
        };
        let mut groups: Vec<(String, Vec<RuleDef>)> = Vec::new();
        for r in rules {
            match groups.iter_mut().find(|(k, _)| *k == r.connection_id) {
                Some((_, list)) => list.push(r),
                None => groups.push((r.connection_id.clone(), vec![r])),
            }
        }
        let mut handles = Vec::new();
        for (_, group) in groups {
            let weak = Arc::downgrade(self);
            handles.push(tokio::spawn(async move {
                let mgr = match weak.upgrade() {
                    Some(m) => m,
                    None => return Vec::new(),
                };
                let mut out = Vec::new();
                for rule in group {
                    let result = match which {
                        Which::Start => mgr.start_rule(&rule.id).await.map_err(|e| e.message()),
                        Which::Stop => mgr
                            .stop_rule(&rule.id, true, true)
                            .await
                            .map_err(|e| e.message()),
                    };
                    out.push(match result {
                        Ok(()) => OpResult::ok(&rule),
                        Err(message) => OpResult::err(&rule, message),
                    });
                }
                out
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            if let Ok(group_results) = handle.await {
                results.extend(group_results);
            }
        }
        results
    }

    /// Shut everything down without touching `enabled`, so the next boot restores the same set.
    /// Must finish inside the shutdown budget: closes are local and the SSH end() has its own
    /// 1.5s cap.
    pub async fn close_all(self: &Arc<Self>) {
        // Raised BEFORE the stops: a start that queues behind one of them (or arrives after
        // the sweep) must find the gate up, or it rebuilds what this is tearing down.
        self.closed.store(true, Ordering::SeqCst);
        let rts: Vec<Arc<RuleRuntime>> = self
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for rt in &rts {
            self.clear_retry(rt);
        }
        let rules = self.with_store(|s| s.rules());
        let mut handles = Vec::new();
        for r in rules {
            let weak = Arc::downgrade(self);
            let id = r.id.clone();
            handles.push(tokio::spawn(async move {
                if let Some(mgr) = weak.upgrade() {
                    let _ = mgr.stop_rule(&id, false, true).await; // best effort
                }
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
        let live: Vec<Arc<dyn SshLike>> = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, c)| c)
            .collect();
        for c in live {
            let _ = c.end().await; // best effort
        }
    }
}

/// Which operation `run_many` applies.
enum RunOp {
    Start,
    StopForce,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, built_len, echo_server, free_port, manager, nth, port_is_free,
        round_trip, rule_input, scratch, squat, state_of, stored_rule,
    };

    // --- boot and shutdown ------------------------------------------------------------------

    #[tokio::test]
    async fn starts_only_enabled_rules_and_never_throws_when_one_fails() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let good = add_rule(&store, rule_input("good", &c.id, free_port().await, echo));
        let off = add_rule(&store, rule_input("off", &c.id, free_port().await, echo));
        let (squatter, taken) = squat().await;
        let bad = add_rule(&store, rule_input("bad", &c.id, taken, echo));
        {
            let mut s = store.lock().unwrap();
            s.set_enabled(&good.id, true).expect("enable good");
            s.set_enabled(&bad.id, true).expect("enable bad");
        }

        let results = m.start_enabled().await;
        let mut names: Vec<&str> = results.iter().map(|x| x.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["bad", "good"]);
        assert!(results.iter().any(|x| x.name == "good" && x.ok));
        assert!(results.iter().any(|x| x.name == "bad" && !x.ok));
        assert_eq!(state_of(&m, &off.id), "stopped");
        drop(squatter);
        m.close_all().await;
    }

    /// The tunnels plugin's Restart, clicked twice in a row: the first boot task is still
    /// starting rules when close_all runs. Every rule it starts AFTER close_all passed it used
    /// to keep a listening forward on the client close_all then ended — reported "up", every
    /// connection through it refused with "ssh connection is not established".
    #[tokio::test]
    async fn a_start_that_lands_during_or_after_close_all_is_refused_not_leaked() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut rules = Vec::new();
        for i in 0..6 {
            let r = add_rule(
                &store,
                rule_input(&format!("r{i}"), &c.id, free_port().await, echo),
            );
            store
                .lock()
                .unwrap()
                .set_enabled(&r.id, true)
                .expect("enable");
            rules.push(r);
        }

        // Boot in the background, close while it is still working through the group.
        let booting = tokio::spawn({
            let m = m.clone();
            async move { m.start_enabled().await }
        });
        tokio::task::yield_now().await;
        m.close_all().await;
        let _ = booting.await;
        // The boot's group task is a detached spawn; give it every chance to run past close_all.
        tokio::time::sleep(Duration::from_millis(50)).await;

        for r in &rules {
            assert_ne!(
                state_of(&m, &r.id),
                "up",
                "{} came up after close_all",
                r.name
            );
            assert!(
                port_is_free(r.local_port).await,
                "{} still holds its port",
                r.name
            );
        }
        for i in 0..built_len(&built) {
            assert!(
                !nth(&built, i).is_connected(),
                "a client survived close_all"
            );
        }
        // A rule asked to start behind the gate is refused, not half-built.
        let refused = m.start_rule(&rules[0].id).await.expect_err("closed");
        assert!(
            refused.message().contains("closed"),
            "{}",
            refused.message()
        );
        assert_eq!(state_of(&m, &rules[0].id), "stopped");

        // The next boot lowers the gate and everything comes back.
        let results = m.start_enabled().await;
        assert!(results.iter().all(|x| x.ok), "{results:?}");
        for r in &rules {
            assert_eq!(state_of(&m, &r.id), "up");
        }
        m.close_all().await;
    }

    /// A listening forward whose client has been ended (a stray `end()` from any path) is
    /// rebuilt on the next start rather than reported "up" over a dead session.
    #[tokio::test]
    async fn a_listener_on_a_dead_client_is_rebuilt_on_start_not_reported_up() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));
        m.start_rule(&r.id).await.expect("start");
        let first = nth(&built, 0);
        first.end().await; // the session is gone; the listener is not
        assert!(
            !port_is_free(port).await,
            "the stale listener is still bound"
        );

        m.start_rule(&r.id)
            .await
            .expect("restart over the dead client");
        assert_eq!(state_of(&m, &r.id), "up");
        // The dead client was the rule's last reference, so it left the map; the restart
        // built a fresh one instead of reviving it.
        assert!(!first.is_connected(), "the ended client stays ended");
        assert!(
            nth(&built, built_len(&built) - 1).is_connected(),
            "a live client replaced it"
        );
        assert_eq!(round_trip(port, "hello").await, "HELLO");
        m.close_all().await;
    }

    #[tokio::test]
    async fn close_all_frees_every_port_but_leaves_enabled_alone() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut ports = Vec::new();
        let mut ids = Vec::new();
        for i in 0..2 {
            let port = free_port().await;
            ids.push(add_rule(&store, rule_input(&format!("r{i}"), &c.id, port, echo)).id);
            ports.push(port);
        }
        m.start_all().await;

        m.close_all().await;
        for port in ports {
            assert!(port_is_free(port).await);
        }
        // Enabled is untouched, so the next boot restores exactly this set.
        for id in &ids {
            assert!(stored_rule(&store, id).enabled);
        }
        assert_eq!(nth(&built, 0).ended(), 1);
    }
}
