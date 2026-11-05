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

//! Encrypted JSON state — port of `secure/statefile.ts`. The ONLY way gateway state reaches disk.
//!
//! Every state file (gateway.config.json, managed.json, tunnels.json, env.json) is an AES-256-GCM
//! envelope sealed under the machine-bound master key. Reading accepts BOTH envelopes and legacy
//! plaintext JSON: a pre-encryption install (or a hand-authored file dropped into the data dir)
//! is read, then immediately re-saved sealed — the migration is "boot once and the plaintext is
//! gone". Writing only ever produces an envelope.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::envelope::{self, Sealed};
use super::key::{self, MASTER_KEY_ENV_SWISS};
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
        MASTER_KEY_ENV_SWISS
    ))
}

/// Write a state file as a sealed envelope, atomically (tmp + rename — see atomic_json.rs).
pub fn write_secure_json(path: &Path, data: &Value) -> Result<(), String> {
    write_text_atomic(path, &seal_text(data)?)
}

/// The envelope a value seals to, as the text that lands in the file.
fn seal_text(data: &Value) -> Result<String, String> {
    let candidates = key::master_key_candidates()?;
    let Some(first) = candidates.first() else {
        return Err("no master key available to seal with".into());
    };
    let payload = serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".into());
    let envelope = envelope::seal(&first.key, first.id, &payload);
    serde_json::to_string_pretty(&serde_json::to_value(&envelope).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// What a refusal adds to the reason a state file could not be read (SPEC §core.sealed).
pub fn left_in_place(err: String) -> String {
    format!(
        "{err} (The file stays as it is: nothing writes over a state file that cannot be read. \
         Restore it, or move it aside to start without it.)"
    )
}

/// Is the file absent, or readable and of a shape its store `holds`? Anything else is refused.
fn check_replaceable(path: &Path, holds: fn(&Value) -> bool) -> Result<(), String> {
    match read_secure_json(path).map_err(left_in_place)? {
        Some(found) if !holds(&found) => Err(left_in_place(format!(
            "{}: not the shape this store holds",
            path.display()
        ))),
        _ => Ok(()),
    }
}

/// Write over a state file whose contents exist nowhere else. Refused when the file is there and
/// cannot be opened, or holds a shape `holds` does not accept (SPEC §core.sealed): the store read
/// it as empty at boot, so a write would swap everything it holds for one edit, and a key that
/// comes back could no longer recover it. Files a boot rewrites anyway (the session, run state) use
/// [write_secure_json].
pub fn replace_secure_json(
    path: &Path,
    data: &Value,
    holds: fn(&Value) -> bool,
) -> Result<(), String> {
    check_replaceable(path, holds)?;
    write_secure_json(path, data)
}

/// Replace several state files as one (SPEC §core.sealed). Each holds a JSON object. Any file that
/// cannot be read, or holds something else, refuses the whole set before anything is written,
/// everything is sealed before the first file changes, and a write that fails puts back the ones
/// before it.
pub fn replace_all_secure_json(files: &[(PathBuf, Value)]) -> Result<(), String> {
    replace_all_with(files, write_text_atomic)
}

fn replace_all_with(
    files: &[(PathBuf, Value)],
    mut write: impl FnMut(&Path, &str) -> Result<(), String>,
) -> Result<(), String> {
    let mut plan = Vec::with_capacity(files.len());
    for (path, data) in files {
        check_replaceable(path, Value::is_object)?;
        // Taken after the read above, which seals a legacy plaintext file: the bytes put back are
        // never plaintext. Absent means there is nothing to put back, and the file goes again.
        let before = std::fs::read_to_string(path).ok();
        plan.push((path.as_path(), seal_text(data)?, before));
    }
    for (landed, (path, sealed, _)) in plan.iter().enumerate() {
        let Err(err) = write(path, sealed) else {
            continue;
        };
        let mut stuck = Vec::new();
        for (path, _, before) in &plan[..landed] {
            let back = match before {
                Some(text) => write(path, text),
                None => std::fs::remove_file(path).map_err(|e| e.to_string()),
            };
            if let Err(why) = back {
                stuck.push(format!("{}: {why}", path.display()));
            }
        }
        return Err(if stuck.is_empty() {
            format!("{err}; every file is back as it was")
        } else {
            format!("{err}; these could not be put back: {}", stuck.join(", "))
        });
    }
    Ok(())
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
            "tokenEnv": "SWISS_TOKEN",
            "servers": { "db": { "type": "mysql", "password": "hunter2-in-the-clear" } },
        })
    }

    // SPEC §core.sealed: a file that cannot be read is still somebody's only copy.
    #[test]
    fn a_file_that_cannot_be_read_is_never_written_over() {
        let dir = scratch();
        let torn = dir.join("torn.json");
        std::fs::write(&torn, "{ not json").expect("plant a torn file");
        let err = replace_secure_json(&torn, &json!({ "fresh": true }), Value::is_object).unwrap_err();
        assert!(err.contains("stays as it is"), "{err}");
        assert_eq!(std::fs::read_to_string(&torn).expect("still there"), "{ not json");

        // An absent file and a readable one are ordinary writes.
        let plain = dir.join("plain.json");
        replace_secure_json(&plain, &json!({ "n": 1 }), Value::is_object).expect("absent");
        replace_secure_json(&plain, &json!({ "n": 2 }), Value::is_object).expect("readable");
        assert_eq!(read_secure_json(&plain).expect("readable"), Some(json!({ "n": 2 })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // SPEC §core.sealed: a store reads a payload of another shape as empty, the same trap as a torn
    // file, so the one edit that follows would replace it.
    #[test]
    fn a_payload_of_another_shape_is_never_written_over() {
        let dir = scratch();
        let path = dir.join("other.json");
        write_secure_json(&path, &json!(["kept"])).expect("seal");

        let err = replace_secure_json(&path, &json!({ "fresh": true }), Value::is_object).unwrap_err();
        assert!(err.contains("not the shape") && err.contains("stays as it is"), "{err}");
        assert!(replace_all_secure_json(&[(path.clone(), json!({ "fresh": true }))]).is_err());
        assert_eq!(read_secure_json(&path).expect("readable"), Some(json!(["kept"])));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // SPEC §core.sealed: one bad file in a set stops the set before the first write.
    #[test]
    fn a_set_with_an_unreadable_file_changes_nothing() {
        let dir = scratch();
        let good = dir.join("good.json");
        let torn = dir.join("torn.json");
        write_secure_json(&good, &json!({ "n": "old" })).expect("seal");
        std::fs::write(&torn, "{ not json").expect("plant a torn file");

        let files = [
            (good.clone(), json!({ "n": "new" })),
            (torn.clone(), json!({ "n": "new" })),
        ];
        assert!(replace_all_secure_json(&files).is_err());
        assert_eq!(read_secure_json(&good).expect("readable"), Some(json!({ "n": "old" })));
        assert_eq!(std::fs::read_to_string(&torn).expect("still there"), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // SPEC §core.sealed: a write that fails part way leaves the set as it found it.
    #[test]
    fn a_failed_write_puts_back_the_files_before_it() {
        let dir = scratch();
        let existing = dir.join("existing.json");
        let created = dir.join("created.json");
        let refused = dir.join("refused.json");
        write_secure_json(&existing, &json!({ "n": "old" })).expect("seal");
        let files = [
            (existing.clone(), json!({ "n": "new" })),
            (created.clone(), json!({ "n": "new" })),
            (refused.clone(), json!({ "n": "new" })),
        ];

        let err = replace_all_with(&files, |path, text| {
            if path == refused {
                return Err("disk full".to_string());
            }
            write_text_atomic(path, text)
        })
        .unwrap_err();

        assert!(err.contains("disk full") && err.contains("back as it was"), "{err}");
        assert_eq!(read_secure_json(&existing).expect("readable"), Some(json!({ "n": "old" })));
        assert!(!created.exists(), "a file the set created goes again");
        assert!(!refused.exists());

        replace_all_secure_json(&files).expect("the whole set lands when nothing refuses");
        for (path, _) in &files {
            assert_eq!(read_secure_json(path).expect("readable"), Some(json!({ "n": "new" })));
        }
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(err.contains(MASTER_KEY_ENV_SWISS), "{err}");
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
