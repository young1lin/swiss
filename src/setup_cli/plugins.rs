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

//! The manifest's `plugins` section (SPEC §host.setup): a merge patch over each plugin's row and
//! whether the plugin runs. A row can hold a credential, and a job or an agent is something that
//! runs, so what is read here stays here: the document says what changes and what would run, and
//! never prints a row.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use reqwest::{Method, StatusCode};
use serde_json::{json, Map, Value};

use super::apply::fail;
use super::check::{CHECKS_AT_ONCE, CHECK_BOUND};
use super::manifest::{Manifest, PluginEntry, Warning};
use super::plan::Options;
use super::refuse::nearest;
use super::report::{Item, Status};
use super::CONFLICT_RETRIES;
use crate::remote_cli::Gateway;
use swiss_core::secure::literal;
use swiss_host::mask::mask_command;

/// A gateway answers on its port before its plugins have started (SPEC §host.plugins), and the slowest
/// start the host knows is a proc-holding plugin bringing its children back.
const BOOT_BOUND: Duration = Duration::from_secs(60);
const BOOT_POLL: Duration = Duration::from_millis(200);

const KEPT_CHANGING: &str = "the configuration kept changing while this wrote; run it again";

/// Each plugin's inventory row, by id.
pub type Rows = BTreeMap<String, Value>;

pub async fn read_rows(gw: &Gateway) -> Result<Rows, String> {
    let inventory = gw.get("/api/plugins").await?;
    Ok(rows_of(&inventory))
}

fn rows_of(inventory: &Value) -> Rows {
    inventory["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str().map(|id| (id.to_string(), row.clone())))
        .collect()
}

/// The plugins a boot has yet to bring up. An entry reads `disabled` until the boot reaches it, so
/// what tells it from one someone turned off is that its row still wants it on.
fn still_starting(rows: &Rows) -> Vec<&str> {
    rows.iter()
        .filter(|(_, row)| row["enabled"] == json!(true) && matches!(row["state"].as_str(), Some("disabled" | "starting")))
        .map(|(id, _)| id.as_str())
        .collect()
}

/// The rows, once the boot has finished. Until then a route of a plugin it has not reached answers
/// 503, and a command that just started this gateway would read that as no gateway at all.
pub async fn wait_until_started(gw: &Gateway) -> Result<Rows, String> {
    let deadline = Instant::now() + BOOT_BOUND;
    loop {
        let rows = read_rows(gw).await?;
        let waiting = still_starting(&rows);
        if waiting.is_empty() {
            return Ok(rows);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} still starting after {} s; `swiss logs` says why",
                waiting.join(", "),
                BOOT_BOUND.as_secs()
            ));
        }
        tokio::time::sleep(BOOT_POLL).await;
    }
}

/// Why this gateway cannot answer for its MCPs, when it cannot.
pub fn mcp_off_reason(rows: &Rows) -> Option<String> {
    plugin_off_reason(rows, "mcp")
}

pub(super) fn plugin_off_reason(rows: &Rows, id: &str) -> Option<String> {
    let row = rows.get(id);
    let state = row.and_then(|row| row["state"].as_str()).unwrap_or_default();
    let how = format!("plugins.{id}.enabled: true in the manifest, or swiss api POST /api/plugins/{id}/enable, turns it on");
    match state {
        "active" => None,
        "failed" => {
            let error = row.and_then(|row| row["lastError"].as_str()).unwrap_or("no detail");
            Some(format!("the {id} plugin failed to start: {}; {how}", literal::mask(error)))
        }
        _ => Some(format!("the {id} plugin is off; {how}")),
    }
}

/// Objects merge key by key; anything else, an array included, replaces what was there. A manifest
/// holds no null, so this has no way to remove a key, and a stale manifest removes nothing.
pub fn merge_patch(base: &Value, patch: &Value) -> Value {
    let (Value::Object(base), Value::Object(patch)) = (base, patch) else {
        return patch.clone();
    };
    let mut merged = base.clone();
    for (key, value) in patch {
        let next = merged.get(key).map_or_else(|| value.clone(), |held| merge_patch(held, value));
        merged.insert(key.clone(), next);
    }
    Value::Object(merged)
}

/// What the gateway said about one named plugin. The row's config is compared and dropped here: it
/// is what must not reach the document.
#[derive(Debug, Default)]
pub struct Stored {
    pub row: Value,
    pub config_changes: bool,
    /// The plugin's own validator refused the merged row.
    pub invalid: Option<String>,
    /// The merged row saves, and these are true of it.
    pub warnings: Vec<String>,
    /// Nobody was asked whether the merged row saves.
    pub not_validated: bool,
}

/// Read every named plugin the gateway has. An id it does not have is never put in a URL.
///
/// `may_dry_run` is false for a gateway on another build: an older one has no `?dryRun=1` and would
/// save what it was only asked about, which a plan must never do.
pub async fn read_named(
    gw: &Gateway,
    manifest: &Manifest,
    rows: &Rows,
    may_dry_run: bool,
) -> Result<BTreeMap<String, Stored>, String> {
    let mut named = BTreeMap::new();
    for (id, entry) in &manifest.plugins {
        let Some(row) = rows.get(id) else {
            continue;
        };
        let mut stored = Stored { row: row.clone(), ..Stored::default() };
        if let Some(patch) = &entry.config {
            let held = gw.get(&config_path(id)).await?;
            let merged = merge_patch(&held["config"], patch);
            stored.config_changes = merged != held["config"];
            if stored.config_changes && may_dry_run {
                dry_run(gw, id, merged, &mut stored).await?;
            } else if stored.config_changes {
                stored.not_validated = true;
            }
        }
        named.insert(id.clone(), stored);
    }
    Ok(named)
}

fn config_path(id: &str) -> String {
    format!("/api/plugins/{id}/config")
}

async fn dry_run(gw: &Gateway, id: &str, merged: Value, stored: &mut Stored) -> Result<(), String> {
    let path = format!("{}?dryRun=1", config_path(id));
    let (status, answer) = gw.exchange(Method::PUT, &path, Some(json!({ "config": merged }))).await?;
    let error = literal::mask(answer["error"].as_str().unwrap_or("no detail"));
    match status {
        StatusCode::BAD_REQUEST => stored.invalid = Some(error),
        status if status.is_success() => {
            stored.warnings = answer["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(literal::mask)
                .collect();
        }
        status => return Err(format!("{} {error}", status.as_u16())),
    }
    Ok(())
}

/// One plugin's item. `stored` is None for an id the gateway does not have.
pub fn decide(
    id: &str,
    entry: &PluginEntry,
    stored: Option<&Stored>,
    known: &[&str],
    unsettled: &BTreeSet<String>,
    warnings: &mut Vec<Warning>,
) -> Item {
    let path = format!("plugins.{id}.config");
    let Some(stored) = stored else {
        let error = match nearest(id, known) {
            Some(near) => format!("no such plugin; did you mean {near}?"),
            None => format!("no such plugin; there are: {}", known.join(", ")),
        };
        return Item::new("plugin", id, Status::Failed).with("code", "plugin.unknown").with("error", error);
    };
    for warning in &stored.warnings {
        warnings.push(Warning::new("plugin.warning", path.clone(), warning.clone()));
    }
    if stored.not_validated {
        warnings.push(Warning::new(
            "plugin.not-validated",
            path.clone(),
            "the gateway runs another build, which might save what it is only asked about, so this is validated when apply writes it",
        ));
    }
    let by: Vec<Value> = entry
        .secrets
        .iter()
        .filter(|secret| unsettled.contains(*secret))
        .map(|secret| json!(format!("secret:{secret}")))
        .collect();
    let mut item = if let Some(error) = &stored.invalid {
        Item::new("plugin", id, Status::Failed).with("code", "plugin.invalid").with("error", error.clone())
    } else if !by.is_empty() {
        Item::new("plugin", id, Status::Blocked).with("by", by)
    } else if stored.config_changes || toggle_for(entry.enabled, &stored.row).is_some() {
        Item::new("plugin", id, Status::Updated)
    } else {
        Item::new("plugin", id, Status::Unchanged)
    };
    let would_run = entry.config.as_ref().map(|patch| executes(id, patch)).unwrap_or_default();
    if !would_run.is_empty() {
        item.note("executes", Value::Object(would_run));
    }
    item
}

/// Which way this plugin must be turned, if at all. Asked for on and failed counts as off: that is
/// how a rerun retries a start, once what stopped it is fixed.
fn toggle_for(wanted: Option<bool>, row: &Value) -> Option<bool> {
    let enabled = row["enabled"] == json!(true);
    match wanted? {
        true if !enabled || row["state"] == json!("failed") => Some(true),
        false if enabled => Some(false),
        _ => None,
    }
}

/// What the patch would run, by name, read from the patch alone: a manifest is reviewed for what it
/// adds. Masked, because a command line can carry what the panel masks too.
fn executes(id: &str, patch: &Value) -> Map<String, Value> {
    let mut found = Map::new();
    let mut note = |name: String, command: Option<String>| {
        if let Some(command) = command {
            found.insert(name, json!(mask_command(&command)));
        }
    };
    match id {
        "jobs" => {
            for (name, job) in patch["definitions"].as_object().into_iter().flatten() {
                let action = &job["action"];
                if action["type"].as_str().is_some_and(|kind| kind.starts_with("process.")) {
                    let input = &action["input"];
                    let command = input["command"].as_str().map(str::to_string).or_else(|| program_line(input));
                    note(name.clone(), command);
                }
            }
        }
        "process" => {
            for app in patch["apps"].as_array().into_iter().flatten() {
                let name = app["name"].as_str().unwrap_or_default();
                note(name.to_string(), program_line(&app["start"]));
                note(format!("{name}.stop"), program_line(&app["stop"]));
            }
        }
        "llm" => {
            for (name, agent) in patch["agents"].as_object().into_iter().flatten() {
                note(name.clone(), agent["command"].as_str().map(str::to_string));
            }
        }
        _ => {}
    }
    found
}

fn program_line(spec: &Value) -> Option<String> {
    let mut words = vec![spec["program"].as_str()?.to_string()];
    words.extend(spec["args"].as_array().into_iter().flatten().filter_map(|arg| arg.as_str().map(str::to_string)));
    Some(words.join(" "))
}

/// Config, then the turn, so a plugin turned on starts on what the manifest says and never on what
/// it held before.
pub async fn write(gw: &Gateway, entry: &PluginEntry, item: &mut Item) {
    if item.status != Status::Updated {
        return;
    }
    let id = item.name.clone();
    let wrote = match &entry.config {
        Some(patch) => write_config(gw, &id, patch, item).await,
        None => Ok(()),
    };
    let wrote = match (wrote, entry.enabled) {
        (Ok(()), Some(wanted)) => write_toggle(gw, &id, wanted).await,
        (wrote, _) => wrote,
    };
    if let Err(error) = wrote {
        fail(item, "plugin.write", literal::mask(&error));
    }
}

/// Merge against what is there now and write against that revision. A write that loses the race
/// merges again, so what the other writer saved stays.
async fn write_config(gw: &Gateway, id: &str, patch: &Value, item: &mut Item) -> Result<(), String> {
    let path = config_path(id);
    for _ in 0..=CONFLICT_RETRIES {
        let held = gw.get(&path).await?;
        let merged = merge_patch(&held["config"], patch);
        if merged == held["config"] {
            return Ok(());
        }
        let body = json!({ "config": merged, "revision": held["revision"] });
        let (status, answer) = gw.exchange(Method::PUT, &path, Some(body)).await?;
        if status == StatusCode::CONFLICT {
            continue;
        }
        let detail = answer["error"].as_str().unwrap_or("no detail");
        if !status.is_success() {
            return Err(format!("{} {detail}", status.as_u16()));
        }
        // Saved, and the running plugin did not take it: the row is right and the machine is not.
        if answer["applied"] == json!(false) {
            let error = literal::mask(detail);
            item.note("check", json!({ "ok": false, "code": "plugin.apply", "error": error }));
        }
        return Ok(());
    }
    Err(KEPT_CHANGING.into())
}

/// The revision moves on a write to any plugin, the config just written included, so each try reads
/// it again along with whether the turn is still needed.
async fn write_toggle(gw: &Gateway, id: &str, wanted: bool) -> Result<(), String> {
    for _ in 0..=CONFLICT_RETRIES {
        let inventory = gw.get("/api/plugins").await?;
        let rows = rows_of(&inventory);
        let row = rows.get(id).ok_or("the plugin left the gateway's inventory")?;
        let Some(turn) = toggle_for(Some(wanted), row) else {
            return Ok(());
        };
        let verb = if turn { "enable" } else { "disable" };
        let body = json!({ "revision": inventory["revision"] });
        let (status, answer) = gw.exchange(Method::POST, &format!("/api/plugins/{id}/{verb}"), Some(body)).await?;
        if status == StatusCode::CONFLICT {
            continue;
        }
        if !status.is_success() {
            let detail = answer["error"].as_str().unwrap_or("no detail");
            return Err(format!("{} {detail}", status.as_u16()));
        }
        return Ok(());
    }
    Err(KEPT_CHANGING.into())
}

/// A plugin asked for that did not start is a failed check, under `--no-check` too, since finding it
/// costs nothing. A provider is asked for its model listing, which is not billed, and never for a
/// completion, which is.
pub async fn check(gw: &Gateway, manifest: &Manifest, options: Options, items: &mut [Item]) {
    if manifest.plugins.is_empty() {
        return;
    }
    let rows = read_rows(gw).await.unwrap_or_default();
    let settled = |item: &Item| {
        item.kind == "plugin"
            && !matches!(item.status, Status::Failed | Status::Blocked)
            && !item.extra.contains_key("check")
    };
    for item in items.iter_mut().filter(|item| settled(item)) {
        let Some(row) = rows.get(&item.name) else {
            continue;
        };
        if row["enabled"] == json!(true) && row["state"] == json!("failed") {
            let error = literal::mask(row["lastError"].as_str().unwrap_or("no detail"));
            item.note("check", json!({ "ok": false, "code": "plugin.start", "error": error }));
        }
    }
    if options.no_check || rows.get("llm").is_none_or(|row| row["state"] != json!("active")) {
        return;
    }
    let Some(item) = items.iter_mut().find(|item| settled(item) && item.name == "llm") else {
        return;
    };
    let names: Vec<String> = manifest
        .plugins
        .get("llm")
        .and_then(|entry| entry.config.as_ref())
        .and_then(|patch| patch["providers"].as_object())
        .map(|providers| providers.keys().cloned().collect())
        .unwrap_or_default();
    if !names.is_empty() {
        item.note("check", check_providers(gw, names).await);
    }
}

/// Every provider's listing, at most a few at a time, in the order of their names.
async fn check_providers(gw: &Gateway, names: Vec<String>) -> Value {
    let mut outcomes: Vec<(String, Result<usize, String>)> = stream::iter(names)
        .map(|name| async move {
            let listing = tokio::time::timeout(CHECK_BOUND, list_models(gw, &name)).await;
            let outcome = listing.unwrap_or_else(|_| Err(format!("no answer after {} s", CHECK_BOUND.as_secs())));
            (name, outcome)
        })
        .buffer_unordered(CHECKS_AT_ONCE)
        .collect()
        .await;
    outcomes.sort_by(|a, b| a.0.cmp(&b.0));
    let failures: Vec<String> = outcomes
        .iter()
        .filter_map(|(name, outcome)| outcome.as_ref().err().map(|error| format!("{name}: {error}")))
        .collect();
    if !failures.is_empty() {
        return json!({ "ok": false, "code": "check.provider", "error": failures.join("; ") });
    }
    let counted: Vec<String> = outcomes
        .iter()
        .filter_map(|(name, outcome)| {
            let count = *outcome.as_ref().ok()?;
            Some(format!("{name}: {count} model{}", if count == 1 { "" } else { "s" }))
        })
        .collect();
    json!({ "ok": true, "detail": counted.join("; ") })
}

/// How many models this provider lists, or why not. Its URL is left out of the answer, because a
/// reference in a base URL has been resolved by the time it is printed.
async fn list_models(gw: &Gateway, provider: &str) -> Result<usize, String> {
    let body = json!({ "provider": provider });
    let answer = gw.send(Method::POST, "/api/llm/providers/models", Some(body)).await.map_err(|error| literal::mask(&error))?;
    if answer["ok"] == json!(true) {
        return Ok(answer["models"].as_array().map_or(0, Vec::len));
    }
    let error = answer["error"].as_str().unwrap_or("no detail");
    Err(literal::mask(&match answer["url"].as_str().filter(|url| !url.is_empty()) {
        Some(url) => error.replace(url, "its endpoint"),
        None => error.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    fn entry(enabled: Option<bool>, config: Option<Value>, secrets: &[&str]) -> PluginEntry {
        PluginEntry { enabled, config, secrets: secrets.iter().map(|s| s.to_string()).collect() }
    }

    fn rows(listed: &[(&str, bool, &str)]) -> Rows {
        listed
            .iter()
            .map(|(id, enabled, state)| (id.to_string(), json!({ "id": id, "enabled": enabled, "state": state })))
            .collect()
    }

    // SPEC §host.setup: "objects merge, arrays and everything else replace".
    #[test]
    fn a_patch_merges_objects_and_replaces_the_rest() {
        let base = json!({ "keep": 1, "map": { "a": { "x": 1 }, "b": 2 }, "list": [1, 2], "text": "old" });
        let patch = json!({ "map": { "a": { "y": 2 }, "c": 3 }, "list": [3], "text": { "now": "object" }, "new": true });
        assert_eq!(
            merge_patch(&base, &patch),
            json!({ "keep": 1, "map": { "a": { "x": 1, "y": 2 }, "b": 2, "c": 3 }, "list": [3], "text": { "now": "object" }, "new": true })
        );
        assert_eq!(merge_patch(&base, &json!({})), base);
        assert_eq!(merge_patch(&json!(null), &json!({ "a": 1 })), json!({ "a": 1 }));
    }

    // SPEC §host.setup: an entry reads `disabled` until the boot reaches it, and `idle` is not a start.
    #[test]
    fn a_boot_is_over_when_no_row_it_owes_is_left() {
        let waiting = rows(&[
            ("a", true, "disabled"),
            ("b", true, "starting"),
            ("c", false, "disabled"),
            ("d", true, "active"),
            ("e", true, "failed"),
            ("f", true, "idle"),
        ]);
        assert_eq!(still_starting(&waiting), ["a", "b"]);
        assert!(still_starting(&rows(&[("c", false, "disabled"), ("d", true, "active")])).is_empty());
    }

    #[test]
    fn a_plugin_is_turned_only_when_it_is_not_as_asked() {
        let on = json!({ "enabled": true, "state": "active" });
        let off = json!({ "enabled": false, "state": "disabled" });
        let failed = json!({ "enabled": true, "state": "failed" });
        assert_eq!(toggle_for(None, &on), None);
        assert_eq!(toggle_for(None, &failed), None);
        assert_eq!(toggle_for(Some(true), &on), None);
        assert_eq!(toggle_for(Some(false), &off), None);
        assert_eq!(toggle_for(Some(true), &off), Some(true));
        assert_eq!(toggle_for(Some(false), &on), Some(false));
        assert_eq!(toggle_for(Some(true), &failed), Some(true), "a rerun retries a failed start");
        assert_eq!(toggle_for(Some(false), &failed), Some(false));
    }

    // SPEC §host.setup: "`executes` names what a `proc` MCP will run ... a job patched into
    // `plugins.jobs` is listed the same way".
    #[test]
    fn a_patch_says_what_it_would_run() {
        let jobs = json!({ "definitions": {
            "backup": { "action": { "type": "process.exec", "input": { "program": "restic", "args": ["backup", "."] } } },
            "old": { "action": { "type": "process.legacy-command", "input": { "command": "nightly.sh --full" } } },
            "quiet": { "action": { "type": "memory.reindex", "input": { "program": "not-a-process" } } },
            "bare": { "maxConcurrentRuns": 1 },
        } });
        assert_eq!(
            Value::Object(executes("jobs", &jobs)),
            json!({ "backup": "restic backup .", "old": "nightly.sh --full" })
        );

        let process = json!({ "apps": [
            { "name": "web", "start": { "program": "node", "args": ["server.js"] }, "stop": { "program": "pkill" } },
            { "name": "dot", "dir": "/srv" },
        ] });
        assert_eq!(
            Value::Object(executes("process", &process)),
            json!({ "web": "node server.js", "web.stop": "pkill" })
        );

        let llm = json!({ "agents": { "claude": { "command": "claude --verbose" }, "lead": { "model": "m" } } });
        assert_eq!(Value::Object(executes("llm", &llm)), json!({ "claude": "claude --verbose" }));

        assert!(executes("tunnels", &json!({ "definitions": jobs["definitions"] })).is_empty());
        let masked = json!({ "agents": { "x": { "command": "tool --token abcd1234" } } });
        assert!(!Value::Object(executes("llm", &masked)).to_string().contains("abcd1234"));
    }

    #[test]
    fn an_id_the_gateway_does_not_have_is_named_and_stops_only_itself() {
        let known = ["jobs", "tunnels"];
        let run = |id: &str| decide(id, &entry(Some(true), None, &[]), None, &known, &BTreeSet::new(), &mut Vec::new());
        let item = run("job");
        assert_eq!(item.status, Status::Failed);
        assert_eq!(item.extra["code"], json!("plugin.unknown"));
        assert!(item.extra["error"].as_str().unwrap_or_default().contains("did you mean jobs?"));
        assert!(run("zzzzzzzz").extra["error"].as_str().unwrap_or_default().contains("there are: jobs, tunnels"));
    }

    // SPEC §host.setup: what a validator refuses is a failed item, and a change it accepts an update.
    #[test]
    fn a_plugin_item_reads_what_the_gateway_said() {
        let row = json!({ "enabled": true, "state": "active" });
        let unsettled: BTreeSet<String> = ["waits".to_string()].into();
        let run = |entry: &PluginEntry, stored: Stored| {
            let mut warnings = Vec::new();
            let item = decide("jobs", entry, Some(&stored), &["jobs"], &unsettled, &mut warnings);
            (item, warnings)
        };
        let plain = entry(None, Some(json!({})), &[]);

        let same = Stored { row: row.clone(), ..Stored::default() };
        assert_eq!(run(&plain, same).0.status, Status::Unchanged);

        let moved = Stored { row: row.clone(), config_changes: true, warnings: vec!["a note".into()], ..Stored::default() };
        let (item, warnings) = run(&plain, moved);
        assert_eq!(item.status, Status::Updated);
        assert_eq!(warnings[0].code, "plugin.warning");
        assert_eq!(warnings[0].path, "plugins.jobs.config");

        let refused = Stored { row: row.clone(), config_changes: true, invalid: Some("no such field".into()), ..Stored::default() };
        let (item, _) = run(&entry(None, None, &["waits"]), refused);
        assert_eq!(item.status, Status::Failed, "a bad patch is worth saying before any secret is found");
        assert_eq!(item.extra["code"], json!("plugin.invalid"));

        let waiting = Stored { row: row.clone(), config_changes: true, ..Stored::default() };
        let (item, _) = run(&entry(None, None, &["waits"]), waiting);
        assert_eq!(item.status, Status::Blocked);
        assert_eq!(item.extra["by"], json!(["secret:waits"]));

        let unasked = Stored { row, config_changes: true, not_validated: true, ..Stored::default() };
        let (item, warnings) = run(&plain, unasked);
        assert_eq!(item.status, Status::Updated);
        assert_eq!(warnings[0].code, "plugin.not-validated");
    }

    #[test]
    fn why_the_mcp_plugin_cannot_answer_names_a_way_to_turn_it_on() {
        assert_eq!(mcp_off_reason(&rows(&[("mcp", true, "active")])), None);
        let off = mcp_off_reason(&rows(&[("mcp", false, "disabled")])).expect("off");
        assert!(off.contains("is off") && off.contains("plugins.mcp.enabled"), "{off}");
        let mut failed = rows(&[("mcp", true, "failed")]);
        failed.get_mut("mcp").expect("a row")["lastError"] = json!("the port is taken");
        let told = mcp_off_reason(&failed).expect("failed");
        assert!(told.contains("the port is taken"), "{told}");
    }

    type Seen = Arc<Mutex<Vec<String>>>;

    /// A gateway that holds one plugin's row and records the method and path of everything it hears.
    async fn fake_gateway() -> (Gateway, Seen) {
        let seen = Seen::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let log = log.clone();
            async move {
                let line = format!("{} {}", request.method(), request.uri());
                log.lock().expect("the log").push(line);
                axum::Json(json!({ "revision": 1, "config": { "a": 1 }, "warnings": [] }))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let port = listener.local_addr().expect("a bound address").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (Gateway::at(port), seen)
    }

    // SPEC §host.setup: an older gateway has no `?dryRun=1` and would save it, so a gateway on another
    // build is not asked, and a plan stays a plan.
    #[tokio::test]
    async fn a_gateway_on_another_build_is_read_and_not_asked_to_validate() {
        let (gw, seen) = fake_gateway().await;
        let manifest = Manifest {
            plugins: [("jobs".to_string(), entry(None, Some(json!({ "b": 2 })), &[]))].into(),
            ..Manifest::default()
        };
        let known = rows(&[("jobs", true, "active")]);

        let named = read_named(&gw, &manifest, &known, false).await.expect("read");
        assert!(named["jobs"].config_changes);
        assert!(named["jobs"].not_validated);
        assert_eq!(*seen.lock().expect("the log"), ["GET /api/plugins/jobs/config"]);

        seen.lock().expect("the log").clear();
        let named = read_named(&gw, &manifest, &known, true).await.expect("read");
        assert!(!named["jobs"].not_validated);
        assert_eq!(
            *seen.lock().expect("the log"),
            ["GET /api/plugins/jobs/config", "PUT /api/plugins/jobs/config?dryRun=1"]
        );
    }
}
