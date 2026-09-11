//! The management API under /api — port of `adminapi.ts`.
//!
//! The gate: there is none. The panel has no login — the router's loopback guard ahead of every
//! route is the real boundary: a request that did not come from this machine never reaches /api
//! at all, and on this machine there is exactly one operator. (The MCP endpoints stay
//! token-gated — that token is what AI clients authenticate with, and it is not a panel login.)
//!
//! Every response is shape-identical to the Node build's (camelCase, absent-not-null): the panel
//! JS is the spec (ADR-009).

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::Router;
use serde_json::{json, Map, Value};

use crate::app::{admin_error, admin_json, AppContext};
use swiss_core::log;
use swiss_host::config::ServerDef;
use swiss_host::managed::ManagedEntry;
use swiss_host::mask::{mask_def, unmask_body};
use swiss_host::mem::get_memory_info;
use swiss_mcp::adapters::make_adapter;
use swiss_mcp::calls::{with_call_source_panel, CALLS_PAGE_SIZE};
use swiss_mcp::paging::{list_page, PageCache, PAGE_SIZE};
use swiss_mcp::registry::Lifecycle;
use swiss_tunnels::tunnel::manager::TunnelLinks;

// --- tunnel seam ---------------------------------------------------------------------------------

// `TunnelLinks` — what this API is allowed to do with tunnels — is defined by the tunnel
// side (swiss_tunnels::TunnelLinks); the tunnel manager implements it. Deliberately that
// narrow: an MCP operation must never start or stop a tunnel.

/// Consult the tunnel seam, if the boot sequence installed one (None until the tunnel subsystem
/// lands; every consult site treats that as "no tunnels to report").
fn tunnel_links(ctx: &AppContext) -> Option<Arc<dyn TunnelLinks>> {
    ctx.tunnel_links.read().ok().and_then(|g| g.clone())
}

// --- name validation ---------------------------------------------------------------------------

/// `NAME_RE` in the Node build, hand-rolled (ADR-007): first char alphanumeric, then up to 62 of
/// [A-Za-z0-9_-].
fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 63 || !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// Names that would shadow built-in routes or be ambiguous as a path segment.
const RESERVED: [&str; 3] = ["api", "health", "admin"];

fn name_ok(name: &str) -> bool {
    is_valid_name(name) && !RESERVED.contains(&name.to_ascii_lowercase().as_str())
}

/// How this MCP is launched, for the sidebar chip. The adapter type, except `proc`, which is
/// split by its command's first word — npx / uvx / docker is the difference a user reads at a
/// glance. Windows wrappers (`npx.cmd`, `uvx.exe`) resolve to the same tag.
fn tag_of(type_: &str, def: &ServerDef) -> String {
    if type_ != "proc" {
        return type_.to_string();
    }
    let command = def.get_str("command").unwrap_or_default();
    let first = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let base = first.replace('\\', "/");
    let base = base.rsplit('/').next().unwrap_or("").to_string();
    let base = if base.len() > 4 && (base.ends_with(".cmd") || base.ends_with(".exe")) {
        base[..base.len() - 4].to_string()
    } else {
        base
    };
    match base.as_str() {
        "npx" | "uvx" | "docker" => base,
        "uv" => "uvx".into(),
        _ => "proc".into(),
    }
}

// --- definition building --------------------------------------------------------------------

/// Connection fields each direct adapter reads. Anything else in the request body is dropped, so
/// the panel can edit a definition without polluting it.
const DIRECT_FIELDS: [(&str, &[&str]); 3] = [
    (
        "mysql",
        &[
            "description",
            "host",
            "port",
            "user",
            "password",
            "database",
            "timezone",
            "readonly",
            "maxRows",
        ],
    ),
    (
        "redis",
        &[
            "description",
            "host",
            "port",
            "password",
            "db",
            "readonly",
            "allowDestructive",
            "allowEval",
        ],
    ),
    ("pg", &["description", "url", "readonly", "maxRows"]),
];
const BOOL_FIELDS: [&str; 5] = [
    "readonly",
    "allowDestructive",
    "allowEval",
    "exposeResources",
    "exposePrompts",
];
/// Fields without which the adapter has nothing to connect to. An empty pg connection
/// string is the dangerous one: libpq falls back to PGHOST/PGDATABASE, so the MCP would
/// quietly point at whatever the environment happens to name rather than failing.
/// mysql/redis default to localhost on their standard port, which is a
/// stated default, not a surprise.
const REQUIRED_FIELD: [(&str, &str); 1] = [("pg", "url")];

fn as_bool(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
        || v.as_str() == Some("true")
        || *v == json!(1)
        || v.as_str() == Some("1")
}

fn str_field(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

/// Build a ServerDef from an admin add/edit request body — port of `buildDef`/`buildTypedDef`.
pub fn build_def(body: &Value) -> Result<ServerDef, String> {
    let mut def = build_typed_def(body)?;
    // `lazy` rides on every type — it is what the panel's "Start automatically" checkbox writes
    // (inverted). A proc is lazy by default; anything else opts in explicitly (see is_lazy).
    if let Some(lazy) = body.get("lazy") {
        if !lazy.is_null() {
            def.set("lazy", Value::Bool(as_bool(lazy)));
        }
    }
    Ok(def)
}

fn build_typed_def(body: &Value) -> Result<ServerDef, String> {
    let type_ = str_field(body, "type")
        .unwrap_or_default()
        .trim()
        .to_string();
    let type_ = if type_.is_empty() {
        "proc".to_string()
    } else {
        type_
    };
    let mut def = Map::new();
    def.insert("type".into(), json!(type_));

    if type_ == "proc" {
        let command = str_field(body, "command")
            .unwrap_or_default()
            .trim()
            .to_string();
        if command.is_empty() {
            return Err("command is required".into());
        }
        def.insert("command".into(), json!(command));
        if let Some(description) = str_field(body, "description") {
            if !description.trim().is_empty() {
                def.insert("description".into(), json!(description.trim()));
            }
        }
        if let Some(env) = body.get("env").filter(|v| v.is_object()) {
            if env.as_object().is_some_and(|o| !o.is_empty()) {
                def.insert("env".into(), env.clone());
            }
        }
        if let Some(cwd) = str_field(body, "cwd") {
            if !cwd.trim().is_empty() {
                def.insert("cwd".into(), json!(cwd.trim()));
            }
        }
        // Default true in the adapter; only persist an explicit opt-out, so a child with
        // thousands of resources can be hidden from client context.
        for key in ["exposeResources", "exposePrompts"] {
            if let Some(v) = body.get(key) {
                if !v.is_null() && !as_bool(v) {
                    def.insert(key.into(), json!(false));
                }
            }
        }
        return Ok(ServerDef(def));
    }
    // A remote MCP is configured, not launched: where it is, and the headers its API key travels
    // in. Shaped by hand rather than DIRECT_FIELDS because `headers` is a map, and because an
    // http MCP with no url has nothing to connect to at all.
    if type_ == "http" {
        let url = str_field(body, "url")
            .unwrap_or_default()
            .trim()
            .to_string();
        if url.is_empty() {
            return Err("url is required for an http MCP".into());
        }
        def.insert("url".into(), json!(url));
        if let Some(description) = str_field(body, "description") {
            if !description.trim().is_empty() {
                def.insert("description".into(), json!(description.trim()));
            }
        }
        if let Some(headers) = body.get("headers").filter(|v| v.is_object()) {
            if headers.as_object().is_some_and(|o| !o.is_empty()) {
                def.insert("headers".into(), headers.clone());
            }
        }
        for key in ["exposeResources", "exposePrompts"] {
            if let Some(v) = body.get(key) {
                if !v.is_null() && !as_bool(v) {
                    def.insert(key.into(), json!(false));
                }
            }
        }
        // An http(s):// URL, or a ${ENV} ref to one; the adapter validates the resolved value at
        // build.
        if let Some(proxy) = str_field(body, "proxy") {
            if !proxy.trim().is_empty() {
                def.insert("proxy".into(), json!(proxy.trim()));
            }
        }
        return Ok(ServerDef(def));
    }
    // A declared REST API: `tools` is an array of declarations and is kept whole — it is
    // authored, not filled in from form fields, and nothing here should reshape it.
    if type_ == "rest" {
        let base_url = str_field(body, "baseUrl")
            .unwrap_or_default()
            .trim()
            .to_string();
        if base_url.is_empty() {
            return Err("baseUrl is required for a rest MCP".into());
        }
        let tools = body.get("tools").and_then(Value::as_array);
        if tools.is_none_or(|t| t.is_empty()) {
            return Err("tools must be a non-empty array for a rest MCP".into());
        }
        def.insert("baseUrl".into(), json!(base_url));
        def.insert("tools".into(), json!(tools));
        if let Some(description) = str_field(body, "description") {
            if !description.trim().is_empty() {
                def.insert("description".into(), json!(description.trim()));
            }
        }
        if let Some(headers) = body.get("headers").filter(|v| v.is_object()) {
            if headers.as_object().is_some_and(|o| !o.is_empty()) {
                def.insert("headers".into(), headers.clone());
            }
        }
        if let Some(timeout) = body.get("timeoutMs") {
            if !timeout.is_null() && timeout.as_f64().is_some_and(|n| n > 0.0) {
                def.insert("timeoutMs".into(), timeout.clone());
            }
        }
        if let Some(proxy) = str_field(body, "proxy") {
            if !proxy.trim().is_empty() {
                def.insert("proxy".into(), json!(proxy.trim()));
            }
        }
        return Ok(ServerDef(def));
    }
    if type_ == "echo" {
        return Ok(ServerDef(def));
    }
    let Some(allowed) = DIRECT_FIELDS
        .iter()
        .find(|(t, _)| *t == type_)
        .map(|(_, f)| *f)
    else {
        return Err(format!(
            "unknown type: {type_} (supported: mysql | redis | pg | proc | http | rest | echo)"
        ));
    };
    for k in allowed {
        let Some(v) = body.get(k) else { continue };
        if v.is_null() || v.as_str() == Some("") {
            continue;
        }
        let value = if BOOL_FIELDS.contains(k) {
            json!(as_bool(v))
        } else {
            v.clone()
        };
        def.insert(k.to_string(), value);
    }
    if let Some((_, need)) = REQUIRED_FIELD.iter().find(|(t, _)| *t == type_) {
        if def.get(*need).is_none() {
            return Err(format!("{need} is required for a {type_} MCP"));
        }
    }
    Ok(ServerDef(def))
}

// --- shared route plumbing ----------------------------------------------------------------------

/// The lifecycle string after a lifecycle operation — `registry.get(name)?.lifecycle`.
fn lifecycle_of(ctx: &AppContext, name: &str) -> Value {
    ctx.registry
        .get(name)
        .and_then(|e| e.data.read().ok().map(|d| json!(d.lifecycle.as_str())))
        .unwrap_or(Value::Null)
}

/// Register + persist + (maybe) start one managed MCP — `addManaged` in the Node build.
async fn add_managed(
    ctx: &AppContext,
    name: &str,
    def: ServerDef,
    enabled: bool,
    start_now: bool,
) -> Result<Value, String> {
    let adapter = make_adapter(&def, name, &ctx.calls)?;
    ctx.store.add(ManagedEntry {
        name: name.to_string(),
        def: def.clone(),
        enabled,
        override_: false,
    })?;
    if let Err(err) = ctx
        .registry
        .register(name, swiss_mcp::registry::Source::Managed, def, adapter)
    {
        let _ = ctx.store.remove(name);
        return Err(err);
    }
    if start_now {
        if let Err(err) = ctx.registry.start(name).await {
            log::warn(
                "managed mcp start failed on add",
                Some(json!({ "name": name, "err": err })),
            );
        }
    }
    Ok(lifecycle_of(ctx, name))
}

// --- mounting -----------------------------------------------------------------------------------

pub fn mount(_ctx: Arc<AppContext>) -> Router<Arc<AppContext>> {
    let mut r = Router::new();

    // The env var the seed token came from — metadata for the panel, never a secret. The panel
    // stamp summarizes the whole admin tree; the page polls it and reloads ITSELF when a new
    // build lands — one edited module counts as much as the shell — so an update never costs the
    // operator a manual refresh.
    r = r.route("/api/info", get(|State(ctx): State<Arc<AppContext>>| async move {
        // The build stamp (docs/16 H3) joins tokenEnv/panelVersion — additive to the Node
        // shape: the panel reads named fields only, and the stamp is metadata, never a secret.
        admin_json(
            StatusCode::OK,
            json!({ "tokenEnv": ctx.token_env, "panelVersion": swiss_panel::admin::panel_version_stamp(), "build": crate::app::build_info() }),
        )
    }));

    // --- tokens: named per-client bearers, so the logs can attribute every request to a client ----
    r = r.route(
        "/api/tokens",
        get(|State(ctx): State<Arc<AppContext>>| async move {
            let tokens: Vec<Value> = ctx
                .tokens
                .list()
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
                .collect();
            admin_json(StatusCode::OK, json!({ "tokens": tokens, "tokenEnv": ctx.token_env }))
        })
        .post(
            |State(ctx): State<Arc<AppContext>>, body: crate::reply::NodeBody| async move {
                let body = body.0;
                let label = body.get("label").and_then(Value::as_str).unwrap_or("").to_string();
                let rec = ctx.tokens.create(&label);
                log::log(
                    "info",
                    "token created",
                    Some(json!({ "id": rec.id, "label": rec.label })),
                );
                admin_json(
                    StatusCode::CREATED,
                    json!({ "id": rec.id, "label": rec.label, "secret": rec.secret, "createdAt": rec.created_at }),
                )
            },
        ),
    );

    // Hand a stored secret back, so the panel can build a connect command from a token that
    // already exists. A deliberate relaxation of "shown once on create": verify() compares the
    // secret directly, so it sits in plaintext in managed.json and `cat` recovers it —
    // withholding it from a loopback caller bought no secrecy while costing a rotate every time
    // someone wanted to copy a connect command. Its own route, so reading a secret stays an
    // explicit act.
    r = r.route(
        "/api/tokens/{id}/secret",
        get(
            |State(ctx): State<Arc<AppContext>>, Path(id): Path<String>| async move {
                match ctx.tokens.get(&id) {
                    None => admin_error(StatusCode::NOT_FOUND, "no such token"),
                    Some(rec) => admin_json(
                        StatusCode::OK,
                        json!({ "id": rec.id, "label": rec.label, "secret": rec.secret }),
                    ),
                }
            },
        ),
    );
    r = r.route(
        "/api/tokens/{id}/rotate",
        post(|State(ctx): State<Arc<AppContext>>, Path(id): Path<String>| async move {
            match ctx.tokens.rotate(&id) {
                None => admin_error(StatusCode::NOT_FOUND, "no such token"),
                Some(rec) => {
                    log::log("info", "token rotated", Some(json!({ "id": rec.id })));
                    admin_json(
                        StatusCode::OK,
                        json!({ "id": rec.id, "label": rec.label, "secret": rec.secret, "createdAt": rec.created_at }),
                    )
                }
            }
        }),
    );
    r = r.route(
        "/api/tokens/{id}",
        delete(
            |State(ctx): State<Arc<AppContext>>, Path(id): Path<String>| async move {
                if !ctx.tokens.remove(&id) {
                    return admin_error(StatusCode::NOT_FOUND, "no such token");
                }
                log::log("info", "token revoked", Some(json!({ "id": id })));
                admin_json(StatusCode::OK, json!({ "ok": true }))
            },
        ),
    );

    // --- interaction traffic: who (token + self-reported client) asked what, across every MCP ------
    // Paged and body-less: each entry's request and reply are capped at 8 KB, and the panel polls
    // this every 6 seconds. `clients` is folded over the whole ring, never over the page.
    r = r.route(
        "/api/traffic",
        get(
            |State(ctx): State<Arc<AppContext>>,
             Query(q): Query<std::collections::HashMap<String, String>>| async move {
                let _ = &ctx;
                let page = q
                    .get("page")
                    .and_then(|p| p.parse::<usize>().ok())
                    .unwrap_or(0);
                let page_size = q.get("pageSize").and_then(|p| p.parse::<usize>().ok());
                let query = swiss_mcp::traffic::TrafficQuery {
                    mcp: q.get("mcp").map(String::as_str),
                    client: q.get("client").map(String::as_str),
                    method: q.get("method").map(String::as_str),
                    actions_only: q.get("actions").map(|a| a == "1").unwrap_or(false),
                    page,
                    page_size,
                };
                let mut out = swiss_mcp::traffic::read_traffic(&query);
                out["clients"] = json!(swiss_mcp::traffic::traffic_clients());
                admin_json(StatusCode::OK, out)
            },
        )
        .delete(
            |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                // ?client=<prefixed key> clears only that client's rows; without it, all traffic.
                let client = q.get("client").cloned();
                swiss_mcp::traffic::clear_traffic(client.as_deref());
                admin_json(StatusCode::OK, json!({ "ok": true, "client": client }))
            },
        ),
    );
    r = r.route(
        "/api/traffic/{seq}",
        get(|Path(seq): Path<String>| async move {
            let Ok(seq) = seq.parse::<u64>() else {
                return admin_error(StatusCode::BAD_REQUEST, "seq must be a number");
            };
            match swiss_mcp::traffic::read_traffic_entry(seq) {
                None => admin_error(
                    StatusCode::NOT_FOUND,
                    "No such interaction (the ring may have rolled over)",
                ),
                Some(entry) => admin_json(StatusCode::OK, entry),
            }
        }),
    );

    // The panel's sidebar order. Names the panel sends back verbatim; registered MCPs missing
    // from the list are appended by GET /api/mcps in name order, so a fresh MCP never vanishes.
    r = r.route(
        "/api/order",
        put(
            |State(ctx): State<Arc<AppContext>>, body: crate::reply::NodeBody| async move {
                let body = body.0;
                let Some(order) = body.get("order").and_then(Value::as_array) else {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "order must be an array of MCP names",
                    );
                };
                if !order.iter().all(|n| n.as_str().is_some()) {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "order must be an array of MCP names",
                    );
                }
                let names: Vec<String> = order
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                match ctx.store.set_order(names) {
                    Err(err) => admin_error(StatusCode::BAD_REQUEST, &err),
                    Ok(()) => admin_json(StatusCode::OK, json!({ "order": ctx.store.get_order() })),
                }
            },
        ),
    );

    // The panel's custom sidebar groups, sent as one whole ordered list: creating, reordering
    // and deleting are all "here is the new list". A group dropped by omission is deleted, and
    // the store returns its MCPs to the default group.
    r = r.route(
        "/api/groups",
        put(
            |State(ctx): State<Arc<AppContext>>, body: crate::reply::NodeBody| async move {
                let body = body.0;
                let Some(groups) = body.get("groups").and_then(Value::as_array) else {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "groups must be an array of group names",
                    );
                };
                if !groups.iter().all(|n| n.as_str().is_some()) {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "groups must be an array of group names",
                    );
                }
                let names: Vec<String> = groups
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                match ctx.store.set_groups(names) {
                    Err(err) => admin_error(StatusCode::BAD_REQUEST, &err),
                    Ok(()) => {
                        admin_json(StatusCode::OK, json!({ "groups": ctx.store.get_groups() }))
                    }
                }
            },
        ),
    );

    // Rename in place: the group keeps its slot in the order and takes its members with it,
    // which a delete-then-create could not do.
    r = r.route(
        "/api/groups/{name}/rename",
        post(
            |State(ctx): State<Arc<AppContext>>,
             Path(name): Path<String>,
             body: crate::reply::NodeBody| async move {
                let body = body.0;
                let to = body
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let known = ctx
                    .store
                    .get_groups()
                    .iter()
                    .any(|g| g.eq_ignore_ascii_case(&name));
                if !known {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown group: {name}"));
                }
                match ctx.store.rename_group(&name, &to) {
                    Err(err) => admin_error(StatusCode::BAD_REQUEST, &err),
                    Ok(()) => {
                        admin_json(StatusCode::OK, json!({ "groups": ctx.store.get_groups() }))
                    }
                }
            },
        ),
    );

    // Put one MCP in a group. `null` (or "default") returns it to the default group. Works for
    // config-sourced MCPs because membership is keyed by name in managed.json, not on the def.
    r = r.route(
        "/api/mcps/{name}/group",
        put(
            |State(ctx): State<Arc<AppContext>>,
             Path(name): Path<String>,
             body: crate::reply::NodeBody| async move {
                if !ctx.registry.has(&name) {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
                }
                let body = body.0;
                let raw = body.get("group");
                match raw {
                    None | Some(Value::Null) => {}
                    Some(Value::String(_)) => {}
                    Some(_) => {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "group must be a group name or null",
                        )
                    }
                }
                let group = raw.and_then(Value::as_str);
                match ctx.store.set_mcp_group(&name, group) {
                    Err(err) => admin_error(StatusCode::BAD_REQUEST, &err),
                    Ok(()) => admin_json(
                        StatusCode::OK,
                        json!({ "name": name, "group": ctx.store.group_of(&name) }),
                    ),
                }
            },
        ),
    );

    // List every MCP — status only (light, safe to poll). Tools/resources load on demand.
    r = r.route(
        "/api/mcps",
        get(|State(ctx): State<Arc<AppContext>>| async move {
            let order = ctx.store.get_order();
            let rank: std::collections::HashMap<String, usize> =
                order.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();
            let mut rows: Vec<Value> = ctx
                .registry
                .all()
                .iter()
                .filter_map(|entry| {
                    let d = entry.data.read().ok()?;
                    let mut row = json!({
                        "name": d.name,
                        "source": d.source.as_str(),
                        "type": d.adapter.kind(),
                        // How this MCP is launched (http / rest / npx / uvx / ...), so the sidebar
                        // can label every row without a per-row details fetch.
                        "tag": tag_of(d.adapter.kind(), &d.def),
                        "description": d.def.get_str("description").unwrap_or_default(),
                        "lifecycle": d.lifecycle.as_str(),
                        "state": if d.lifecycle == Lifecycle::Started { d.status.as_str() } else { d.lifecycle.as_str() },
                        // The sidebar group. Always a real name — an unassigned MCP answers "default".
                        "group": ctx.store.group_of(&d.name),
                    });
                    // Node's `latencyMs: e.latencyMs` etc. — an `undefined` field drops out of
                    // JSON.stringify, so these keys are absent (never null) until they have a
                    // value.
                    if let Some(obj) = row.as_object_mut() {
                        if let Some(v) = d.latency_ms {
                            obj.insert("latencyMs".into(), json!(v));
                        }
                        if let Some(v) = d.last_check.clone() {
                            obj.insert("lastCheck".into(), json!(v));
                        }
                        let reason = if d.lifecycle == Lifecycle::Error {
                            d.error.clone()
                        } else {
                            d.last_error.clone()
                        };
                        if let Some(v) = reason {
                            obj.insert("reason".into(), json!(v));
                        }
                        if let Some(v) = d.started_at.clone() {
                            obj.insert("startedAt".into(), json!(v));
                        }
                    }
                    Some(row)
                })
                .collect();
            // The panel-defined order first; everything the user has never positioned falls back
            // to plain name order rather than to whatever order the registry happened to start
            // them in. Order stays flat across the whole list; the panel slices it by group.
            rows.sort_by(|a, b| {
                let ra = rank
                    .get(a["name"].as_str().unwrap_or_default())
                    .copied()
                    .unwrap_or(usize::MAX);
                let rb = rank
                    .get(b["name"].as_str().unwrap_or_default())
                    .copied()
                    .unwrap_or(usize::MAX);
                // Compare ranks only when they actually differ; names decide otherwise.
                if ra != rb {
                    ra.cmp(&rb)
                } else {
                    a["name"].as_str().cmp(&b["name"].as_str())
                }
            });
            admin_json(StatusCode::OK, json!({ "mcps": rows, "groups": ctx.store.get_groups() }))
        })
        .post(|State(ctx): State<Arc<AppContext>>, body: crate::reply::NodeBody| async move {
            let body = body.0;
            let name = body.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if !name_ok(&name) {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    &format!("invalid name '{name}' (use letters, digits, - or _)"),
                );
            }
            if ctx.registry.has(&name) || ctx.store.has(&name) {
                return admin_error(StatusCode::CONFLICT, &format!("name already exists: {name}"));
            }
            let def = match build_def(&body) {
                Ok(def) => def,
                Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
            };
            let type_ = def.type_().to_string();
            let enabled = body.get("enabled").map(as_bool) .unwrap_or(true);
            match add_managed(&ctx, &name, def, enabled, enabled).await {
                Ok(lifecycle) => admin_json(StatusCode::CREATED, json!({ "name": name, "type": type_, "lifecycle": lifecycle })),
                Err(err) => {
                    let status = if err.contains("already registered") { StatusCode::CONFLICT } else { StatusCode::BAD_REQUEST };
                    admin_error(status, &err)
                }
            }
        }),
    );

    // Import a client `.mcp.json` (Claude Code / Cursor / OpenCode). Stdio entries become proc
    // MCPs, remote URLs become http MCPs. Names already in use get -1, -2 rather than being
    // overwritten.
    r = r.route(
        "/api/mcps/import",
        post(
            |State(ctx): State<Arc<AppContext>>, body: crate::reply::NodeBody| async move {
                let body = body.0;
                let mut taken: std::collections::HashSet<String> =
                    ctx.registry.names().into_iter().collect();
                for m in ctx.store.all() {
                    taken.insert(m.name.clone());
                }
                let plan = match swiss_mcp::mcp_import::plan_mcp_import(&body, &taken, ctx.port) {
                    Ok(plan) => plan,
                    // Node's handler let planMcpImport throw into the router's catch-all, which
                    // answered 500 {error} — structural surprises are server errors there, not 400s.
                    Err(err) => return admin_error(StatusCode::INTERNAL_SERVER_ERROR, &err),
                };
                let mut imported: Vec<Value> = Vec::new();
                let mut skipped: Vec<Value> = plan
                    .skip
                    .iter()
                    .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
                    .collect();
                for item in plan.add {
                    let type_ = item.def.type_().to_string();
                    match add_managed(&ctx, &item.name, item.def.clone(), true, false).await {
                        Ok(lifecycle) => imported.push(json!({
                            "name": item.name,
                            "from": item.wanted,
                            "type": type_,
                            "lifecycle": lifecycle,
                        })),
                        Err(err) => skipped.push(json!({ "name": item.wanted, "reason": err })),
                    }
                }
                admin_json(
                    StatusCode::OK,
                    json!({ "imported": imported, "skipped": skipped }),
                )
            },
        ),
    );

    // A REAL connection test against the values in the form: build_def validates them,
    // make_adapter expands the ${ENV} refs and constructs the same driver the MCP would run.
    // Nothing is registered and nothing is persisted — the credential exists only inside the
    // throwaway adapter, closed immediately after. What "a real test" means per family:
    // - DB (mysql/redis/pg): adapter.ping() — exactly what the health probe runs.
    // - http: adapter.build() — the initialize handshake, so a wrong URL or a rejected key fails.
    // - rest: one plain GET to the baseUrl (through the def's proxy if it names one). ANY HTTP
    //   answer counts as reachable — the base path itself need not serve anything — a network
    //   error does not.
    r = r.route(
        "/api/mcps/test",
        post(|State(ctx): State<Arc<AppContext>>, body: crate::reply::NodeBody| async move {
            const TESTABLE_TYPES: [&str; 5] = ["mysql", "redis", "pg", "http", "rest"];
            const TEST_TIMEOUT_MS: u64 = 5000;
            let body = body.0;
            let type_ = body.get("type").and_then(Value::as_str).unwrap_or("").to_string();
            if !TESTABLE_TYPES.contains(&type_.as_str()) {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    &format!(
                        "no connection test for type '{type_}' (testable: {})",
                        TESTABLE_TYPES.join(" | ")
                    ),
                );
            }
            let t0 = std::time::Instant::now();

            // rest is tested with one plain request rather than an adapter: its tools are
            // authored calls into a metered API and must not be fired on a button press.
            if type_ == "rest" {
                let def = match build_def(&body) {
                    Ok(def) => def,
                    Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
                };
                let base_url = def.get_str("baseUrl").unwrap_or("").to_string();
                let mut headers = reqwest::header::HeaderMap::new();
                if let Ok(accept) = "application/json".parse() {
                    headers.insert("accept", accept);
                }
                if let Some(Value::Object(extra)) = def.get("headers") {
                    for (k, v) in extra {
                        if let (Ok(name), Some(value)) = (k.parse::<reqwest::header::HeaderName>(), v.as_str()) {
                            if let Ok(hv) = value.parse() {
                                headers.insert(name, hv);
                            }
                        }
                    }
                }
                let client = match def.get_str("proxy").filter(|p| !p.is_empty()) {
                    // A malformed proxy URL reads like every other rest failure ({ok:false}),
                    // never a 500.
                    Some(proxy) => match reqwest::Proxy::all(proxy)
                        .map_err(|e| e.to_string())
                        .and_then(|p| reqwest::Client::builder().proxy(p).build().map_err(|e| e.to_string()))
                    {
                        Ok(client) => client,
                        Err(err) => {
                            return admin_json(
                                StatusCode::OK,
                                json!({ "ok": false, "ms": t0.elapsed().as_millis() as u64, "error": err }),
                            )
                        }
                    },
                    None => reqwest::Client::new(),
                };
                return match client
                    .get(&base_url)
                    .headers(headers)
                    .timeout(std::time::Duration::from_millis(TEST_TIMEOUT_MS))
                    .send()
                    .await
                {
                    Ok(out) => admin_json(
                        StatusCode::OK,
                        json!({ "ok": true, "ms": t0.elapsed().as_millis() as u64, "status": out.status().as_u16() }),
                    ),
                    Err(err) => admin_json(
                        StatusCode::OK,
                        json!({ "ok": false, "ms": t0.elapsed().as_millis() as u64, "error": err.to_string() }),
                    ),
                };
            }

            let adapter = match build_def(&body) {
                Ok(def) => match swiss_mcp::adapters::make_adapter(&def, "test", &ctx.calls) {
                    Ok(adapter) => adapter,
                    Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
                },
                Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
            };
            let ms = |t0: std::time::Instant| t0.elapsed().as_millis() as u64;
            let outcome: Result<(), String> = if type_ == "http" {
                // The initialize handshake IS the test — but capped: a dead remote must not hold
                // the button for the SDK's own ~60s connect timeout.
                match tokio::time::timeout(std::time::Duration::from_millis(TEST_TIMEOUT_MS), adapter.build()).await
                {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Err(format!("timed out after {TEST_TIMEOUT_MS} ms")),
                }
            } else {
                // The cap matters most for driver connect retries: a wrong host can otherwise
                // sit there for 30s.
                match tokio::time::timeout(std::time::Duration::from_millis(TEST_TIMEOUT_MS), async {
                    match adapter.ping().await {
                        Some(Ok(())) => Ok::<(), String>(()),
                        Some(Err(err)) => Err(err),
                        None => Err("this adapter has no connection probe".into()),
                    }
                })
                .await
                {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Err(format!("timed out after {TEST_TIMEOUT_MS} ms")),
                }
            };
            adapter.close().await;
            match outcome {
                // The driver's own message is the useful part ("Access denied for user
                // 'x'@'h'") — driver errors never embed the password, so it is safe to hand
                // back verbatim.
                Ok(()) => admin_json(StatusCode::OK, json!({ "ok": true, "ms": ms(t0) })),
                Err(err) => admin_json(StatusCode::OK, json!({ "ok": false, "ms": ms(t0), "error": err })),
            }
        }),
    );

    // Memory footprint. The gateway's own numbers are read in-process (free); pass ?tree=1 to
    // also walk proc-MCP child subtrees, which is opt-in.
    r = r.route(
        "/api/memory",
        get(
            |State(ctx): State<Arc<AppContext>>,
             Query(q): Query<std::collections::HashMap<String, String>>| async move {
                let wants_tree = q.get("tree").map(|t| t == "1").unwrap_or(false);
                admin_json(
                    StatusCode::OK,
                    get_memory_info(&ctx.registry.child_pids(), wants_tree),
                )
            },
        ),
    );

    // Stop the gateway. This endpoint exists because Windows has no deliverable SIGTERM, so
    // `swiss stop` has no other way to reach the graceful sequence. The local-only guard ahead of
    // every route means it is unreachable from another machine.
    r = r.route(
        "/api/shutdown",
        post(|State(ctx): State<Arc<AppContext>>| async move {
            log::info("shutdown requested over the admin API");
            // Answer before tearing anything down: `swiss stop` distinguishes a clean stop from a
            // crash by receiving this response. The signal fires just after the response is
            // handed to the connection (Node waited on the socket's finish event).
            let tx = ctx.shutdown.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let _ = tx.send(true);
            });
            admin_json(StatusCode::OK, json!({ "ok": true, "stopping": true }))
        }),
    );

    // --- per-MCP lifecycle ------------------------------------------------------------------------

    async fn lifecycle_route(ctx: Arc<AppContext>, name: String, verb: &str) -> Response {
        let result = match verb {
            "start" => ctx.registry.start(&name).await,
            "stop" => ctx.registry.stop(&name).await,
            _ => ctx.registry.restart(&name).await,
        };
        if let Err(err) = result {
            return admin_error(StatusCode::BAD_REQUEST, &err);
        }
        if let Err(err) = ctx.store.set_enabled(&name, verb != "stop") {
            return admin_error(StatusCode::BAD_REQUEST, &err);
        }
        admin_json(
            StatusCode::OK,
            json!({ "name": name, "lifecycle": lifecycle_of(&ctx, &name) }),
        )
    }

    r = r.route(
        "/api/mcps/{name}/start",
        post(
            |State(ctx): State<Arc<AppContext>>, Path(name): Path<String>| async move {
                lifecycle_route(ctx, name, "start").await
            },
        ),
    );
    r = r.route(
        "/api/mcps/{name}/stop",
        post(
            |State(ctx): State<Arc<AppContext>>, Path(name): Path<String>| async move {
                lifecycle_route(ctx, name, "stop").await
            },
        ),
    );
    r = r.route(
        "/api/mcps/{name}/restart",
        post(
            |State(ctx): State<Arc<AppContext>>, Path(name): Path<String>| async move {
                lifecycle_route(ctx, name, "restart").await
            },
        ),
    );

    r = r.route(
        "/api/mcps/{name}/rename",
        post(
            |State(ctx): State<Arc<AppContext>>,
             Path(name): Path<String>,
             body: crate::reply::NodeBody| async move {
                let body = body.0;
                let new_name = body
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !name_ok(&new_name) {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        &format!("invalid name '{new_name}'"),
                    );
                }
                if ctx.registry.has(&new_name) {
                    return admin_error(
                        StatusCode::CONFLICT,
                        &format!("name already exists: {new_name}"),
                    );
                }
                if let Err(err) = ctx.registry.rename(&name, &new_name).await {
                    return admin_error(StatusCode::BAD_REQUEST, &err);
                }
                if let Err(err) = ctx.store.rename(&name, &new_name) {
                    return admin_error(StatusCode::BAD_REQUEST, &err);
                }
                // A tunnel that declares it serves this MCP must follow the rename, or the link
                // silently dies and the panel starts claiming the tunnel serves nothing.
                if let Some(links) = tunnel_links(&ctx) {
                    links.rename_mcp(&name, &new_name);
                }
                admin_json(StatusCode::OK, json!({ "name": new_name }))
            },
        ),
    );

    r = r.route(
        "/api/mcps/{name}",
        delete(|State(ctx): State<Arc<AppContext>>, Path(name): Path<String>| async move {
            // A config-sourced MCP must leave gateway.config.json too, or the next start
            // resurrects it. The write is atomic and touches only this key — ${ENV} refs
            // elsewhere survive verbatim.
            let source = ctx.registry.get(&name).and_then(|e| e.data.read().ok().map(|d| d.source));
            if source == Some(swiss_mcp::registry::Source::Config)
                && !swiss_host::config::remove_config_server(&name, &swiss_host::config::config_path()) {
                    log::log(
                        "warn",
                        "config entry was already gone on delete",
                        Some(json!({ "name": name })),
                    );
                }
            if let Err(err) = ctx.registry.delete(&name).await {
                return admin_error(StatusCode::BAD_REQUEST, &err);
            }
            if let Err(err) = ctx.store.remove(&name) {
                return admin_error(StatusCode::BAD_REQUEST, &err);
            }
            // No MCP by that name any more, so no rule may claim to serve it.
            if let Some(links) = tunnel_links(&ctx) {
                links.forget_mcp(&name);
            }
            admin_json(StatusCode::OK, json!({ "name": name, "deleted": true }))
        })
        .put(|State(ctx): State<Arc<AppContext>>, Path(name): Path<String>, body: crate::reply::NodeBody| async move {
            // Edit an MCP's config and restart it. Managed MCPs persist to managed.json;
            // config-file MCPs persist as an override (also managed.json), so the committed
            // gateway.config.json keeps its ${ENV} refs — an override stores the reference
            // rather than the resolved secret.
            let Some(entry) = ctx.registry.get(&name) else {
                return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
            };
            let current = entry.data.read().ok().map(|d| d.def.clone());
            let body = body.0;
            let unmasked = match body.as_object() {
                Some(obj) => unmask_body(obj, current.as_ref()),
                None => Map::new(),
            };
            let def = match build_def(&Value::Object(unmasked)) {
                Ok(def) => def,
                Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
            };
            let adapter = match make_adapter(&def, &name, &ctx.calls) {
                Ok(adapter) => adapter,
                Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
            };
            let source = entry.data.read().ok().map(|d| d.source);
            let persist = if source == Some(swiss_mcp::registry::Source::Config) {
                ctx.store.upsert_override(&name, def.clone())
            } else {
                ctx.store.update_def(&name, def.clone())
            };
            if let Err(err) = persist {
                return admin_error(StatusCode::BAD_REQUEST, &err);
            }
            // An edit is not a start. A stopped MCP that came back up because its connection
            // string was corrected reads as the panel ignoring the Stop — so a stopped one stays
            // stopped.
            let was_running = entry.data.read().ok().map(|d| d.server.is_some()).unwrap_or(false);
            if let Err(err) = ctx.registry.update_def(&name, def.clone(), adapter, was_running).await {
                return admin_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("restart failed: {err}"));
            }
            admin_json(StatusCode::OK, json!({ "name": name, "type": def.type_(), "lifecycle": lifecycle_of(&ctx, &name) }))
        }),
    );

    // Live details: state, reason, config (secrets masked) and, for proc, captured stderr.
    r = r.route(
        "/api/mcps/{name}/details",
        get(|State(ctx): State<Arc<AppContext>>, Path(name): Path<String>| async move {
            let Some(entry) = ctx.registry.get(&name) else {
                return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
            };
            let d = entry.data.read().ok();
            let Some(d) = d else {
                return admin_error(StatusCode::INTERNAL_SERVER_ERROR, "entry poisoned");
            };
            // Tunnels this MCP's traffic depends on. When a health check fails, the answer to "is
            // it the tunnel or the database?" belongs on the same screen as the failure.
            let tunnels = tunnel_links(&ctx).map(|l| l.tunnels_for_mcp(&d.name)).unwrap_or_default();
            let mut body = json!({
                "name": d.name,
                "source": d.source.as_str(),
                "type": d.adapter.kind(),
                "lifecycle": d.lifecycle.as_str(),
                "state": if d.lifecycle == Lifecycle::Started { d.status.as_str() } else { d.lifecycle.as_str() },
                "logs": d.adapter.logs().unwrap_or_default(),
                "config": serde_json::to_value(mask_def(&d.def)).unwrap_or(Value::Null),
                "tunnels": tunnels,
            });
            // `reason` is absent, never null, until there is one (JSON.stringify drops undefined).
            let reason = if d.lifecycle == Lifecycle::Error {
                d.error.clone()
            } else {
                d.last_error.clone()
            };
            if let (Some(obj), Some(reason)) = (body.as_object_mut(), reason) {
                obj.insert("reason".into(), json!(reason));
            }
            admin_json(StatusCode::OK, body)
        }),
    );

    // One page of this MCP's call log, newest first: the arguments each call received and the
    // reply it sent, read from the on-disk log so it survives a restart. Replies come back
    // shortened; /calls/{seq} returns one in full.
    r = r.route(
        "/api/mcps/{name}/calls",
        get(
            |Path(name): Path<String>,
             Query(q): Query<std::collections::HashMap<String, String>>,
             State(ctx): State<Arc<AppContext>>| async move {
                let Some(entry) = ctx.registry.get(&name) else {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
                };
                let page = q
                    .get("page")
                    .and_then(|p| p.parse::<i64>().ok())
                    .unwrap_or(0);
                let page_size = q
                    .get("pageSize")
                    .and_then(|p| p.parse::<i64>().ok())
                    .unwrap_or(CALLS_PAGE_SIZE as i64);
                let mut result = ctx.calls.read_calls(&name, page, page_size).await;
                result["name"] = json!(name);
                result["stderr"] = json!(entry
                    .data
                    .read()
                    .ok()
                    .and_then(|d| d.adapter.logs())
                    .unwrap_or_default());
                admin_json(StatusCode::OK, result)
            },
        )
        .delete(
            |Path(name): Path<String>, State(ctx): State<Arc<AppContext>>| async move {
                if !ctx.registry.has(&name) {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
                }
                ctx.calls.clear_calls(&name).await;
                admin_json(StatusCode::OK, json!({ "name": name, "cleared": true }))
            },
        ),
    );

    // One logged call with its reply in full — the panel's "Show full result".
    r = r.route(
        "/api/mcps/{name}/calls/{seq}",
        get(|State(ctx): State<Arc<AppContext>>, Path((name, seq)): Path<(String, String)>| async move {
            if !ctx.registry.has(&name) {
                return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
            }
            let Ok(seq) = seq.parse::<u64>() else {
                return admin_error(StatusCode::BAD_REQUEST, "seq must be a number");
            };
            match ctx.calls.read_call(&name, seq).await {
                None => admin_error(
                    StatusCode::NOT_FOUND,
                    &format!("no call #{seq} in the log for {name}"),
                ),
                Some(call) => {
                    admin_json(StatusCode::OK, json!({ "name": name, "call": serde_json::to_value(call).unwrap_or(Value::Null) }))
                }
            }
        }),
    );

    // One tool's newest recorded runs — the Run tab's refill dropdown. Metadata plus a one-line
    // args preview only. "q", when set, narrows the list to runs whose FULL recorded arguments
    // contain it — the preview label clips at 96 chars, the match must not.
    r = r.route(
        "/api/mcps/{name}/tool-history",
        get(
            |State(ctx): State<Arc<AppContext>>,
             Path(name): Path<String>,
             Query(q): Query<std::collections::HashMap<String, String>>| async move {
                if !ctx.registry.has(&name) {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
                }
                let tool = q.get("tool").cloned().unwrap_or_default();
                if tool.is_empty() {
                    return admin_error(StatusCode::BAD_REQUEST, "tool is required");
                }
                let limit = q
                    .get("limit")
                    .and_then(|l| l.parse::<usize>().ok())
                    .filter(|l| *l > 0);
                let entries = ctx
                    .calls
                    .read_tool_history(
                        &name,
                        &tool,
                        limit.unwrap_or(swiss_mcp::calls::TOOL_HISTORY_MAX),
                        q.get("q").map(String::as_str),
                    )
                    .await;
                admin_json(StatusCode::OK, json!({ "tool": tool, "entries": entries }))
            },
        ),
    );

    // Invoke a tool from the panel, so a config can be verified in place ("does SELECT 1 come
    // back?"). It goes through this API rather than the browser POSTing the MCP endpoint
    // directly, because that endpoint is bearer-gated and the gateway token should never be
    // handed to the browser.
    r = r.route(
        "/api/mcps/{name}/call",
        post(|State(ctx): State<Arc<AppContext>>, Path(name): Path<String>, body: crate::reply::NodeBody| async move {
            let Some(entry) = ctx.registry.get(&name) else {
                return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
            };
            let server = entry.data.read().ok().and_then(|d| d.server.clone());
            let Some(server) = server else {
                return admin_error(StatusCode::SERVICE_UNAVAILABLE, &format!("MCP '{name}' is not started"));
            };
            ctx.registry.note_activity(&name); // a panel run is traffic — it keeps a lazy proc's child alive
            let body = body.0;
            let tool = body.get("tool").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if tool.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "tool is required");
            }
            let args = body
                .get("arguments")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            let t0 = Instant::now();
            // The call is logged by the tool handler itself; this only tags where it came from,
            // so the Logs tab can tell a panel experiment apart from a real client's request.
            let outcome = with_call_source_panel(async { server.probe.call_tool(&tool, args).await }).await;
            match outcome {
                Ok(out) => {
                    let is_error = out.get("isError").and_then(Value::as_bool).unwrap_or(false);
                    admin_json(
                        StatusCode::OK,
                        json!({
                            "ok": !is_error,
                            "isError": is_error,
                            "ms": t0.elapsed().as_millis() as u64,
                            "text": swiss_mcp::calls::content_text(&out),
                        }),
                    )
                }
                // A refused command or a SQL error is the interesting output when testing, so
                // report it as a result rather than as a transport failure.
                Err(err) => admin_json(
                    StatusCode::OK,
                    json!({ "ok": false, "isError": true, "ms": t0.elapsed().as_millis() as u64, "text": err }),
                ),
            }
        }),
    );

    // Read one resource from the panel — the counterpart of the tool runner, for the other
    // primitive. Same reasoning as /call: the browser never holds the gateway token.
    r = r.route(
        "/api/mcps/{name}/resource",
        post(
            |State(ctx): State<Arc<AppContext>>,
             Path(name): Path<String>,
             body: crate::reply::NodeBody| async move {
                let Some(entry) = ctx.registry.get(&name) else {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
                };
                let server = entry.data.read().ok().and_then(|d| d.server.clone());
                let Some(server) = server else {
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &format!("MCP '{name}' is not started"),
                    );
                };
                ctx.registry.note_activity(&name); // reading a resource is traffic too
                let body = body.0;
                let uri = body
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if uri.is_empty() {
                    return admin_error(StatusCode::BAD_REQUEST, "uri is required");
                }
                let t0 = Instant::now();
                let outcome =
                    with_call_source_panel(async { server.probe.read_resource(&uri).await }).await;
                match outcome {
                    Ok(out) => {
                        let contents = out
                            .get("contents")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let text = contents
                            .iter()
                            .map(|c| {
                                c.get("text")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .unwrap_or_else(|| {
                                        c.get("blob")
                                            .and_then(Value::as_str)
                                            .map(|b| {
                                                let bytes =
                                                    swiss_core::util::to_hex(b.as_bytes()).len() / 2;
                                                format!("[{bytes} bytes of binary]")
                                            })
                                            .unwrap_or_default()
                                    })
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut body = json!({
                            "ok": true,
                            "ms": t0.elapsed().as_millis() as u64,
                            "text": text,
                        });
                        // mimeType rides the FIRST content block and is absent when it carries none
                        // (Node read `contents[0]?.mimeType` — undefined drops out of the JSON).
                        if let Some(mime) = contents.first().and_then(|c| c.get("mimeType")) {
                            if let Some(obj) = body.as_object_mut() {
                                obj.insert("mimeType".into(), mime.clone());
                            }
                        }
                        admin_json(StatusCode::OK, body)
                    }
                    // A URI that names nothing is the interesting answer here, not a transport
                    // failure.
                    Err(err) => admin_json(
                        StatusCode::OK,
                        json!({ "ok": false, "ms": t0.elapsed().as_millis() as u64, "text": err }),
                    ),
                }
            },
        ),
    );

    // Toggle this MCP's resources on or off (all of them — the master switch). Live: the
    // provider answers an empty list while off, and the resources capability stays announced.
    // (The push notification to held-open clients is a Phase-4 deviation — see registry.rs.)
    r = r.route(
        "/api/mcps/{name}/resources-toggle",
        post(
            |State(ctx): State<Arc<AppContext>>,
             Path(name): Path<String>,
             body: crate::reply::NodeBody| async move {
                let Some(entry) = ctx.registry.get(&name) else {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
                };
                let (toggle, type_) = match entry.data.read() {
                    Ok(d) => (d.adapter.resource_toggle(), d.adapter.kind().to_string()),
                    Err(_) => (None, String::new()),
                };
                let Some(toggle) = toggle else {
                    return admin_error(
                        StatusCode::NOT_IMPLEMENTED,
                        &format!("resource toggle is not supported for MCP type '{type_}'"),
                    );
                };
                let body = body.0;
                let on = body.get("enabled").map(as_bool).unwrap_or(true);
                let was = toggle.load(std::sync::atomic::Ordering::SeqCst);
                if on == was {
                    return admin_json(StatusCode::OK, json!({ "enabled": on, "unchanged": true }));
                }
                // Persist BEFORE mutating the live flag: a failed write must not leave the
                // in-memory state diverged from what the next boot restores.
                if let Err(err) = ctx.store.set_resource_enabled(&name, on) {
                    return admin_error(StatusCode::BAD_REQUEST, &err);
                }
                toggle.store(on, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut d) = entry.data.write() {
                    d.res_page = None; // the cached list no longer matches
                }
                admin_json(StatusCode::OK, json!({ "enabled": on }))
            },
        ),
    );

    // Toggle one tool on or off. Live: mutates the shared toggle the next make_server() reads,
    // so the change is visible to the next tools/list with no restart; persisted so it survives
    // one.
    r = r.route(
        "/api/mcps/{name}/tools/{tool}",
        post(|State(ctx): State<Arc<AppContext>>, Path((name, tool)): Path<(String, String)>, body: crate::reply::NodeBody| async move {
            let Some(entry) = ctx.registry.get(&name) else {
                return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
            };
            let toggle = entry.data.read().ok().and_then(|d| d.adapter.tool_toggle());
            let Some(toggle) = toggle else {
                let type_ = entry.data.read().ok().map(|d| d.adapter.kind().to_string()).unwrap_or_default();
                return admin_error(
                    StatusCode::NOT_IMPLEMENTED,
                    &format!("tool toggles are not supported for MCP type '{type_}'"),
                );
            };
            let body = body.0;
            let enabled = body.get("enabled").map(as_bool).unwrap_or(true); // absent or true → on
            let was = toggle
                .read()
                .ok()
                .map(|set| !set.contains(&tool))
                .unwrap_or(enabled);
            if enabled == was {
                let disabled: Vec<String> = toggle
                    .read()
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default();
                return admin_json(
                    StatusCode::OK,
                    json!({ "tool": tool, "enabled": enabled, "disabledTools": disabled, "unchanged": true }),
                );
            }
            // Build the next set WITHOUT touching the live one, persist it, then swap it in — a
            // failed write must not leave the live set diverged from what the next boot restores.
            let next = {
                let mut next = toggle.read().map(|set| set.clone()).unwrap_or_default();
                if enabled {
                    next.remove(&tool);
                } else {
                    next.insert(tool.clone());
                }
                next
            };
            let disabled: Vec<String> = next.iter().cloned().collect();
            if let Err(err) = ctx.store.set_disabled_tools(&name, &disabled) {
                return admin_error(StatusCode::BAD_REQUEST, &err);
            }
            if let Ok(mut set) = toggle.write() {
                *set = next;
            }
            if let Ok(mut d) = entry.data.write() {
                d.tool_page = None; // the cached (filtered) list no longer matches
            }
            admin_json(StatusCode::OK, json!({ "tool": tool, "enabled": enabled, "disabledTools": disabled }))
        }),
    );

    // Paged tools/resources/prompts via the MCP cursor protocol (see paging.rs).
    r = r.route("/api/mcps/{name}/{kind}", get(mcps_list_kind));

    r
}

/// GET /api/mcps/{name}/{kind} — one paged listing for kind ∈ tools|resources|prompts.
async fn mcps_list_kind(
    State(ctx): State<Arc<AppContext>>,
    Path((name, kind)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if kind != "tools" && kind != "resources" && kind != "prompts" {
        return admin_error(StatusCode::NOT_FOUND, &format!("unknown endpoint: {kind}"));
    }
    let Some(entry) = ctx.registry.get(&name) else {
        return admin_error(StatusCode::NOT_FOUND, &format!("unknown MCP: {name}"));
    };
    let server = entry.data.read().ok().and_then(|d| d.server.clone());
    let Some(server) = server else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("MCP '{name}' is not started"),
        );
    };
    // One page cache per MCP+kind, cleared on (re)start and on toggle.
    let cache = {
        let Ok(mut d) = entry.data.write() else {
            return admin_error(StatusCode::INTERNAL_SERVER_ERROR, "entry poisoned");
        };
        let slot = match kind.as_str() {
            "tools" => &mut d.tool_page,
            "resources" => &mut d.res_page,
            _ => &mut d.prompt_page,
        };
        slot.get_or_insert_with(|| Arc::new(PageCache::new()))
            .clone()
    };
    let cursor = q.get("cursor").cloned();
    // A FRESH in-memory session per page — the port of the Node build's makeServer()
    // call (two concurrent readers must not clobber one shared transport).
    match list_page(server.probe.clone(), &kind, &cache, cursor.as_deref()).await {
        Err(err) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &err),
        Ok(page) => {
            // The list lands under the kind's own key ("tools" | "resources" | "prompts").
            // nextCursor/total are absent until the page carries them — Node's `undefined`s drop
            // out of JSON.stringify, and the panel keys off their presence.
            let mut out = json!({
                "pageSize": PAGE_SIZE,
            });
            if let Some(obj) = out.as_object_mut() {
                if let Some(c) = page.next_cursor {
                    obj.insert("nextCursor".into(), json!(c));
                }
                if let Some(t) = page.total {
                    obj.insert("total".into(), json!(t));
                }
            }
            out[kind.as_str()] = json!(page.items);
            // The tool list is filtered (disabled tools are absent); surface their names so the
            // panel can show them as rows to re-enable. For resources, surface the master on/off
            // so the panel can show the toggle that empties the list.
            if kind == "tools" {
                out["disabledTools"] = json!(ctx.store.disabled_tools(&name));
            } else if kind == "resources" {
                let on = entry
                    .data
                    .read()
                    .ok()
                    .and_then(|d| {
                        d.adapter
                            .resource_toggle()
                            .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
                    })
                    .unwrap_or(true);
                out["resourceEnabled"] = json!(on);
            }
            admin_json(StatusCode::OK, out)
        }
    }
}
