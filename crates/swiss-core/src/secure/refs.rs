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

//! The one resolver every credential string passes on its way to a live use (docs/19 D4,
//! grammar revised by docs/25 E1).
//!
//! One envelope, two families, one pass:
//! - `${UPPER_SNAKE}` — env refs, resolved against envstore's overlay plus the process env.
//!   Missing stays the historical lenient contract: expand to an empty string. Ambient machine
//!   state may legitimately be absent.
//! - `${secret://kebab-name}` — vault refs, resolved against the secret vault. Missing is a
//!   HARD failure naming the reference: the operator declared a secret this machine does not
//!   hold, and silently sending an empty credential would turn a configuration error into a
//!   mysterious 401 on someone else's server. The scheme inside the envelope keeps the
//!   reference self-describing; the envelope gives it a boundary.
//!
//! Outside the envelope there are NO references. A bare `secret://` is literal text —
//! `https://x/secret://aaa/y` passes through byte-identical whatever the vault holds,
//! because without delimiters a scheme substring cannot be told apart from a URL path (the
//! defect docs/25 §0 records as the reason the bare scheme was retired).
//!
//! A replaced value is never re-scanned: a secret whose value itself contains `${X}` is data,
//! not a second-order reference. The scan advances one CHARACTER past anything it does not
//! consume, so multi-byte text survives untouched.

use serde_json::{Map, Value};

use super::envstore::env_lookup;
use super::secretstore::{valid_name, vault_lookup};

/// The scheme a vault reference carries inside the envelope.
const SECRET_SCHEME: &str = "secret://";

/// Resolve every reference in ONE string (docs/19 D4; grammar docs/25 E1). Err carries the
/// sentence an operator reads — it names the reference, never a value.
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
        // --- the envelope: ${...} ----------------------------------------------------
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            if let Some(close) = (i + 2..chars.len()).find(|&j| chars[j] == '}') {
                let content: String = chars[i + 2..close].iter().collect();
                // Vault refs: the scheme inside the envelope is a declared intent. A
                // well-formed name resolves or fails hard; a malformed one refuses rather
                // than shipping a credential-shaped literal to a third party.
                if let Some(name) = content.strip_prefix(SECRET_SCHEME) {
                    if valid_name(name) {
                        let Some(value) = vault_lookup(name) else {
                            return Err(format!(
                                "references secret://{name} which is not in the vault"
                            ));
                        };
                        resolved.push(value.clone());
                        out.push_str(&value);
                        i = close + 1;
                        continue;
                    }
                    let shown: String = content.chars().take(24).collect();
                    return Err(format!(
                        "invalid reference '${{{shown}}}' — secret names are lowercase kebab ([a-z][a-z0-9-]{{0,63}})"
                    ));
                }
                // Env refs: the pre-vault grammar, unchanged and lenient (docs/19 D4).
                let is_env_name = !content.is_empty()
                    && content
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                if is_env_name {
                    out.push_str(&env_lookup(&content).unwrap_or_default());
                    i = close + 1;
                    continue;
                }
                // No family claims the content — the '$' is plain text. Advance ONE
                // character so the rest rescans exactly as authored (`${lowercase}`
                // stays `${lowercase}`, the contract its test locks in).
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok((out, resolved))
}

/// The one-time shape normalization (docs/25 E2): rewrite WHOLE-VALUE bare vault refs
/// `secret://name` into the envelope form `${secret://name}`. Loaders call it on a state
/// file's tree right after reading; the disk copy catches up on the next save, so no boot
/// ever rewrites a file just to re-spell a reference. Mixed strings (`Bearer secret://x`)
/// and bare schemes inside larger text (URLs) are left byte-identical: rewriting mid-string
/// tokens would need exactly the ambiguous scan the envelope exists to replace. Idempotent;
/// returns how many strings changed, for the loader's one-line boot note.
pub fn migrate_legacy(v: &mut Value) -> usize {
    match v {
        Value::String(s) => {
            let Some(name) = s.strip_prefix(SECRET_SCHEME) else {
                return 0;
            };
            if !valid_name(name) {
                return 0;
            }
            *s = format!("${{{SECRET_SCHEME}{name}}}");
            1
        }
        Value::Array(items) => items.iter_mut().map(migrate_legacy).sum(),
        Value::Object(map) => {
            // Keys are field names, never credentials — the same stance resolve_at takes.
            map.values_mut().map(migrate_legacy).sum()
        }
        _ => 0,
    }
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
    fn a_vault_ref_substitutes_whole_and_embedded() {
        plant("refs-present", "sk_live_xyz");
        assert_eq!(resolve("${secret://refs-present}").unwrap(), "sk_live_xyz");
        // The reference is a token inside a larger string, not a whole-value convention.
        assert_eq!(
            resolve("Bearer ${secret://refs-present}").unwrap(),
            "Bearer sk_live_xyz"
        );
    }

    #[test]
    fn a_missing_vault_ref_fails_naming_the_reference() {
        let err = resolve("Bearer ${secret://refs-absent-tail}").unwrap_err();
        assert!(
            err.contains("secret://refs-absent-tail") && err.contains("not in the vault"),
            "names the reference: {err}"
        );
        assert!(!err.contains("sk_live"), "never carries a value");
    }

    #[test]
    fn an_invalid_secret_name_inside_the_envelope_is_refused() {
        for bad in ["${secret://BadName}", "${secret://}", "${secret://a b}"] {
            let err = resolve(bad).unwrap_err();
            assert!(err.contains("lowercase kebab"), "explains the grammar for {bad}: {err}");
        }
    }

    #[test]
    fn a_bare_scheme_is_literal_text_whatever_the_vault_holds() {
        // The URL that decided the grammar (docs/25 §0): no envelope, no reference —
        // byte-identical passthrough with the name stored AND with it absent.
        plant("refs-url", "v");
        let stored = "https://test.com/secret://refs-url/tail";
        assert_eq!(resolve(stored).unwrap(), stored);
        let absent = "https://test.com/secret://refs-never-planted/tail";
        assert_eq!(resolve(absent).unwrap(), absent);
        // Punctuation after the scheme claims no name either — same as ever.
        assert_eq!(resolve("see secret:// docs").unwrap(), "see secret:// docs");
        assert_eq!(resolve("secret://").unwrap(), "secret://");
    }

    #[test]
    fn an_unterminated_envelope_is_literal_text() {
        // No closing brace, no envelope: the '$' passes through and nothing is claimed.
        plant("refs-present", "v");
        assert_eq!(
            resolve("${secret://refs-present").unwrap(),
            "${secret://refs-present"
        );
        assert_eq!(resolve("Bearer ${secret://x").unwrap(), "Bearer ${secret://x");
    }

    #[test]
    fn both_families_mix_in_one_string() {
        unsafe { std::env::set_var("SWISS_REFS_TEST_B", "beta") };
        plant("refs-mixed", "gamma");
        assert_eq!(
            resolve("${SWISS_REFS_TEST_B}/${secret://refs-mixed}/tail").unwrap(),
            "beta/gamma/tail"
        );
    }

    #[test]
    fn a_nested_envelope_scans_from_the_inside_out() {
        // The outer '${' claims up to the FIRST '}', which names no family it knows, so
        // the '$' passes through and the inner reference resolves on its own pass.
        plant("refs-nested", "v");
        assert_eq!(resolve("${${secret://refs-nested}}").unwrap(), "${v}");
    }

    #[test]
    fn replaced_values_are_not_rescanned() {
        // The planted value itself contains an env ref — data, not a second-order reference.
        plant("refs-indirect", "x=${SWISS_REFS_TEST_NEVER_SET}");
        assert_eq!(
            resolve("${secret://refs-indirect}").unwrap(),
            "x=${SWISS_REFS_TEST_NEVER_SET}"
        );
    }

    #[test]
    fn collect_returns_what_secrets_resolved_to() {
        plant("refs-collect", "sk_mask_me");
        let (out, resolved) = resolve_collect("a ${secret://refs-collect} b").unwrap();
        assert_eq!(out, "a sk_mask_me b");
        assert_eq!(resolved, vec!["sk_mask_me".to_string()]);
    }

    #[test]
    fn value_walk_reports_the_json_path() {
        plant("refs-walk", "v");
        let def = json!({ "headers": { "Authorization": "${secret://refs-walk}" } });
        let resolved = resolve_value(&def).unwrap();
        assert_eq!(resolved["headers"]["Authorization"], "v");

        let bad = json!({ "headers": { "Authorization": "${secret://refs-walk-missing}" } });
        let err = resolve_value(&bad).unwrap_err();
        assert!(
            err.starts_with("headers.Authorization "),
            "carries the path: {err}"
        );

        let arr = json!({ "args": ["--key", "${secret://refs-walk-missing}"] });
        let err = resolve_value(&arr).unwrap_err();
        assert!(err.starts_with("args[1] "), "arrays carry an index: {err}");
    }

    #[test]
    fn migration_rewrites_whole_value_bare_refs_only() {
        let mut v = json!({
            "headers": {
                "X-Key": "secret://m-1",
                "Auth": "Bearer secret://m-2",
                "Url": "https://x/secret://m-1/tail",
                "Keep": "plain"
            },
            "args": ["secret://m-3", "no-ref"],
            "shaped": "secret://Bad-Name"
        });
        assert_eq!(migrate_legacy(&mut v), 2);
        assert_eq!(v["headers"]["X-Key"], "${secret://m-1}");
        assert_eq!(v["args"][0], "${secret://m-3}");
        // Mixed strings, URLs, non-refs and invalid names stay byte-identical: a mid-string
        // rewrite would need the ambiguous scan the envelope exists to replace.
        assert_eq!(v["headers"]["Auth"], "Bearer secret://m-2");
        assert_eq!(v["headers"]["Url"], "https://x/secret://m-1/tail");
        assert_eq!(v["headers"]["Keep"], "plain");
        assert_eq!(v["args"][1], "no-ref");
        assert_eq!(v["shaped"], "secret://Bad-Name");
        // Idempotent: a second pass over the same tree finds nothing.
        assert_eq!(migrate_legacy(&mut v), 0);
    }
}
