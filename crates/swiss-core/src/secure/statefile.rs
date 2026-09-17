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

//! Encrypted JSON state — port of `secure/statefile.ts`. The ONLY way gateway state reaches disk.
//!
//! Every state file (gateway.config.json, managed.json, tunnels.json, env.json) is an AES-256-GCM
//! envelope sealed under the machine-bound master key. Reading accepts BOTH envelopes and legacy
//! plaintext JSON: a pre-encryption install (or a hand-authored file dropped into the data dir)
//! is read, then immediately re-saved sealed — the migration is "boot once and the plaintext is
//! gone". Writing only ever produces an envelope.

use std::path::Path;

use serde_json::Value;

use super::envelope::{self, Sealed};
use super::key::{self, MASTER_KEY_ENV};
use crate::atomic_json::{write_json_atomic, write_text_atomic};
use crate::log;

/// Read a state file. None when the file does not exist. Err when it cannot be opened.
pub fn read_secure_json(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{}: cannot read: {}", path.display(), err)),
    };
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: not valid JSON: {}", path.display(), e))?;

    if !envelope::is_sealed(&raw) {
        // Legacy plaintext (pre-encryption install, or a hand-authored file). Seal it now so the
        // plaintext window closes on first read; when no key is available at all, keep serving
        // the plaintext read-only rather than failing the boot on a file we cannot protect.
        match write_secure_json(path, &raw) {
            Ok(()) => log::log(
                "info",
                "sealed a plaintext state file",
                Some(serde_json::json!({ "path": path.display().to_string() })),
            ),
            Err(err) => log::log(
                "warn",
                "could not seal a plaintext state file",
                Some(serde_json::json!({ "path": path.display().to_string(), "err": err })),
            ),
        }
        return Ok(Some(raw));
    }

    let sealed: Sealed = serde_json::from_value(raw.clone())
        .map_err(|e| format!("{}: malformed envelope: {}", path.display(), e))?;
    let candidates = key::master_key_candidates()?;
    for cand in &candidates {
        match envelope::unseal(&cand.key, &sealed) {
            Ok(plaintext) => {
                let value: Value = serde_json::from_str(&plaintext).map_err(|e| {
                    format!("{}: decrypted payload is not JSON: {}", path.display(), e)
                })?;
                return Ok(Some(value));
            }
            Err(_) => continue, // wrong candidate — the tag decides, keep trying
        }
    }
    Err(format!(
        "{}: cannot decrypt with this machine's key (sealed by key source '{}'). \
         The file was copied from another machine, or the OS credential holding the key changed. \
         Run 'swiss export' on the machine that sealed it and 'swiss import' here, or set {}.",
        path.display(),
        sealed.key_source,
        MASTER_KEY_ENV
    ))
}

/// Write a state file as a sealed envelope, atomically (tmp + rename — see atomic_json.rs).
pub fn write_secure_json(path: &Path, data: &Value) -> Result<(), String> {
    let candidates = key::master_key_candidates()?;
    let Some(first) = candidates.first() else {
        return Err("no master key available to seal with".into());
    };
    let payload = serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".into());
    let envelope = envelope::seal(&first.key, first.id, &payload);
    let text =
        serde_json::to_string_pretty(&serde_json::to_value(&envelope).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    write_text_atomic(path, &text)
}

// Keep write_json_atomic linked in for the state layer's future non-sealed uses.
#[allow(dead_code)]
fn _unused(path: &Path, data: &Value) -> Result<(), String> {
    write_json_atomic(path, data)
}

#[cfg(test)]
mod tests {
    // Ported from the statefile half of test/secure-store.test.ts.
    use super::*;
    use serde_json::json;

    /// A scratch directory of this test's own. Nothing here reads the data dir: every path is
    /// passed in, which is what lets these run beside the rest of the suite.
    fn scratch() -> std::path::PathBuf {
        key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-statefile-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    /// State with a secret in it, so a leak into the file bytes is visible.
    fn secretive() -> Value {
        json!({
            "tokenEnv": "MCP_GATEWAY_TOKEN",
            "servers": { "db": { "type": "mysql", "password": "hunter2-in-the-clear" } },
        })
    }

    #[test]
    fn seals_on_write_and_hands_the_same_value_back_on_read() {
        let dir = scratch();
        let path = dir.join("gateway.config.json");
        write_secure_json(&path, &secretive()).expect("seal");
        assert_eq!(
            read_secure_json(&path).expect("readable"),
            Some(secretive())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_only_ever_produces_an_envelope() {
        // This is the one way gateway state reaches disk; a plaintext write here would put every
        // database password in the data dir in the clear.
        let dir = scratch();
        let path = dir.join("managed.json");
        write_secure_json(&path, &secretive()).expect("seal");

        let text = std::fs::read_to_string(&path).expect("the file is there");
        assert!(!text.contains("hunter2-in-the-clear"), "{text}");
        assert!(!text.contains("mysql"), "{text}");
        let raw: Value = serde_json::from_str(&text).expect("an envelope is still JSON");
        assert!(envelope::is_sealed(&raw), "{raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_write_replaces_the_first_and_reseals_under_a_fresh_nonce() {
        // A repeated nonce under one key is what breaks GCM outright, so two seals of the SAME
        // value must not produce the same ciphertext.
        let dir = scratch();
        let path = dir.join("tunnels.json");
        write_secure_json(&path, &secretive()).expect("seal");
        let first = std::fs::read_to_string(&path).expect("the file is there");
        write_secure_json(&path, &secretive()).expect("re-seal");
        let second = std::fs::read_to_string(&path).expect("the file is there");

        assert_ne!(first, second);
        assert_eq!(
            read_secure_json(&path).expect("readable"),
            Some(secretive())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_there_is_not_an_error() {
        // Every loader asks before the first boot has written anything.
        let dir = scratch();
        assert_eq!(read_secure_json(&dir.join("never-written.json")), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_file_that_is_not_json_rather_than_guessing() {
        let dir = scratch();
        let path = dir.join("torn.json");
        std::fs::write(&path, "{\"port\": 199").expect("write a torn file");
        let err = read_secure_json(&path).expect_err("a torn state file is an error");
        assert!(err.contains("not valid JSON"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adopts_a_hand_written_plaintext_file_and_seals_it_on_the_first_read() {
        // A pre-encryption install, or a config dropped into the data dir by hand: it is read as
        // written, then re-saved sealed, so the plaintext window closes on the first boot.
        let dir = scratch();
        let path = dir.join("gateway.config.json");
        std::fs::write(&path, secretive().to_string()).expect("plant a plaintext config");

        assert_eq!(
            read_secure_json(&path).expect("readable"),
            Some(secretive())
        );

        let after = std::fs::read_to_string(&path).expect("the file is there");
        assert!(!after.contains("hunter2-in-the-clear"), "{after}");
        assert!(envelope::is_sealed(
            &serde_json::from_str(&after).expect("still JSON")
        ));
        // And it still reads back as the same state, now through the envelope.
        assert_eq!(
            read_secure_json(&path).expect("readable"),
            Some(secretive())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_malformed_envelope() {
        let dir = scratch();
        let path = dir.join("bent.json");
        // Sealed-looking enough to take the envelope branch (swiss + alg + ct decide that), but
        // missing a field the envelope needs — the shape a hand-edited file ends up in.
        let mut raw = serde_json::to_value(envelope::seal(&[7u8; 32], "dpapi", "{}"))
            .expect("an envelope serializes");
        raw.as_object_mut().expect("an object").remove("iv");
        std::fs::write(&path, raw.to_string()).expect("write");

        let err = read_secure_json(&path).expect_err("a bent envelope is an error");
        assert!(err.contains("malformed envelope"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_sealed_on_another_machine_is_refused_with_a_way_out() {
        // Machine binding cuts both ways: the message has to name the escape hatch, or a copied
        // data dir looks like data loss.
        let dir = scratch();
        let path = dir.join("foreign.json");
        let sealed = envelope::seal(&[9u8; 32], "dpapi", &secretive().to_string());
        std::fs::write(
            &path,
            serde_json::to_string(&sealed).expect("an envelope serializes"),
        )
        .expect("write");

        let err = read_secure_json(&path).expect_err("another machine's key cannot open it");
        assert!(err.contains("cannot decrypt"), "{err}");
        assert!(err.contains("swiss export"), "{err}");
        assert!(err.contains(MASTER_KEY_ENV), "{err}");
        assert!(err.contains("dpapi"), "{err}"); // which source sealed it
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        // The write is tmp + rename, and a torn gateway.config.json is the one file this gateway
        // cannot boot through.
        let dir = scratch();
        write_secure_json(&dir.join("managed.json"), &secretive()).expect("seal");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("the directory is readable")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["managed.json".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
