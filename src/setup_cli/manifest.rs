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

//! The manifest: a JSON description of what this machine should have (SPEC §host.setup). Parsing is
//! pure and touches no gateway, so a bad file is refused before anything starts, and it lists every
//! problem at once.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use super::refuse::{join, nearest, refuse_nulls, scan_definition, Problem, Scan};
use super::sources::Source;
use crate::adminapi::{build_def, is_valid_name, DEF_KEYS};
use swiss_core::secure::secretstore::valid_name;
use swiss_host::config::ServerDef;
use swiss_mcp::mcp_import::{entry_to_def, EntryMapping, ENTRY_KEYS};

/// The only version this build reads.
const VERSION: u64 = 1;

/// Every key a manifest's top level takes.
const TOP_LEVEL: [&str; 5] = ["version", "secrets", "mcps", "mcpServers", "servers"];

/// The sections `swiss setup schema` describes, by name.
const SECTIONS: [&str; 2] = ["secrets", "mcps"];

/// The keys that ride on every MCP definition, beside those of its type.
const EVERY_TYPE: [&str; 3] = ["type", "lazy", "idleMs"];

const SECRET_NAME_RULE: &str = "a secret's name is a lowercase letter, then lowercase letters, digits or -, 64 at most";
const MCP_NAME_RULE: &str = "an MCP's name starts with a letter or digit, then letters, digits, _ or -, 63 at most";
const THE_SOURCES: &str = "a secret's entry says where its value is: env:VAR, file:PATH, dotenv:PATH#KEY or ask";

/// Where a secret's value is.
#[derive(Debug, PartialEq, Eq)]
pub enum SecretSpec {
    /// Only a human has it (`swiss secret ask`).
    Ask,
    From(Source),
}

/// One MCP, as the definition builder left it.
#[derive(Debug)]
pub struct McpEntry {
    pub def: ServerDef,
    /// The vault names its definition references.
    pub secrets: BTreeSet<String>,
}

/// Something true of the manifest that does not stop it. Holds a path and a name, never a value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl Warning {
    pub fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Warning { code, path: path.into(), message: message.into() }
    }
}

#[derive(Debug, Default)]
pub struct Manifest {
    pub secrets: BTreeMap<String, SecretSpec>,
    pub mcps: BTreeMap<String, McpEntry>,
    pub warnings: Vec<Warning>,
}

/// Read a manifest, or say everything wrong with it. `gateway_port` is what an entry pointing at
/// this gateway is recognised by.
pub fn parse(text: &str, gateway_port: u16) -> Result<Manifest, Vec<Problem>> {
    let raw: Value = serde_json::from_str(text)
        .map_err(|err| vec![Problem::new("", format!("not JSON: {err}"))])?;
    let Some(top) = raw.as_object() else {
        return Err(vec![Problem::new("", "a manifest is one JSON object")]);
    };
    let mut problems = Vec::new();
    refuse_nulls("", &raw, &mut problems);
    for key in top.keys().filter(|key| !TOP_LEVEL.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(key.clone(), key, TOP_LEVEL));
    }
    if top.get("version").is_some_and(|v| *v != json!(VERSION) && !v.is_null()) {
        problems.push(Problem::new("version", format!("this build reads version {VERSION}")));
    }
    let mut manifest = Manifest::default();
    read_secrets(top.get("secrets"), &mut manifest, &mut problems);
    for block in ["mcps", "mcpServers", "servers"] {
        read_mcps(top, block, gateway_port, &mut manifest, &mut problems);
    }
    if problems.is_empty() {
        Ok(manifest)
    } else {
        Err(problems)
    }
}

fn read_secrets(section: Option<&Value>, manifest: &mut Manifest, problems: &mut Vec<Problem>) {
    let Some(section) = section.filter(|s| !s.is_null()) else {
        return;
    };
    let Some(entries) = section.as_object() else {
        problems.push(Problem::new("secrets", THE_SOURCES));
        return;
    };
    for (name, entry) in entries {
        let path = join("secrets", name);
        if !valid_name(name) {
            problems.push(Problem::new(path.clone(), SECRET_NAME_RULE));
        }
        match entry {
            Value::Null => {}
            Value::String(spec) if spec == "ask" => {
                manifest.secrets.insert(name.clone(), SecretSpec::Ask);
            }
            Value::String(spec) => match Source::parse(spec) {
                Ok(source) => {
                    manifest.secrets.insert(name.clone(), SecretSpec::From(source));
                }
                Err(refusal) => problems.push(Problem::new(path, refusal)),
            },
            _ => problems.push(Problem::new(path, THE_SOURCES)),
        }
    }
}

fn read_mcps(
    top: &Map<String, Value>,
    block: &str,
    gateway_port: u16,
    manifest: &mut Manifest,
    problems: &mut Vec<Problem>,
) {
    let Some(section) = top.get(block).filter(|s| !s.is_null()) else {
        return;
    };
    let Some(entries) = section.as_object() else {
        problems.push(Problem::new(block, "a map of names to definitions"));
        return;
    };
    for (name, entry) in entries {
        let path = join(block, name);
        if !is_valid_name(name) {
            problems.push(Problem::new(path.clone(), MCP_NAME_RULE));
        }
        if manifest.mcps.contains_key(name) {
            problems.push(Problem::new(path.clone(), "this name is defined twice in the manifest"));
            continue;
        }
        if entry.is_null() {
            continue;
        }
        let Some(fields) = entry.as_object() else {
            problems.push(Problem::new(path, "an MCP is an object"));
            continue;
        };
        // A client's file was written for that client: it normalises as an import does, and what
        // that leaves behind is the client's own business. `mcps` is written for swiss, so a key
        // it does not take is a typo.
        let body = if block == "mcps" {
            if !keys_are_known(&path, fields, problems) {
                continue;
            }
            entry.clone()
        } else {
            match client_entry(&path, entry, gateway_port, manifest, problems) {
                Some(body) => body,
                None => continue,
            }
        };
        let mut scan = Scan::default();
        scan_definition(&path, &body, &mut scan);
        if !scan.problems.is_empty() {
            // Not built: what the builder says about a bad value may quote it.
            problems.append(&mut scan.problems);
            continue;
        }
        for (where_, var) in scan.env_refs {
            manifest.warnings.push(Warning::new(
                "ref.env-unverifiable",
                where_,
                format!(
                    "${{{var}}} is read from the gateway's environment, which this command cannot see, and a missing variable expands to empty; ${{secret://name}} is the form to write"
                ),
            ));
        }
        match build_def(&body, &[]) {
            Ok(def) => {
                manifest.mcps.insert(name.clone(), McpEntry { def, secrets: scan.secrets });
            }
            Err(refusal) => problems.push(Problem::new(path, refusal)),
        }
    }
}

/// Every key refused by its own path, where the builder stops at the first.
fn keys_are_known(path: &str, fields: &Map<String, Value>, problems: &mut Vec<Problem>) -> bool {
    let type_name = match fields.get("type") {
        None => "proc",
        Some(Value::String(name)) => name.trim(),
        Some(_) => {
            problems.push(Problem::new(join(path, "type"), "a type is a string"));
            return false;
        }
    };
    let Some(row) = DEF_KEYS.iter().find(|row| row.type_ == type_name) else {
        let known: Vec<&str> = DEF_KEYS.iter().map(|row| row.type_).collect();
        let message = match nearest(type_name, &known) {
            Some(near) => format!("unknown type; did you mean {near}?"),
            None => format!("unknown type; this build takes: {}", known.join(", ")),
        };
        problems.push(Problem::new(join(path, "type"), message));
        return false;
    };
    let known: Vec<&str> = EVERY_TYPE.iter().chain(row.required).chain(row.other).copied().collect();
    let mut clean = true;
    for key in fields.keys().filter(|key| !known.contains(&key.as_str())) {
        // figma's own refusal says why a url does not belong better than a list could.
        if type_name == "figma" && matches!(key.as_str(), "url" | "auth" | "oauthClientName" | "headers") {
            continue;
        }
        problems.push(Problem::unknown_field(join(path, key), key, known.iter().copied()));
        clean = false;
    }
    clean
}

/// One `mcpServers` or `servers` entry as a definition, or None with the reason recorded.
fn client_entry(
    path: &str,
    entry: &Value,
    gateway_port: u16,
    manifest: &mut Manifest,
    problems: &mut Vec<Problem>,
) -> Option<Value> {
    match entry_to_def(entry, gateway_port) {
        EntryMapping::Def(def) => {
            let dropped: Vec<&str> = entry
                .as_object()
                .into_iter()
                .flat_map(|fields| fields.keys())
                .map(String::as_str)
                .filter(|key| !ENTRY_KEYS.contains(key))
                .collect();
            if !dropped.is_empty() {
                manifest.warnings.push(Warning::new(
                    "mcp.keys-dropped",
                    path,
                    format!("a client's own keys, which no definition takes: {}", dropped.join(", ")),
                ));
            }
            Some(Value::Object(def.0))
        }
        EntryMapping::Skip(reason @ "points at this gateway") => {
            manifest.warnings.push(Warning::new("mcp.skipped", path, reason));
            None
        }
        EntryMapping::Skip(reason) => {
            problems.push(Problem::new(path, reason));
            None
        }
    }
}

/// What a manifest may say: the whole of it, or one section. Unknown section: the refusal.
pub fn schema(section: Option<&str>) -> Result<Value, String> {
    let describe = |name: &str| match name {
        "secrets" => json!({
            "identity": "the secret's name",
            "name": SECRET_NAME_RULE,
            "value": "env:VAR | file:PATH | dotenv:PATH#KEY | ask",
            "paths": "relative to the manifest's folder, or the working directory for stdin; ~/ is the home directory",
        }),
        _ => {
            let types: Map<String, Value> = DEF_KEYS
                .iter()
                .map(|row| {
                    (row.type_.to_string(), json!({ "required": row.required, "other": row.other }))
                })
                .collect();
            json!({
                "identity": "the MCP's name",
                "name": MCP_NAME_RULE,
                "aliases": ["mcpServers", "servers"],
                "aliasesNormalise": "a client's own .mcp.json: command and args, or a url, as an import reads them",
                "defaultType": "proc",
                "everyType": EVERY_TYPE,
                "types": types,
            })
        }
    };
    match section {
        None => {
            let sections: Map<String, Value> =
                SECTIONS.iter().map(|name| (name.to_string(), describe(name))).collect();
            Ok(json!({ "ok": true, "version": VERSION, "sections": sections }))
        }
        Some(name) if SECTIONS.contains(&name) => {
            Ok(json!({ "ok": true, "version": VERSION, "section": name, "schema": describe(name) }))
        }
        Some(name) => Err(match nearest(name, &SECTIONS) {
            Some(near) => format!("no such section; did you mean {near}?"),
            None => format!("no such section; there are: {}", SECTIONS.join(", ")),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORT: u16 = 19999;
    const VALUE: &str = "S3ntinelValue9";

    fn parsed(document: Value) -> Result<Manifest, Vec<Problem>> {
        parse(&document.to_string(), PORT)
    }

    fn told(document: Value) -> Vec<(String, String)> {
        parsed(document)
            .expect_err("a refusal")
            .into_iter()
            .map(|p| (p.path, p.message))
            .collect()
    }

    fn command_of(manifest: &Manifest, name: &str) -> String {
        manifest.mcps[name].def.get_str("command").expect("a command").to_string()
    }

    // SPEC §host.setup: "every problem listed, with its path".
    #[test]
    fn a_manifest_lists_every_problem_at_once() {
        let problems = told(json!({
            "version": 2,
            "secret": {},
            "secrets": { "Bad_Name": "env:A", "no-source": "plain text", "whole": 7 },
            "mcps": {
                "db": { "type": "mysql", "pasword": "x", "hots": "y" },
                "bad name": { "command": "a" },
                "typed": { "type": "myqsl" },
                "empty": { "type": "pg" },
                "nul": { "command": "a", "cwd": null },
                "not-an-object": "x",
            },
        }));
        let paths: Vec<&str> = problems.iter().map(|(path, _)| path.as_str()).collect();
        for expected in [
            "version",
            "secret",
            "secrets.Bad_Name",
            "secrets.no-source",
            "secrets.whole",
            "mcps.db.pasword",
            "mcps.db.hots",
            "mcps.bad name",
            "mcps.typed.type",
            "mcps.empty",
            "mcps.nul.cwd",
            "mcps.not-an-object",
        ] {
            assert!(paths.contains(&expected), "{expected} missing from {paths:?}");
        }
        let message = |wanted: &str| {
            problems
                .iter()
                .find(|(path, _)| path == wanted)
                .map(|(_, message)| message.as_str())
                .unwrap_or_default()
        };
        assert_eq!(message("mcps.db.pasword"), "unknown field; did you mean password?");
        assert_eq!(message("secret"), "unknown field; did you mean secrets?");
        assert_eq!(message("mcps.typed.type"), "unknown type; did you mean mysql?");
        assert!(message("mcps.empty").contains("url is required"), "{problems:?}");
        assert!(message("mcps.nul.cwd").contains("null is refused"), "{problems:?}");
    }

    // SPEC §host.setup: a source that makes no sense may be the value, typed in the wrong place.
    #[test]
    fn a_value_where_a_source_belongs_is_not_repeated() {
        let problems = told(json!({ "secrets": { "a-key": VALUE } }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(!problems[0].1.contains(VALUE), "{problems:?}");
    }

    // SPEC §host.setup: the refusal covers a whole manifest, and quotes nothing of it.
    #[test]
    fn a_literal_stops_the_file_without_being_quoted() {
        let problems = told(json!({
            "mcps": {
                "db": { "type": "mysql", "password": VALUE },
                "web": { "type": "http", "url": "https://x.example/", "headers": { "Authorization": format!("Bearer {VALUE}") } },
            },
            "mcpServers": { "tool": { "command": "t", "env": { "API_KEY": VALUE } } },
        }));
        let paths: Vec<&str> = problems.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, ["mcps.db.password", "mcps.web.headers.Authorization", "mcpServers.tool.env.API_KEY"]);
        assert!(!format!("{problems:?}").contains(VALUE), "{problems:?}");
    }

    // SPEC §host.setup: "A client's own `.mcp.json` is a valid manifest".
    #[test]
    fn a_client_mcp_json_is_a_manifest() {
        let manifest = parsed(json!({
            "mcpServers": {
                "playwright": { "type": "stdio", "command": "npx", "args": ["-y", "@playwright/mcp@latest"], "autoApprove": ["x"] },
                "github": { "type": "streamable-http", "url": "https://api.githubcopilot.com/mcp/", "headers": { "Authorization": "Bearer ${secret://gh-token}" } },
                "ourselves": { "url": "http://127.0.0.1:19999/mcp/all" },
            }
        }))
        .expect("a client's file reads");

        assert_eq!(command_of(&manifest, "playwright"), "npx -y @playwright/mcp@latest");
        assert_eq!(manifest.mcps["playwright"].def.type_(), "proc");
        assert_eq!(manifest.mcps["github"].def.type_(), "http");
        assert!(manifest.mcps["github"].secrets.contains("gh-token"));
        assert!(!manifest.mcps.contains_key("ourselves"), "an entry pointing at this gateway is skipped");
        let codes: Vec<(&str, &str)> =
            manifest.warnings.iter().map(|w| (w.code, w.path.as_str())).collect();
        assert_eq!(codes, [("mcp.keys-dropped", "mcpServers.playwright"), ("mcp.skipped", "mcpServers.ourselves")]);

        let servers = parsed(json!({ "servers": { "a": { "command": "a" } } })).expect("servers reads");
        assert_eq!(servers.mcps["a"].def.type_(), "proc");
    }

    // SPEC §host.setup: a name stays exactly what was written, where an import would rewrite it.
    #[test]
    fn a_client_entry_keeps_its_exact_name_or_is_refused() {
        let kept = parsed(json!({ "mcpServers": { "Upper_Name": { "command": "a" } } })).expect("a valid name");
        assert!(kept.mcps.contains_key("Upper_Name"));
        let problems = told(json!({ "mcpServers": { "has.dot": { "command": "a" } } }));
        assert_eq!(problems[0].0, "mcpServers.has.dot");
        let problems = told(json!({ "mcpServers": { "neither": { "cwd": "/" } } }));
        assert!(problems[0].1.contains("needs a command or a url"), "{problems:?}");
    }

    #[test]
    fn one_name_in_two_blocks_is_refused() {
        let problems = told(json!({
            "mcps": { "a": { "command": "a" } },
            "mcpServers": { "a": { "command": "b" } },
        }));
        assert_eq!(problems[0].0, "mcpServers.a");
        assert!(problems[0].1.contains("twice"), "{problems:?}");
    }

    // SPEC §host.setup: "A `${ENV_VAR}` reference is accepted with the warning".
    #[test]
    fn an_environment_reference_is_accepted_with_a_warning() {
        let manifest = parsed(json!({ "mcps": { "db": { "type": "mysql", "password": "${DB_PASS}" } } }))
            .expect("accepted");
        assert_eq!(manifest.warnings.len(), 1, "{:?}", manifest.warnings);
        assert_eq!(manifest.warnings[0].code, "ref.env-unverifiable");
        assert_eq!(manifest.warnings[0].path, "mcps.db.password");
        assert!(manifest.warnings[0].message.contains("${DB_PASS}"));
    }

    #[test]
    fn the_manifest_examples_read() {
        let manifest = parsed(json!({
            "version": 1,
            "secrets": {
                "github-token": "env:GITHUB_TOKEN",
                "glm-key": "dotenv:.env#ZAI_API_KEY",
                "deploy-key-pass": "file:~/.config/acme/pass",
                "prod-db-password": "ask",
            },
            "mcps": {
                "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": { "Authorization": "Bearer ${secret://github-token}" } },
                "playwright": { "command": "npx", "args": ["-y", "@playwright/mcp@latest"] },
                "prod-db": { "type": "mysql", "host": "127.0.0.1", "port": 13306, "user": "reader", "password": "${secret://prod-db-password}", "database": "app" },
            },
        }))
        .expect("the SPEC's own example reads");
        assert_eq!(manifest.secrets["prod-db-password"], SecretSpec::Ask);
        assert_eq!(command_of(&manifest, "playwright"), "npx -y @playwright/mcp@latest");
        assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
        assert!(parse("{}", PORT).is_ok(), "every section is optional");
    }

    #[test]
    fn a_file_that_is_no_manifest_says_so() {
        assert!(parse("not json", PORT).unwrap_err()[0].message.starts_with("not JSON"));
        assert!(parse("[]", PORT).unwrap_err()[0].message.contains("one JSON object"));
    }

    // SPEC §host.setup: one table says which keys a type takes, and the schema prints that table.
    #[test]
    fn the_schema_prints_the_table_the_refusal_reads() {
        let whole = schema(None).expect("the whole schema");
        assert_eq!(whole["version"], json!(1));
        for row in &DEF_KEYS {
            let printed = &whole["sections"]["mcps"]["types"][row.type_];
            assert_eq!(printed["required"], json!(row.required), "{}", row.type_);
            assert_eq!(printed["other"], json!(row.other), "{}", row.type_);
        }
        let one = schema(Some("secrets")).expect("one section");
        assert_eq!(one["section"], json!("secrets"));
        assert!(one["schema"]["value"].as_str().unwrap_or_default().contains("ask"));
        assert!(schema(Some("secret")).unwrap_err().contains("did you mean secrets"));
        assert!(schema(Some("zzzzzzzz")).unwrap_err().contains("there are"));
    }
}
