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

//! Whether what the manifest names works (SPEC §host.setup). Every check runs after every write, so
//! a database behind a tunnel is tried once its tunnel is up. A definition that did not change is
//! checked all the same: a rerun is how an Agent learns its fix landed, and exit 0 says every check
//! passed.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use reqwest::Method;
use serde_json::{json, Value};

use super::manifest::{Manifest, Warning};
use super::plan::Options;
use super::report::{Item, Status};
use crate::adminapi::TESTABLE_TYPES;
use crate::remote_cli::Gateway;
use swiss_host::config::ServerDef;
use swiss_mcp::oauth::is_oauth;

/// A manifest may name many children, on a machine that may be short of memory.
pub(super) const CHECKS_AT_ONCE: usize = 4;

/// Past a proc's 60 s handshake, so the gateway's own answer is what normally ends a slow one.
pub(super) const CHECK_BOUND: Duration = Duration::from_secs(75);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    None,
    /// `POST /api/mcpdefs/test`: a handshake or a plain GET on a throwaway adapter, 5 s at most.
    Test,
    /// List the tools of the running MCP. A child is then left to the idle reaper.
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Step {
    start: bool,
    probe: Probe,
    /// Someone stopped it and this run did not add it, so stopped it stays, as an edit leaves it.
    left_stopped: bool,
}

/// What one MCP needs, from its type, whether this run added it, and the state it is in. Under
/// `no_check`, what starts is what would be running anyway: a proc waits to be asked for, and an MCP
/// added here was only held back so that every check runs after every write.
fn step_for(def: &ServerDef, created: bool, lifecycle: &str, no_check: bool) -> Step {
    let left_stopped = lifecycle == "stopped" && !created;
    let probe = if no_check {
        Probe::None
    } else if TESTABLE_TYPES.contains(&def.type_()) {
        Probe::Test
    } else if left_stopped {
        Probe::None
    } else {
        Probe::Tools
    };
    let start = !left_stopped
        && lifecycle != "started"
        && if def.type_() == "proc" { probe == Probe::Tools } else { lifecycle != "idle" || probe == Probe::Tools };
    Step { start, probe, left_stopped }
}

struct Failure {
    code: &'static str,
    error: String,
}

/// Checks every MCP that was saved, or was already there. A check that fails leaves the item's
/// status as it is and sets `ok` false; the definition stays saved.
pub async fn check_all(
    gw: &Gateway,
    manifest: &Manifest,
    options: Options,
    items: &mut [Item],
    warnings: &mut Vec<Warning>,
) {
    let rows = read_rows(gw).await;
    let mut jobs = Vec::new();
    for (index, item) in items.iter_mut().enumerate() {
        if item.kind != "mcp" || matches!(item.status, Status::Failed | Status::Blocked) {
            continue;
        }
        let Some(entry) = manifest.mcps.get(&item.name) else {
            continue;
        };
        let row = rows.get(&item.name);
        // A throwaway adapter holds no grant, and only a person can give one. Approval starts it.
        if is_oauth(&entry.def) {
            if row.is_none_or(|row| row["oauth"] != json!("authorized")) {
                item.status = Status::NeedsInput;
                item.note("code", "mcp.oauth");
            }
            continue;
        }
        let lifecycle = row.and_then(|row| row["lifecycle"].as_str()).unwrap_or_default();
        let step = step_for(&entry.def, item.status == Status::Created, lifecycle, options.no_check);
        if step.left_stopped {
            warnings.push(Warning::new(
                "mcp.left-stopped",
                format!("mcps.{}", item.name),
                format!(
                    "this MCP is stopped and a stop is someone's choice, so it stays stopped; swiss api POST /api/mcps/{}/start starts it",
                    item.name
                ),
            ));
        }
        if step.start || step.probe != Probe::None {
            jobs.push((index, item.name.clone(), &entry.def, step));
        }
    }
    let outcomes: Vec<(usize, Result<Option<String>, Failure>)> = stream::iter(jobs)
        .map(|(index, name, def, step)| async move {
            let outcome = tokio::time::timeout(CHECK_BOUND, run_step(gw, &name, def, step)).await;
            let outcome = outcome.unwrap_or_else(|_| {
                Err(Failure {
                    code: "check.timeout",
                    error: format!("no answer after {} s", CHECK_BOUND.as_secs()),
                })
            });
            (index, outcome)
        })
        .buffer_unordered(CHECKS_AT_ONCE)
        .collect()
        .await;
    for (index, outcome) in outcomes {
        match outcome {
            Ok(None) => {}
            Ok(Some(detail)) => items[index].note("check", json!({ "ok": true, "detail": detail })),
            Err(failure) => items[index].note(
                "check",
                json!({ "ok": false, "code": failure.code, "error": failure.error }),
            ),
        }
    }
}

/// Each MCP's row by name. A gateway that cannot answer here will not answer a check either, and
/// each check then says so.
async fn read_rows(gw: &Gateway) -> BTreeMap<String, Value> {
    let listing = gw.get("/api/mcps").await.unwrap_or_default();
    listing["mcps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["name"].as_str().map(|name| (name.to_string(), row.clone())))
        .collect()
}

async fn run_step(gw: &Gateway, name: &str, def: &ServerDef, step: Step) -> Result<Option<String>, Failure> {
    if step.start {
        gw.send(Method::POST, &format!("/api/mcps/{name}/start"), None)
            .await
            .map_err(|error| Failure { code: "check.start", error })?;
    }
    match step.probe {
        Probe::None => Ok(None),
        Probe::Test => test_definition(gw, def).await.map(Some),
        Probe::Tools => {
            let listing = gw
                .get(&format!("/api/mcps/{name}/tools"))
                .await
                .map_err(|error| Failure { code: "check.tools", error })?;
            Ok(Some(count_tools(&listing)))
        }
    }
}

async fn test_definition(gw: &Gateway, def: &ServerDef) -> Result<String, Failure> {
    let unreachable = |error: String| Failure { code: "check.unreachable", error };
    let answer = gw
        .send(Method::POST, "/api/mcpdefs/test", Some(Value::Object(def.0.clone())))
        .await
        .map_err(unreachable)?;
    if answer["ok"] == json!(true) {
        Ok(format!("answered in {} ms", answer["ms"]))
    } else {
        Err(unreachable(answer["error"].as_str().unwrap_or("no detail").to_string()))
    }
}

/// One page says how many there are when it knows, and that there are more when it does not.
fn count_tools(listing: &Value) -> String {
    let listed = listing["tools"].as_array().map_or(0, Vec::len);
    match listing["total"].as_u64() {
        Some(total) => format!("{total} tools"),
        None if listing.get("nextCursor").is_some() => format!("{listed}+ tools"),
        None => format!("{listed} tools"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(body: Value) -> ServerDef {
        crate::adminapi::build_def(&body, &[]).expect("a definition")
    }

    fn step(start: bool, probe: Probe) -> Step {
        Step { start, probe, left_stopped: false }
    }

    // SPEC §host.setup, the checks table: a proc is started to be listed, and `--no-check` leaves it
    // to the first request that wants it.
    #[test]
    fn a_proc_starts_only_to_be_checked() {
        let tool = def(json!({ "command": "tool" }));
        assert_eq!(step_for(&tool, true, "idle", false), step(true, Probe::Tools));
        assert_eq!(step_for(&tool, false, "idle", false), step(true, Probe::Tools));
        assert_eq!(step_for(&tool, false, "started", false), step(false, Probe::Tools));
        assert_eq!(step_for(&tool, false, "error", false), step(true, Probe::Tools));
        assert_eq!(step_for(&tool, true, "idle", true), step(false, Probe::None));
        assert_eq!(step_for(&tool, false, "error", true), step(false, Probe::None));
    }

    // SPEC §host.setup: "skipped by `--no-check`, except that non-proc MCPs still start".
    #[test]
    fn a_typed_mcp_starts_either_way_and_is_tested_apart() {
        let db = def(json!({ "type": "mysql" }));
        assert_eq!(step_for(&db, true, "stopped", false), step(true, Probe::Test));
        assert_eq!(step_for(&db, true, "stopped", true), step(true, Probe::None));
        assert_eq!(step_for(&db, false, "error", true), step(true, Probe::None));
        assert_eq!(step_for(&db, false, "started", false), step(false, Probe::Test));
        assert_eq!(step_for(&db, false, "started", true), step(false, Probe::None));
        // Lazy on purpose: testing it costs nothing, and starting it is not what was asked for.
        assert_eq!(step_for(&db, true, "idle", false), step(false, Probe::Test));

        let echo = def(json!({ "type": "echo" }));
        assert_eq!(step_for(&echo, true, "stopped", false), step(true, Probe::Tools));
        assert_eq!(step_for(&echo, true, "stopped", true), step(true, Probe::None));
    }

    // SPEC §host.setup: a stop is someone's choice. Testing a definition does not touch what runs.
    #[test]
    fn what_someone_stopped_stays_stopped() {
        let stopped = |probe| Step { start: false, probe, left_stopped: true };
        let db = def(json!({ "type": "mysql" }));
        assert_eq!(step_for(&db, false, "stopped", false), stopped(Probe::Test));
        assert_eq!(step_for(&db, false, "stopped", true), stopped(Probe::None));
        let tool = def(json!({ "command": "tool", "lazy": false }));
        assert_eq!(step_for(&tool, false, "stopped", false), stopped(Probe::None));
        assert_eq!(step_for(&tool, true, "stopped", false), step(true, Probe::Tools));
    }

    #[test]
    fn a_listing_says_how_many_tools_it_knows_of() {
        assert_eq!(count_tools(&json!({ "tools": [{}, {}] })), "2 tools");
        assert_eq!(count_tools(&json!({ "tools": [{}], "total": 21 })), "21 tools");
        assert_eq!(count_tools(&json!({ "tools": [{}], "nextCursor": "c" })), "1+ tools");
        assert_eq!(count_tools(&json!({})), "0 tools");
    }
}
