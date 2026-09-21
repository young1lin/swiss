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

//! Atomic file writes — port of `atomic-json.ts`.
//!
//! A plain write truncates before it writes, so being killed mid-write — Task Manager's End Task,
//! or a force-exit landing on a save that followed a panel edit — leaves the file empty or torn.
//! A loader that answers a parse failure with "empty" then silently discards everything that was
//! in it. A temp file renamed over the target is replaced whole or not at all (rename is atomic
//! on NTFS and POSIX).
//!
//! Reporting the failure matters as much as the rename: a store that only logged let the admin
//! API report 201 Created for a definition that never reached the disk.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::log;
use crate::platform::{chmod_private, private_file_mode};

/// A temp name unique to THIS writer: two concurrent writers (two instances sharing a data dir, a
/// test racing a live gateway) used to share `<path>.tmp`, and whichever renamed first turned the
/// other's rename into an ENOENT.
fn tmp_path(path: &Path) -> std::path::PathBuf {
    let unique = format!("{}-{}", std::process::id(), crate::util::random_hex(4));
    path.with_extension(format!("{unique}.tmp"))
}

/// Rename over an existing file can fail TRANSIENTLY on Windows — the target is held for a moment
/// by antivirus, the search indexer or a backup agent. Back off and retry; a persistent failure
/// still surfaces through the error. Node slept with Atomics.wait because its helper was sync by
/// contract; here the same sync contract holds, so a blocking sleep is equally correct.
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0u32;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let transient = matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::AlreadyExists
                        | std::io::ErrorKind::ResourceBusy // EBUSY-ish, where mapped
                );
                if !transient || attempt >= 4 {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(25 * (attempt as u64 + 1)));
                attempt += 1;
            }
        }
    }
}

fn write_atomic(path: &Path, data: &str) -> Result<(), String> {
    let tmp = tmp_path(path);
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, data.as_bytes())?;
        chmod_private(&tmp, private_file_mode());
        rename_retry(&tmp, path)
    })();
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp); // best effort; nothing may have been written
            let message = err.to_string();
            log::error(
                "atomic save failed",
                Some(serde_json::json!({ "err": message, "path": path.display().to_string() })),
            );
            Err(format!("could not save {}: {}", path.display(), message))
        }
    }
}

pub fn write_json_atomic(path: &Path, data: &Value) -> Result<(), String> {
    // JSON.stringify(data, null, 2): two-space pretty print, keys in map order.
    write_atomic(
        path,
        &serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".into()),
    )
}

/// The same contract for plain text (.env): tmp + rename + the transient-rename retry. A torn
/// .env is nastier than a torn JSON file — the token line can be cut mid-value and the next boot
/// starts with a credential nothing can authenticate against.
pub fn write_text_atomic(path: &Path, text: &str) -> Result<(), String> {
    write_atomic(path, text)
}

#[cfg(test)]
mod tests {
    // Ported from test/atomic-json.test.ts in spirit: rename-retry and tmp uniqueness are
    // Windows-transient behaviours the assertions cover via round-trip and concurrent writers.
    use super::*;

    #[test]
    fn round_trips_json() {
        let dir = std::env::temp_dir().join(format!("swiss-atomic-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let data = serde_json::json!({ "a": 1, "nested": { "b": [1, 2] } });
        write_json_atomic(&path, &data).expect("writes");
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, data);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_writers_do_not_break_each_other() {
        let dir = std::env::temp_dir().join(format!("swiss-atomic-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::thread::scope(|s| {
            for i in 0..4 {
                let path = path.clone();
                s.spawn(move || {
                    for n in 0..20 {
                        write_json_atomic(&path, &serde_json::json!({ "writer": i, "n": n }))
                            .expect("every write lands or fails loudly, never torn");
                    }
                });
            }
        });
        // Whatever won, the file is valid JSON — never a half-written target.
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(back.get("writer").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
