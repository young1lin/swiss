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

//! Configuring swiss from the command line, for an Agent or a script (SPEC §host.setup). Everything
//! here runs inside the CLI process and reaches the gateway through the same `/api/*` routes the
//! panel uses, signed with the CLI key; it holds no state and adds nothing to the daemon.
//!
//! One document on stdout for every outcome, and it never holds a secret's value.

mod apply;
mod ask;
mod check;
mod connect;
mod machine;
mod manifest;
mod plan;
mod plugins;
mod refuse;
mod remote;
mod report;
mod show;
mod sources;
mod tunnels;

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::daemon::{resolve_port, start_daemon, StartOptions, StartResult};
use crate::remote_cli::{decode_text, read_path_or_stdin, Gateway};
use manifest::Manifest;
pub use ask::ask_on;
pub use connect::{connect_on, ConnectEnv};
pub use plan::Options;
pub use show::show_at;
use report::{Mode, Report, Status, EXIT_FAILED, EXIT_NO_GATEWAY, EXIT_REFUSED};
use sources::{trim_newline, Source};
use swiss_core::secure::secretstore::valid_name;

/// A 409 on the vault's `rev` or a plugin's `revision` means someone else wrote first: read again,
/// this many times.
const CONFLICT_RETRIES: usize = 3;

const SECRET_USAGE: &str = "usage: swiss secret set <name> [--from <env:VAR|file:PATH|dotenv:PATH#KEY>]\n       \
    swiss secret ask <name>... [--wait <seconds>]\n\
    set takes the value from the source or from piped stdin, never from the command line. \
    ask opens the panel on those names and does not print the sign-in link.";

const TERMINAL_REFUSAL: &str = "stdin is a terminal, and a value typed here would echo. Pipe it in, \
    use --from env:VAR, file:PATH or dotenv:PATH#KEY, or have whoever holds it store it on the \
    panel's Secrets page.";

const SETUP_USAGE: &str = "usage: swiss setup schema [section | plugins.<id>]\n       \
    swiss setup show\n       \
    swiss setup guide\n       \
    swiss setup plan <file|-> [--rotate-secrets]\n       \
    swiss setup apply <file|-> [--no-check] [--rotate-secrets]\n\
    A file or stdin (-) holds one JSON manifest. swiss setup schema says what it may hold, \
    swiss setup show prints what this gateway has, without values, \
    and swiss setup guide prints SETUP.md without a gateway.";

/// The entry from cli.rs: argv is everything after `swiss setup`.
pub async fn setup_main(argv: Vec<String>) -> i32 {
    match argv.split_first() {
        Some((sub, rest)) if sub == "plan" => run_manifest(Mode::Plan, rest).await,
        Some((sub, rest)) if sub == "apply" => run_manifest(Mode::Apply, rest).await,
        Some((sub, rest)) if sub == "schema" => setup_schema(rest).await,
        Some((sub, rest)) if sub == "show" => setup_show(rest).await,
        Some((sub, rest)) if sub == "guide" => setup_guide(rest),
        Some((sub, _)) if matches!(sub.as_str(), "help" | "-h" | "--help") => {
            println!("{SETUP_USAGE}");
            0
        }
        _ => refuse(SETUP_USAGE),
    }
}

fn setup_guide(argv: &[String]) -> i32 {
    if !argv.is_empty() {
        return refuse(SETUP_USAGE);
    }
    print!("{}", include_str!("../../SETUP.md"));
    0
}

async fn setup_show(argv: &[String]) -> i32 {
    if !argv.is_empty() {
        return refuse(SETUP_USAGE);
    }
    let up = match ensure_gateway().await {
        Ok(up) => up,
        Err(error) => return print_outcome((json!({ "ok": false, "mode": "show", "error": error }), EXIT_NO_GATEWAY)),
    };
    print_outcome(show::show_on(up.port, up.started).await)
}

async fn setup_schema(argv: &[String]) -> i32 {
    let section = match argv {
        [] => None,
        [section] => Some(section.as_str()),
        _ => return refuse(SETUP_USAGE),
    };
    // What one plugin's row takes is the plugin's to say, so this one needs a gateway.
    if let Some(id) = section.and_then(|section| section.strip_prefix("plugins.")) {
        return print_outcome(match ensure_gateway().await {
            Ok(up) => plugin_schema_at(up.port, id).await,
            Err(error) => (json!({ "ok": false, "error": error }), EXIT_NO_GATEWAY),
        });
    }
    match manifest::schema(section) {
        Ok(document) => {
            print_document(&document);
            0
        }
        Err(refusal) => refuse(&refusal),
    }
}

/// What the gateway on `port` says this plugin's row takes. Its rows, which may hold a credential,
/// stay out of the answer, and an id it does not list never reaches a URL.
pub async fn plugin_schema_at(port: u16, id: &str) -> (Value, i32) {
    let gw = Gateway::at(port);
    let unavailable = |error: String| (json!({ "ok": false, "error": error }), EXIT_NO_GATEWAY);
    let rows = match plugins::read_rows(&gw).await {
        Ok(rows) => rows,
        Err(error) => return unavailable(error),
    };
    if !rows.contains_key(id) {
        let known: Vec<&str> = rows.keys().map(String::as_str).collect();
        let error = match refuse::nearest(id, &known) {
            Some(near) => format!("no such plugin; did you mean {near}?"),
            None => format!("no such plugin; there are: {}", known.join(", ")),
        };
        return (json!({ "ok": false, "error": error }), EXIT_REFUSED);
    }
    match gw.get(&format!("/api/plugins/{id}/config")).await {
        Ok(answer) => {
            let section = format!("plugins.{id}");
            let document = json!({ "ok": true, "version": manifest::VERSION, "section": section, "schema": answer["schema"] });
            (document, 0)
        }
        Err(error) => unavailable(error),
    }
}

async fn run_manifest(mode: Mode, argv: &[String]) -> i32 {
    let args = match parse_manifest_args(mode, argv) {
        Ok(args) => args,
        Err(refusal) => return refuse(&refusal),
    };
    let stdin = std::io::stdin();
    let input = match read_manifest(&args.file, stdin.is_terminal(), &mut stdin.lock()) {
        Ok(input) => input,
        Err(refusal) => return refuse(&refusal),
    };
    // Before any gateway starts: a file that is wrong has nothing to ask one.
    let parsed = match manifest::parse(&input.text, resolve_port()) {
        Ok(parsed) => parsed,
        Err(problems) => return print_outcome(report::refused(mode, &problems)),
    };
    let up = match ensure_gateway().await {
        Ok(up) => up,
        Err(error) => return print_outcome(report::no_gateway(mode, &error)),
    };
    print_outcome(run_on(mode, &up, &parsed, &input, args.options).await)
}

/// One manifest, once it is text.
pub struct Input {
    pub text: String,
    /// What its relative paths resolve against.
    pub base: PathBuf,
    /// How a next step names this file: what was typed on the command line.
    pub file: String,
}

/// A plan against the gateway already on `port`: everything `setup plan` does but read its file
/// and start a gateway.
pub async fn plan_at(port: u16, input: &Input, options: Options) -> (Value, i32) {
    run_at(Mode::Plan, port, input, options).await
}

/// An apply against the gateway already on `port`: everything `setup apply` does but read its file
/// and start a gateway.
pub async fn apply_at(port: u16, input: &Input, options: Options) -> (Value, i32) {
    run_at(Mode::Apply, port, input, options).await
}

async fn run_at(mode: Mode, port: u16, input: &Input, options: Options) -> (Value, i32) {
    match manifest::parse(&input.text, port) {
        Ok(parsed) => run_on(mode, &GatewayUp { port, started: false }, &parsed, input, options).await,
        Err(problems) => report::refused(mode, &problems),
    }
}

async fn run_on(mode: Mode, up: &GatewayUp, parsed: &Manifest, input: &Input, options: Options) -> (Value, i32) {
    let gw = Gateway::at(up.port);
    let own_build = env!("SWISS_GIT_HASH");
    let gateway_state = async {
        let info = gw.get("/api/info").await?;
        let rows = plugins::wait_until_started(&gw).await?;
        let may_dry_run = info["build"]["hash"].as_str() == Some(own_build);
        let state = plan::read_state(&gw, parsed, &rows, may_dry_run).await?;
        Ok::<_, String>((info, state))
    };
    let (info, state) = match gateway_state.await {
        Ok(found) => found,
        Err(error) => return report::no_gateway(mode, &error),
    };
    let build = info["build"]["hash"].as_str();
    let mut warnings = parsed.warnings.clone();
    warnings.extend(plan::stale_build(build, own_build));
    let mut items = plan::decide(parsed, &state, &input.base, options, &mut warnings);
    if mode == Mode::Apply {
        apply::write_all(&gw, parsed, &input.base, &mut items, &mut warnings).await;
        plugins::check(&gw, parsed, options, &mut items).await;
        tunnels::check(&gw, parsed, options, &mut items, &mut warnings).await;
        check::check_all(&gw, parsed, options, &mut items, &mut warnings).await;
    }
    if !parsed.clients.is_empty() {
        match connect::ConnectEnv::of_this_user() {
            Ok(env) => connect::sync(&gw, up.port, parsed, mode, &env, &mut items, &mut warnings).await,
            Err(error) => {
                for name in parsed.clients.keys() {
                    items.push(report::Item::new("client", name, Status::Failed).with("client", name.clone()).failed("clients.home", &error));
                }
            }
        }
    }
    let mut report = Report {
        mode,
        gateway: json!({ "port": up.port, "started": up.started, "build": build }),
        items,
        warnings,
        next: Vec::new(),
    };
    say_who_acts_next(&mut report, &input.file);
    report.finish()
}

/// What waits for a human, and that an Agent runs the same file again afterwards.
fn say_who_acts_next(report: &mut Report, file: &str) {
    let waiting = |kind: &str| -> Vec<String> {
        report
            .items
            .iter()
            .filter(|item| item.kind == kind && item.status == Status::NeedsInput)
            .map(|item| item.name.clone())
            .collect()
    };
    let (secrets, sign_ins, host_keys) = (waiting("secret"), waiting("mcp"), waiting("connection"));
    if !secrets.is_empty() {
        report.next_step("human", format!("swiss secret ask {}", secrets.join(" ")), "no source for this secret");
    }
    if !sign_ins.is_empty() {
        let what = format!("authorize {} on the panel's MCP page", sign_ins.join(", "));
        report.next_step("human", what, "only a human can sign in");
    }
    if !host_keys.is_empty() {
        report.next_step("human", format!("compare the reported SSH fingerprints for {} with a trusted source, then set hostKey in the manifest", host_keys.join(", ")),
                         "only a human can confirm the server's identity");
    }
    if !secrets.is_empty() || !sign_ins.is_empty() || !host_keys.is_empty() {
        report.next_step("agent", format!("swiss setup apply {file}"), "once that is done");
    }
    connect::note_restart(report);
}

#[derive(Debug, PartialEq, Eq)]
struct ManifestArgs {
    file: String,
    options: Options,
}

fn parse_manifest_args(mode: Mode, argv: &[String]) -> Result<ManifestArgs, String> {
    let mut file = None;
    let mut options = Options::default();
    for word in argv {
        match word.as_str() {
            "--rotate-secrets" => options.rotate_secrets = true,
            "--no-check" if mode == Mode::Apply => options.no_check = true,
            flag if flag.starts_with('-') && flag != "-" => return Err(SETUP_USAGE.into()),
            _ if file.is_some() => return Err(SETUP_USAGE.into()),
            _ => file = Some(word.clone()),
        }
    }
    let file = file.ok_or(SETUP_USAGE)?;
    Ok(ManifestArgs { file, options })
}

/// The manifest's text, from a file or from stdin for `-`. A terminal is refused before a byte is
/// read: nothing would ever end the read.
fn read_manifest(file: &str, stdin_is_terminal: bool, stdin: &mut dyn Read) -> Result<Input, String> {
    if file == "-" && stdin_is_terminal {
        return Err("stdin is a terminal; pipe the manifest in, or name its file".into());
    }
    let raw = read_path_or_stdin(file, stdin)?;
    let text = decode_text(&raw).ok_or_else(|| format!("{file} is not text"))?;
    let base = match file {
        "-" => PathBuf::new(),
        _ => Path::new(file).parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    Ok(Input { text, base, file: file.to_string() })
}

/// The entry from cli.rs: argv is everything after `swiss connect`.
pub async fn connect_main(argv: Vec<String>) -> i32 {
    if argv.iter().any(|word| word == "--help" || word == "-h") {
        println!("{}", connect::usage());
        return 0;
    }
    let args = match connect::parse(&argv) {
        Ok(args) => args,
        Err(refusal) => return refuse(&refusal),
    };
    let env = match connect::ConnectEnv::of_this_user() {
        Ok(env) => env,
        Err(error) => return refuse(&error),
    };
    let up = match ensure_gateway().await {
        Ok(up) => up,
        Err(error) => {
            print_document(&json!({ "ok": false, "mode": "connect", "error": error }));
            return EXIT_NO_GATEWAY;
        }
    };
    print_outcome(connect::connect_on(up.port, up.started, &args.client, &args.mcps, args.replace, &env).await)
}

/// The entry from cli.rs: argv is everything after `swiss secret`.
pub async fn secret_main(argv: Vec<String>) -> i32 {
    match argv.split_first() {
        Some((sub, rest)) if sub == "set" => secret_set(rest).await,
        Some((sub, rest)) if sub == "ask" => secret_ask(rest).await,
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

async fn secret_ask(argv: &[String]) -> i32 {
    if matches!(argv, [word] if word == "--help" || word == "-h") {
        println!("{SECRET_USAGE}");
        return 0;
    }
    let args = match ask::parse(argv) {
        Ok(args) => args,
        Err(refusal) => return refuse(&refusal),
    };
    let up = match ensure_gateway().await {
        Ok(up) => up,
        Err(error) => {
            print_document(&json!({ "ok": false, "mode": "ask", "error": error }));
            return EXIT_NO_GATEWAY;
        }
    };
    let port = up.port;
    print_outcome(
        ask::ask_on(port, up.started, &args.names, args.wait, || crate::daemon::login_url(port), crate::cli::try_open_in_browser)
            .await,
    )
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

fn print_outcome((document, code): (Value, i32)) -> i32 {
    print_document(&document);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use axum::extract::{Path as RoutePath, State};
    use axum::routing::{get, put};

    const SENTINEL: &str = "sk-live-sentinel-7f3a";

    #[test]
    fn the_setup_guide_names_every_command_and_exit_code() {
        let guide = include_str!("../../SETUP.md");
        for command in [
            "swiss --version",
            "swiss status",
            "swiss skill list",
            "swiss skill install",
            "swiss setup schema",
            "swiss setup show",
            "swiss setup plan",
            "swiss setup apply",
            "swiss setup guide",
            "swiss secret set",
            "swiss secret ask",
            "swiss connect",
        ] {
            assert!(guide.contains(command), "{command}");
        }
        for code in ["exit code 0", "exit code 1", "exit code 2", "exit code 3", "exit code 4"] {
            assert!(guide.contains(code), "{code}");
        }
    }

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

    // SPEC §host.setup: "`-` reads stdin", which may carry what Windows PowerShell 5 writes.
    #[test]
    fn a_manifest_reads_from_a_file_or_stdin_in_any_encoding() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("{}".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let input = read_manifest("-", false, &mut &utf16[..]).expect("UTF-16 from stdin");
        assert_eq!(input.text, "{}");
        assert_eq!(input.base, PathBuf::new());

        let bom = b"\xEF\xBB\xBF{}";
        assert_eq!(read_manifest("-", false, &mut &bom[..]).expect("a BOM").text, "{}");

        let err = read_manifest("-", true, &mut Untouchable).err().expect("a terminal is refused");
        assert!(err.contains("terminal"), "{err}");
        let err = read_manifest("-", false, &mut &[0x41, 0xFF, 0x42][..]).err().expect("not text");
        assert!(err.contains("not text"), "{err}");

        let dir = std::env::temp_dir().join(format!("swiss-setup-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("m.json");
        std::fs::write(&path, "{}").expect("a scratch manifest");
        let file = path.to_string_lossy().into_owned();
        let input = read_manifest(&file, true, &mut Untouchable).expect("a file needs no stdin");
        assert_eq!(input.base, dir);
    }

    #[test]
    fn a_manifest_command_takes_one_file_and_refuses_what_it_does_not_know() {
        let plain = ManifestArgs { file: "m.json".into(), options: Options::default() };
        assert_eq!(parse_manifest_args(Mode::Plan, &argv(&["m.json"])), Ok(plain));
        let rotating = ManifestArgs {
            file: "-".into(),
            options: Options { rotate_secrets: true, ..Options::default() },
        };
        assert_eq!(parse_manifest_args(Mode::Plan, &argv(&["--rotate-secrets", "-"])), Ok(rotating));
        let unchecked = ManifestArgs {
            file: "m.json".into(),
            options: Options { no_check: true, ..Options::default() },
        };
        assert_eq!(parse_manifest_args(Mode::Apply, &argv(&["--no-check", "m.json"])), Ok(unchecked));
        // A plan checks nothing, so a flag saying not to has no business there.
        assert!(parse_manifest_args(Mode::Plan, &argv(&["--no-check", "m.json"])).is_err());
        for words in [vec![], vec!["a", "b"], vec!["a", "--force"], vec!["-", "-"], vec!["a", "-"]] {
            assert!(parse_manifest_args(Mode::Apply, &argv(&words)).is_err(), "{words:?}");
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
