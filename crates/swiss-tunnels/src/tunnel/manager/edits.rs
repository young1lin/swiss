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

//! Edits and deletes of rules and connections, the connection test, and trusting a host key
//! that changed.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::tunnel::ssh::{JumpDefs, SshConnection};
use crate::tunnel::store::{ConnInput, RuleInput};
use crate::tunnel::types::{RuleDef, SshConnDef};
use super::{OpError, TunnelManager};

impl TunnelManager {
    /// Save a rule edit, restarting it when it was running. A restart failure must not turn
    /// into a PUT error: the edit itself already saved, and the row carries the error state.
    pub async fn apply_rule_update(
        self: &Arc<Self>,
        id: &str,
        input: &RuleInput,
    ) -> Result<RuleDef, OpError> {
        let was_running = self.is_active(id);
        if was_running {
            self.stop_rule(id, false, true).await?;
        }
        let def = self
            .with_store(|s| s.update_rule(id, input))
            .map_err(OpError::msg)?;
        if was_running {
            let _ = self.start_rule(id).await; // the rule holds its own error state
        }
        self.table_changed();
        Ok(def)
    }

    /// Save a connection edit: stop its rules, replace the client, restart the ones that were
    /// running. Only those — a rule the user had deliberately stopped must not come up because a
    /// neighbour was edited.
    pub async fn apply_connection_update(
        self: &Arc<Self>,
        id: &str,
        input: &ConnInput,
    ) -> Result<SshConnDef, OpError> {
        let was_running: Vec<RuleDef> = self
            .with_store(|s| s.rules_for_connection(id))
            .into_iter()
            .filter(|r| self.is_active(&r.id))
            .collect();
        // SPEC §tunnels.jump: connections that jump through this one ride its transport, so
        // their running rules restart with it — conservatively, without diffing which
        // fields changed: the old client is going away either way, and every dependent
        // redials the whole chain through the fresh one.
        let dependent_rules: Vec<RuleDef> = self
            .with_store(|s| s.connections())
            .into_iter()
            .filter(|c| c.jump.as_deref() == Some(id))
            .flat_map(|c| self.with_store(|s| s.rules_for_connection(&c.id)))
            .filter(|r| self.is_active(&r.id))
            .collect();
        for r in was_running.iter().chain(&dependent_rules) {
            self.stop_rule(&r.id, false, true).await?;
        }
        let existing = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(existing) = existing {
            existing.end().await;
        }
        self.mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let def = self
            .with_store(|s| s.update_connection(id, input))
            .map_err(OpError::msg)?;
        for r in was_running.iter().chain(&dependent_rules) {
            let _ = self.start_rule(&r.id).await; // the rule holds its own error state
        }
        // SPEC §host.seats: the endpoint inventory moved — the seat's notice owes a page
        // its re-read. The tunnels table reads the same connection.
        self.remote_seat_changed();
        self.table_changed();
        Ok(def)
    }

    pub async fn delete_rule(self: &Arc<Self>, id: &str, force: bool) -> Result<(), OpError> {
        if self.with_store(|s| s.rule(id)).is_none() {
            return Err(OpError::msg(format!("unknown rule: {id}")));
        }
        if !force {
            let dependents = self.dependents_of(id);
            if !dependents.is_empty() {
                return Err(OpError::Dependents(dependents));
            }
        }
        self.stop_rule(id, false, true).await?;
        self.runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        self.with_store(|s| s.remove_rule(id))
            .map_err(OpError::msg)?;
        self.table_changed();
        Ok(())
    }

    pub async fn delete_connection(self: &Arc<Self>, id: &str) -> Result<(), OpError> {
        if self.with_store(|s| s.connection(id)).is_none() {
            return Err(OpError::msg(format!("unknown SSH connection: {id}")));
        }
        // SPEC §tunnels.jump: a connection other definitions jump through cannot be deleted.
        // Unlike the rules case below there is no confirm/force path — deleting would
        // leave every dependent's chain pointing at nothing — so this is a plain error
        // naming the dependents (a 400, not the 409 + structured list).
        let jump_users: Vec<String> = self
            .with_store(|s| s.connections())
            .iter()
            .filter(|c| c.jump.as_deref() == Some(id))
            .map(|c| c.name.clone())
            .collect();
        if !jump_users.is_empty() {
            return Err(OpError::msg(format!(
                "connection is used as a jump by: {}",
                jump_users.join(", ")
            )));
        }
        // Rules still riding this connection block the delete — as Dependents, so the API
        // answers 409 + a structured list exactly like rule deletion.
        let used_by: Vec<String> = self
            .with_store(|s| s.rules_for_connection(id))
            .iter()
            .map(|r| r.name.clone())
            .collect();
        if !used_by.is_empty() {
            return Err(OpError::Dependents(used_by));
        }
        self.with_store(|s| s.remove_connection(id))
            .map_err(OpError::msg)?;
        let existing = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(existing) = existing {
            existing.end().await;
        }
        self.mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        // SPEC §host.seats: an endpoint left the inventory.
        self.remote_seat_changed();
        self.table_changed();
        Ok(())
    }

    /// Dial a throwaway client to prove credentials and host key end to end.
    pub async fn test_connection(self: &Arc<Self>, id: &str) -> Result<Value, OpError> {
        let Some(def) = self.with_store(|s| s.connection(id)) else {
            return Err(OpError::msg(format!("unknown SSH connection: {id}")));
        };
        // SPEC §tunnels.jump: the probe resolves its hop definitions read-only out of the
        // store and dials a private throwaway chain — nothing lands in the shared
        // connection table and no hop learns a host key.
        let store = self.store.clone();
        let defs: JumpDefs = Arc::new(move |jump_id: &str| {
            store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .connection(jump_id)
        });
        let res = SshConnection::test(&def, Some(defs)).await;
        if !res.ok && res.kind.as_deref() == Some("hostkey") {
            if let Some(fp) = res.fingerprint.clone() {
                self.mismatches
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.to_string(), fp);
            }
        }
        let mut out = res.to_json();
        if res.ok && def.host_key.is_none() {
            // The throwaway test client runs WITHOUT the persistence hook, so a successful Test
            // of a keyless connection stores nothing — the first real connect (a rule start) is
            // what learns and persists the fingerprint. The result says what it was shown, and
            // this re-read says whatever IS stored.
            if let Some(stored) = self
                .with_store(|s| s.connection(id))
                .and_then(|c| c.host_key)
            {
                out.as_object_mut()
                    .map(|m| m.insert("hostKey".into(), json!(stored)));
            }
        }
        Ok(out)
    }

    /// Accept the fingerprint a connection presented. With a recorded mismatch we store exactly
    /// that key; otherwise we clear the stored one so the next connect learns it (trust on
    /// first use).
    pub fn trust_host_key(&self, id: &str) -> Result<Option<String>, OpError> {
        if self.with_store(|s| s.connection(id)).is_none() {
            return Err(OpError::msg(format!("unknown SSH connection: {id}")));
        }
        let presented = self
            .mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        match presented {
            Some(fp) => {
                self.with_store(|s| s.set_host_key(id, &fp))
                    .map_err(OpError::msg)?;
                let fresh = self.with_store(|s| s.connection(id));
                if let (Some(c), Some(def)) = (
                    self.conns
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(id)
                        .cloned(),
                    fresh,
                ) {
                    c.set_def(def);
                }
                self.table_changed();
                Ok(Some(fp))
            }
            // Not update_connection({...def, hostKey: undefined}): the store deliberately
            // re-adopts a learned key when host/port are unchanged, which would silently undo
            // this clear.
            None => {
                self.with_store(|s| s.clear_host_key(id))
                    .map_err(OpError::msg)?;
                self.table_changed();
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, built_len, conn_input, echo_server, free_port, manager, nth,
        port_is_free, round_trip, rule_input, scratch, state_of,
    };
    use crate::tunnel::store::ConnInput;
    use crate::tunnel::types::AuthType;

    // --- edits ------------------------------------------------------------------------------

    /// SPEC §tunnels.store: a Test stores nothing, and it does say what it was shown, which is
    /// how a key gets confirmed by someone before anything pins it. A real SSH handshake, with
    /// real auth, on a throwaway client.
    #[tokio::test]
    async fn a_keyless_test_reports_the_key_it_was_shown_and_pins_nothing() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let port = crate::tunnel::sshtest::spawn_ssh_server().await;
        let conn = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "keyless".into(),
                host: "127.0.0.1".into(),
                port: f64::from(port),
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                ..Default::default()
            })
            .expect("the connection");

        let answer = m.test_connection(&conn.id).await.expect("a test answer");

        assert_eq!(answer["ok"], json!(true), "{answer}");
        let fingerprint = answer["fingerprint"].as_str().expect("the key it was shown");
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
        assert!(answer.get("hostKey").is_none(), "nothing is stored, so nothing reads as stored");
        let stored = store.lock().unwrap().connection(&conn.id).expect("the row");
        assert_eq!(stored.host_key, None, "a Test pins nothing");

        // Pinning what it reported is the confirmation: the same key passes and reports nothing.
        store
            .lock()
            .unwrap()
            .set_host_key(&conn.id, fingerprint)
            .expect("the pin");
        let again = m.test_connection(&conn.id).await.expect("a test answer");
        assert_eq!(again["ok"], json!(true), "{again}");
        assert!(again.get("fingerprint").is_none(), "{again}");
    }

    /// SPEC §tunnels.store: the host key a Test met differs from the pinned one, and the answer
    /// names what it was shown, which is what the panel's Trust stores.
    #[tokio::test]
    async fn a_test_on_the_wrong_pin_reports_what_it_met() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let port = crate::tunnel::sshtest::spawn_ssh_server().await;
        let conn = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "pinned".into(),
                host: "127.0.0.1".into(),
                port: f64::from(port),
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                host_key: Some("SHA256:not-this-key".into()),
                ..Default::default()
            })
            .expect("the connection");

        let answer = m.test_connection(&conn.id).await.expect("a test answer");

        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert_eq!(answer["kind"], json!("hostkey"), "{answer}");
        let met = answer["fingerprint"].as_str().expect("the key it met");
        assert!(met.starts_with("SHA256:") && met != "SHA256:not-this-key", "{met}");
    }

    #[tokio::test]
    async fn restarts_a_running_rule_on_the_new_port_and_frees_the_old_one() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let old_port = free_port().await;
        let new_port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, old_port, echo));
        m.start_rule(&r.id).await.expect("start");

        m.apply_rule_update(&r.id, &rule_input("pg", &c.id, new_port, echo))
            .await
            .expect("edit");
        assert_eq!(state_of(&m, &r.id), "up");
        assert!(port_is_free(old_port).await);
        assert_eq!(round_trip(new_port, "hi").await, "HI");
        m.close_all().await;
    }

    #[tokio::test]
    async fn leaves_a_stopped_rule_stopped_after_an_edit() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.apply_rule_update(&r.id, &rule_input("pg2", &c.id, port, echo))
            .await
            .expect("edit");
        assert_eq!(state_of(&m, &r.id), "stopped");
        assert!(port_is_free(port).await);
        assert_eq!(built_len(&built), 0, "an edit must not dial");
    }

    #[tokio::test]
    async fn restarts_only_the_rules_that_were_running_when_a_connection_is_edited() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let p1 = free_port().await;
        let p2 = free_port().await;
        let running = add_rule(&store, rule_input("on", &c.id, p1, echo));
        let stopped = add_rule(&store, rule_input("off", &c.id, p2, echo));
        m.start_rule(&running.id).await.expect("start");

        m.apply_connection_update(
            &c.id,
            &ConnInput {
                username: "someone-else".into(),
                ..conn_input()
            },
        )
        .await
        .expect("edit the connection");

        assert_eq!(state_of(&m, &running.id), "up");
        assert_eq!(
            state_of(&m, &stopped.id),
            "stopped",
            "a rule the user stopped must not come up because a neighbour was edited"
        );
        // The old client was ended and a new one dialed with the new definition.
        assert_eq!(built_len(&built), 2);
        assert_eq!(nth(&built, 0).ended(), 1);
        assert_eq!(nth(&built, 1).username(), "someone-else");
        m.close_all().await;
    }

    #[tokio::test]
    async fn refuses_to_delete_a_connection_with_rules_and_succeeds_once_they_are_gone() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        // Structured, like rule deletion — the API answers 409 + a confirm, not a flat string.
        match m.delete_connection(&c.id).await {
            Err(OpError::Dependents(names)) => assert_eq!(names, vec!["pg".to_string()]),
            other => panic!("expected a Dependents refusal, got {other:?}"),
        }
        m.delete_rule(&r.id, false).await.expect("delete the rule");
        m.delete_connection(&c.id)
            .await
            .expect("delete the connection");
        assert!(store.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn frees_the_port_when_a_running_rule_is_deleted() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));
        m.start_rule(&r.id).await.expect("start");

        m.delete_rule(&r.id, false).await.expect("delete");
        assert!(port_is_free(port).await);
        assert!(store.lock().unwrap().rules().is_empty());
    }
}
