//! Secret masking for the admin panel — port of `mask.ts`.
//!
//! The panel has to show and edit a server definition, and a definition can hold a live DB
//! password. So secrets go out as a sentinel, and a PUT that sends the sentinel back unchanged
//! keeps whatever was stored — the browser never receives the credential, and editing an
//! unrelated field cannot destroy it. `${ENV_VAR}` references are not secrets (the value lives
//! in the sealed env store) and pass through as-is.
//!
//! Every matcher here is a hand-written scanner — ADR-007 keeps the regex engine out of a
//! process whose entire budget is ~15 MB.

use serde_json::{Map, Value};

use crate::config::{is_env_ref, ServerDef};

pub const MASK: &str = "••••••••";

/// Whole-key match: exactly one of these names, case-insensitive.
const SECRET_KEYS: [&str; 5] = ["password", "pass", "passphrase", "secret", "token"];

/// Substring wordlist for env VAR names. An env var is named by someone who knows what it holds,
/// so the list is broader than the definition-key one.
fn is_secret_env_key(name: &str) -> bool {
    contains_any_ci(name, &["pass", "secret", "token", "key", "credential"])
}

/// Substring wordlist for header names. `auth` is the one that matters: a remote MCP's API key
/// travels in `Authorization`. Over-matching is harmless — a masked value the panel sends back
/// is restored from what is stored.
fn is_secret_header_key(name: &str) -> bool {
    contains_any_ci(
        name,
        &[
            "auth",
            "api key",
            "api-key",
            "apikey",
            "token",
            "secret",
            "credential",
            "cookie",
        ],
    )
}

/// THE one substring wordlist for "does this ARGUMENT KEY name carry a credential", shared by
/// the persisted call log and the traffic log. `key` alone is deliberately NOT a plain substring
/// here (it would over-redact names like tokenEnv); env VAR names keep their own broader list.
pub fn is_secret_arg_key(name: &str) -> bool {
    contains_any_ci(
        name,
        &[
            "password",
            "passwd",
            "passphrase",
            "secret",
            "token",
            "credential",
            "authorization",
            "api key",
            "api-key",
            "api_key",
            "apikey",
        ],
    )
}

fn is_url_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "url" | "connectionstring" | "dsn" | "proxy"
    )
}

/// Case-insensitive substring test against a wordlist (the two-char variants cover the regex's
/// `api[_-]?key` shape).
fn contains_any_ci(haystack: &str, words: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    words.iter().any(|w| lower.contains(w))
}

// --- URLs: scheme://user:password@host/... ------------------------------------------------------

/// Split `scheme://user:password@rest` — returns (head, password) where head is everything up to
/// and including the colon before the password, or None when there is no password part. The
/// shape the Node regex pinned: scheme starts with a letter, then [A-Za-z0-9+.-]*, "://",
/// a userinfo run with no ':', '@', '/' or whitespace, then ':' password '@'.
pub fn split_url_password(url: &str) -> Option<(String, String)> {
    let bytes = url.as_bytes();
    // scheme: one letter then [alnum + . -]*
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let mut i = 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'+' | b'.' | b'-'))
    {
        i += 1;
    }
    if !url[i..].starts_with("://") {
        return None;
    }
    i += 3;
    // userinfo: no ':', '@', '/', whitespace
    while i < bytes.len()
        && !matches!(bytes[i], b':' | b'@' | b'/')
        && !bytes[i].is_ascii_whitespace()
    {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    let head_end = i; // exclusive end of "scheme://user"
    i += 1; // the ':'
    let pass_start = i;
    // password: no '@', '/', whitespace (may be empty)
    while i < bytes.len() && bytes[i] != b'@' && bytes[i] != b'/' && !bytes[i].is_ascii_whitespace()
    {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'@' {
        return None;
    }
    let password = url[pass_start..i].to_string();
    Some((url[..head_end + 1].to_string(), password))
}

pub fn mask_url(url: &str) -> String {
    match split_url_password(url) {
        Some((head, pass)) if !pass.is_empty() => format!(
            "{head}{MASK}@{}",
            &url[url.find('@').map(|i| i + 1).unwrap_or(url.len())..]
        ),
        Some((head, _)) => format!(
            "{head}@{}",
            &url[url.find('@').map(|i| i + 1).unwrap_or(url.len())..]
        ),
        None => url.to_string(),
    }
}

pub fn url_password(url: &str) -> Option<String> {
    split_url_password(url).map(|(_, p)| p)
}

/// Put a stored URL's password back into an edited URL that still carries the sentinel.
fn unmask_url(next: &str, current: Option<&Value>) -> String {
    if url_password(next).as_deref() != Some(MASK) {
        return next.to_string();
    }
    let Some(current_pass) = current.and_then(Value::as_str).and_then(url_password) else {
        return next.to_string();
    };
    match split_url_password(next) {
        Some((head, _)) => {
            let rest = &next[next.find('@').map(|i| i + 1).unwrap_or(next.len())..];
            format!("{head}{current_pass}@{rest}")
        }
        None => next.to_string(),
    }
}

// --- command lines -------------------------------------------------------------------------------

/// One credential slot on a command line: the flag plus its separator (`--token=`, `--api-key `,
/// `-p`) and the value that follows. The regex this replaces had global, case-insensitive scan
/// semantics; the scanner walks the same shape.
struct CmdSlot {
    /// Byte range of the flag + separator ("--token=", "--api-key ", "-p").
    head: (usize, usize),
    /// Byte range of the value (a non-space run).
    value: (usize, usize),
}

/// Scan a command string for credential-looking flags. A flag name "carries a credential" when
/// it contains one of the words (case-insensitive) and ends in '=' or one space before its
/// value; `-p` immediately followed by a non-space is the classic mysql-client password form.
fn scan_command(cmd: &str) -> Vec<CmdSlot> {
    let bytes = cmd.as_bytes();
    let mut slots = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'-' {
            i += 1;
            continue;
        }
        let dashes = if i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            2
        } else {
            1
        };
        let name_start = i + dashes;
        let mut j = name_start;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
            j += 1;
        }
        let name = &cmd[name_start..j];
        let name_ci = name.to_ascii_lowercase();
        let carries = ["pass", "secret", "token", "key", "credential"]
            .iter()
            .any(|w| name_ci.contains(w));
        if carries && j < bytes.len() && (bytes[j] == b'=' || bytes[j] == b' ') {
            let head_end = j + 1;
            if head_end < bytes.len() && !bytes[head_end].is_ascii_whitespace() {
                let mut v = head_end;
                while v < bytes.len() && !bytes[v].is_ascii_whitespace() {
                    v += 1;
                }
                slots.push(CmdSlot {
                    head: (i, head_end),
                    value: (head_end, v),
                });
                i = v;
                continue;
            }
        } else if dashes == 1
            && bytes.get(name_start) == Some(&b'p')
            && name_start + 1 < bytes.len()
            && !bytes[name_start + 1].is_ascii_whitespace()
        {
            // The classic mysql-client password form: `-phunter2` — and, matching the Node
            // regex exactly, ANY single-dash flag starting with `p` (`-port x` too), because
            // the regex's second branch was `-p(?=\S)` with no word boundary after the p.
            let mut v = name_start + 1;
            while v < bytes.len() && !bytes[v].is_ascii_whitespace() {
                v += 1;
            }
            slots.push(CmdSlot {
                head: (i, name_start + 1),
                value: (name_start + 1, v),
            });
            i = v;
            continue;
        }
        i = j.max(i + 1);
    }
    slots
}

/// Mask credentials sitting on a command line: `--token=xyz`, `--api-key xyz`, `-phunter2`.
/// `${ENV}`-ref values pass through — the secret is not in the command.
pub fn mask_command(cmd: &str) -> String {
    let slots = scan_command(cmd);
    if slots.is_empty() {
        return cmd.to_string();
    }
    let mut out = String::with_capacity(cmd.len());
    let mut pos = 0;
    for slot in &slots {
        out.push_str(&cmd[pos..slot.head.1]);
        let value = &cmd[slot.value.0..slot.value.1];
        if is_env_ref(&Value::String(value.to_string())) {
            out.push_str(value);
        } else {
            out.push_str(MASK);
        }
        pos = slot.value.1;
    }
    out.push_str(&cmd[pos..]);
    out
}

/// Put stored command-line credentials back, matching them up in order, so the rest of the
/// command stays editable. A slot with nothing behind it keeps the sentinel and is dropped by
/// drop_sentinel.
fn unmask_command(next: &str, current: Option<&Value>) -> String {
    if !next.contains(MASK) {
        return next.to_string();
    }
    let stored: Vec<String> = current
        .and_then(Value::as_str)
        .map(|c| {
            scan_command(c)
                .iter()
                .map(|s| c[s.value.0..s.value.1].to_string())
                .collect()
        })
        .unwrap_or_default();
    let slots = scan_command(next);
    if slots.is_empty() {
        return next.to_string();
    }
    let mut out = String::with_capacity(next.len());
    let mut pos = 0;
    for (idx, slot) in slots.iter().enumerate() {
        out.push_str(&next[pos..slot.head.1]);
        let value = &next[slot.value.0..slot.value.1];
        if value != MASK {
            out.push_str(value);
        } else {
            match stored.get(idx) {
                Some(was) => out.push_str(was),
                None => out.push_str(MASK),
            }
        }
        pos = slot.value.1;
    }
    out.push_str(&next[pos..]);
    out
}

// --- records (env / headers) ---------------------------------------------------------------------

fn mask_record(rec: &Map<String, Value>, is_secret: fn(&str) -> bool) -> Map<String, Value> {
    rec.iter()
        .map(|(k, v)| {
            let masked =
                !is_env_ref(v) && is_secret(k) && v.as_str().is_some_and(|s| !s.is_empty());
            (
                k.clone(),
                if masked {
                    Value::String(MASK.into())
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}

/// Definition fields holding a name→value map whose values can be credentials: a proc MCP's
/// `env` and an http MCP's request `headers`.
fn record_matcher(key: &str) -> Option<fn(&str) -> bool> {
    match key {
        "env" => Some(is_secret_env_key),
        "headers" => Some(is_secret_header_key),
        _ => None,
    }
}

/// A definition safe to send to the browser: every secret replaced by the sentinel.
pub fn mask_def(def: &ServerDef) -> ServerDef {
    let mut out = Map::new();
    for (k, v) in &def.0 {
        if is_env_ref(v) {
            out.insert(k.clone(), v.clone());
            continue;
        }
        let lower = k.to_ascii_lowercase();
        if SECRET_KEYS.contains(&lower.as_str()) {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    out.insert(k.clone(), Value::String(MASK.into()));
                    continue;
                }
            }
        }
        if is_url_key(k) {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), Value::String(mask_url(s)));
                continue;
            }
        }
        if k == "command" {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), Value::String(mask_command(s)));
                continue;
            }
        }
        if let Some(matcher) = record_matcher(k) {
            if let Some(rec) = v.as_object() {
                out.insert(k.clone(), Value::Object(mask_record(rec, matcher)));
                continue;
            }
        }
        out.insert(k.clone(), v.clone());
    }
    ServerDef(out)
}

/// Delete anything still carrying the sentinel after restoration. A value that survives with
/// dots in it is one there was nothing to restore from — the panel's type dropdown submits a
/// `url` against a stored mysql def that never had one, and a stored `password` has no
/// counterpart under the new shape. Persisting `••••••••` as the credential would replace a
/// working secret with dots and report 200; dropping the field instead leaves it unset.
fn drop_sentinel(out: &mut Map<String, Value>) {
    let keys: Vec<String> = out.keys().cloned().collect();
    for k in keys {
        // A top-level string still carrying dots is dropped whole.
        if out
            .get(&k)
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains(MASK))
        {
            out.remove(&k);
            continue;
        }
        // Inside env/headers records, only the offending entries go.
        if let Some(Value::Object(rec)) = out.get_mut(&k) {
            if record_matcher(&k).is_some() {
                let inner: Vec<String> = rec.keys().cloned().collect();
                for ek in inner {
                    if rec
                        .get(&ek)
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.contains(MASK))
                    {
                        rec.remove(&ek);
                    }
                }
            }
        }
    }
}

/// Restore masked values in an incoming edit from the definition currently stored.
pub fn unmask_body(body: &Map<String, Value>, current: Option<&ServerDef>) -> Map<String, Value> {
    let mut out = body.clone();
    if let Some(current) = current {
        for (k, v) in out.clone() {
            let lower = k.to_ascii_lowercase();
            if SECRET_KEYS.contains(&lower.as_str()) && v.as_str() == Some(MASK) {
                if let Some(stored) = current.0.get(&k) {
                    out.insert(k, stored.clone());
                    continue;
                }
            }
            if is_url_key(&k) {
                if let Some(next) = v.as_str() {
                    out.insert(
                        k.clone(),
                        Value::String(unmask_url(next, current.0.get(&k))),
                    );
                }
                continue;
            }
            if k == "command" {
                if let Some(next) = v.as_str() {
                    out.insert(
                        k.clone(),
                        Value::String(unmask_command(next, current.0.get(&k))),
                    );
                }
                continue;
            }
            if record_matcher(&k).is_some() {
                if let Some(next_rec) = v.as_object() {
                    let stored = current
                        .0
                        .get(&k)
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let mut merged = next_rec.clone();
                    for (ek, ev) in next_rec {
                        if ev.as_str() == Some(MASK) {
                            if let Some(was) = stored.get(ek) {
                                merged.insert(ek.clone(), was.clone());
                            }
                        }
                        let _ = ev;
                    }
                    out.insert(k, Value::Object(merged));
                }
            }
        }
    }
    drop_sentinel(&mut out);
    out
}

#[cfg(test)]
mod tests {
    // Ported from test/mask.test.ts.
    use super::*;
    use serde_json::json;

    fn mask(v: Value) -> Value {
        serde_json::to_value(mask_def(&ServerDef(v.as_object().cloned().unwrap()))).unwrap()
    }

    #[test]
    fn masks_exact_secret_keys() {
        let out = mask(json!({ "password": "hunter2", "user": "root", "type": "mysql" }));
        assert_eq!(out["password"], json!(MASK));
        assert_eq!(out["user"], json!("root"));
        // A `pass` key is caught by the same whole-key list.
        let out = mask(json!({ "PASS": "x" }));
        assert_eq!(out["PASS"], json!(MASK));
    }

    #[test]
    fn empty_secrets_stay_empty_not_masked() {
        let out = mask(json!({ "password": "" }));
        assert_eq!(out["password"], json!(""));
    }

    #[test]
    fn env_refs_pass_through() {
        let out = mask(json!({ "password": "${DB_PASS}", "token": "${API_TOKEN}" }));
        assert_eq!(out["password"], json!("${DB_PASS}"));
        assert_eq!(out["token"], json!("${API_TOKEN}"));
    }

    #[test]
    fn url_passwords_are_masked_in_place() {
        let out = mask(json!({ "url": "postgres://root:hunter2@db.local:5432/x" }));
        assert_eq!(
            out["url"],
            json!("postgres://root:••••••••@db.local:5432/x")
        );
        // dsn/connectionstring/proxy share the treatment.
        let out = mask(json!({ "dsn": "mysql://u:pw@h/d" }));
        assert_eq!(out["dsn"], json!("mysql://u:••••••••@h/d"));
        // No password → untouched.
        let out = mask(json!({ "url": "postgres://db.local/x" }));
        assert_eq!(out["url"], json!("postgres://db.local/x"));
    }

    #[test]
    fn command_line_credentials_are_masked() {
        let out = mask(
            json!({ "command": "server --token=abc123 --api-key sk-1 -phunter2 --port 8080" }),
        );
        assert_eq!(
            out["command"],
            json!("server --token=•••••••• --api-key •••••••• -p•••••••• --port 8080")
        );
        // An ${ENV} ref on the command line is not a secret.
        let out = mask(json!({ "command": "server --token=${SVC_TOKEN} run" }));
        assert_eq!(out["command"], json!("server --token=${SVC_TOKEN} run"));
    }

    #[test]
    fn env_and_headers_records_mask_by_name() {
        let out = mask(
            json!({ "env": { "MY_TOKEN": "x", "PLAIN": "y" }, "headers": { "Authorization": "Bearer x", "Accept": "json" } }),
        );
        assert_eq!(out["env"]["MY_TOKEN"], json!(MASK));
        assert_eq!(out["env"]["PLAIN"], json!("y"));
        assert_eq!(out["headers"]["Authorization"], json!(MASK));
        assert_eq!(out["headers"]["Accept"], json!("json"));
    }

    #[test]
    fn unmask_restores_sentinels_from_stored() {
        let stored = ServerDef(
            json!({ "type": "mysql", "password": "real-secret", "url": "postgres://u:realpw@h/d", "env": { "T": "tok" } })
                .as_object()
                .cloned()
                .unwrap(),
        );
        let body = json!({
            "type": "mysql",
            "password": MASK,
            "url": "postgres://u:••••••••@h/d2",
            "env": { "T": MASK },
        });
        let out = unmask_body(body.as_object().unwrap(), Some(&stored));
        assert_eq!(out["password"], json!("real-secret"));
        assert_eq!(out["url"], json!("postgres://u:realpw@h/d2"));
        assert_eq!(out["env"]["T"], json!("tok"));
    }

    #[test]
    fn unmask_drops_unrestorable_sentinels() {
        // A sentinel with nothing behind it is DROPPED, not persisted as dots.
        let body = json!({ "type": "pg", "password": MASK, "url": "postgres://h/d" });
        let out = unmask_body(body.as_object().unwrap(), None);
        assert!(out.get("password").is_none());
        assert_eq!(out["url"], json!("postgres://h/d"));
    }

    #[test]
    fn unmask_command_restores_in_order() {
        let stored = Value::String("run --token=a1 --api-key b2 --verbose".into());
        let next = "run --token=•••••••• --api-key •••••••• --verbose";
        let out = unmask_command(next, Some(&stored));
        assert_eq!(out, "run --token=a1 --api-key b2 --verbose");
    }

    #[test]
    fn secret_arg_keys_wordlist() {
        assert!(is_secret_arg_key("apiKey"));
        assert!(is_secret_arg_key("api_key"));
        assert!(is_secret_arg_key("api-key"));
        assert!(is_secret_arg_key("Authorization"));
        assert!(is_secret_arg_key("db_password"));
        // `key` alone is not a wordlist entry — only api-key shaped names are. A bare `key`
        // substring (keyboard, monkey) must not over-redact; "tokenEnv" IS caught, by `token`.
        assert!(is_secret_arg_key("tokenEnv"));
        assert!(
            !is_secret_arg_key("keyboard"),
            "`key` alone must not over-redact"
        );
        assert!(!is_secret_arg_key("rows"));
    }
}
