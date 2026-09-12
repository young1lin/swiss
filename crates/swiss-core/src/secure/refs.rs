//! The one resolver every credential string passes on its way to a live use (docs/19 D1/D4).
//!
//! Two reference families, one scanner, one pass:
//! - `${UPPER_SNAKE}` — env refs, resolved against envstore's overlay plus the process env.
//!   Missing stays the historical lenient contract: expand to an empty string. Ambient machine
//!   state may legitimately be absent.
//! - `secret://kebab-name` — vault refs, resolved against the secret vault. Missing is a HARD
//!   failure naming the reference: the operator declared a secret this machine does not hold,
//!   and silently sending an empty credential would turn a configuration error into a
//!   mysterious 401 on someone else's server.
//!
//! A replaced value is never re-scanned: a secret whose value itself contains `${X}` is data,
//! not a second-order reference. The scan advances one CHARACTER past anything it does not
//! consume, so multi-byte text survives untouched (same care as config.rs's scanner).

use serde_json::{Map, Value};

use super::envstore::env_lookup;
use super::secretstore::vault_lookup;

/// The scheme a vault reference starts with.
const SECRET_SCHEME: &str = "secret://";

/// Resolve every reference in ONE string (docs/19 D4). Err carries the sentence an operator
/// reads — it names the reference, never a value.
pub fn resolve(input: &str) -> Result<String, String> {
    resolve_collect(input).map(|(out, _)| out)
}

/// Resolve and also collect what each secret ref resolved to. Callers that capture output
/// (the actions' run paths) mask exactly these values, the same discipline env refs already
/// have there: what a ref produced is a credential; what the user typed as a literal is config.
pub fn resolve_collect(input: &str) -> Result<(String, Vec<String>), String> {
    let mut out = String::with_capacity(input.len());
    let mut resolved: Vec<String> = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // --- vault refs: secret://name -----------------------------------------------
        if input[byte_of(&chars, i)..].starts_with(SECRET_SCHEME) {
            let mut j = i + SECRET_SCHEME.len();
            while j < chars.len()
                && (chars[j].is_ascii_lowercase() || chars[j].is_ascii_digit() || chars[j] == '-')
            {
                j += 1;
            }
            let name: String = chars[i + SECRET_SCHEME.len()..j].iter().collect();
            let first = chars.get(i + SECRET_SCHEME.len()).copied();
            match first {
                // A name-shaped run: a real reference, present or missing.
                Some(c) if c.is_ascii_lowercase() => {
                    let Some(value) = vault_lookup(&name) else {
                        return Err(format!(
                            "references secret://{name} which is not in the vault"
                        ));
                    };
                    resolved.push(value.clone());
                    out.push_str(&value);
                    i = j;
                    continue;
                }
                // Letters that cannot start a kebab name: almost certainly a typo of a
                // reference. Refuse it — a literal passthrough would ship a credential-shaped
                // string to a third party.
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    let mut shown = String::new();
                    for ch in &chars[i + SECRET_SCHEME.len()..] {
                        if ch.is_whitespace() || shown.chars().count() >= 24 {
                            break;
                        }
                        shown.push(*ch);
                    }
                    return Err(format!(
                        "invalid reference 'secret://{shown}' — secret names are lowercase kebab ([a-z][a-z0-9-]{{0,63}})"
                    ));
                }
                // No name claimed (punctuation, digits, end): plain text that happens to
                // contain the scheme — pass it through.
                _ => {}
            }
        }
        // --- env refs: ${UPPER_SNAKE} ------------------------------------------------
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            let mut j = i + 2;
            while j < chars.len()
                && (chars[j].is_ascii_uppercase() || chars[j].is_ascii_digit() || chars[j] == '_')
            {
                j += 1;
            }
            if j > i + 2 && chars.get(j) == Some(&'}') {
                let name: String = chars[i + 2..j].iter().collect();
                // Lenient by contract (docs/19 D4): ambient state may be absent.
                out.push_str(&env_lookup(&name).unwrap_or_default());
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok((out, resolved))
}

/// The char index as a byte index into `input` (chars are indexed uniformly; the byte offset
/// is what starts_with needs).
fn byte_of(chars: &[char], i: usize) -> usize {
    chars[..i].iter().map(|c| c.len_utf8()).sum()
}

/// Walk a serde_json Value and resolve every string in it. Errors carry the JSON path, so the
/// operator sees WHERE the reference lives: `headers.Authorization references secret://stripe-key
/// which is not in the vault`. Object keys are never touched — a key is a field name, not a
/// credential.
pub fn resolve_value(v: &Value) -> Result<Value, String> {
    resolve_at(v, "")
}

fn resolve_at(v: &Value, path: &str) -> Result<Value, String> {
    match v {
        Value::String(s) => resolve(s)
            .map(Value::String)
            .map_err(|e| if path.is_empty() { e } else { format!("{path} {e}") }),
        Value::Array(a) => Ok(Value::Array(
            a.iter()
                .enumerate()
                .map(|(i, item)| {
                    let p = if path.is_empty() {
                        format!("[{i}]")
                    } else {
                        format!("{path}[{i}]")
                    };
                    resolve_at(item, &p)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Value::Object(o) => Ok(Value::Object(
            o.iter()
                .map(|(k, val)| {
                    let p = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    resolve_at(val, &p).map(|v| (k.clone(), v))
                })
                .collect::<Result<Map<_, _>, _>>()?,
        )),
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;
    use serde_json::json;

    /// Plant a vault value under a name unique to this test (one scratch home serves the whole
    /// binary — unique names keep parallel tests from reading each other's secrets).
    fn plant(name: &str, value: &str) {
        let _guard = paths::DATA_DIR_LOCK.blocking_lock();
        let path = paths::test_home().join("secrets.json");
        super::super::secretstore::inject_vault(&path);
        let rev = super::super::secretstore::vault_rev();
        super::super::secretstore::put_secret(&path, name, value, rev).expect("plant a secret");
    }

    #[test]
    fn env_refs_stay_lenient() {
        // An env ref resolves from the process env and keeps the empty-when-missing contract.
        unsafe { std::env::set_var("SWISS_REFS_TEST_A", "alpha") };
        assert_eq!(resolve("x=${SWISS_REFS_TEST_A}").unwrap(), "x=alpha");
        assert_eq!(resolve("${SWISS_REFS_TEST_MISSING}").unwrap(), "");
        // Non-conforming shapes stay literal, as before.
        assert_eq!(resolve("${lowercase}").unwrap(), "${lowercase}");
        assert_eq!(resolve("$NOPE {a}").unwrap(), "$NOPE {a}");
    }

    #[test]
    fn a_present_vault_ref_substitutes() {
        plant("refs-present", "sk_live_xyz");
        assert_eq!(
            resolve("Bearer secret://refs-present").unwrap(),
            "Bearer sk_live_xyz"
        );
    }

    #[test]
    fn a_missing_vault_ref_fails_naming_the_reference() {
        let err = resolve("Bearer secret://refs-absent-tail").unwrap_err();
        assert!(
            err.contains("secret://refs-absent-tail") && err.contains("not in the vault"),
            "names the reference: {err}"
        );
        assert!(!err.contains("sk_live"), "never carries a value");
    }

    #[test]
    fn an_invalid_secret_name_is_refused() {
        let err = resolve("secret://BadName rest").unwrap_err();
        assert!(err.contains("lowercase kebab"), "explains the grammar: {err}");
    }

    #[test]
    fn bare_scheme_without_a_name_is_plain_text() {
        // "secret://" followed by punctuation or end claims no name — pass through.
        assert_eq!(resolve("see secret:// docs").unwrap(), "see secret:// docs");
        assert_eq!(resolve("secret://").unwrap(), "secret://");
    }

    #[test]
    fn both_families_mix_in_one_string() {
        unsafe { std::env::set_var("SWISS_REFS_TEST_B", "beta") };
        plant("refs-mixed", "gamma");
        assert_eq!(
            resolve("${SWISS_REFS_TEST_B}/secret://refs-mixed/tail").unwrap(),
            "beta/gamma/tail"
        );
    }

    #[test]
    fn replaced_values_are_not_rescanned() {
        // The planted value itself contains an env ref — data, not a second-order reference.
        plant("refs-indirect", "x=${SWISS_REFS_TEST_NEVER_SET}");
        assert_eq!(
            resolve("secret://refs-indirect").unwrap(),
            "x=${SWISS_REFS_TEST_NEVER_SET}"
        );
    }

    #[test]
    fn collect_returns_what_secrets_resolved_to() {
        plant("refs-collect", "sk_mask_me");
        let (out, resolved) = resolve_collect("a secret://refs-collect b").unwrap();
        assert_eq!(out, "a sk_mask_me b");
        assert_eq!(resolved, vec!["sk_mask_me".to_string()]);
    }

    #[test]
    fn value_walk_reports_the_json_path() {
        plant("refs-walk", "v");
        let def = json!({ "headers": { "Authorization": "secret://refs-walk" } });
        let resolved = resolve_value(&def).unwrap();
        assert_eq!(resolved["headers"]["Authorization"], "v");

        let bad = json!({ "headers": { "Authorization": "secret://refs-walk-missing" } });
        let err = resolve_value(&bad).unwrap_err();
        assert!(
            err.starts_with("headers.Authorization "),
            "carries the path: {err}"
        );

        let arr = json!({ "args": ["--key", "secret://refs-walk-missing"] });
        let err = resolve_value(&arr).unwrap_err();
        assert!(err.starts_with("args[1] "), "arrays carry an index: {err}");
    }
}
