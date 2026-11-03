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

//! The MCP module's own history database — the D2 acceptance set (plan
//! docs/plans/module-log-storage.md): migration fidelity (C1), archive semantics the
//! ring could not offer (C2), import honesty (M1/M3), single-db ownership (L5) and
//! clear/retention identity floors (I1). Every test builds its own scratch home.

use serde_json::{json, Value};

use swiss_mcp::calls::{CallLog, CallRecord, CallSource};
use swiss_mcp::history::McpHistory;
use swiss_mcp::traffic::{TrafficEntry, TrafficLog, TrafficQuery};

fn entry_json(seq: u64, tool: &str, args: &str, output: &str, extra: Value) -> Value {
    let mut v = json!({
        "seq": seq,
        "at": "2026-01-02T03:04:05.000Z",
        "tool": tool,
        "via": "mcp",
        "ok": true,
        "ms": 12,
        "args": args,
        "output": output,
        "chars": output.chars().count(),
    });
    if let (Value::Object(base), Value::Object(add)) = (&mut v, &extra) {
        for (k, val) in add {
            base.insert(k.clone(), val.clone());
        }
    }
    v
}

fn write_legacy_call_line(home: &std::path::Path, mcp: &str, line: &Value) {
    let dir = home.join("logs").join("calls");
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = std::fs::read_to_string(dir.join(format!("{mcp}.jsonl"))).unwrap_or_default();
    text.push_str(&line.to_string());
    text.push('\n');
    std::fs::write(dir.join(format!("{mcp}.jsonl")), text).unwrap();
}

fn write_legacy_body(home: &std::path::Path, mcp: &str, seq: u64, body: &str) {
    let dir = home.join("logs").join("calls").join("bodies").join(mcp);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{seq}.txt")), body).unwrap();
}

fn write_legacy_traffic_line(home: &std::path::Path, line: &Value) {
    let file = home.join("logs").join("traffic.jsonl");
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    text.push_str(&line.to_string());
    text.push('\n');
    std::fs::write(&file, text).unwrap();
}

use std::sync::Arc;
use swiss_mcp::history::McpBodyReader;

fn record(history: &McpHistory, mcp: &str, tool: &str, output: &str) {
    history.record_call(
        mcp,
        &CallSource::default(),
        CallRecord {
            tool: tool.to_string(),
            args: Some(json!({ "sql": "select 1" })),
            ok: true,
            ms: 5,
            output: output.to_string(),
        },
    );
}

/// The sweep is the quota's exit, not its victim: with the share exhausted every
/// plain record submit is refused (the wedge state), yet the sweep still deletes -
/// a quota that refuses its own reclamation path would wedge with no exit (the
/// store contract). Regression shape of the round-4 review's quota-wedge finding.
#[tokio::test]
async fn the_sweep_reclaims_even_when_the_share_is_exhausted() {
    let home = swiss_core::paths::test_home().join("mcp-hist-sweep-quota-exit");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    record(&history, "db", "query", "first output");
    record(&history, "db", "query", "second output");
    history.flush().await;
    let (calls, _, bytes, _) = history.usage().await;
    assert!(calls >= 2, "two calls recorded");

    // Wedge the owner: a share below the measured file refuses every plain submit.
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("mcp".to_string(), bytes.saturating_sub(1).max(1));
    history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares));
    record(&history, "db", "query", "refused output");
    history.flush().await;
    let (wedged, _, _, _) = history.usage().await;
    assert_eq!(wedged, calls, "a plain record is refused at the exhausted share");

    // The exit: the sweep submits exempt and still deletes every row. Age zero
    // makes the cutoff "now", so rows written before this line all qualify;
    // the sleep keeps the cutoff strictly after their timestamps.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    history.sweep(0).await;
    history.flush().await;
    let (swept, _, _, _) = history.usage().await;
    assert_eq!(swept, 0, "the exempt sweep reclaims what the quota refused to");
    let _ = history.close().await;
}

fn traffic_query() -> TrafficQuery<'static> {
    TrafficQuery {
        mcp: None,
        client: None,
        method: None,
        actions_only: false,
        page: 0,
        page_size: None,
    }
}

// --- C1: migration fidelity ----------------------------------------------------------------------

#[tokio::test]
async fn call_migration_preserves_seq_preview_bodygone_and_rename() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c1-migration");
    let long_output = "x".repeat(5000);
    // One short call, one long call with a body file, one long call whose body file has
    // since been pruned (bodyGone), one with an explicit client.
    write_legacy_call_line(&home, "db", &entry_json(1, "query", "{\"sql\":\"a\"}", "short", json!({})));
    write_legacy_call_line(
        &home,
        "db",
        &entry_json(2, "query", "{\"sql\":\"b\"}", &"x".repeat(2000), json!({ "preview": true, "body": true })),
    );
    write_legacy_body(&home, "db", 2, &long_output);
    write_legacy_call_line(
        &home,
        "db",
        &entry_json(3, "query", "{\"sql\":\"c\"}", &"y".repeat(2000), json!({ "preview": true, "body": true, "client": "tok" })),
    );
    let history = McpHistory::open(&home.join("logs")).await.unwrap();

    let page = history.read_calls("db", 0, 50, None).await;
    let calls = page["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2]["seq"], json!(1)); // oldest last: the wire order survives
    assert_eq!(calls[1]["seq"], json!(2));
    assert_eq!(calls[1]["preview"], json!(true));
    assert_eq!(calls[0]["client"], json!("tok"));

    let with_body = history.read_call("db", 2).await.unwrap();
    assert_eq!(with_body.output, long_output);
    assert!(with_body.preview.is_none());

    // The pruned body comes back flagged, never silently short.
    let gone = history.read_call("db", 3).await.unwrap();
    assert_eq!(gone.body_gone, Some(true));

    // Rename moves the rows under the new name; the old name is empty afterwards.
    history.rename_calls("db", "renamed").await;
    let moved = history.read_calls("renamed", 0, 50, None).await;
    assert_eq!(moved["calls"].as_array().unwrap().len(), 3);
    let old = history.read_calls("db", 0, 50, None).await;
    assert_eq!(old["calls"].as_array().unwrap().len(), 0);
    let _ = history.close().await;
}

#[tokio::test]
async fn a_recorded_call_is_visible_to_the_very_next_read() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c1-nextread");
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    record(&history, "db", "query", "select-1-result");
    let page = history.read_calls("db", 0, 10, None).await;
    let calls = page["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["tool"], json!("query"));
    assert_eq!(calls[0]["args"], json!("{\"sql\":\"select 1\"}"));
    // The literal search finds the FULL arguments, not a clipped preview.
    let hit = history.read_calls("db", 0, 10, Some("select 1")).await;
    assert_eq!(hit["calls"].as_array().unwrap().len(), 1);
    let _ = history.close().await;
}

#[tokio::test]
async fn tool_history_preserves_wire_ids_for_preview_and_refill() {
    let home = swiss_core::paths::test_home().join("mcp-hist-tool-history-wire");
    let args = r#"{"sql":"SELECT 1"}"#;
    let long_args = json!({ "sql": format!("SELECT '{}' AS note", "x".repeat(180) + "needle") }).to_string();
    write_legacy_call_line(&home, "db", &entry_json(7, "query", args, "old result", json!({})));
    write_legacy_call_line(&home, "db", &entry_json(11, "query", &long_args, "preview", json!({ "preview": true, "body": true })));
    write_legacy_body(&home, "db", 11, "the complete stored result");
    write_legacy_call_line(&home, "db", &entry_json(37, "query", args, "latest error", json!({ "ok": false, "via": "panel", "client": "history-fixture", "ms": 19 })));
    write_legacy_call_line(&home, "db", &entry_json(42, "other", "{}", "other tool", json!({})));
    write_legacy_call_line(&home, "elsewhere", &entry_json(99, "query", "{}", "other MCP", json!({})));

    let log = CallLog::at(home.join("logs").join("calls"));
    let file_rows = log.read_tool_history("db", "query", 10, None).await;
    assert_eq!(file_rows.len(), 2);
    assert_eq!(file_rows[0]["seq"], json!(37));
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    log.attach_history(history.clone());
    let rows = log.read_tool_history("db", "query", 10, None).await;
    // The database row id differs from the public per-MCP seq. Every history row must
    // still address the exact full call that hover and refill read, just as the file path did.
    assert_eq!(rows, file_rows, "the database and file paths keep the same history contract");
    for row in &rows {
        let seq = row["seq"].as_u64().expect("a history row carries its public call sequence");
        let full = log.read_call("db", seq).await.expect("history opens its recorded call");
        assert_eq!(full.seq, seq);
        assert_eq!(full.tool, "query");
        serde_json::from_str::<Value>(&full.args).expect("refill reads complete JSON arguments");
    }
    let latest = log.read_call("db", 37).await.unwrap();
    assert!(!latest.ok);
    assert_eq!(latest.output, "latest error");
    assert_eq!(log.read_call("db", 11).await.unwrap().output, "the complete stored result");
    let found = log.read_tool_history("db", "query", 1, Some("NEEDLE")).await;
    assert_eq!(found.len(), 1, "search matches full arguments beyond the row preview");
    assert_eq!(found[0]["seq"], json!(11));
    assert_eq!(found[0]["args"].as_str().unwrap().chars().count(), 97);
    let _ = history.close().await;
}

// --- C2: traffic archive semantics ----------------------------------------------------------------

#[tokio::test]
async fn traffic_pages_are_metadata_only_and_clients_survive_clear() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c2-pages");
    let log = TrafficLog::memory();
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    log.attach_history(history.clone()).await;
    log.record_traffic(
        "db",
        Some(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"clientInfo":{"name":"claude","version":"1"}}})),
        Some("tok"),
        true,
        3,
        Some("{\"result\":{}}"),
    );
    log.flush_traffic().await;
    let page = history.read_traffic(&traffic_query()).await;
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let row = &entries[0];
    assert!(row.get("body").is_none(), "list pages never carry bodies");
    assert!(row.get("response").is_none());
    assert_eq!(row["hasResponse"], json!(true));
    assert_eq!(row["clientName"], json!("claude"));
    let expanded = history.read_traffic_entry(1).await.unwrap();
    assert!(expanded["body"].as_str().is_some());
    assert!(expanded["response"].as_str().is_some());

    let clients = history.traffic_clients().await;
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["label"], json!("claude 1"));
    assert_eq!(clients[0]["tokens"], json!(["tok"]));

    // Clearing every row keeps the client memory — the jsonl era's known_client rule.
    history.clear_traffic(None).await;
    let cleared = history.read_traffic(&traffic_query()).await;
    assert_eq!(cleared["total"], json!(0));
    let clients_after = history.traffic_clients().await;
    assert_eq!(clients_after.len(), 1, "clients survive a clear");
    assert_eq!(clients_after[0]["label"], json!("claude 1"));
    let _ = history.close().await;
}

#[tokio::test]
async fn retained_traffic_beyond_live_ring_is_queryable() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c2-archive");
    let log = TrafficLog::memory();
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    log.attach_history(history.clone()).await;
    for i in 0..520 {
        let method = if i % 10 == 0 { "tools/call" } else { "tools/list" };
        log.record_traffic(
            "db",
            Some(&json!({"jsonrpc":"2.0","id":i,"method":method})),
            Some("tok"),
            true,
            1,
            None,
        );
    }
    log.flush_traffic().await;
    // The ring holds 500; the archive holds all 520 and the filters still reach the tail.
    let all = history.read_traffic(&traffic_query()).await;
    assert_eq!(all["total"], json!(520));
    let actions = history
        .read_traffic(&TrafficQuery { actions_only: true, ..traffic_query() })
        .await;
    assert_eq!(actions["total"], json!(52));
    let oldest = history
        .read_traffic(&TrafficQuery {
            method: Some("tools/call"),
            page_size: Some(500),
            ..traffic_query()
        })
        .await;
    let entries = oldest["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["seq"].as_u64().unwrap() <= 10),
        "rows older than the live ring are still queryable"
    );
    let _ = history.close().await;
}

// --- M1/M3: import honesty --------------------------------------------------------------------------

#[tokio::test]
async fn import_resume_after_each_commit_is_idempotent() {
    let home = swiss_core::paths::test_home().join("mcp-hist-m1-resume");
    write_legacy_call_line(&home, "db", &entry_json(1, "query", "{}", "one", json!({})));
    write_legacy_call_line(&home, "db", &entry_json(2, "query", "{}", "two", json!({})));
    write_legacy_traffic_line(&home, &json!({
        "seq": 7, "at": "2026-01-02T03:04:05.000Z", "mcp": "db", "method": "tools/list",
        "body": "{}", "ok": true, "ms": 1
    }));
    // A first open imports and stamps done.
    {
        let history = McpHistory::open(&home.join("logs")).await.unwrap();
        let (calls, traffic, _, state) = history.usage().await;
        assert_eq!((calls, traffic, state.as_str()), (2, 1, "done"));
        let _ = history.close().await;
    }
    // A second open does NOT reimport; rows are still there exactly once.
    {
        let history = McpHistory::open(&home.join("logs")).await.unwrap();
        let page = history.read_calls("db", 0, 50, None).await;
        assert_eq!(page["calls"].as_array().unwrap().len(), 2);
        let all = history.read_traffic(&traffic_query()).await;
        assert_eq!(all["total"], json!(1));
        let _ = history.close().await;
    }
    // A crash mid-import resumes safely: active state + OR IGNORE natural keys means the
    // same files folded twice leave one copy of every row.
    {
        let history = McpHistory::open(&home.join("logs")).await.unwrap();
        history.import_legacy().await.unwrap();
        let (calls, traffic, _, _) = history.usage().await;
        assert_eq!((calls, traffic), (2, 1), "resume imports nothing twice");
        let _ = history.close().await;
    }
}

#[tokio::test]
async fn import_every_legacy_variant_preserves_bytes_and_option_absence() {
    let home = swiss_core::paths::test_home().join("mcp-hist-m1-variants");
    // Absent optionals must STAY absent: an entry without client/preview/body imports
    // without those keys, byte-for-byte on the wire.
    write_legacy_call_line(&home, "db", &entry_json(1, "查询", "{\"q\":\"héllo→世界\"}", "réponse ✓", json!({})));
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let page = history.read_calls("db", 0, 10, None).await;
    let row = &page["calls"].as_array().unwrap()[0];
    assert!(row.get("client").is_none());
    assert!(row.get("preview").is_none());
    assert!(row.get("body").is_none());
    assert!(row.get("bodyGone").is_none());
    assert_eq!(row["args"], json!("{\"q\":\"héllo→世界\"}"));
    assert_eq!(row["output"], json!("réponse ✓"));
    // The unicode needle matches through the ASCII-folded contains too (no false negative
    // on non-ASCII stored text).
    let hit = history.read_calls("db", 0, 10, Some("héllo")).await;
    assert_eq!(hit["calls"].as_array().unwrap().len(), 1);
    let _ = history.close().await;
}

#[tokio::test]
async fn torn_tail_is_accounted_without_losing_complete_records() {
    let home = swiss_core::paths::test_home().join("mcp-hist-m3-torn");
    let calls = home.join("logs").join("calls");
    std::fs::create_dir_all(&calls).unwrap();
    let good = entry_json(1, "query", "{}", "kept", json!({})).to_string();
    let mut text = good;
    text.push('\n');
    text.push_str("{\"seq\":2, \"tool\": \"torn"); // killed mid-append: no newline, no brace
    std::fs::write(calls.join("db.jsonl"), text).unwrap();
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let page = history.read_calls("db", 0, 10, None).await;
    assert_eq!(page["calls"].as_array().unwrap().len(), 1);
    let (calls_count, _, _, state) = history.usage().await;
    assert_eq!(calls_count, 1);
    assert_eq!(state, "done");
    let _ = history.close().await;
}

#[tokio::test]
async fn cleanup_never_deletes_unverified_source() {
    let home = swiss_core::paths::test_home().join("mcp-hist-m3-keepsource");
    write_legacy_call_line(&home, "db", &entry_json(1, "query", "{}", "one", json!({})));
    write_legacy_body(&home, "db", 2, "body");
    write_legacy_traffic_line(&home, &json!({
        "seq": 1, "at": "2026-01-02T03:04:05.000Z", "mcp": "db", "method": "tools/list",
        "body": "{}", "ok": true, "ms": 1
    }));
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let (calls, traffic, _, _) = history.usage().await;
    assert_eq!((calls, traffic), (1, 1));
    // The whole legacy tree is still on disk, untouched: retirement is the operator's
    // explicit call, after verifying the cutover.
    assert!(home.join("logs").join("calls").join("db.jsonl").is_file());
    assert!(home.join("logs").join("calls").join("bodies").join("db").join("2.txt").is_file());
    assert!(home.join("logs").join("traffic.jsonl").is_file());
    let _ = history.close().await;
}

// --- L5: one module, one database -------------------------------------------------------------------

#[tokio::test]
async fn mcp_calls_and_traffic_share_one_module_db() {
    let home = swiss_core::paths::test_home().join("mcp-hist-l5-onedb");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    // Two MCPs recording calls and traffic simultaneously: rows add up, and exactly ONE
    // database file appears under the home's logs tree for this module.
    record(&history, "one", "query", "r1");
    record(&history, "two", "query", "r2");
    history.record_traffic(vec![TrafficEntry {
        seq: 1,
        at: "2026-01-02T03:04:05.000Z".into(),
        mcp: "one".into(),
        method: "tools/call".into(),
        client: None,
        client_name: None,
        client_version: None,
        params: None,
        body: "{}".into(),
        response: None,
        ok: true,
        ms: 1,
    }]);
    history.flush().await;
    let (calls, traffic, _, _) = history.usage().await;
    assert_eq!((calls, traffic), (2, 1));
    assert!(logs.join("mcp.db").is_file());
    let db_files: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(db_files, vec!["mcp.db"], "resources add rows, never files");
    let _ = history.close().await;
}

// --- I1: identity floors ------------------------------------------------------------------------------

#[tokio::test]
async fn internal_identity_survives_clear_and_retention() {
    let home = swiss_core::paths::test_home().join("mcp-hist-i1-identity");
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    record(&history, "db", "query", "one");
    record(&history, "db", "query", "two");
    history.flush().await;
    let before = history
        .read_calls("db", 0, 10, None).await;
    let seqs: Vec<u64> = before["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![2, 1]);
    history.clear_calls("db").await;
    record(&history, "db", "query", "three");
    history.flush().await;
    let after = history.read_calls("db", 0, 10, None).await;
    let rows = after["calls"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    // The WIRE sequence starts over (the jsonl clear always did)…
    assert_eq!(rows[0]["seq"], json!(1));
    // …while the internal identity never reuses a number: observable through the
    // historical-order guarantee — a fresh row after a clear is NEWER than everything
    // cleared, so a page over the module must list it first even when its wire seq is 1.
    let ids = history.internal_call_ids("db").await;
    assert_eq!(ids.len(), 1);
    assert!(ids[0] > 2, "internal id continues past the clear floor");
    let _ = history.close().await;
}

// --- L1: the disabled module opens nothing ---------------------------------------------------------------

#[tokio::test]
async fn constructors_open_zero_history_paths_while_disabled() {
    let home = swiss_core::paths::test_home().join("mcp-hist-l1-disabled");
    let logs = home.join("logs");
    std::fs::create_dir_all(logs.join("calls")).unwrap();
    // Construction alone must not read, write or migrate anything under the home.
    let _call_log = CallLog::at(logs.join("calls"));
    let _traffic = TrafficLog::at(logs.clone());
    assert!(
        !logs.join("mcp.db").is_file(),
        "no module database exists before the plugin starts"
    );
    // And an unattached TrafficLog still serves reads from the armed legacy tail on
    // demand — the fallback path for a module that never started.
    write_legacy_traffic_line(&home, &json!({
        "seq": 9, "at": "2026-01-02T03:04:05.000Z", "mcp": "db", "method": "tools/list",
        "body": "{}", "ok": true, "ms": 1
    }));
    let page = _traffic.read_traffic(&traffic_query()).await;
    assert_eq!(page["total"], json!(1), "lazy restore still answers");
}

// --- D5: the module feeds the cross-module literal query ------------------------------------------

/// A tiny seat + router, mirroring what McpInstance::start wires: the adapter is
/// registered while the database is open and withdrawn at stop.
fn wired_seat(
    history: &Arc<McpHistory>,
    logs_dir: &std::path::Path,
) -> (axum::Router, Arc<swiss_host::services::history_query::HistoryQuerySeat>) {
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
    services
        .history_query
        .register(swiss_host::services::history_query::HistoryQueryAdapter {
            owner: "mcp".into(),
            db_file: logs_dir.join("mcp.db"),
            source: history.clone(),
            bodies: None,
        })
        .unwrap();
    (swiss_host::services::api::mount(services.clone()), services.history_query.clone())
}

async fn seat_call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    use axum::body::Body;
    use tower::util::ServiceExt;
    let request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(serde_json::to_string(&body).unwrap())
        })
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn recorded_calls_are_searchable_through_the_history_query_route() {
    let home = swiss_core::paths::test_home().join("mcp-hist-seat-basic");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    // Bodies beyond the 2 KiB preview are the stored ones (D2): the needle must
    // sit past the preview line to prove the scan reads stored chunks, not clips.
    let output = format!("{}all good except SECRET once", "x".repeat(2_500));
    record(&history, "db", "query", &output);
    let (app, seat) = wired_seat(&history, &logs);
    let (status, page) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["mcp"] }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{page}");
    assert_eq!(hits[0]["module"], "mcp");
    assert_eq!(hits[0]["runId"], json!(1));
    // The seat withdrawn at stop: the module is skipped by name, never auto-opened.
    seat.unregister("mcp");
    let (_, page) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["mcp"] }),
    )
    .await;
    assert_eq!(page["skipped"], json!(["mcp"]), "{page}");
    let _ = history.close().await;
}

#[tokio::test]
async fn a_needle_straddling_call_body_parts_is_found_once() {
    let home = swiss_core::paths::test_home().join("mcp-hist-seat-straddle");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    // Exactly two parts: SECRET starts 4 bytes before the 64 KiB boundary.
    let mut output = "a".repeat(65_532);
    output.push_str("SECRET");
    output.push_str(&"b".repeat(64));
    record(&history, "db", "query", &output);
    let (app, _seat) = wired_seat(&history, &logs);
    let (_, page) = seat_call(&app, "POST", "/api/history/query", json!({ "q": "SECRET" })).await;
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "one straddling hit, exactly once: {page}");
    assert_eq!(hits[0]["byteOfs"], json!(65_532), "{page}");
    let _ = history.close().await;
}

// --- D5: the body endpoint -----------------------------------------------------------------------

#[tokio::test]
async fn the_body_endpoint_serves_bounded_slices_with_a_frontier() {
    let home = swiss_core::paths::test_home().join("mcp-hist-body-slice");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    let output = format!("{}SECRET-TAIL", "y".repeat(70_000));
    record(&history, "db", "query", &output);
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
    services
        .history_query
        .register(swiss_host::services::history_query::HistoryQueryAdapter {
            owner: "mcp".into(),
            db_file: logs.join("mcp.db"),
            source: history.clone(),
            bodies: Some(std::sync::Arc::new(McpBodyReader::new(history.clone()))),
        })
        .unwrap();
    let app = swiss_host::services::api::mount(services.clone());
    let call_id: i64 = history.read_calls("db", 0, 1, None).await["calls"][0]["seq"]
        .as_i64()
        .unwrap();
    let id = call_id;
    // The internal call id is what the endpoint routes on; the wire seq and the
    // internal id are the same single-MCP authority here.
    let internal_id = 1i64;
    let (_, page) = seat_call(
        &app,
        "GET",
        &format!("/api/history/mcp/records/{internal_id}/body?after=0&max=100"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(page["text"].as_str().unwrap().len(), 100);
    assert_eq!(page["next"], serde_json::json!(100), "{page}");
    assert_eq!(page["gone"], serde_json::json!(false));
    // The tail slice through the frontier.
    let (_, tail) = seat_call(
        &app,
        "GET",
        &format!("/api/history/mcp/records/{internal_id}/body?after={}&max=200", output.len() - 10),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(tail["text"].as_str().unwrap(), "ECRET-TAIL");
    assert!(tail["next"].is_null(), "{tail}");
    let _ = (call_id, id);
    let _ = history.close().await;
}

#[tokio::test]
async fn short_replies_and_stopped_modules_answer_honestly() {
    let home = swiss_core::paths::test_home().join("mcp-hist-body-honest");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    record(&history, "db", "query", "short reply");
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
    services
        .history_query
        .register(swiss_host::services::history_query::HistoryQueryAdapter {
            owner: "mcp".into(),
            db_file: logs.join("mcp.db"),
            source: history.clone(),
            bodies: Some(std::sync::Arc::new(McpBodyReader::new(history.clone()))),
        })
        .unwrap();
    let app = swiss_host::services::api::mount(services.clone());
    // A short reply never crossed the preview line: gone, not a clipped text.
    let (status, page) = seat_call(&app, "GET", "/api/history/mcp/records/1/body", serde_json::Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    assert_eq!(page["gone"], serde_json::json!(true), "{page}");
    // No such call: an explicit 404 that says why.
    let (status, missing) = seat_call(&app, "GET", "/api/history/mcp/records/99/body", serde_json::Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    assert!(missing["reason"].is_string());
    // The seat withdrawn: the module is unavailable, never auto-opened.
    services.history_query.unregister("mcp");
    let (status, stopped) = seat_call(&app, "GET", "/api/history/mcp/records/1/body", serde_json::Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    assert_eq!(stopped["reason"], serde_json::json!("module is not available"));
    let _ = history.close().await;
}


// --- code-review C1: the resume/cursor contract over the REAL SQL ----------------------------------

#[tokio::test]
async fn a_resume_cursor_inside_a_later_part_still_scans_that_call() {
    let home = swiss_core::paths::test_home().join("mcp-hist-resume-part");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    // Part 0: filler. Part 1: twenty hits - they fill a limit-20 page INSIDE part 1,
    // so the returned cursor anchors at part 1 (byte 65 536). Part 2: five more hits
    // that only a correct strictly-after resume can ever see.
    let mut output = "a".repeat(65_536);
    for _ in 0..20 {
        output.push_str("SECRET");
        output.push_str(&"b".repeat(3_200));
    }
    for _ in 0..5 {
        output.push_str("SECRET");
        output.push_str(&"c".repeat(3_200));
    }
    record(&history, "db", "query", &output);
    let (app, _seat) = wired_seat(&history, &logs);
    let mut total = 0usize;
    let mut cursors: Option<String> = None;
    for _ in 0..10 {
        let mut body = json!({ "q": "SECRET", "limit": 20 });
        if let Some(token) = cursors {
            body["cursors"] = json!({ "mcp": token });
        }
        let (_, page) = seat_call(&app, "POST", "/api/history/query", body).await;
        total += page["hits"].as_array().map(|a| a.len()).unwrap_or(0);
        if page["complete"] == json!(true) {
            break;
        }
        cursors = page["next"]["mcp"].as_str().map(|s| s.to_string());
        assert!(cursors.is_some(), "an incomplete page carries a cursor: {page}");
    }
    assert_eq!(total, 25, "parts 1 and 2 both scanned across the resume: {total}");
    let _ = history.close().await;
}

#[tokio::test]
async fn clearing_history_expires_live_scan_cursors() {
    let home = swiss_core::paths::test_home().join("mcp-hist-clear-cursor");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    record(&history, "db", "query", &format!("{}SECRET-one", "x".repeat(2_500)));
    record(&history, "db", "query", &format!("{}SECRET-two", "y".repeat(2_500)));
    let (app, _seat) = wired_seat(&history, &logs);
    // Page one stops mid-scan and hands back a cursor.
    let (_, page) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 1 }),
    )
    .await;
    let token = page["next"]["mcp"].as_str().expect("a live cursor").to_string();
    // A clear bumps the owner's generation: the live cursor must expire instead of
    // silently serving rows that no longer exist (SPEC host.history, Q4).
    history.clear_calls("db").await;
    let (_, page) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 1, "cursors": { "mcp": token } }),
    )
    .await;
    let errors = page["errors"].as_array().cloned().unwrap_or_default();
    assert!(
        errors
            .iter()
            .any(|e| e["module"] == json!("mcp") && e["reason"] == json!("cursor-expired")),
        "the cleared scan is refused by name: {page}"
    );
    let _ = history.close().await;
}

#[tokio::test]
async fn a_zero_limit_is_clamped_not_an_infinite_empty_scan() {
    let home = swiss_core::paths::test_home().join("mcp-hist-limit-zero");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    record(&history, "db", "query", &format!("{}SECRET-once", "x".repeat(2_500)));
    let (app, _seat) = wired_seat(&history, &logs);
    // limit 0 must behave like the smallest sane page, never as one empty page per
    // stored chunk - the body route clamps its max the same way.
    let (_, page) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 0 }),
    )
    .await;
    let hits = page["hits"].as_array().cloned().unwrap_or_default();
    assert_eq!(hits.len(), 1, "the clamped page answers with the hit: {page}");
    let _ = history.close().await;
}


// --- code-review C2: multi-byte characters straddling the 64 KiB part cuts --------------------------

#[tokio::test]
async fn a_cjk_reply_survives_the_part_boundaries_and_paged_slices_stay_aligned() {
    let home = swiss_core::paths::test_home().join("mcp-hist-utf8-boundary");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    // 65 534 ASCII bytes, then CJK: the first U+56FD (3 bytes) spans 65 534..65 537,
    // so the 64 KiB part cut at 65 536 lands INSIDE a character.
    let mut output = "a".repeat(65_534);
    for _ in 0..40 {
        output.push('\u{56FD}');
    }
    record(&history, "db", "query", &output);
    // The joined full body decodes ONCE: no replacement characters anywhere.
    let entry = history.read_call("db", 1).await.expect("the call");
    assert!(
        !entry.output.contains('\u{FFFD}'),
        "a straddled character must not become mojibake"
    );
    assert!(entry.output.ends_with(&"\u{56FD}".repeat(40)));
    // Walking the bounded body endpoint page by page must reassemble the exact same
    // string: the cursor advances by raw bytes and never splits a character.
    let mut after = 0u64;
    let mut walked = String::new();
    for _ in 0..200 {
        let slice = history.read_call_body(1, after, 1_000).await.unwrap();
        walked.push_str(&slice.text);
        match slice.next {
            Some(next) => after = next,
            None => break,
        }
    }
    assert_eq!(walked, entry.output, "the pages reassemble byte-exactly");
    let _ = history.close().await;
}


// --- round-1 review: the resume anchor may sit INSIDE part zero ------------------------------------

/// A cursor that anchors mid-part-zero must still see the REST of part zero: the
/// old predicate skipped the anchor call's whole part, so a hit later in the same
/// part could never be served on resume.
#[tokio::test]
async fn a_resume_cursor_inside_part_zero_finds_the_rest_of_that_part() {
    let home = swiss_core::paths::test_home().join("mcp-hist-resume-part0");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    // Two hits inside part zero, one more past the 64 KiB boundary in part one:
    // page one fills the whole anchor part (the page may exceed the limit - that
    // is the documented no-truncate fill) and hands a cursor that sits INSIDE
    // part zero. The resume must still see part one's hit.
    let mut output = "a".repeat(40_000);
    output.push_str("SECRET");
    output.push_str(&"b".repeat(20_000));
    output.push_str("SECRET");
    output.push_str(&"c".repeat(10_000));
    output.push_str("SECRET");
    record(&history, "db", "query", &output);
    let (app, _seat) = wired_seat(&history, &logs);
    let (_, page) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 1 }),
    )
    .await;
    let first = page["hits"].as_array().cloned().unwrap_or_default();
    assert_eq!(first.len(), 2, "the anchor part's two hits fill page one: {page}");
    let token = page["next"]["mcp"].as_str().expect("a mid-part cursor").to_string();
    let (_, rest) = seat_call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 10, "cursors": { "mcp": token } }),
    )
    .await;
    let hits = rest["hits"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        hits.len(),
        1,
        "the hit past the anchor part is served on resume: {rest}"
    );
    assert_eq!(rest["complete"], json!(true), "{rest}");
    let _ = history.close().await;
}

// --- round-1 review: one corrupt part must not poison the whole body ------------------------------

/// A part whose block no longer parses (bit rot, a torn write) serves as a HOLE - and
/// the parts after it keep reading, because the seal binding is built from the row's
/// declared write-time offset, never from the bytes served so far. A buf.len()-derived
/// binding cascades one refusal into refusing every later part of the body.
#[tokio::test]
async fn a_corrupted_part_leaves_a_hole_but_the_later_parts_still_read() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c6-cascade");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    let output = format!("{}SECRET-TAIL", "a".repeat(65_536));
    record(&history, "db", "query", &output);
    let whole = history.read_call_body(1, 0, 130_000).await.unwrap();
    assert!(whole.text.contains("SECRET-TAIL"), "the healthy body reads");
    // Bit-rot part 0 in place: the block stops parsing as an envelope (plain), and the
    // raw decode of three garbage bytes against a 64 KiB declared length fails - the
    // part is a hole. Part 1 must still open: its binding pins byte 65 536.
    let db = logs.join("mcp.db");
    let url = format!("sqlite://{}?mode=rw", db.to_string_lossy().replace('\\', "/"));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("reach the store's file");
    sqlx::query("UPDATE call_body SET block = ? WHERE call_id = 1 AND part = 0")
        .bind(vec![0u8, 1, 2, 3])
        .execute(&pool)
        .await
        .expect("rot part 0");
    pool.close().await;
    let holed = history.read_call_body(1, 0, 130_000).await.unwrap();
    assert!(
        holed.text.contains("SECRET-TAIL"),
        "part 1 keeps its write-time offset and still opens: {}",
        holed.text.len()
    );
    let _ = history.close().await;
}

/// A pre-sealing row (or a degraded machine's write) stores PLAIN coded blocks: the
/// open answers plain and the read serves them. A regression that turns plain into
/// refused makes every legacy body silently gone - this pins the fallback.
#[tokio::test]
async fn a_plain_pre_sealing_row_still_reads() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c6-plain");
    let logs = home.join("logs");
    let history = McpHistory::open(&logs).await.unwrap();
    let output = format!("PLAINMARKER-legacy{}", "z".repeat(2_400));
    record(&history, "db", "query", &output);
    // Swap the sealed envelope for the plain bytes a pre-sealing build would have
    // stored: raw codec, declared plain length, marker in the clear.
    let marker = b"PLAINMARKER-legacy".to_vec();
    let db = logs.join("mcp.db");
    let url = format!("sqlite://{}?mode=rw", db.to_string_lossy().replace('\\', "/"));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("reach the store's file");
    sqlx::query("UPDATE call_body SET block = ?, plain = ?, codec = 0 WHERE call_id = 1")
        .bind(&marker)
        .bind(marker.len() as i64)
        .execute(&pool)
        .await
        .expect("rewrite the row as a plain pre-sealing block");
    pool.close().await;
    let entry = history.read_call_body(1, 0, 200).await.unwrap();
    assert!(
        entry.text.starts_with("PLAINMARKER-legacy"),
        "a plain row serves its bytes, not a refusal: {}",
        entry.text.len()
    );
    let _ = history.close().await;
}


// --- code-review C5: the import trio and the traffic read contract ----------------------------------

#[tokio::test]
async fn a_duplicate_legacy_seq_cannot_bind_its_body_to_another_call() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c5-rowid");
    // seq 1 (no body), then seq 2 WITH a stored body, then a DUPLICATE seq 1 carrying a
    // different, longer body. On the buggy path the duplicate's INSERT OR IGNORE leaves
    // last_insert_rowid() stale (seq 2's fresh id) and its body parts bind to SEQ 2's
    // row, corrupting another call's stored reply.
    let mut dup = entry_json(1, "query", "{}", "one", json!({}));
    dup["body"] = json!(true);
    let mut two = entry_json(2, "query", "{}", "two", json!({}));
    two["body"] = json!(true);
    write_legacy_call_line(&home, "db", &entry_json(1, "query", "{}", "one", json!({})));
    write_legacy_call_line(&home, "db", &two);
    write_legacy_call_line(&home, "db", &dup);
    write_legacy_body(&home, "db", 2, "TWO");
    write_legacy_body(&home, "db", 1, "EVIL-BODY-THAT-MUST-NOT-MOVE");
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let entry = history.read_call("db", 2).await.expect("seq 2 exists");
    assert_eq!(
        entry.output, "TWO",
        "seq 2's stored body stays byte-exact; a duplicate seq 1 must not rebind parts onto it"
    );
    // Hygiene invariant the wire cannot observe: no stored body part may be parentless
    // after an import. This is not the red witness (that is the output assertion above);
    // it pins the table shape so a future re-binding regression cannot hide orphans.
    let db = home.join("logs").join("mcp.db");
    let url = format!("sqlite://{}?mode=rw", db.to_string_lossy().replace('\\', "/"));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("reach the store's file");
    let orphans: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM call_body cb LEFT JOIN call c ON c.id = cb.call_id WHERE c.id IS NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("count parentless parts");
    pool.close().await;
    assert_eq!(orphans, 0, "no body part may outlive its call row");
    let _ = history.close().await;
}

#[tokio::test]
async fn reimported_traffic_does_not_recount_ignored_duplicates() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c5-dim");
    let line = json!({
        "seq": 10, "at": "2026-01-02T03:04:05.000Z", "mcp": "db", "method": "tools/list",
        "client": "tok1", "body": "{}", "ok": true, "ms": 1
    });
    write_legacy_traffic_line(&home, &line);
    // The same wire seq twice in the tail (a crash mid-append can leave exactly this):
    // one traffic row, one client, count ONE - never inflated by the ignored duplicate.
    write_legacy_traffic_line(&home, &line);
    write_legacy_traffic_line(&home, &json!({
        "seq": 11, "at": "2026-01-02T03:04:06.000Z", "mcp": "db", "method": "tools/call",
        "client": "tok1", "body": "{}", "ok": true, "ms": 1
    }));
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let (_, traffic, _, _) = history.usage().await;
    assert_eq!(traffic, 2, "two distinct wire seqs, the duplicate ignored");
    let clients = history.traffic_clients().await;
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["count"], json!(2), "count matches distinct rows, not re-import passes");
    let _ = history.close().await;
}

#[tokio::test]
async fn a_refused_import_batch_stays_active_and_retries_on_the_next_open() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c5-refused");
    // One entry whose admission estimate exceeds the writer queue's 16 MiB bound: the
    // batch is refused. The import must NOT stamp done behind it.
    let big = "x".repeat(16 * 1024 * 1024 + 65_536);
    write_legacy_call_line(&home, "db", &entry_json(1, "query", "{}", &big, json!({})));
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let (calls, _, _, state) = history.usage().await;
    assert_eq!(calls, 0, "the oversized batch never landed");
    assert_eq!(state, "active", "a refused batch keeps the import retryable, never done");
    let _ = history.close().await;
    // The source shrinks (the operator trimmed the runaway log): the next open retries
    // and the rows land. A premature done stamp would have made them invisible forever.
    let calls_dir = home.join("logs").join("calls");
    std::fs::write(
        calls_dir.join("db.jsonl"),
        format!("{}\n", entry_json(1, "query", "{}", "small now", json!({}))),
    )
    .unwrap();
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let (calls, _, _, state) = history.usage().await;
    assert_eq!(calls, 1, "the retried import lands the rows");
    assert_eq!(state, "done");
    let _ = history.close().await;
}

#[tokio::test]
async fn a_traffic_read_sees_just_recorded_rows_without_an_explicit_flush() {
    let home = swiss_core::paths::test_home().join("mcp-hist-c5-flush");
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let entry = TrafficEntry {
        seq: 1,
        at: "2026-01-02T03:04:05.000Z".into(),
        mcp: "db".into(),
        method: "tools/list".into(),
        client: Some("tok".into()),
        client_name: None,
        client_version: None,
        params: None,
        body: "{}".into(),
        response: None,
        ok: true,
        ms: 1,
    };
    history.record_traffic(vec![entry]);
    // No manual flush: the read's own flush is the visibility contract every other read
    // path already honors. On the current-thread runtime the queued write is unpolled
    // until something awaits it.
    let page = history.read_traffic(&traffic_query()).await;
    assert_eq!(page["total"], json!(1), "the fresh row is visible to the racing read");
    let _ = history.close().await;
}

// --- C6: sealed at rest -------------------------------------------------------------------------

/// A body chunk is SEALED at rest (SPEC host.history): the plaintext marker never
/// lands in mcp.db, the row carries the sealing key source label, and the read path
/// unseals and serves the body intact after a close/reopen round-trip - the seal's
/// binding is row identity, not process state.
#[tokio::test]
async fn a_body_chunk_is_sealed_at_rest_and_reads_back_intact() {
    // Sealed mode needs a key; a machine with none runs the documented degraded
    // plain-block mode and this acceptance test does not apply.
    assert!(
        !swiss_core::secure::key::no_key_available(),
        "this test asserts sealed mode: the machine must expose a master key source"
    );
    let home = swiss_core::paths::test_home().join("mcp-hist-c6-sealed");
    let _ = std::fs::remove_dir_all(&home);
    // Random-looking hex: it does not compress away, so a plaintext row would hold
    // these exact bytes - exactly what sealing must stop.
    let marker = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    // The preview column keeps the head of the output as plaintext BY DESIGN, so the
    // marker starts past it: everything the preview legitimately holds is padding.
    let output = format!("{}{}", "p".repeat(4096), marker.repeat(80));
    {
        let history = McpHistory::open(&home.join("logs")).await.unwrap();
        record(&history, "alpha", "query", &output);
        history.flush().await;
        // Committed pages may still live in the WAL: physical bytes means the db file
        // AND its sidecars, which is exactly what quota accounting charges too.
        let mut db = std::fs::read(home.join("logs").join("mcp.db")).unwrap();
        if let Ok(wal) = std::fs::read(home.join("logs").join("mcp.db-wal")) {
            db.extend_from_slice(&wal);
        }
        assert!(
            !db.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "the plaintext marker must not appear in the database file"
        );
        assert!(
            db.windows(b"history:mcp:call_body".len()).any(|w| w == b"history:mcp:call_body"),
            "sealed rows carry the sealing key source label"
        );
        let _ = history.close().await;
    }
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let page = history.read_calls("alpha", 0, 10, None).await;
    let seq = page["calls"][0]["seq"].as_u64().unwrap();
    let call = history.read_call("alpha", seq).await.unwrap();
    assert_eq!(call.output, output, "the reopened store serves the sealed body intact");
    let _ = history.close().await;
    let _ = std::fs::remove_dir_all(&home);
}


/// A hand-set cursor that points INSIDE a multi-byte character must terminate a
/// follower: the next cursor advances to the character boundary instead of
/// returning the same offset forever (the livelock the round-3 audit found).
#[tokio::test]
async fn a_cursor_inside_a_leading_multibyte_char_advances_to_the_boundary() {
    let home = swiss_core::paths::test_home().join("mcp-hist-midchar");
    let _ = std::fs::remove_dir_all(&home);
    let history = McpHistory::open(&home.join("logs")).await.unwrap();
    let emoji = "\u{1F600}"; // 4 bytes at the body start
    let pad = "pad ".repeat(600); // push the body past the inline preview
    let body = format!("{emoji}{pad}TAIL-MIDCHAR");
    record(&history, "alpha", "query", &body);
    history.flush().await;
    let first = history.read_call_body(1, 1, 1_000).await.unwrap();
    // Byte 1 is inside the 4-byte emoji: the window must serve from the boundary
    // (clean "pad ..."), never a replacement character for the swallowed bytes.
    assert!(
        first.text.starts_with("pad "),
        "the window serves from the character boundary: {:?}",
        first.text.chars().take(6).collect::<String>()
    );
    assert!(
        first.next.unwrap_or(0) > 4,
        "the cursor advances past the partial character"
    );
    let second = history.read_call_body(1, 4, 1_000).await.unwrap();
    assert!(second.text.starts_with("pad "), "the follower reads from the boundary");
    let _ = history.close().await;
    let _ = std::fs::remove_dir_all(&home);
}
