//! The terminal history's cross-module surface (SPEC §terminal.recording, ADR-035):
//! deep-search enrollment through the real query route, the generic body route, the
//! `.cast` export route, and the stopped-module honesty — everything through the same
//! routers the gateway serves.

use std::sync::Arc;

use serde_json::{json, Value};
use tower::ServiceExt;

use swiss_host::services::history_query::HistoryQueryAdapter;
use swiss_host::services::RuntimeServices;
use swiss_terminal::terminal::history::{TerminalBodyReader, TerminalHistory};

/// The composition under test: the services' api router (history query + body routes)
/// merged with the terminal tree, its state carrying one open history store.
async fn wired() -> (axum::Router, Arc<TerminalHistory>, Arc<RuntimeServices>) {
    let root = swiss_core::paths::test_home()
        .join("terminal-routes")
        .join(swiss_core::util::random_hex(6));
    let sessions = root.join("terminal");
    let logs = root.join("logs");
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    std::fs::create_dir_all(&logs).expect("logs dir");
    let history = TerminalHistory::open(&sessions, &logs).await.expect("store opens");
    let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
    services
        .history_query
        .register(HistoryQueryAdapter {
            owner: "terminal".into(),
            db_file: logs.join("terminal.db"),
            source: history.clone(),
            bodies: Some(Arc::new(TerminalBodyReader::new(history.clone()))),
        })
        .expect("the owner registers once");
    let state = swiss_plugins::terminal_api::TerminalState::new();
    state.install_history(history.clone());
    let app = swiss_host::services::api::mount(services.clone())
        .merge(swiss_plugins::terminal_api::mount(state));
    (app, history, services)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (axum::http::StatusCode, Value) {
    use axum::body::Body;
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Record one finished session holding the needle, exactly the way the recorder would.
async fn record_needle(history: &TerminalHistory, sid: &str, needle: &str) -> i64 {
    let uid = swiss_terminal::terminal::history::session_uid(sid);
    let _ = history.begin(uid, sid, 80, 24, "xterm-256color");
    let first = format!("[0.1, \"o\", \"before {needle} after\"]\n");
        let _ = history.append_part(uid, 0, 0, first.as_bytes());
    // The end marker is ordinary recorded output the RECORDER appends (the store's
    // finish only stamps the row) — the helper mirrors the recorder so the export
    // under test is shaped like a real one.
    // The envelope binds each part to its CUMULATIVE offset (the recorder's
    // written-before counter), so part 1's ofs is part 0's length.
        let _ = history.append_part(
        uid,
        1,
        first.len() as u64,
        b"[0.2, \"o\",\"[gateway] session ended: the shell exited with code 0\\n\"]\n",
    );
        let _ = history.finish(uid, "the shell exited with code 0", false);
    history.flush().await;
    uid
}

#[tokio::test]
async fn terminal_output_is_deep_searchable_and_the_body_route_serves_it() {
    let (app, history, services) = wired().await;
    let sid = swiss_core::util::random_hex(16);
    let uid = record_needle(&history, &sid, "TERMINALNEEDLE").await;

    let (status, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "TERMINALNEEDLE", "modules": ["terminal"] }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{page}");
    assert_eq!(hits[0]["module"], "terminal");
    assert_eq!(hits[0]["runId"], json!(uid));
    assert_eq!(page["complete"], json!(true), "{page}");

    // The generic body route: bounded, gone-flagged, byte-cursored — the same contract
    // the other owners serve, now over a recording's stored event lines.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/history/terminal/records/{uid}/body"),
        Value::Null,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert!(body["text"].as_str().unwrap().contains("TERMINALNEEDLE"), "{body}");
    assert_eq!(body["gone"], json!(false));

    // The body route pages by BYTES with the UTF-8 page-edge rule: a read whose
    // max lands mid-character holds the halves together across pages instead of
    // emitting U+FFFD at every edge — the CJK half-loss the line-aligned reader
    // used to cause is the exact regression this pins.
    let body_text = "你好世界".repeat(4000);
    let stream_uid = 7101i64;
    let _ = history.begin(stream_uid, &"f".repeat(32), 80, 24, "xterm");
    // Many event lines, each smaller than a page: the pager must cut INSIDE
    // lines (and inside characters) and still reassemble them exactly.
    let line = format!("[1.0, \"o\", \"{}\"]\n", body_text);
    let bytes = line.as_bytes();
    let _ = history.append_part(stream_uid, 0, 0, bytes);
    history.flush().await;
    let mut assembled = String::new();
    let mut after = 0u64;
    for _ in 0..40 {
        let (status, page) = call(
            &app,
            "GET",
            &format!("/api/history/terminal/records/{stream_uid}/body?after={after}&max=4096"),
            Value::Null,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{page}");
        let text = page["text"].as_str().unwrap_or("");
        assembled.push_str(text);
        let next = page["next"].as_u64().or_else(|| page["next"].as_i64().map(|v| v as u64));
        match next {
            Some(n) if n > after => after = n,
            _ => break,
        }
    }
    assert!(
        !assembled.contains('\u{fffd}'),
        "paged reads never split a character: {} replacement chars",
        assembled.matches('\u{fffd}').count()
    );
    let head: String = body_text.chars().take(80).collect();
    assert!(
        assembled.contains(&head),
        "the paged reassembly carries the original text"
    );

    // A stopped module is skipped by name and its database is not opened (§host.history).
    services.history_query.unregister("terminal");
    let (_, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "TERMINALNEEDLE", "modules": ["terminal"] }),
    )
    .await;
    assert_eq!(page["skipped"], json!(["terminal"]), "{page}");
    assert_eq!(page["hits"].as_array().unwrap().len(), 0);
    let _ = history.close().await;
}

#[tokio::test]
async fn the_cast_export_route_reconstructs_a_byte_faithful_asciicast() {
    let (app, history, _services) = wired().await;
    let sid = swiss_core::util::random_hex(16);
    record_needle(&history, &sid, "CASTNEEDLE").await;

    let request = axum::http::Request::builder()
        .uri(format!("/api/terminal/recordings/{sid}/cast"))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .starts_with("text/plain"));
    let bytes = axum::body::to_bytes(response.into_body(), 8 << 20).await.unwrap();
    let cast = String::from_utf8(bytes.to_vec()).unwrap();
    let mut lines = cast.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header["version"], 2);
    assert!(cast.contains("CASTNEEDLE"));
    assert!(cast.contains("session ended: the shell exited with code 0"));

    // An unknown session id is a plain 404, never an empty cast.
    let (status, _) = call(
        &app,
        "GET",
        "/api/terminal/recordings/does-not-exist/cast",
        Value::Null,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    let _ = history.close().await;
}
