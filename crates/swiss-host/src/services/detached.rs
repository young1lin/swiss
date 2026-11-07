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

//! The only way a program is started to outlive the gateway (SPEC §process.detached).
//!
//! Runs, MCPs and terminals are children that die with the gateway on purpose (ADR-008);
//! a managed app is not. Letting one outlive its parent falls into traps that are each
//! avoided by a rule of that section; this module is where the rules live in code:
//!
//! - no job assignment, no kill_on_drop, no ledger entry, and the spawn carries
//!   CREATE_BREAKAWAY_FROM_JOB (the `detach_command` of `swiss_core::platform`), with the
//!   documented retry without it when the job forbids the escape (os error 5);
//! - stdout and stderr go to the capture file, opened append-only; stdin is null. Never a
//!   pipe: nobody would read one once the start is over, and a full pipe is a hung app;
//! - the PATH is rebuilt per spawn from the registry plus the login extras, because a
//!   resident gateway's environment is a snapshot taken days ago;
//! - the program resolves against that PATH with PATHEXT, never as an extensionless
//!   spelling (os error 193 on Windows' batch shims).

use std::fs::OpenOptions;
use std::path::PathBuf;
#[cfg(any(windows, test))]
use std::path::Path;

use swiss_core::platform::{self, process_created_at};

use crate::pathenv::resolve_command;
#[cfg(any(windows, test))]
use crate::pathenv::same_dir;
#[cfg(windows)]
use crate::pathenv::extra_bin_dirs;
#[cfg(not(windows))]
use crate::pathenv::login_path_from_env;

/// One app start, everything already resolved by the caller (references, home, config):
/// the program as configured, its argv, the working directory, the env overlay ON TOP of
/// the gateway's scrubbed environment, and where the app's output is captured.
pub struct DetachedSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub capture: PathBuf,
}

/// A detached child in flight. The pid is the identity the caller tracks; the creation
/// time pins it against reuse; the child handle is a `tokio::process::Child` WITHOUT
/// `kill_on_drop` — dropping it must not kill anyone — owned by the caller's exit watcher
/// (§process.detached), which is also what reaps it, so a held app never becomes a zombie.
pub struct DetachedChild {
    pub pid: u32,
    pub created_at: Option<u64>,
    pub child: tokio::process::Child,
    /// True when the spawn succeeded only WITHOUT the breakaway flag — the app then shares
    /// the gateway's job and dies with it. The start's message must say so.
    pub shares_job: bool,
}

/// Why a start failed. Each sentence is operator-facing (SPEC §process.apps): it names the
/// program or the capture file, never a value.
#[derive(Debug)]
pub enum SpawnError {
    Capture(String),
    Spawn(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Capture(sentence) | SpawnError::Spawn(sentence) => f.write_str(sentence),
        }
    }
}

/// The PATH a detached spawn resolves its program against (SPEC §process.detached, the
/// stale-PATH row): the user's registry Path, then the machine's, both expanded, then the
/// login extras of §host.daemon, deduplicated keeping the first spelling. When the
/// registry cannot be read at all, the gateway's own PATH stands in.
pub fn detached_path() -> String {
    #[cfg(windows)]
    {
        let mut user: Vec<String> = Vec::new();
        let mut machine: Vec<String> = Vec::new();
        if let Ok(Some((raw, _expand))) = platform::user_path_read() {
            user.extend(split_path(&platform::expand_env(&raw)));
        }
        if let Ok(Some((raw, _expand))) = platform::machine_path_read() {
            machine.extend(split_path(&platform::expand_env(&raw)));
        }
        let own = std::env::var("PATH").unwrap_or_default();
        compose_detached_path(&user, &machine, &extra_bin_dirs(), &own)
    }
    #[cfg(not(windows))]
    {
        // No registry off Windows: the login repair over the gateway's own PATH is the
        // whole rebuild (§host.daemon).
        login_path_from_env()
    }
}

#[cfg(any(windows, test))]
fn separator() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

#[cfg(any(windows, test))]
fn split_path(path: &str) -> Vec<String> {
    path.split(separator())
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// The pure core of the PATH rebuild, so the order and the dedup are testable without a
/// registry. Empty sources fall back to the gateway's own PATH, the row's last resort.
#[cfg(any(windows, test))]
pub(crate) fn compose_detached_path(
    user: &[String],
    machine: &[String],
    extras: &[PathBuf],
    own_path: &str,
) -> String {
    let extra_strings: Vec<String> = extras
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for entry in user.iter().chain(machine.iter()).chain(extra_strings.iter()) {
        let path = Path::new(entry);
        if seen.iter().any(|s| same_dir(s, path)) {
            continue;
        }
        seen.push(path.to_path_buf());
        kept.push(entry.clone());
    }
    if kept.is_empty() {
        return own_path.to_string();
    }
    kept.join(&separator().to_string())
}

/// Build the spawn for one attempt. The capture file is cloned per stdio end so the child
/// owns both; the gateway's own handles to it are dropped right after the spawn, so an app
/// never holds so much as its capture file open from the gateway's side.
fn build_command(
    spec: &DetachedSpec,
    path: &str,
    program: &str,
    capture: &std::fs::File,
    with_breakaway: bool,
) -> Result<tokio::process::Command, SpawnError> {
    let stdout = capture.try_clone().map_err(|err| {
        SpawnError::Capture(format!(
            "could not reuse {} to capture output: {err}",
            spec.capture.display()
        ))
    })?;
    let stderr = capture.try_clone().map_err(|err| {
        SpawnError::Capture(format!(
            "could not reuse {} to capture output: {err}",
            spec.capture.display()
        ))
    })?;
    let mut command = std::process::Command::new(program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command.env("PATH", path);
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::from(stdout));
    command.stderr(std::process::Stdio::from(stderr));
    if with_breakaway {
        platform::detach_command(&mut command);
    } else {
        #[cfg(windows)]
        platform::detach_command_without_breakaway(&mut command);
        #[cfg(not(windows))]
        platform::detach_command(&mut command);
    }
    Ok(tokio::process::Command::from(command))
}

/// Start one program detached (SPEC §process.detached). On Windows, a job that forbids
/// breakaway makes the first attempt fail with os error 5; the retry without the flag is
/// the documented compromise, reported through `DetachedChild::shares_job`.
pub async fn spawn_detached(spec: &DetachedSpec) -> Result<DetachedChild, SpawnError> {
    let path = detached_path();
    let capture = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.capture)
        .map_err(|err| {
            SpawnError::Capture(format!(
                "could not open {} to capture output: {err}",
                spec.capture.display()
            ))
        })?;
    // Resolve against the rebuilt PATH (PATHEXT included); an unresolvable name is still
    // handed to the OS so ITS error names the command — the spawn ENOENT the panel shows.
    let program = resolve_command(&spec.program, &path)
        .map(|resolved| resolved.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.program.clone());

    let first = build_command(spec, &path, &program, &capture, true)?.spawn();
    let (child, shares_job) = match first {
        Ok(child) => (child, false),
        Err(err) => {
            let retryable = cfg!(windows) && err.raw_os_error() == Some(5);
            if !retryable {
                return Err(SpawnError::Spawn(format!(
                    "could not start {}: {err}",
                    spec.program
                )));
            }
            // The job forbids the escape (SPEC §process.detached): run inside it and say so.
            let retried =
                build_command(spec, &path, &program, &capture, false)?.spawn().map_err(|err| {
                    SpawnError::Spawn(format!("could not start {}: {err}", spec.program))
                })?;
            (retried, true)
        }
    };
    // tokio's id() is Option (the child can be reaped between spawn and read); 0 is the
    // registry's own "no pid to speak of", and a row nobody can act on is not a row.
    let pid = child.id().unwrap_or(0);
    Ok(DetachedChild {
        pid,
        created_at: process_created_at(pid),
        child,
        shares_job,
    })
}

#[cfg(test)]
mod tests {
    //! The pure half of the module (the spawn half is exercised end to end by swiss-it's
    //! detached group, where the job, the console and the capture file are real).
    use super::*;

    #[test]
    fn the_spawn_path_puts_the_registry_path_first_and_dedups() {
        let sep = separator().to_string();
        // The fixture speaks the platform's own spelling, because the separator is
        // the platform's too: on POSIX a drive letter's colon would split mid-entry.
        // Both spellings give the machine half the exact dup of the user's second
        // entry, which folds on every platform, plus a case-variant dup besides,
        // which folds only where same_dir lowercases - Windows alone. Raw strings
        // on the Windows side: this test is about the bytes, not escape discipline.
        let (raw_user, raw_machine, extra, fallback, expected) = if cfg!(windows) {
            (
                format!(r"C:\usr\bin{sep}C:\usr\tools"),
                format!(r"C:\win\system32{sep}C:\usr\tools{sep}c:\USR\BIN"),
                r"C:\Users\dev\.cargo\bin",
                r"C:\fallback",
                format!(r"C:\usr\bin{sep}C:\usr\tools{sep}C:\win\system32{sep}C:\Users\dev\.cargo\bin"),
            )
        } else {
            (
                format!("/usr/local/bin{sep}/usr/bin"),
                format!("/bin{sep}/usr/bin{sep}/USR/BIN"),
                "/home/dev/.cargo/bin",
                "/fallback",
                format!("/usr/local/bin{sep}/usr/bin{sep}/bin{sep}/USR/BIN{sep}/home/dev/.cargo/bin"),
            )
        };
        let extras = vec![PathBuf::from(extra)];
        // The registry's user half leads, the machine half follows, the login
        // extras trail; the exact dup has folded, the case-variant only on Windows.
        let out = compose_detached_path(
            &split_path(&raw_user),
            &split_path(&raw_machine),
            &extras,
            fallback,
        );
        assert_eq!(out, expected);

        // An unreadable registry (nothing from either source) falls back to the gateway's
        // own PATH — the row's last resort, not a silent empty PATH.
        assert_eq!(
            compose_detached_path(&[], &[], &[], r"C:\gateway\own"),
            r"C:\gateway\own"
        );
    }
}
