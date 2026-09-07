//! First-run seeding — port of `bootstrap.ts`.
//!
//! The minimal config seeded on first run: one 'echo' MCP so the panel has a working endpoint
//! before any database is configured, and nothing else. Everything this writes is SEALED on
//! arrival: the seed config, the env store holding the token, and any migrated repo-local
//! state. A plaintext file dropped into the data dir by hand is adopted and sealed on read.

use serde_json::{json, Value};

use crate::log;
use crate::paths::{data_path, env_listen_port, DEFAULT_PORT};
use crate::platform::{chmod_private, mkdir_private, private_file_mode, PRIVATE_DIR_MODE};
use crate::secure::envstore::{
    env_store_path, parse_env_text, read_env_store, set_env_default, write_env_store,
};
use crate::secure::statefile::{read_secure_json, write_secure_json};
use crate::util::random_hex;

fn seed_config() -> Value {
    json!({
        "port": env_listen_port().unwrap_or(DEFAULT_PORT),
        "host": "127.0.0.1",
        "tokenEnv": "MCP_GATEWAY_TOKEN",
        "servers": {
            "echo": {
                "type": "echo",
                "description": "Built-in no-op — a working endpoint before any database is configured.",
            }
        }
    })
}

/// Does this text look like IT BELONGS to a gateway setup? Without this gate, ANY directory's
/// .env (a random project full of database passwords) would be adopted.
fn looks_like_gateway_env(text: &str) -> bool {
    // /^MCP_GATEWAY_TOKEN=/m — a line starting with the marker.
    text.lines()
        .any(|l| l.trim_start().starts_with("MCP_GATEWAY_TOKEN="))
}

/// A gateway config carries tokenEnv + servers; anything else is not ours to adopt.
fn looks_like_gateway_config_shape(parsed: &Value) -> bool {
    parsed.get("tokenEnv").is_some() && parsed.get("servers").is_some()
}

/// Fold a repo-local .env into the sealed env store — PARSED, never file-copied, so no
/// plaintext lands in the data dir even briefly. First run only: an existing env.json wins.
fn migrate_env_once() -> bool {
    let repo_env = std::env::current_dir().unwrap_or_default().join(".env");
    if env_store_path().exists() || !repo_env.exists() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&repo_env) else {
        return false;
    };
    if !looks_like_gateway_env(&text) {
        return false;
    }
    let pairs = parse_env_text(&text);
    if pairs.is_empty() {
        return false;
    }
    write_env_store(&pairs, &env_store_path()).is_ok()
}

/// Adopt a repo-local gateway.config.json once — sealed on arrival, never plaintext on disk.
fn migrate_config_once() -> bool {
    let repo = std::env::current_dir()
        .unwrap_or_default()
        .join("gateway.config.json");
    let data_file = data_path(&["gateway.config.json"]);
    if data_file.exists() || !repo.exists() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&repo) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if !looks_like_gateway_config_shape(&parsed) {
        return false;
    }
    write_secure_json(&data_file, &parsed).is_ok()
}

/// What the run that just bootstrapped should tell the operator (secrets only on the run that
/// created them, so a restart never re-prints a token into a log).
pub struct FirstRunReport {
    pub data_dir: std::path::PathBuf,
    /// The token, only when this run generated it.
    pub new_token: Option<String>,
    /// True when a repo-local .env / config was folded into the sealed stores.
    pub migrated: bool,
    /// True when the data dir did not exist before this call.
    pub created: bool,
}

/// Make the gateway runnable with no prior setup: create the data dir, fold any repo-local
/// state in once (sealed), seed a default config, and guarantee a token exists in the sealed
/// env store. Safe to call every boot — it only acts on what is missing.
///
/// Runs before load_config(), because load_config injects the env store this seeds.
pub fn ensure_first_run() -> FirstRunReport {
    let dir = crate::paths::data_dir();
    let created = !dir.exists();
    mkdir_private(&dir);

    // Pull an existing repo-local setup in once (the user's current .env / config), so an
    // upgrade does not throw away a working token and hand the operator a blank slate. Both
    // are attempted — the Node build once short-circuited past the config copy after the .env
    // copy, silently replacing the user's real config with the echo-only seed.
    let migrated_env = migrate_env_once();
    let migrated_cfg = migrate_config_once();
    let migrated = migrated_env || migrated_cfg;

    let config_file = data_path(&["gateway.config.json"]);
    if !config_file.exists() {
        let _ = write_secure_json(&config_file, &seed_config());
    }

    // Guarantee the token exists, in the sealed env store (the plaintext .env replacement). A
    // generated token means clients configured against it keep working only if it is durable —
    // hence the data dir, not a package cache. (The panel has no login: the loopback guard is
    // its boundary, so no password is generated.)
    let new_token = set_env_default_marker();

    chmod_private(&dir, PRIVATE_DIR_MODE);
    for f in [
        "env.json",
        "master.key",
        "managed.json",
        "tunnels.json",
        "gateway.config.json",
    ] {
        chmod_private(&data_path(&[f]), private_file_mode());
    }

    let report = FirstRunReport {
        data_dir: dir,
        new_token,
        migrated,
        created,
    };
    print_report(&report);
    report
}

/// The setEnvDefault contract returns "was written"; the token VALUE then comes from the store.
fn set_env_default_marker() -> Option<String> {
    let store = read_env_store(&env_store_path());
    let before = store.contains_key("MCP_GATEWAY_TOKEN");
    let written = set_env_default("MCP_GATEWAY_TOKEN", &random_hex(24), &env_store_path());
    match (!before, written) {
        (true, Some(tok)) => Some(tok), // was absent, now written by us
        _ => None,
    }
}

/// The panel URL from the config we just wrote or migrated — never a hardcoded 19999.
fn panel_url() -> String {
    let from_config = read_secure_json(&data_path(&["gateway.config.json"]))
        .ok()
        .flatten()
        .and_then(|c| c.get("port")?.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p >= 1);
    if let Some(p) = from_config {
        return format!("http://127.0.0.1:{p}/");
    }
    format!(
        "http://127.0.0.1:{}/",
        env_listen_port().unwrap_or(DEFAULT_PORT)
    )
}

fn print_report(r: &FirstRunReport) {
    if !r.created && !r.migrated && r.new_token.is_none() {
        return; // ordinary boot: say nothing
    }
    println!();
    println!("mcp-gateway data dir: {}", r.data_dir.display());
    if r.migrated {
        println!("  (imported your existing .env / gateway.config.json from this directory — now encrypted at rest)");
    }
    if r.new_token.is_some() {
        println!("  token:              lmg creds (the panel itself has no login)");
    }
    println!("  panel:              {}", panel_url());
    println!();
    let _ = log::info; // keep the logger linked; the report itself is plain console output, as in Node
}
