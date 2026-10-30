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

//! The one resolver every credential string passes on its way to a live use (SPEC §host.refs,
//! grammar revised by SPEC §host.refs).
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
//! - `${secret://kebab-name:default}` — the same ref with an operator-declared fallback: the
//!   text after the first `:` (up to the closing brace, so it cannot hold a `}`) stands in
//!   when the vault does not hold the name. The default is the operator's own literal, so it
//!   is never collected for masking; the failure stays hard for a ref without one. A name
//!   cannot contain `:`, so the form was an invalid reference before it meant anything.
//!
//! Outside the envelope there are NO references. A bare `secret://` is literal text —
//! `https://x/secret://aaa/y` passes through byte-identical whatever the vault holds,
//! because without delimiters a scheme substring cannot be told apart from a URL path (the
//! defect SPEC §host.refs records as the reason the bare scheme was retired).
//!
//! A replaced value is never re-scanned: a secret whose value itself contains `${X}` is data,
//! not a second-order reference. The scan advances one CHARACTER past anything it does not
//! consume, so multi-byte text survives untouched.

use serde_json::{Map, Value};

use super::envstore::env_lookup;
use super::secretstore::{valid_name, vault_lookup};

/// The scheme a vault reference carries inside the envelope.
const SECRET_SCHEME: &str = "secret://";

/// Resolve every reference in ONE string (SPEC §host.refs). Err carries the
/// sentence an operator reads — it names the reference, never a value.
pub fn resolve(input: &str) -> Result<String, String> {
    resolve_collect(input).map(|(out, _)| out)
}

/// Resolve and also collect what each secret ref resolved to. Callers that capture output
/// (the actions' run paths) mask exactly these values, the same discipline env refs already
/// have there: what a ref produced is a credential; what the user typed as a literal is config.
pub fn resolve_collect(input: &str) -> Result<(String, Vec<String>), String> {
    resolve_families(input, true, false, &vault_lookup)
}

/// Strict for BOTH families (SPEC §process.apps): a missing env reference is a named error
/// the way a missing vault name always was — the `process.exec` contract, now shared by the
/// Process apps' start path. Also collects every value a reference produced (env and vault
/// alike), because that is exactly the set the caller masks out of what it shows. Nothing is
/// stored resolved.
pub fn resolve_strict_collect(input: &str) -> Result<(String, Vec<String>), String> {
    resolve_families(input, true, true, &vault_lookup)
}

/// The vault half alone: `${secret://...}` resolves, `${UPPER}` stays text. For a command
/// that runs on ANOTHER machine (a remote exec), whose `${HOME}` is that machine's shell's
/// to expand - substituting this machine's value there would be a silent wrong answer.
pub fn resolve_secrets_collect(input: &str) -> Result<(String, Vec<String>), String> {
    resolve_families(input, false, false, &vault_lookup)
}

/// Is every vault reference in the string well-formed? The question validation asks - a job
/// saved, a job loaded at boot - where "is it stored" is the wrong one: the vault may gain the
/// name later, and the run is where a missing one fails. Never reads the vault.
pub fn check_secret_refs(input: &str) -> Result<(), String> {
    resolve_families(input, false, false, &|_| Some(String::new())).map(|_| ())
}

/// Split what follows `secret://` into the name and its optional default, and validate the
/// name. None when the name is malformed - the caller refuses rather than guessing.
pub fn secret_ref_parts(rest: &str) -> Option<(&str, Option<&str>)> {
    let (name, default) = match rest.split_once(':') {
        Some((name, default)) => (name, Some(default)),
        None => (rest, None),
    };
    valid_name(name).then_some((name, default))
}

/// Does one string reference the vault secret `name` - `${secret://name}`, the defaulted
/// `${secret://name:...}` anywhere inside it, or the legacy bare whole value `secret://name`
/// (SPEC §host.refs)? The question a secret replace asks of every definition string it may have
/// to rebuild.
pub fn names_secret(s: &str, name: &str) -> bool {
    s == format!("{SECRET_SCHEME}{name}")
        || s.contains(&format!("${{{SECRET_SCHEME}{name}}}"))
        || s.contains(&format!("${{{SECRET_SCHEME}{name}:"))
}

/// What one vault reference stood for after [secret_refs_aside]: the name, and the text the
/// stand-in carries - the stored value, or the reference's own default when the vault does
/// not hold the name (`stored` false: the operator's literal, never masked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAside {
    pub name: String,
    pub value: String,
    pub stored: bool,
}

/// Rewrite every vault reference into what `stand_in(n)` spells for the n-th distinct one
/// (from 0) and hand back what each stands for, in that order: the values leave the text
/// entirely, for a shell command whose values travel in its child's environment, never on
/// its command line or through its parser (SPEC §llm.agents Test). `expands` says,
/// by the character index of its `$`, whether a reference sits where the consumer expands
/// it; one that does not (a shell's single quotes) stays text, needing no grant. `gate`
/// speaks next for every name, before the vault is read, so a refused name is refused
/// whether it is stored or not. `${UPPER}` stays text, as in [resolve_secrets_collect]; a
/// malformed reference and a missing name without a default fail as they do there.
pub fn secret_refs_aside(
    input: &str,
    expands: &dyn Fn(usize) -> bool,
    gate: &dyn Fn(&str) -> Result<(), String>,
    stand_in: &dyn Fn(usize) -> String,
) -> Result<(String, Vec<SetAside>), String> {
    let mut aside: Vec<(String, Option<String>, SetAside)> = Vec::new();
    let out = scan(input, &mut |reference| match reference {
        Ref::Env(_) => Ok(None),
        Ref::Secret { at, .. } if !expands(at) => Ok(None),
        Ref::Secret { name, default, .. } => {
            gate(name)?;
            let key = (name.to_string(), default.map(str::to_string));
            if let Some(at) = aside.iter().position(|(n, d, _)| (n, d) == (&key.0, &key.1)) {
                return Ok(Some(stand_in(at)));
            }
            let one = match (vault_lookup(name), default) {
                (Some(value), _) => SetAside { name: name.to_string(), value, stored: true },
                (None, Some(default)) => SetAside { name: name.to_string(), value: default.to_string(), stored: false },
                (None, None) => return Err(format!("references secret://{name} which is not in the vault")),
            };
            aside.push((key.0, key.1, one));
            Ok(Some(stand_in(aside.len() - 1)))
        }
    })?;
    Ok((out, aside.into_iter().map(|(_, _, one)| one).collect()))
}

/// One reference the scanner found inside a `${...}` envelope; a vault one knows the
/// character index of its `$`.
enum Ref<'a> {
    Secret { name: &'a str, default: Option<&'a str>, at: usize },
    Env(&'a str),
}

/// The one envelope scanner every pass shares (SPEC §host.refs). `on` answers each
/// well-formed reference with its replacement, or None to leave the `$` as plain text; a
/// malformed vault reference refuses here, before `on` hears of it.
fn scan(input: &str, on: &mut dyn FnMut(Ref<'_>) -> Result<Option<String>, String>) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // --- the envelope: ${...} ----------------------------------------------------
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            if let Some(close) = (i + 2..chars.len()).find(|&j| chars[j] == '}') {
                let content: String = chars[i + 2..close].iter().collect();
                let claimed = if let Some(rest) = content.strip_prefix(SECRET_SCHEME) {
                    // Vault refs: the scheme inside the envelope is a declared intent. A
                    // well-formed name resolves or fails hard; a malformed one refuses
                    // rather than shipping a credential-shaped literal to a third party.
                    let Some((name, default)) = secret_ref_parts(rest) else {
                        let shown: String = content.chars().take(24).collect();
                        return Err(format!(
                            "invalid reference '${{{shown}}}' — secret names are lowercase kebab ([a-z][a-z0-9-]{{0,63}}), optionally followed by :default"
                        ));
                    };
                    on(Ref::Secret { name, default, at: i })?
                } else if !content.is_empty()
                    && content
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    on(Ref::Env(&content))?
                } else {
                    None
                };
                if let Some(text) = claimed {
                    out.push_str(&text);
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
    Ok(out)
}

fn resolve_families(
    input: &str,
    env_refs: bool,
    strict_env: bool,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(String, Vec<String>), String> {
    let mut resolved: Vec<String> = Vec::new();
    let out = scan(input, &mut |reference| match reference {
        Ref::Secret { name, default, .. } => match (lookup(name), default) {
            (Some(value), _) => {
                resolved.push(value.clone());
                Ok(Some(value))
            }
            (None, Some(default)) => Ok(Some(default.to_string())),
            (None, None) => Err(format!("references secret://{name} which is not in the vault")),
        },
        // Env refs: the pre-vault grammar, lenient by default (SPEC §host.refs) — the
        // vault-only pass leaves them as text for the far side's shell. The strict pass
        // (SPEC §process.apps) turns a missing name into a named error instead: the
        // operator declared a variable this machine does not hold.
        Ref::Env(_) if !env_refs => Ok(None),
        Ref::Env(name) => match env_lookup(name) {
            Some(value) => {
                if strict_env {
                    // The strict caller masks what a ref produced, env included.
                    resolved.push(value.clone());
                }
                Ok(Some(value))
            }
            None if strict_env => Err(format!("references env var ${{{name}}} which is not set")),
            None => Ok(Some(String::new())),
        },
    })?;
    Ok((out, resolved))
}

/// The one-time shape normalization (SPEC §host.refs): rewrite WHOLE-VALUE bare vault refs
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
        Value::String(s) => resolve(s).map(Value::String).map_err(|e| {
            if path.is_empty() {
                e
            } else {
                format!("{path} {e}")
            }
        }),
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
    fn strict_resolution_refuses_a_missing_env_var_and_a_missing_secret() {
        // The process.exec contract (SPEC §process.apps): a declared-but-absent credential is
        // a configuration error, never an empty one. Both errors name the reference; neither
        // can name a value, because none exists.
        unsafe { std::env::set_var("SWISS_REFS_STRICT_SET", "strict-value") };
        let (out, collected) =
            resolve_strict_collect("a=${SWISS_REFS_STRICT_SET}").expect("resolves");
        assert_eq!(out, "a=strict-value");
        assert_eq!(collected, ["strict-value".to_string()], "env values collected too");

        let err = resolve_strict_collect("b=${SWISS_REFS_STRICT_MISSING}").unwrap_err();
        assert!(
            err.contains("SWISS_REFS_STRICT_MISSING") && err.contains("which is not set"),
            "{err}"
        );
        let err = resolve_strict_collect("c=${secret://refs-strict-absent}").unwrap_err();
        assert!(
            err.contains("secret://refs-strict-absent") && err.contains("not in the vault"),
            "{err}"
        );
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
            assert!(
                err.contains("lowercase kebab"),
                "explains the grammar for {bad}: {err}"
            );
        }
    }

    #[test]
    fn a_bare_scheme_is_literal_text_whatever_the_vault_holds() {
        // The URL that decided the grammar (SPEC §host.refs): no envelope, no reference —
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
        assert_eq!(
            resolve("Bearer ${secret://x").unwrap(),
            "Bearer ${secret://x"
        );
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

    #[test]
    fn a_default_stands_in_only_when_the_secret_is_missing() {
        plant("refs-default-held", "held-value-123");
        let (out, got) = resolve_collect("a=${secret://refs-default-held:fallback}").expect("held");
        assert_eq!(
            out, "a=held-value-123",
            "a held secret wins over its default"
        );
        assert_eq!(got, ["held-value-123"]);

        let (out, got) =
            resolve_collect("a=${secret://refs-default-absent:fallback} b").expect("defaulted");
        assert_eq!(out, "a=fallback b");
        assert!(
            got.is_empty(),
            "a default is the operator's own literal, not a credential"
        );
        assert_eq!(
            resolve("${secret://refs-default-absent:}").unwrap(),
            "",
            "an empty default"
        );
        // The default runs to the closing brace, colons and slashes included.
        assert_eq!(
            resolve("${secret://refs-default-absent:redis://h:6379/0}").unwrap(),
            "redis://h:6379/0"
        );
        // Without a default a missing secret still fails hard (SPEC §host.refs).
        let err = resolve("${secret://refs-default-absent}").unwrap_err();
        assert!(err.contains("not in the vault"), "{err}");
    }

    #[test]
    fn a_default_never_makes_a_malformed_name_valid() {
        for bad in [
            "${secret://Bad_Name:x}",
            "${secret://:x}",
            "${secret://9lives:x}",
        ] {
            assert!(resolve(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_syntax_check_never_consults_the_vault() {
        // Validation (a job saved or loaded) asks "is this a reference" - not "is it stored".
        // A name the vault does not hold yet passes: the run is where a missing one fails.
        assert!(check_secret_refs("echo ${secret://refs-check-absent}").is_ok());
        assert!(check_secret_refs("${secret://refs-check-absent:fallback}").is_ok());
        assert!(check_secret_refs("plain ${HOME} text").is_ok());
        for bad in [
            "${secret://Bad}",
            "${secret://Bad_Name:x}",
            "x ${secret://}",
        ] {
            assert!(check_secret_refs(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn the_vault_only_pass_leaves_env_refs_for_the_far_side() {
        // A remote command's ${HOME} is the REMOTE shell's to expand; only vault refs are ours.
        plant("refs-vault-only", "vault-only-value");
        unsafe { std::env::set_var("SWISS_REFS_FAR", "local") };
        let (out, got) =
            resolve_secrets_collect("echo ${SWISS_REFS_FAR} $HOME ${secret://refs-vault-only}")
                .expect("resolves");
        assert_eq!(out, "echo ${SWISS_REFS_FAR} $HOME vault-only-value");
        assert_eq!(got, ["vault-only-value"]);
        assert!(resolve_secrets_collect("${secret://refs-vault-absent}").is_err());
    }

    #[test]
    fn set_aside_moves_the_values_out_of_the_text() {
        // The shell path: every reference becomes its stand-in, the values travel beside the
        // text, a repeat reuses its stand-in, and ${UPPER} stays for the shell.
        plant("refs-aside-key", "aside-key-value");
        let (out, aside) = secret_refs_aside(
            "curl -H \"X: ${secret://refs-aside-key}\" ${HOME} ${secret://refs-aside-absent:dev} ${secret://refs-aside-key}",
            &|_| true,
            &|_| Ok(()),
            &|n| format!("<{n}>"),
        )
        .expect("sets aside");
        assert_eq!(out, "curl -H \"X: <0>\" ${HOME} <1> <0>");
        assert_eq!(
            aside,
            [
                SetAside { name: "refs-aside-key".into(), value: "aside-key-value".into(), stored: true },
                SetAside { name: "refs-aside-absent".into(), value: "dev".into(), stored: false },
            ]
        );
        assert!(!out.contains("aside-key-value"), "the value never stays in the text");
        let err = secret_refs_aside("${secret://refs-aside-nowhere}", &|_| true, &|_| Ok(()), &|n| n.to_string()).unwrap_err();
        assert!(err.contains("secret://refs-aside-nowhere") && err.contains("not in the vault"), "{err}");
    }

    #[test]
    fn set_aside_asks_the_gate_before_the_vault() {
        // A refused name is refused alike stored or not: the gate speaks first.
        plant("refs-aside-gated", "gated-value-123");
        let gate = |name: &str| if name == "refs-aside-open" { Ok(()) } else { Err(format!("{name} is not granted")) };
        for name in ["refs-aside-gated", "refs-aside-never"] {
            let err = secret_refs_aside(&format!("${{secret://{name}}}"), &|_| true, &gate, &|n| n.to_string()).unwrap_err();
            assert_eq!(err, format!("{name} is not granted"));
        }
        assert!(secret_refs_aside("${secret://Bad}", &|_| true, &gate, &|n| n.to_string()).unwrap_err().contains("lowercase kebab"));
        // Where the consumer does not expand it, a reference is text: no gate, no vault.
        let (out, aside) = secret_refs_aside("a '${secret://refs-aside-never}' b", &|at| at != 3, &gate, &|n| n.to_string())
            .expect("kept as text");
        assert_eq!((out.as_str(), aside.len()), ("a '${secret://refs-aside-never}' b", 0));
    }

    #[test]
    fn a_reference_names_its_secret_with_or_without_a_default() {
        assert!(names_secret("Bearer ${secret://api-key}", "api-key"));
        assert!(names_secret("${secret://api-key:dev-key}", "api-key"));
        assert!(
            names_secret("secret://api-key", "api-key"),
            "the legacy whole value"
        );
        assert!(!names_secret("${secret://api-key-2}", "api-key"));
        assert!(!names_secret("${secret://api-key-2:x}", "api-key"));
        assert!(
            !names_secret("Bearer secret://api-key", "api-key"),
            "a bare mid-string scheme is text"
        );
    }
}
