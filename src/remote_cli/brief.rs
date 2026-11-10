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

//! `swiss remote brief [name]` (SPEC §remote.cli): what an agent should know about a target
//! before its first command there, in one call - where it is, whether its endpoint is up, what
//! is running on it now, how the last runs went and why they ran, and the target's note index.
//! The skill runs it as it loads, so the first remote command starts informed instead of after
//! a round of `targets` / `endpoints` / `notes` / `audit` calls.
//!
//! It is a briefing, not a gate: a gateway that is down or a name that is no target still
//! prints what is known and exits 0, so a skill that runs it never fails to load.

use serde_json::Value;

use super::{ago, human_bytes, notes_topics, Gateway, RemoteArgs};

/// The note index a brief prints whole; past this it prints the head and says so - an index
/// that big has stopped being one (SPEC §remote.project "Notes").
pub const INDEX_BUDGET: usize = 6 * 1024;
/// How many of the target's past runs a brief lists.
pub const RECENT_RUNS: usize = 5;
/// A `why` as a brief line shows it.
const WHY_CHARS: usize = 100;

pub async fn cmd_brief(gw: &Gateway, a: &RemoteArgs) -> i32 {
    let targets = match gw.get("/api/remote/targets").await {
        Ok(value) => value["targets"].as_array().cloned().unwrap_or_default(),
        Err(err) => {
            println!("swiss gateway: not reachable ({err}); remote commands fail until it is up (swiss status)");
            print_note_list();
            return 0;
        }
    };
    let endpoints = gw.get("/api/remote/endpoints").await.ok();
    let binding = swiss_remote::project::ProjectBinding::discover(std::path::Path::new(".")).unwrap_or(None);
    // The skill passes whatever followed its name, so the target is the first word that is one
    // (or a project action naming one); the rest of the sentence is not for the brief.
    let wanted = a.target.clone().or_else(|| {
        a.words.iter().find_map(|word| {
            let word = word.trim().trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            if targets.iter().any(|t| t["id"].as_str() == Some(word)) {
                return Some(word.to_string());
            }
            binding.as_ref().and_then(|b| b.action(word)).map(|action| action.target.clone())
        })
    });
    let wanted = wanted.or_else(|| {
        // A bare brief in a bound project is about the project's default target.
        a.words.is_empty().then(|| binding.as_ref().and_then(|b| b.default_target.clone())).flatten()
    });
    match wanted.and_then(|id| targets.iter().find(|t| t["id"].as_str() == Some(id.as_str())).cloned()) {
        Some(target) => brief_one(gw, &target, endpoints.as_ref()).await,
        None => {
            if let Some(word) = a.words.first() {
                println!("{word:?} is no target here; every target:\n");
            }
            overview(gw, &targets, endpoints.as_ref()).await;
        }
    }
    0
}

/// One target, everything an agent needs before it acts there.
async fn brief_one(gw: &Gateway, target: &Value, endpoints: Option<&Value>) {
    let id = target["id"].as_str().unwrap_or("?");
    let (endpoint, state) = endpoint_of(endpoints, target["endpoint"].as_str().unwrap_or("?"));
    println!("target {id}: {endpoint} ({state}), root {}", target["workspaceRoot"].as_str().unwrap_or("?"));
    let caps = caps_of(target);
    let timeout = target["defaultTimeoutMs"].as_u64().map(|ms| format!(", default timeout {}", minutes(ms))).unwrap_or_default();
    println!("can: {caps}{timeout}");
    if let Some(note) = state_note(&state) {
        println!("note: {note}");
    }

    let running = running_on(gw, id).await;
    if !running.is_empty() {
        println!("\nrunning now:");
        for line in &running {
            println!("  {line}");
        }
    }

    let recent = gw
        .get(&format!("/api/remote/runs?target={}&limit={RECENT_RUNS}", super::query_encode(id)))
        .await
        .ok()
        .and_then(|page| page["runs"].as_array().cloned())
        .unwrap_or_default();
    if !recent.is_empty() {
        println!("\nlast runs:");
        for row in &recent {
            println!("  {}", recent_line(row));
        }
    }

    let dir = swiss_core::paths::data_path(&["remote-notes"]);
    println!();
    match std::fs::read_to_string(dir.join(format!("{id}.md"))) {
        Ok(text) => {
            println!("notes ({}, swiss remote notes {id}):", human_bytes(text.len() as u64));
            print!("{}", index_cut(&text, INDEX_BUDGET));
        }
        Err(_) => println!("no notes for {id} yet; save what you learn there: swiss remote notes --append {id} < note.md"),
    }
    let topics = notes_topics(&dir, id);
    if !topics.is_empty() {
        let list: Vec<String> = topics.iter().map(|(name, bytes)| format!("{name} ({})", human_bytes(*bytes))).collect();
        println!("topics: {}  (swiss remote notes {id} <topic>)", list.join(", "));
    }
}

/// Every target, one line each, for a brief that named none.
async fn overview(gw: &Gateway, targets: &[Value], endpoints: Option<&Value>) {
    if targets.is_empty() {
        println!("no targets yet: swiss remote target add <id> --endpoint <id|name> --root <path>");
        return;
    }
    let dir = swiss_core::paths::data_path(&["remote-notes"]);
    for target in targets {
        let id = target["id"].as_str().unwrap_or("?");
        let (endpoint, state) = endpoint_of(endpoints, target["endpoint"].as_str().unwrap_or("?"));
        let notes = std::fs::metadata(dir.join(format!("{id}.md")))
            .map(|m| format!("notes {}", human_bytes(m.len())))
            .unwrap_or_else(|_| "no notes".into());
        let last = gw
            .get(&format!("/api/remote/runs?target={}&limit=1", super::query_encode(id)))
            .await
            .ok()
            .and_then(|page| page["runs"].as_array().and_then(|runs| runs.first().cloned()))
            .map(|row| format!(", last run {} {}", when_of(&row), super::audit_outcome(&row)))
            .unwrap_or_default();
        println!(
            "{id:<16} {endpoint} ({state}), root {}, {} - {notes}{last}",
            target["workspaceRoot"].as_str().unwrap_or("?"),
            caps_of(target)
        );
    }
    println!("\nswiss remote brief <target> for one target's runs and notes");
}

/// A brief with no gateway still shows which notes exist.
fn print_note_list() {
    let dir = swiss_core::paths::data_path(&["remote-notes"]);
    let notes = super::notes_list(&dir);
    if !notes.is_empty() {
        let names: Vec<&str> = notes.iter().map(|n| n.alias.as_str()).collect();
        println!("notes kept for: {} (swiss remote notes <target>)", names.join(", "));
    }
}

/// The endpoint's display name and state, from the transport's inventory.
fn endpoint_of(endpoints: Option<&Value>, id: &str) -> (String, String) {
    let row = endpoints
        .and_then(|e| e["endpoints"].as_array())
        .and_then(|rows| rows.iter().find(|e| e["id"].as_str() == Some(id)));
    match row {
        Some(row) => (
            row["label"].as_str().filter(|l| !l.is_empty()).unwrap_or(id).to_string(),
            row["state"].as_str().unwrap_or("?").to_string(),
        ),
        None if endpoints.is_some() => (id.to_string(), "not served".to_string()),
        None => (id.to_string(), "?".to_string()),
    }
}

/// What an endpoint's state means for the next command (the tunnels states: idle, connecting,
/// connected, error), `None` when there is nothing to say.
pub fn state_note(state: &str) -> Option<String> {
    match state {
        "connected" | "?" => None,
        "idle" => Some("the endpoint is idle; the first command dials it".into()),
        "not served" => Some("no connection serves this endpoint; a command fails until Tunnels is on and has it".into()),
        other => Some(format!("the endpoint is {other}; a command dials it again and may fail")),
    }
}

fn caps_of(target: &Value) -> String {
    target["capabilities"]
        .as_array()
        .map(|caps| caps.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","))
        .filter(|caps| !caps.is_empty())
        .unwrap_or_else(|| "exec".into())
}

/// The target's runs that are queued or running now, from the live run table.
async fn running_on(gw: &Gateway, id: &str) -> Vec<String> {
    let Ok(runs) = gw.get("/api/runs").await else { return Vec::new() };
    let now = swiss_core::util::now_ms() as i64;
    runs["runs"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|run| run["action"].as_str().is_some_and(|a| a.starts_with("remote.")))
                .filter(|run| matches!(run["state"].as_str(), Some("queued" | "running")))
                .filter(|run| {
                    run["meta"]["target"].as_str() == Some(id)
                        || run["label"].as_str().is_some_and(|l| l.rsplit(' ').next() == Some(id))
                })
                .map(|run| {
                    let since = run["startedAt"]
                        .as_str()
                        .or(run["queuedAt"].as_str())
                        .and_then(swiss_core::util::parse_iso_ms)
                        .map(|at| format!(" for {}", minutes(now.saturating_sub(at).max(0) as u64)))
                        .unwrap_or_default();
                    format!(
                        "#{} {} {}{since} by {}",
                        run["runId"].as_u64().unwrap_or(0),
                        run["state"].as_str().unwrap_or("?"),
                        run["label"].as_str().unwrap_or("?"),
                        run["actor"].as_str().unwrap_or("-"),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One past run: when, how it ended, what ran, who, and why when the caller said.
pub fn recent_line(row: &Value) -> String {
    let mut command = super::audit_command(row);
    if command.chars().count() > 80 {
        command = command.chars().take(79).collect::<String>() + "…";
    }
    let mut line = format!(
        "#{} {} {} {} by {}",
        row["runId"].as_u64().unwrap_or(0),
        when_of(row),
        super::audit_outcome(row),
        command,
        row["actor"].as_str().unwrap_or("-"),
    );
    if let Some(why) = row["input"]["why"].as_str().filter(|w| !w.trim().is_empty()) {
        line.push_str(&format!(" - why: {}", one_line(why, WHY_CHARS)));
    }
    line
}

fn when_of(row: &Value) -> String {
    let at = row["endedAt"].as_str().or(row["startedAt"].as_str()).unwrap_or("");
    match swiss_core::util::parse_iso_ms(at) {
        Some(ms) => {
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms.max(0) as u64);
            ago(at)
        }
        None => at.get(..16).unwrap_or(at).to_string(),
    }
}

fn minutes(ms: u64) -> String {
    match ms / 1000 {
        s if s < 90 => format!("{s}s"),
        s if s < 90 * 60 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// Text on one line, cut to `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        flat.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        flat
    }
}

/// The index as a brief prints it: whole when it fits the budget, else cut at a line end with
/// a pointer to the whole note and a hint to move detail into topics.
pub fn index_cut(text: &str, budget: usize) -> String {
    let mut out = String::new();
    if text.len() <= budget {
        out.push_str(text);
    } else {
        let mut end = budget;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let head = &text[..end];
        out.push_str(head.rsplit_once('\n').map(|(head, _)| head).unwrap_or(head));
        out.push_str(&format!(
            "\n[... {} more in the note; it is past the {} an index should be - move detail into topic notes]",
            human_bytes((text.len() - end) as u64),
            human_bytes(budget as u64)
        ));
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_index_past_its_budget_is_cut_at_a_line_end() {
        assert_eq!(index_cut("a\nb", 100), "a\nb\n");
        let long = "line one\nline two\nline three\n";
        let cut = index_cut(long, 12);
        assert!(cut.starts_with("line one\n["), "{cut}");
        assert!(cut.contains("move detail into topic notes"));
    }

    #[test]
    fn a_past_run_shows_why_it_ran() {
        let row = json!({
            "runId": 7, "state": "succeeded", "action": "remote.exec", "actor": "cli:codex",
            "input": {"argv": ["make", "-j8"], "why": "rebuild\nafter the  fix"},
        });
        let line = recent_line(&row);
        assert!(line.starts_with("#7 "), "{line}");
        assert!(line.contains("exit 0 make -j8 by cli:codex - why: rebuild after the fix"), "{line}");
        assert_eq!(one_line("abcdef", 4), "abc…");
    }

    // Found live 2026-11-11: a connected endpoint was briefed as one a command waits for.
    #[test]
    fn a_connected_endpoint_needs_no_note() {
        assert_eq!(state_note("connected"), None);
        assert!(state_note("idle").is_some_and(|note| note.contains("first command dials")));
        assert!(state_note("error").is_some_and(|note| note.contains("may fail")));
    }
}
