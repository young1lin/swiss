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

//! The keys an MCP definition takes (SPEC §mcp.defs): one table, and a key outside it is refused
//! by name rather than dropped, so a typo cannot read as a saved setting.

use std::path::Path;

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{field_of, setup};
use swiss::adminapi::build_def;

/// Every form in the panel's field table, with the keys it reads. What the submit paths rewrite
/// before the server sees a body is left to the caller: `autostart` is dropped here because it
/// becomes `lazy`, and the pg form's pieces come back as written.
fn panel_forms() -> Vec<(String, Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/swiss-panel/panel/src/fields.ts");
    let text = std::fs::read_to_string(path).expect("read the panel's field table");
    let start = text.find("const TYPE_FIELDS").expect("the field table");
    let end = text.find("const TYPE_LABELS").expect("the end of the field table");
    let mut forms: Vec<(String, Vec<String>)> = Vec::new();
    for line in text[start..end].lines().skip(1) {
        if let Some(name) = line.trim().strip_suffix(": [") {
            forms.push((name.trim_matches('"').to_string(), Vec::new()));
            continue;
        }
        let Some((_, keys)) = forms.last_mut() else { continue };
        if line.contains("DESC_FIELD") {
            keys.push("description".to_string());
        }
        let mut rest = line;
        while let Some(at) = rest.find("k: \"") {
            rest = &rest[at + 4..];
            let Some(close) = rest.find('"') else { break };
            keys.push(rest[..close].to_string());
            rest = &rest[close..];
        }
    }
    forms
}

/// A value the definition builder keeps for this key: a false flag because only an explicit opt-out
/// of `expose*` is stored, a reference because a zai-vision key refuses a literal.
fn value_for(key: &str) -> Value {
    match key {
        "exposeResources" | "exposePrompts" | "allowDestructive" | "allowEval" => json!(false),
        "port" | "db" | "maxRows" | "timeoutMs" => json!(7),
        "env" | "headers" => json!({ "A": "b" }),
        "tools" => json!([{ "name": "x", "request": { "method": "GET", "path": "/x" } }]),
        "apiKey" => json!("${SOME_KEY}"),
        "mode" => json!("ZHIPU"),
        "auth" => json!("oauth"),
        _ => json!("x"),
    }
}

// SPEC §mcp.defs: "anything else is refused", through every route that takes a definition.
#[tokio::test]
async fn an_unknown_mcp_key_is_refused_by_name() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({
                "name": "typo", "type": "http", "url": "https://x.test/mcp",
                "hedaers": { "A": "b" }, "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(error.contains("hedaers"), "names the key: {body}");
    assert!(error.contains("headers"), "lists what the type takes: {body}");
    assert!(!h.registry.has("typo"), "nothing was added");

    // Another type's key is as unknown as a typo.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "wrong", "command": "node x", "url": "https://x.test", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("'url'"), "{body}");

    let (status, _) = h
        .post(
            "/api/mcps",
            json!({ "name": "live", "type": "http", "url": "https://x.test/mcp", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // The same refusal on an edit, a replace and a connection test, and what each route takes for
    // itself is no key anywhere else.
    let edit = json!({ "type": "http", "url": "https://y.test/mcp", "hedaers": {} });
    let (status, body) = h.put("/api/mcps/live", edit.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = h.post("/api/mcps/live/replace", edit.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = h.post("/api/mcpdefs/test", edit).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = h
        .put(
            "/api/mcps/live",
            json!({ "type": "http", "url": "https://y.test/mcp", "note": "not here" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(h.def_of("live")["url"], json!("https://x.test/mcp"), "the def is untouched");
}

// SPEC §mcp.defs: the panel is the spec for the admin API, so the table can never refuse a form
// field, and a key the form offers must reach the definition instead of being dropped.
#[test]
fn every_panel_form_key_is_accepted() {
    let forms = panel_forms();
    assert!(forms.len() >= 10, "the table was read: {forms:?}");
    assert!(
        forms.iter().any(|(form, keys)| form == "proc" && keys.iter().any(|k| k == "timeoutMs")),
        "the proc form was read: {forms:?}"
    );
    for (form, keys) in forms {
        let mut body = json!({ "type": form, "lazy": true });
        for key in keys {
            // fields.ts translatePg: the pg form's pieces reach the server as one url.
            let piece = ["host", "port", "user", "password", "database", "params"].contains(&key.as_str());
            let key = if form == "pg" && piece { "url".to_string() } else { key };
            if key == "autostart" {
                continue;
            }
            body[key.as_str()] = if key == "url" { json!("https://x.test") } else { value_for(&key) };
        }
        let built = build_def(&body, &[]).unwrap_or_else(|err| panic!("{form}: {err}"));
        for key in body.as_object().expect("an object").keys() {
            assert!(built.get(key).is_some(), "{form} dropped {key}");
        }
    }
}

// SPEC §mcp.defs: `args` join into `command` exactly as an import joins them.
#[tokio::test]
async fn args_join_into_the_command_like_an_import() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({
                "name": "added", "type": "proc", "command": "npx", "args": ["-y", "a b", "say \"hi\""],
                "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = h
        .post(
            "/api/mcpdefs/import",
            json!({ "mcpServers": { "imported": { "command": "npx", "args": ["-y", "a b", "say \"hi\""] } } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(field_of(&body["imported"], "name"), ["imported"]);

    let added = h.def_of("added");
    assert_eq!(added["command"], json!("npx -y \"a b\" \"say \\\"hi\\\"\""));
    assert_eq!(added["command"], h.def_of("imported")["command"]);
    assert!(added.get("args").is_none(), "only the joined command is stored: {added}");

    // Without args the command stays as written: the panel's form is one line.
    let plain = build_def(&json!({ "command": "npx -y \"a b\"", "args": [] }), &[]).expect("a def");
    assert_eq!(plain.get_str("command"), Some("npx -y \"a b\""));

    for args in [json!("-y"), json!(["-y", 1])] {
        let (status, body) = h
            .post("/api/mcps", json!({ "name": "bad", "command": "npx", "args": args, "enabled": false }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap_or_default().contains("args"), "{body}");
    }
}

// SPEC §mcp.defs: the panel's Call timeout field is a real key of a proc, on add and on edit.
#[tokio::test]
async fn a_proc_keeps_its_call_timeout() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "slow", "command": "node x", "timeoutMs": 180000, "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(h.def_of("slow")["timeoutMs"], json!(180000));

    let (status, body) = h
        .put("/api/mcps/slow", json!({ "command": "node x", "timeoutMs": 240000 }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.def_of("slow")["timeoutMs"], json!(240000));
}

// SPEC §mcp.defs: the registry reads `idleMs` from a def, so the API takes it on any type.
#[test]
fn idle_ms_rides_on_every_type() {
    for (body, idle) in [
        (json!({ "command": "node x", "idleMs": 0 }), 0),
        (json!({ "type": "mysql", "lazy": true, "idleMs": 5000 }), 5000),
    ] {
        let built = build_def(&body, &[]).expect("a def");
        assert_eq!(built.get("idleMs"), Some(&json!(idle)), "{body}");
    }
}

// SPEC §mcp.defs: `start` decides whether an add starts the MCP, and a disabled one never starts.
#[tokio::test]
async fn start_false_adds_without_starting() {
    let h = setup();
    // A child that could never finish a handshake: starting it would hold this request for the
    // whole handshake timeout.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "later", "command": "node never-handshakes.js", "start": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(!h.has_server("later"));
    assert!(h.def_of("later").get("start").is_none(), "start is not stored");
    assert!(
        h.store.all().iter().any(|m| m.name == "later" && m.enabled),
        "it is still enabled: it wakes on its first request"
    );

    let (status, body) = h.post("/api/mcps", json!({ "name": "now", "type": "echo" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(h.has_server("now"), "without the key, an enabled MCP starts as it always did");

    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "off", "type": "echo", "enabled": false, "start": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(!h.has_server("off"), "a disabled MCP never starts");

    // What a route reads for itself is not a definition key.
    assert!(h.def_of("off").get("enabled").is_none());
}
