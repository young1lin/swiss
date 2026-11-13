// Copyright 2026 young1lin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `swiss agent run` - one task for a builtin agent, with nobody at the panel (SPEC §llm.cli).
//!
//! The task goes to the gateway's run stream (§llm.api `agents/run/stream`). The agent reads
//! in the working directory and writes only in the `--write` directories. Each step it takes
//! is one line on stderr, and its closing answer goes to stdout whole (`--json`: the outcome
//! as one JSON line). The run is in History, live while it works.
//!
//! Ctrl+C asks the gateway to stop the run and waits for its last word; a second one leaves
//! at once. A dropped connection stops it too, so a script that kills this process stops the
//! run with it.

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::time::Duration;

use serde_json::{json, Value};

use crate::daemon::{resolve_port, with_cli_key};

const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_INTERRUPTED: i32 = 130;
/// How long the gateway may take to open the stream: it checks the request and reserves
/// the run row first.
const OPEN_WITHIN: Duration = Duration::from_secs(30);
/// After Ctrl+C, how long to wait for the run's closing frame.
const STOP_WITHIN: Duration = Duration::from_secs(30);
/// A tool's argument as a step line shows it.
const ARG_SHOWN: usize = 100;
/// A failed tool's answer as a step line shows it.
const ERROR_SHOWN: usize = 200;

#[derive(Debug, PartialEq, Default)]
struct RunArgs {
    agent: Option<String>,
    cwd: Option<String>,
    write: Vec<String>,
    tools: Option<Vec<String>>,
    label: Option<String>,
    prompt: Prompt,
    json: bool,
    quiet: bool,
}

#[derive(Debug, PartialEq, Default)]
enum Prompt {
    /// Piped in on stdin.
    #[default]
    Stdin,
    Text(String),
    File(String),
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Run(RunArgs),
    Help,
}

fn usage() -> String {
    [
        "usage:",
        "  swiss agent run [--agent NAME] [--cwd DIR] [--write DIR]... [--tools a,b]",
        "                  [--label TEXT] [--prompt TEXT | --prompt-file FILE|-] [--json] [--quiet]",
        "      one task for a builtin agent, with nobody at the panel. It reads in the working",
        "      directory (--cwd, else this one) and writes only in the --write directories",
        "      (relative to it; nothing dot-named below them); without --write it changes",
        "      nothing. Tools: read, write, edit, glob, grep - no shell. The task comes from",
        "      --prompt, --prompt-file, or stdin. Without --agent, the agent added last runs.",
        "      Steps go to stderr, the answer to stdout (--json: the outcome as one JSON line).",
        "      Exit 0 done, 1 failed or cut short, 2 refused, 130 stopped.",
    ]
    .join("\n")
}

/// Strict, the way cli.rs parses: a word this does not know is refused, never guessed.
fn parse(argv: &[String]) -> Result<Cmd, String> {
    match argv.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => Ok(Cmd::Help),
        Some("run") => parse_run(&argv[1..]).map(Cmd::Run),
        Some(other) => Err(format!("unknown agent command: {other}")),
    }
}

fn parse_run(argv: &[String]) -> Result<RunArgs, String> {
    let mut args = RunArgs::default();
    let mut prompt_said = false;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            match inline.clone() {
                Some(value) => Ok(value),
                None => {
                    i += 1;
                    argv.get(i).cloned().ok_or_else(|| format!("{flag} needs {what}"))
                }
            }
        };
        match flag {
            "--agent" => args.agent = Some(value("an agent name")?),
            "--cwd" => args.cwd = Some(value("a directory")?),
            "--write" => args.write.push(value("a directory")?),
            "--tools" => {
                let names: Vec<String> = value("tool names like read,grep")?
                    .split(',')
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty())
                    .collect();
                if names.is_empty() {
                    return Err("--tools needs tool names like read,grep".into());
                }
                args.tools = Some(names);
            }
            "--label" => args.label = Some(value("a label")?),
            "--prompt" | "--prompt-file" => {
                if prompt_said {
                    return Err("give the task once: --prompt or --prompt-file".into());
                }
                prompt_said = true;
                let given = value(if flag == "--prompt" { "the task text" } else { "a file, or - for stdin" })?;
                args.prompt = match (flag, given.as_str()) {
                    ("--prompt-file", "-") => Prompt::Stdin,
                    ("--prompt-file", _) => Prompt::File(given),
                    _ => Prompt::Text(given),
                };
            }
            "--json" if inline.is_none() => args.json = true,
            "--quiet" | "-q" if inline.is_none() => args.quiet = true,
            other => return Err(format!("unknown or misplaced: {other}")),
        }
        i += 1;
    }
    Ok(args)
}

/// The entry `cli::main` dispatches to before the generic parser (SPEC §llm.cli).
pub async fn main(argv: Vec<String>) -> i32 {
    match parse(&argv) {
        Err(err) => {
            eprintln!("{}", usage());
            eprintln!("{err}");
            EXIT_USAGE
        }
        Ok(Cmd::Help) => {
            println!("{}", usage());
            0
        }
        Ok(Cmd::Run(args)) => run_cmd(args).await,
    }
}

/// A line on stderr; a closed stderr is not this run's failure.
fn say(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
    let _ = err.flush();
}

fn task_text(prompt: &Prompt) -> Result<String, String> {
    let text = match prompt {
        Prompt::Text(text) => text.clone(),
        Prompt::File(path) => std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?,
        Prompt::Stdin => {
            if std::io::stdin().is_terminal() {
                return Err("no task: pass --prompt, --prompt-file, or pipe it in".into());
            }
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).map_err(|e| format!("cannot read stdin: {e}"))?;
            String::from_utf8(bytes).map_err(|_| "the task on stdin is not UTF-8 text".to_string())?
        }
    };
    if text.trim().is_empty() {
        return Err("the task is empty".into());
    }
    Ok(text)
}

/// The request body: the working directory made absolute here, where it was given.
fn request_body(args: &RunArgs, prompt: String, here: &std::path::Path) -> Value {
    let cwd = match &args.cwd {
        Some(dir) => here.join(dir),
        None => here.to_path_buf(),
    };
    let mut body = json!({ "prompt": prompt, "cwd": cwd.to_string_lossy() });
    if let Some(agent) = &args.agent {
        body["agent"] = json!(agent);
    }
    if !args.write.is_empty() {
        body["write"] = json!(args.write);
    }
    if let Some(tools) = &args.tools {
        body["tools"] = json!(tools);
    }
    if let Some(label) = &args.label {
        body["label"] = json!(label);
    }
    body
}

async fn run_cmd(args: RunArgs) -> i32 {
    let prompt = match task_text(&args.prompt) {
        Ok(prompt) => prompt,
        Err(why) => {
            say(&format!("agent: {why}"));
            return EXIT_USAGE;
        }
    };
    let here = match std::env::current_dir() {
        Ok(here) => here,
        Err(e) => {
            say(&format!("agent: cannot read the current directory: {e}"));
            return EXIT_USAGE;
        }
    };
    let body = request_body(&args, prompt, &here);
    let base = format!("http://127.0.0.1:{}", resolve_port());
    let client = crate::daemon::loopback_client();
    let send = with_cli_key(client.post(format!("{base}/api/llm/agents/run/stream")).json(&body)).send();
    let mut response = match tokio::time::timeout(OPEN_WITHIN, send).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            say(&format!("agent: cannot reach the gateway on port {}", resolve_port()));
            return EXIT_USAGE;
        }
        Err(_) => {
            say(&format!("agent: the gateway did not answer within {} s", OPEN_WITHIN.as_secs()));
            return EXIT_USAGE;
        }
    };
    let status = response.status();
    if !status.is_success() {
        let answer: Value = response.json().await.unwrap_or(Value::Null);
        let why = match (status.as_u16(), answer["error"].as_str()) {
            (_, Some(why)) => why.to_string(),
            (404, None) => "this gateway has no agent runs (an older build)".to_string(),
            (code, None) => format!("the gateway answered {code}"),
        };
        say(&format!("agent: {why}"));
        return EXIT_USAGE;
    }

    let mut frames = Frames::default();
    let mut run_id: Option<i64> = None;
    let mut done: Option<Value> = None;
    let mut stopping: Option<tokio::time::Instant> = None;
    'read: loop {
        let deadline = stopping.map(|at| at + STOP_WITHIN);
        let chunk = tokio::select! {
            chunk = response.chunk() => chunk,
            // A console that cannot report Ctrl+C disables this arm, never fires it.
            Ok(()) = tokio::signal::ctrl_c() => {
                if stopping.is_some() {
                    say("agent: left without waiting; the dropped connection stops the run");
                    return EXIT_INTERRUPTED;
                }
                stopping = Some(tokio::time::Instant::now());
                say("agent: stopping the run (Ctrl+C again leaves at once)");
                if let Some(id) = run_id {
                    let _ = with_cli_key(client.post(format!("{base}/api/llm/agents/test/cancel")).json(&json!({ "runId": id })))
                        .timeout(Duration::from_secs(5))
                        .send()
                        .await;
                }
                continue;
            }
            _ = sleep_until(deadline) => {
                say("agent: the run did not end in time; leaving (the dropped connection stops it)");
                return EXIT_INTERRUPTED;
            }
        };
        let bytes = match chunk {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => break 'read,
        };
        for (event, data) in frames.push(&bytes) {
            let Ok(value) = serde_json::from_str::<Value>(&data) else { continue };
            match event.as_str() {
                "run" => {
                    run_id = value["runId"].as_i64();
                    if !args.quiet {
                        say(&run_line(&value));
                    }
                }
                "tool" if !args.quiet => say(&tool_line(&value)),
                "toolResult" if !args.quiet => {
                    if let Some(line) = failed_line(&value) {
                        say(&line);
                    }
                }
                "done" => {
                    done = Some(value);
                    break 'read;
                }
                _ => {}
            }
        }
    }

    let Some(answer) = done else {
        say("agent: the gateway closed the stream before the run ended");
        return EXIT_FAILED;
    };
    let code = exit_of(&answer);
    if !args.quiet || code != 0 {
        say(&done_line(&answer));
    }
    let mut out = std::io::stdout().lock();
    if args.json {
        let _ = writeln!(out, "{answer}");
    } else if let Some(text) = answer["text"].as_str().filter(|t| !t.is_empty()) {
        let _ = writeln!(out, "{}", text.trim_end());
    }
    let _ = out.flush();
    code
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn exit_of(answer: &Value) -> i32 {
    if answer["cancelled"].as_bool() == Some(true) {
        EXIT_INTERRUPTED
    } else if answer["ok"].as_bool() == Some(true) && answer["cut"].as_bool() != Some(true) {
        0
    } else {
        EXIT_FAILED
    }
}

/// The server-sent events off the wire, whole: a block ends at a blank line, and a chunk
/// may end anywhere, even inside a character.
#[derive(Default)]
struct Frames {
    pending: Vec<u8>,
}

impl Frames {
    /// The events this chunk completes, each as (event name, data).
    fn push(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        // JSON data never holds a raw CR, so dropping every one makes CRLF framing LF.
        self.pending.extend(bytes.iter().copied().filter(|b| *b != b'\r'));
        let mut out = Vec::new();
        while let Some(at) = self.pending.windows(2).position(|pair| pair == b"\n\n") {
            let block: Vec<u8> = self.pending.drain(..at + 2).collect();
            if let Some(event) = event_of(&String::from_utf8_lossy(&block)) {
                out.push(event);
            }
        }
        out
    }
}

/// One block's event name and data; a comment (the keep-alive) or a block without data is none.
fn event_of(block: &str) -> Option<(String, String)> {
    let mut name = "message".to_string();
    let mut data: Vec<&str> = Vec::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    (!data.is_empty()).then(|| (name, data.join("\n")))
}

fn clipped(text: &str, max: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    let mut cut: String = one.chars().take(max).collect();
    cut.push('…');
    cut
}

fn run_line(run: &Value) -> String {
    let text = |key: &str| run[key].as_str().unwrap_or("?").to_string();
    let mut line = format!(
        "agent: run #{} - {} on {}/{}, in {}",
        run["runId"].as_i64().map_or("-".to_string(), |id| id.to_string()),
        text("agent"),
        text("provider"),
        text("model"),
        text("cwd"),
    );
    let writes: Vec<&str> = run["write"].as_array().map(|dirs| dirs.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if writes.is_empty() {
        line.push_str("; changes nothing");
    } else {
        line.push_str(&format!("; writes {}", writes.join(", ")));
    }
    line
}

/// One tool call: its name, what it names, and the reason the model gave.
fn tool_line(call: &Value) -> String {
    let name = call["name"].as_str().unwrap_or("?");
    // The arguments arrive as an object, or as the JSON text of one.
    let parsed;
    let args = match &call["arguments"] {
        Value::String(text) => {
            parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
            &parsed
        }
        other => other,
    };
    let what = ["path", "pattern"].iter().find_map(|key| args[*key].as_str()).unwrap_or("");
    let mut line = format!("  {name:<5} {}", clipped(what, ARG_SHOWN));
    if let Some(why) = args["description"].as_str().filter(|why| !why.trim().is_empty()) {
        line.push_str(&format!(" - {}", clipped(why, ARG_SHOWN)));
    }
    line.trim_end().to_string()
}

fn failed_line(result: &Value) -> Option<String> {
    if result["ok"].as_bool() != Some(false) {
        return None;
    }
    let text = result["text"].as_str().unwrap_or("failed");
    Some(format!("        ! {}", clipped(text.lines().next().unwrap_or(text), ERROR_SHOWN)))
}

fn done_line(answer: &Value) -> String {
    let took = crate::lane_cli::fmt_secs(answer["ms"].as_u64().unwrap_or(0) / 1000);
    let run = answer["runId"].as_i64().map_or(String::new(), |id| format!(" (run #{id} in History)"));
    if answer["cancelled"].as_bool() == Some(true) {
        return format!("agent: stopped after {took}{run}");
    }
    if answer["ok"].as_bool() != Some(true) {
        return format!("agent: failed after {took}: {}{run}", answer["error"].as_str().unwrap_or("no reason given"));
    }
    let tokens = |key: &str| kilo(answer[key].as_u64().unwrap_or(0));
    let mut line = format!(
        "agent: done in {took} - {} model requests, {} tokens in ({} cached), {} out{run}",
        answer["turns"].as_u64().unwrap_or(0),
        tokens("tokensIn"),
        tokens("tokensCacheRead"),
        tokens("tokensOut"),
    );
    if answer["cut"].as_bool() == Some(true) {
        line.push_str("; the closing answer stopped at the agent's maxTokens, so the task may be unfinished");
    }
    line
}

/// 950 -> "950", 12_345 -> "12.3k", 2_500_000 -> "2.5M".
fn kilo(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_reads_its_flags_strictly() {
        let Ok(Cmd::Run(args)) = parse(&argv(&[
            "run", "--agent", "bl-sync", "--cwd=repo", "--write", ".claude/skills/bl", "--write=docs",
            "--tools", "read, edit ,write", "--label", "sync [a..b]", "--prompt-file", "task.md", "--json", "-q",
        ])) else {
            panic!("a run");
        };
        assert_eq!(args.agent.as_deref(), Some("bl-sync"));
        assert_eq!(args.cwd.as_deref(), Some("repo"));
        assert_eq!(args.write, vec![".claude/skills/bl", "docs"]);
        assert_eq!(args.tools, Some(vec!["read".to_string(), "edit".to_string(), "write".to_string()]));
        assert_eq!(args.label.as_deref(), Some("sync [a..b]"));
        assert_eq!(args.prompt, Prompt::File("task.md".into()));
        assert!(args.json && args.quiet);

        let Ok(Cmd::Run(piped)) = parse(&argv(&["run", "--prompt-file", "-"])) else { panic!("a run") };
        assert_eq!(piped.prompt, Prompt::Stdin);
        let Ok(Cmd::Run(said)) = parse(&argv(&["run", "--prompt", "look --json here"])) else { panic!("a run") };
        assert_eq!(said.prompt, Prompt::Text("look --json here".into()));
        assert!(!said.json, "a flag inside the task text is text");
        let Ok(Cmd::Run(bare)) = parse(&argv(&["run"])) else { panic!("a run") };
        assert_eq!(bare, RunArgs::default());
    }

    #[test]
    fn a_bad_run_is_refused_with_the_reason() {
        let refused = |items: &[&str]| parse(&argv(items)).unwrap_err();
        assert!(refused(&["run", "--agent"]).contains("--agent needs"));
        assert!(refused(&["run", "--prompt", "a", "--prompt-file", "b"]).contains("once"));
        assert!(refused(&["run", "--tools", " , "]).contains("tool names"));
        assert!(refused(&["run", "--json=yes"]).contains("--json"));
        assert!(refused(&["run", "sync the docs"]).contains("unknown or misplaced"));
        assert!(refused(&["start"]).contains("unknown agent command"));
        assert_eq!(parse(&[]), Ok(Cmd::Help));
        assert_eq!(parse(&argv(&["--help"])), Ok(Cmd::Help));
    }

    #[test]
    fn the_body_carries_an_absolute_directory_and_only_what_was_given() {
        let here = std::path::Path::new(if cfg!(windows) { "C:\\work" } else { "/work" });
        let args = RunArgs { cwd: Some("repo".into()), write: vec!["docs".into()], ..RunArgs::default() };
        let body = request_body(&args, "task".into(), here);
        assert_eq!(body["cwd"], json!(here.join("repo").to_string_lossy()));
        assert_eq!(body["write"], json!(["docs"]));
        assert!(body.get("agent").is_none() && body.get("tools").is_none() && body.get("label").is_none());
        let bare = request_body(&RunArgs::default(), "task".into(), here);
        assert_eq!(bare["cwd"], json!(here.to_string_lossy()));
        assert!(bare.get("write").is_none());
    }

    #[test]
    fn frames_come_whole_across_chunks_and_crlf() {
        let mut frames = Frames::default();
        assert!(frames.push(b"event: run\ndata: {\"runId\":3}").is_empty());
        let got = frames.push(b"\n\n: keep-alive\n\nevent: tool\r\ndata: {\"name\":\"read\",\"x\":\"\xe4\xb8").to_vec();
        assert_eq!(got, vec![("run".to_string(), "{\"runId\":3}".to_string())], "the keep-alive comment is no event");
        let got = frames.push(b"\xad\"}\r\n\r\n");
        assert_eq!(got, vec![("tool".to_string(), "{\"name\":\"read\",\"x\":\"中\"}".to_string())], "a character split across chunks survives");
    }

    #[test]
    fn step_lines_say_what_the_agent_does() {
        let call = json!({ "name": "edit", "arguments": { "path": "docs/a.md", "description": "Update the  order\nflow" } });
        assert_eq!(tool_line(&call), "  edit  docs/a.md - Update the order flow");
        let as_text = json!({ "name": "grep", "arguments": "{\"pattern\":\"fn main\"}" });
        assert_eq!(tool_line(&as_text), "  grep  fn main");
        assert_eq!(failed_line(&json!({ "ok": true, "text": "fine" })), None);
        assert_eq!(failed_line(&json!({ "ok": false, "text": "error: no such path: x\nmore" })).as_deref(), Some("        ! error: no such path: x"));
        let run = json!({ "runId": 7, "agent": "w", "provider": "glm", "model": "m", "cwd": "C:/r", "write": ["docs"] });
        assert_eq!(run_line(&run), "agent: run #7 - w on glm/m, in C:/r; writes docs");
        assert!(run_line(&json!({ "write": [] })).ends_with("changes nothing"));
    }

    #[test]
    fn the_outcome_reads_as_a_line_and_an_exit_code() {
        let done = json!({ "ok": true, "ms": 75_000, "turns": 9, "tokensIn": 123_456, "tokensCacheRead": 100_000, "tokensOut": 950, "runId": 4, "text": "x" });
        assert_eq!(done_line(&done), "agent: done in 1m15s - 9 model requests, 123.5k tokens in (100.0k cached), 950 out (run #4 in History)");
        assert_eq!(exit_of(&done), 0);
        let cut = json!({ "ok": true, "cut": true, "ms": 0 });
        assert_eq!(exit_of(&cut), EXIT_FAILED);
        assert!(done_line(&cut).contains("maxTokens"));
        let failed = json!({ "ok": false, "error": "429 rate limited", "ms": 2_000 });
        assert_eq!(exit_of(&failed), EXIT_FAILED);
        assert_eq!(done_line(&failed), "agent: failed after 2s: 429 rate limited");
        let stopped = json!({ "ok": false, "cancelled": true, "error": "cancelled", "ms": 0, "runId": 5 });
        assert_eq!(exit_of(&stopped), EXIT_INTERRUPTED);
        assert_eq!(done_line(&stopped), "agent: stopped after 0s (run #5 in History)");
        assert_eq!(kilo(2_500_000), "2.5M");
    }
}
