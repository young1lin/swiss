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
    let _last = String::new();
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
         Run 'lmg export' on the machine that sealed it and 'lmg import' here, or set {}.",
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
