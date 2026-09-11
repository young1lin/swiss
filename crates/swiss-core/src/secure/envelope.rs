//! The sealed-envelope format every gateway state file is written in — port of
//! `secure/envelope.ts`, whose format is FROZEN (docs/05 §1): the `"lmg"` marker key and the
//! `lmg-state-v1` HKDF info are the format's historical wire literals, not the product name —
//! a rename of the product must never touch them.
//!
//! `{ "lmg": 1, alg, keySource, salt, iv, tag, ct }` — AES-256-GCM over the UTF-8 JSON payload,
//! with a per-file random salt so the master key is stretched through HKDF per file (key reuse
//! across files is fine; two identical payloads still seal to different ciphertext). The GCM tag
//! is the authenticity check: a wrong key, a tampered byte, a truncated file — all fail the tag,
//! which is how "this file was not sealed on this machine" is detected.
//!
//! `keySource` names the backend that sealed the file (dpapi / keychain / secrettool / machineid
//! / env) — a diagnostic, never trusted: opening always tries every available candidate and lets
//! the tag decide.

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hkdf::Hkdf;
// rand 0.9: the OS RNG speaks the fallible TryRngCore (rand_core 0.9 split the trait). An OS
// CSPRNG that answers Err is a broken machine, not a case to handle - expect it away.
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const FORMAT_VERSION: u8 = 1;
// FROZEN wire literal (docs/05 §1): every sealed file on disk was derived with this info
// string. It is the format's historical name, not the product name — never rename it.
const HKDF_INFO: &[u8] = b"lmg-state-v1";

/// The envelope as it appears on disk. Field names are part of the frozen format — `camelCase`
/// on the wire (`keySource`), snake_case in Rust.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sealed {
    /// Envelope marker, serialized as the JSON key `"lmg"` — a FROZEN wire literal (docs/05
    /// §1), the format's historical name rather than the product name. Do not rename.
    pub lmg: u8,
    pub alg: String,
    pub key_source: String,
    pub salt: String,
    pub iv: String,
    pub tag: String,
    pub ct: String,
}

/// True when the parsed JSON is one of OUR envelopes (and therefore not user-authored config).
/// The same three-field shape test as the Node build's `isSealed`.
pub fn is_sealed(value: &serde_json::Value) -> bool {
    value.is_object()
        && value.get("lmg") == Some(&serde_json::Value::from(FORMAT_VERSION))
        && value.get("alg").is_some_and(|v| v.is_string())
        && value.get("ct").is_some_and(|v| v.is_string())
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("sealed with format v{0}; this build understands v{FORMAT_VERSION}")]
    UnsupportedVersion(u8),
    #[error("unknown cipher: {0}")]
    UnknownCipher(String),
    #[error("base64 field {field} is malformed: {source}")]
    Base64 {
        field: &'static str,
        source: base64::DecodeError,
    },
    /// The GCM tag rejected the key or the ciphertext. Every "not openable here" case — wrong
    /// machine, wrong key, tampering, truncation — surfaces here, matching the Node build where
    /// callers treat any failure as that.
    #[error("authentication failed (wrong key, other machine, or tampered file)")]
    AuthFailed,
}

impl From<aes_gcm::Error> for EnvelopeError {
    fn from(_: aes_gcm::Error) -> Self {
        EnvelopeError::AuthFailed
    }
}

fn decode(field: &'static str, text: &str) -> Result<Vec<u8>, EnvelopeError> {
    BASE64
        .decode(text)
        .map_err(|source| EnvelopeError::Base64 { field, source })
}

/// HKDF-SHA256(ikm = master, salt = salt, info = "lmg-state-v1", len = 32).
///
/// Node's `hkdfSync` argument order is `(digest, ikm, salt, info, len)` — the salt/ikm swap is
/// the classic way to get this silently wrong, and "silently wrong" means every existing file
/// fails to open. The fixture test below exists to catch exactly that at the moment it happens.
fn file_key(master: &[u8], salt: &[u8]) -> Result<[u8; 32], EnvelopeError> {
    let hk = Hkdf::<Sha256>::new(Some(salt), master);
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    Ok(okm)
}

/// Seal one UTF-8 payload under a master key. Random salt and IV per call, so the same plaintext
/// never seals to the same bytes twice.
pub fn seal(master: &[u8], key_source: &str, plaintext: &str) -> Sealed {
    let mut salt = [0u8; 16];
    let mut iv = [0u8; 12];
    rand::rngs::OsRng
        .try_fill_bytes(&mut salt)
        .expect("the OS CSPRNG answered an error");
    rand::rngs::OsRng
        .try_fill_bytes(&mut iv)
        .expect("the OS CSPRNG answered an error");

    let key = file_key(master, &salt).expect("HKDF with fixed lengths cannot fail");
    // hybrid-array (the aead 0.6 generation) deprecates from_slice: fixed-size values convert
    // with From, and only file-sourced slices deserve a fallible TryFrom. Same bytes, same
    // format — the fixture test pins that.
    let key_bytes: Key<Aes256Gcm> = key.into();
    let cipher = Aes256Gcm::new(&key_bytes);
    // No AAD — the Node build passes none, so the format does not either. The crate appends the
    // 16-byte tag to the ciphertext, which is how the split below recovers it.
    let out = cipher
        .encrypt(
            &Nonce::<U12>::from(iv),
            Payload {
                msg: plaintext.as_bytes(),
                aad: &[],
            },
        )
        .expect("AES-256-GCM encryption of in-memory data cannot fail");
    let (ct, tag) = out.split_at(out.len() - 16);

    Sealed {
        lmg: FORMAT_VERSION,
        alg: "aes-256-gcm".into(),
        key_source: key_source.into(),
        salt: BASE64.encode(salt),
        iv: BASE64.encode(iv),
        tag: BASE64.encode(tag),
        ct: BASE64.encode(ct),
    }
}

/// Open an envelope. Errors on ANY mismatch — wrong key, other machine, tampering, or a format
/// newer than this build — so callers treat every failure as "not openable here".
pub fn unseal(master: &[u8], sealed: &Sealed) -> Result<String, EnvelopeError> {
    if sealed.lmg != FORMAT_VERSION {
        return Err(EnvelopeError::UnsupportedVersion(sealed.lmg));
    }
    if sealed.alg != "aes-256-gcm" {
        return Err(EnvelopeError::UnknownCipher(sealed.alg.clone()));
    }
    let salt = decode("salt", &sealed.salt)?;
    let iv = decode("iv", &sealed.iv)?;
    let tag = decode("tag", &sealed.tag)?;
    let ct = decode("ct", &sealed.ct)?;

    let key = file_key(master, &salt)?;
    let key_bytes: Key<Aes256Gcm> = key.into();
    let cipher = Aes256Gcm::new(&key_bytes);
    // An iv that is not 12 bytes is a malformed or tampered envelope — the same "not openable
    // here" answer a bad tag gets, never a panic on file-sourced data.
    let nonce = Nonce::<U12>::try_from(iv.as_slice()).map_err(|_| EnvelopeError::AuthFailed)?;
    let mut ciphertext = ct;
    ciphertext.extend_from_slice(&tag);
    let plaintext = cipher.decrypt(
        &nonce,
        Payload {
            msg: &ciphertext,
            aad: &[],
        },
    )?;
    String::from_utf8(plaintext).map_err(|_| EnvelopeError::AuthFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The same key the Node fixture generator seals with (scripts/seal-fixture.mts). A fixed
    /// test key, never a real one — it is checked into the repository on purpose.
    const FIXTURE_KEY: [u8; 32] = [
        0x3f, 0x7a, 0x9c, 0x2e, 0x58, 0xb1, 0xd4, 0x06, 0xaf, 0x83, 0xc2, 0xe9, 0x7b, 0x1f, 0x6d,
        0x0a, 0x4c, 0x8e, 0x2f, 0x5b, 0x7a, 0x9d, 0x1c, 0x06, 0xe3, 0xb8, 0xf4, 0xa2, 0xd5, 0xc7,
        0xe9, 0x1f,
    ];

    /// The blocking Phase 0 test (docs/05 §1): a fixture sealed by the NODE build must open here,
    /// byte for byte. This is the test that catches an HKDF argument-order or base64 mistake at
    /// the moment it is introduced rather than on a user's machine.
    #[test]
    fn opens_a_node_sealed_fixture() {
        let envelope_text = include_str!("../../../../tests/fixtures/sealed-node.json");
        let payload = include_str!("../../../../tests/fixtures/sealed-node.payload.txt");
        let sealed: Sealed =
            serde_json::from_str(envelope_text).expect("fixture parses as an envelope");
        assert_eq!(sealed.lmg, 1);
        assert_eq!(sealed.alg, "aes-256-gcm");
        assert_eq!(sealed.key_source, "env");
        assert_eq!(
            unseal(&FIXTURE_KEY, &sealed).expect("fixture opens"),
            payload
        );
    }

    #[test]
    fn a_wrong_key_fails_the_tag_not_the_payload() {
        let envelope_text = include_str!("../../../../tests/fixtures/sealed-node.json");
        let sealed: Sealed = serde_json::from_str(envelope_text).expect("fixture parses");
        let mut wrong = FIXTURE_KEY;
        wrong[0] ^= 0xff;
        assert!(matches!(
            unseal(&wrong, &sealed),
            Err(EnvelopeError::AuthFailed)
        ));
    }

    #[test]
    fn rust_seal_unseal_round_trips() {
        let payload = "{\"port\":19999,\"host\":\"127.0.0.1\"}";
        let sealed = seal(&FIXTURE_KEY, "env", payload);
        assert_eq!(
            unseal(&FIXTURE_KEY, &sealed).expect("round trip opens"),
            payload
        );
    }

    #[test]
    fn sealing_twice_produces_different_ciphertext() {
        // Per-file random salt + IV: identical payloads seal to different bytes.
        let a = seal(&FIXTURE_KEY, "env", "same");
        let b = seal(&FIXTURE_KEY, "env", "same");
        assert_ne!(a.ct, b.ct);
        assert_ne!(a.salt, b.salt);
    }

    /// docs/05 §1: the marker key and the HKDF info are FROZEN wire literals keyed to the
    /// format's historical name. This fails the moment someone "modernizes" them — which is
    /// exactly the moment every sealed file on every machine stops opening.
    #[test]
    fn the_wire_literals_stay_frozen() {
        assert_eq!(HKDF_INFO, b"lmg-state-v1");
        let sealed = serde_json::to_value(seal(&FIXTURE_KEY, "env", "x")).expect("serializes");
        assert!(sealed.get("lmg").is_some(), "the marker must serialize as \"lmg\"");
        assert!(sealed.get("swiss").is_none());
    }

    #[test]
    fn is_sealed_recognizes_envelopes_only() {
        let sealed = serde_json::to_value(seal(&FIXTURE_KEY, "env", "x")).expect("serializes");
        assert!(is_sealed(&sealed));
        assert!(!is_sealed(&json!({ "port": 19999 })));
        assert!(!is_sealed(&json!({ "lmg": 2, "alg": "x", "ct": "y" })));
    }
}
