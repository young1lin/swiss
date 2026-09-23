//! L3: the proc adapter against the repo's OWN stdio MCP server (docs/44 SS2.7).
//!
//! The child is a real process (CARGO_BIN_EXE_it-mcp-server, cargo-guaranteed for a
//! same-package bin), the gateway is the real listener from L2, and the client is
//! the same rmcp session. What this group pins:
//!
//! - laziness end to end: no child at boot, one after the first request, none after
//!   the idle reap - and the NEXT request wakes a fresh one;
//! - a stop kills the child tree (Job Object on Windows, process group on unix);
//! - the five tools: UTF-8 echo, byte-identical blob over the &RawValue proxy path,
//!   a real sleep, an MCP error that does not kill the process, and the env NAMES
//!   the child sees (the daemon's launcher-noise scrub, docs/16 H1, proven on a proc
//!   child for the first time);
//! - the call log the Logs tab reads (docs/33 C3): replies stored verbatim, a long one
//!   clipped on the page and served whole by seq.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::gateway::{boot, call, call_text, mcp, tool_names};

/// Child presence is asserted by exe name across the whole process table, so two
/// L3 suites waking children at once would read each other's. One lock, held for
/// the whole test body - each of them is seconds.
static L3: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(windows)]
const EXE: &str = "it-mcp-server.exe";
#[cfg(not(windows))]
const EXE: &str = "it-mcp-server";

/// How many processes named `EXE` are alive right now - the spec's "process
/// table" route as a DIRECT API call: Toolhelp32 on Windows, /proc elsewhere.
/// No tasklist subprocess (the repo rule that bought the Rust port in the first
/// place). Failure reads as a huge count, never zero: "cannot tell" must not be
/// able to fake "gone".
fn count_children() -> usize {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return usize::MAX;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut seen = 0usize;
            let mut ok = Process32FirstW(snap, &mut entry);
            while ok != 0 {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                if name.eq_ignore_ascii_case(EXE) {
                    seen += 1;
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
            seen
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::read_dir("/proc")
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit())
                    })
                    .filter(|e| {
                        std::fs::read_to_string(e.path().join("cmdline"))
                            .map(|cmd| cmd.contains(EXE))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(usize::MAX)
    }
}

/// A proc def pointing at the built server. `idle_ms` rides the def the way the
/// panel would write it; 0 opts out of the reap entirely.
fn server_def(idle_ms: u64) -> Value {
    json!({
        "type": "proc",
        "command": env!("CARGO_BIN_EXE_it-mcp-server"),
        "args": [],
        "idleMs": idle_ms,
    })
}

async fn stop(g: &crate::gateway::Gateway, name: &str) {
    let (status, body) = g
        .api(
            reqwest::Method::POST,
            &format!("/api/mcps/{name}/stop"),
            None,
        )
        .await;
    assert_eq!(status, 200, "the panel's own stop route: {body}");
}

/// Poll until no child is left, or fail with what stayed alive.
async fn await_no_child(within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let n = count_children();
        if n == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{n} child(ren) still alive after {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn lazy_until_the_first_request_then_reaped_then_rewoken() {
    let _guard = L3.lock().await;
    let g = boot(vec![("l3-lazy", server_def(1_500))]).await;

    // Registered, enabled, and the boot loop deliberately did NOT start it: a lazy
    // proc's child is the one thing here that costs real memory.
    assert_eq!(count_children(), 0, "no child before the first request");

    // The waking request - list_tools through the real HTTP route, the way a
    // hosted client's session initialize/tools-list would.
    let c = mcp(&g, "l3-lazy").await;
    let names = tool_names(&c).await;
    assert_eq!(
        names,
        vec!["blob", "echo", "env", "fail", "sleep"],
        "the server's five tools, and the exact set the proxy forwards"
    );
    assert_eq!(count_children(), 1, "the wake spawned the child");

    // No traffic for 1.5 s -> the reaper takes the child. The startup line this
    // child wrote to stderr never touched the protocol stream: the session above
    // worked, and the calls below still do.
    await_no_child(Duration::from_secs(8)).await;

    // The resting state of a lazy entry is idle, not off: the next call wakes a
    // fresh child and answers.
    let echo = call(&c, "echo", json!({ "text": "re-woken" })).await;
    assert!(echo.to_string().contains("re-woken"), "{echo}");
    assert_eq!(count_children(), 1, "the re-wake spawned a fresh child");

    // Leave the table clean for the next suite (serialized, but same process).
    stop(&g, "l3-lazy").await;
    await_no_child(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn stopping_the_mcp_kills_the_child_tree() {
    let _guard = L3.lock().await;
    // idleMs 0: no reap - only the stop may take the child, which is the point.
    let g = boot(vec![("l3-kill", server_def(0))]).await;
    let c = mcp(&g, "l3-kill").await;
    let _ = tool_names(&c).await;
    assert_eq!(count_children(), 1);

    // The product's tree kill: Windows Job Object, unix process group - cfg-divided
    // code paths, each exercised on the OS that runs this suite.
    stop(&g, "l3-kill").await;
    await_no_child(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn echo_round_trips_utf8_and_stderr_never_pollutes_stdout() {
    let _guard = L3.lock().await;
    let g = boot(vec![("l3-echo", server_def(0))]).await;
    let c = mcp(&g, "l3-echo").await;
    let _ = tool_names(&c).await; // wake (and prove the startup stderr line was harmless)

    // The bytes that worry ports: CJK, emoji, typographic quotes, Ñ.
    let text = "中文 🙂 “quoted” Æñ";
    let back = call(&c, "echo", json!({ "text": text })).await;
    assert_eq!(back.as_str(), Some(text), "UTF-8 over the whole stdio path: {back}");

    stop(&g, "l3-echo").await;
    await_no_child(Duration::from_secs(2)).await;
}

/// docs/33 C3: the Logs JSON view's contract with the call log. The panel decodes a reply that is
/// JSON held in a string and hands the stored text back on Copy raw, so the log must keep a reply
/// VERBATIM - never re-encoded or unwrapped on the way in. And it fetches a clipped reply whole
/// the moment its row opens, so the page must mark a reply past the 2 KB preview (`preview`, the
/// full `chars`) and `calls/{seq}` must serve every byte of it.
#[tokio::test]
async fn the_call_log_keeps_replies_verbatim_for_the_logs_view() {
    let _guard = L3.lock().await;
    let g = boot(vec![("l3-logs", server_def(0))]).await;
    let c = mcp(&g, "l3-logs").await;
    let _ = tool_names(&c).await;

    // web_search_prime's shape: the whole reply is one JSON string literal holding JSON.
    let inner = json!([{ "title": "a", "content": "line one\nline two" }]);
    let wrapped = serde_json::to_string(&inner.to_string()).expect("a JSON string literal");
    assert_eq!(call_text(&c, "echo", json!({ "text": wrapped })).await, wrapped);
    // A reply past the page's 2 KB preview (calls.rs PREVIEW_MAX).
    let rows: Vec<Value> = (0..120).map(|i| json!({ "id": i, "name": format!("row-{i}") })).collect();
    let long = json!({ "rows": rows }).to_string();
    assert!(long.chars().count() > 2048, "the fixture must outgrow the preview");
    assert_eq!(call_text(&c, "echo", json!({ "text": long })).await, long);

    let (status, page) = g
        .api(reqwest::Method::GET, "/api/mcps/l3-logs/calls", None)
        .await;
    assert_eq!(status, 200, "{page}");
    let calls = page["calls"].as_array().expect("a calls page");
    assert_eq!(calls.len(), 2, "both calls logged, newest first: {page}");
    let (long_row, wrapped_row) = (&calls[0], &calls[1]);

    // Stored verbatim: Copy raw is the wire text, and the arguments still parse to what was sent.
    assert_eq!(wrapped_row["output"].as_str(), Some(wrapped.as_str()));
    assert!(wrapped_row["preview"].is_null(), "a short reply is whole on the page");
    let args: Value = serde_json::from_str(wrapped_row["args"].as_str().expect("args text"))
        .expect("the stored arguments are JSON");
    assert_eq!(args["text"].as_str(), Some(wrapped.as_str()));

    // Clipped on the page, whole on calls/{seq}: what opening the row fetches.
    assert_eq!(long_row["preview"], json!(true));
    assert_eq!(long_row["chars"].as_u64(), Some(long.chars().count() as u64));
    let head: String = long.chars().take(2048).collect();
    assert_eq!(long_row["output"].as_str(), Some(head.as_str()));
    let seq = long_row["seq"].as_u64().expect("a seq");
    let (status, one) = g
        .api(reqwest::Method::GET, &format!("/api/mcps/l3-logs/calls/{seq}"), None)
        .await;
    assert_eq!(status, 200, "{one}");
    assert!(one["call"]["bodyGone"].is_null(), "the newest body is kept: {one}");
    assert_eq!(one["call"]["output"].as_str(), Some(long.as_str()));

    stop(&g, "l3-logs").await;
    await_no_child(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn blob_bytes_survive_the_rawvalue_proxy_path() {
    let _guard = L3.lock().await;
    let g = boot(vec![("l3-blob", server_def(0))]).await;
    let c = mcp(&g, "l3-blob").await;
    let _ = tool_names(&c).await;

    // The proc path forwards payloads as &RawValue: the gateway must hand back the
    // SAME BYTES the server wrote. Rebuild the document locally (the builder is
    // deterministic) and demand string - i.e. byte - equality, not JSON equality.
    let kb = 64usize;
    let head = format!("{{\"kb\":{kb},\"blob\":\"");
    let tail = "\"}";
    let middle = kb * 1024 - head.len() - tail.len();
    let filler = "swiss-it-l3-fill.";
    let mut expected = head.clone();
    let mut left = middle;
    while left > 0 {
        let take = left.min(filler.len());
        expected.push_str(&filler[..take]);
        left -= take;
    }
    expected.push_str(tail);
    assert_eq!(expected.len(), kb * 1024);

    let got = call_text(&c, "blob", json!({ "kb": kb })).await;
    assert!(!got.is_empty(), "the blob rides as text content");
    assert_eq!(got.len(), kb * 1024, "the full document arrived: {} bytes", got.len());
    assert_eq!(got, expected.as_str(), "the SAME bytes, not equal JSON");

    stop(&g, "l3-blob").await;
    await_no_child(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn sleep_really_sleeps() {
    let _guard = L3.lock().await;
    let g = boot(vec![("l3-sleep", server_def(0))]).await;
    let c = mcp(&g, "l3-sleep").await;
    let _ = tool_names(&c).await;

    let started = Instant::now();
    let back = call(&c, "sleep", json!({ "ms": 250 })).await;
    assert!(started.elapsed() >= Duration::from_millis(250), "a real pause");
    assert_eq!(back.as_str(), Some("slept 250ms"), "{back}");

    stop(&g, "l3-sleep").await;
    await_no_child(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn fail_maps_to_an_mcp_error_and_the_process_survives() {
    let _guard = L3.lock().await;
    let g = boot(vec![("l3-fail", server_def(0))]).await;
    let c = mcp(&g, "l3-fail").await;
    let _ = tool_names(&c).await;

    // The deliberate failure: an MCP error carrying our message, not a dead child.
    let mut params = rmcp::model::CallToolRequestParams::default();
    params.name = "fail".into();
    params.arguments = Some(json!({ "code": 7 }).as_object().cloned().expect("object"));
    let err = c
        .call_tool(params)
        .await
        .expect_err("fail answers with an MCP error");
    assert!(
        err.to_string().contains("it-l3-fail-7"),
        "the error carries the tool's message: {err}"
    );
    assert_eq!(count_children(), 1, "the process survived the error");

    // And it still serves afterwards - errors must not poison the child.
    let echo = call(&c, "echo", json!({ "text": "alive" })).await;
    assert!(echo.to_string().contains("alive"), "{echo}");

    stop(&g, "l3-fail").await;
    await_no_child(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn env_names_show_the_daemon_scrub_reached_the_child() {
    let _guard = L3.lock().await;
    // The harness plays the daemon (docs/44 SS2.6): scrub OUR environment the way
    // swiss's main() does at startup, with launcher noise planted first so the
    // scrub has something to bite on. The proc child inherits from this process.
    // SAFETY: env-planting races parallel tests exactly like every other
    // env-planting test in this workspace - none of them reads these names.
    unsafe {
        std::env::set_var("NO_COLOR", "1");
        std::env::set_var("CI", "1");
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_IT_L3", "1");
        std::env::set_var("SWISS_IT_L3_MARKER", "present");
        swiss_core::env::scrub_process_env();
    }
    let g = boot(vec![("l3-env", server_def(0))]).await;
    let c = mcp(&g, "l3-env").await;
    let _ = tool_names(&c).await;

    let env = call(&c, "env", json!({})).await;
    let mut names = env.as_str().expect("env answers with names").lines();
    for noise in ["NO_COLOR", "CI", "CLAUDECODE", "CLAUDE_CODE_IT_L3"] {
        assert!(
            !names.clone().any(|n| n.eq_ignore_ascii_case(noise)),
            "launcher noise {noise} must not reach a proc child: {env}"
        );
    }
    assert!(
        names.any(|n| n == "SWISS_IT_L3_MARKER"),
        "inheritance itself stays intact: {env}"
    );

    stop(&g, "l3-env").await;
    await_no_child(Duration::from_secs(2)).await;
}
