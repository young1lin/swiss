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

//! The tunnels plugin as a provider of remote EXECUTION (docs/34 §9) — the whole SSH
//! side of the remote plugin, exactly as `tunnel/shell.rs` is the whole SSH side of the
//! terminal.
//!
//! The remote plugin never links an SSH client; it holds the host's
//! [RemoteTransportProvider] contract and reaches a build server through
//! [TunnelManager::lease_connection]. Identity is the tunnels connection id — the SAME
//! inventory the terminal lists, never a second one. Credentials stay in `ssh.rs`
//! exactly as they do for forwarding rules and PTYs: the provider sees an endpoint id
//! and hands back events and exit codes, nothing else.
//!
//! ## POSIX quoting, and why it lives here
//!
//! An SSH "exec" request is ultimately a command STRING the far side runs through the
//! user's login shell — but this crate's public surface is argv-shaped, and the
//! translation from argv to string happens in exactly one place (`quote_posix`,
//! `exec_command_string`), unit-tested against every nasty argument below. Callers
//! cannot smuggle in their own quoting, and a future transport (Docker, WinRM) would do
//! its own translation without inheriting POSIX rules that do not apply to it.

use std::sync::Arc;

use async_trait::async_trait;

use super::manager::TunnelManager;
use super::types::{FailureKind, TunnelError};
use swiss_host::services::action::CancelHandle;
use swiss_host::services::remote::{
    RemoteEndpoint, RemoteError, RemoteExecEvent, RemoteExecRequest, RemoteExecResult,
    RemoteFileStat, RemoteListing, RemoteRead, RemoteTransportProvider, RemoteWrite,
};

/// Quote ONE argument for a POSIX shell.
///
/// Two modes, both provably one-word round trips:
/// - a conservative SAFE word (non-empty, ASCII only, chars in the alphanumerics plus
///   `_ . : = / @ % + , -` set) passes bare, which keeps the command string readable
///   for a human on the far side — `ps` shows `cmake --build build`, and a leading
///   `-` is still just a word to the shell;
/// - everything else is wrapped in single quotes — every byte literal — with an
///   embedded quote spliced as the close-quote/escaped-quote/reopen idiom. An empty
///   argument becomes `''`. Unicode always takes this path: quoted bytes survive any
///   locale, and no locale can reinterpret them as syntax.
///
/// This is the ONLY argv->string step in the crate (docs/34 §13); do not grow a
/// second. No join(" ") shortcut exists anywhere.
pub fn quote_posix(arg: &str) -> String {
    let safe = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '.' | ':' | '=' | '/' | '@' | '%' | '+' | ',' | '-')
    };
    if !arg.is_empty() && arg.chars().all(safe) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            // Close the quote, emit an escaped quote, reopen — one word, quote intact.
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// The full command string for one exec request: `export` the env, `cd` when a cwd
/// was resolved, then `exec` the argv with each argument quoted. Built from tested
/// pieces only — no caller-side string, no join(" ") anywhere.
pub fn exec_command_string(request: &RemoteExecRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (name, value) in &request.env {
        // export NAME='value' — the value is quoted like any other argument, so an
        // env var containing $(...) or ; stays data.
        parts.push(format!(
            "export {}={};",
            quote_posix(name),
            quote_posix(value)
        ));
    }
    if let Some(cwd) = &request.cwd {
        parts.push(format!("cd {} &&", quote_posix(cwd)));
    }
    parts.push("exec".to_string());
    for arg in &request.argv {
        parts.push(quote_posix(arg));
    }
    parts.join(" ")
}

/// Map a tunnels failure onto the transport-neutral error the remote plugin sees.
/// Auth/host-key/config problems point back at the Tunnels page (the remote side has no
/// controls of its own for them); cancellation keeps its own kind so the run's outcome
/// can be an honest "canceled".
fn as_remote_error(err: TunnelError) -> RemoteError {
    match err.kind {
        FailureKind::Auth | FailureKind::HostKey => {
            RemoteError::Failed(format!("{} (fix it on the Tunnels page)", err.message))
        }
        FailureKind::Config => RemoteError::Unknown(err.message),
        FailureKind::Canceled => RemoteError::Canceled(err.message),
        _ => RemoteError::Failed(err.message),
    }
}

/// The provider the Tunnels plugin registers on start and withdraws on stop.
pub struct TunnelRemote {
    manager: Arc<TunnelManager>,
}

impl TunnelRemote {
    pub fn new(manager: Arc<TunnelManager>) -> Arc<Self> {
        Arc::new(TunnelRemote { manager })
    }
}

#[async_trait]
impl RemoteTransportProvider for TunnelRemote {
    fn list(&self) -> Vec<RemoteEndpoint> {
        // The endpoint list IS the tunnels connection inventory — ids, labels and live
        // state, no hosts, no users, no credentials (docs/34 §36).
        self.manager
            .connections()
            .into_iter()
            .map(|def| RemoteEndpoint {
                id: def.id.clone(),
                label: def.name.clone(),
                state: self.manager.conn_state(&def.id).as_str().to_string(),
            })
            .collect()
    }

    fn supports_files(&self) -> bool {
        true
    }

    async fn exec(
        &self,
        endpoint: &str,
        holder: &str,
        request: RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<RemoteExecEvent>,
        cancel: CancelHandle,
    ) -> Result<RemoteExecResult, RemoteError> {
        // The lease is minted here and moved INTO the exec future: the operation task
        // owns the reference, so it cannot be released while the command still runs.
        let (_conn, lease) = self
            .manager
            .lease_connection(endpoint)
            .await
            .map_err(as_remote_error)?;
        let _ = holder;
        let code = _conn
            .exec(request, events, cancel, lease)
            .await
            .map_err(as_remote_error)?;
        Ok(RemoteExecResult { exit_code: code })
    }

    async fn stat(
        &self,
        endpoint: &str,
        _holder: &str,
        path: &str,
    ) -> Result<Option<RemoteFileStat>, RemoteError> {
        let (conn, lease) = self
            .manager
            .lease_connection(endpoint)
            .await
            .map_err(as_remote_error)?;
        conn.stat_file(path.to_string(), lease)
            .await
            .map_err(as_remote_error)
    }

    async fn open_read(
        &self,
        endpoint: &str,
        _holder: &str,
        path: &str,
    ) -> Result<Box<dyn RemoteRead>, RemoteError> {
        let (conn, lease) = self
            .manager
            .lease_connection(endpoint)
            .await
            .map_err(as_remote_error)?;
        conn.open_read(path.to_string(), lease)
            .await
            .map_err(as_remote_error)
    }

    async fn create(
        &self,
        endpoint: &str,
        _holder: &str,
        path: &str,
    ) -> Result<Box<dyn RemoteWrite>, RemoteError> {
        let (conn, lease) = self
            .manager
            .lease_connection(endpoint)
            .await
            .map_err(as_remote_error)?;
        conn.create_file(path.to_string(), lease)
            .await
            .map_err(as_remote_error)
    }

    async fn mkdir_p(&self, endpoint: &str, _holder: &str, path: &str) -> Result<(), RemoteError> {
        let (conn, lease) = self
            .manager
            .lease_connection(endpoint)
            .await
            .map_err(as_remote_error)?;
        conn.mkdir_p(path.to_string(), lease)
            .await
            .map_err(as_remote_error)
    }

    async fn list_dir(
        &self,
        endpoint: &str,
        _holder: &str,
        path: &str,
    ) -> Result<Vec<RemoteListing>, RemoteError> {
        let (conn, lease) = self
            .manager
            .lease_connection(endpoint)
            .await
            .map_err(as_remote_error)?;
        conn.list_dir(path.to_string(), lease)
            .await
            .map_err(as_remote_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- POSIX quoting (docs/34 §13) ------------------------------------------------------
    // Every argument that can break a naive join is pinned here; the properties that
    // matter are: exactly one argv element survives as one shell word, and nothing the
    // argument contains is ever interpreted as syntax.

    fn round_trip(args: &[&str]) -> String {
        exec_command_string(&RemoteExecRequest {
            argv: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            cwd: None,
        })
    }

    #[test]
    fn plain_arguments_stay_plain() {
        assert_eq!(
            round_trip(&["cmake", "--build", "build", "-j32"]),
            "exec cmake --build build -j32"
        );
    }

    #[test]
    fn empty_argument_becomes_two_quotes() {
        assert_eq!(round_trip(&["a", "", "b"]), "exec a '' b");
    }

    #[test]
    fn spaces_stay_inside_one_word() {
        assert_eq!(round_trip(&["hello world"]), "exec 'hello world'");
    }

    #[test]
    fn single_quotes_are_spliced() {
        // it's must come back as exactly it's — one word, quotes intact.
        assert_eq!(round_trip(&["it's"]), "exec 'it'\\''s'");
    }

    #[test]
    fn double_quotes_are_plain_bytes() {
        assert_eq!(round_trip(&[r#"say "hi""#]), r#"exec 'say "hi"'"#);
    }

    #[test]
    fn shell_metacharacters_stay_data() {
        for nasty in [
            "$(rm -rf /)",
            "a;b",
            "a&b",
            "a|b",
            "`whoami`",
            "x>y",
            "PATH=$PATH",
            "~/*",
            "a\nb",
        ] {
            let quoted = quote_posix(nasty);
            // The wrapper is single quotes and the content has no unescaped quote left.
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{nasty} -> {quoted}"
            );
            // Crucially: the metacharacters are INSIDE the quotes, i.e. never at a word
            // boundary where a shell could act on them.
            assert!(quoted[1..quoted.len() - 1].contains(&nasty[..1]), "{nasty}");
        }
    }

    #[test]
    fn newline_and_unicode_survive_as_bytes() {
        let arg = "line1\nline2 中文 🎉";
        let quoted = quote_posix(arg);
        assert_eq!(quoted, format!("'{arg}'"));
    }

    #[test]
    fn arguments_beginning_with_a_dash_are_not_flags() {
        // --foo and -j32 arrive as argv elements; the quoting keeps them words.
        let cmd = round_trip(&["--help", "-j", "32"]);
        assert_eq!(cmd, "exec --help -j 32");
    }

    #[test]
    fn env_and_cwd_are_quoted_too() {
        let request = RemoteExecRequest {
            argv: vec!["make".into()],
            env: vec![("BOARD".into(), "foo; rm -rf /".into())],
            cwd: Some("/data/ws/a b".into()),
        };
        let cmd = exec_command_string(&request);
        assert_eq!(
            cmd,
            "export BOARD='foo; rm -rf /'; cd '/data/ws/a b' && exec make"
        );
    }

    #[test]
    fn env_names_must_look_like_identifiers() {
        // Guard the construction side too: a name that is not an identifier could not
        // be exported safely even quoted, and the remote action's validator refuses it
        // long before here — assert what this layer would build if asked.
        let request = RemoteExecRequest {
            argv: vec!["true".into()],
            env: vec![("A=1;evil".into(), "v".into())],
            cwd: None,
        };
        let cmd = exec_command_string(&request);
        // Even a hostile name stays a quoted word: no syntax escapes. (A safe value
        // like "v" passing bare is fine — the name was already refused upstream.)
        assert_eq!(cmd, "export 'A=1;evil'=v; exec true");
    }
}
