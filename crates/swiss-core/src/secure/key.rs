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

//! The machine-bound master key — port of `secure/key.ts`.
//!
//! Where the key lives, per platform — as built (the implemented list is `sources_for`
//! below; anything not there is future work, however the Node build did it):
//!
//!   Windows  DPAPI (CurrentUser). A random key is generated once, protected by DPAPI, and the
//!            protected blob sits in the data dir as 'master.key'. On any other machine or under
//!            any other user the blob is opaque — which is exactly the anti-copy property.
//!   Linux    /etc/machine-id (or /var/lib/dbus/machine-id), hashed. World-readable, so it
//!            binds to the MACHINE, not the user; treat shared hosts accordingly. The Node
//!            build's Secret Service route is future work.
//!   macOS    A random key in the user's login Keychain. Concurrent first starters use a
//!            create-only operation and read back the winner rather than replace its key.
//!
//! An explicit SWISS_MASTER_KEY (64 hex chars) overrides every source — for CI, containers and
//! recovery, and what the test suite uses so it never spawns a single OS helper.
//!
//! The key is resolved at most once per process and cached. On Windows the DPAPI calls are direct
//! Win32 (no PowerShell spawn — see platform/windows.rs).

use std::sync::OnceLock;

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::paths::data_path;
use crate::platform;

/// Domain separation: the app string is also the DPAPI entropy, so a blob is ours or nobody's.
/// Part of the on-disk format — a different string here and `master.key` does not open.
pub const APP: &str = "local-mcp-gateway/v1";

/// The env var that overrides every key source. What CI and the test suite use.
pub const MASTER_KEY_ENV_SWISS: &str = "SWISS_MASTER_KEY";

const KEY_LEN: usize = 32;

#[derive(Clone)]
pub struct KeyMaterial {
    pub id: &'static str,
    pub key: [u8; 32],
}

/// Accept only a full 64-hex-char key — anything else is a misconfiguration, not a key.
pub fn parse_key_hex(raw: &str, what: &str) -> Result<[u8; 32], String> {
    let hex = raw.trim();
    let valid = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid {
        return Err(format!(
            "{what} must be 64 hex chars (32 bytes), got {} chars",
            hex.len()
        ));
    }
    let mut key = [0u8; 32];
    for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16).unwrap_or(0);
        let lo = (pair[1] as char).to_digit(16).unwrap_or(0);
        key[i] = ((hi << 4) | lo) as u8;
    }
    Ok(key)
}

/// sha256(APP + NUL + machineId) — stable per machine, useless off it.
pub fn derive_machine_key(machine_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(APP.as_bytes());
    hasher.update([0u8]);
    hasher.update(machine_id.as_bytes());
    hasher.finalize().into()
}

/// The Windows DPAPI source: unprotect master.key when it exists, else generate a random key and
/// store it protected. Fails (dropped from the candidate list) when this user on this machine is
/// not the one who protected the existing blob.
fn dpapi_source() -> Result<KeyMaterial, String> {
    let path = data_path(&["master.key"]);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let blob = base64::engine::general_purpose::STANDARD
                .decode(text.trim().as_bytes())
                .map_err(|e| format!("master.key is not valid base64: {e}"))?;
            let key = platform::dpapi_unprotect(&blob)
                .map_err(|e| format!("master.key does not open under this user: {e}"))?;
            if key.len() != KEY_LEN {
                return Err("master.key holds a malformed key".into());
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&key);
            Ok(KeyMaterial {
                id: "dpapi",
                key: arr,
            })
        }
        Err(_) => {
            // First run on this machine: generate, protect, persist (0600, atomically enough for
            // a first-write; a concurrent double-generate just seals under whichever wins later).
            let mut key = [0u8; KEY_LEN];
            // TryRng per rand 0.10 (see envelope.rs); an Err here is a broken machine.
            rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut key)
                .expect("the OS CSPRNG answered an error");
            let protected =
                platform::dpapi_protect(&key).map_err(|e| format!("DPAPI protect failed: {e}"))?;
            let text = base64::engine::general_purpose::STANDARD.encode(&protected);
            crate::platform::write_file_private(&path, &text)
                .map_err(|e| format!("could not write master.key: {e}"))?;
            Ok(KeyMaterial { id: "dpapi", key })
        }
    }
}

fn machine_id_source() -> Result<KeyMaterial, String> {
    let id = platform::machine_id().ok_or("no machine id on this system")?;
    Ok(KeyMaterial {
        id: "machineid",
        key: derive_machine_key(&id),
    })
}

/// SPEC §core.masterkey: an unavailable or malformed store is never a reason to replace a key.
#[cfg(any(target_os = "macos", test))]
fn stored_key(
    read: impl Fn() -> Result<Option<Vec<u8>>, String>,
    create: impl Fn(&[u8]) -> Result<bool, String>,
    generate: impl FnOnce() -> Result<[u8; KEY_LEN], String>,
) -> Result<[u8; KEY_LEN], String> {
    let decode = |bytes: Vec<u8>| bytes.try_into().map_err(|_| "the Keychain holds a malformed master key".to_string());
    if let Some(bytes) = read()? { return decode(bytes); }
    let key = generate()?;
    if create(&key)? { return Ok(key); }
    decode(read()?.ok_or("the master key disappeared after a concurrent creation")?)
}

#[cfg(target_os = "macos")]
fn keychain_source() -> Result<KeyMaterial, String> {
    const ACCOUNT: &str = "master-key";
    let key = stored_key(
        || platform::keychain_read(APP, ACCOUNT),
        |key| platform::keychain_create(APP, ACCOUNT, key),
        || {
            let mut key = [0u8; KEY_LEN];
            rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut key)
                .map_err(|error| format!("cannot generate a master key: {error}"))?;
            Ok(key)
        },
    )?;
    Ok(KeyMaterial { id: "keychain", key })
}

fn cached() -> &'static Vec<KeyMaterial> {
    static CACHED: OnceLock<Vec<KeyMaterial>> = OnceLock::new();
    CACHED.get_or_init(|| {
        // The strongest source first; the machine-id fallback is always last so a sealed file
        // from a machine whose credential-store entry vanished still opens.
        let sources: Vec<fn() -> Result<KeyMaterial, String>> = if cfg!(windows) {
            vec![dpapi_source, machine_id_source]
        } else if cfg!(target_os = "macos") {
            #[cfg(target_os = "macos")]
            { vec![keychain_source] }
            #[cfg(not(target_os = "macos"))]
            { Vec::new() }
        } else {
            vec![machine_id_source]
        };
        let mut out = Vec::new();
        let mut failures = Vec::new();
        for src in sources {
            match src() {
                Ok(km) => out.push(km),
                Err(e) => failures.push(e),
            }
        }
        if out.is_empty() {
            // Nothing can produce a key at all — surface every reason once. Callers serve
            // plaintext read-only rather than failing the boot on a file we cannot protect.
            crate::log::error(
                "no master key source worked",
                Some(serde_json::json!({ "failures": failures.join("; ") })),
            );
        }
        out
    })
}

/// The `SWISS_MASTER_KEY` variable when set, as (name, raw value) — the name comes back so a
/// malformed value is reported against the variable the user actually set.
fn master_key_env_raw() -> Option<(&'static str, String)> {
    [MASTER_KEY_ENV_SWISS]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().map(|raw| (name, raw)))
}

/// Every key a sealed file might open with, strongest first. An explicit SWISS_MASTER_KEY never
/// mixes with OS sources; a malformed one is a configuration error and surfaces as such,
/// naming the variable it came from.
pub fn master_key_candidates() -> Result<Vec<KeyMaterial>, String> {
    if let Some((name, raw)) = master_key_env_raw() {
        return Ok(vec![KeyMaterial {
            id: "env",
            key: parse_key_hex(&raw, name)?,
        }]);
    }
    Ok(cached().clone())
}

/// Pin the whole test binary to one deterministic master key. `master_key_candidates` reads this
/// variable fresh on every call, so installing it once here makes every later seal and unseal in
/// the process use it — no DPAPI, no Keychain, no machine id, and nothing cached to invalidate.
///
/// `set_var` is unsafe in edition 2024 because it races other threads; this one runs once under a
/// `OnceLock`, and the variable it sets is read by nothing but the key source above.
///
/// Also built under the `test-utils` feature so OTHER crates' test binaries can pin the same
/// deterministic key (dev-dependencies only; a normal build never sees it).
#[cfg(any(test, feature = "test-utils"))]
pub fn use_test_master_key() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| unsafe { std::env::set_var(MASTER_KEY_ENV_SWISS, "ab".repeat(32)) });
}

/// True when no key source worked at all (the plaintext read-only fallback engages) — the
/// master-key variable unset and nothing from the OS.
pub fn no_key_available() -> bool {
    master_key_env_raw().is_none() && cached().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_lifecycle_retains_existing_keys_and_the_concurrent_winner() {
        use std::cell::Cell;
        let old = [7u8; KEY_LEN];
        assert_eq!(stored_key(|| Ok(Some(old.to_vec())), |_| panic!("must not overwrite"), || panic!("must not regenerate")), Ok(old));
        let reads = Cell::new(0);
        let result = stored_key(|| {
            let count = reads.get(); reads.set(count + 1);
            Ok(if count == 0 { None } else { Some(old.to_vec()) })
        }, |_| Ok(false), || Ok([8u8; KEY_LEN]));
        assert_eq!(result, Ok(old));
        assert_eq!(reads.get(), 2);
        assert_eq!(stored_key(|| Ok(None), |key| { assert_eq!(key, &[8u8; KEY_LEN]); Ok(true) }, || Ok([8u8; KEY_LEN])), Ok([8u8; KEY_LEN]));
    }

    #[test]
    fn keychain_errors_and_malformed_keys_never_create_replacements() {
        for read in [Err("locked".to_string()), Ok(Some(vec![1; KEY_LEN - 1]))] {
            assert!(stored_key(|| read.clone(), |_| panic!("must not overwrite"), || panic!("must not regenerate")).is_err());
        }
        assert!(stored_key(|| Ok(None), |_| Err("denied".into()), || Ok([8u8; KEY_LEN])).is_err());
    }

    #[test]
    fn parse_key_hex_accepts_64_hex_chars() {
        assert_eq!(parse_key_hex(&"00".repeat(32), "test"), Ok([0u8; 32]));
        assert!(parse_key_hex(
            "3f7a9c2e58b1d406af83c2e97b1f6d0a4c8e2f5b7a9d1c06e3b8f4a2d5c7e91f",
            "test"
        )
        .is_ok());
    }

    #[test]
    fn parse_key_hex_rejects_anything_else() {
        // Ported from key.ts: wrong length, non-hex characters, and surrounding whitespace is
        // trimmed before the check (so a padded valid key still parses).
        assert!(parse_key_hex("abc", "test").is_err());
        assert!(parse_key_hex(&"z".repeat(64), "test").is_err());
        assert!(parse_key_hex(&format!(" {} ", "0".repeat(64)), "test").is_ok());
    }

    #[test]
    fn machine_key_matches_the_node_derivation() {
        // sha256("local-mcp-gateway/v1" + "\0" + "test-machine") — cross-checked against the
        // Node build's deriveMachineKey. Pinning it here guards the argument order.
        let k = derive_machine_key("test-machine");
        let mut hasher = Sha256::new();
        hasher.update(b"local-mcp-gateway/v1");
        hasher.update([0u8]);
        hasher.update(b"test-machine");
        let expect: [u8; 32] = hasher.finalize().into();
        assert_eq!(k, expect);
    }
}
