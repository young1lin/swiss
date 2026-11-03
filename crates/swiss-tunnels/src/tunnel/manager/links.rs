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

//! The MCP linkage, display and guard rail only: which started MCPs a rule serves, and keeping the
//! links on the right MCP name. Nothing here can start or stop an MCP.

use serde_json::{json, Map, Value};

use super::TunnelManager;

/// The slice of the MCP registry the tunnel subsystem reads. Display and guard rail only —
/// nothing here can start or stop an MCP.
pub trait McpView: Send + Sync {
    fn has(&self, name: &str) -> bool;
    /// Display state ("up" | "down" | "stopped" | "error" | …), or None when unknown.
    fn state_of(&self, name: &str) -> Option<String>;
    /// True when the MCP is actually running — what the stop guard asks about.
    fn is_started(&self, name: &str) -> bool;
    /// ISO timestamp of the MCP's current run, for the stale-pool notice.
    fn started_at(&self, name: &str) -> Option<String>;
    /// Drop sockets this running MCP pooled before the tunnel it rides came back.
    /// True when they were released. The default does nothing: a test view, and any
    /// adapter that has no pool, leaves the restart notice in place.
    fn release_pooled_connections<'a>(
        &'a self,
        _name: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(std::future::ready(false))
    }
}

impl TunnelManager {
    /// Started MCPs that this rule declares it serves.
    pub fn dependents_of(&self, rule_id: &str) -> Vec<String> {
        let Some(rule) = self.with_store(|s| s.rule(rule_id)) else {
            return Vec::new();
        };
        let Some(view) = self.mcps.as_ref() else {
            return Vec::new();
        };
        rule.mcps
            .iter()
            .filter(|n| view.is_started(n))
            .cloned()
            .collect()
    }

    /// An MCP was renamed: carry every link over so it does not silently point at a dead name.
    pub fn rename_mcp(&self, from: &str, to: &str) -> Result<(), String> {
        self.with_store(|s| s.rename_mcp(from, to))
    }

    /// An MCP was deleted: no rule may go on claiming to serve it.
    pub fn forget_mcp(&self, name: &str) -> Result<(), String> {
        self.with_store(|s| s.forget_mcp(name))
    }

    /// Tunnels declaring they serve `mcp`, for the MCP detail page.
    pub fn tunnels_for_mcp(&self, mcp: &str) -> Vec<Value> {
        let started_at = self.mcps.as_ref().and_then(|v| v.started_at(mcp));
        self.with_store(|s| s.rules())
            .into_iter()
            .filter(|r| r.mcps.iter().any(|m| m == mcp))
            .map(|r| {
                let runtime = self.rt(&r.id);
                let data = runtime.data.lock().unwrap_or_else(|e| e.into_inner());
                let mut row = Map::new();
                row.insert("id".into(), json!(r.id));
                row.insert("name".into(), json!(r.name));
                row.insert("state".into(), json!(data.state.as_str()));
                row.insert("localPort".into(), json!(r.local_port));
                row.insert("targetHost".into(), json!(r.target_host));
                row.insert("targetPort".into(), json!(r.target_port));
                if let Some(reason) = &data.reason {
                    row.insert("reason".into(), json!(reason));
                }
                if let Some(at) = &data.reconnected_at {
                    row.insert("reconnectedAt".into(), json!(at));
                }
                // A reconnect after the MCP started, and we could not drop its pool. A pool
                // that was released dials the new session on its next call, so it is not stale.
                let reconnected_after_start = data
                    .reconnected_at
                    .as_ref()
                    .zip(started_at.as_ref())
                    .is_some_and(|(reconn, started)| reconn > started);
                let released = data.pools_released.iter().any(|name| name == mcp);
                let stale = reconnected_after_start && !released;
                row.insert("stalePool".into(), json!(stale));
                Value::Object(row)
            })
            .collect()
    }
}

/// What the MCP admin API is allowed to do with tunnels: read the links, and keep them pointing
/// at the right MCP name. Deliberately this narrow — an MCP operation must never start or stop
/// a tunnel (the port of Node's `TunnelLinks`), and it lives HERE so the admin side depends on
/// the tunnel side's vocabulary, never the reverse.
pub trait TunnelLinks: Send + Sync {
    /// Tunnel rows this MCP's traffic depends on, for the detail page's "tunnels" list.
    fn tunnels_for_mcp(&self, name: &str) -> Vec<Value>;
    /// A tunnel that declares it serves this MCP must follow a rename, or the link silently dies.
    fn rename_mcp(&self, from: &str, to: &str);
    /// No MCP by that name any more, so no rule may claim to serve it.
    fn forget_mcp(&self, name: &str);
}

impl TunnelLinks for TunnelManager {
    fn tunnels_for_mcp(&self, name: &str) -> Vec<Value> {
        self.tunnels_for_mcp(name)
    }

    fn rename_mcp(&self, from: &str, to: &str) {
        let _ = TunnelManager::rename_mcp(self, from, to);
    }

    fn forget_mcp(&self, name: &str) {
        let _ = TunnelManager::forget_mcp(self, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, echo_server, field, free_port, manager_with, nth, row_of, rule_input,
        scratch, state_of, wait_until,
    };
    use crate::tunnel::manager::OpError;
    use crate::tunnel::store::RuleInput;
    use crate::tunnel::types::FailureKind;

    /// The MCP registry view the linkage tests run against.
    struct View {
        started: Vec<String>,
        /// When set, a reconnect drops this MCP's pool and the restart notice stays off.
        release: bool,
    }

    impl View {
        fn boxed(started: &[&str]) -> Option<Box<dyn McpView>> {
            Some(Box::new(View {
                started: started.iter().map(|s| s.to_string()).collect(),
                release: false,
            }))
        }
    }

    impl McpView for View {
        fn has(&self, name: &str) -> bool {
            matches!(name, "pg-analytics" | "redis-b-6380")
        }
        fn state_of(&self, name: &str) -> Option<String> {
            Some(if self.is_started(name) {
                "up".into()
            } else {
                "stopped".into()
            })
        }
        fn is_started(&self, name: &str) -> bool {
            self.started.iter().any(|s| s == name)
        }
        fn started_at(&self, name: &str) -> Option<String> {
            self.is_started(name)
                .then(|| "2026-08-13T10:00:00.000Z".to_string())
        }
        fn release_pooled_connections<'a>(
            &'a self,
            name: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
            let ok = self.release && self.is_started(name);
            Box::pin(std::future::ready(ok))
        }
    }

    // --- MCP linkage ------------------------------------------------------------------------

    #[tokio::test]
    async fn blocks_a_stop_while_a_linked_mcp_is_started_and_obeys_force() {
        let (_dir, store) = scratch();
        let (m, _built) = manager_with(&store, View::boxed(&["pg-analytics"]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");

        match m.stop_rule(&r.id, true, false).await {
            Err(OpError::Dependents(names)) => {
                assert_eq!(names, vec!["pg-analytics".to_string()])
            }
            other => panic!("expected a Dependents refusal, got {other:?}"),
        }
        assert_eq!(
            state_of(&m, &r.id),
            "up",
            "not stopped behind the user's back"
        );
        m.stop_rule(&r.id, true, true).await.expect("forced stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
    }

    #[tokio::test]
    async fn does_not_block_when_the_linked_mcp_is_not_running() {
        let (_dir, store) = scratch();
        let (m, _built) = manager_with(&store, View::boxed(&[]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");
        m.stop_rule(&r.id, true, false).await.expect("stop");
    }

    #[tokio::test]
    async fn collects_every_dependent_into_one_error_for_stop_all() {
        let (_dir, store) = scratch();
        let (m, _built) =
            manager_with(&store, View::boxed(&["pg-analytics", "redis-b-6380"]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let pa = free_port().await;
        let pb = free_port().await;
        add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("a", &c.id, pa, echo)
            },
        );
        add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["redis-b-6380".into()]),
                ..rule_input("b", &c.id, pb, echo)
            },
        );
        m.start_all().await;

        let mut blocked = m.stop_all(false).await.expect_err("both are in use");
        blocked.sort();
        assert_eq!(blocked, vec!["pg-analytics", "redis-b-6380"]);

        let results = m.stop_all(true).await.expect("forced stop-all");
        assert!(results.iter().all(|x| x.ok), "{results:?}");
    }

    #[tokio::test]
    async fn reports_link_state_and_an_unknown_mcp_name_honestly() {
        let (_dir, store) = scratch();
        let (m, _built) = manager_with(&store, View::boxed(&["pg-analytics"]), None);
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into(), "ghost".into()]),
                ..rule_input("pg", &c.id, port, 5432)
            },
        );

        assert_eq!(
            field(&row_of(&m, &r.id), "mcpRows"),
            json!([
                { "name": "pg-analytics", "state": "up", "known": true },
                { "name": "ghost", "state": "stopped", "known": false },
            ])
        );
    }

    #[tokio::test]
    async fn flags_a_stale_pool_only_when_the_reconnect_came_after_the_mcp_started() {
        let (_dir, store) = scratch();
        let (m, built) = manager_with(&store, View::boxed(&["pg-analytics"]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");
        assert_eq!(
            m.tunnels_for_mcp("pg-analytics")[0]["stalePool"],
            json!(false)
        );

        nth(&built, 0).die(FailureKind::Network);
        // Down first, then up again — without the first wait this races the spawned recovery
        // and reads the state the rule still had before the transport died.
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id)
                == "reconnecting")
            .await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(
            wait_until(Duration::from_secs(10), || state_of(&m, &r.id) == "up").await,
            "the retry never brought it back up"
        );
        // The MCP started at 10:00 on 2026-08-13; this reconnect is now, so its pool may still
        // hold sockets of the dead session.
        assert_eq!(
            m.tunnels_for_mcp("pg-analytics")[0]["stalePool"],
            json!(true)
        );
        m.close_all().await;
    }

    #[tokio::test]
    async fn a_reconnect_that_releases_the_pool_does_not_ask_for_a_restart() {
        let (_dir, store) = scratch();
        let (m, built) = manager_with(
            &store,
            Some(Box::new(View {
                started: vec!["pg-analytics".into()],
                release: true,
            })),
            None,
        );
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id) == "reconnecting").await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(
            wait_until(Duration::from_secs(10), || state_of(&m, &r.id) == "up").await,
            "the retry never brought it back up"
        );
        assert_eq!(
            m.tunnels_for_mcp("pg-analytics")[0]["stalePool"],
            json!(false),
            "the pool was dropped with the old session"
        );
        m.close_all().await;
    }
}
