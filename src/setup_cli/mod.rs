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

//! Configuring swiss from the command line, for an Agent or a script (SPEC §host.cli). Everything
//! here runs inside the CLI process and reaches the gateway through the same `/api/*` routes the
//! panel uses, signed with the CLI key; it holds no state and adds nothing to the daemon.
//!
//! One document on stdout for every outcome, and it never holds a secret's value.

mod sources;

use std::io::{IsTerminal, Read};
use std::path::Path;

use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::daemon::{start_daemon, StartOptions, StartResult};
use crate::remote_cli::{decode_text, Gateway};
use sources::{trim_newline, Source};
use swiss_core::secure::secretstore::valid_name;

const EXIT_FAILED: i32 = 1;
const EXIT_REFUSED: i32 = 2;
const EXIT_NO_GATEWAY: i32 = 3;

/// A 409 on the vault's `rev` means someone else wrote first: read again, this many times.
const CONFLICT_RETRIES: usize = 3;

const SECRET_USAGE: &str = "usage: swiss secret set <name> [--from <env:VAR|file:PATH|dotenv:PATH#KEY>]\n\
    The value comes from the source, or from piped stdin. It is never taken from the command line.";

const TERMINAL_REFUSAL: &str = "stdin is a terminal, and a value typed here would echo. Pipe it in, \
    use --from env:VAR, file:PATH or dotenv:PATH#KEY, or have whoever holds it store it on the \
    panel's Secrets page.";

/// The entry from cli.rs: argv is everything after `swiss secret`.
pub async fn secret_main(argv: Vec<String>) -> i32 {
    match argv.split_first() {
        Some((sub, rest)) if sub == "set" => secret_set(rest).await,
        Some((sub, _)) if matches!(sub.as_str(), "help" | "-h" | "--help") => {
            println!("{SECRET_USAGE}");
            0
        }
        _ => refuse(SECRET_USAGE),
    }
}

async fn secret_set(argv: &[String]) -> i32 {
    let args = match parse_set(argv) {
        Ok(args) => args,
        Err(refusal) => return refuse(&refusal),
    };
    let here = match std::env::current_dir() {
        Ok(here) => here,
        Err(err) => return refuse(&format!("cannot read the working directory: {err}")),
    };
    let stdin = std::io::stdin();
    let value = read_value(args.from.as_deref(), stdin.is_terminal(), &mut stdin.lock(), &here);
    let value = match value {
        Ok(value) => value,
        Err(refusal) => return refuse(&refusal),
    };
    let up = match ensure_gateway().await {
        Ok(up) => up,
        Err(err) => {
            print_document(&json!({ "ok": false, "error": err }));
            return EXIT_NO_GATEWAY;
        }
    };
    let stored = store_secret(&Gateway::at(up.port), &args.name, &value).await;
    let (document, code) = set_report(&up, &args.name, stored);
    print_document(&document);
    code
}

#[derive(Debug, PartialEq, Eq)]
struct SetArgs {
    name: String,
    from: Option<String>,
}

/// Nothing past the name is quoted in a refusal: a word that does not belong here may be the value.
fn parse_set(argv: &[String]) -> Result<SetArgs, String> {
    let Some((name, rest)) = argv.split_first() else {
        return Err(SECRET_USAGE.into());
    };
    if !valid_name(name) {
        return Err(format!(
            "a secret's name is a lowercase letter, then lowercase letters, digits or -, 64 at most\n{SECRET_USAGE}"
        ));
    }
    let from = match rest {
        [] => None,
        [flag, source] if flag == "--from" => Some(source.clone()),
        _ => {
            return Err(format!(
                "secret set takes no value on the command line, where the process list and the transcript would keep it\n{SECRET_USAGE}"
            ))
        }
    };
    Ok(SetArgs { name: name.clone(), from })
}

/// The value, from its source or piped stdin. Without a source, a terminal is refused before a byte
/// is read: reading it would echo what the person types.
fn read_value(
    from: Option<&str>,
    stdin_is_terminal: bool,
    stdin: &mut dyn Read,
    here: &Path,
) -> Result<String, String> {
    let (text, label) = match from {
        Some(spec) => {
            let source = Source::parse(spec)?;
            (source.read(here)?, source.label())
        }
        None if stdin_is_terminal => return Err(TERMINAL_REFUSAL.into()),
        None => {
            let mut raw = Vec::new();
            stdin
                .read_to_end(&mut raw)
                .map_err(|err| format!("cannot read stdin: {err}"))?;
            let text = decode_text(&raw).ok_or("stdin is not text")?;
            (text, "stdin".to_string())
        }
    };
    let value = trim_newline(text);
    if value.is_empty() {
        return Err(format!("{label} is empty, and an empty value is never a secret"));
    }
    Ok(value)
}

/// The gateway this process talks to.
struct GatewayUp {
    port: u16,
    /// This call started it: the report says so, because it now outlives the command.
    started: bool,
}

/// The gateway, started without a browser when it is down.
async fn ensure_gateway() -> Result<GatewayUp, String> {
    match start_daemon(StartOptions::default()).await {
        StartResult::AlreadyRunning { port, .. } => Ok(GatewayUp { port, started: false }),
        StartResult::Started { port, .. } => {
            eprintln!("started the gateway on port {port}");
            Ok(GatewayUp { port, started: true })
        }
        StartResult::Failed { port, .. } => {
            Err(format!("the gateway on port {port} did not start; `swiss logs` says why"))
        }
    }
}

#[derive(Debug)]
struct Stored {
    status: &'static str,
    /// What the server rebuilt on the new value, and what it could not, as it reported them.
    refreshed: Value,
    failed: Value,
}

/// Write one secret against the rev just read. The vault cannot be read back, so `updated` means
/// the name existed, not that the value differs.
async fn store_secret(gw: &Gateway, name: &str, value: &str) -> Result<Stored, String> {
    for _ in 0..=CONFLICT_RETRIES {
        let listing = gw.get("/api/secrets").await?;
        let rev = listing["rev"]
            .as_u64()
            .ok_or("the gateway reported no vault rev")?;
        let existed = listing["secrets"]
            .as_array()
            .is_some_and(|names| names.iter().any(|known| known == name));
        let (status, answer) = gw
            .exchange(
                reqwest::Method::PUT,
                &format!("/api/secrets/{name}"),
                Some(json!({ "value": value, "rev": rev })),
            )
            .await?;
        if status == StatusCode::CONFLICT {
            continue;
        }
        if !status.is_success() {
            let message = answer["error"].as_str().unwrap_or("no detail");
            return Err(format!("{} {message}", status.as_u16()));
        }
        return Ok(Stored {
            status: if existed { "updated" } else { "created" },
            refreshed: answer["refreshed"].clone(),
            failed: answer["failed"].clone(),
        });
    }
    Err("the vault kept changing while this wrote; run it again".into())
}

/// The document for one `secret set`, and its exit code. A stored value that an MCP could not
/// rebuild on is still stored; `ok` false says the machine is not yet right.
fn set_report(up: &GatewayUp, name: &str, stored: Result<Stored, String>) -> (Value, i32) {
    let gateway = json!({ "port": up.port, "started": up.started });
    match stored {
        Ok(stored) => {
            let ok = stored.failed.as_array().is_none_or(Vec::is_empty);
            let item = json!({
                "kind": "secret",
                "name": name,
                "status": stored.status,
                "refreshed": stored.refreshed,
                "failed": stored.failed,
            });
            let document = json!({ "ok": ok, "gateway": gateway, "items": [item] });
            (document, if ok { 0 } else { EXIT_FAILED })
        }
        Err(error) => {
            let item = json!({ "kind": "secret", "name": name, "status": "failed", "error": error });
            (json!({ "ok": false, "gateway": gateway, "items": [item] }), EXIT_FAILED)
        }
    }
}

/// Bad usage, or a value that could not be read: nothing was written.
fn refuse(error: &str) -> i32 {
    print_document(&json!({ "ok": false, "error": error }));
    EXIT_REFUSED
}

fn print_document(document: &Value) {
    println!("{}", serde_json::to_string_pretty(document).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use axum::extract::{Path as RoutePath, State};
    use axum::routing::{get, put};

    const SENTINEL: &str = "sk-live-sentinel-7f3a";

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A reader that fails the test if anyone touches it.
    struct Untouchable;
    impl Read for Untouchable {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("a terminal must not be read");
        }
    }

    // SPEC §host.cli: "a terminal stdin is refused" before anything can echo.
    #[test]
    fn a_terminal_stdin_is_refused() {
        let err = read_value(None, true, &mut Untouchable, Path::new(".")).unwrap_err();
        assert!(err.contains("terminal") && err.contains("--from"), "{err}");
        // A source needs no stdin at all, so a terminal is fine beside one.
        let missing = read_value(Some("file:absent"), true, &mut Untouchable, Path::new("."));
        assert!(missing.unwrap_err().contains("absent"));
    }

    // SPEC §host.cli: one trailing newline goes, and an empty value is refused.
    #[test]
    fn piped_stdin_is_the_value_with_one_newline_trimmed() {
        let piped = |bytes: &[u8]| read_value(None, false, &mut &*bytes, Path::new("."));
        assert_eq!(piped(b"tok\n").as_deref(), Ok("tok"));
        assert_eq!(piped(b"tok\r\n").as_deref(), Ok("tok"));
        assert_eq!(piped(b"tok\n\n").as_deref(), Ok("tok\n"));
        assert_eq!(piped(b"\xEF\xBB\xBFtok").as_deref(), Ok("tok"));
        for empty in [&b""[..], b"\n", b"\r\n"] {
            let err = piped(empty).unwrap_err();
            assert!(err.contains("empty"), "{err}");
        }
        let err = piped(&[0x41, 0xFF, 0x42]).unwrap_err();
        assert!(err.contains("not text"), "{err}");
    }

    // SPEC §host.cli: there is no value flag, and what stands in for one is not repeated back.
    #[test]
    fn a_value_on_the_command_line_is_refused_without_quoting_it() {
        for words in [
            vec!["glm-key", SENTINEL],
            vec!["glm-key", "--value", SENTINEL],
            vec!["glm-key", "--from", "env:A", SENTINEL],
        ] {
            let err = parse_set(&argv(&words)).unwrap_err();
            assert!(err.contains("no value on the command line"), "{err}");
            assert!(!err.contains("sentinel"), "{err}");
        }
        let err = parse_set(&argv(&["Not_A_Name"])).unwrap_err();
        assert!(err.contains("lowercase") && !err.contains("Not_A_Name"), "{err}");
        assert!(parse_set(&[]).is_err());
        assert_eq!(
            parse_set(&argv(&["glm-key", "--from", "env:A"])),
            Ok(SetArgs { name: "glm-key".into(), from: Some("env:A".into()) })
        );
        assert_eq!(parse_set(&argv(&["glm-key"])), Ok(SetArgs { name: "glm-key".into(), from: None }));
    }

    #[derive(Default)]
    struct FakeVault {
        rev: u64,
        names: Vec<String>,
        /// How many writes answer 409 first.
        conflicts: usize,
        bodies: Vec<Value>,
        failed: Value,
    }

    /// The two vault routes, with the rev rule the real ones keep.
    async fn serve(vault: FakeVault) -> (Gateway, Arc<Mutex<FakeVault>>) {
        type Shared = Arc<Mutex<FakeVault>>;
        let shared: Shared = Arc::new(Mutex::new(vault));
        let app = axum::Router::new()
            .route(
                "/api/secrets",
                get(|State(vault): State<Shared>| async move {
                    let vault = vault.lock().expect("the fake vault");
                    axum::Json(json!({ "rev": vault.rev, "secrets": vault.names }))
                }),
            )
            .route(
                "/api/secrets/{name}",
                put(
                    |State(vault): State<Shared>,
                     RoutePath(name): RoutePath<String>,
                     axum::Json(body): axum::Json<Value>| async move {
                        let mut vault = vault.lock().expect("the fake vault");
                        vault.bodies.push(body.clone());
                        if vault.conflicts > 0 {
                            vault.conflicts -= 1;
                            vault.rev += 1;
                            return (
                                axum::http::StatusCode::CONFLICT,
                                axum::Json(json!({ "error": "stale" })),
                            );
                        }
                        if name == "refused" {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({ "error": "no such name" })),
                            );
                        }
                        assert_eq!(body["rev"], json!(vault.rev), "written against the rev just read");
                        vault.rev += 1;
                        vault.names.push(name);
                        let failed = vault.failed.clone();
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(json!({ "rev": vault.rev, "refreshed": ["db"], "failed": failed })),
                        )
                    },
                ),
            )
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("a bound address").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (Gateway::at(port), shared)
    }

    const UP: GatewayUp = GatewayUp { port: 1, started: false };

    #[tokio::test]
    async fn a_new_name_is_created_and_a_known_one_updated() {
        let (gw, vault) = serve(FakeVault { failed: json!([]), ..FakeVault::default() }).await;
        let (document, code) = set_report(&UP, "a-key", store_secret(&gw, "a-key", SENTINEL).await);
        assert_eq!(code, 0, "{document}");
        assert_eq!(document["ok"], json!(true));
        assert_eq!(document["items"][0]["status"], json!("created"));
        assert_eq!(document["items"][0]["refreshed"], json!(["db"]));
        assert!(!document.to_string().contains("sentinel"), "{document}");
        assert_eq!(vault.lock().expect("the fake vault").bodies[0]["value"], json!(SENTINEL));

        let (document, code) = set_report(&UP, "a-key", store_secret(&gw, "a-key", SENTINEL).await);
        assert_eq!(code, 0, "{document}");
        assert_eq!(document["items"][0]["status"], json!("updated"));
    }

    // SPEC §host.cli: a stale rev is read again, up to three times.
    #[tokio::test]
    async fn a_conflict_reads_again_and_gives_up_after_three_retries() {
        let (gw, vault) = serve(FakeVault { conflicts: 3, failed: json!([]), ..FakeVault::default() }).await;
        let stored = store_secret(&gw, "a-key", SENTINEL).await.expect("the fourth try lands");
        assert_eq!(stored.status, "created");
        assert_eq!(vault.lock().expect("the fake vault").bodies.len(), 4);

        let (gw, _) = serve(FakeVault { conflicts: 4, ..FakeVault::default() }).await;
        let err = store_secret(&gw, "a-key", SENTINEL).await.unwrap_err();
        assert!(err.contains("run it again"), "{err}");
    }

    #[tokio::test]
    async fn a_refusal_and_a_failed_rebuild_both_exit_1_without_the_value() {
        let (gw, _) = serve(FakeVault::default()).await;
        let (document, code) = set_report(&UP, "refused", store_secret(&gw, "refused", SENTINEL).await);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(document["items"][0]["status"], json!("failed"));
        assert!(document["items"][0]["error"].as_str().unwrap_or_default().contains("no such name"));
        assert!(!document.to_string().contains("sentinel"), "{document}");

        // Stored, but an MCP using it did not come back: the value is there, the machine is not right.
        let (gw, _) = serve(FakeVault {
            failed: json!([{ "name": "db", "error": "refused" }]),
            ..FakeVault::default()
        })
        .await;
        let (document, code) = set_report(&UP, "a-key", store_secret(&gw, "a-key", SENTINEL).await);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(document["ok"], json!(false));
        assert_eq!(document["items"][0]["status"], json!("created"));
    }
}
