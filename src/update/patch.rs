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

//! Decode a zstd full archive or a `zstd --patch-from` patch. The window is capped at
//! 2^28 before any byte is decoded (ADR-041). The decoded exe streams to disk with a running
//! SHA-256 and is never held in memory; a patch holds only the old exe, its reference.

use std::io::{BufRead, Read, Write};
use std::path::Path;

pub const WINDOW_LOG_MAX: u32 = 28;
const DECODED_MAX: u64 = 256 * 1024 * 1024;

/// What the packed asset is decoded against.
pub enum Packed<'a> {
    Full,
    Patch { old: &'a [u8], window_log: u32 },
}

/// Decode `packed` into `path`, then keep the file only when its SHA-256 is `expected`.
/// A decode error, an output over 256 MiB or a wrong hash removes it.
pub fn decode_to_file(packed: &Path, how: Packed<'_>, path: &Path, expected: &str) -> Result<(), String> {
    let input = std::fs::File::open(packed).map_err(|err| format!("cannot read download: {err}"))?;
    decode_reader(std::io::BufReader::new(input), how, path, expected)
}

/// [`decode_to_file`] over any reader, so a test can decode bytes it holds.
pub fn decode_reader(input: impl BufRead, how: Packed<'_>, path: &Path, expected: &str) -> Result<(), String> {
    let mut decoder = match how {
        Packed::Full => zstd::stream::read::Decoder::with_buffer(input).map_err(|err| format!("full decode: {err}"))?,
        Packed::Patch { old, window_log } => {
            if window_log > WINDOW_LOG_MAX {
                return Err(format!("patch windowLog {window_log} is above {WINDOW_LOG_MAX}"));
            }
            zstd::stream::read::Decoder::with_ref_prefix(input, old).map_err(|err| format!("patch decode: {err}"))?
        }
    };
    decoder.window_log_max(WINDOW_LOG_MAX).map_err(|err| format!("decode window: {err}"))?;
    let result = write_hashed(&mut decoder, path, expected);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn write_hashed(reader: &mut impl Read, path: &Path, expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let mut file = std::fs::File::create(path).map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|err| format!("decode: {err}"))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > DECODED_MAX {
            return Err("decoded exe is over 256 MiB".to_string());
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    }
    file.sync_all().map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    drop(file);
    let got: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    if !got.eq_ignore_ascii_case(expected) {
        return Err(format!("sha256 mismatch for {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
pub fn encode_patch(old: &[u8], new: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), 3, old)
        .map_err(|err| format!("patch encode: {err}"))?;
    encoder.write_all(new).map_err(|err| format!("patch encode: {err}"))?;
    encoder.finish().map_err(|err| format!("patch encode: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("swiss-patch-{}", swiss_core::util::random_hex(6)));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_patch_round_trips_and_a_corrupt_one_is_an_error() {
        let dir = Scratch::new();
        let out = dir.0.join("swiss.exe.new");
        let old = vec![7u8; 2 * 1024 * 1024];
        let mut new = old.clone();
        new.extend_from_slice(b"tail-differs");
        let patch = encode_patch(&old, &new).unwrap();
        let how = Packed::Patch { old: &old, window_log: 27 };
        decode_reader(&patch[..], how, &out, &sha256_bytes(&new)).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), new);

        let how = Packed::Patch { old: &old, window_log: 27 };
        let err = decode_reader(&b"not-a-patch"[..], how, &out, &sha256_bytes(&new)).unwrap_err();
        assert!(err.contains("frame") || err.contains("decode"), "{err}");
        assert!(!out.exists(), "a failed decode leaves no .new");
    }

    #[test]
    fn a_full_archive_streams_to_the_file() {
        let dir = Scratch::new();
        let out = dir.0.join("swiss.exe.new");
        let new = b"a whole new exe".repeat(1000);
        let packed = zstd::stream::encode_all(&new[..], 3).unwrap();
        let file = dir.0.join("full.zst");
        std::fs::write(&file, &packed).unwrap();
        decode_to_file(&file, Packed::Full, &out, &sha256_bytes(&new)).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), new);
    }

    #[test]
    fn a_window_over_28_is_refused_before_decode() {
        let dir = Scratch::new();
        let out = dir.0.join("swiss.exe.new");
        let err = decode_reader(&b"patch"[..], Packed::Patch { old: b"old", window_log: 29 }, &out, "00").unwrap_err();
        assert!(err.contains("windowLog 29"), "{err}");
        assert!(!out.exists());
    }

    #[test]
    fn a_sha_mismatch_removes_the_new_file() {
        let dir = Scratch::new();
        let out = dir.0.join("swiss.exe.new");
        let packed = zstd::stream::encode_all(&b"bytes"[..], 3).unwrap();
        let err = decode_reader(&packed[..], Packed::Full, &out, "0000").unwrap_err();
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert!(!out.exists());
    }
}
