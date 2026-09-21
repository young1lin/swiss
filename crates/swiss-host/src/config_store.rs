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

//! One shared, revision-checked writer for plugin configuration. Runtime facts never enter it.
//! Both legacy root rows and versioned plugin rows are projected without migrating files at boot.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use swiss_core::secure::statefile::{read_secure_json, write_secure_json};

const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ConfigSnapshot {
    pub revision: u64,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigStoreError {
    Conflict,
    Invalid(String),
    Persist(String),
}

impl std::fmt::Display for ConfigStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => f.write_str("configuration changed; reload before saving"),
            Self::Invalid(message) | Self::Persist(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ConfigStoreError {}

struct State {
    snapshot: ConfigSnapshot,
    existed: bool,
}

pub struct ConfigStore {
    path: Option<PathBuf>,
    state: Mutex<State>,
}

/// A content revision fits exactly in a JavaScript number and survives process restarts.
/// No revision metadata is inserted into legacy configuration files.
fn revision(raw: &Value) -> u64 {
    let bytes = serde_json::to_vec(raw).unwrap_or_default();
    let hash = Sha256::digest(bytes);
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&hash[..8]);
    u64::from_be_bytes(prefix) >> 11
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !matches!(
            id,
            "host" | "port" | "tokenEnv" | "servers" | "plugins" | "schemaVersion"
        )
}

/// A versioned row takes precedence even when it contains no configuration.
pub fn plugin_row<'a>(raw: &'a Value, id: &str) -> Option<&'a Value> {
    raw.get("plugins")
        .and_then(|plugins| plugins.get(id))
        .or_else(|| raw.get(id))
}

pub fn plugin_config(raw: &Value, id: &str) -> Value {
    plugin_row(raw, id)
        .and_then(|row| row.get("config"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

pub fn plugin_disabled(raw: &Value, id: &str) -> bool {
    plugin_row(raw, id)
        .and_then(|row| row.get("disabled"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Legacy whole-value bare vault refs become envelope refs before any snapshot is published
/// (docs/25 E2). In memory only: the file keeps its spelling until the next save rewrites
/// it, so a boot never rewrites state just to re-spell a reference.
fn normalize_legacy_refs(path: &std::path::Path, mut raw: Value) -> Value {
    let n = swiss_core::secure::refs::migrate_legacy(&mut raw);
    if n > 0 {
        swiss_core::log::info(&format!(
            "{}: {n} legacy secret:// reference(s) migrated to ${{secret://...}}",
            path.file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        ));
    }
    raw
}

impl ConfigStore {
    pub fn from_loaded(path: PathBuf, raw: Value) -> Arc<Self> {
        let raw = normalize_legacy_refs(&path, raw);
        let existed = path.exists();
        Arc::new(Self {
            path: Some(path),
            state: Mutex::new(State {
                snapshot: ConfigSnapshot {
                    revision: revision(&raw),
                    raw,
                },
                existed,
            }),
        })
    }

    pub fn memory(raw: Value) -> Arc<Self> {
        // Same normalization as from_loaded: every snapshot the process can see speaks the
        // envelope grammar, whatever its caller handed in.
        let mut raw = raw;
        swiss_core::secure::refs::migrate_legacy(&mut raw);
        Arc::new(Self {
            path: None,
            state: Mutex::new(State {
                snapshot: ConfigSnapshot {
                    revision: revision(&raw),
                    raw,
                },
                existed: false,
            }),
        })
    }

    pub fn snapshot(&self) -> ConfigSnapshot {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }

    pub fn plugin_config(&self, id: &str) -> Value {
        plugin_config(
            &self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot
                .raw,
            id,
        )
    }

    pub fn plugin_disabled(&self, id: &str) -> bool {
        plugin_disabled(
            &self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot
                .raw,
            id,
        )
    }

    pub fn update_plugin(
        &self,
        id: &str,
        expected: u64,
        config: Value,
    ) -> Result<ConfigSnapshot, ConfigStoreError> {
        if !config.is_object() {
            return Err(ConfigStoreError::Invalid(
                "plugin config must be an object".into(),
            ));
        }
        self.update_row(id, expected, |row| {
            row.insert("config".into(), config);
        })
    }

    pub fn set_plugin_disabled(
        &self,
        id: &str,
        expected: u64,
        disabled: bool,
    ) -> Result<ConfigSnapshot, ConfigStoreError> {
        self.update_row(id, expected, |row| {
            row.insert("disabled".into(), Value::Bool(disabled));
        })
    }

    fn update_row(
        &self,
        id: &str,
        expected: u64,
        apply: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<ConfigSnapshot, ConfigStoreError> {
        if !valid_id(id) {
            return Err(ConfigStoreError::Invalid(
                "invalid or reserved plugin id".into(),
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.snapshot.revision != expected {
            return Err(ConfigStoreError::Conflict);
        }
        // Detect a writer outside this process before replacing its work. Online writers share
        // this store; offline writers must not race a running instance. This is not a file lock.
        if let Some(path) = &self.path {
            let disk = read_secure_json(path).map_err(ConfigStoreError::Persist)?;
            match disk {
                Some(raw) if raw != state.snapshot.raw => {
                    state.snapshot = ConfigSnapshot {
                        revision: revision(&raw),
                        raw,
                    };
                    state.existed = true;
                    return Err(ConfigStoreError::Conflict);
                }
                None if state.existed => return Err(ConfigStoreError::Conflict),
                _ => {}
            }
        }
        let mut next = state.snapshot.raw.clone();
        let root = next
            .as_object_mut()
            .ok_or_else(|| ConfigStoreError::Invalid("config root must be an object".into()))?;
        let versioned = root.get("schemaVersion").and_then(Value::as_u64) == Some(2)
            || root.contains_key("plugins");
        let rows = if versioned {
            root.entry("plugins")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| ConfigStoreError::Invalid("plugins must be an object".into()))?
        } else {
            root
        };
        let row = rows
            .entry(id)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| {
                ConfigStoreError::Invalid(format!("plugin {id} row must be an object"))
            })?;
        apply(row);
        let bytes =
            serde_json::to_vec(&next).map_err(|e| ConfigStoreError::Invalid(e.to_string()))?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ConfigStoreError::Invalid(
                "configuration exceeds the 2 MiB budget".into(),
            ));
        }
        if let Some(path) = &self.path {
            write_secure_json(path, &next).map_err(ConfigStoreError::Persist)?;
            state.existed = true;
        }
        state.snapshot = ConfigSnapshot {
            revision: revision(&next),
            raw: next,
        };
        Ok(state.snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_on_and_versioned_rows_override_legacy_rows() {
        let store = ConfigStore::memory(json!({"jobs":{"disabled":true},"plugins":{"jobs":{}}}));
        assert!(!store.plugin_disabled("jobs"));
        assert!(!store.plugin_disabled("data"));
        assert_eq!(store.plugin_config("jobs"), json!({}));
    }

    #[test]
    fn compare_and_swap_preserves_other_rows_and_env_references() {
        let store = ConfigStore::memory(
            json!({"tokenEnv":"TOKEN","jobs":{"disabled":true},"servers":{"db":{"password":"${DB_PASSWORD}"}}}),
        );
        let original = store.snapshot();
        let updated = store
            .update_plugin("jobs", original.revision, json!({"definitions":{}}))
            .expect("valid config");
        assert!(store.plugin_disabled("jobs"));
        assert_eq!(updated.raw["servers"], original.raw["servers"]);
        assert_ne!(updated.revision, original.revision);
        assert!(updated.revision <= 9_007_199_254_740_991);
        assert_eq!(
            store
                .set_plugin_disabled("jobs", original.revision, false)
                .unwrap_err(),
            ConfigStoreError::Conflict
        );
        store
            .set_plugin_disabled("jobs", updated.revision, false)
            .expect("current revision");
        assert!(!store.plugin_disabled("jobs"));
    }

    #[test]
    fn versioned_updates_do_not_migrate_legacy_roots() {
        let store = ConfigStore::memory(
            json!({"schemaVersion":2,"plugins":{"jobs":{"kind":"jobs","disabled":true}},"servers":{}}),
        );
        let result = store
            .update_plugin(
                "jobs",
                store.snapshot().revision,
                json!({"maxConcurrentRuns":2}),
            )
            .expect("save");
        assert_eq!(result.raw["plugins"]["jobs"]["kind"], "jobs");
        assert!(result.raw.get("jobs").is_none());
        assert!(store.plugin_disabled("jobs"));
    }

    #[test]
    fn failed_persistence_never_publishes_a_successful_revision() {
        let path = std::env::temp_dir().join(format!(
            "swiss-config-failure-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::write(&path, b"not a directory").expect("fixture");
        let store = ConfigStore::from_loaded(path.join("config.json"), json!({}));
        let before = store.snapshot();
        assert!(matches!(
            store.update_plugin("jobs", before.revision, json!({})),
            Err(ConfigStoreError::Persist(_))
        ));
        assert_eq!(store.snapshot().raw, before.raw);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sealed_roundtrip_and_outside_changes_conflict() {
        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-config-store-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("fixture");
        let path = dir.join("gateway.config.json");
        let raw = json!({"tokenEnv":"TOKEN","servers":{}});
        write_secure_json(&path, &raw).expect("seed");
        let store = ConfigStore::from_loaded(path.clone(), raw);
        let updated = store
            .update_plugin("jobs", store.snapshot().revision, json!({"definitions":{}}))
            .expect("sealed write");
        assert_eq!(
            read_secure_json(&path).expect("read"),
            Some(updated.raw.clone())
        );
        let reopened = ConfigStore::from_loaded(path.clone(), updated.raw);
        assert_eq!(reopened.snapshot().revision, updated.revision);
        write_secure_json(&path, &json!({"jobs":{"disabled":true}})).expect("outside writer");
        assert_eq!(
            store
                .set_plugin_disabled("data", updated.revision, true)
                .unwrap_err(),
            ConfigStoreError::Conflict
        );
        assert!(store.plugin_disabled("jobs"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_bare_vault_refs_load_as_envelope_refs() {
        // docs/25 E2 at the config surface: whole-value bare refs migrate in memory at
        // load; mixed strings stay byte-identical; the disk file is untouched by the load
        // and catches up only when a later save rewrites it.
        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-config-refs-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("fixture");
        let path = dir.join("gateway.config.json");
        let seeded = json!({
            "servers": { "echo": { "headers": {
                "X-Key": "secret://legacy-a",
                "Mix": "Bearer secret://legacy-b"
            }}}
        });
        write_secure_json(&path, &seeded).expect("seed");
        let raw = read_secure_json(&path).expect("read").expect("present");
        let store = ConfigStore::from_loaded(path.clone(), raw);
        let snap = store.snapshot();
        assert_eq!(
            snap.raw["servers"]["echo"]["headers"]["X-Key"],
            "${secret://legacy-a}"
        );
        assert_eq!(
            snap.raw["servers"]["echo"]["headers"]["Mix"],
            "Bearer secret://legacy-b"
        );
        // The disk keeps its legacy spelling until the next save.
        let disk = read_secure_json(&path).expect("re-read").expect("present");
        assert_eq!(
            disk["servers"]["echo"]["headers"]["X-Key"],
            "secret://legacy-a"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_shape_reserved_ids_and_oversized_configs_are_refused() {
        let store = ConfigStore::memory(json!({}));
        let rev = store.snapshot().revision;
        for id in ["", "../jobs", "servers", "plugins"] {
            assert!(store.update_plugin(id, rev, json!({})).is_err());
        }
        assert!(store.update_plugin("jobs", rev, json!([])).is_err());
        assert!(store
            .update_plugin("jobs", rev, json!({"large":"x".repeat(MAX_CONFIG_BYTES)}))
            .is_err());
        assert_eq!(store.snapshot().revision, rev);
    }
}
