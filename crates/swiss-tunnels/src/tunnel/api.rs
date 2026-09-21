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

//! The tunnel API under /api/tunnels — port of `tunnels/api.ts`.
//!
//! Mounted behind the same loopback boundary every /api route sits behind (the app-level guard;
//! the Node build's `authed` wrapper has no per-route counterpart in this build, since adminapi
//! has none either — the router boundary IS the gate). Every response is shape-identical to
//! the Node build's: camelCase, absent-not-null, the panel JS is the spec.
//!
//! `mcp_display` is optional and read-only here: it feeds the "Serves MCPs" suggestion and the
//! rows' MCP name column. Nothing in this file starts, stops or restarts an MCP.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::Router;
use serde_json::{json, Map, Value};

use super::manager::{OpError, TunnelManager};
use super::port::{force_free, port_owner, probe_port};
use super::store::{ConnInput, RuleInput, TunnelStore};
use super::types::{value_number, AuthType, GroupKind, SshConnDef};
use swiss_core::paths::home_dir;
use swiss_host::config::ServerDef;
use swiss_host::mask::{mask_def, unmask_body, MASK};
use swiss_host::reply::{admin_error, admin_json};

/// The read-only MCP world the tunnel API displays: the rows' MCP names and the rule editor's
/// loopback-port suggestion. Composition supplies the registry-backed impl; a registry-less
/// composition runs without it. Display and guard rail only — nothing here can start or stop
/// an MCP, which is what keeps the tunnel subsystem independent of the registry's type.
pub trait McpDisplay: Send + Sync {
    /// Every known MCP name.
    fn names(&self) -> Vec<String>;
    /// Which MCPs point at this local port — the editor's pre-checked suggestion.
    fn suggest(&self, local_port: u16) -> Vec<String>;
}

/// The state the tunnel routes carry: everything the panel's tunnels page talks to.
pub struct Tunnels {
    pub store: Arc<Mutex<TunnelStore>>,
    pub manager: Arc<TunnelManager>,
    /// Read-only; feeds the MCP suggestion list and the rows' MCP names.
    pub mcp_display: Option<Arc<dyn McpDisplay>>,
}

fn with_store<T>(t: &Arc<Tunnels>, f: impl FnOnce(&mut TunnelStore) -> T) -> T {
    let mut store = t.store.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut store)
}

/// Secrets go out masked and come back restored, reusing the MCP panel's sentinel round-trip:
/// the browser never receives a password or a passphrase, and editing an unrelated field cannot
/// destroy one. mask_def/unmask_body work on ServerDef-shaped records, which a connection is.
fn mask_conn(def: &SshConnDef) -> Value {
    let def_value = def.to_json();
    let mut server_def = ServerDef(def_value.as_object().cloned().unwrap_or_default());
    // docs/27 §1.4: the host masker's secret-key list is an EXACT match ("password",
    // "pass", …), so proxyPassword would ride out in the clear. The sentinel is applied
    // here instead — mask.rs itself stays untouched, and a whole-value `${...}` reference
    // still passes through (the reference is not the secret).
    let pw = server_def.0.get("proxyPassword").cloned();
    if let Some(pw) = pw {
        if !swiss_host::config::is_env_ref(&pw) && pw.as_str().is_some_and(|s| !s.is_empty()) {
            server_def
                .0
                .insert("proxyPassword".into(), Value::String(MASK.into()));
        }
    }
    serde_json::to_value(mask_def(&server_def)).unwrap_or(def_value)
}

fn unmask_conn(body: &Map<String, Value>, current: Option<&SshConnDef>) -> Map<String, Value> {
    let current_def = current.map(|c| {
        let v = c.to_json();
        ServerDef(v.as_object().cloned().unwrap_or_default())
    });
    // Restore a sentinel proxyPassword from the stored def before the generic pass; the
    // pair of the masking above. An unrestorable sentinel is dropped by unmask_body's
    // drop_sentinel, exactly like every other secret field.
    let mut body = body.clone();
    if let (Some(cur), Some(sent)) = (current_def.as_ref(), body.get("proxyPassword")) {
        if sent.as_str() == Some(MASK) {
            if let Some(stored) = cur.0.get("proxyPassword") {
                body.insert("proxyPassword".into(), stored.clone());
            }
        }
    }
    unmask_body(&body, current_def.as_ref())
}

/// `?force=1` or a `{force: true}` body — both spellings, so no caller has to guess.
fn wants_force(query: &HashMap<String, String>, body: &Value) -> bool {
    query.get("force").map(String::as_str) == Some("1")
        || body.get("force") == Some(&Value::Bool(true))
}

/// The one error funnel: Dependents -> 409 + the structured confirm body; a message starting
/// with "unknown " -> 404 (case-insensitively, like the Node regex); anything else -> 400.
fn fail(err: &OpError) -> Response {
    match err {
        OpError::Dependents(dependents) => admin_json(
            StatusCode::CONFLICT,
            json!({
                "error": err.message(),
                "dependents": dependents,
                "confirmRequired": true,
            }),
        ),
        OpError::Msg(message) => {
            let unknown = message.to_ascii_lowercase().starts_with("unknown ");
            admin_error(
                if unknown {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::BAD_REQUEST
                },
                message,
            )
        }
    }
}

fn fail_str(err: &str) -> Response {
    fail(&OpError::msg(err))
}

// --- request-body shaping --------------------------------------------------------------------------

/// The rule body the Node api built before handing it to the store. Numbers keep JS `Number()`
/// semantics (absent -> NaN), because the store's error strings render the coerced value.
fn rule_input(body: &Value) -> RuleInput {
    RuleInput {
        id: None,
        name: body
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        connection_id: body
            .get("connectionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        local_port: body.get("localPort").map_or(f64::NAN, value_number),
        target_host: body
            .get("targetHost")
            .and_then(Value::as_str)
            .unwrap_or("127.0.0.1")
            .to_string(),
        target_port: body.get("targetPort").map_or(f64::NAN, value_number),
        remark: body
            .get("remark")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        auto_reconnect: body.get("autoReconnect") == Some(&Value::Bool(true))
            || body.get("autoReconnect") == Some(&json!("true")),
        reconnect_interval: body.get("reconnectInterval").map_or(10.0, value_number),
        enabled: None, // never taken from a request; the manager owns it
        // Present only when the caller sent a list: an omitted key means "keep the stored
        // links" (RuleInput.mcps is optional for exactly this), an explicit [] clears them.
        mcps: body.get("mcps").and_then(Value::as_array).map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .collect()
        }),
        group: body
            .get("group")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn conn_input(body: &Map<String, Value>) -> ConnInput {
    ConnInput {
        id: None,
        name: body
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        host: body
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        port: body.get("port").map_or(22.0, value_number),
        username: body
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        auth_type: if body.get("authType").and_then(Value::as_str) == Some("password") {
            AuthType::Password
        } else {
            AuthType::Key
        },
        key_path: body
            .get("keyPath")
            .and_then(Value::as_str)
            .map(str::to_string),
        passphrase: body
            .get("passphrase")
            .and_then(Value::as_str)
            .map(str::to_string),
        password: body
            .get("password")
            .and_then(Value::as_str)
            .map(str::to_string),
        host_key: body
            .get("hostKey")
            .and_then(Value::as_str)
            .map(str::to_string),
        group: body
            .get("group")
            .and_then(Value::as_str)
            .map(str::to_string),
        // docs/27 §1.1: the proxy/jump fields, absent when the panel did not send them.
        proxy: body
            .get("proxy")
            .and_then(Value::as_str)
            .map(str::to_string),
        proxy_username: body
            .get("proxyUsername")
            .and_then(Value::as_str)
            .map(str::to_string),
        proxy_password: body
            .get("proxyPassword")
            .and_then(Value::as_str)
            .map(str::to_string),
        jump: body.get("jump").and_then(Value::as_str).map(str::to_string),
    }
}

// --- key browsing ------------------------------------------------------------------------------------

/// Private keys sitting in ~/.ssh — what the panel's Browse button offers.
fn list_keys() -> Vec<Value> {
    let dir = home_dir().join(".ssh");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".pub")
            || name == "known_hosts"
            || name == "config"
            || name == "authorized_keys"
        {
            continue;
        }
        // /^id_/ or *.pem or *.key — hand-rolled (ADR-007).
        if !name.starts_with("id_") && !name.ends_with(".pem") && !name.ends_with(".key") {
            continue;
        }
        if !entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
            continue;
        }
        out.push(json!({ "path": entry.path().to_string_lossy(), "name": name }));
    }
    out
}

pub struct DirEntry {
    pub name: String,
    pub path: String,
    /// true for a directory (navigable), false for a file (pickable).
    pub dir: bool,
}

pub struct DirListing {
    /// The resolved directory this listing describes.
    pub dir: String,
    /// The parent directory, for the Up button — absent at the filesystem root.
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
    /// Set when the directory could not be read (missing, not a dir, no permission).
    pub error: Option<String>,
}

impl DirListing {
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("dir".into(), json!(self.dir));
        if let Some(parent) = &self.parent {
            m.insert("parent".into(), json!(parent));
        }
        m.insert(
            "entries".into(),
            Value::Array(
                self.entries
                    .iter()
                    .map(|e| json!({ "name": e.name, "path": e.path, "dir": e.dir }))
                    .collect(),
            ),
        );
        if let Some(error) = &self.error {
            m.insert("error".into(), json!(error));
        }
        Value::Object(m)
    }
}

/// List one directory for the key-file browser: subdirectories (navigable) and files
/// (pickable), with a parent to go up. Directories come first, then files, each alphabetical.
///
/// A browser file picker cannot return a real server-side path (only a bare filename), so
/// Browse is served from here. Unlike `/keys` — which only knows `~/.ssh` — this reaches any
/// path the gateway process can read, so a key kept outside `~/.ssh` is still pickable.
pub fn list_dir(dir: &str) -> DirListing {
    let abs = match std::path::absolute(dir) {
        Ok(p) => p,
        Err(err) => {
            return DirListing {
                dir: dir.to_string(),
                parent: None,
                entries: Vec::new(),
                error: Some(err.to_string()),
            }
        }
    };
    let entries = match std::fs::read_dir(&abs) {
        Ok(iter) => {
            let mut out: Vec<DirEntry> = iter
                .flatten()
                .map(|entry| DirEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: entry.path().to_string_lossy().to_string(),
                    dir: entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                })
                .collect();
            // Directories first, then files, each alphabetical (localeCompare's intent).
            out.sort_by(|a, b| {
                b.dir
                    .cmp(&a.dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            out
        }
        Err(err) => {
            return DirListing {
                dir: abs.to_string_lossy().to_string(),
                parent: None,
                entries: Vec::new(),
                error: Some(err.to_string()),
            }
        }
    };
    let parent = abs
        .parent()
        .map(|p| {
            let value = p.to_string_lossy().to_string();
            if cfg!(windows) && !value.ends_with('\\') {
                format!("{value}\\")
            } else {
                value
            }
        })
        .filter(|p| {
            !p.is_empty()
                && p.trim_end_matches(['\\', '/'])
                    != abs.to_string_lossy().trim_end_matches(['\\', '/'])
        });
    DirListing {
        dir: abs.to_string_lossy().to_string(),
        parent,
        entries,
        error: None,
    }
}

fn parse_port(seg: &str) -> Option<u16> {
    seg.parse::<u16>().ok().filter(|p| *p >= 1)
}

// --- mounting ---------------------------------------------------------------------------------------

/// Mount the tunnel API under /api/tunnels. The returned router is state-resolved (Router<()>),
/// so the caller merges it straight into the app router.
pub fn mount(tunnels: Arc<Tunnels>) -> Router {
    let mut r = Router::<Arc<Tunnels>>::new();

    // Everything both lists need, in one in-memory read — safe for the panel's poll.
    r = r.route(
        "/api/tunnels",
        get(|State(t): State<Arc<Tunnels>>| async move {
            let mut body = t.manager.rows();
            body["ruleGroups"] = json!(with_store(&t, |s| s.groups_of(GroupKind::Rules)));
            body["connGroups"] = json!(with_store(&t, |s| s.groups_of(GroupKind::Connections)));
            body["mcps"] = match &t.mcp_display {
                Some(view) => json!(view.names()),
                None => json!([]),
            };
            admin_json(StatusCode::OK, body)
        }),
    );

    // --- groups and order ---------------------------------------------------------------------------
    //
    // Retired (docs/20 §3): /api/tunnels/order, /api/tunnels/groups/{kind},
    // /api/tunnels/groups/{kind}/rename and /api/tunnels/groups/{kind}/{id} answered the
    // same store the /api/groups/{scope} family does, with a second set of grouping rules
    // that had already forked from managed.json's once. The family owns it now - conns and
    // rules register as scopes over this store (groups.rs) - and one protocol is what the
    // panel implements. Reading stays here: /api/tunnels above still answers ruleGroups/
    // connGroups so the poll keeps one round trip.
    // Private keys under ~/.ssh. A browser cannot hand a real path to the page, so Browse is
    // served from here instead of a file picker that would only ever return a bare filename.
    r = r.route(
        "/api/tunnels/keys",
        get(|State(t): State<Arc<Tunnels>>| async move {
            let _ = &t;
            let default_path = home_dir().join(".ssh").join("id_rsa");
            admin_json(
                StatusCode::OK,
                json!({ "keys": list_keys(), "defaultPath": default_path.to_string_lossy() }),
            )
        }),
    );

    // Browse any directory the gateway process can read, for the key picker — not just ~/.ssh.
    // `?dir=` defaults to ~/.ssh; an unreadable / non-directory path returns an error, not a throw.
    r = r.route(
        "/api/tunnels/browse",
        get(
            |State(t): State<Arc<Tunnels>>, Query(q): Query<HashMap<String, String>>| async move {
                let _ = &t;
                let dir = q
                    .get("dir")
                    .map(String::as_str)
                    .filter(|d| !d.is_empty())
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| home_dir().join(".ssh").to_string_lossy().to_string());
                admin_json(StatusCode::OK, list_dir(&dir).to_json())
            },
        ),
    );

    // Which MCPs point at this local port — the rule editor's pre-checked suggestion.
    r = r.route(
        "/api/tunnels/suggest/{port}",
        get(
            |State(t): State<Arc<Tunnels>>, Path(port): Path<String>| async move {
                let Some(port) = parse_port(&port) else {
                    return admin_error(StatusCode::BAD_REQUEST, &format!("invalid port: {port}"));
                };
                let mcps = match &t.mcp_display {
                    Some(view) => view.suggest(port),
                    None => Vec::new(),
                };
                admin_json(StatusCode::OK, json!({ "port": port, "mcps": mcps }))
            },
        ),
    );

    // --- connections -----------------------------------------------------------------------------------

    r = r.route(
        "/api/tunnels/connections",
        post(
            |State(t): State<Arc<Tunnels>>, body: swiss_host::reply::NodeBody| async move {
                let body = body.0;
                let input = match body.as_object() {
                    Some(obj) => conn_input(&unmask_conn(obj, None)),
                    None => conn_input(&Map::new()),
                };
                match with_store(&t, |s| s.add_connection(&input)) {
                    Ok(def) => admin_json(
                        StatusCode::CREATED,
                        json!({ "connection": mask_conn(&def) }),
                    ),
                    Err(err) => fail_str(&err),
                }
            },
        ),
    );

    r = r.route(
        "/api/tunnels/connections/{id}",
        put(
            |State(t): State<Arc<Tunnels>>,
             Path(id): Path<String>,
             body: swiss_host::reply::NodeBody| async move {
                let current = with_store(&t, |s| s.connection(&id));
                let Some(current) = current else {
                    return admin_error(
                        StatusCode::NOT_FOUND,
                        &format!("unknown SSH connection: {id}"),
                    );
                };
                let body = body.0;
                let input = match body.as_object() {
                    Some(obj) => conn_input(&unmask_conn(obj, Some(&current))),
                    None => conn_input(&Map::new()),
                };
                match t.manager.apply_connection_update(&id, &input).await {
                    Ok(def) => admin_json(StatusCode::OK, json!({ "connection": mask_conn(&def) })),
                    Err(err) => fail(&err),
                }
            },
        )
        .delete(
            |State(t): State<Arc<Tunnels>>, Path(id): Path<String>| async move {
                // Same funnel as rule deletion: Dependents -> 409 + dependents + confirmRequired,
                // "unknown …" -> 404, anything else -> 400.
                match t.manager.delete_connection(&id).await {
                    Ok(()) => admin_json(StatusCode::OK, json!({ "id": id, "deleted": true })),
                    Err(err) => fail(&err),
                }
            },
        ),
    );

    r = r.route(
        "/api/tunnels/connections/{id}/test",
        post(
            |State(t): State<Arc<Tunnels>>, Path(id): Path<String>| async move {
                match t.manager.test_connection(&id).await {
                    Ok(result) => admin_json(StatusCode::OK, result),
                    Err(err) => fail(&err),
                }
            },
        ),
    );

    r = r.route(
        "/api/tunnels/connections/{id}/trust",
        post(
            |State(t): State<Arc<Tunnels>>, Path(id): Path<String>| async move {
                match t.manager.trust_host_key(&id) {
                    Ok(host_key) => admin_json(
                        StatusCode::OK,
                        json!({ "id": id, "hostKey": host_key }), // absent -> null, as in Node
                    ),
                    Err(err) => fail(&err),
                }
            },
        ),
    );

    // --- rules ------------------------------------------------------------------------------------------

    r = r.route(
        "/api/tunnels/rules",
        post(
            |State(t): State<Arc<Tunnels>>, body: swiss_host::reply::NodeBody| async move {
                let body = body.0;
                let input = rule_input(&body);
                let id = match with_store(&t, |s| s.add_rule(&input)) {
                    Ok(def) => def.id,
                    Err(err) => return fail_str(&err),
                };
                // Start it now when asked, but a start failure must not undo a rule that saved
                // fine — it lands in `error` with its reason, which is what the panel shows.
                if body.get("start") == Some(&Value::Bool(true)) {
                    let _ = t.manager.start_rule(&id).await;
                }
                let row = t.manager.rule_row(&id);
                let mut out = Map::new();
                if let Some(row) = row {
                    out.insert("rule".into(), row);
                }
                admin_json(StatusCode::CREATED, Value::Object(out))
            },
        ),
    );

    r = r.route(
        "/api/tunnels/rules/{id}",
        put(
            |State(t): State<Arc<Tunnels>>,
             Path(id): Path<String>,
             body: swiss_host::reply::NodeBody| async move {
                if with_store(&t, |s| s.rule(&id)).is_none() {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown rule: {id}"));
                }
                let body = body.0;
                let input = rule_input(&body);
                if let Err(err) = t.manager.apply_rule_update(&id, &input).await {
                    return fail(&err);
                }
                let row = t.manager.rule_row(&id);
                let mut out = Map::new();
                if let Some(row) = row {
                    out.insert("rule".into(), row);
                }
                admin_json(StatusCode::OK, Value::Object(out))
            },
        )
        .delete(
            |State(t): State<Arc<Tunnels>>,
             Path(id): Path<String>,
             Query(q): Query<HashMap<String, String>>,
             body: swiss_host::reply::NodeBody| async move {
                let body = body.0;
                let force = wants_force(&q, &body);
                match t.manager.delete_rule(&id, force).await {
                    Ok(()) => admin_json(StatusCode::OK, json!({ "id": id, "deleted": true })),
                    Err(err) => fail(&err),
                }
            },
        ),
    );

    r = r.route(
        "/api/tunnels/rules/{id}/start",
        post(
            |State(t): State<Arc<Tunnels>>, Path(id): Path<String>| async move {
                match t.manager.start_rule(&id).await {
                    Ok(()) => {
                        let row = t.manager.rule_row(&id);
                        let mut out = Map::new();
                        if let Some(row) = row {
                            out.insert("rule".into(), row);
                        }
                        admin_json(StatusCode::OK, Value::Object(out))
                    }
                    Err(err) => {
                        let Some(row) = t.manager.rule_row(&id) else {
                            return fail(&err);
                        };
                        // A start failure is a result, not a transport error: the row carries the
                        // state, the reason and the port holder, which is exactly what the panel
                        // needs to offer Force free.
                        admin_json(
                            StatusCode::OK,
                            json!({ "rule": row, "ok": false, "error": err.message() }),
                        )
                    }
                }
            },
        ),
    );

    r = r.route(
        "/api/tunnels/rules/{id}/stop",
        post(
            |State(t): State<Arc<Tunnels>>,
             Path(id): Path<String>,
             Query(q): Query<HashMap<String, String>>,
             body: swiss_host::reply::NodeBody| async move {
                let body = body.0;
                let force = wants_force(&q, &body);
                if let Err(err) = t.manager.stop_rule(&id, true, force).await {
                    return fail(&err);
                }
                let row = t.manager.rule_row(&id);
                let mut out = Map::new();
                if let Some(row) = row {
                    out.insert("rule".into(), row);
                }
                admin_json(StatusCode::OK, Value::Object(out))
            },
        ),
    );

    r = r.route(
        "/api/tunnels/start-all",
        post(|State(t): State<Arc<Tunnels>>| async move {
            let results = t.manager.start_all().await;
            let rows: Vec<Value> = results.iter().map(|r| r.to_json()).collect();
            admin_json(StatusCode::OK, json!({ "results": rows }))
        }),
    );

    r = r.route(
        "/api/tunnels/stop-all",
        post(
            |State(t): State<Arc<Tunnels>>,
             Query(q): Query<HashMap<String, String>>,
             body: swiss_host::reply::NodeBody| async move {
                let body = body.0;
                match t.manager.stop_all(wants_force(&q, &body)).await {
                    Ok(results) => {
                        let rows: Vec<Value> = results.iter().map(|r| r.to_json()).collect();
                        admin_json(StatusCode::OK, json!({ "results": rows }))
                    }
                    Err(blocked) => fail(&OpError::Dependents(blocked)),
                }
            },
        ),
    );

    // --- ports -----------------------------------------------------------------------------------------

    r = r.route(
        "/api/tunnels/port/{port}",
        get(
            |State(t): State<Arc<Tunnels>>, Path(port): Path<String>| async move {
                let _ = &t;
                let Some(port) = parse_port(&port) else {
                    return admin_error(StatusCode::BAD_REQUEST, &format!("invalid port: {port}"));
                };
                let free = probe_port(port, "127.0.0.1").await;
                // Node wrote an explicit null owner when the port is not free but unidentifiable.
                let owner = if free {
                    Value::Null
                } else {
                    port_owner(port).await.map_or(Value::Null, |o| o.to_json())
                };
                admin_json(
                    StatusCode::OK,
                    json!({ "port": port, "free": free, "owner": owner }),
                )
            },
        ),
    );

    r = r.route(
        "/api/tunnels/port/{port}/free",
        post(
            |State(t): State<Arc<Tunnels>>, Path(port): Path<String>| async move {
                let _ = &t;
                let Some(port) = parse_port(&port) else {
                    return admin_error(StatusCode::BAD_REQUEST, &format!("invalid port: {port}"));
                };
                let Some(owner) = port_owner(port).await else {
                    return admin_error(
                        StatusCode::NOT_FOUND,
                        &format!("nothing is listening on port {port}"),
                    );
                };
                if let Err(err) = force_free(owner.pid).await {
                    return admin_json(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": err, "owner": owner.to_json() }),
                    );
                }
                admin_json(
                    StatusCode::OK,
                    json!({ "port": port, "killed": owner.to_json() }),
                )
            },
        ),
    );

    r.with_state(tunnels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_listing_sorts_directories_first() {
        let dir =
            std::env::temp_dir().join(format!("swiss-dir-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(dir.join("zed")).unwrap();
        std::fs::create_dir_all(dir.join("abc")).unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        std::fs::write(dir.join("a.key"), b"x").unwrap();
        let listing = list_dir(&dir.to_string_lossy());
        assert!(listing.error.is_none());
        let names: Vec<(&String, bool)> =
            listing.entries.iter().map(|e| (&e.name, e.dir)).collect();
        assert_eq!(
            names,
            vec![
                (&"abc".to_string(), true),
                (&"zed".to_string(), true),
                (&"a.key".to_string(), false),
                (&"b.txt".to_string(), false)
            ]
        );
        assert_eq!(
            listing.parent.as_deref(),
            Some(
                std::path::absolute(std::env::temp_dir())
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dir_listing_reports_errors_not_panics() {
        let listing = list_dir("Z:/definitely/not/here");
        assert!(listing.error.is_some());
        assert!(listing.entries.is_empty());
    }

    #[test]
    fn rule_input_keeps_js_number_semantics() {
        let input = rule_input(&json!({
            "name": "pg",
            "connectionId": "c1",
            "localPort": 5433,
            "reconnectInterval": "15",
            "autoReconnect": "true"
        }));
        assert_eq!(input.local_port, 5433.0);
        assert_eq!(input.reconnect_interval, 15.0);
        assert!(input.auto_reconnect, "'true' as a string counts");
        assert_eq!(input.mcps, None);

        let missing = rule_input(&json!({ "name": "x" }));
        assert!(
            missing.local_port.is_nan(),
            "absent localPort is NaN, like Number(undefined)"
        );
        assert_eq!(missing.reconnect_interval, 10.0);

        let cleared = rule_input(&json!({ "name": "x", "mcps": [] }));
        assert_eq!(
            cleared.mcps,
            Some(Vec::new()),
            "an explicit [] clears the links"
        );
    }

    /// The panel's `api()` helper stamps `Content-Type: application/json` on EVERY request, and
    /// the row buttons send no body at all. Stop and delete must read that as "no options",
    /// not as a JSON parse error — which is what axum's `Json` extractor made of it (400 on
    /// every Stop click in the tunnels page).
    #[tokio::test]
    async fn stop_and_delete_accept_the_panels_bodiless_json_post() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-tapi-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        let id = {
            let mut s = store.lock().unwrap();
            let conn = s
                .add_connection(&conn_input(
                    json!({ "name": "box", "host": "127.0.0.1", "username": "u", "authType": "password", "password": "p" })
                        .as_object()
                        .unwrap(),
                ))
                .expect("a connection");
            s.add_rule(&rule_input(&json!({
                "name": "pg", "connectionId": conn.id, "localPort": 5433,
                "targetHost": "127.0.0.1", "targetPort": 5432
            })))
            .expect("a rule")
            .id
        };
        let manager = TunnelManager::new(store.clone(), None);
        let app = mount(Arc::new(Tunnels {
            store,
            manager,
            mcp_display: None,
        }));

        let bodiless = |uri: String, method: &str| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::empty())
                .expect("request literal")
        };
        let stop = app
            .clone()
            .oneshot(bodiless(format!("/api/tunnels/rules/{id}/stop"), "POST"))
            .await
            .expect("infallible router");
        assert_eq!(
            stop.status(),
            StatusCode::OK,
            "a bodiless stop is a plain stop"
        );

        let delete = app
            .oneshot(bodiless(format!("/api/tunnels/rules/{id}"), "DELETE"))
            .await
            .expect("infallible router");
        assert_eq!(
            delete.status(),
            StatusCode::OK,
            "a bodiless delete is a plain delete"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fail_funnel_status_codes() {
        let dependents = fail(&OpError::Dependents(vec!["pg".into()]));
        assert_eq!(dependents.status(), StatusCode::CONFLICT);
        let unknown = fail(&OpError::msg("unknown rule: r1"));
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        let bad = fail(&OpError::msg("local port 5433 is already used by 'pg'"));
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    /// docs/27 §1.4/§1.5: proxyPassword rides the sentinel round-trip; proxy (no userinfo
    /// possible after save-time validation), proxyUsername and jump are plaintext. The host
    /// masker's secret-key list is exact-match, so the proxyPassword sentinel is applied
    /// here rather than in mask.rs.
    #[test]
    fn conn_masking_round_trips_the_proxy_fields() {
        let def = SshConnDef {
            id: "c1".into(),
            name: "box".into(),
            host: "bastion".into(),
            port: 22,
            username: "root".into(),
            auth_type: AuthType::Password,
            group: None,
            key_path: None,
            passphrase: None,
            password: Some("ssh-secret".into()),
            host_key: None,
            proxy: Some("socks5://127.0.0.1:7890".into()),
            proxy_username: Some("pxuser".into()),
            proxy_password: Some("px-pass".into()),
            jump: Some("other".into()),
        };
        let masked = mask_conn(&def);
        assert_eq!(masked["proxyPassword"], json!(swiss_host::mask::MASK));
        assert_eq!(masked["proxy"], json!("socks5://127.0.0.1:7890"));
        assert_eq!(masked["proxyUsername"], json!("pxuser"));
        assert_eq!(masked["jump"], json!("other"));
        assert_eq!(
            masked["password"],
            json!(swiss_host::mask::MASK),
            "the pre-existing password field keeps its sentinel"
        );

        // An edit that sends the sentinel back keeps the stored secret; editing an
        // unrelated field leaves it intact (the existing unmask semantics).
        let mut body = mask_conn(&def).as_object().cloned().unwrap();
        body.insert("name".into(), json!("renamed"));
        let restored = unmask_conn(&body, Some(&def));
        assert_eq!(restored["proxyPassword"], json!("px-pass"));
        assert_eq!(restored["password"], json!("ssh-secret"));
        assert_eq!(restored["name"], json!("renamed"));

        // A whole-value credential reference is not a secret and passes through.
        let as_ref = SshConnDef {
            proxy_password: Some("${secret://some-proxy-secret}".into()),
            ..def
        };
        assert_eq!(
            mask_conn(&as_ref)["proxyPassword"],
            json!("${secret://some-proxy-secret}"),
            "the reference travels, not the value"
        );

        // A sentinel with nothing behind it (a create that echoes one) is dropped, not
        // stored as dots.
        let mut stray = Map::new();
        stray.insert("proxyPassword".into(), json!(swiss_host::mask::MASK));
        let dropped = unmask_conn(&stray, None);
        assert!(dropped.get("proxyPassword").is_none());
    }

    /// docs/20 §3: the four tunnel group/order routes are retired in favour of the
    /// /api/groups/{scope} family (the host owns them; conns and rules register as scopes).
    /// What this pins is that they are GONE, not merely unused - two doors into one store is
    /// how the two copies of the grouping rules forked in the first place.
    #[tokio::test]
    async fn the_group_and_order_routes_are_retired_to_the_family() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-tapi-retired-{}",
            swiss_core::util::random_hex(8),
        ));
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        let manager = TunnelManager::new(store.clone(), None);
        let app = mount(Arc::new(Tunnels {
            store,
            manager,
            mcp_display: None,
        }));

        let send = |uri: String, method: &'static str, body: String| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request literal")
        };
        for (uri, method, body) in [
            (
                "/api/tunnels/order".to_string(),
                "PUT",
                r#"{"connections":[],"rules":[]}"#.to_string(),
            ),
            (
                "/api/tunnels/groups/rules".to_string(),
                "PUT",
                r#"{"groups":["default"]}"#.to_string(),
            ),
            (
                "/api/tunnels/groups/rules/rename".to_string(),
                "POST",
                r#"{"from":"a","to":"b"}"#.to_string(),
            ),
            (
                "/api/tunnels/groups/rules/xyz".to_string(),
                "PUT",
                r#"{"group":null}"#.to_string(),
            ),
        ] {
            let res = app
                .clone()
                .oneshot(send(uri.clone(), method, body))
                .await
                .expect("infallible router");
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "retired: {uri}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// docs/27 §3.2/§3.4.5: deleting a connection another one jumps through is a plain
    /// 400 naming the dependent. Unlike the rules case there is no confirm/force path —
    /// deleting would leave the dependent's chain pointing at nothing.
    #[tokio::test]
    async fn deleting_a_jump_connection_answers_400_naming_the_dependent() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-tapi-jump-{}",
            swiss_core::util::random_hex(8),
        ));
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        let jump_id = {
            let mut s = store.lock().unwrap();
            let a = s
                .add_connection(&conn_input(
                    json!({
                        "name": "bastion", "host": "127.0.0.1", "port": 2222,
                        "username": "u", "authType": "password", "password": "p"
                    })
                    .as_object()
                    .unwrap(),
                ))
                .expect("the jump connection");
            s.add_connection(&conn_input(
                json!({
                    "name": "db", "host": "10.0.0.9", "port": 22,
                    "username": "u", "authType": "password", "password": "p",
                    "jump": a.id
                })
                .as_object()
                .unwrap(),
            ))
            .expect("the dependent connection");
            a.id
        };
        let manager = TunnelManager::new(store.clone(), None);
        let app = mount(Arc::new(Tunnels {
            store,
            manager,
            mcp_display: None,
        }));

        let res = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/tunnels/connections/{jump_id}"))
                    .body(Body::empty())
                    .expect("request literal"),
            )
            .await
            .expect("infallible router");
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("the body");
        let body: Value = serde_json::from_slice(&bytes).expect("a json error body");
        assert!(
            body["error"].as_str().unwrap_or_default().contains("db"),
            "names the dependent: {body}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
