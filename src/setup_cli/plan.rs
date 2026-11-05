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

//! What the manifest would change on this gateway (SPEC §host.setup). Reads only: `GET`s, a plugin
//! patch put to its validator with `?dryRun=1`, which persists nothing, and a secret's source read
//! and dropped, so a source that cannot be read shows before any write.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use super::manifest::{Manifest, McpEntry, SecretSpec, Warning};
use super::plugins::{self, Rows, Stored};
use super::{machine, remote, tunnels};
use super::report::{Item, Status};
use super::sources::{trim_newline, Source};
use crate::remote_cli::Gateway;
use swiss_host::mask::{mask_command, mask_def};

/// What was asked for beside the manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Rewrite secrets that already exist, from their sources.
    pub rotate_secrets: bool,
    /// Write, but do not check that what was written works.
    pub no_check: bool,
}

/// What the gateway holds of what the manifest names.
#[derive(Debug, Default)]
pub struct State {
    pub machine: machine::MachineState,
    pub tunnels: tunnels::TunnelState,
    pub remote: remote::RemoteState,
    pub secrets: BTreeSet<String>,
    /// Each named plugin the gateway has.
    pub plugins: BTreeMap<String, Stored>,
    /// Every plugin id it has.
    pub plugin_ids: Vec<String>,
    /// Each named MCP that exists, with its masked definition.
    pub mcps: BTreeMap<String, Value>,
    /// The manifest names MCPs and the plugin that holds them is not serving, for this reason.
    pub mcp_off: Option<String>,
}

pub async fn read_state(gw: &Gateway, manifest: &Manifest, rows: &Rows, may_dry_run: bool) -> Result<State, String> {
    let vault = gw.get("/api/secrets").await?;
    let secrets = vault["secrets"]
        .as_array()
        .ok_or("the gateway reported no vault")?
        .iter()
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect();
    let plugins = plugins::read_named(gw, manifest, rows, may_dry_run).await?;
    let plugin_ids = rows.keys().cloned().collect();
    let mcp_off = if manifest.mcps.is_empty() { None } else { plugins::mcp_off_reason(rows) };
    let mcps = if manifest.mcps.is_empty() || mcp_off.is_some() { BTreeMap::new() } else { read_mcps(gw, manifest).await? };
    let machine = machine::read_state(gw, &manifest.machine).await;
    let tunnels = tunnels::read_state(gw, manifest, rows).await?;
    let remote = remote::read_state(gw, manifest, rows).await?;
    Ok(State { secrets, plugins, plugin_ids, mcps, mcp_off, machine, tunnels, remote })
}

/// Each named MCP that exists, with its masked definition. Only a serving mcp plugin can say.
pub async fn read_mcps(gw: &Gateway, manifest: &Manifest) -> Result<BTreeMap<String, Value>, String> {
    let listing = gw.get("/api/mcps").await?;
    let existing: BTreeSet<&str> = listing["mcps"]
        .as_array()
        .ok_or("the gateway reported no MCPs")?
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    let mut mcps = BTreeMap::new();
    for name in manifest.mcps.keys().filter(|name| existing.contains(name.as_str())) {
        let details = gw.get(&format!("/api/mcps/{name}/details")).await?;
        mcps.insert(name.clone(), details["config"].clone());
    }
    Ok(mcps)
}

/// The value this source holds, as `secret set` would store it, or why not. Holds a value only for
/// as long as the caller does; nothing here prints it.
pub fn read_source(source: &Source, base: &Path) -> Result<String, String> {
    let value = trim_newline(source.read(base)?);
    if value.is_empty() {
        return Err(format!("{} is empty, and an empty value is never a secret", source.label()));
    }
    Ok(value)
}

/// Decide every item, in the order they would be applied. `warnings` gains what the decisions turn
/// up.
pub fn decide(
    manifest: &Manifest,
    state: &State,
    base: &Path,
    options: Options,
    warnings: &mut Vec<Warning>,
) -> Vec<Item> {
    let mut items = Vec::new();
    for (name, spec) in &manifest.secrets {
        items.push(decide_secret(name, spec, state.secrets.contains(name), base, options, warnings));
    }
    // A reference the vault cannot answer and the manifest does not provide for is a question for a
    // human too, so it is one item of its own rather than a detail of every MCP using it.
    let provided = |name: &String| manifest.secrets.contains_key(name) || state.secrets.contains(name);
    let mut missing: BTreeSet<&String> = BTreeSet::new();
    for referenced in manifest.plugins.values().map(|entry| &entry.secrets)
        .chain(manifest.mcps.values().map(|entry| &entry.secrets))
        .chain(manifest.connections.values().map(|entry| &entry.secrets)) {
        missing.extend(referenced.iter().filter(|name| !provided(name)));
    }
    for name in missing {
        items.push(
            Item::new("secret", name, Status::NeedsInput)
                .with("code", "secret.missing"),
        );
    }
    let unsettled: BTreeSet<String> = items
        .iter()
        .filter(|item| matches!(item.status, Status::Failed | Status::NeedsInput))
        .map(|item| item.name.clone())
        .collect();
    let known: Vec<&str> = state.plugin_ids.iter().map(String::as_str).collect();
    for (id, entry) in &manifest.plugins {
        items.push(plugins::decide(id, entry, state.plugins.get(id), &known, &unsettled, warnings));
    }
    items.extend(machine::decide(&manifest.machine, &state.machine));
    items.extend(decide_tunnels_and_remote(manifest, &state.tunnels, &state.remote, &unsettled, warnings));
    let turned_on = manifest.plugins.get("mcp").is_some_and(|entry| entry.enabled == Some(true));
    if state.mcp_off.is_some() && turned_on {
        warnings.push(Warning::new(
            "mcp.plugin-off",
            "plugins.mcp",
            "the mcp plugin is off, so what it holds cannot be read here; apply turns it on first and decides each MCP again",
        ));
    }
    for (name, entry) in &manifest.mcps {
        items.push(match &state.mcp_off {
            Some(reason) if !turned_on => decide_mcp(name, entry, None, &BTreeSet::new(), warnings)
                .failed("mcp.plugin-off", reason),
            _ => decide_mcp(name, entry, state.mcps.get(name), &unsettled, warnings),
        });
    }
    items
}

pub(super) fn decide_tunnels_and_remote(
    manifest: &Manifest,
    tunnels: &tunnels::TunnelState,
    remote: &remote::RemoteState,
    unsettled: &BTreeSet<String>,
    warnings: &mut Vec<Warning>,
) -> Vec<Item> {
    for (id, off) in [("tunnels", &tunnels.off), ("remote", &remote.off)] {
        if off.is_some() && manifest.turns_on(id) {
            tunnels::plugin_warning(id, warnings);
        }
    }
    let mut items = tunnels::decide_connections(manifest, tunnels, unsettled, warnings);
    let planned = tunnels::after_connections(manifest, tunnels, &items);
    items.extend(tunnels::decide_rules(manifest, &planned, &items));
    items.extend(remote::decide(manifest, remote, &planned, &items));
    items
}

fn decide_secret(
    name: &str,
    spec: &SecretSpec,
    exists: bool,
    base: &Path,
    options: Options,
    warnings: &mut Vec<Warning>,
) -> Item {
    match spec {
        SecretSpec::Ask if exists => Item::new("secret", name, Status::Unchanged),
        SecretSpec::Ask => Item::new("secret", name, Status::NeedsInput).with("code", "secret.ask"),
        // The vault cannot be read back and a write rebuilds every MCP using the secret, so a name
        // it holds stays as it is until someone asks otherwise.
        SecretSpec::From(_) if exists && !options.rotate_secrets => Item::new("secret", name, Status::Unchanged),
        SecretSpec::From(source) => match read_source(source, base) {
            Ok(_) if exists => Item::new("secret", name, Status::Updated),
            Ok(_) => Item::new("secret", name, Status::Created),
            Err(error) if exists => {
                warnings.push(Warning::new(
                    "secret.not-rotated",
                    format!("secrets.{name}"),
                    format!("kept the stored value, because the source cannot be read: {error}"),
                ));
                Item::new("secret", name, Status::Unchanged)
            }
            Err(error) => Item::new("secret", name, Status::Failed)
                .with("code", "secret.source")
                .with("error", error),
        },
    }
}

pub(super) fn decide_mcp(
    name: &str,
    entry: &McpEntry,
    stored: Option<&Value>,
    unsettled: &BTreeSet<String>,
    warnings: &mut Vec<Warning>,
) -> Item {
    let by: Vec<Value> = entry
        .secrets
        .iter()
        .filter(|secret| unsettled.contains(*secret))
        .map(|secret| json!(format!("secret:{secret}")))
        .collect();
    let status = if !by.is_empty() {
        Status::Blocked
    } else {
        match stored {
            None => Status::Created,
            Some(stored) if read_back_form(entry) == *stored => {
                // A manifest holds no literal, so what the mask hid here is an ordinary argument
                // its wordlist over-matched, like `-p8080:80`. A change there cannot be seen.
                if mask_def(&entry.def).0 != entry.def.0 {
                    warnings.push(Warning::new(
                        "mcp.compare-masked",
                        format!("mcps.{name}"),
                        "the gateway masks part of this definition when it reads it back, so a change to that part would not show here",
                    ));
                }
                Status::Unchanged
            }
            Some(_) => Status::Updated,
        }
    };
    let mut item = Item::new("mcp", name, status);
    if !by.is_empty() {
        item = item.with("by", by);
    }
    // What a manifest would run, so whoever reviews one can read it (SPEC §host.setup). Masked,
    // because the stored form may carry what the panel masks too.
    if entry.def.type_() == "proc" {
        if let Some(command) = entry.def.get_str("command") {
            item = item.with("executes", mask_command(command));
        }
    }
    item
}

/// This entry as the gateway would answer with it: it only ever reads back its masked form.
fn read_back_form(entry: &McpEntry) -> Value {
    Value::Object(mask_def(&entry.def).0)
}

/// A gateway running a build other than this CLI's: an old daemon may not honour what a manifest
/// says, and the next restart would change the answer.
pub fn stale_build(gateway_hash: Option<&str>, own_hash: &str) -> Option<Warning> {
    gateway_hash.filter(|hash| *hash != own_hash).map(|hash| {
        Warning::new(
            "gateway.stale-build",
            "gateway",
            format!("the gateway runs build {hash} and this command is {own_hash}; swiss restart brings them together"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adminapi::build_def;

    fn mcp_entry(body: Value, secrets: &[&str]) -> McpEntry {
        McpEntry {
            def: build_def(&body, &[]).expect("a definition"),
            secrets: secrets.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn manifest(secrets: Vec<(&str, SecretSpec)>, mcps: Vec<(&str, McpEntry)>) -> Manifest {
        Manifest {
            secrets: secrets.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            mcps: mcps.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            ..Manifest::default()
        }
    }

    fn scratch(file: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("swiss-plan-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        std::fs::write(dir.join(file), text).expect("a scratch file");
        dir
    }

    fn statuses(items: &[Item]) -> Vec<(&str, &str, Status)> {
        items.iter().map(|i| (i.kind, i.name.as_str(), i.status)).collect()
    }

    fn from_file(path: &str) -> SecretSpec {
        SecretSpec::From(Source::File(path.to_string()))
    }

    // SPEC §host.setup: "Secrets are not rewritten", and a source's failure names no value.
    #[test]
    fn a_secret_is_decided_from_its_source_and_the_vault() {
        let base = scratch("value", "s3cret-value\n");
        let run = |manifest: &Manifest, stored: &[&str], rotate: bool| {
            let state = State {
                secrets: stored.iter().map(|s| s.to_string()).collect(),
                ..State::default()
            };
            let mut warnings = Vec::new();
            let options = Options { rotate_secrets: rotate, ..Options::default() };
            let items = decide(manifest, &state, &base, options, &mut warnings);
            (items, warnings)
        };
        let readable = manifest(vec![("a", from_file("value"))], vec![]);
        assert_eq!(statuses(&run(&readable, &[], false).0), [("secret", "a", Status::Created)]);
        assert_eq!(statuses(&run(&readable, &["a"], false).0), [("secret", "a", Status::Unchanged)]);
        assert_eq!(statuses(&run(&readable, &["a"], true).0), [("secret", "a", Status::Updated)]);

        let unreadable = manifest(vec![("a", from_file("absent"))], vec![]);
        let (items, _) = run(&unreadable, &[], false);
        assert_eq!(items[0].status, Status::Failed);
        assert_eq!(items[0].extra["code"], json!("secret.source"));
        // Holding the value already, it is not asked for again unless a rotation was.
        assert_eq!(run(&unreadable, &["a"], false).0[0].status, Status::Unchanged);
        let (items, warnings) = run(&unreadable, &["a"], true);
        assert_eq!(items[0].status, Status::Unchanged);
        assert_eq!(warnings[0].code, "secret.not-rotated");

        let ask = manifest(vec![("a", SecretSpec::Ask)], vec![]);
        assert_eq!(run(&ask, &[], false).0[0].status, Status::NeedsInput);
        assert_eq!(run(&ask, &["a"], true).0[0].status, Status::Unchanged);

        let told = format!("{:?}", run(&readable, &[], false));
        assert!(!told.contains("s3cret-value"), "{told}");
    }

    // SPEC §host.setup: "A failed item does not stop the rest; what depends on it is `blocked`".
    #[test]
    fn what_uses_an_unsettled_secret_is_blocked_and_named() {
        let base = scratch("unused", "x");
        let db = || mcp_entry(json!({ "type": "mysql", "password": "${secret://db-pass}" }), &["db-pass"]);
        let free = || mcp_entry(json!({ "command": "tool" }), &[]);
        let decided = |manifest: &Manifest, stored: &[&str]| {
            let state = State {
                secrets: stored.iter().map(|s| s.to_string()).collect(),
                ..State::default()
            };
            decide(manifest, &state, &base, Options::default(), &mut Vec::new())
        };

        // Asked of a human.
        let asked = manifest(vec![("db-pass", SecretSpec::Ask)], vec![("db", db()), ("free", free())]);
        let items = decided(&asked, &[]);
        assert_eq!(
            statuses(&items),
            [
                ("secret", "db-pass", Status::NeedsInput),
                ("mcp", "db", Status::Blocked),
                ("mcp", "free", Status::Created),
            ]
        );
        assert_eq!(items[1].extra["by"], json!(["secret:db-pass"]));

        // Referenced, and provided for by nothing.
        let unnamed = manifest(vec![], vec![("db", db())]);
        let items = decided(&unnamed, &[]);
        assert_eq!(items[0].status, Status::NeedsInput);
        assert_eq!(items[0].extra["code"], json!("secret.missing"));
        assert_eq!(items[1].status, Status::Blocked);

        // A source that cannot be read blocks as well.
        let failed = manifest(vec![("db-pass", from_file("absent"))], vec![("db", db())]);
        assert_eq!(decided(&failed, &[])[1].status, Status::Blocked);

        // The vault already holds it: nothing to wait for.
        assert_eq!(decided(&asked, &["db-pass"])[1].status, Status::Created);
        assert_eq!(decided(&unnamed, &["db-pass"])[0].status, Status::Created);
    }

    // SPEC §host.setup: compared "through `build_def` on one side and the masked read-back on the
    // other".
    #[test]
    fn an_mcp_compares_with_its_masked_read_back() {
        let entry = mcp_entry(json!({ "type": "mysql", "host": "db", "password": "${secret://db-pass}" }), &[]);
        let stored = read_back_form(&entry);
        let mut warnings = Vec::new();
        let mut decide = |stored| decide_mcp("m", &entry, stored, &BTreeSet::new(), &mut warnings).status;
        assert_eq!(decide(Some(&stored)), Status::Unchanged);
        assert_eq!(decide(None), Status::Created);
        let other = json!({ "type": "mysql", "host": "elsewhere", "password": "${secret://db-pass}" });
        assert_eq!(decide(Some(&other)), Status::Updated);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    // SPEC §host.setup: what the mask hides cannot be compared, and the document says so.
    #[test]
    fn a_definition_the_mask_reaches_into_says_a_change_there_would_not_show() {
        let entry = mcp_entry(json!({ "command": "docker run -p8080:80 img" }), &[]);
        let stored = read_back_form(&entry);
        let mut warnings = Vec::new();
        let item = decide_mcp("m", &entry, Some(&stored), &BTreeSet::new(), &mut warnings);
        assert_eq!(item.status, Status::Unchanged);
        assert_eq!(warnings[0].code, "mcp.compare-masked");
        assert_eq!(warnings[0].path, "mcps.m");
    }

    // SPEC §host.setup: "`executes` names what a `proc` MCP will run, in `plan` too".
    #[test]
    fn a_proc_says_what_it_would_run() {
        let proc_entry = mcp_entry(json!({ "command": "npx", "args": ["-y", "@playwright/mcp@latest"] }), &[]);
        let item = decide_mcp("p", &proc_entry, None, &BTreeSet::new(), &mut Vec::new());
        assert_eq!(item.extra["executes"], json!("npx -y @playwright/mcp@latest"));
        let typed = mcp_entry(json!({ "type": "mysql" }), &[]);
        assert!(decide_mcp("t", &typed, None, &BTreeSet::new(), &mut Vec::new()).extra.get("executes").is_none());
    }

    #[test]
    fn a_gateway_on_another_build_is_warned_about() {
        assert!(stale_build(Some("abc"), "abc").is_none());
        assert!(stale_build(None, "abc").is_none());
        let warning = stale_build(Some("old"), "new").expect("a warning");
        assert_eq!(warning.code, "gateway.stale-build");
        assert!(warning.message.contains("old") && warning.message.contains("new"));
    }
}
