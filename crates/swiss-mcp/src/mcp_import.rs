/*
 * Copyright 2026 The swiss authors
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

//! Client `.mcp.json` import — port of `mcp-import.ts`.
//!
//! The file is how those clients (Claude Code, Cursor, OpenCode, …) spawn or reach MCPs today.
//! Importing it is how they move onto this gateway: a stdio `command` becomes a `proc` MCP, a
//! remote `url` becomes an `http` MCP. Names that already exist are not overwritten — they get
//! `-1`, `-2`, … so two clients' copies of `redis` land as `redis` and `redis-1`.
//!
//! URLs that already point at THIS gateway are skipped: those entries are the client's wiring to
//! us, not a server we should host (importing them would proxy the gateway to itself).

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use swiss_host::config::ServerDef;
use swiss_host::local_only::is_loopback_bind_host;

/// Name as it appeared in the file, after sanitizing (before any collision suffix).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportAdd {
    pub wanted: String,
    /// Registry name that will be used (wanted, or wanted-N).
    pub name: String,
    pub def: ServerDef,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportSkip {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportPlan {
    pub add: Vec<ImportAdd>,
    pub skip: Vec<ImportSkip>,
}

/// A free name under the gateway's path rules: the wanted name, or wanted-N on collision.
/// `taken` is the whole rule (docs/24 P2) — under the /mcp/ domain nothing is reserved, so a
/// name is taken only when an existing MCP holds it.
///
/// Node's version throws when 9999 suffixed names are all taken; here that surfaces as `Err`,
/// which the admin route answers with instead of an unhandled 500.
pub fn unique_name(base: &str, taken: &HashSet<String>) -> Result<String, String> {
    if !taken.contains(base) {
        return Ok(base.to_string());
    }
    for i in 1..10000 {
        let n = format!("{base}-{i}");
        if !taken.contains(&n) {
            return Ok(n);
        }
    }
    Err(format!("could not allocate a unique name for {base}"))
}

/// Letters, digits, `_`, `-`; must start alphanumeric; max 63. Matches adminapi NAME_RE.
/// Hand-rolled scanner (ADR-007) replacing the Node regex chain.
fn sanitize_name(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let mut collapsed = String::with_capacity(lowered.len());
    let mut in_run = false;
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            collapsed.push(c);
            in_run = false;
        } else if !in_run {
            // Each run of disallowed characters collapses into ONE dash (the `+` quantifier);
            // a literal '-' next to it must survive as its own dash.
            collapsed.push('-');
            in_run = true;
        }
    }
    let mut s = collapsed.trim_matches('-').to_string();
    // After dash-stripping the only possible non-alphanumeric lead is `_`.
    let starts_alnum = s
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !s.is_empty() && !starts_alnum {
        s = format!("m{s}");
    }
    if s.is_empty() {
        s = "mcp".into();
    }
    if s.len() > 63 {
        // `s` is pure ASCII by construction (every pushed char is from the allowed class or the
        // "m" / "mcp" fallbacks), so byte slicing at 63 is a character boundary.
        s = s[..63].trim_end_matches('-').to_string();
        if s.is_empty() {
            s = "mcp".into();
        }
    }
    s
}

/// JS `\s` — Unicode whitespace plus the BOM, which JS counts as space and Unicode does not.
fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// Quote one argv element so the proc adapter's `tokenize_command` round-trips it: unquoted when
/// it holds no whitespace and no `"`, else wrapped in double quotes with inner quotes `\"`-escaped
/// (backslashes stay literal — they are path separators on Windows).
fn quote_arg(s: &str) -> String {
    if !s.chars().any(|c| c == '"' || is_js_space(c)) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn join_command(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(quote_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The slice of WHATWG URL parsing `is_gateway_url` needs. Full spec parsing is far bigger than
/// this job, so this reads absolute URLs with an authority — the shape every client config
/// holds — and returns `None` where WHATWG would throw.
struct ParsedUrl {
    scheme: String,
    /// Lowercased, percent-decoded, brackets kept on IPv6 literals (as WHATWG reports them).
    hostname: String,
    /// Explicit port; `None` when absent or empty (`http://h:/x` normalizes to no port).
    port: Option<u16>,
}

/// Schemes WHATWG calls "special" — they tolerate any number of slashes before the authority
/// (`http:/host` still has a host) and treat `\` as `/`.
const SPECIAL_SCHEMES: [&str; 6] = ["http", "https", "ws", "wss", "ftp", "file"];

fn parse_url(url: &str) -> Option<ParsedUrl> {
    // Scheme: an ASCII letter, then letters/digits/+/-/. up to ':'. WHATWG rejects anything else
    // as "not an absolute URL" (there is no base URL to resolve against here).
    let bytes = url.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.' {
            i += 1;
        } else {
            break;
        }
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    let scheme = url[..i].to_ascii_lowercase();

    // Authority: `//` for any scheme; special schemes also accept a thinner slash run.
    let rest = &url[i + 1..];
    let special = SPECIAL_SCHEMES.contains(&scheme.as_str());
    let has_authority =
        rest.starts_with("//") || (special && (rest.starts_with('/') || rest.starts_with('\\')));
    if !has_authority {
        // Opaque-path URL (`mailto:…`, `localhost:19999/x`): no host, which never passes the
        // loopback check — the same answer Node reaches through `new URL`.
        return Some(ParsedUrl {
            scheme,
            hostname: String::new(),
            port: None,
        });
    }
    let after_slashes = rest.trim_start_matches(['/', '\\']);
    let auth_end = after_slashes
        .find(['/', '\\', '?', '#'])
        .unwrap_or(after_slashes.len());
    let authority = &after_slashes[..auth_end];

    // Userinfo ends at the LAST '@' (WHATWG rule), and never reaches the host.
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);

    let (hostname, port_str) = if hostport.starts_with('[') {
        // IPv6 literal: hostname keeps its brackets (`[::1]`), the port follows `]:`.
        let end = hostport.find(']')?;
        let tail = &hostport[end + 1..];
        let digits = tail.strip_prefix(':').unwrap_or("");
        if !tail.is_empty() && digits.is_empty() {
            return None; // junk where `:port` was expected
        }
        (&hostport[..=end], digits)
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h, p),
            None => (hostport, ""),
        }
    };

    let port = if port_str.is_empty() {
        None
    } else {
        // WHATWG allows digits only and caps at 65535; anything else throws (Rust's u16 parse
        // would also accept a leading '+', hence the explicit digits check).
        if !port_str.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(port_str.parse::<u16>().ok()?)
    };

    Some(ParsedUrl {
        scheme,
        hostname: percent_decode(hostname).to_ascii_lowercase(),
        port,
    })
}

/// Decode `%XX` escapes, leaving malformed sequences as literal text. WHATWG applies this to
/// hostnames (`http://127%2E0.0.1` reads as `127.0.0.1`), so the self-proxy check must land on
/// the decoded form.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_gateway_url(url: &str, gateway_port: u16) -> bool {
    let Some(u) = parse_url(url) else {
        return false;
    };
    if !is_loopback_bind_host(&u.hostname) {
        return false;
    }
    let p = u.port.unwrap_or(if u.scheme == "https" { 443 } else { 80 });
    p == gateway_port
}

/// Find the name -> entry map: the `mcpServers` block (Claude Code), the `servers` block
/// (Cursor), or — when every value is itself an object — the pasted object as a bare map.
fn server_map(raw: &Value) -> Option<&Map<String, Value>> {
    let o = raw.as_object()?;
    for key in ["mcpServers", "servers"] {
        if let Some(block) = o.get(key).and_then(Value::as_object) {
            return Some(block);
        }
    }
    if !o.is_empty() && o.values().all(|v| v.as_object().is_some()) {
        return Some(o);
    }
    None
}

/// `String(v)` for argv elements: client configs hold numbers, booleans and nulls in `args`, and
/// Node stringified each before joining. (Number formatting matches JS for the integer and
/// simple-fraction shapes a config file can hold.)
fn js_coerce_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(i) = n.as_i64() {
                i.to_string()
            } else {
                n.as_f64().map(|f| f.to_string()).unwrap_or_default()
            }
        }
        Value::Array(a) => a.iter().map(js_coerce_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

enum EntryMapping {
    Def(ServerDef),
    Skip(&'static str),
}

fn entry_to_def(entry: &Value, gateway_port: u16) -> EntryMapping {
    let Some(e) = entry.as_object() else {
        return EntryMapping::Skip("not an object");
    };
    // `String(e.type ?? "")`: only a string `type` can ever match the httpish list, so every
    // other shape collapses to "" as far as behaviour is concerned.
    let type_ = e
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase()
        .replace('_', "-");
    let url = e
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let command = e
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let args: Vec<String> = e
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(js_coerce_string).collect())
        .unwrap_or_default();
    let httpish = matches!(
        type_.as_str(),
        "http" | "sse" | "streamable-http" | "remote"
    );

    // A URL wins unless the entry looks stdio-shaped (a command and no http-ish type marker).
    if !url.is_empty() && (httpish || command.is_empty()) {
        if is_gateway_url(url, gateway_port) {
            return EntryMapping::Skip("points at this gateway");
        }
        let mut map = Map::new();
        map.insert("type".into(), json!("http"));
        map.insert("url".into(), json!(url));
        if let Some(Value::Object(headers)) = e.get("headers") {
            map.insert("headers".into(), Value::Object(headers.clone()));
        }
        return EntryMapping::Def(ServerDef(map));
    }
    if !command.is_empty() {
        let mut map = Map::new();
        map.insert("type".into(), json!("proc"));
        map.insert("command".into(), json!(join_command(command, &args)));
        if let Some(Value::Object(env)) = e.get("env") {
            map.insert("env".into(), Value::Object(env.clone()));
        }
        if let Some(cwd) = e
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            map.insert("cwd".into(), json!(cwd.trim()));
        }
        return EntryMapping::Def(ServerDef(map));
    }
    EntryMapping::Skip("needs a command or a url")
}

/// Plan the import: names allocated, defs built, skips explained. Does not touch the registry —
/// the admin route applies the plan so a half-failed register still reports the rest.
///
/// `Err` carries the (practically unreachable) name-allocation failure from `unique_name`.
pub fn plan_mcp_import(
    raw: &Value,
    taken: &HashSet<String>,
    gateway_port: u16,
) -> Result<ImportPlan, String> {
    let mut plan = ImportPlan::default();
    let Some(map) = server_map(raw) else {
        plan.skip.push(ImportSkip {
            name: String::new(),
            reason: "no mcpServers / servers map in the file".into(),
        });
        return Ok(plan);
    };
    // A local copy so entries imported earlier in this same file block later collisions.
    let mut taken = taken.clone();
    for (raw_name, entry) in map {
        let wanted = sanitize_name(raw_name);
        match entry_to_def(entry, gateway_port) {
            // Skips report the name as it appeared in the file, before sanitizing.
            EntryMapping::Skip(reason) => {
                plan.skip.push(ImportSkip {
                    name: raw_name.clone(),
                    reason: reason.into(),
                });
            }
            EntryMapping::Def(def) => {
                let name = unique_name(&wanted, &taken)?;
                taken.insert(name.clone());
                plan.add.push(ImportAdd { wanted, name, def });
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    // Ported from test/mcp-import.test.ts (assertions verbatim), plus the error paths and
    // precedence rules that file leaves to the implementation.
    use super::*;
    use serde_json::json;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn def_value(add: &ImportAdd) -> Value {
        serde_json::to_value(&add.def).unwrap()
    }

    // --- uniqueName -----------------------------------------------------------

    #[test]
    fn keeps_the_name_when_it_is_free() {
        assert_eq!(unique_name("redis", &set(&[])).unwrap(), "redis");
    }

    #[test]
    fn appends_1_then_2_when_the_name_is_taken() {
        assert_eq!(unique_name("redis", &set(&["redis"])).unwrap(), "redis-1");
        assert_eq!(
            unique_name("redis", &set(&["redis", "redis-1"])).unwrap(),
            "redis-2"
        );
    }

    #[test]
    fn plugin_domain_names_need_no_host_reservation() {
        // docs/24 P2: the host's root reserved words retired with the root-level route. A
        // name is taken only when it is actually taken; collisions still suffix.
        assert_eq!(unique_name("health", &set(&[])).unwrap(), "health");
        assert_eq!(unique_name("api", &set(&[])).unwrap(), "api");
        assert_eq!(unique_name("admin", &set(&["admin"])).unwrap(), "admin-1");
    }

    // --- planMcpImport --------------------------------------------------------

    const PORT: u16 = 19999;

    #[test]
    fn turns_a_stdio_entry_into_a_proc_mpc_joining_command_and_args() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert!(r.skip.is_empty());
        assert_eq!(r.add.len(), 1);
        assert_eq!(r.add[0].wanted, "files");
        assert_eq!(r.add[0].name, "files");
        assert_eq!(
            def_value(&r.add[0]),
            json!({ "type": "proc", "command": "npx -y @modelcontextprotocol/server-filesystem /tmp" })
        );
    }

    #[test]
    fn quotes_args_that_contain_spaces_so_tokenize_command_round_trips() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "fs": { "command": "npx", "args": ["-y", "pkg", "C:\\Program Files\\data"] },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        let command = r.add[0].def.get_str("command").unwrap();
        assert!(command.contains(r#""C:\Program Files\data""#), "{command}");
    }

    #[test]
    fn turns_an_http_sse_remote_url_into_an_http_mcp_keeping_headers() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "docs": {
                        "type": "http",
                        "url": "https://mcp.example/mcp",
                        "headers": { "Authorization": "Bearer ${DOCS_KEY}" },
                    },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(
            def_value(&r.add[0]),
            json!({
                "type": "http",
                "url": "https://mcp.example/mcp",
                "headers": { "Authorization": "Bearer ${DOCS_KEY}" },
            })
        );
    }

    #[test]
    fn skips_a_url_that_already_points_at_this_gateway() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "redis": { "url": "http://127.0.0.1:19999/redis", "headers": { "Authorization": "Bearer x" } },
                    "other": { "url": "http://localhost:19999/mysql" },
                    "keep": { "url": "http://127.0.0.1:3000/mcp" },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        let mut names: Vec<&str> = r.skip.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["other", "redis"]);
        assert!(r.skip.iter().all(|s| s.reason == "points at this gateway"));
        let add_names: Vec<&str> = r.add.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(add_names, vec!["keep"]);
    }

    #[test]
    fn renames_collisions_instead_of_overwriting() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "redis": { "command": "npx", "args": ["a"] },
                    "extra": { "command": "npx", "args": ["b"] },
                }
            }),
            &set(&["redis"]),
            PORT,
        )
        .unwrap();
        let mut names: Vec<&str> = r.add.iter().map(|a| a.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["extra", "redis-1"]);
    }

    #[test]
    fn two_entries_that_sanitize_to_the_same_name_get_1_on_the_second() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "Redis": { "command": "npx", "args": ["a"] },
                    "redis": { "command": "npx", "args": ["b"] },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        let names: Vec<&str> = r.add.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"redis"));
        assert!(names.contains(&"redis-1"));
    }

    #[test]
    fn accepts_cursors_servers_key_and_a_bare_name_map() {
        let via_servers = plan_mcp_import(
            &json!({ "servers": { "a": { "command": "npx", "args": ["x"] } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(via_servers.add[0].name, "a");

        let bare = plan_mcp_import(
            &json!({ "b": { "url": "https://example.test/mcp" } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(bare.add[0].name, "b");
    }

    #[test]
    fn skips_an_entry_that_has_neither_command_nor_url() {
        let r = plan_mcp_import(
            &json!({ "mcpServers": { "broken": { "type": "stdio" } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert!(r.add.is_empty());
        assert_eq!(r.skip[0].name, "broken");
        assert_eq!(r.skip[0].reason, "needs a command or a url");
    }

    #[test]
    fn carries_proc_env_and_cwd_through() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "x": { "command": "node", "args": ["s.js"], "env": { "FOO": "1" }, "cwd": "/tmp" },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(
            def_value(&r.add[0]),
            json!({ "type": "proc", "command": "node s.js", "env": { "FOO": "1" }, "cwd": "/tmp" })
        );
    }

    // --- error paths and precedence rules the Node suite leaves implicit ------

    #[test]
    fn a_file_with_no_map_reports_it_as_one_whole_file_skip() {
        for raw in [
            json!(null),
            json!("a string"),
            json!({ "b": "not an entry" }),
            json!({ "mcpServers": [] }),
            json!({}),
        ] {
            let r = plan_mcp_import(&raw, &set(&[]), PORT).unwrap();
            assert!(r.add.is_empty(), "{raw}");
            assert_eq!(r.skip.len(), 1);
            assert_eq!(r.skip[0].name, "");
            assert_eq!(r.skip[0].reason, "no mcpServers / servers map in the file");
        }
    }

    #[test]
    fn an_entry_that_is_not_an_object_is_skipped() {
        let r = plan_mcp_import(
            &json!({ "mcpServers": { "x": "run something", "y": 7, "z": ["a"] } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert!(r.add.is_empty());
        let names: Vec<&str> = r.skip.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["x", "y", "z"]);
        assert!(r.skip.iter().all(|s| s.reason == "not an object"));
    }

    #[test]
    fn an_empty_mcp_servers_block_is_a_valid_empty_plan() {
        let r = plan_mcp_import(&json!({ "mcpServers": {} }), &set(&[]), PORT).unwrap();
        assert!(r.add.is_empty());
        assert!(r.skip.is_empty());
    }

    #[test]
    fn a_url_with_a_command_and_no_type_marker_stays_proc() {
        // `url && (httpish || !command)`: without an http-ish type the command wins.
        let r = plan_mcp_import(
            &json!({ "mcpServers": { "x": { "url": "https://e.test/x", "command": "node" } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(
            def_value(&r.add[0]),
            json!({ "type": "proc", "command": "node" })
        );
    }

    #[test]
    fn an_http_type_marker_makes_the_url_win_over_a_command() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "y": { "type": "http", "url": "https://e.test/y", "command": "node" },
                    "z": { "type": "streamable_http", "url": "https://e.test/z" },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(r.add[0].def.type_(), "http");
        assert_eq!(r.add[0].def.get_str("url"), Some("https://e.test/y"));
        // Underscored `streamable_http` normalizes to the `streamable-http` marker.
        assert_eq!(r.add[1].def.type_(), "http");
        assert_eq!(r.add[1].def.get_str("url"), Some("https://e.test/z"));
    }

    #[test]
    fn non_object_headers_env_and_blank_cwd_are_dropped() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "h": { "type": "http", "url": "https://e.test/h", "headers": ["Authorization"], "env": "nope" },
                    "p": { "command": "node", "env": "nope", "cwd": "   " },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(
            def_value(&r.add[0]),
            json!({ "type": "http", "url": "https://e.test/h" })
        );
        assert_eq!(
            def_value(&r.add[1]),
            json!({ "type": "proc", "command": "node" })
        );
    }

    #[test]
    fn args_are_stringified_like_javascript_before_joining() {
        let r = plan_mcp_import(
            &json!({ "mcpServers": { "x": { "command": "node", "args": [8080, true, null] } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(r.add[0].def.get_str("command"), Some("node 8080 true null"));
    }

    #[test]
    fn default_ports_count_80_for_http_and_443_for_https() {
        // No explicit port: http defaults to 80, https to 443 — either can be this gateway.
        let on_http = plan_mcp_import(
            &json!({ "mcpServers": { "a": { "type": "http", "url": "http://127.0.0.1/x" } } }),
            &set(&[]),
            80,
        )
        .unwrap();
        assert!(on_http.add.is_empty());
        assert_eq!(on_http.skip[0].reason, "points at this gateway");

        let https_on_an_http_gateway = plan_mcp_import(
            &json!({ "mcpServers": { "b": { "type": "http", "url": "https://127.0.0.1/x" } } }),
            &set(&[]),
            80,
        )
        .unwrap();
        assert_eq!(https_on_an_http_gateway.add.len(), 1);

        let on_https = plan_mcp_import(
            &json!({ "mcpServers": { "c": { "type": "http", "url": "https://localhost/x" } } }),
            &set(&[]),
            443,
        )
        .unwrap();
        assert!(on_https.add.is_empty());
        assert_eq!(on_https.skip[0].reason, "points at this gateway");
    }

    #[test]
    fn an_ipv6_loopback_url_with_the_gateway_port_is_skipped() {
        let r = plan_mcp_import(
            &json!({ "mcpServers": { "v6": { "type": "http", "url": "http://[::1]:19999/x" } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert!(r.add.is_empty());
        assert_eq!(r.skip[0].reason, "points at this gateway");
    }

    #[test]
    fn an_unparseable_url_is_kept_rather_than_skipped() {
        // `new URL` throwing means "not this gateway" — the entry imports as-is.
        let r = plan_mcp_import(
            &json!({ "mcpServers": { "u": { "type": "http", "url": "not a url" } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert!(r.skip.is_empty());
        assert_eq!(r.add[0].def.get_str("url"), Some("not a url"));
    }

    #[test]
    fn sanitize_collapses_runs_strips_edges_and_falls_back_to_mcp() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "": { "command": "node" },
                    "My Server!!": { "command": "node" },
                    "_under": { "command": "node" },
                    "a-b--c": { "command": "node" },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        let wanted: Vec<&str> = r.add.iter().map(|a| a.wanted.as_str()).collect();
        assert!(wanted.contains(&"mcp"), "{wanted:?}"); // empty name falls back
        assert!(wanted.contains(&"my-server"), "{wanted:?}"); // run collapses, edges stripped
        assert!(wanted.contains(&"m_under"), "{wanted:?}"); // non-alphanumeric lead gets an m
        assert!(wanted.contains(&"a-b--c"), "{wanted:?}"); // literal dashes are not a run
    }

    #[test]
    fn a_name_longer_than_63_is_truncated_and_re_dashed() {
        // Dashes are legal name characters (not a collapsible run), so only the 63-char cap cuts
        // this one: slice to "a" + 62 dashes, strip the trailing dash run, leaving "a".
        let long = format!("a{}", "-".repeat(64));
        let r = plan_mcp_import(
            &json!({ "mcpServers": { long: { "command": "node" } } }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        assert_eq!(r.add[0].wanted, "a");
        assert_eq!(r.add[0].name, "a");
    }

    #[test]
    fn the_taken_set_grows_within_one_file_so_three_copies_of_redis_all_land() {
        let r = plan_mcp_import(
            &json!({
                "mcpServers": {
                    "redis": { "command": "node" },
                    "Redis!": { "command": "node" },
                    "REDIS": { "command": "node" },
                }
            }),
            &set(&[]),
            PORT,
        )
        .unwrap();
        let mut got: Vec<&str> = r.add.iter().map(|a| a.name.as_str()).collect();
        got.sort();
        assert_eq!(got, vec!["redis", "redis-1", "redis-2"]);
    }
}
