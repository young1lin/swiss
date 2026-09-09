//! First-run seeding — port of `bootstrap.ts`.
//!
//! The minimal config seeded on first run: one 'echo' MCP so the panel has a working endpoint
//! before any database is configured, and nothing else. Everything this writes is SEALED on
//! arrival: the seed config, the env store holding the token, and any migrated repo-local
//! state. A plaintext file dropped into the data dir by hand is adopted and sealed on read.

use std::path::Path;

use serde_json::{json, Value};

use lmg_core::log;
use lmg_core::paths::{data_path, env_listen_port, DEFAULT_PORT};
use lmg_core::platform::{chmod_private, mkdir_private, private_file_mode, PRIVATE_DIR_MODE};
use lmg_core::secure::envstore::{
    env_store_path, parse_env_text, read_env_store, set_env_default, write_env_store,
};
use lmg_core::secure::statefile::{read_secure_json, write_secure_json};
use lmg_core::util::random_hex;

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
fn migrate_env_once(repo: &Path) -> bool {
    let repo_env = repo.join(".env");
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
fn migrate_config_once(repo: &Path) -> bool {
    let repo_config = repo.join("gateway.config.json");
    let data_file = data_path(&["gateway.config.json"]);
    if data_file.exists() || !repo_config.exists() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&repo_config) else {
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
    ensure_first_run_from(&std::env::current_dir().unwrap_or_default())
}

/// The same seeding, with the directory to adopt repo-local state from named outright. Production
/// always passes the process's working directory; the tests pass a scratch dir, so no run of the
/// suite can ever reach a real project's .env.
fn ensure_first_run_from(repo: &Path) -> FirstRunReport {
    let dir = lmg_core::paths::data_dir();
    let created = !dir.exists();
    mkdir_private(&dir);

    // Pull an existing repo-local setup in once (the user's current .env / config), so an
    // upgrade does not throw away a working token and hand the operator a blank slate. Both
    // are attempted — the Node build once short-circuited past the config copy after the .env
    // copy, silently replacing the user's real config with the echo-only seed.
    let migrated_env = migrate_env_once(repo);
    let migrated_cfg = migrate_config_once(repo);
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

#[cfg(test)]
mod tests {
    // Ported from test/bootstrap.test.ts.
    use super::*;

    /// A data dir that has never been written to, plus a scratch "repo" to adopt from.
    ///
    /// The shared test home cannot serve here: `ensure_first_run` reports whether it CREATED the
    /// dir and refuses to migrate over state that is already present, so both answers are only
    /// observable on a genuinely new path. The data-dir lock is held throughout, because
    /// `MCP_GATEWAY_HOME` is process-wide and is put back on the way out.
    struct Sandbox {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        base: std::path::PathBuf,
        home: std::path::PathBuf,
        repo: std::path::PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let lock = lmg_core::paths::DATA_DIR_LOCK.blocking_lock();
            // Pin the key before anything seals, so no test here depends on DPAPI or on which
            // other test happened to install it first.
            lmg_core::secure::key::use_test_master_key();
            // Initialise the shared home now, so the restore in Drop has somewhere to go back to.
            lmg_core::paths::test_home();
            let base = std::env::temp_dir().join(format!("lmg-bootstrap-{}", random_hex(8)));
            let repo = base.join("repo");
            std::fs::create_dir_all(&repo).expect("create the scratch repo");
            let home = base.join("home");
            unsafe { std::env::set_var("MCP_GATEWAY_HOME", &home) };
            unsafe { std::env::remove_var("MCP_GATEWAY_PORT") };
            Self {
                _lock: lock,
                base,
                home,
                repo,
            }
        }

        /// Put a file where the migration looks for it.
        fn plant(&self, name: &str, text: &str) {
            std::fs::write(self.repo.join(name), text).expect("plant a repo-local file");
        }

        fn boot(&self) -> FirstRunReport {
            ensure_first_run_from(&self.repo)
        }

        fn config(&self) -> Value {
            read_secure_json(&self.home.join("gateway.config.json"))
                .expect("the config is readable")
                .expect("the config is there")
        }

        fn env(&self) -> std::collections::HashMap<String, String> {
            read_env_store(&self.home.join("env.json"))
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            unsafe { std::env::set_var("MCP_GATEWAY_HOME", lmg_core::paths::test_home()) };
            unsafe { std::env::remove_var("MCP_GATEWAY_PORT") };
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// A .env that carries the marker the adoption gate looks for.
    const GATEWAY_ENV: &str = "MCP_GATEWAY_TOKEN=abc123\nDB_PASSWORD=hunter2\n";

    fn user_config() -> String {
        json!({
            "port": 18090,
            "tokenEnv": "MCP_GATEWAY_TOKEN",
            "servers": { "mine": { "type": "echo", "password": "${DB_PASSWORD}" } },
        })
        .to_string()
    }

    #[test]
    fn seeds_a_working_gateway_on_a_machine_that_has_nothing() {
        let sb = Sandbox::new();
        let report = sb.boot();

        assert!(report.created);
        assert!(!report.migrated);
        assert!(sb.home.is_dir());

        // One echo MCP, so the panel has a working endpoint before any database is configured.
        let config = sb.config();
        assert_eq!(config["host"], json!("127.0.0.1"));
        assert_eq!(config["port"], json!(DEFAULT_PORT));
        assert_eq!(config["tokenEnv"], json!("MCP_GATEWAY_TOKEN"));
        assert_eq!(config["servers"]["echo"]["type"], json!("echo"));
        assert_eq!(config["servers"].as_object().map(|s| s.len()), Some(1));

        // The token exists, and the run that generated it is the one that reports it.
        let token = report.new_token.expect("the first run generates a token");
        assert!(token.len() >= 32 && token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(sb.env().get("MCP_GATEWAY_TOKEN"), Some(&token));
    }

    #[test]
    fn a_second_boot_changes_nothing_and_reports_nothing_new() {
        // Safe to call every boot: it acts only on what is missing. Re-seeding would hand every
        // client a token that no longer works.
        let sb = Sandbox::new();
        let first = sb.boot();
        let config_before = sb.config();

        let second = sb.boot();
        assert!(!second.created);
        assert!(!second.migrated);
        // A restart must never re-print a token into a log.
        assert_eq!(second.new_token, None);
        assert_eq!(sb.env().get("MCP_GATEWAY_TOKEN"), first.new_token.as_ref());
        assert_eq!(sb.config(), config_before);
    }

    #[test]
    fn the_seed_honours_the_port_the_operator_asked_for() {
        let sb = Sandbox::new();
        unsafe { std::env::set_var("MCP_GATEWAY_PORT", "18091") };
        sb.boot();
        assert_eq!(sb.config()["port"], json!(18091));
        assert_eq!(panel_url(), "http://127.0.0.1:18091/");
    }

    #[test]
    fn only_an_env_that_names_the_gateway_token_is_adopted() {
        // Without this gate, ANY directory's .env — a random project full of database passwords —
        // would be pulled into the gateway's store.
        assert!(looks_like_gateway_env("MCP_GATEWAY_TOKEN=abc\n"));
        assert!(looks_like_gateway_env(
            "# header\n  MCP_GATEWAY_TOKEN=abc\n"
        ));
        assert!(looks_like_gateway_env("DB_URL=x\nMCP_GATEWAY_TOKEN=abc"));
        assert!(!looks_like_gateway_env(
            "DB_PASSWORD=hunter2\nAWS_SECRET=x\n"
        ));
        assert!(!looks_like_gateway_env("# MCP_GATEWAY_TOKEN=abc\n"));
        assert!(!looks_like_gateway_env("XMCP_GATEWAY_TOKEN=abc\n"));
        assert!(!looks_like_gateway_env("MCP_GATEWAY_TOKENS=abc\n"));
        assert!(!looks_like_gateway_env(""));
    }

    #[test]
    fn only_a_config_shaped_like_a_gateways_is_adopted() {
        assert!(looks_like_gateway_config_shape(
            &json!({ "tokenEnv": "T", "servers": {} })
        ));
        assert!(!looks_like_gateway_config_shape(&json!({ "servers": {} })));
        assert!(!looks_like_gateway_config_shape(
            &json!({ "tokenEnv": "T" })
        ));
        assert!(!looks_like_gateway_config_shape(&json!({})));
        assert!(!looks_like_gateway_config_shape(&json!("not an object")));
    }

    #[test]
    fn folds_a_repo_local_env_into_the_sealed_store_without_leaving_plaintext() {
        // Parsed and re-sealed, never file-copied, so no plaintext secret lands in the data dir
        // even briefly.
        let sb = Sandbox::new();
        sb.plant(".env", GATEWAY_ENV);
        let report = sb.boot();

        assert!(report.migrated);
        let env = sb.env();
        assert_eq!(
            env.get("MCP_GATEWAY_TOKEN").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(env.get("DB_PASSWORD").map(String::as_str), Some("hunter2"));
        // The adopted token is not "new" — this run did not generate it.
        assert_eq!(report.new_token, None);

        let sealed = std::fs::read(sb.home.join("env.json")).expect("the store is on disk");
        assert!(
            !String::from_utf8_lossy(&sealed).contains("hunter2"),
            "the store must be sealed, not a copy of the .env"
        );
        // The user's own file is left where it was; adoption reads, it does not move.
        assert!(sb.repo.join(".env").exists());
    }

    #[test]
    fn never_adopts_a_second_time_over_state_the_data_dir_already_has() {
        // First run wins: a later `lmg serve` from some other project directory must not fold that
        // project's .env over a working setup.
        let sb = Sandbox::new();
        sb.boot();
        sb.plant(".env", GATEWAY_ENV);
        sb.plant("gateway.config.json", &user_config());

        assert!(!migrate_env_once(&sb.repo));
        assert!(!migrate_config_once(&sb.repo));
        assert_ne!(
            sb.env().get("MCP_GATEWAY_TOKEN").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(sb.config()["port"], json!(DEFAULT_PORT));
    }

    #[test]
    fn adopts_both_the_env_and_the_config_not_just_the_first() {
        // The Node build once short-circuited past the config adoption after the .env adoption,
        // silently replacing the user's real config with the echo-only seed.
        let sb = Sandbox::new();
        sb.plant(".env", GATEWAY_ENV);
        sb.plant("gateway.config.json", &user_config());

        let report = sb.boot();
        assert!(report.migrated);
        assert_eq!(
            sb.env().get("MCP_GATEWAY_TOKEN").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(sb.config()["servers"]["mine"]["type"], json!("echo"));
    }

    #[test]
    fn keeps_a_migrated_config_instead_of_replacing_it_with_the_echo_seed() {
        let sb = Sandbox::new();
        sb.plant("gateway.config.json", &user_config());
        sb.boot();

        let config = sb.config();
        assert!(config["servers"].get("echo").is_none(), "{config}");
        // The ${ENV} reference survives adoption — it is sealed as written, not expanded.
        assert_eq!(
            config["servers"]["mine"]["password"],
            json!("${DB_PASSWORD}")
        );
        // And the panel is announced on the user's port, not a hardcoded 19999.
        assert_eq!(panel_url(), "http://127.0.0.1:18090/");
    }

    #[test]
    fn refuses_a_repo_config_that_is_not_json_and_falls_back_to_the_seed() {
        let sb = Sandbox::new();
        sb.plant("gateway.config.json", "{ this is not json");
        let report = sb.boot();

        assert!(!report.migrated);
        assert_eq!(sb.config()["servers"]["echo"]["type"], json!("echo"));
    }

    #[test]
    fn the_panel_url_falls_back_to_the_env_port_when_no_config_names_one() {
        // Held for the data dir it installs and puts back, not for anything read off it.
        let _sb = Sandbox::new();
        assert_eq!(panel_url(), format!("http://127.0.0.1:{DEFAULT_PORT}/"));
        unsafe { std::env::set_var("MCP_GATEWAY_PORT", "18092") };
        assert_eq!(panel_url(), "http://127.0.0.1:18092/");
    }
}
