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

//! `swiss memory sync` and the gateway calls behind it (SPEC §memory.cli): the one part of the
//! CLI that talks HTTP, because the queue and the worker live in the running gateway; the
//! map, docs and embedding commands wait on their runs through it too.

use super::*;

/// One admin API call against the running gateway: the sync command is the one
/// CLI surface that needs it, because the queue and the worker live there.
pub(super) async fn gateway_call(method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<(u16, Value), Fail> {
    gateway_call_within(method, path, body, GATEWAY_WAIT).await
}

/// [gateway_call], waiting `wait` for the answer: an ask is an agent's loop of turns, and
/// its own agent's timeout bounds it on the gateway.
pub(super) async fn gateway_call_within(method: reqwest::Method, path: &str, body: Option<&Value>, wait: Duration) -> Result<(u16, Value), Fail> {
    let port = crate::daemon::resolve_port();
    let url = format!("http://127.0.0.1:{port}{path}");
    let request = crate::daemon::with_cli_key(crate::daemon::loopback_client().request(method, &url).timeout(wait));
    let request = match body {
        Some(body) => request.json(body),
        None => request,
    };
    let response = request.send().await.map_err(|err| {
        if err.is_timeout() {
            Fail::Io(format!("the gateway on port {port} did not answer within {} s", wait.as_secs()))
        } else {
            Fail::Io(format!("the gateway on port {port} cannot be reached; is swiss running? (sync and init --map/--full need it)"))
        }
    })?;
    let status = response.status().as_u16();
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| Fail::Io("the gateway's answer is not JSON".into()))?;
    Ok((status, value))
}

/// The gateway's error message, whatever shape it answered.
pub(super) fn gateway_message(value: &Value, status: u16) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("the gateway answered {status}"))
}

/// The project the cwd belongs to: a sync needs a registered checkout.
pub(super) async fn sync_project_id(a: &Args, s: &MemoryService) -> Result<String, Fail> {
    let target = a.target()?;
    let project = s
        .resolve(&target)
        .await?
        .ok_or_else(|| Fail::Io("this directory is not a registered project yet; the first save registers it".into()))?;
    Ok(project.id)
}

pub(super) async fn sync_cli(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    if a.has("nudge") {
        return sync_nudge(a, out).await;
    }
    match a.positional.first().map(String::as_str) {
        None => sync_now(a, s, out).await,
        Some("status") => sync_status_cmd(a, s, out).await,
        Some("cancel") => sync_cancel_cmd(a, s, out).await,
        Some("hooks") => sync_hooks(a, s).await,
        Some(other) => Err(usage(format!("unknown sync subcommand {other:?}; {SYNC_USAGE}"))),
    }
}

/// Queue a manual run now; `--wait` follows it to its end (exit 1 on failure).
async fn sync_now(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "dry-run", "since", "agent", "wait", "json"], 0..=0)?;
    let project = sync_project_id(a, s).await?;
    let mut body = json!({ "project": project });
    picks(a, &mut body)?;
    if a.has("dry-run") {
        body["dryRun"] = json!(true);
    }
    if let Some(since) = a.value("since") {
        body["since"] = json!(since);
    }
    let (status, value) = gateway_call(reqwest::Method::POST, "/api/knowledge/sync", Some(&body)).await?;
    if status != 202 {
        return Err(Fail::Io(gateway_message(&value, status)));
    }
    let run = value.get("run").cloned().unwrap_or(Value::Null);
    let id = run.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
    if a.json() {
        // The receipt first, then - with --wait - the run's final JSON: --json used to
        // return here before the wait, so `sync --wait --json` never followed the run
        // (found live: the queued receipt was printed and the CLI exited 0 at once).
        out.line(&run.to_string());
        if a.has("wait") {
            let project = body.get("project").and_then(Value::as_str).unwrap_or_default().to_string();
            return wait_run(&project, &id, a.json(), out).await;
        }
        return Ok(());
    }
    if value.get("merged").and_then(Value::as_bool) == Some(true) {
        out.line("a run is already queued; merged into it");
    } else {
        out.line(&format!("queued run {id}"));
    }
    if a.has("wait") {
        let project = body.get("project").and_then(Value::as_str).unwrap_or_default().to_string();
        return wait_run(&project, &id, false, out).await;
    }
    Ok(())
}

/// Follow one run, a poll a second, until it lands; a failed run exits 1. `json` prints
/// the landing run as JSON (the receipt above already went out) instead of the text line.
pub(super) async fn wait_run(project: &str, id: &str, json: bool, out: &dyn Out) -> Run {
    let run = wait_for(project, id).await?;
    say_run(&run, id, json, out);
    Ok(())
}

/// Follow a queued run to its end: the finished run, or why it failed.
pub(super) async fn wait_for(project: &str, id: &str) -> Result<Value, Fail> {
    let path = format!("/api/knowledge/sync/runs/{id}?project={project}");
    for _ in 0..3600 {
        let (status, value) = gateway_call(reqwest::Method::GET, &path, None).await?;
        if status != 200 {
            return Err(Fail::Io(gateway_message(&value, status)));
        }
        let run = value.get("run").cloned().unwrap_or(Value::Null);
        match run.get("state").and_then(Value::as_str).unwrap_or_default() {
            "queued" | "running" => {}
            "ok" | "empty" => return Ok(run),
            "cancelled" => {
                let summary = run.get("summary").and_then(Value::as_str).unwrap_or_default();
                return Err(Fail::Io(format!("run {id} cancelled: {summary}")));
            }
            other => {
                let error = run.get("error").and_then(Value::as_str).unwrap_or_default();
                return Err(Fail::Io(format!("run {id} {other}: {error}")));
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(Fail::Io(format!("run {id} did not finish within an hour; it keeps going without the CLI")))
}

/// What a finished run says: its JSON, or its state and summary.
pub(super) fn say_run(run: &Value, id: &str, json: bool, out: &dyn Out) {
    if json {
        out.line(&run.to_string());
        return;
    }
    if run.get("state").and_then(Value::as_str) == Some("empty") {
        let summary = run.get("summary").and_then(Value::as_str).filter(|s| !s.is_empty());
        let why = match run.get("trigger").and_then(Value::as_str) {
            Some("recheck") => "no note waits for a recheck",
            Some("review") => summary.unwrap_or("the review inbox holds no note a model wrote"),
            Some("docs") => summary.unwrap_or("no document changed, and every one has a card"),
            _ => "nothing new since the last sync",
        };
        out.line(&format!("run {id}: empty ({why})"));
        return;
    }
    match run.get("summary").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(summary) => out.line(&format!("run {id}: ok - {summary}")),
        None => out.line(&format!("run {id}: ok")),
    }
}

/// Where the project stands: the pending commits, the unread conversations, the runs.
async fn sync_status_cmd(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    // The word "status" is the first positional; it routed the dispatch and rides along.
    a.allow(&["cwd", "json"], 1..=1)?;
    let project = sync_project_id(a, s).await?;
    let (status, value) = gateway_call(
        reqwest::Method::GET,
        &format!("/api/knowledge/sync?project={project}"),
        None,
    )
    .await?;
    if status != 200 {
        return Err(Fail::Io(gateway_message(&value, status)));
    }
    if a.json() {
        out.line(&value.to_string());
        return Ok(());
    }
    let head = value.pointer("/state/head").and_then(Value::as_str).unwrap_or("none yet");
    let commits = value.pointer("/state/pendingCommits").and_then(Value::as_u64).unwrap_or(0);
    let bytes = value.pointer("/state/pendingBytes").and_then(Value::as_u64).unwrap_or(0);
    let agent = value.pointer("/agent/name").and_then(Value::as_str).unwrap_or("off");
    let runtime = value.pointer("/agent/runtime").and_then(Value::as_str);
    match runtime {
        Some(runtime) => out.line(&format!("agent {agent} ({runtime}), head {head}")),
        None => out.line(&format!("agent {agent}, head {head}")),
    }
    if let Some(error) = value.pointer("/agent/error").and_then(Value::as_str) {
        out.line(&format!("agent error: {error}"));
    }
    out.line(&format!("pending: {commits} commit(s), {bytes} unread conversation byte(s)"));
    let runs = value.get("runs").and_then(Value::as_array).cloned().unwrap_or_default();
    for run in runs.iter().take(5) {
        let (id, state) = (
            run.get("id").and_then(Value::as_str).unwrap_or("?"),
            run.get("state").and_then(Value::as_str).unwrap_or("?"),
        );
        let summary = run.get("summary").and_then(Value::as_str).unwrap_or_default();
        match progress_line(run) {
            Some(progress) if state == "running" => out.line(&format!("run {id}: running - {progress}")),
            _ => out.line(&format!("run {id}: {state} {summary}")),
        }
    }
    Ok(())
}

/// Where a running map, full, recheck or docs run is: its phase, the areas described, the
/// slices answered and the notes saved so far, the notes rechecked, the documents carded,
/// and how long it has run.
fn progress_line(run: &Value) -> Option<String> {
    let scope = run.get("scope")?;
    let phase = scope.get("phase").and_then(Value::as_str)?;
    let mut parts = vec![format!("phase {phase}")];
    let asks = scope.get("asks").and_then(Value::as_u64).unwrap_or(0);
    if asks > 0 {
        let filled = scope.get("filled").and_then(Value::as_u64).unwrap_or(0);
        parts.push(format!("{filled}/{asks} area(s) described"));
    }
    if let Some(k) = scope.get("knowledge") {
        let n = |key: &str| k.get(key).and_then(Value::as_u64).unwrap_or(0);
        let left = n("slices").saturating_sub(n("unchanged") + n("more"));
        parts.push(format!("{}/{left} slice(s) answered, {} note(s) saved", n("asked"), n("saved")));
        if n("failed") > 0 {
            parts.push(format!("{} failed", n("failed")));
        }
    }
    if let Some(w) = scope.get("wrapup") {
        let n = |key: &str| w.get(key).and_then(Value::as_u64).unwrap_or(0);
        parts.push(format!("{}/{} domain(s) wrapped up", n("wrapped"), n("domains")));
    }
    if let Some(c) = scope.get("recheck") {
        let n = |key: &str| c.get(key).and_then(Value::as_u64).unwrap_or(0);
        let judged = n("kept") + n("fixed") + n("dropped") + n("merged") + n("held");
        parts.push(format!("{}/{} recheck question(s) answered, {judged}/{} note(s) judged", n("asked"), n("questions"), n("notes")));
        if run.get("trigger").and_then(Value::as_str) == Some("review") {
            parts.push(format!("{} confirmed, {} contradiction(s) lifted", n("confirmed"), n("lifted")));
        } else if n("lifted") > 0 {
            parts.push(format!("{} contradiction(s) lifted", n("lifted")));
        }
        if n("failed") > 0 {
            parts.push(format!("{} failed", n("failed")));
        }
    }
    if let Some(d) = scope.get("docs") {
        let n = |key: &str| d.get(key).and_then(Value::as_u64).unwrap_or(0);
        parts.push(format!("{}/{} card question(s) answered, {} document(s) carded", n("asked"), n("questions"), n("carded")));
        if n("failed") > 0 {
            parts.push(format!("{} failed", n("failed")));
        }
    }
    if let Some(started) = run.get("startedMs").and_then(Value::as_i64) {
        let secs = (swiss_core::util::now_ms() as i64 - started).max(0) / 1000;
        parts.push(format!("{}m{:02}s", secs / 60, secs % 60));
    }
    Some(parts.join(", "))
}

/// `sync cancel ID`: a queued run is cancelled at once; a running one is told to stop
/// and ends cancelled between its questions, keeping the notes it saved.
async fn sync_cancel_cmd(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    // Positionals are ["cancel", ID]: the word that routed the dispatch rides along.
    a.allow(&["cwd", "json"], 2..=2)?;
    let project = sync_project_id(a, s).await?;
    let id = a.pos(1);
    let path = format!("/api/knowledge/sync/runs/{id}/cancel?project={project}");
    let (status, value) = gateway_call(reqwest::Method::POST, &path, None).await?;
    if status != 200 {
        return Err(Fail::Io(gateway_message(&value, status)));
    }
    if a.json() {
        out.line(&value.to_string());
    } else if value.get("stopping").and_then(Value::as_bool) == Some(true) {
        out.line(&format!("run {id} is stopping: it ends cancelled after the question in hand, keeping what it saved"));
    } else {
        out.line(&format!("run {id} cancelled before it started"));
    }
    Ok(())
}

/// `sync --nudge`: a Git hook's call. Exit 0 always, two seconds at most, the cwd
/// from the stdin JSON (Git's and Claude Code's hook input), nothing to stdout -
/// and never a nudge from inside a sync run (the env guard).
async fn sync_nudge(a: &Args, out: &dyn Out) -> Run {
    a.allow(&["cwd", "nudge"], 0..=0)?;
    // A sync run's own git calls must not nudge itself into another run.
    if std::env::var_os("SWISS_MEMORY_SYNC").is_some() {
        return Ok(());
    }
    let cwd = match a.value("cwd") {
        Some(cwd) => cwd.to_string(),
        None => {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text);
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("cwd").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default()
        }
    };
    if cwd.is_empty() {
        return Ok(());
    }
    let (status, value) = gateway_call(
        reqwest::Method::POST,
        "/api/knowledge/sync/nudge",
        Some(&json!({ "cwd": cwd })),
    )
    .await?;
    if status != 200 {
        out.err(&format!("swiss memory sync --nudge: {}", gateway_message(&value, status)));
    }
    Ok(())
}

/// `sync hooks install|uninstall`: the marked block in post-merge and pre-push.
async fn sync_hooks(a: &Args, s: &MemoryService) -> Run {
    // Positionals are ["hooks", "install"|"uninstall"]: the word that routed the dispatch rides along.
    a.allow(&["cwd"], 2..=2)?;
    match a.pos(1) {
        "install" => {}
        "uninstall" => {}
        other => return Err(usage(format!("usage: swiss memory sync hooks install|uninstall (got {other:?})"))),
    }
    let target = a.target()?;
    let project = s
        .resolve(&target)
        .await?
        .ok_or_else(|| Fail::Io("this directory is not a registered project yet; the first save registers it".into()))?;
    let root = project
        .roots
        .first()
        .map(|r| std::path::PathBuf::from(&r.path))
        .ok_or_else(|| Fail::Io("the project has no root checkout bound".into()))?;
    let install = a.pos(0) == "install";
    if install {
        let exe = std::env::current_exe().map_err(|err| Fail::Io(format!("this binary's path cannot be read: {err}")))?;
        let home = swiss_core::paths::data_dir();
        let port = crate::daemon::resolve_port();
        let touched = swiss_plugins::memory_sync::hooks_install(&root, &exe, &home.display().to_string(), port)
            .map_err(Fail::Io)?;
        for path in touched {
            println!("installed {path}");
        }
    } else {
        let touched = swiss_plugins::memory_sync::hooks_uninstall(&root).map_err(Fail::Io)?;
        if touched.is_empty() {
            println!("no hook carried the block");
        }
        for path in touched {
            println!("cleaned {path}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Capture(Mutex<Vec<String>>);

    impl Out for Capture {
        fn line(&self, text: &str) {
            self.0.lock().expect("capture").push(format!("out:{text}"));
        }

        fn err(&self, text: &str) {
            self.0.lock().expect("capture").push(format!("err:{text}"));
        }
    }

    impl Capture {
        fn text(&self) -> String {
            self.0.lock().expect("capture").join("\n")
        }
    }

    /// The scratch service has no project registered, so a sync that needs one fails
    /// BEFORE any gateway call - deterministic, no port involved.
    async fn run(argv: &[&str]) -> (i32, String) {
        let home = std::env::temp_dir().join(format!("swiss-memsync-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&home).expect("scratch");
        let service = MemoryService::at(&home.join("store.sqlite3"));
        let out = Capture(Mutex::new(Vec::new()));
        let exit = run_with(argv.iter().map(|s| s.to_string()).collect(), &out, &service).await;
        let _ = std::fs::remove_dir_all(&home);
        (exit, out.text())
    }

    #[test]
    fn a_running_runs_status_line_says_its_phase_and_counts() {
        let run = json!({
            "state": "running",
            "scope": { "phase": "knowledge", "asks": 12, "filled": 12,
                       "knowledge": { "slices": 691, "unchanged": 136, "more": 0, "asked": 40, "saved": 97, "failed": 2 } }
        });
        let line = progress_line(&run).expect("a phase");
        assert_eq!(line, "phase knowledge, 12/12 area(s) described, 40/555 slice(s) answered, 97 note(s) saved, 2 failed");
        assert_eq!(progress_line(&json!({ "state": "running", "scope": {} })), None, "a plain sync has no phase");
        let wrapping = json!({
            "state": "running",
            "scope": { "phase": "wrapup", "knowledge": { "slices": 4, "asked": 4, "saved": 9 },
                       "wrapup": { "domains": 3, "wrapped": 1 } }
        });
        let line = progress_line(&wrapping).expect("a phase");
        assert_eq!(line, "phase wrapup, 4/4 slice(s) answered, 9 note(s) saved, 1/3 domain(s) wrapped up");
        let rechecking = json!({
            "state": "running",
            "scope": { "phase": "recheck",
                       "recheck": { "notes": 45, "questions": 3, "asked": 1, "kept": 12, "fixed": 3, "dropped": 2, "merged": 1, "held": 1, "failed": 1 } }
        });
        let line = progress_line(&rechecking).expect("a phase");
        assert_eq!(line, "phase recheck, 1/3 recheck question(s) answered, 19/45 note(s) judged, 1 failed");
    }

    #[tokio::test]
    async fn the_sync_usage_errors_name_their_subcommand() {
        let (exit, text) = run(&["sync", "fly"]).await;
        assert_eq!(exit, 2, "a usage error");
        assert!(text.contains("unknown sync subcommand"), "{text}");
        let (exit, text) = run(&["sync", "hooks", "nope"]).await;
        assert_eq!(exit, 2, "a usage error");
        assert!(text.contains("usage"), "{text}");
    }

    #[tokio::test]
    async fn a_sync_on_an_unregistered_directory_names_the_problem() {
        let (exit, text) = run(&["sync"]).await;
        assert_eq!(exit, 1);
        assert!(text.contains("not a registered project"), "{text}");
    }

    #[tokio::test]
    async fn the_status_and_hooks_subcommands_reach_their_handler() {
        // The subcommand word is a positional: a wrong allow() count once refused
        // `sync status` as a usage error before any work happened (found live on 19998).
        let (exit, text) = run(&["sync", "status"]).await;
        assert_eq!(exit, 1, "the scratch store has no project - not a usage error");
        assert!(text.contains("not a registered project"), "{text}");
        let (exit, text) = run(&["sync", "cancel", "r-00000000"]).await;
        assert_eq!(exit, 1, "ditto, past the parser");
        assert!(text.contains("not a registered project"), "{text}");
        let (exit, text) = run(&["sync", "cancel"]).await;
        assert_eq!(exit, 2, "cancel names its run");
        assert!(text.contains("usage"), "{text}");
        let (exit, text) = run(&["sync", "hooks", "install"]).await;
        assert_eq!(exit, 1, "ditto, past the parser");
        assert!(text.contains("not a registered project"), "{text}");
        assert!(!text.contains("usage"), "{text}");
    }
}
