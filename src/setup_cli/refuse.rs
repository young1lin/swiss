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

//! What a manifest may not hold (SPEC §host.setup). Every problem is listed at once, with its path,
//! so an Agent fixes them in one turn, and no message ever quotes a value: the value may be the
//! credential being refused.
//!
//! There is no override. A credential-named slot that holds something else is a false positive with
//! a way out: the value goes in the vault like any other, and the slot holds the reference.

use std::collections::BTreeSet;

use serde_json::Value;

use swiss_core::secure::literal::{self, credential_name, typed_in, MASK};
use swiss_core::secure::refs::{references, Named};
use swiss_host::mask::{
    is_scheme_and_reference, is_secret_env_key, is_secret_header_key, SECRET_KEYS,
};

const WRITE_A_REFERENCE: &str =
    "a credential typed as text; add a secrets entry saying where the value is, and write ${secret://name} here";
const THE_MASK: &str =
    "holds the mask `swiss setup show` prints where a credential was, which is not the credential; write ${secret://name} here";
const A_DEFAULT: &str =
    "a reference's default is typed text, and the value it stands in for may be a credential; drop the default";
const MALFORMED: &str =
    "a reference that is not well formed; a secret's name is a lowercase letter, then lowercase letters, digits or -";

/// One thing wrong with a manifest. Holds a path and a sentence, never a value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

impl Problem {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem { path: path.into(), message: message.into() }
    }

    /// A field this section does not take, with the nearest one it does when one is near.
    pub fn unknown_field<'a>(path: String, field: &str, known: impl IntoIterator<Item = &'a str>) -> Self {
        let known: Vec<&str> = known.into_iter().collect();
        let message = match nearest(field, &known) {
            Some(near) => format!("unknown field; did you mean {near}?"),
            None => format!("unknown field; this takes: {}", known.join(", ")),
        };
        Problem::new(path, message)
    }
}

/// `parent.child`, or `child` at the top.
pub fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Levenshtein, case-insensitive: what `pasword` and `password` are apart.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().flat_map(char::to_lowercase).collect();
    let b: Vec<char> = b.chars().flat_map(char::to_lowercase).collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let next = (row[j + 1] + 1).min(row[j] + 1).min(diagonal + cost);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// The closest of `known`, if it is close enough to be what was meant.
pub fn nearest<'a>(wanted: &str, known: &[&'a str]) -> Option<&'a str> {
    let reach = (wanted.chars().count() / 3).max(2);
    known
        .iter()
        .map(|candidate| (distance(wanted, candidate), *candidate))
        .filter(|(apart, _)| *apart <= reach)
        .min_by_key(|(apart, _)| *apart)
        .map(|(_, candidate)| candidate)
}

/// `null` means nothing here: removal is `swiss api DELETE`, so a stale manifest removes nothing.
pub fn refuse_nulls(path: &str, value: &Value, problems: &mut Vec<Problem>) {
    match value {
        Value::Null => problems.push(Problem::new(
            path,
            "null is refused; leave the field out, and remove an item with swiss api DELETE",
        )),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                refuse_nulls(&format!("{path}[{index}]"), item, problems);
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields {
                refuse_nulls(&join(path, key), item, problems);
            }
        }
        _ => {}
    }
}

/// What reading one definition found.
#[derive(Debug, Default)]
pub struct Scan {
    pub problems: Vec<Problem>,
    /// The vault names it references.
    pub secrets: BTreeSet<String>,
    /// Where it references the daemon's environment, and the variable.
    pub env_refs: Vec<(String, String)>,
}

/// Which name stands over a value, which is what makes it a credential's slot.
#[derive(Clone, Copy)]
enum Slot<'a> {
    Field(&'a str),
    Env(&'a str),
    Header(&'a str),
    Item,
}

/// Refuse every literal in one definition, and note the references it makes.
pub fn scan_definition(path: &str, definition: &Value, scan: &mut Scan) {
    walk(path, Slot::Item, definition, scan);
}

fn walk(path: &str, slot: Slot<'_>, value: &Value, scan: &mut Scan) {
    match value {
        Value::String(text) => read_text(path, slot, text, scan),
        // A password that happens to be digits is still a password.
        Value::Number(_) if matches!(slot, Slot::Field(key) if is_credential_field(key)) => {
            scan.problems.push(Problem::new(path, WRITE_A_REFERENCE));
        }
        Value::Array(items) => {
            let before = scan.problems.len();
            for (index, item) in items.iter().enumerate() {
                walk(&format!("{path}[{index}]"), Slot::Item, item, scan);
            }
            // A flag and its value are two words here, which neither word shows by itself.
            let words: Vec<String> = items.iter().filter_map(|i| i.as_str().map(str::to_string)).collect();
            if scan.problems.len() == before {
                if let Some((index, _)) = literal::find_argv(&words) {
                    scan.problems.push(Problem::new(format!("{path}[{index}]"), WRITE_A_REFERENCE));
                }
            }
        }
        Value::Object(fields) => {
            let record = match slot {
                Slot::Field(key) => key,
                _ => "",
            };
            for (key, item) in fields {
                let child = match record {
                    "env" => Slot::Env(key),
                    "headers" => Slot::Header(key),
                    _ => Slot::Field(key),
                };
                walk(&join(path, key), child, item, scan);
            }
        }
        _ => {}
    }
}

fn read_text(path: &str, slot: Slot<'_>, text: &str, scan: &mut Scan) {
    if text.contains(MASK) {
        scan.problems.push(Problem::new(path, THE_MASK));
        return;
    }
    let Ok((named, rest)) = references(text) else {
        scan.problems.push(Problem::new(path, MALFORMED));
        return;
    };
    for reference in named {
        match reference {
            Named::Secret { has_default: true, .. } => {
                scan.problems.push(Problem::new(path, A_DEFAULT));
                return;
            }
            Named::Secret { name, .. } => {
                scan.secrets.insert(name);
            }
            Named::Env(var) => scan.env_refs.push((path.to_string(), var)),
        }
    }
    if holds_a_literal(slot, text, rest.trim()) {
        scan.problems.push(Problem::new(path, WRITE_A_REFERENCE));
    }
}

/// `text` is the whole value; `rest` is what is left of it around its references. The name over a
/// value is one test and the value's own shape another, because a token under an innocent name and
/// a name that says nothing about a token are both common.
fn holds_a_literal(slot: Slot<'_>, text: &str, rest: &str) -> bool {
    let in_a_named_slot = match slot {
        // `auth` names the mode, and `oauth` is the one it takes.
        Slot::Field("auth") if rest == "oauth" => false,
        Slot::Field(key) => is_credential_field(key) && typed_in(rest, 1),
        Slot::Env(key) => is_secret_env_key(key) && env_value_could_be_one(rest),
        Slot::Header(key) => {
            is_secret_header_key(key)
                && !is_scheme_and_reference(&Value::String(text.to_string()))
                && typed_in(rest, 4)
        }
        Slot::Item => false,
    };
    in_a_named_slot || literal::find(text).is_some() || swiss_memory::secrets::scan(rest).is_some()
}

/// The env vocabulary is a substring match (`MAX_TOKENS`, `KEY_FILE`), so a value that plainly is
/// not a credential is let through: a count, a path, or an address with no password in it.
fn env_value_could_be_one(rest: &str) -> bool {
    typed_in(rest, 4)
        && !rest.bytes().all(|b| b.is_ascii_digit())
        && !rest.contains("://")
        && !is_windows_path(rest)
}

/// The paths `typed_in` does not know: `C:\x`, `C:/x`, `.\x`, `..\x`, `~\x`, `\\server\share`.
fn is_windows_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || [".\\", "..\\", "~\\", "\\\\"].iter().any(|prefix| text.starts_with(prefix))
}

fn is_credential_field(key: &str) -> bool {
    SECRET_KEYS.contains(&key.to_ascii_lowercase().as_str()) || credential_name(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    const VALUE: &str = "S3ntinelValue9";

    fn scanned(definition: Value) -> Scan {
        let mut scan = Scan::default();
        scan_definition("mcps.m", &definition, &mut scan);
        scan
    }

    // SPEC §host.setup: "a credential literal in any credential-named field, header, env key, URL
    // password or command argument" is refused, and "the message never quotes the value".
    #[test]
    fn a_literal_credential_is_refused_without_quoting_it() {
        let shapes = [
            json!({ "password": VALUE }),
            json!({ "token": VALUE }),
            json!({ "passphrase": VALUE }),
            json!({ "apiKey": VALUE }),
            json!({ "password": 12345 }),
            json!({ "headers": { "Authorization": format!("Bearer {VALUE}") } }),
            json!({ "headers": { "X-Api-Key": VALUE } }),
            json!({ "env": { "API_KEY": VALUE } }),
            json!({ "env": { "MY_SERVICE_KEY": VALUE } }),
            json!({ "url": format!("redis://u:{VALUE}@10.0.0.1:6379") }),
            json!({ "url": format!("https://x.example/mcp?token={VALUE}") }),
            json!({ "command": format!("server --password={VALUE}") }),
            json!({ "command": format!("server --api-key {VALUE}") }),
            json!({ "command": format!("redis-cli -a {VALUE}") }),
            json!({ "command": format!("mysql -p{VALUE}") }),
            json!({ "args": ["--token", VALUE] }),
            json!({ "args": [format!("--token={VALUE}")] }),
            json!({ "description": "ghp_0123456789abcdef0123456789abcdef0123" }),
            json!({ "password": format!("${{secret://db-pass:{VALUE}}}") }),
            json!({ "password": format!("{VALUE}${{secret://db-pass}}") }),
        ];
        for shape in shapes {
            let scan = scanned(shape.clone());
            assert_eq!(scan.problems.len(), 1, "{shape}: {:?}", scan.problems);
            let told = serde_json::to_string(&scan.problems).expect("problems serialize");
            assert!(!told.contains(VALUE), "{told}");
            assert!(!told.contains("ghp_"), "{told}");
        }
    }

    // SPEC §host.setup: a reference holds no credential, and neither does what merely sits in a
    // slot with a suggestive name.
    #[test]
    fn what_holds_no_credential_passes() {
        let clean = [
            json!({ "password": "${secret://db-pass}" }),
            json!({ "password": "${DB_PASS}" }),
            json!({ "password": "" }),
            json!({ "headers": { "Authorization": "Bearer ${secret://gh-token}" } }),
            json!({ "headers": { "Accept": "application/json" } }),
            json!({ "env": { "MAX_TOKENS": "4096" } }),
            json!({ "env": { "KEY_FILE": "/etc/acme/key" } }),
            json!({ "env": { "KEY_FILE": "C:\\acme\\key" } }),
            json!({ "env": { "TOKEN_URL": "https://auth.example.com/token" } }),
            json!({ "env": { "TOKENIZERS_PARALLELISM": "false" } }),
            json!({ "url": "mysql://u:${secret://db-pass}@10.0.0.1/app" }),
            json!({ "url": "https://api.githubcopilot.com/mcp/" }),
            json!({ "command": "npx -y @playwright/mcp@latest" }),
            json!({ "command": "docker run -p8080:80 img" }),
            json!({ "command": "server --key-file /etc/acme/key --max-tokens 100" }),
            json!({ "command": "server --password ${secret://db-pass}" }),
            json!({ "args": ["--token", "${secret://gh-token}"] }),
            json!({ "auth": "oauth" }),
        ];
        for shape in clean {
            let scan = scanned(shape.clone());
            assert!(scan.problems.is_empty(), "{shape}: {:?}", scan.problems);
        }
    }

    // SPEC §host.setup: "the mask sentinel, which only `setup show` can have produced".
    #[test]
    fn the_mask_is_refused_where_a_credential_was() {
        let scan = scanned(json!({ "password": MASK, "headers": { "Authorization": format!("Bearer {MASK}") } }));
        assert_eq!(scan.problems.len(), 2, "{:?}", scan.problems);
        assert!(scan.problems.iter().all(|p| p.message.contains("setup show")));
    }

    #[test]
    fn a_malformed_reference_is_refused_without_repeating_it() {
        let scan = scanned(json!({ "password": format!("${{secret://Not_A_Name:{VALUE}}}") }));
        assert_eq!(scan.problems.len(), 1);
        assert!(!scan.problems[0].message.contains(VALUE), "{:?}", scan.problems);
    }

    #[test]
    fn the_references_a_definition_makes_are_noted() {
        let scan = scanned(json!({
            "password": "${secret://db-pass}",
            "headers": { "Authorization": "Bearer ${secret://gh-token}" },
            "url": "${ACME_URL}",
        }));
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
        assert_eq!(scan.secrets.iter().map(String::as_str).collect::<Vec<_>>(), ["db-pass", "gh-token"]);
        assert_eq!(scan.env_refs, [("mcps.m.url".to_string(), "ACME_URL".to_string())]);
    }

    #[test]
    fn a_near_miss_names_the_field_meant() {
        assert_eq!(nearest("pasword", &["host", "password", "port"]), Some("password"));
        assert_eq!(nearest("Password", &["password"]), Some("password"));
        assert_eq!(nearest("zzzzzzzz", &["host", "password", "port"]), None);
        let told = Problem::unknown_field("mcps.db.pasword".into(), "pasword", ["host", "password"]);
        assert_eq!(told.message, "unknown field; did you mean password?");
        let told = Problem::unknown_field("mcps.db.zzzzzzzz".into(), "zzzzzzzz", ["host", "password"]);
        assert_eq!(told.message, "unknown field; this takes: host, password");
    }

    #[test]
    fn a_null_anywhere_is_named_by_its_path() {
        let mut problems = Vec::new();
        refuse_nulls("", &json!({ "mcps": { "a": { "args": ["x", null] }, "b": null } }), &mut problems);
        let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["mcps.a.args[1]", "mcps.b"]);
    }
}
