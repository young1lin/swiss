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

//! One SSH client per connection, refcounted: dial a client or reuse the live one, take and give
//! back a rule's reference, and end the client when the last reference goes.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tunnel::ssh::{JumpDialer, SshHooks};
use crate::tunnel::types::{FailureKind, SshConnDef, TunnelError};
use super::client::SshLike;
use super::{RuleRuntime, TunnelManager};
use swiss_core::log;

impl TunnelManager {
    fn make_conn(self: &Arc<Self>, def: &SshConnDef) -> Arc<dyn SshLike> {
        let store = self.store.clone();
        let conn_id = def.id.clone();
        let conn_name = def.name.clone();
        let on_host_key: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |fp: &str| {
            match store.lock().map(|mut s| s.set_host_key(&conn_id, fp)) {
                Ok(Ok(())) => {}
                Ok(Err(err)) => log::warn(
                    "could not persist host key",
                    Some(json!({ "connection": conn_name, "err": err })),
                ),
                Err(_) => {}
            }
        });
        let weak = Arc::downgrade(self);
        let lost_id = def.id.clone();
        let on_lost: Arc<dyn Fn(TunnelError) + Send + Sync> = Arc::new(move |err: TunnelError| {
            if let Some(mgr) = weak.upgrade() {
                mgr.on_connection_lost(&lost_id, &err);
            }
        });
        // SPEC §tunnels.jump: the dialer this connection's dial calls back into when its def
        // carries a jump. Weak so the connection table never closes into an Arc cycle
        // (manager -> client -> hook -> manager); a manager that is already gone fails
        // the hop as config rather than holding shutdown open.
        let weak_jump = Arc::downgrade(self);
        let jump: JumpDialer = Arc::new(move |jump_id: &str, host: &str, port: u16| {
            let weak_jump = weak_jump.clone();
            let jump_id = jump_id.to_string();
            let host = host.to_string();
            Box::pin(async move {
                let Some(mgr) = weak_jump.upgrade() else {
                    return Err(TunnelError::new(
                        "the tunnel manager is shutting down",
                        FailureKind::Config,
                    ));
                };
                mgr.dial_jump(&jump_id, &host, port).await
            })
        });
        (self.make_connection)(
            def,
            SshHooks {
                on_host_key: Some(on_host_key),
                on_lost: Some(on_lost),
                jump: Some(jump),
            },
        )
    }

    fn conn(self: &Arc<Self>, def: &SshConnDef) -> Arc<dyn SshLike> {
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = conns.get(&def.id) {
            existing.set_def(def.clone());
            existing.clone()
        } else {
            let made = self.make_conn(def);
            conns.insert(def.id.clone(), made.clone());
            made
        }
    }

    /// Connect (or reuse) the connection, WITHOUT taking a reference on it.
    ///
    /// Split out of `acquire` for the shell capability: a terminal session needs the same
    /// dial and the same host-key-mismatch bookkeeping, but its reference is session-
    /// scoped rather than rule-scoped, so the two diverge only after this point.
    pub(super) async fn dial(self: &Arc<Self>, def: &SshConnDef) -> Result<Arc<dyn SshLike>, TunnelError> {
        let c = self.conn(def);
        if let Err(te) = c.clone().connect().await {
            if te.kind == FailureKind::HostKey {
                if let Some(actual) = te
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("actual"))
                    .and_then(Value::as_str)
                {
                    self.mismatches
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(def.id.clone(), actual.to_string());
                }
            }
            return Err(te);
        }
        Ok(c)
    }

    /// Connect (or reuse) the connection and take a reference for this rule.
    pub(super) async fn acquire(
        self: &Arc<Self>,
        def: &SshConnDef,
        rt: &Arc<RuleRuntime>,
    ) -> Result<Arc<dyn SshLike>, TunnelError> {
        let c = self.dial(def).await?;
        // A rule holds at most one reference, however many times it is started — taken on THIS
        // exact client. A holding left over from a swap (the old client ended and the map
        // rebuilt one under the same id) is released against the old client, never the new.
        let stale = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            match &data.holding {
                Some(h) if Arc::ptr_eq(h, &c) => None, // already holding this very client
                _ => {
                    c.refs().fetch_add(1, Ordering::SeqCst);
                    data.holding.replace(c.clone())
                }
            }
        };
        if let Some(stale) = stale {
            self.put_ref(&stale).await;
        }
        Ok(c)
    }

    /// Drop this rule's reference; the last one out ends the client and frees its session.
    pub(super) async fn release(&self, rt: &Arc<RuleRuntime>) {
        let held = rt
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .holding
            .take();
        if let Some(c) = held {
            self.put_ref(&c).await;
        }
    }

    /// Return one reference on `c`. The last one out removes it from the connection map — only
    /// if the map still holds this exact client (a replacement built in between must not be
    /// swept out from under its own holders) — and ends the session.
    pub(super) async fn put_ref(&self, c: &Arc<dyn SshLike>) {
        if c.refs().fetch_sub(1, Ordering::SeqCst) <= 1 {
            let id = c.id();
            // Scoped: the std lock is never held across the end() await below.
            {
                let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
                let same = conns
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, c));
                if same {
                    conns.remove(&id);
                }
            }
            c.end().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, built_len, echo_server, free_port, manager, nth, rule_input, scratch,
    };

    // --- connection sharing -----------------------------------------------------------------

    #[tokio::test]
    async fn dials_once_for_many_rules_and_ends_the_client_when_the_last_one_stops() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut rules = Vec::new();
        for i in 0..3 {
            let port = free_port().await;
            rules.push(add_rule(
                &store,
                rule_input(&format!("r{i}"), &c.id, port, echo),
            ));
        }

        let results = m.start_all().await;
        assert!(results.iter().all(|x| x.ok), "{results:?}");
        assert_eq!(built_len(&built), 1);
        assert_eq!(nth(&built, 0).dials(), 1);
        assert_eq!(nth(&built, 0).ref_count(), 3);
        // The panel's connection row reads the live client through the same seam.
        let rows = m.rows();
        let conn_row = &rows["connections"][0];
        assert_eq!(conn_row["state"], json!("connected"));
        assert_eq!(conn_row["ruleCount"], json!(3));
        assert_eq!(conn_row["activeRules"], json!(3));

        m.stop_rule(&rules[0].id, true, false)
            .await
            .expect("stop 0");
        assert_eq!(nth(&built, 0).ended(), 0, "two rules still use it");
        m.stop_rule(&rules[1].id, true, false)
            .await
            .expect("stop 1");
        m.stop_rule(&rules[2].id, true, false)
            .await
            .expect("stop 2");
        assert_eq!(nth(&built, 0).ref_count(), 0);
        assert_eq!(nth(&built, 0).ended(), 1);
    }

    #[tokio::test]
    async fn does_not_double_count_a_reference_when_a_rule_is_started_twice() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("first start");
        m.start_rule(&r.id).await.expect("second start");
        assert_eq!(nth(&built, 0).ref_count(), 1);

        m.stop_rule(&r.id, true, false).await.expect("stop");
        assert_eq!(nth(&built, 0).ended(), 1);
    }
}
