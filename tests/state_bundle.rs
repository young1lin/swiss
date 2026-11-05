/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

//! `swiss export` and `swiss import` run as the real binary, the way a person runs them: stdout
//! redirected to a file, on a home that may still hold legacy plaintext state.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

/// A home of this test's own. The binary reads nothing else: `SWISS_HOME` and `SWISS_MASTER_KEY`
/// keep it away from the machine's real state and credential store.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "swiss-state-bundle-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&base).expect("scratch directory");
        Scratch(base)
    }

    fn home(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key(byte: &str) -> String {
    byte.repeat(32)
}

fn swiss(home: &Path, master_key: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_swiss"))
        .args(args)
        .env("SWISS_HOME", home)
        .env("SWISS_MASTER_KEY", master_key)
        .env_remove("SWISS_PORT")
        .current_dir(home.parent().expect("a home has a parent"))
        .output()
        .expect("run swiss")
}

fn plant(home: &Path, file: &str, text: &str) {
    std::fs::create_dir_all(home).expect("home");
    std::fs::write(home.join(file), text).expect("plant a state file");
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// SPEC §host.cli: stdout belongs to the command, so a redirect holds the document alone even on
/// the run that migrates a legacy `.env` and seals plaintext files.
#[test]
fn an_export_of_legacy_state_is_one_document_and_imports_on_a_new_home() {
    let scratch = Scratch::new();
    let old = scratch.home("old");
    plant(&old, ".env", "SWISS_TOKEN=legacy-token\nDB_PASSWORD=legacy-password\n");
    plant(&old, "gateway.config.json", r#"{"tokenEnv":"SWISS_TOKEN","servers":{}}"#);
    plant(&old, "managed.json", r#"{"mcps":[]}"#);
    plant(&old, "tunnels.json", r#"{"connections":[]}"#);
    plant(&old, "mcp-oauth.json", r#"{"figma":{"client_id":"cid"}}"#);
    plant(&old, "secrets.json", r#"{"rev":3,"secrets":{"kept-name":"legacy-value"}}"#);

    let exported = swiss(&old, &key("ab"), &["export"]);
    assert!(exported.status.success(), "{}", text_of(&exported.stderr));
    let bundle: Value = serde_json::from_slice(&exported.stdout).unwrap_or_else(|err| {
        panic!("stdout is not one JSON document ({err}):\n{}", text_of(&exported.stdout))
    });
    assert_eq!(bundle["env"]["DB_PASSWORD"], json!("legacy-password"));
    assert_eq!(bundle["config"]["tokenEnv"], json!("SWISS_TOKEN"));
    assert_eq!(bundle["secrets"]["secrets"]["kept-name"], json!("******"));
    assert!(
        text_of(&exported.stderr).contains("moved the plaintext .env"),
        "the migration still says so, on stderr: {}",
        text_of(&exported.stderr)
    );
    assert!(!old.join(".env").exists(), "the plaintext .env is consumed");

    // The file a person gets from `swiss export > bundle.json`, read on a home not made yet.
    let file = scratch.home("bundle.json");
    std::fs::write(&file, &exported.stdout).expect("the redirect");
    let fresh = scratch.home("fresh");
    let imported = swiss(&fresh, &key("cd"), &["import", file.to_str().expect("utf-8 path")]);
    assert!(imported.status.success(), "{}", text_of(&imported.stderr));

    let again = swiss(&fresh, &key("cd"), &["export"]);
    assert!(again.status.success(), "{}", text_of(&again.stderr));
    let restored: Value = serde_json::from_slice(&again.stdout).expect("a second document");
    for section in ["config", "managed", "tunnels", "oauth", "env"] {
        assert_eq!(restored[section], bundle[section], "{section}");
    }
}

/// SPEC §core.sealed: the torn store and the legacy `.env` beside it may be the only copies of
/// their values. Reading the torn store as empty used to overwrite it and delete the `.env`.
#[test]
fn an_export_that_cannot_read_a_file_prints_nothing_and_changes_nothing() {
    let scratch = Scratch::new();
    let home = scratch.home("torn");
    plant(&home, "env.json", "{ not json");
    plant(&home, ".env", "SWISS_TOKEN=legacy-token\n");
    plant(&home, "gateway.config.json", r#"{"tokenEnv":"SWISS_TOKEN","servers":{}}"#);

    let out = swiss(&home, &key("ab"), &["export"]);
    assert_eq!(out.status.code(), Some(1), "{}", text_of(&out.stderr));
    assert!(out.stdout.is_empty(), "stdout: {}", text_of(&out.stdout));
    assert!(text_of(&out.stderr).contains("env.json"), "{}", text_of(&out.stderr));
    assert_eq!(std::fs::read_to_string(home.join("env.json")).expect("kept"), "{ not json");
    assert_eq!(
        std::fs::read_to_string(home.join(".env")).expect("kept"),
        "SWISS_TOKEN=legacy-token\n"
    );
}

/// SPEC §host.cli: a vault this machine cannot open is not an empty one, and the sections before
/// it in the bundle do not land without it.
#[test]
fn an_import_over_a_vault_sealed_by_another_key_changes_nothing() {
    let scratch = Scratch::new();
    let home = scratch.home("home");
    let bundle = |name: &str, body: Value| {
        let file = scratch.home(name);
        std::fs::write(&file, body.to_string()).expect("write a bundle");
        file
    };
    let seed = bundle(
        "seed.json",
        json!({ "version": 1, "secrets": { "secrets": { "kept-name": "kept-value" } } }),
    );
    let seeded = swiss(&home, &key("ab"), &["import", seed.to_str().expect("utf-8 path")]);
    assert!(seeded.status.success(), "{}", text_of(&seeded.stderr));
    let sealed = std::fs::read(home.join("secrets.json")).expect("the sealed vault");

    let next = bundle(
        "next.json",
        json!({
            "version": 1,
            "config": { "tokenEnv": "SWISS_TOKEN", "servers": {} },
            "secrets": { "secrets": { "new-name": "new-value" } },
        }),
    );
    let refused = swiss(&home, &key("cd"), &["import", next.to_str().expect("utf-8 path")]);
    assert!(!refused.status.success(), "a vault it cannot open refuses the bundle");
    assert!(text_of(&refused.stderr).contains("secrets.json"), "{}", text_of(&refused.stderr));
    assert_eq!(std::fs::read(home.join("secrets.json")).expect("kept"), sealed);
    assert!(!home.join("gateway.config.json").exists(), "the config did not land either");
}
