//! The secret vault (docs/19) — a namespace-isolated store for credential values that other
//! services reference as `secret://name` strings.
//!
//! Why it is NOT the env store: the env store doubles as the environment every child process
//! reads (envstore.rs merges its overlay into each spawn). A key saved for one http MCP has no
//! business being readable by every proc MCP, job script and local shell — so the vault keeps
//! its own file, its own lookup path, and no merge anywhere. Values only ever leave through
//! `refs::resolve` at a use point (docs/19 D4), and no API reads one back (docs/19 D5: write
//! only; a forgotten secret is re-stored, never revealed).
//!
//! The file is `secrets.json` — the same sealed envelope as every state file (statefile.rs),
//! device-bound through the DPAPI-protected master key (docs/05). Copying it to another
//! machine, or another Windows user, yields ciphertext nobody can open, by design; `swiss
//! export` is the one sanctioned plaintext escape.
//!
//! Envelope shape: `{"rev": N, "secrets": {name -> value}}`. The rev is the discipline the
//! plugins API already has: every mutation bumps it and must name the rev it was planned
//! against, so two panels cannot silently overwrite each other. (docs/19 D2 said "flat like the
//! env store"; the rev needs one counter, so the values are flat under one key — recorded as a
//! spec deviation in the commit that adds this file.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use super::statefile::{read_secure_json, write_secure_json};
use crate::paths::data_path;

/// Where the sealed vault lives.
pub fn secret_store_path() -> PathBuf {
    data_path(&["secrets.json"])
}

/// Vault names are lowercase kebab — `[a-z][a-z0-9-]{0,63}` (docs/19 D1). Lowercase on
/// purpose: a different character class than `${UPPER_ENV}` refs, impossible to confuse by
/// eye or by grammar; kebab to match the plugin and page ids the panel already speaks.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// The in-process vault, loaded once at boot (same pattern as envstore's overlay). Values stay
/// resident because connect-time and run-time expansion needs them without re-reading the
/// sealed file; what is NOT resident is any copy in a child environment — nothing merges this
/// map into a spawn (docs/19 D8).
pub struct Vault {
    pub rev: u64,
    pub secrets: HashMap<String, String>,
}

static VAULT: OnceLock<RwLock<Vault>> = OnceLock::new();

fn vault() -> &'static RwLock<Vault> {
    VAULT.get_or_init(|| RwLock::new(Vault { rev: 0, secrets: HashMap::new() }))
}

/// Read the sealed file into a Vault. An absent file is an empty vault at rev 0 — first run.
fn read_file(path: &Path) -> Vault {
    let Ok(Some(raw)) = read_secure_json(path) else {
        return Vault { rev: 0, secrets: HashMap::new() };
    };
    Vault {
        rev: raw.get("rev").and_then(|v| v.as_u64()).unwrap_or(0),
        secrets: raw
            .get("secrets")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The load-time step: fill the in-process vault. Called once at boot, next to
/// `inject_env_store`.
pub fn inject_vault(path: &Path) {
    let loaded = read_file(path);
    if let Ok(mut v) = vault().write() {
        *v = loaded;
    }
}

/// The one lookup a `secret://name` reference resolves against (refs.rs).
pub fn vault_lookup(name: &str) -> Option<String> {
    vault().read().ok()?.secrets.get(name).cloned()
}

/// Names, sorted — all a listing may ever see.
pub fn list_secrets() -> Vec<String> {
    match vault().read() {
        Ok(v) => {
            let mut names: Vec<String> = v.secrets.keys().cloned().collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

/// The revision every mutation must name (docs/19 D5).
pub fn vault_rev() -> u64 {
    vault().read().map(|v| v.rev).unwrap_or(0)
}

/// What a mutation can fail with; the API layer maps these to 400 / 404 / 409 / 500.
#[derive(Debug)]
pub enum MutateError {
    /// The name is not lowercase kebab — the message explains the grammar.
    InvalidName(String),
    /// Someone else mutated the vault after the caller read its rev.
    RevMismatch { have: u64, saw: u64 },
    /// DELETE of a name that is not stored.
    NotFound,
    /// The sealed write failed; nothing changed in memory (persist-then-commit).
    Seal(String),
}

impl MutateError {
    pub fn message(&self) -> String {
        match self {
            MutateError::InvalidName(name) => format!(
                "invalid secret name '{name}': secret names are lowercase kebab ([a-z][a-z0-9-]{{0,63}})"
            ),
            MutateError::RevMismatch { have, saw } => format!(
                "the vault changed since rev {saw} (now at {have}) — reload and retry"
            ),
            MutateError::NotFound => "no such secret".into(),
            MutateError::Seal(e) => format!("could not seal the vault: {e}"),
        }
    }
}

/// Persist a candidate vault, then commit it to the static on success — a failed seal leaves
/// the in-process vault exactly as it was, so a later PUT built on the old rev still matches.
fn persist_then_commit(path: &Path, candidate: Vault) -> Result<u64, MutateError> {
    let value = serde_json::json!({ "rev": candidate.rev, "secrets": candidate.secrets });
    write_secure_json(path, &value).map_err(MutateError::Seal)?;
    let rev = candidate.rev;
    if let Ok(mut v) = vault().write() {
        *v = candidate;
    }
    Ok(rev)
}

/// Store or overwrite one secret (docs/19 D5). `expect_rev` is the rev the caller saw.
pub fn put_secret(path: &Path, name: &str, value: &str, expect_rev: u64) -> Result<u64, MutateError> {
    if !valid_name(name) {
        return Err(MutateError::InvalidName(name.to_string()));
    }
    let current = vault().read().map(|v| (v.rev, v.secrets.clone())).ok();
    let Some((rev, mut secrets)) = current else {
        return Err(MutateError::Seal("vault lock poisoned".into()));
    };
    if rev != expect_rev {
        return Err(MutateError::RevMismatch { have: rev, saw: expect_rev });
    }
    secrets.insert(name.to_string(), value.to_string());
    persist_then_commit(path, Vault { rev: rev + 1, secrets })
}

/// Bulk restore for 'swiss import' (docs/19 D7): merge imported entries into the vault,
/// imported values winning over what is stored — the same semantics the env section of a
/// bundle has. Nothing is deleted: a name absent from the bundle keeps its value, because an
/// import is a restore, not a mirror. Entries whose name is not valid or whose value is not a
/// string are skipped, not fatal — the env section of a bundle tolerates the same, and one bad
/// hand-edited entry must not cost the operator the other forty.
pub fn import_secrets(
    path: &Path,
    entries: &serde_json::Map<String, serde_json::Value>,
) -> Result<u64, MutateError> {
    // Merge against the FILE, not the in-process vault: 'swiss import' runs in a fresh CLI
    // process where nothing has injected the vault yet, and the merge promise (a name the
    // bundle does not mention keeps its value) is about what is stored on disk.
    let Vault { rev, mut secrets } = read_file(path);
    for (name, value) in entries {
        if !valid_name(name) {
            continue;
        }
        if let Some(s) = value.as_str() {
            secrets.insert(name.clone(), s.to_string());
        }
    }
    persist_then_commit(path, Vault { rev: rev + 1, secrets })
}

/// Remove one secret. Absent names answer NotFound rather than quietly bumping the rev.
pub fn delete_secret(path: &Path, name: &str, expect_rev: u64) -> Result<u64, MutateError> {
    let current = vault().read().map(|v| (v.rev, v.secrets.clone())).ok();
    let Some((rev, mut secrets)) = current else {
        return Err(MutateError::Seal("vault lock poisoned".into()));
    };
    if rev != expect_rev {
        return Err(MutateError::RevMismatch { have: rev, saw: expect_rev });
    }
    if secrets.remove(name).is_none() {
        return Err(MutateError::NotFound);
    }
    persist_then_commit(path, Vault { rev: rev + 1, secrets })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_ok(n: &str) {
        assert!(valid_name(n), "{n} should be a valid name");
    }
    fn name_bad(n: &str) {
        assert!(!valid_name(n), "{n} should be rejected");
    }

    #[test]
    fn names_are_lowercase_kebab() {
        name_ok("a");
        name_ok("stripe-key");
        name_ok("context7");
        name_ok("a-b-c-123");
        // The grammar is [a-z][a-z0-9-]{0,63} verbatim (docs/19 D1): a trailing dash is
        // ugly but legal, and inventing extra rules is not this function's job.
        name_ok("trailing-dash-");
        name_bad("");
        name_bad("A");
        name_bad("Upper-Kebab");
        name_bad("under_score");
        name_bad("1starts-with-digit");
        name_bad("-leading-dash");
        name_bad(&"x".repeat(65));
    }

    #[test]
    fn put_then_lookup_then_delete_round_trips() {
        let _guard = crate::paths::DATA_DIR_LOCK.blocking_lock();
        let home = crate::paths::test_home();
        let path = home.join("secrets.json");
        inject_vault(&path);

        let rev0 = vault_rev();
        let rev1 = put_secret(&path, "stripe-key", "sk_live_abcd", rev0).expect("first put");
        assert_eq!(rev1, rev0 + 1);
        assert_eq!(vault_lookup("stripe-key").as_deref(), Some("sk_live_abcd"));
        // Containment, not equality: one in-process vault serves the whole test binary, and
        // other tests plant their own uniquely-named secrets into it.
        assert!(list_secrets().contains(&"stripe-key".to_string()));

        // The sealed file must not carry the plaintext.
        let raw = std::fs::read(&path).expect("the vault file exists");
        let raw_text = String::from_utf8_lossy(&raw);
        assert!(!raw_text.contains("sk_live_abcd"), "the value never lands in the clear");

        // Wrong rev answers a mismatch carrying both numbers.
        match put_secret(&path, "other", "v", rev0) {
            Err(MutateError::RevMismatch { have, saw }) => {
                assert_eq!((have, saw), (rev1, rev0));
            }
            other => panic!("expected a rev mismatch, got {other:?}"),
        }

        let rev2 = delete_secret(&path, "stripe-key", rev1).expect("delete");
        assert_eq!(vault_lookup("stripe-key"), None);
        assert!(!list_secrets().contains(&"stripe-key".to_string()));
        match delete_secret(&path, "stripe-key", rev2) {
            Err(MutateError::NotFound) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_name_never_mutates_anything() {
        let _guard = crate::paths::DATA_DIR_LOCK.blocking_lock();
        let home = crate::paths::test_home();
        let path = home.join("secrets.json");
        inject_vault(&path);
        let rev0 = vault_rev();
        match put_secret(&path, "BadName", "v", rev0) {
            Err(e @ MutateError::InvalidName(_)) => {
                let msg = e.message();
                assert!(msg.contains("lowercase kebab"), "explains the grammar: {msg}");
            }
            other => panic!("expected InvalidName, got {other:?}"),
        }
        // Nothing changed — not the rev, not the vault.
        assert_eq!(vault_rev(), rev0, "a rejected name does not bump the rev");
        assert_eq!(vault_lookup("BadName"), None);
    }

    #[test]
    fn inject_loads_a_pre_sealed_vault() {
        let _guard = crate::paths::DATA_DIR_LOCK.blocking_lock();
        let home = crate::paths::test_home();
        let path = home.join("secrets.json");
        inject_vault(&path);
        let rev = put_secret(&path, "reload-me", "v1", vault_rev()).expect("put");
        // A fresh process would call inject_vault on the existing file.
        inject_vault(&path);
        assert_eq!(vault_rev(), rev);
        assert_eq!(vault_lookup("reload-me").as_deref(), Some("v1"));
        let _ = delete_secret(&path, "reload-me", vault_rev());
    }
}
