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

//! One-shot import of forward-port's config, read-only — port of `tunnels/import.ts`.
//!
//! Every rule arrives STOPPED on purpose: forward-port.exe may still be running and holding the
//! local ports, and a gateway that started every imported tunnel on boot would collide with it
//! and report a wall of failures on its first run. The user starts them once the old tool is
//! retired.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::store::TunnelStore;
use super::types::{AuthType, RuleDef, SshConnDef};
use swiss_core::log;

/// Where forward-port keeps its config on Windows.
pub fn forward_port_config_path() -> PathBuf {
    let app_data = std::env::var("APPDATA").unwrap_or_default();
    if app_data.is_empty() {
        return PathBuf::new();
    }
    PathBuf::from(app_data)
        .join("forward-port")
        .join("config.json")
}

fn num(v: &Value, fallback: u16) -> u16 {
    let n = super::types::value_number(v);
    if n.fract() == 0.0 && (1.0..=65535.0).contains(&n) {
        n as u16
    } else {
        fallback
    }
}

fn truthy_str(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| !s.is_empty())
}

/// Import the forward-port config into `store`. None when there is nothing to adopt (no path,
/// unreadable file, or nothing usable in it); counts on success.
pub fn import_forward_port(store: &mut TunnelStore, path: Option<&Path>) -> Option<(usize, usize)> {
    let path = path?;
    if path.as_os_str().is_empty() || !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let raw: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(err) => {
            log::warn(
                "forward-port import failed to parse",
                Some(json!({ "err": err.to_string(), "path": path.display().to_string() })),
            );
            return None;
        }
    };

    let mut conns: Vec<SshConnDef> = Vec::new();
    for c in raw
        .get("connections")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(id) = c.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !truthy_str(c.get("name")) || !truthy_str(c.get("host")) {
            continue;
        }
        let auth_type = if c.get("auth_type").and_then(Value::as_str) == Some("password") {
            AuthType::Password
        } else {
            AuthType::Key
        };
        let mut def = SshConnDef {
            id: id.to_string(),
            name: c
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            host: c
                .get("host")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            port: num(c.get("port").unwrap_or(&Value::Null), 22),
            username: c
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            auth_type,
            group: None,
            key_path: None,
            passphrase: None,
            password: None,
            host_key: None,
            // The forward-port config has no proxy/jump vocabulary (docs/27 §1.1).
            proxy: None,
            proxy_username: None,
            proxy_password: None,
            jump: None,
        };
        match auth_type {
            AuthType::Key => {
                def.key_path = Some(
                    c.get("key_path")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("~/.ssh/id_rsa")
                        .to_string(),
                );
                if let Some(pass) = c.get("key_passphrase").and_then(Value::as_str) {
                    if !pass.is_empty() {
                        def.passphrase = Some(pass.to_string());
                    }
                }
            }
            AuthType::Password => {
                def.password = Some(
                    c.get("password")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        conns.push(def);
    }

    let mut rules: Vec<RuleDef> = Vec::new();
    let mut seen_ports: Vec<u16> = Vec::new();
    for t in raw
        .get("tunnels")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(id) = t.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !truthy_str(t.get("name")) {
            continue;
        }
        let local_port = num(t.get("local_port").unwrap_or(&Value::Null), 0);
        if local_port == 0 {
            continue;
        }
        // The old tool tolerated both of these; this store does not, and one bad row must not
        // abort the other 17 imports — so they are skipped with a line in the log.
        let connection_id = t
            .get("connection_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !conns.iter().any(|c| c.id == connection_id) {
            log::warn(
                "import skipped rule with unknown connection",
                Some(json!({ "rule": t.get("name").and_then(Value::as_str).unwrap_or_default() })),
            );
            continue;
        }
        if seen_ports.contains(&local_port) {
            log::warn(
                "import skipped rule with a duplicate local port",
                Some(json!({
                    "rule": t.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "localPort": local_port,
                })),
            );
            continue;
        }
        seen_ports.push(local_port);
        rules.push(RuleDef {
            id: id.to_string(),
            name: t
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            connection_id,
            local_port,
            target_host: t
                .get("target_host")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("127.0.0.1")
                .to_string(),
            target_port: num(t.get("target_port").unwrap_or(&Value::Null), local_port),
            remark: t
                .get("remark")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            auto_reconnect: t.get("auto_reconnect") == Some(&Value::Bool(true)),
            reconnect_interval: u64::from(
                num(t.get("reconnect_interval").unwrap_or(&json!(10)), 10).max(1),
            ),
            enabled: false,
            mcps: Vec::new(),
            group: None,
        });
    }

    if conns.is_empty() && rules.is_empty() {
        return None;
    }
    let counts = (conns.len(), rules.len());
    if let Err(err) = store.replace_all(conns, rules) {
        log::warn(
            "forward-port import failed to save",
            Some(json!({ "err": err, "path": path.display().to_string() })),
        );
        return None;
    }
    log::log(
        "info",
        "tunnels imported from forward-port",
        Some(json!({
            "connections": counts.0,
            "rules": counts.1,
            "path": path.display().to_string(),
        })),
    );
    Some(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(name: &str, body: &str) -> (std::path::PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "swiss-import-{}-{}",
            name,
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, body).unwrap();
        (dir, cfg)
    }

    fn store_for(_name: &str) -> (std::path::PathBuf, TunnelStore) {
        let dir = std::env::temp_dir().join(format!(
            "swiss-import-store-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (
            dir.clone(),
            TunnelStore::new(dir.join("tunnels.json"), 19999),
        )
    }

    #[test]
    fn imports_snake_case_rows_stopped() {
        let (_dir, cfg) = write_config(
            "basic",
            r#"{
  "connections": [
    { "id": "c1", "name": "bastion", "host": "10.0.0.9", "port": 2222, "username": "root", "auth_type": "key", "key_path": "C:/keys/x.pem", "key_passphrase": "pp" },
    { "id": "c2", "name": "pw", "host": "h2", "auth_type": "password", "password": "sekret" },
    { "id": "c3", "name": "", "host": "x" }
  ],
  "tunnels": [
    { "id": "t1", "name": "pg", "connection_id": "c1", "local_port": 5433, "target_port": 5432, "auto_reconnect": true },
    { "id": "t2", "name": "default-key", "connection_id": "c1", "local_port": 6379 },
    { "id": "t3", "name": "dupe", "connection_id": "c1", "local_port": 5433 },
    { "id": "t4", "name": "orphan", "connection_id": "nope", "local_port": 1234 },
    { "id": "t5", "name": "badport", "connection_id": "c1", "local_port": 0 }
  ]
}"#,
        );
        let (_dir, mut store) = store_for("basic");
        let imported = import_forward_port(&mut store, Some(&cfg)).expect("imports");
        assert_eq!(imported, (2, 2)); // c3 skipped; t3/t4/t5 skipped
        let conns = store.connections();
        assert_eq!(conns[0].key_path.as_deref(), Some("C:/keys/x.pem"));
        assert_eq!(conns[0].passphrase.as_deref(), Some("pp"));
        assert_eq!(conns[1].password.as_deref(), Some("sekret"));
        let rules = store.rules();
        assert!(
            rules.iter().all(|r| !r.enabled),
            "every rule arrives STOPPED"
        );
        let t1 = &rules[0];
        assert_eq!(t1.target_port, 5432);
        assert!(t1.auto_reconnect);
        assert_eq!(t1.reconnect_interval, 10, "absent interval defaults to 10");
        let t2 = &rules[1];
        assert_eq!(
            t2.target_port, 6379,
            "target port falls back to the local port"
        );
    }

    #[test]
    fn missing_file_and_empty_config_import_nothing() {
        let (_dir, mut store) = store_for("none");
        assert!(import_forward_port(&mut store, Some(Path::new("Z:/nope/nope.json"))).is_none());
        let (_dir2, cfg) = write_config("empty", r#"{ "connections": [], "tunnels": [] }"#);
        assert!(import_forward_port(&mut store, Some(&cfg)).is_none());
    }

    #[test]
    fn default_key_path_is_assigned_when_absent() {
        let (_dir, cfg) = write_config(
            "defaultkey",
            r#"{ "connections": [ { "id": "c1", "name": "b", "host": "h", "username": "u" } ],
                 "tunnels": [ { "id": "t1", "name": "r", "connection_id": "c1", "local_port": 7000 } ] }"#,
        );
        let (_dir, mut store) = store_for("defaultkey");
        import_forward_port(&mut store, Some(&cfg)).unwrap();
        let conns = store.connections();
        assert_eq!(conns[0].key_path.as_deref(), Some("~/.ssh/id_rsa"));
        assert_eq!(conns[0].auth_type, AuthType::Key);
    }
}
