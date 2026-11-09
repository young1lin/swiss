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

//! The Remote module's own history database — the D3 acceptance set (plan
//! docs/plans/module-log-storage.md): archive semantics the coordinator ring never had
//! (L5), byte-cursor fidelity over chunked storage (B1), import honesty (M1–M3) and the
//! clear/identity floors (I1). Everything runs through the real coordinator, the real
//! routes and the real sink seam.

use std::sync::Arc;

use serde_json::{json, Value};

use swiss_remote::store::RemoteHistory;

/// Wire the full chain: fake transport, actions, the sink, the module database
/// attached. The scratch home is the system's own, so the legacy tree the spill writes
/// sits exactly where the importer looks for it.
async fn wired() -> (
    axum::Router,
    Arc<swiss_remote::RemoteSystem>,
    Arc<RemoteHistory>,
    std::path::PathBuf,
    Arc<swiss_remote::testing::FakeTransport>,
) {
    let (system, fake) = swiss_remote::testing::system_with_fake();
    fake.program(
        "true",
        swiss_remote::testing::FakeProgram {
            argv0: "true".into(),
            stdout: b"ok\n".to_vec(),
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 0,
        },
    );
    swiss_remote::actions::register_all(system.clone(), &system.services().actions).unwrap();
    let sink: Arc<dyn swiss_host::services::runs::RunHistorySink> = system.history().clone();
    system.services().runs.add_history_sink(sink);
    let logs_root = system.history().dir().parent().unwrap().to_path_buf();
    let history = RemoteHistory::open(&logs_root).await.unwrap();
    system.history().attach_history(history.clone()).await;
    // Mirror the plugin's start wiring: this database is searchable while open.
    system
        .services()
        .history_query
        .register(swiss_host::services::history_query::HistoryQueryAdapter {
            owner: "remote".into(),
            db_file: logs_root.join("remote.db"),
            source: history.clone(),
            bodies: None,
        })
        .unwrap();
    let state = swiss_remote::api::RemoteState::new();
    state.install(system.clone());
    let app = swiss_host::services::api::mount(system.services().clone()).merge(swiss_remote::api::mount(state));
    (app, system, history, logs_root, fake)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (axum::http::StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Submit a remote.exec and wait for it to go terminal (the chain tests' poll loop).
async fn exec_and_wait(app: &axum::Router, target: &str, argv: &str) -> u64 {
    let (status, submitted) = call(
        app,
        "POST",
        "/api/runs",
        json!({ "action": "remote.exec", "input": { "target": target, "argv": [argv] }, "timeoutMs": 30000 }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{submitted}");
    let run_id = submitted["runId"].as_u64().unwrap();
    for _ in 0..500 {
        let (_, run) = call(app, "GET", &format!("/api/runs/{run_id}?output=0"), Value::Null).await;
        if run["state"] != "queued" && run["state"] != "running" {
            return run_id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("the run never went terminal");
}

// --- L5: one module, one database ------------------------------------------------------------------

#[tokio::test]
async fn remote_targets_share_one_module_db() {
    let (app, system, history, logs_root, _fake) = wired().await;
    // A second target: resources add rows, never files or pools.
    system.with_store(|store| {
        store.add(swiss_remote::target::RemoteTarget {
            id: "prod".into(),
            label: "prod".into(),
            endpoint: "conn-1".into(),
            workspace_root: "/data/other".into(),
            shell: "posix".into(),
            capabilities: vec!["exec".into()],
            default_timeout_ms: None,
            group: None,
        })
        .expect("the target adds");
    });
    let a = exec_and_wait(&app, "dev", "true").await;
    let b = exec_and_wait(&app, "prod", "true").await;
    let (status, page) = call(&app, "GET", "/api/remote/runs", Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let rows = page["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{page}");
    assert!(rows.iter().any(|r| r["runId"] == json!(a)));
    assert!(rows.iter().any(|r| r["runId"] == json!(b)));
    assert!(logs_root.join("remote.db").is_file());
    let db_files: Vec<String> = std::fs::read_dir(&logs_root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".db"))
        .collect();
    assert_eq!(db_files, vec!["remote.db".to_string()]);
    let _ = history.close().await;
}

// --- B1: the byte cursor over chunked storage --------------------------------------------------------

#[tokio::test]
async fn migrated_output_cursors_reassemble_utf8_byte_exactly() {
    // A stream that crosses the 64 KiB chunk boundary in the MIDDLE of an emoji: the
    // follower must read the character whole on the next call, and the concatenated
    // bytes must equal the stream byte for byte.
    let mut stdout: Vec<u8> = Vec::new();
    stdout.resize(64 * 1024 - 1, b'a');
    stdout.extend("😀😀😀😀".as_bytes()); // 4-byte chars spanning the boundary
    stdout.resize(stdout.len() + 2048, b'z');
    let (_app, _system, _opened, logs, _fake) = wired().await;
    // The migration path itself: a fabricated finished run whose out/*.txt holds the
    // stream — exactly what record_run files for a live run and what the importer
    // folds for a legacy one. The cursor contract is the store's either way.
    let logs = logs.join("remote");
    std::fs::create_dir_all(logs.join("out")).unwrap();
    std::fs::write(logs.join("out").join("9001.txt"), &stdout).unwrap();
    let line = json!({
        "runId": 9001, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
        "endedAtMs": 1770000000000u64, "target": "dev", "actor": "api",
        "input": { "target": "dev", "argv": ["emit"] },
    });
    let history = RemoteHistory::open(logs.parent().unwrap()).await.unwrap();
    history.record_run(&line, Some(&logs.join("out").join("9001.txt")), None).await.unwrap();
    let _ = history.flush().await;
    let total = stdout.len() as u64;
    // Walk the whole stream in odd-sized windows — a cursor is bytes, not rows.
    let mut cursor = 0u64;
    let mut reassembled: Vec<u8> = Vec::new();
    while cursor < total {
        let chunk = history.output(9001, cursor, 70001).await.unwrap();
        assert_eq!(chunk.cursor, cursor);
        reassembled.extend_from_slice(chunk.text.as_bytes());
        cursor = chunk.next_cursor;
        assert!(cursor > chunk.cursor || chunk.text.is_empty());
    }
    assert_eq!(reassembled, stdout, "the reassembled stream is byte-exact");
    // A window that would end inside the 4-byte emoji stops before it — the follower
    // reads the character whole on the next call, so nothing is lost or doubled.
    let mid = history.output(9001, 64 * 1024 - 2, 4).await.unwrap();
    assert!(mid.text.is_empty() || mid.text.len() <= 2);
    let _ = history.close().await;
}

#[tokio::test]
async fn a_line_that_names_its_target_in_meta_only_still_filters_and_old_rows_backfill() {
    // The coordinator's own lines carry the target under meta/input, never at the
    // top level - the shape the first writer missed, so every recorded row filed
    // target NULL and the panel's target filter answered an empty page while the
    // unfiltered list showed every run (the owner's finding, 2026-10-10).
    let dir = std::env::temp_dir().join(format!("swiss-remote-tgt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let line = json!({
        "runId": 9101, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
        "endedAtMs": 1770000000001u64, "actor": "api",
        "meta": { "target": "test", "endpoint": "e" },
        "input": { "target": "test", "argv": ["true"] },
    });
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        history.record_run(&line, None, None).await.unwrap();
        history.flush().await.unwrap();
        // The fresh row filters by target.
        let (page, _) = history
            .page(&swiss_remote::history::PageQuery { target: Some("test"), limit: 20, ..Default::default() })
            .await;
        assert_eq!(page.len(), 1, "the meta's target is the row's target");
        let _ = history.close().await;
    }
    // A row the FIRST writer filed (target NULL): the next open repairs it from its
    // own view, and the filter that answered empty finds it.
    let db = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", dir.join("remote.db").display())).await.unwrap();
    sqlx::query("UPDATE run SET target = NULL").execute(&db).await.unwrap();
    db.close().await;
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        let (page, _) = history
            .page(&swiss_remote::history::PageQuery { target: Some("test"), limit: 20, ..Default::default() })
            .await;
        assert_eq!(page.len(), 1, "the backfill at open repairs the old NULL rows");
        let _ = history.close().await;
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn rows_filed_with_a_credential_in_the_command_are_masked_at_open() {
    // 2026-10-21: a redis password typed into a `bash -c` word on target test sat in the
    // rows of those runs - the Runs page, `swiss run audit`, its exports. Synthetic value.
    use swiss_core::secure::literal::MASK;
    let dir = std::env::temp_dir().join(format!("swiss-remote-lit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = "R(){ sudo -n docker exec -e REDISCLI_AUTH=Xq7pLm2Rv9 redis redis-cli \"$@\"; }; R ping";
    let typed = json!({
        "runId": 9201, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
        "endedAtMs": 1770000000002u64, "actor": "api", "meta": { "target": "test" },
        "input": { "target": "test", "argv": ["bash", "-c", script] },
    });
    let plain = json!({
        "runId": 9202, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
        "endedAtMs": 1770000000003u64, "actor": "api", "meta": { "target": "test" },
        "input": { "target": "test", "argv": ["redis-cli", "--no-auth-warning", "ping"] },
    });
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        history.record_run(&typed, None, None).await.unwrap();
        history.record_run(&plain, None, None).await.unwrap();
        let _ = history.close().await;
    }
    // A database from before the pass: the value in clear and no marker that it ran.
    let url = format!("sqlite://{}?mode=rwc", dir.join("remote.db").display());
    let held = "SELECT count(*) FROM run WHERE view LIKE '%Xq7pLm2Rv9%'";
    let db = sqlx::SqlitePool::connect(&url).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>(held).fetch_one(&db).await.unwrap(), 1);
    sqlx::query("DELETE FROM meta WHERE key = 'literal_mask'").execute(&db).await.unwrap();
    db.close().await;
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        let row = history.get(9201).await.expect("kept, masked");
        assert_eq!(
            row["input"]["argv"][2],
            json!(format!("R(){{ sudo -n docker exec -e REDISCLI_AUTH={MASK} redis redis-cli \"$@\"; }}; R ping"))
        );
        let row = history.get(9202).await.expect("untouched");
        assert_eq!(row["input"]["argv"], plain["input"]["argv"]);
        let _ = history.close().await;
    }
    let db = sqlx::SqlitePool::connect(&url).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>(held).fetch_one(&db).await.unwrap(), 0);
    let marker: String = sqlx::query_scalar("SELECT value FROM meta WHERE key = 'literal_mask'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(marker, "1");
    db.close().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_exec_stdin_is_sealed_beside_the_record_and_unseals_on_show() {
    // The owner's finding (2026-10-10): a `bash -s` run's record said argv and a byte
    // count - WHAT ran was nowhere. The stdin is sealed beside the record now, exactly
    // as a write's body is, and unseals through the same route. The line never carries
    // the text (a script may hold a password in clear) - only stdinBytes and stdinStored.
    let (app, _system, history, _logs_root, _fake) = wired().await;
    let script = "echo marker-$RANDOM\nexit 0\n";
    let (status, submitted) = call(
        &app,
        "POST",
        "/api/runs",
        json!({ "action": "remote.exec", "input": { "target": "dev", "argv": ["true"], "stdin": script }, "timeoutMs": 30000 }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{submitted}");
    let run_id = submitted["runId"].as_u64().unwrap();
    for _ in 0..500 {
        let (_, run) = call(&app, "GET", &format!("/api/runs/{run_id}?output=0"), Value::Null).await;
        if run["state"] != "queued" && run["state"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let (_, page) = call(&app, "GET", "/api/remote/runs", Value::Null).await;
    let row = page["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["runId"] == json!(run_id))
        .expect("the run recorded")
        .clone();
    assert_eq!(row["stdinStored"], json!(true), "the line says the stdin is sealed");
    assert_eq!(row["input"]["stdinBytes"], json!(script.len()), "the size is accounted");
    assert!(row["input"].get("stdin").is_none(), "the text never rides the line");
    let (status, body) = call(&app, "GET", &format!("/api/remote/runs/{run_id}/content"), Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(body["content"], json!(script), "Show unseals what actually ran");
    assert_eq!(body["truncated"], json!(false));
    let _ = history.close().await;
}

#[tokio::test]
async fn a_stdin_only_row_counts_in_the_ledger_and_strips_honestly() {
    // Two gaps the owner's review found in the stdin seal (2026-10-10): the ledger
    // rebuild dropped the sealed stdin's bytes (the budget undercounted, and a
    // stdin-only row was never reclaimable), and a strip deleted the shared content
    // file while the line kept advertising stdinStored (a dead Show button).
    let (app, system, history, _logs_root, fake) = wired().await;
    fake.program(
        "silent",
        swiss_remote::testing::FakeProgram {
            argv0: "silent".into(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 0,
        },
    );
    let script = "printf hi\n";
    let (status, submitted) = call(
        &app,
        "POST",
        "/api/runs",
        json!({ "action": "remote.exec", "input": { "target": "dev", "argv": ["silent"], "stdin": script }, "timeoutMs": 30000 }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{submitted}");
    let run_id = submitted["runId"].as_u64().unwrap();
    for _ in 0..500 {
        let (_, run) = call(&app, "GET", &format!("/api/runs/{run_id}?output=0"), Value::Null).await;
        if run["state"] != "queued" && run["state"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let run_history = system.history();
    let row = run_history.get(run_id).expect("the run recorded");
    assert_eq!(row["stdinStored"], json!(true), "the line says the stdin is sealed");
    assert!(row["stdinFileBytes"].as_u64().expect("the sealed size rides the line") > 0);
    assert_eq!(row["outputBytes"], json!(0), "no stdout: the sealed stdin is the only file");
    let (before, _) = run_history.usage();
    let dir = run_history.dir().to_path_buf();
    let stdin_bytes = std::fs::metadata(dir.join(format!("out/{run_id}.content")))
        .expect("the sealed stdin file")
        .len();
    assert!(stdin_bytes > 0);
    assert!(before >= stdin_bytes + row.to_string().len() as u64, "the live ledger counts the stdin file");

    // Gap A: a ledger rebuilt from the index (a reopen) must still count it.
    let tiny = swiss_remote::history::Limits {
        max_total_bytes: before - 1, // the pass must strip to come back under
        ..swiss_remote::history::Limits::default()
    };
    let reopened = swiss_remote::RunHistory::open_with(dir.clone(), tiny);
    let (after, _) = reopened.usage();
    assert_eq!(before, after, "the rebuilt ledger counts the stdin file's bytes");

    // Gap B: the strip flips the stdin flags instead of advertising a dead file.
    let _ = reopened.page(None, 10, None); // a page runs the budget pass first
    let stripped = reopened.get(run_id).expect("the stripped line stays");
    assert_eq!(stripped["outputEvicted"], json!(true));
    assert_eq!(stripped["stdinStored"], json!(false), "no stdinStored after the file is deleted");
    assert_eq!(stripped["stdinEvicted"], json!(true), "the eviction is named, not implied");
    assert!(
        reopened.content(run_id).expect("content read").is_none(),
        "Show must not point at a deleted file"
    );
    let _ = history.close().await;
}

#[tokio::test]
async fn byte_cursor_is_not_record_cursor() {
    let (app, _system, history, logs, _fake) = wired().await;
    let a = exec_and_wait(&app, "dev", "true").await;
    let _b = exec_and_wait(&app, "dev", "true").await;
    let (status, one) = call(&app, "GET", &format!("/api/remote/runs/{a}"), Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let output_bytes = one["outputBytes"].as_u64().unwrap();
    assert!(output_bytes > 0);
    // after=N reads from byte N of THIS run's stream, whatever other rows exist.
    let chunk = history.output(a, 2, 1024).await.unwrap();
    assert_eq!(chunk.cursor, 2);
    assert_eq!(chunk.total, output_bytes);
    let _ = logs;
    let _ = history.close().await;
}

#[tokio::test]
async fn gaps_never_join_head_to_tail() {
    let (app, _system, history, logs, fake) = wired().await;
    fake.program(
        "big",
        swiss_remote::testing::FakeProgram {
            argv0: "big".into(),
            stdout: vec![b'g'; 150 * 1024],
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 0,
        },
    );
    let run = exec_and_wait(&app, "dev", "big").await;
    let _ = history.flush().await;
    let db = logs.join("remote.db");
    // A middle chunk goes missing (the honest worst case the reader must not paper
    // over): open the file directly and drop part 1.
    {
        // A second connection over the same file (WAL allows it) drops one middle
        // chunk row — the honest worst case the reader must not paper over.
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&db)
            .read_only(false);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("DELETE FROM run_output WHERE run_id = ? AND part = 1")
            .bind(run as i64)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
    let full = history.output(run, 0, 128 * 1024).await.unwrap();
    let head_len = full.next_cursor;
    // The read stopped at the hole: it never returned tail bytes as if they followed.
    assert!(head_len < full.total, "the gap stopped the read before the end");
    let tail_probe = history.output(run, head_len, 128 * 1024).await;
    if let Some(probe) = tail_probe {
        assert!(
            probe.text.is_empty() || probe.next_cursor <= head_len + 64 * 1024,
            "a gap never joins head to tail"
        );
    }
    let _ = history.close().await;
}

// --- M1/M2/M3: import honesty -------------------------------------------------------------------------

#[tokio::test]
async fn import_is_idempotent_and_resume_safe() {
    let (app, _system, history, logs, _fake) = wired().await;
    let run = exec_and_wait(&app, "dev", "true").await;
    let _ = history.flush().await;
    let (runs, _, state) = history.usage().await;
    assert_eq!((runs, state.as_str()), (1, "done"), "the spill folded once");
    // A "crash then resume": the same import runs again and duplicates nothing.
    history.import_legacy(&logs.join("remote")).await.unwrap();
    let (runs2, _, _) = history.usage().await;
    assert_eq!(runs2, 1);
    // And a full reopen of the database over the same tree changes nothing.
    let _ = history.close().await;
    let history2 = RemoteHistory::open(&logs).await.unwrap();
    let page = history2.page(&swiss_remote::history::PageQuery::default()).await;
    assert_eq!(page.0.len(), 1);
    assert_eq!(page.0[0]["runId"], json!(run));
    let _ = history2.close().await;
}

#[tokio::test]
async fn torn_tail_is_counted_and_sources_survive() {
    let (app, _system, history, logs, _fake) = wired().await;
    let run = exec_and_wait(&app, "dev", "true").await;
    let _ = history.flush().await;
    // A kill mid-append: a torn last line in the spill index.
    let index = logs.join("remote").join("runs.jsonl");
    let mut text = std::fs::read_to_string(&index).unwrap();
    text.push_str("{\"runId\":99,{\"torn");
    std::fs::write(&index, text).unwrap();
    let _ = history.close().await;
    let history2 = RemoteHistory::open(&logs).await.unwrap();
    let (runs, _, state) = history2.usage().await;
    assert_eq!((runs, state.as_str()), (1, "done"), "the torn tail is counted, not fatal");
    // M3: the whole legacy tree is still on disk — the importer never deletes.
    assert!(index.is_file());
    assert!(logs.join("remote").join("out").join(format!("{run}.txt")).is_file());
    let _ = history2.close().await;
}

#[tokio::test]
async fn foreign_envelope_is_an_error_and_the_source_stays() {
    let (_app, _system, history, logs, _fake) = wired().await;
    // Reset to a pre-cutover state: the wired db (already imported, done) goes away,
    // so the next open performs the one import that must fold this tree.
    let _ = history.close().await;
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(logs.join(format!("remote.db{suffix}")));
    }
    // A content file sealed by ANOTHER machine's key: the reader must say so, and the
    // import must keep the file exactly as it found it.
    let dir = logs.join("remote");
    std::fs::create_dir_all(dir.join("out")).unwrap();
    let foreign_key = [7u8; 32];
    let payload = serde_json::to_string(&json!({ "content": "sekrit", "truncated": false })).unwrap();
    let sealed = swiss_core::secure::envelope::seal(&foreign_key, "foreign", &payload);
    let text = serde_json::to_string(&serde_json::to_value(&sealed).unwrap()).unwrap();
    std::fs::write(dir.join("out").join("7001.content"), &text).unwrap();
    std::fs::write(
        dir.join("runs.jsonl"),
        concat!(
            "{\"runId\":7001,\"action\":\"remote.write\",",
            "\"state\":\"succeeded\",\"endedAtMs\":1770000000000}\u{a}"
        ),
    )
    .unwrap();
    let _ = history.close().await;
    let history2 = RemoteHistory::open(&logs).await.unwrap();
    match history2.content(7001).await {
        Err(_) => {}
        Ok(kept) => panic!("a foreign envelope must not open: {kept:?}"),
    }
    assert!(dir.join("out").join("7001.content").is_file(), "the source stays");
    let _ = history2.close().await;
}



// --- D5: the module feeds the cross-module literal query ------------------------------------------

#[tokio::test]
async fn recorded_output_is_searchable_through_the_history_query_route() {
    let (app, system, history, logs, fake) = wired().await;
    fake.program(
        "findme",
        swiss_remote::testing::FakeProgram {
            argv0: "findme".into(),
            stdout: b"all good except SECRET leak\n".to_vec(),
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 0,
        },
    );
    let run = exec_and_wait(&app, "dev", "findme").await;
    let (status, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["remote"] }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{page}");
    assert_eq!(hits[0]["module"], "remote");
    assert_eq!(hits[0]["runId"], json!(run));
    assert_eq!(page["complete"], json!(true), "{page}");
    // A stopped module is skipped by name and its database is not opened.
    system
        .services()
        .history_query
        .unregister("remote");
    let (_, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["remote"] }),
    )
    .await;
    assert_eq!(page["skipped"], json!(["remote"]), "{page}");
    assert_eq!(page["hits"].as_array().unwrap().len(), 0);
    let _ = history.close().await;
    let _ = logs;
}
// --- R2: stop cancels accepted remote work by identity ----------------------------------------------

/// R2 (SPEC `§remote.history`, `§host.lifecycle`): a provider's stop cancels runs still
/// executing its ACTIONS — whoever submitted them — and leaves every unrelated run
/// alive. This is the exact call the plugin's stop makes.
#[tokio::test]
async fn stop_cancels_accepted_remote_runs_and_keeps_unrelated_runs_alive() {
    use swiss_host::services::action::{Action, ActionOutcome, CancelHandle};
    let (system, fake) = swiss_remote::testing::system_with_fake();
    fake.program(
        "slow",
        swiss_remote::testing::FakeProgram {
            argv0: "slow".into(),
            // The delay spaces stdout chunks: an empty stdout never parks (the
            // run would finish instantly and the cancel window would be a race).
            stdout: b"pending\n".to_vec(),
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 60_000,
        },
    );
    swiss_remote::actions::register_all(system.clone(), &system.services().actions).unwrap();
    struct Nap;
    #[async_trait::async_trait]
    impl Action for Nap {
        fn type_name(&self) -> &'static str {
            "test.nap"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(
            &self,
            _input: &Value,
            cancel: CancelHandle,
        ) -> Result<ActionOutcome, swiss_host::services::action::ActionError> {
            cancel.cancelled().await;
            Ok(ActionOutcome::ok())
        }
    }
    system
        .services()
        .actions
        .register(Arc::new(Nap))
        .expect("the unrelated action registers");
    let sink: Arc<dyn swiss_host::services::runs::RunHistorySink> = system.history().clone();
    system.services().runs.add_history_sink(sink);
    let state = swiss_remote::api::RemoteState::new();
    state.install(system.clone());
    let app = swiss_host::services::api::mount(system.services().clone())
        .merge(swiss_remote::api::mount(state));

    // The same owner submits a remote run AND an unrelated one: cancellation is by
    // action identity, never by submitting owner (plan §5).
    let (_, remote_sub) = call(
        &app,
        "POST",
        "/api/runs",
        json!({ "action": "remote.exec", "input": { "target": "dev", "argv": ["slow"] }, "timeoutMs": 60000, "owner": "jobs" }),
    )
    .await;
    let (_, nap_sub) = call(
        &app,
        "POST",
        "/api/runs",
        json!({ "action": "test.nap", "input": {}, "timeoutMs": 60000, "owner": "jobs" }),
    )
    .await;
    let remote_id = remote_sub["runId"].as_u64().unwrap();
    let nap_id = nap_sub["runId"].as_u64().unwrap();
    for _ in 0..500 {
        let (_, r) = call(&app, "GET", &format!("/api/runs/{remote_id}?output=0"), Value::Null).await;
        let (_, n) = call(&app, "GET", &format!("/api/runs/{nap_id}?output=0"), Value::Null).await;
        if r["state"] == "running" && n["state"] == "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    // The plugin's stop call: the five remote actions, by identity.
    let canceled = system
        .services()
        .runs
        .cancel_action_types(&[
            "remote.exec",
            "remote.sync",
            "remote.pull",
            "remote.cat",
            "remote.write",
        ])
        .await;
    assert_eq!(canceled, 1, "exactly the accepted remote run was canceled");
    // The canceled run is terminal — the drain AWAITED it before returning, which is
    // the "acks before close" half of R2. (The fake transport exits its delayed
    // program cleanly on cancel, so the terminal state is its clean exit; the
    // coordinator-level canceled-state path is the host's own cancel test's business.)
    let (_, r) = call(&app, "GET", &format!("/api/runs/{remote_id}?output=0"), Value::Null).await;
    assert!(
        r["state"] == "canceled" || r["state"] == "succeeded" || r["state"] == "failed",
        "the canceled remote run is terminal: {r}"
    );
    assert!(r.get("endedAt").is_some(), "its finish was awaited: {r}");
    let (_, n) = call(&app, "GET", &format!("/api/runs/{nap_id}?output=0"), Value::Null).await;
    assert_eq!(
        n["state"], "running",
        "an unrelated run owned by the SAME owner stays alive",
    );
    let _ = system.services().runs.shutdown_all().await;
}
// --- I1: clear and the identity floors ------------------------------------------------------------------

#[tokio::test]
async fn clear_preserves_wire_numbering_and_internal_identity() {
    let (app, _system, history, logs, _fake) = wired().await;
    let a = exec_and_wait(&app, "dev", "true").await;
    let b = exec_and_wait(&app, "dev", "true").await;
    let _ = history.flush().await;
    let ids_before = history.internal_run_ids().await;
    assert_eq!(ids_before.len(), 2);
    // Clear through the route the panel uses.
    let (status, cleared) = call(&app, "DELETE", "/api/remote/runs", Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{cleared}");
    let (_, empty) = call(&app, "GET", "/api/remote/runs", Value::Null).await;
    assert_eq!(empty["runs"], json!([]));
    // The wire numbering continues (the coordinator never reuses an id) and the
    // internal identity continues too: fresh rows, fresh internal ids.
    let c = exec_and_wait(&app, "dev", "true").await;
    assert!(c > b, "the wire id continued past the clear: {c} > {b}");
    let _ = history.flush().await;
    let ids_after = history.internal_run_ids().await;
    assert_eq!(ids_after.len(), 1);
    assert!(
        ids_after[0] > ids_before.iter().copied().max().unwrap(),
        "internal identity never reuses a number"
    );
    let _ = a;
    let _ = logs;
    let _ = history.close().await;
}


// --- code-review C3: the page cursor and the open-failure cleanup -----------------------------------

#[tokio::test]
async fn paging_a_hundred_runs_loses_none_at_the_page_boundaries() {
    let home = swiss_core::paths::test_home().join("remote-page-cursor");
    let logs = home.join("logs");
    std::fs::create_dir_all(logs.join("remote")).unwrap();
    // A fresh database with no legacy tree: open imports nothing.
    let history = RemoteHistory::open(&logs).await.unwrap();
    for i in 1..=100u64 {
        let line = serde_json::json!({
            "runId": i,
            "target": "t",
            "actor": "a",
            "endedAtMs": 1_700_000_000_000u64 + i,
            "exit": 0,
        });
        history.record_run(&line, None, None).await.unwrap();
    }
    let mut seen: Vec<u64> = Vec::new();
    let mut before: Option<u64> = None;
    for _ in 0..20 {
        let (page, next) = history
            .page(&swiss_remote::history::PageQuery {
                before,
                since: None,
                until: None,
                target: None,
                actor: None,
                limit: 20,
            })
            .await;
        assert!(!page.is_empty(), "an empty page ends the walk instead");
        seen.extend(page.iter().filter_map(|v| v.get("runId")).filter_map(|v| v.as_u64()));
        match next {
            Some(n) => before = Some(n),
            None => break,
        }
    }
    assert_eq!(seen.len(), 100, "every run is served exactly once");
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 100, "no run id appears twice");
    assert_eq!(sorted[0], 1, "the oldest run is reachable");
    assert_eq!(sorted[99], 100, "the newest run is reachable");
    let _ = history.close().await;
}

#[tokio::test]
async fn a_failed_legacy_import_releases_the_store_it_opened() {
    let home = swiss_core::paths::test_home().join("remote-open-leak");
    let logs = home.join("logs");
    let remote = logs.join("remote");
    std::fs::create_dir_all(remote.join("out")).unwrap();
    // One legacy run whose output file exceeds the writer queue's 16 MiB byte bound:
    // the import's admission refuses it, the import fails, the open fails.
    std::fs::write(
        remote.join("runs.jsonl"),
        serde_json::to_string(&serde_json::json!({
            "runId": 9001, "target": "t", "actor": "a",
            "endedAtMs": 1_700_000_000_000u64, "exit": 0,
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();
    let big = vec![b'x'; 16 * 1024 * 1024 + 65_536];
    std::fs::write(remote.join("out").join("9001.txt"), &big).unwrap();
    let err = RemoteHistory::open(&logs).await.err().expect("the oversized import fails");
    assert!(err.contains("importing the legacy run log"), "{err}");
    // The store it opened must be WITHDRAWN, not leaked: on Windows a leaked pool
    // keeps the database file locked, and the next line would fail with a sharing
    // violation. A released store lets the file go.
    std::fs::remove_file(logs.join("remote.db")).expect("the failed open released the database file");
    std::fs::remove_file(logs.join("remote.db-wal")).or_else(|e| {
        assert!(!e.to_string().contains("being used by another process"), "wal released: {e}");
        Ok::<(), std::io::Error>(())
    })
    .ok();
}

/// run_output parts are SEALED at rest (SPEC host.history): the plaintext marker never
/// lands in remote.db, the rows carry the sealing key source label, and the stream
/// reads byte-exact after a close/reopen round-trip - the binding pins the run, part
/// and byte range, all of which survive a reopen.
#[tokio::test]
async fn run_output_parts_are_sealed_at_rest_and_read_back_intact() {
    assert!(
        !swiss_core::secure::key::no_key_available(),
        "this test asserts sealed mode: the machine must expose a master key source"
    );
    let (_app, _system, _opened, logs_root, _fake) = wired().await;
    let logs = logs_root.join("remote");
    std::fs::create_dir_all(logs.join("out")).unwrap();
    let marker = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let stream = marker.repeat(80);
    std::fs::write(logs.join("out").join("9007.txt"), stream.as_bytes()).unwrap();
    let line = json!({
        "runId": 9007, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
        "endedAtMs": 1770000000000u64, "target": "dev", "actor": "api",
        "input": { "target": "dev", "argv": ["emit"] },
    });
    {
        let history = RemoteHistory::open(logs.parent().unwrap()).await.unwrap();
        history.record_run(&line, Some(&logs.join("out").join("9007.txt")), None).await.unwrap();
        let _ = history.flush().await;
        let mut db = std::fs::read(logs_root.join("remote.db")).unwrap();
        if let Ok(wal) = std::fs::read(logs_root.join("remote.db-wal")) {
            db.extend_from_slice(&wal);
        }
        assert!(
            !db.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "the plaintext marker must not appear in the database file"
        );
        assert!(
            db.windows(b"history:remote:run_output".len()).any(|w| w == b"history:remote:run_output"),
            "sealed rows carry the sealing key source label"
        );
        let _ = history.close().await;
    }
    let history = RemoteHistory::open(&logs_root).await.unwrap();
    let read = history.output(9007, 0, 128 * 1024).await.unwrap();
    assert_eq!(read.text, stream, "the reopened store serves the sealed stream intact");
    let _ = history.close().await;
}

/// The hourly maintenance pass reaches the age sweep: a run past the retention age is
/// gone after maintain(), a fresh one stays (SPEC host.history — the sweep is wired,
/// not just implemented).
#[tokio::test]
async fn maintain_runs_the_age_sweep() {
    let (_app, _system, _opened, logs_root, _fake) = wired().await;
    let logs = logs_root.join("remote");
    std::fs::create_dir_all(logs.join("out")).unwrap();
    std::fs::write(logs.join("out").join("9100.txt"), b"old run output").unwrap();
    std::fs::write(logs.join("out").join("9101.txt"), b"fresh run output").unwrap();
    let line = |id: u64, ended: u64| {
        json!({
            "runId": id, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
            "endedAtMs": ended, "target": "dev", "actor": "api",
            "input": { "target": "dev", "argv": ["emit"] },
        })
    };
    let history = RemoteHistory::open(logs.parent().unwrap()).await.unwrap();
    history
        .record_run(&line(9100, 1_000), Some(&logs.join("out").join("9100.txt")), None)
        .await
        .unwrap();
    let now = swiss_core::util::now_ms();
    history
        .record_run(&line(9101, now), Some(&logs.join("out").join("9101.txt")), None)
        .await
        .unwrap();
    let _ = history.flush().await;
    history.maintain(7 * 24 * 60 * 60 * 1000).await;
    let _ = history.flush().await;
    let page = history.page(&swiss_remote::history::PageQuery::default()).await;
    let ids: Vec<u64> = page
        .0
        .iter()
        .filter_map(|r| r["runId"].as_u64())
        .collect();
    assert!(!ids.contains(&9100), "the aged run is gone after maintain");
    assert!(ids.contains(&9101), "the fresh run stays");
    let _ = history.close().await;
}

/// The database's own retention pass publishes the history notice when it removed
/// a line and stays silent when it removed nothing (SPEC §remote.history: the
/// hourly sweep must not wake the Runs page every hour it changes no row).
#[tokio::test]
async fn the_hourly_sweep_publishes_only_when_it_removed_a_line() {
    let (_app, _system, _opened, logs_root, _fake) = wired().await;
    let logs = logs_root.join("remote");
    std::fs::create_dir_all(logs.join("out")).unwrap();
    std::fs::write(logs.join("out").join("9200.txt"), b"old run output").unwrap();
    let line = |id: u64, ended: u64| {
        json!({
            "runId": id, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
            "endedAtMs": ended, "target": "dev", "actor": "api",
            "input": { "target": "dev", "argv": ["emit"] },
        })
    };
    let history = RemoteHistory::open(logs.parent().unwrap()).await.unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = count.clone();
    history.set_notice(Arc::new(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    history
        .record_run(&line(9200, 1_000), Some(&logs.join("out").join("9200.txt")), None)
        .await
        .unwrap();
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0, "a commit through the spill's finish path is its notice, not this one");
    history.sweep(7 * 24 * 60 * 60 * 1000).await;
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1, "the row the sweep removed moved the answer");
    history.sweep(7 * 24 * 60 * 60 * 1000).await;
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1, "a sweep that removed nothing stays silent");
    let _ = history.close().await;
}

/// A Clear whose spill ledger reads empty but whose attached database still holds
/// rows is a move all the same (SPEC §remote.history): spill and database keep their
/// own retention, so an empty spill does not mean an empty record - without this
/// notice another window never learns the record emptied. A second clear over
/// both-empty stays silent. The path is the real one: RunHistory::clear_records
/// over an attached database, never RemoteHistory::clear directly.
#[tokio::test]
async fn a_clear_publishes_when_only_the_database_held_rows() {
    let (_app, system, history, logs_root, _fake) = wired().await;
    let logs = logs_root.join("remote");
    std::fs::create_dir_all(logs.join("out")).unwrap();
    std::fs::write(logs.join("out").join("9300.txt"), b"db-only run output").unwrap();
    let line = json!({
        "runId": 9300, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
        "endedAtMs": 1_000, "target": "dev", "actor": "api",
        "input": { "target": "dev", "argv": ["emit"] },
    });
    // Straight into the database, bypassing the spill: the spill ledger stays (0, 0).
    history
        .record_run(&line, Some(&logs.join("out").join("9300.txt")), None)
        .await
        .unwrap();
    let _ = history.flush().await;
    // One binding: RemoteSystem::history() re-attaches the bus notice on every call,
    // so the counting closure is installed on a handle taken once and kept.
    let spill = system.history();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = count.clone();
    spill.set_notice(Arc::new(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    spill.clear_records().await;
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a clear that emptied only the database moved the answer"
    );
    spill.clear_records().await;
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a clear over both-empty stays silent"
    );
    let _ = history.close().await;
}


// --- round-1 review: the honest ack and the live cursor over the module store ----------------------

/// The sink's finish must not claim Recorded for a record it could not persist: with
/// the module database withdrawn, the run publishes historyAck=unavailable plus its
/// reason - a visible status, never false success (SPEC remote.history).
#[tokio::test]
async fn a_run_that_cannot_persist_publishes_an_unavailable_ack_never_recorded() {
    let (app, _system, history, _logs, _fake) = wired().await;
    // The healthy path first: an open store acknowledges its records.
    let healthy = exec_and_wait(&app, "true", "ok").await;
    let (_, view) = call(
        &app,
        "GET",
        &format!("/api/runs/{healthy}?output=0"),
        Value::Null,
    )
    .await;
    assert_ne!(
        view["meta"]["historyAck"], json!("unavailable"),
        "an open store must not deny its record: {view}"
    );
    // Withdraw the database mid-flight: the next finish hits a closed store.
    history.close().await.unwrap();
    let denied = exec_and_wait(&app, "true", "ok").await;
    let (_, view) = call(
        &app,
        "GET",
        &format!("/api/runs/{denied}?output=0"),
        Value::Null,
    )
    .await;
    assert_eq!(view["meta"]["historyAck"], json!("unavailable"), "{view}");
    let reason = view["meta"]["historyAckError"].as_str().unwrap_or("");
    assert!(!reason.is_empty(), "the denial carries its reason: {view}");
}

/// A clear bumps the remote store's generation: a scan cursor handed out before the
/// clear must expire by name instead of serving rows that no longer exist.
#[tokio::test]
async fn clearing_remote_history_expires_live_scan_cursors() {
    let (app, _system, history, _logs, fake) = wired().await;
    fake.program(
        "cursorfeed",
        swiss_remote::testing::FakeProgram {
            argv0: "cursorfeed".into(),
            stdout: format!("{}SECRET-a\n{}SECRET-b\n", "x".repeat(2_500), "y".repeat(2_500))
                .into_bytes(),
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 0,
        },
    );
    // The fake matches on the program (argv), the target is the connection name.
    for _ in 0..3 {
        exec_and_wait(&app, "dev", "cursorfeed").await;
    }
    // Page one stops mid-scan and hands back a cursor.
    let (_, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["remote"], "limit": 2 }),
    )
    .await;
    let token = page["next"]["remote"].as_str().expect("a live cursor").to_string();
    // The clear bumps the generation; the stale cursor must be refused.
    history.clear().await.unwrap();
    let (_, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["remote"], "limit": 2, "cursors": { "remote": token } }),
    )
    .await;
    let errors = page["errors"].as_array().cloned().unwrap_or_default();
    assert!(
        errors
            .iter()
            .any(|e| e["module"] == json!("remote") && e["reason"] == json!("cursor-expired")),
        "the cleared scan is refused by name: {page}"
    );
    let _ = history.close().await;
}


/// A hand-set cursor pointing INSIDE the leading multi-byte character must make
/// progress: the partial bytes are drained, the cursor advances past the whole
/// character, and the follower reads clean text from the boundary (the same
/// policy as the mcp body read; the round-3 audit found the lossy swallow).
#[tokio::test]
async fn a_cursor_inside_a_leading_multibyte_char_advances_to_the_boundary() {
    let (app, _system, history, logs, fake) = wired().await;
    let mut stdout = "\u{1F600}".as_bytes().to_vec(); // 4 bytes at the stream start
    stdout.extend_from_slice(b"pad TAIL-REMOTE");
    fake.program(
        "midchar",
        swiss_remote::testing::FakeProgram {
            argv0: "midchar".into(),
            stdout,
            stderr: Vec::new(),
            exit: 0,
            delay_ms: 0,
        },
    );
    let run = exec_and_wait(&app, "dev", "midchar").await;
    // Byte 1 is inside the 4-byte emoji: nothing servable, but the cursor must
    // move PAST the character - never stand still, never eat it lossy.
    let first = history.output(run, 1, 1024).await.unwrap();
    // The window must serve from the boundary (clean "pad ..."), never a
    // replacement character that swallows the real one lossy.
    assert!(
        first.text.starts_with("pad "),
        "the window serves from the character boundary: {:?}",
        first.text.chars().take(6).collect::<String>()
    );
    assert!(
        first.next_cursor > 4,
        "the cursor advances past the partial character"
    );
    let second = history.output(run, 4, 1024).await.unwrap();
    assert!(second.text.starts_with("pad "), "the follower reads from the boundary");
    let _ = logs;
    let _ = history.close().await;
}

// --- the spill's crash recovery --------------------------------------------------------------------

/// The spill is write-through AND crash-recovery (SPEC §remote.history): a run's line
/// and out file land on disk before the database commit, so a crash between the two
/// leaves a spill-only record - a window that exists for every run, not only the
/// migration's first day. Every open folds spill-only records back in, idempotently,
/// and what a sweep expired or a Clear wiped stays gone: recovery never fights
/// retention, and a record that crashed after the last sweep still recovers.
#[tokio::test]
async fn spill_only_records_recover_at_every_open_but_never_past_a_sweep() {
    let dir = std::env::temp_dir().join(format!("swiss-remote-spill-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("remote").join("out")).unwrap();
    let line = |id: u64, ended: u64| {
        json!({
            "runId": id, "action": "remote.exec", "state": "succeeded", "exitCode": 0,
            "endedAtMs": ended, "actor": "api",
            "meta": { "target": "test" }, "input": { "target": "test", "argv": ["true"] },
        })
    };
    let page_len = |history: &Arc<RemoteHistory>| {
        let history = history.clone();
        async move {
            history
                .page(&swiss_remote::history::PageQuery { target: Some("test"), limit: 20, ..Default::default() })
                .await
                .0
                .len()
        }
    };
    // One committed run through the ordinary path.
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        history.record_run(&line(9201, 1770000000001), None, None).await.unwrap();
        history.flush().await.unwrap();
        assert_eq!(page_len(&history).await, 1);
        let _ = history.close().await;
    }
    // A later run reaches only the spill: its line and out file exist, its row never
    // did. The next open must fold it in - the legacy marker says done, this is not
    // the migration.
    let index = dir.join("remote").join("runs.jsonl");
    let existing = std::fs::read_to_string(&index).unwrap_or_default();
    std::fs::write(&index, format!("{existing}{}\n", line(9202, 1770000000002))).unwrap();
    std::fs::write(dir.join("remote").join("out").join("9202.txt"), "ok\n").unwrap();
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        assert_eq!(page_len(&history).await, 2, "the spill-only record recovers at the next open");
        let _ = history.close().await;
    }
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        assert_eq!(page_len(&history).await, 2, "recovery is idempotent across opens");
        let _ = history.close().await;
    }
    // The sweep expires both (they ended months ago): the spill lines stay on disk,
    // and no later open may resurrect them.
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        history.sweep(0).await;
        assert_eq!(page_len(&history).await, 0, "the sweep expires both");
        let _ = history.close().await;
    }
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        assert_eq!(page_len(&history).await, 0, "expired records stay swept");
        let _ = history.close().await;
    }
    // A run that crashes AFTER that sweep (its ended time is newer than the stamp)
    // still recovers: the stamp fences the past, not the future.
    let existing = std::fs::read_to_string(&index).unwrap_or_default();
    let fresh_ended = swiss_core::util::now_ms() as u64;
    std::fs::write(&index, format!("{existing}{}\n", line(9204, fresh_ended))).unwrap();
    std::fs::write(dir.join("remote").join("out").join("9204.txt"), "fresh\n").unwrap();
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        assert_eq!(page_len(&history).await, 1, "a fresh spill-only record recovers past the stamp");
        // A Clear wipes the row while the spill line stays: the same fence holds.
        history.clear().await.unwrap();
        assert_eq!(page_len(&history).await, 0, "the clear wipes the row");
        // A normal maintenance pass has an OLDER cutoff than Clear. It must not
        // roll the recovery fence backwards while a surviving spill still exists.
        history.sweep(90 * 24 * 60 * 60 * 1000).await;
        assert_eq!(page_len(&history).await, 0);
        let _ = history.close().await;
    }
    {
        let history = RemoteHistory::open(&dir).await.unwrap();
        assert_eq!(page_len(&history).await, 0, "a cleared record stays cleared");
        let _ = history.close().await;
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
