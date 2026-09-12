//! Gateway configuration — port of `config.ts`.
//!
//! `gateway.config.json` (a sealed envelope) holds the port, the bind host, the token env var
//! name and the server table. `${ENV_VAR}` references are deliberately NOT expanded at load:
//! the registry (and therefore the admin panel, and anything persisted to managed.json as an
//! override) holds the definition exactly as authored, so `${MYSQL_PASS}` stays a reference
//! instead of being written back to disk as plaintext. Only the adapter, at build time, sees the
//! real credential.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::local_only::is_loopback_bind_host;
use swiss_core::paths::{data_path, env_listen_port, env_port_raw, DEFAULT_PORT};
use swiss_core::secure::envstore::{env_lookup, inject_env_store};
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};

/// A server definition: `type` plus whatever fields that type reads. Arbitrary-key by design —
/// the Node ServerDef was an index signature, and the file format keeps fields we do not model.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct ServerDef(pub Map<String, Value>);

impl ServerDef {
    pub fn type_(&self) -> &str {
        self.0.get("type").and_then(Value::as_str).unwrap_or("")
    }
    /// Raw field access — the adapters read loose-typed fields (booleans-as-strings, numbers)
    /// that the typed getters below refuse.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.0.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    pub fn get_number(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(Value::as_f64)
    }
    pub fn set(&mut self, key: &str, value: Value) {
        self.0.insert(key.into(), value);
    }
}

pub fn config_path() -> PathBuf {
    data_path(&["gateway.config.json"])
}

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub port: u16,
    pub host: String,
    pub token: String,
    /// Name of the env var the token came from. Safe to show — it is what client configs read.
    pub token_env: String,
    /// BTreeMap: deterministic iteration order (the panel lists in config-file order; Node's
    /// object key order is insertion-ordered which JSON preserves — serde_json's default Map is
    /// also insertion-ordered, but deterministic sorting here keeps snapshots stable).
    pub servers: BTreeMap<String, ServerDef>,
    /// The whole config object as loaded, credential refs intact. The subsystem composition
    /// (subsystems.rs) reads its per-subsystem rows from here — toggles like
    /// {"jobs":{"disabled":true}} are ROW metadata, not parsed fields, so they never need a
    /// struct field of their own (the RH entry-metadata rule: addressing a row and disabling
    /// it are the same syntax as configuring it).
    pub raw: Value,
}

/// Expand `${ENV_VAR}` references in ONE string — the build-time step of the credential model,
/// shared with the tunnel connections so tunnels.json can hold refs exactly like the config
/// files do.
///
/// Now a thin wrapper over the ONE shared resolver (docs/19 D1/D4, swiss-core refs.rs), which
/// also speaks `secret://name`. This wrapper stays LENIENT on vault errors for the callers that
/// have not been moved to the strict contract yet: a string it cannot fully resolve comes back
/// exactly as authored, the pre-vault behaviour, rather than half-expanded.
pub fn resolve_env_refs(value: &str) -> String {
    swiss_core::secure::refs::resolve(value).unwrap_or_else(|_| value.to_string())
}

fn resolve_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(resolve_env_refs(s)),
        Value::Array(a) => Value::Array(a.iter().map(resolve_value).collect()),
        Value::Object(o) => Value::Object(resolve_obj(o)),
        other => other.clone(),
    }
}

fn resolve_obj(o: &Map<String, Value>) -> Map<String, Value> {
    o.iter()
        .map(|(k, v)| (k.clone(), resolve_value(v)))
        .collect()
}

/// True for a value that is exactly one credential reference — `${ENV_VAR}` (held in the
/// sealed env store) or `secret://name` (held in the vault, docs/19 D1) — i.e. a secret held
/// outside the file, not inline. The masking stack keys off this to show the reference instead
/// of ever holding the value.
pub fn is_env_ref(v: &Value) -> bool {
    let Some(s) = v.as_str() else { return false };
    if let Some(name) = s.strip_prefix("secret://") {
        return swiss_core::secure::secretstore::valid_name(name);
    }
    if !s.starts_with("${") || !s.ends_with('}') || s.len() < 4 {
        return false;
    }
    let name = &s[2..s.len() - 1];
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Expand every `${ENV_VAR}` reference in a server definition. See the module comment for why
/// this is build-time, not load-time.
pub fn resolve_def(def: &ServerDef) -> ServerDef {
    ServerDef(resolve_obj(&def.0))
}

/// The strict form (docs/19 D4): expand env refs AND vault refs across the whole definition
/// tree, and refuse the definition when a vault reference names a secret this machine does not
/// hold. The error carries the JSON path (`headers.Authorization references secret://x which is
/// not in the vault`) plus no value ever — the caller prefixes the MCP's name.
pub fn resolve_def_checked(def: &ServerDef) -> Result<ServerDef, String> {
    let resolved =
        swiss_core::secure::refs::resolve_value(&Value::Object(def.0.clone()))?;
    Ok(ServerDef(
        resolved.as_object().cloned().unwrap_or_default(),
    ))
}

/// The token env var this build seeds into new configs.
pub const TOKEN_ENV: &str = "SWISS_TOKEN";

/// The Node-era token env var, still honored so pre-rename shells and sealed stores keep
/// working.
pub const TOKEN_ENV_LEGACY: &str = "MCP_GATEWAY_TOKEN";

/// The other well-known token env var, when `name` is one of the two. The two names are a
/// pair: a config from one era may point at a token pinned under the other era's name, and the
/// auth check, `swiss creds` and the panel must all resolve that the same way, from this one
/// place. A config naming its own variable gets no pairing — it must not quietly authenticate
/// with somebody else's secret.
pub fn token_pair_other(name: &str) -> Option<&'static str> {
    match name {
        TOKEN_ENV => Some(TOKEN_ENV_LEGACY),
        TOKEN_ENV_LEGACY => Some(TOKEN_ENV),
        _ => None,
    }
}

/// The bearer token for a configured `tokenEnv` name: the name itself, then its well-known
/// pair partner when the name is one of the two eras' variables (see `token_pair_other`).
/// An empty value counts as missing, exactly as a direct lookup treated it before the pair
/// existed.
fn token_lookup(token_env: &str) -> Option<String> {
    if let Some(t) = env_lookup(token_env).filter(|t| !t.is_empty()) {
        return Some(t);
    }
    token_pair_other(token_env)
        .and_then(env_lookup)
        .filter(|t| !t.is_empty())
}

/// A missing port must not reach the listener as "pick any free port" — the gateway would come
/// up looking healthy on an address no client was ever pointed at. A malformed one is refused
/// for the same reason. `SWISS_PORT` / `MCP_GATEWAY_PORT` (new name first) wins over the file,
/// so `swiss start --port N` actually changes where this process listens.
fn resolve_port(raw: Option<&Value>) -> Result<u16, String> {
    if let Some(from_env) = env_listen_port() {
        return Ok(from_env);
    }
    // env_listen_port returned None, so whatever env_port_raw holds does not parse — refuse it
    // under its own name rather than silently falling through to the file.
    if let Some((name, raw)) = env_port_raw() {
        return Err(format!(
            "{name} {raw:?} is not usable: give an integer between 1 and 65535."
        ));
    }
    match raw {
        None | Some(Value::Null) => Ok(DEFAULT_PORT),
        Some(Value::Number(n)) => {
            if let Some(p) = n.as_u64().filter(|p| (1..=65535).contains(p)) {
                Ok(p as u16)
            } else {
                Err(format!(
                    "port {} is not usable: give an integer between 1 and 65535, or omit it to listen on {DEFAULT_PORT}.",
                    n
                ))
            }
        }
        Some(other) => Err(format!(
            "port {other} is not usable: give an integer between 1 and 65535, or omit it to listen on {DEFAULT_PORT}."
        )),
    }
}

pub fn load_config(path: &Path) -> Result<GatewayConfig, String> {
    // The sealed env store (where bootstrap seeds the token) replaces the old plaintext .env:
    // its values land in the env overlay here, never overriding what the environment already
    // set, so the `${ENV_VAR}` build-time expansion keeps resolving exactly as before.
    inject_env_store(&swiss_core::secure::envstore::env_store_path());
    let raw = read_secure_json(path)?.ok_or_else(|| {
        format!(
            "{}: no config file — run 'swiss start' once (it seeds one) or 'swiss import' a backup",
            path.display()
        )
    })?;

    // Structure gate before any field is read: a missing `servers` used to die as a cryptic
    // TypeError and a SCALAR servers value booted a gateway with zero MCPs, silently healthy.
    let obj = raw.as_object().ok_or_else(|| {
        format!(
            "{}: expected a JSON object at the top level",
            path.display()
        )
    })?;
    let servers_value = obj
        .get("servers")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let servers_obj = servers_value.as_object().ok_or_else(|| {
        format!(
            "{}: \"servers\" must be an object of name -> definition",
            path.display()
        )
    })?;

    let token_env = obj
        .get("tokenEnv")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "{}: \"tokenEnv\" must name the env var holding the auth token",
                path.display()
            )
        })?
        .to_string();

    // Omitted means loopback: there is exactly one sensible bind address for this gateway, and
    // leaving it undefined used to mean "every interface" — the opposite. Refused at load, not
    // warned about: a gateway holding live DB credentials has no business being offered to
    // another machine, and `0.0.0.0` is how that happens by accident.
    let host = match obj.get("host") {
        None | Some(Value::Null) => "127.0.0.1".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    if !is_loopback_bind_host(&host) {
        return Err(format!(
            "host {host:?} is not local: this gateway serves only its own machine. \
             Use 127.0.0.1 — \"0.0.0.0\" binds every interface. To reach it from elsewhere, forward the port over SSH."
        ));
    }

    // The bearer token for a configured `tokenEnv` name: the name itself, then its well-known
    // pair partner when the name is one of the two eras' variables (see `token_pair_other`).
    // An empty value counts as missing, exactly as a direct lookup treated it before the pair
    // existed. Resolved after the host refusal so a bad host reports as a bad host, not as a
    // missing token.
    let token = token_lookup(&token_env)
        .ok_or_else(|| format!("Missing token env var: {token_env}"))?;

    let port = resolve_port(obj.get("port"))?;

    let mut servers = BTreeMap::new();
    for (name, def) in servers_obj {
        servers.insert(
            name.clone(),
            ServerDef(def.as_object().cloned().unwrap_or_default()),
        );
    }
    Ok(GatewayConfig {
        port,
        host,
        token,
        token_env,
        servers,
        raw,
    })
}

/// Remove one server entry from gateway.config.json — the file leg of the panel's Delete for a
/// config-sourced MCP. Without it the runtime entry goes away but the next start resurrects the
/// MCP from the file. Only the named key is removed; the rest of the file keeps its `${ENV}`
/// credential references verbatim. Returns false when neither the file nor the entry has it.
pub fn remove_config_server(name: &str, path: &Path) -> bool {
    let Ok(Some(raw)) = read_secure_json(path) else {
        return false;
    };
    let Some(mut obj) = raw.as_object().cloned() else {
        return false;
    };
    let Some(servers) = obj.get("servers").and_then(Value::as_object).cloned() else {
        return false;
    };
    if !servers.contains_key(name) {
        return false;
    }
    let mut next = servers.clone();
    next.remove(name);
    obj.insert("servers".into(), Value::Object(next));
    write_secure_json(path, &Value::Object(obj)).is_ok()
}

#[cfg(test)]
mod tests {
    // Ported from test/config.test.ts (load/validation/ENV-expansion assertions).
    //
    // env::set_var is unsafe in edition 2024 because it races other threads; each test below
    // uses a variable name unique to this module and tests run on their own threads, so the
    // mutation is confined and sound.
    use super::*;
    use serde_json::json;

    fn set_env(key: &str, value: &str) {
        unsafe { std::env::set_var(key, value) };
    }
    fn remove_env(key: &str) {
        unsafe { std::env::remove_var(key) };
    }

    fn write_cfg(dir: &Path, v: Value) -> PathBuf {
        let p = dir.join("gateway.config.json");
        std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();
        p
    }
    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("swiss-cfg-{tag}-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn env_ref_expansion_and_detection() {
        set_env("SWISS_TEST_SECRET", "hunter2");
        assert_eq!(
            resolve_env_refs("pass=${SWISS_TEST_SECRET}!"),
            "pass=hunter2!"
        );
        assert_eq!(resolve_env_refs("${SWISS_TEST_SECRET}"), "hunter2");
        // Unknown refs expand empty, exactly like Node's `?? ""`.
        assert_eq!(resolve_env_refs("${NOPE_MISSING}"), "");
        // Non-conforming names are left as literal text.
        assert_eq!(resolve_env_refs("${lowercase}"), "${lowercase}");
        assert_eq!(resolve_env_refs("$NOPE {a}"), "$NOPE {a}");
        assert!(is_env_ref(&json!("${SWISS_TEST_SECRET}")));
        assert!(!is_env_ref(&json!("plain")));
        assert!(!is_env_ref(&json!("${a-b}")));
        assert!(!is_env_ref(&json!("${}")));
        remove_env("SWISS_TEST_SECRET");
    }

    #[test]
    fn resolve_def_expands_nested_values_only() {
        let def = ServerDef(
            serde_json::from_value(json!({
                "type": "mysql",
                "password": "${SWISS_X_PASS}",
                "host": "db.local",
                "env": { "TOKEN": "${SWISS_X_TOKEN}" },
            }))
            .unwrap(),
        );
        set_env("SWISS_X_PASS", "p");
        set_env("SWISS_X_TOKEN", "t");
        let resolved = resolve_def(&def);
        assert_eq!(resolved.get_str("password"), Some("p"));
        assert_eq!(resolved.get_str("host"), Some("db.local"));
        assert_eq!(resolved.0["env"]["TOKEN"], json!("t"));
        remove_env("SWISS_X_PASS");
        remove_env("SWISS_X_TOKEN");
    }

    #[test]
    fn a_config_missing_everything_is_rejected_by_name() {
        let dir = temp_dir("missing");
        let p = write_cfg(&dir, json!({}));
        let err = load_config(&p).unwrap_err();
        assert!(err.contains("tokenEnv"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_scalar_servers_table_is_rejected() {
        let dir = temp_dir("scalar");
        let p = write_cfg(
            &dir,
            json!({ "tokenEnv": "MCP_GATEWAY_TOKEN", "servers": "nope" }),
        );
        let err = load_config(&p).unwrap_err();
        assert!(err.contains("servers"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_non_loopback_host_is_refused_at_load() {
        let dir = temp_dir("host");
        let p = write_cfg(
            &dir,
            json!({ "tokenEnv": "MCP_GATEWAY_TOKEN", "host": "0.0.0.0", "servers": {} }),
        );
        let err = load_config(&p).unwrap_err();
        assert!(err.contains("not local"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn defaults_are_loopback_and_default_port() {
        let dir = temp_dir("defaults");
        let p = write_cfg(&dir, json!({ "tokenEnv": "SWISS_TOK_V", "servers": {} }));
        set_env("SWISS_TOK_V", "x");
        let cfg = load_config(&p).unwrap();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, DEFAULT_PORT);
        remove_env("SWISS_TOK_V");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Serialises the tests that write the two well-known token names: unlike the unique
    /// SWISS_TOK_V-style vars elsewhere, these are shared, and set/remove from two tests at
    /// once makes each read the other's value.
    static WELL_KNOWN_TOKEN_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_well_known_pair_maps_both_ways() {
        // The pair rule is what every reader (serve, CLI, panel) routes through; a custom name
        // is never a member.
        assert_eq!(token_pair_other(TOKEN_ENV), Some(TOKEN_ENV_LEGACY));
        assert_eq!(token_pair_other(TOKEN_ENV_LEGACY), Some(TOKEN_ENV));
        assert_eq!(token_pair_other("MINE_TOKEN"), None);
    }

    #[test]
    fn a_present_primary_token_wins_over_its_pair() {
        // When the config's own name holds a value, the pair partner is not consulted. Only
        // the process-env legs are asserted here: the absent-primary fallback cannot be
        // observed in this process — load_config injects the machine's real sealed store into
        // a sticky overlay, and this machine's store legitimately holds MCP_GATEWAY_TOKEN.
        // That fallback leg is covered end-to-end by the daemon tests' hermetic scratch home.
        let _well_known = WELL_KNOWN_TOKEN_TESTS.lock().expect("well-known token test lock");
        for (named, held) in [(TOKEN_ENV, TOKEN_ENV_LEGACY), (TOKEN_ENV_LEGACY, TOKEN_ENV)] {
            set_env(named, "primary-value");
            set_env(held, "pair-value");
            assert_eq!(token_lookup(named).as_deref(), Some("primary-value"));
            remove_env(named);
            remove_env(held);
        }
    }

    #[test]
    fn a_custom_token_env_name_gets_no_fallback() {
        // Only the two well-known names are a pair; a config naming its own variable must not
        // quietly authenticate with a token pinned under either of them.
        let _well_known = WELL_KNOWN_TOKEN_TESTS.lock().expect("well-known token test lock");
        let dir = temp_dir("token-custom");
        let p = write_cfg(&dir, json!({ "tokenEnv": "MINE_TOKEN", "servers": {} }));
        set_env(TOKEN_ENV, "not-mine");
        let err = load_config(&p).unwrap_err();
        assert!(err.contains("MINE_TOKEN"), "{err}");
        remove_env(TOKEN_ENV);
        std::fs::remove_dir_all(&dir).ok();
    }
}
