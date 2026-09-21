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

//! The encrypted replacement for the data-dir '.env' — port of `secure/envstore.ts`.
//!
//! Secrets used to sit in the data dir's '.env' as plaintext KEY=VALUE lines. They now live in
//! 'env.json' — a sealed envelope holding a JSON object of key -> value, opened only by the
//! machine key. A legacy '.env' is consumed on first read: parsed, merged into the sealed store,
//! verified, and only then deleted, so the plaintext copy cannot survive past the boot that saw
//! it.
//!
//! The Node build injected the store into process.env; Rust's process env is not safely mutable
//! from edition 2024, so the store lives in an in-process overlay instead. `env_lookup` reads the
//! overlay with the process env as fallback (the overlay never overrides what the OS or service
//! manager injected — dotenv semantics), and every `${ENV_VAR}` expansion goes through it. Child
//! processes (proc MCPs) receive the overlay merged in explicitly at spawn time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::RwLock;

use super::statefile::{read_secure_json, write_secure_json};
use crate::paths::data_path;

/// Where the encrypted env store lives.
pub fn env_store_path() -> PathBuf {
    data_path(&["env.json"])
}

static OVERLAY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn overlay() -> &'static RwLock<HashMap<String, String>> {
    OVERLAY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Parse .env-style text: KEY=value, 'export KEY=value', # comments, optional matching quotes.
pub fn parse_env_text(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);
        let Some(eq) = line.find('=') else { continue };
        if eq == 0 {
            continue;
        }
        let key = line[..eq].trim();
        let mut value = line[eq + 1..].trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        if is_env_key(key) {
            out.insert(key.to_string(), value.to_string());
        }
    }
    out
}

/// KEY grammar: [A-Za-z_][A-Za-z0-9_]* — hand-rolled, the regex this replaces had exactly this
/// shape (ADR-007: no regex crate for one trivial pattern).
fn is_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn read_store(path: &Path) -> HashMap<String, String> {
    let Ok(Some(raw)) = read_secure_json(path) else {
        return HashMap::new();
    };
    let Some(obj) = raw.as_object() else {
        return HashMap::new();
    };
    obj.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

/// Overwrite the sealed store in one shot.
pub fn write_env_store(store: &HashMap<String, String>, path: &Path) -> Result<(), String> {
    let value = serde_json::to_value(store).map_err(|e| e.to_string())?;
    write_secure_json(path, &value)
}

/// The sealed env store, with any legacy plaintext '.env' folded in and removed.
///
/// Deletion is verify-then-delete: the just-written sealed store is read back and must contain
/// every legacy key with its exact value before the plaintext file goes. A failed seal leaves
/// '.env' in place and the merged values readable in-process, so the boot still works and the
/// next boot retries.
pub fn read_env_store(path: &Path) -> HashMap<String, String> {
    let mut store = read_store(path);
    let legacy_path = data_path(&[".env"]);
    let Ok(legacy_text) = std::fs::read_to_string(&legacy_path) else {
        return store;
    };
    let legacy = parse_env_text(&legacy_text);
    // Sealed values win: they were written later.
    let mut merged = legacy.clone();
    merged.extend(store.iter().map(|(k, v)| (k.clone(), v.clone())));
    match write_env_store(&merged, path).map(|_| {
        let verify = read_store(path);
        let safe = legacy.iter().all(|(k, v)| verify.get(k) == Some(v));
        safe
    }) {
        Ok(true) => {
            let _ = std::fs::remove_file(&legacy_path);
            crate::log::log(
                "info",
                "moved the plaintext .env into the encrypted store",
                Some(serde_json::json!({ "keys": legacy.len() })),
            );
            store = merged;
        }
        _ => {
            crate::log::log(
                "warn",
                "could not migrate the plaintext .env; leaving it in place",
                None,
            );
            store = merged;
        }
    }
    store
}

/// Set 'key' only when absent; true when it was written (the ensureEnvKey contract).
pub fn set_env_default(key: &str, value: &str, path: &Path) -> Option<String> {
    let mut store = read_env_store(path);
    if let Some(existing) = store.get(key) {
        return Some(existing.clone());
    }
    store.insert(key.to_string(), value.to_string());
    if write_env_store(&store, path).is_err() {
        return None;
    }
    Some(value.to_string())
}

/// Load the sealed store into the in-process overlay — the load-time step of the credential
/// model. Existing environment variables always win: what the OS or the service manager injected
/// is never overridden (dotenv semantics). Returns the loaded store for callers that need values
/// eagerly (bootstrap's token report).
pub fn inject_env_store(path: &Path) -> HashMap<String, String> {
    let store = read_env_store(path);
    if let Ok(mut overlay) = overlay().write() {
        for (k, v) in &store {
            if std::env::var(k).is_err() {
                overlay.insert(k.clone(), v.clone());
            }
        }
    }
    store
}

/// One environment lookup: process env first, then the sealed overlay. This is what
/// `${ENV_VAR}` expansion resolves against.
pub fn env_lookup(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        return Some(v);
    }
    overlay().read().ok()?.get(name).cloned()
}

/// The overlay contents, for spawning child processes with the merged environment.
pub fn overlay_env() -> HashMap<String, String> {
    overlay().read().map(|m| m.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // Ported from the envstore half of test/secure-store.test.ts.
    use super::*;

    #[test]
    fn parses_env_text_shapes() {
        let mut expect = HashMap::new();
        expect.insert("A".to_string(), "1".to_string());
        expect.insert("B".to_string(), "hello world".to_string());
        expect.insert("C".to_string(), "quoted".to_string());
        expect.insert("D".to_string(), "sq".to_string());
        expect.insert("E".to_string(), "".to_string());
        assert_eq!(
            parse_env_text("# comment\nA=1\nB=hello world\nC=\"quoted\"\nD='sq'\nexport E=\nnot a pair\n9bad=x\n"),
            expect
        );
    }

    #[test]
    fn quoted_value_keeps_inner_spaces() {
        let m = parse_env_text("X=\"a b c\"\n");
        assert_eq!(m.get("X").map(String::as_str), Some("a b c"));
    }
}
