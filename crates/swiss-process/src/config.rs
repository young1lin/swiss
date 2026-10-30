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

//! The `plugins.process.config` schema, version 2 (SPEC §process.apps).
//!
//! Three validation doors over one reader:
//!
//! - [validate_config] for the PUT: one invalid entry refuses the whole save,
//!   and every error names its `apps[i].field`.
//! - [validate_config_for_start] for boot: an invalid entry is dropped with a
//!   warning naming it, and the rest load.
//! - [config_warnings] for what is storable but not runnable NOW: a program
//!   that does not resolve on PATH, or a `dir` that does not exist.
//!
//! Saving config never runs anything. References (`${ENV}`, `${secret://…}`)
//! are stored as written and resolve at each spawn, strictly; nothing here
//! resolves them, so nothing here can leak a value either.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

/// The facts about THIS gateway a config must not collide with: the port it
/// listens on (a declared app port must never be it) and its own image name
/// (never a valid `image` — a 19998 instance must not be able to read, stop or
/// kill 19999 through its own config).
#[derive(Clone)]
pub struct ConfigRules {
    pub gateway_port: u16,
    /// The gateway exe's basename as the platform reports it ("swiss.exe").
    pub gateway_image: String,
}

/// One program invocation, typed argv — never a string to split (SPEC
/// §process.actions).
#[derive(Clone, Debug, PartialEq)]
pub struct CmdSpec {
    pub program: String,
    pub args: Vec<String>,
}

/// One declared app, exactly the config row it came from (plus the `~` in
/// `dir` still unexpanded: expansion is re-done at start, so a moved home is
/// followed, and validation only checks that it CAN expand to an absolute path).
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub name: String,
    pub start: CmdSpec,
    pub stop: Option<CmdSpec>,
    pub dir: Option<String>,
    pub ports: Vec<u16>,
    pub image: Option<String>,
    pub log: Option<String>,
    pub ui: Option<String>,
    pub autostart: bool,
    pub env: BTreeMap<String, String>,
}

/// The whole config: the apps in their declared order, which is the page's
/// order and the start-with-swiss order.
#[derive(Clone, Debug, Default)]
pub struct ProcessConfig {
    pub apps: Vec<AppConfig>,
}

/// The most apps one config may declare (SPEC §process.apps).
pub const MAX_APPS: usize = 64;
/// The most ports one app may declare.
pub const MAX_PORTS: usize = 16;
/// The most args one command may carry.
pub const MAX_ARGS: usize = 64;
/// The most env names one app may declare.
pub const MAX_ENV: usize = 64;

/// `^[a-z0-9-]{1,32}$` — the key in routes, the CLI, the history, and the
/// capture file's name, which is why it is this strict.
fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `^[A-Za-z_][A-Za-z0-9_]*$` — the shape of an env name on every platform
/// this runs on.
fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Expand a leading `~` against the user's home. None when the result would
/// not be absolute — the save is then refused, not repaired.
pub fn expand_dir(dir: &str) -> Option<PathBuf> {
    let expanded = if dir == "~" {
        swiss_core::paths::home_dir()
    } else if let Some(rest) = dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\")) {
        let mut home = swiss_core::paths::home_dir();
        // A literal "~/" or "~\" leaves the empty rest: home itself.
        if !rest.is_empty() {
            home.push(rest);
        }
        home
    } else {
        PathBuf::from(dir)
    };
    expanded.is_absolute().then_some(expanded)
}

/// Does this declared image name match the process that owns a port? No path
/// separators allowed in the declaration; the comparison ignores case, and on
/// Windows a declaration without an extension also matches `<name>.exe` (SPEC
/// §process.apps).
pub fn image_matches(declared: &str, owner: &str) -> bool {
    if declared.is_empty() || declared.contains(['/', '\\']) {
        return false;
    }
    let d = declared.to_ascii_lowercase();
    let o = owner.to_ascii_lowercase();
    if o == d {
        return true;
    }
    cfg!(windows) && !d.contains('.') && format!("{d}.exe") == o
}

fn parse_cmd(value: &Value, field: &str, errs: &mut Vec<String>) -> Option<CmdSpec> {
    // A non-object start/stop (a bare string is the easy typo) is NAMED like every
    // other bad field: a silent None here marked the app unbuildable with no sentence,
    // and the save went through with the app quietly missing - a table that no longer
    // said what the operator wrote.
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => {
            errs.push(format!("{field}: must be an object like {{ \"program\": \"..\", \"args\": [..] }}"));
            return None;
        }
    };
    let program = obj.get("program").and_then(Value::as_str).unwrap_or("");
    if program.is_empty() {
        errs.push(format!("{field}.program: required and must not be empty"));
        return None;
    }
    let mut args = Vec::new();
    if let Some(list) = obj.get("args").and_then(Value::as_array) {
        if list.len() > MAX_ARGS {
            errs.push(format!("{field}.args: at most {MAX_ARGS} entries"));
            return None;
        }
        for arg in list {
            match arg.as_str() {
                Some(arg) => args.push(arg.to_string()),
                None => {
                    errs.push(format!("{field}.args: every entry must be a string"));
                    return None;
                }
            }
        }
    }
    Some(CmdSpec {
        program: program.to_string(),
        args,
    })
}

/// Record one field's verdict: a failed check names the field and marks the
/// entry unbuildable, but the walk continues so every bad field is named.
fn flag(errs: &mut Vec<String>, bad: &mut bool, valid: bool, sentence: String) {
    if !valid {
        errs.push(sentence);
        *bad = true;
    }
}

fn read_app(
    entry: &Value,
    index: usize,
    rules: &ConfigRules,
    errs: &mut Vec<String>,
) -> Option<AppConfig> {
    let obj = match entry.as_object() {
        Some(obj) => obj,
        None => {
            errs.push(format!("apps[{index}]: must be an object"));
            return None;
        }
    };
    let row = format!("apps[{index}]");

    // Every field is validated and EVERY error is named: a save with three bad
    // fields reports three sentences, not the first (SPEC §process.apps).
    // `bad` marks the entry unbuildable while the walk continues.
    let mut bad = false;

    let name = match obj.get("name") {
        Some(Value::String(name)) if valid_name(name) => Some(name.clone()),
        Some(Value::String(_)) => {
            flag(errs, &mut bad, false, format!("{row}.name: must match ^[a-z0-9-]{{1,32}}$"));
            None
        }
        Some(_) => {
            flag(errs, &mut bad, false, format!("{row}.name: must be a string"));
            None
        }
        None => {
            flag(errs, &mut bad, false, format!("{row}.name: required"));
            None
        }
    };

    let start = obj
        .get("start")
        .and_then(|value| parse_cmd(value, &format!("{row}.start"), errs));
    if start.is_none() {
        bad = true;
        if !obj.contains_key("start") {
            errs.push(format!("{row}.start: required"));
        }
    }
    let stop = match obj.get("stop") {
        None => None,
        Some(value) => {
            let parsed = parse_cmd(value, &format!("{row}.stop"), errs);
            if parsed.is_none() {
                bad = true;
            }
            parsed
        }
    };

    let mut ports = Vec::new();
    match obj.get("ports") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            flag(errs, &mut bad, list.len() <= MAX_PORTS, format!("{row}.ports: at most {MAX_PORTS}"));
            for port in list {
                match port
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
                {
                    Some(port) if port == rules.gateway_port => {
                        flag(errs, &mut bad, 
                            false,
                            format!("{row}.ports: {port} is this gateway's own port"),
                        );
                    }
                    Some(port) if ports.contains(&port) => {
                        flag(errs, &mut bad, false, format!("{row}.ports: {port} is declared twice"));
                    }
                    Some(port) => ports.push(port),
                    None => flag(errs, &mut bad, false, format!("{row}.ports: every port is 1-65535")),
                }
            }
        }
        Some(_) => flag(errs, &mut bad, false, format!("{row}.ports: must be a list")),
    }

    let image = match obj.get("image") {
        Some(Value::String(declared)) => {
            flag(errs, &mut bad, 
                !declared.contains(['/', '\\']),
                format!("{row}.image: the executable basename, no path"),
            );
            let refused = declared.eq_ignore_ascii_case("swiss")
                || declared.eq_ignore_ascii_case("swiss.exe")
                || declared.eq_ignore_ascii_case(&rules.gateway_image);
            flag(errs, &mut bad, 
                !refused,
                format!("{row}.image: the gateway's own image is never a valid app image"),
            );
            flag(errs, &mut bad, !ports.is_empty(), format!("{row}.image: refused without ports"));
            Some(declared.clone())
        }
        Some(_) => {
            flag(errs, &mut bad, false, format!("{row}.image: must be a string"));
            None
        }
        None => {
            flag(errs, &mut bad, ports.is_empty(), format!("{row}.image: required when ports is set"));
            None
        }
    };

    let dir = match obj.get("dir") {
        Some(Value::String(dir)) => {
            flag(errs, &mut bad, 
                expand_dir(dir).is_some(),
                format!("{row}.dir: must expand (~ allowed) to an absolute path"),
            );
            Some(dir.clone())
        }
        Some(_) => {
            flag(errs, &mut bad, false, format!("{row}.dir: must be a string"));
            None
        }
        None => None,
    };

    let log = match obj.get("log") {
        Some(Value::String(log)) => {
            // Absolute after ~ expansion, and the wildcards the log route reads
            // by are allowed in the LAST component only: a '*' or '?' anywhere
            // earlier would glob directories, and the newest-match rule (SPEC
            // §process.apps) is about files.
            let ok = expand_dir(log).is_some_and(|path| {
                let parent_wild = path
                    .parent()
                    .map(|p| {
                        p.to_string_lossy().contains('*') || p.to_string_lossy().contains('?')
                    })
                    .unwrap_or(true);
                !parent_wild
            });
            flag(errs, &mut bad, 
                ok,
                format!("{row}.log: an absolute path; * and ? only in the last component"),
            );
            Some(log.clone())
        }
        Some(_) => {
            flag(errs, &mut bad, false, format!("{row}.log: must be a string"));
            None
        }
        None => None,
    };

    let ui = match obj.get("ui") {
        Some(Value::String(ui)) => {
            flag(errs, &mut bad, 
                ui.starts_with("http://") || ui.starts_with("https://"),
                format!("{row}.ui: must be an http:// or https:// URL"),
            );
            Some(ui.clone())
        }
        Some(_) => {
            flag(errs, &mut bad, false, format!("{row}.ui: must be a string"));
            None
        }
        None => None,
    };

    let mut env = BTreeMap::new();
    match obj.get("env") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            flag(errs, &mut bad, map.len() <= MAX_ENV, format!("{row}.env: at most {MAX_ENV} names"));
            for (key, value) in map {
                flag(errs, &mut bad, 
                    valid_env_name(key),
                    format!("{row}.env: {key:?} is not an env name"),
                );
                match value.as_str() {
                    // Literals and ${...} references are both stored as
                    // written; the spawn resolves them, strictly (SPEC
                    // §process.apps).
                    Some(value) => {
                        env.insert(key.clone(), value.to_string());
                    }
                    None => {
                        flag(errs, &mut bad, false, format!("{row}.env: every value must be a string"));
                    }
                }
            }
        }
        Some(_) => flag(errs, &mut bad, false, format!("{row}.env: must be an object")),
    }

    let autostart = obj
        .get("autostart")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if bad {
        return None;
    }
    Some(AppConfig {
        name: name?,
        start: start?,
        stop,
        dir,
        ports,
        image,
        log,
        ui,
        autostart,
        env,
    })
}

/// Read and validate the whole config. Every error names its `apps[i].field`;
/// an empty list means the config is storable. A version-1 config (`{}`) reads
/// as no apps — there is no migration code (SPEC §process.apps).
pub fn read_config(value: &Value, rules: &ConfigRules) -> Result<ProcessConfig, Vec<String>> {
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => return Err(vec!["process config must be an object".to_string()]),
    };
    // Unknown keys are refused like every plugin config: a typo fails at save.
    if let Some(key) = obj.keys().find(|k| k.as_str() != "apps") {
        return Err(vec![format!("process config has no {key:?} field")]);
    }
    let list = match obj.get("apps") {
        None => return Ok(ProcessConfig::default()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(vec!["apps: must be a list".to_string()]),
    };
    if list.len() > MAX_APPS {
        return Err(vec![format!("apps: at most {MAX_APPS} entries")]);
    }

    let mut errs = Vec::new();
    let mut apps = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        if let Some(app) = read_app(entry, index, rules, &mut errs) {
            // Uniqueness across entries: names and ports both belong to one app.
            if apps.iter().any(|a: &AppConfig| a.name == app.name) {
                errs.push(format!("apps[{index}].name: {} is declared twice", app.name));
                continue;
            }
            let shared = apps.iter().find_map(|a| {
                a.ports
                    .iter()
                    .find(|p| app.ports.contains(p))
                    .map(|p| (a.name.as_str(), *p))
            });
            if let Some((owner, port)) = shared {
                errs.push(format!(
                    "apps[{index}].ports: {port} belongs to {owner}"
                ));
                continue;
            }
            apps.push(app);
        }
    }
    if errs.is_empty() {
        Ok(ProcessConfig { apps })
    } else {
        Err(errs)
    }
}

/// The PUT's door: the whole save is refused when anything is invalid.
pub fn validate_config(value: &Value, rules: &ConfigRules) -> Result<(), String> {
    read_config(value, rules).map(|_| ()).map_err(|errs| errs.join("; "))
}

/// The boot's door: invalid entries are dropped with a warning naming them, and
/// the rest load (SPEC §process.apps).
pub fn validate_config_for_start(
    value: &Value,
    rules: &ConfigRules,
) -> (ProcessConfig, Vec<String>) {
    match read_config(value, rules) {
        Ok(config) => (config, Vec::new()),
        Err(errs) => {
            // Boot keeps what the PUT would refuse: an entry survives when it
            // reads cleanly on its own AND no error names its index (a
            // cross-entry clash or a duplicate name names the later entry, so
            // the earlier one keeps its port and its name).
            let list = value
                .as_object()
                .and_then(|o| o.get("apps"))
                .and_then(Value::as_array);
            let mut apps = Vec::new();
            if let Some(list) = list {
                for (index, entry) in list.iter().enumerate() {
                    let named = errs.iter().any(|e| e.starts_with(&format!("apps[{index}]")));
                    if named {
                        continue;
                    }
                    let mut one = Vec::new();
                    if let Some(app) = read_app(entry, index, rules, &mut one) {
                        apps.push(app);
                    }
                }
            }
            (ProcessConfig { apps }, errs)
        }
    }
}

/// What is storable but not runnable NOW (SPEC §process.apps): a `program`
/// that does not resolve on the spawn PATH, or a `dir` that does not exist.
/// Pure over the seams it is given, so the unit tests need no machine.
pub fn config_warnings(config: &ProcessConfig, program_resolves: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut warnings = Vec::new();
    for app in &config.apps {
        if !program_resolves(&app.start.program) {
            warnings.push(format!(
                "{}: start.program {} does not resolve on PATH",
                app.name, app.start.program
            ));
        }
        if let Some(stop) = &app.stop {
            if !program_resolves(&stop.program) {
                warnings.push(format!(
                    "{}: stop.program {} does not resolve on PATH",
                    app.name, stop.program
                ));
            }
        }
        if let Some(dir) = &app.dir {
            if let Some(expanded) = expand_dir(dir) {
                if !expanded.is_dir() {
                    warnings.push(format!(
                        "{}: dir {} does not exist",
                        app.name,
                        expanded.display()
                    ));
                }
            }
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules() -> ConfigRules {
        ConfigRules {
            gateway_port: 19999,
            gateway_image: "swiss.exe".to_string(),
        }
    }

    fn cc_otel() -> Value {
        json!({
            "name": "cc-otel",
            "start": { "program": "cc-otel", "args": ["start"] },
            "stop": { "program": "cc-otel", "args": ["stop"] },
            "ports": [4317, 4318],
            "image": "cc-otel.exe",
            "ui": "http://127.0.0.1:8899/",
            "autostart": true
        })
    }

    /// A start/stop that is not an object (a bare string is the easy typo) names its
    /// field and refuses the WHOLE save: the app is never silently dropped from the
    /// table, so what runs stays what was written (SPEC §process.apps).
    #[test]
    fn a_non_object_start_or_stop_names_the_field_and_refuses_the_save() {
        let mixed = json!({ "apps": [
            { "name": "good", "start": { "program": "x" } },
            { "name": "typo", "start": "npm run dev" },
            { "name": "stoptypo", "start": { "program": "x" }, "stop": "taskkill /im node" }
        ]});
        let errs = read_config(&mixed, &rules()).unwrap_err();
        let joined = errs.join("\n");
        assert!(joined.contains("apps[1].start"), "{joined}");
        assert!(joined.contains("apps[2].stop"), "{joined}");
        // The same shape with the typos repaired saves whole - including the app that
        // sat beside them.
        let fixed = json!({ "apps": [
            { "name": "good", "start": { "program": "x" } },
            { "name": "typo", "start": { "program": "npm", "args": ["run", "dev"] } },
            { "name": "stoptypo", "start": { "program": "x" }, "stop": { "program": "taskkill" } }
        ]});
        assert_eq!(read_config(&fixed, &rules()).unwrap().apps.len(), 3);
    }

    #[test]
    fn every_field_is_validated_and_named_by_index() {
        let bad = json!({ "apps": [
            { "name": "UPPER", "start": { "program": "" }, "ports": [0], "image": "dir/x.exe",
              "dir": "relative", "log": r"C:\logs\*.log\x", "ui": "ftp://x", "env": { "1BAD": "x" } },
            { "name": "ok", "start": { "program": "x", "args": ["a", 5] } },
            { "name": "ok", "start": { "program": "x" }, "dir": 7 },
            { "name": "docs", "start": { "program": "x" }, "ui": "http://127.0.0.1/" },
            { "name": "docs", "start": { "program": "x" }, "ui": "http://127.0.0.1/" }
        ]});
        let errs = read_config(&bad, &rules()).unwrap_err();
        let joined = errs.join("\n");
        // Every field this config gets wrong is named with its index.
        for needle in [
            "apps[0].name",
            "apps[0].start.program",
            "apps[0].ports",
            "apps[0].image",
            "apps[0].dir",
            "apps[0].log",
            "apps[0].ui",
            "apps[0].env",
            "apps[1].start.args",
            "apps[2].dir",
        ] {
            assert!(joined.contains(needle), "missing {needle} in\n{joined}");
        }
        // And the second entry's duplicate name is an error too.
        assert!(joined.contains("declared twice"), "{joined}");
    }

    #[test]
    fn the_gateways_own_port_and_the_swiss_image_are_refused() {
        let with_port = json!({ "apps": [
            { "name": "evil", "start": { "program": "x" }, "ports": [19999], "image": "node.exe" }
        ]});
        let errs = read_config(&with_port, &rules()).unwrap_err().join("; ");
        assert!(errs.contains("this gateway's own port"), "{errs}");

        let with_image = json!({ "apps": [
            { "name": "evil", "start": { "program": "x" }, "ports": [8080], "image": "SWISS.EXE" },
            { "name": "also", "start": { "program": "x" }, "ports": [8081], "image": "swiss" }
        ]});
        let errs = read_config(&with_image, &rules()).unwrap_err().join("; ");
        assert!(errs.contains("gateway's own image"), "{errs}");
    }

    #[test]
    fn a_port_belongs_to_one_app() {
        let clash = json!({ "apps": [
            { "name": "one", "start": { "program": "x" }, "ports": [8080, 8081], "image": "a.exe" },
            { "name": "two", "start": { "program": "y" }, "ports": [8081, 8082], "image": "b.exe" }
        ]});
        let errs = read_config(&clash, &rules()).unwrap_err().join("; ");
        assert!(errs.contains("8081 belongs to one"), "{errs}");
        // The clean pair reads fine.
        let ok = json!({ "apps": [
            { "name": "one", "start": { "program": "x" }, "ports": [8080], "image": "a.exe" },
            { "name": "two", "start": { "program": "y" }, "ports": [8081], "image": "b.exe" }
        ]});
        assert_eq!(read_config(&ok, &rules()).unwrap().apps.len(), 2);
    }

    #[test]
    fn image_without_ports_is_refused() {
        let no_ports = json!({ "apps": [
            { "name": "odd", "start": { "program": "x" }, "image": "a.exe" }
        ]});
        let errs = read_config(&no_ports, &rules()).unwrap_err().join("; ");
        assert!(errs.contains("refused without ports"), "{errs}");

        let no_image = json!({ "apps": [
            { "name": "odd", "start": { "program": "x" }, "ports": [8080] }
        ]});
        let errs = read_config(&no_image, &rules()).unwrap_err().join("; ");
        assert!(errs.contains("required when ports"), "{errs}");
    }

    #[test]
    fn a_non_http_ui_is_refused() {
        for bad in ["ftp://x", "localhost:8080", "file:///x"] {
            let app = json!({ "apps": [
                { "name": "a", "start": { "program": "x" }, "ui": bad }
            ]});
            let errs = read_config(&app, &rules()).unwrap_err().join("; ");
            assert!(errs.contains("apps[0].ui"), "{errs} for {bad}");
        }
        let ok = json!({ "apps": [
            { "name": "a", "start": { "program": "x" }, "ui": "https://127.0.0.1:9/" }
        ]});
        assert!(read_config(&ok, &rules()).is_ok());
    }

    #[test]
    fn boot_drops_an_invalid_app_and_keeps_the_rest() {
        let mixed = json!({ "apps": [
            cc_otel(),
            { "name": "bad", "start": { "program": "" } },
            { "name": "docs", "start": { "program": "npx", "args": ["serve"] }, "ports": [5173], "image": "node.exe" }
        ]});
        let (config, warnings) = validate_config_for_start(&mixed, &rules());
        assert_eq!(
            config.apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["cc-otel", "docs"]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("apps[1].start.program"), "{warnings:?}");
    }

    #[test]
    fn a_version_1_empty_config_reads_as_no_apps() {
        let (config, warnings) = validate_config_for_start(&json!({}), &rules());
        assert!(config.apps.is_empty());
        assert!(warnings.is_empty());
        // The old shape is accepted by the PUT door too, with the same meaning.
        assert!(read_config(&json!({}), &rules()).unwrap().apps.is_empty());
    }

    #[test]
    fn image_matching_ignores_case_and_a_missing_exe_suffix_on_windows() {
        assert!(image_matches("cc-otel.exe", "CC-OTEL.exe"));
        if cfg!(windows) {
            assert!(image_matches("cc-otel", "cc-otel.exe"));
            // A declared extension never matches a different one.
            assert!(!image_matches("cc-otel.exe", "cc-otel.com"));
        } else {
            assert!(!image_matches("cc-otel", "cc-otel.exe"));
        }
        assert!(!image_matches("dir/x.exe", "x.exe"), "no separators");
    }

    #[test]
    fn config_warnings_name_the_unrunnable_parts() {
        let config = read_config(
            &json!({ "apps": [
                cc_otel(),
                { "name": "docs", "start": { "program": "definitely-not-a-program" },
                  "dir": "~/definitely-not-a-dir" }
            ]}),
            &rules(),
        )
        .unwrap();
        let warnings = config_warnings(&config, &|program| program != "definitely-not-a-program");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("does not resolve on PATH"), "{warnings:?}");
        assert!(warnings[1].contains("does not exist"), "{warnings:?}");
    }
}
