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

//! Reporting: whether a rule is active, and the rule and connection rows the panel reads.

use serde_json::{json, Map, Value};

use crate::tunnel::types::{RuleDef, RuleState};
use super::TunnelManager;
use swiss_host::config::is_env_ref;
use swiss_host::mask::MASK;

/// What a row shows for a stored credential: a reference is not the secret and rides as itself,
/// and anything else is the sentinel an edit sends back through unmask_conn.
fn masked_credential(stored: &str) -> &str {
    if stored.is_empty() || is_env_ref(&Value::String(stored.to_string())) {
        stored
    } else {
        MASK
    }
}

impl TunnelManager {
    pub fn is_active(&self, id: &str) -> bool {
        let state = self
            .runtimes
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .and_then(|rt| rt.data.lock().ok().map(|d| d.state));
        matches!(
            state,
            Some(RuleState::Up) | Some(RuleState::Starting) | Some(RuleState::Reconnecting)
        )
    }

    /// One rule's API row (definition + live state), or None when the rule no longer exists.
    pub fn rule_row(&self, id: &str) -> Option<Value> {
        let rule = self.with_store(|s| s.rule(id))?;
        Some(self.rule_row_of(&rule))
    }

    fn rule_row_of(&self, rule: &RuleDef) -> Value {
        let rt = self.rt(&rule.id);
        let (state, reason_field, started_at, reconnected_at, port_owner, stats) = {
            let data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            let stats = data.forward.as_ref().map(|f| f.stats());
            (
                data.state,
                data.reason.clone(),
                data.started_at.clone(),
                data.reconnected_at.clone(),
                data.port_owner.clone(),
                stats,
            )
        };
        let conn_name = self
            .with_store(|s| s.connection(&rule.connection_id))
            .map(|c| c.name)
            .unwrap_or_else(|| "(unknown)".into());
        let mut row = rule.to_json().as_object().cloned().unwrap_or_default();
        row.insert("state".into(), json!(state.as_str()));
        // rt.reason wins; a live listener's lastError fills in when the rule has no reason.
        let reason = match (&reason_field, &stats) {
            (Some(r), _) => Some(r.clone()),
            (None, Some(s)) => s.last_error.clone(),
            (None, None) => None,
        };
        if let Some(r) = reason {
            row.insert("reason".into(), json!(r));
        }
        row.insert("connectionName".into(), json!(conn_name));
        if let Some(at) = started_at {
            row.insert("startedAt".into(), json!(at));
        }
        if let Some(at) = reconnected_at {
            row.insert("reconnectedAt".into(), json!(at));
        }
        row.insert(
            "sockets".into(),
            json!(stats.as_ref().map_or(0, |s| s.sockets)),
        );
        row.insert(
            "bytesIn".into(),
            json!(stats.as_ref().map_or(0, |s| s.bytes_in)),
        );
        row.insert(
            "bytesOut".into(),
            json!(stats.as_ref().map_or(0, |s| s.bytes_out)),
        );
        row.insert(
            "channelFailures".into(),
            json!(stats.as_ref().map_or(0, |s| s.channel_failures)),
        );
        if let Some(owner) = port_owner {
            row.insert("portOwner".into(), owner.to_json());
        }
        // Linked MCPs with their live state; known:false means the name has no registered MCP.
        let mcp_rows: Vec<Value> = rule
            .mcps
            .iter()
            .map(|name| {
                let view = self.mcps.as_ref();
                json!({
                    "name": name,
                    "state": view.and_then(|v| v.state_of(name)).unwrap_or_else(|| "unknown".into()),
                    "known": view.map(|v| v.has(name)).unwrap_or(false),
                })
            })
            .collect();
        row.insert("mcpRows".into(), Value::Array(mcp_rows));
        Value::Object(row)
    }

    /// Both lists in one in-memory read — safe for the panel's poll.
    pub fn rows(&self) -> Value {
        let all_rules = self.with_store(|s| s.rules());
        let rules: Vec<Value> = all_rules.iter().map(|r| self.rule_row_of(r)).collect();
        let connections: Vec<Value> = self
            .with_store(|s| s.connections())
            .iter()
            .map(|c| {
                let live = self
                    .conns
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&c.id)
                    .cloned();
                // Rows of THIS connection, for the counts.
                let mine: Vec<&RuleDef> = all_rules
                    .iter()
                    .filter(|r| r.connection_id == c.id)
                    .collect();
                let active = mine
                    .iter()
                    .filter(|r| {
                        matches!(
                            self.rt(&r.id).data.lock().map(|d| d.state),
                            Ok(RuleState::Up)
                        )
                    })
                    .count();
                let mut row = Map::new();
                row.insert("id".into(), json!(c.id));
                row.insert("name".into(), json!(c.name));
                row.insert("host".into(), json!(c.host));
                row.insert("port".into(), json!(c.port));
                row.insert("username".into(), json!(c.username));
                row.insert("authType".into(), json!(c.auth_type.as_str()));
                if let Some(g) = &c.group {
                    row.insert("group".into(), json!(g));
                }
                row.insert(
                    "state".into(),
                    json!(live.as_ref().map(|l| l.state().as_str()).unwrap_or("idle")),
                );
                if let Some(reason) = live.as_ref().and_then(|l| l.reason()) {
                    row.insert("reason".into(), json!(reason));
                }
                if let Some(hk) = &c.host_key {
                    row.insert("hostKey".into(), json!(hk));
                }
                row.insert("ruleCount".into(), json!(mine.len()));
                row.insert("activeRules".into(), json!(active));
                // SPEC §tunnels.panel: the panel's READ surface for the proxy/jump fields is
                // this row (the POST/PUT echo is read once and dropped). Proxy, proxyUsername
                // and the jump id ride plaintext, absent-when-unset — the panel resolves the
                // jump id to a name from this same list. proxyPassword rides as the MASK
                // sentinel, the same rule mask_conn applies to the POST/PUT echo: the row
                // never carries a secret, and an edit that leaves the field untouched sends
                // the sentinel back through unmask_conn, keeping the stored value. The
                // connection's key path joins this read surface too: plaintext, absent
                // when unset — the edit sheet prefills it from the row.
                if let Some(url) = &c.proxy {
                    row.insert("proxy".into(), json!(url));
                }
                if let Some(u) = &c.proxy_username {
                    row.insert("proxyUsername".into(), json!(u));
                }
                if let Some(pw) = &c.proxy_password {
                    row.insert("proxyPassword".into(), json!(masked_credential(pw)));
                }
                if let Some(jump) = &c.jump {
                    row.insert("jump".into(), json!(jump));
                }
                // The key path rides the same read surface: the edit sheet prefills from
                // this row, so a custom path must arrive here or a save would rewrite it
                // to the default. Plaintext (a path, not a secret), absent when unset,
                // appended after the frozen prefix — the SPEC §tunnels.panel rule.
                if let Some(kp) = &c.key_path {
                    row.insert("keyPath".into(), json!(kp));
                }
                // The two credentials ride by proxyPassword's rule. The edit sheet echoes what
                // the row gave it, so a row without them sent an empty field back, which
                // destroyed a stored passphrase and refused a save on password auth. A manifest
                // compares a reference against them too.
                for (key, value) in [("passphrase", &c.passphrase), ("password", &c.password)] {
                    if let Some(value) = value {
                        row.insert(key.into(), json!(masked_credential(value)));
                    }
                }
                Value::Object(row)
            })
            .collect();
        json!({ "connections": Value::Array(connections), "rules": Value::Array(rules) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tunnel::manager::test_util::{add_conn, manager, scratch};
    use crate::tunnel::store::ConnInput;
    use crate::tunnel::types::AuthType;

    /// SPEC §tunnels.panel: the connection row is the panel's READ surface for the
    /// proxy/jump fields — the sheet's echo and the row badges render from rows(), not
    /// from the POST/PUT response the panel reads once and drops. Plaintext,
    /// absent-when-unset, for proxy/proxyUsername/jump (the jump is an id; the panel
    /// resolves the name from this same list) and the MASK sentinel for a set
    /// proxyPassword — an env ref passes, the reference is not the secret — so an
    /// untouched edit echoes the sentinel back through unmask_conn and keeps the stored
    /// secret. The new keys append after the existing ones; the historical prefix is
    /// frozen. The connection's key path follows the same rule — the edit sheet prefills
    /// from this row, so a custom path must ride along or a save would rewrite it to the
    /// default.
    #[test]
    fn connection_rows_carry_the_proxy_jump_and_key_path_fields_for_the_panel() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let _plain = add_conn(&store);
        let _clash = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "clash".into(),
                host: "10.0.0.2".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                proxy: Some("socks5://127.0.0.1".into()), // portless: stored as :1080
                proxy_username: Some("pxuser".into()),
                proxy_password: Some("px-pass".into()),
                ..Default::default()
            })
            .expect("the proxied connection");
        let bastion = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "bastion".into(),
                host: "10.0.0.3".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                ..Default::default()
            })
            .expect("the jump host");
        let _db = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "db".into(),
                host: "10.0.0.9".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                jump: Some(bastion.id.clone()),
                ..Default::default()
            })
            .expect("the jumping connection");
        let _refconn = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "refproxy".into(),
                host: "10.0.0.4".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                proxy: Some("http://proxy.lan".into()),
                proxy_password: Some("${secret://proxy-pass}".into()),
                ..Default::default()
            })
            .expect("the env-ref connection");
        let _keyed = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "keyed".into(),
                host: "10.0.0.5".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Key,
                key_path: Some("~/.ssh/alt_ed25519".into()),
                ..Default::default()
            })
            .expect("the keyed connection");

        let rows = m.rows();
        let by_name = |n: &str| {
            rows["connections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == json!(n))
                .unwrap()
                .clone()
        };
        let keys = |c: &Value| c.as_object().unwrap().keys().cloned().collect::<Vec<_>>();

        // A plain row keeps the exact historical shape, then its credential: nothing else
        // appears. bastion is the plain case here — password auth, no proxy, no jump, no key
        // path; the shared add_conn helper happens to carry a key path, so it no longer
        // qualifies.
        let plain_row = by_name("bastion");
        assert_eq!(
            keys(&plain_row),
            vec![
                "id",
                "name",
                "host",
                "port",
                "username",
                "authType",
                "state",
                "ruleCount",
                "activeRules",
                "password",
            ]
        );
        assert_eq!(plain_row["id"], json!(bastion.id));
        assert_eq!(plain_row["password"], json!(swiss_host::mask::MASK));

        // The proxy trio: plaintext URL and username, the sentinel — never the secret.
        let clash_row = by_name("clash");
        assert_eq!(clash_row["proxy"], json!("socks5://127.0.0.1:1080"));
        assert_eq!(clash_row["proxyUsername"], json!("pxuser"));
        assert_eq!(clash_row["proxyPassword"], json!(swiss_host::mask::MASK));
        assert!(clash_row.get("jump").is_none());
        // Appended after the frozen prefix, in the §1.2 field order.
        let clash_keys = keys(&clash_row);
        assert_eq!(clash_keys[..9], keys(&plain_row)[..9]);
        assert_eq!(
            clash_keys[9..],
            vec!["proxy", "proxyUsername", "proxyPassword", "password"]
        );

        // The jump: a plaintext id, and no proxy keys on a jumping row.
        let db_row = by_name("db");
        assert_eq!(db_row["jump"], json!(bastion.id));
        assert!(db_row.get("proxy").is_none());
        assert!(db_row.get("proxyPassword").is_none());

        // An env-ref proxy password is a reference, not a secret: it rides as itself.
        let ref_row = by_name("refproxy");
        assert_eq!(ref_row["proxyPassword"], json!("${secret://proxy-pass}"));

        // The key path: absent-when-unset on a plain row, present and plaintext on a keyed
        // one, appended after the frozen prefix — the edit sheet prefills from this row, so
        // a save must never rewrite a custom path to the default.
        let keyed_row = by_name("keyed");
        assert_eq!(keyed_row["keyPath"], json!("~/.ssh/alt_ed25519"));
        assert!(plain_row.get("keyPath").is_none());
        let keyed_keys = keys(&keyed_row);
        assert_eq!(keyed_keys[..9], keys(&plain_row)[..9]);
        assert_eq!(keyed_keys[9..], vec!["keyPath"]);
    }

    /// SPEC §tunnels.store: "editing an unrelated field never destroys a stored secret". The
    /// edit sheet echoes what the row gave it, so a row holding no passphrase sent an empty one
    /// back. A literal rides as the sentinel and a reference as itself, and neither stored value
    /// can reach a row as plaintext.
    #[test]
    fn a_row_carries_a_credential_by_the_same_rule_as_the_proxy_password() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let add = |name: &str, auth_type: AuthType, passphrase: Option<&str>, password: Option<&str>| {
            store
                .lock()
                .unwrap()
                .add_connection(&ConnInput {
                    name: name.into(),
                    host: "10.0.0.7".into(),
                    port: 22.0,
                    username: "u".into(),
                    auth_type,
                    key_path: (auth_type == AuthType::Key).then(|| "~/.ssh/id".to_string()),
                    passphrase: passphrase.map(str::to_string),
                    password: password.map(str::to_string),
                    ..Default::default()
                })
                .expect("the connection")
        };
        add("literal-key", AuthType::Key, Some("key-words"), None);
        add("reference-key", AuthType::Key, Some("${secret://key-pass}"), None);
        add("literal-password", AuthType::Password, None, Some("pass-words"));
        add("reference-password", AuthType::Password, None, Some("${secret://ssh-pass}"));

        let rows = m.rows();
        let row = |name: &str| {
            rows["connections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == json!(name))
                .unwrap()
                .clone()
        };
        assert_eq!(row("literal-key")["passphrase"], json!(MASK));
        assert_eq!(row("reference-key")["passphrase"], json!("${secret://key-pass}"));
        assert_eq!(row("literal-password")["password"], json!(MASK));
        assert_eq!(row("reference-password")["password"], json!("${secret://ssh-pass}"));
        assert!(row("literal-key").get("password").is_none());
        assert!(row("literal-password").get("passphrase").is_none());
        let printed = rows.to_string();
        assert!(!printed.contains("key-words") && !printed.contains("pass-words"), "{printed}");
    }
}
