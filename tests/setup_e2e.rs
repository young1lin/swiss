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

//! `swiss setup`, as an Agent runs it: the real binary against a gateway it starts itself, on a
//! private home and a private port, so what is measured is every byte that leaves (SPEC §host.setup).

use std::path::{Path, PathBuf};
use std::process::Output;

/// One private instance. Dropping it stops the gateway the binary left running.
struct Instance {
    home: PathBuf,
    port: u16,
}

impl Instance {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("swiss-setup-e2e-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(home.join("user")).expect("a scratch home");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
        let port = listener.local_addr().expect("a bound address").port();
        drop(listener);
        Instance { home, port }
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_swiss"));
        command
            .args(args)
            .env("SWISS_HOME", &self.home)
            .env("SWISS_PORT", self.port.to_string())
            .env("SWISS_MASTER_KEY", "ab".repeat(32))
            .env("USERPROFILE", self.home.join("user"))
            .env("HOME", self.home.join("user"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .current_dir(&self.home)
            .stdin(std::process::Stdio::null());
        command
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = self.command(args);
        command.envs(env.iter().copied());
        command.output().expect("run swiss")
    }

    /// Every file the gateway wrote under its home that a person would read as a log.
    fn logs(&self) -> String {
        let mut text = String::new();
        collect_logs(&self.home, &mut text);
        text
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.command(&["stop"]).output();
    }
}

fn collect_logs(dir: &Path, text: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_logs(&path, text);
        } else if path.extension().is_some_and(|extension| extension == "log") {
            text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()));
        }
    }
}

// SPEC §host.setup: "It never holds a credential, a token, a one-time link or a secret's value".
// One value from each source, each used by a definition that fails its check, because a failure is
// where a value would most easily be repeated.
#[test]
fn no_secret_value_ever_reaches_the_output() {
    const FROM_ENV: &str = "sentinel-env-5d0c1e77";
    const FROM_FILE: &str = "sentinel-file-a94be210";
    const FROM_DOTENV: &str = "sentinel-dotenv-73f6c8d1";

    let instance = Instance::new();
    std::fs::write(instance.home.join("value.txt"), format!("{FROM_FILE}\n")).expect("the file source");
    std::fs::write(instance.home.join("vars.env"), format!("E2E_KEY={FROM_DOTENV}\n")).expect("the dotenv source");
    let manifest = serde_json::json!({
        "version": 1,
        "secrets": {
            "from-env": "env:E2E_SECRET_VALUE",
            "from-file": "file:value.txt",
            "from-dotenv": "dotenv:vars.env#E2E_KEY",
        },
        "mcps": {
            "web": {
                "type": "http",
                "url": "http://127.0.0.1:1/mcp",
                "headers": { "Authorization": "Bearer ${secret://from-env}" },
            },
            "tool": {
                "command": "nosuchprogram",
                "args": ["--flag", "${secret://from-file}"],
                "env": { "API_KEY": "${secret://from-dotenv}" },
            },
            "db": {
                "type": "mysql",
                "host": "127.0.0.1",
                "port": 1,
                "user": "u",
                "password": "${secret://from-file}",
            },
        },
    });
    let path = instance.home.join("swiss.setup.json");
    std::fs::write(&path, manifest.to_string()).expect("the manifest");
    let file = path.to_string_lossy().into_owned();
    let env = [("E2E_SECRET_VALUE", FROM_ENV)];

    // This plan starts the gateway, and waits out the boot behind its open port.
    let plan = instance.run(&["setup", "plan", &file], &env);
    let planned: serde_json::Value = serde_json::from_slice(&plan.stdout).expect("one JSON document on stdout");
    assert_eq!(plan.status.code(), Some(0), "{planned}");
    assert_eq!(planned["gateway"]["started"], serde_json::json!(true), "{planned}");

    let apply = instance.run(&["setup", "apply", &file], &env);
    let again = instance.run(&["setup", "apply", &file], &env);

    let document: serde_json::Value = serde_json::from_slice(&apply.stdout).expect("one JSON document on stdout");
    assert_eq!(apply.status.code(), Some(1), "{document}");
    assert_eq!(document["gateway"]["port"], serde_json::json!(instance.port), "{document}");
    assert_eq!(document["summary"]["created"], serde_json::json!(6), "{document}");
    assert_eq!(document["summary"]["failed"], serde_json::json!(0), "{document}");
    assert_eq!(again.status.code(), Some(1));

    let logs = instance.logs();
    for output in [&plan, &apply, &again] {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for value in [FROM_ENV, FROM_FILE, FROM_DOTENV] {
            assert!(!stdout.contains(value), "{value} on stdout: {stdout}");
            assert!(!stderr.contains(value), "{value} on stderr: {stderr}");
        }
    }
    for value in [FROM_ENV, FROM_FILE, FROM_DOTENV] {
        assert!(!logs.contains(value), "{value} in the gateway's logs");
    }
}

// SPEC §host.setup: a home that has never run a gateway converges on its first apply, with no call to
// anything but the command itself.
#[test]
fn a_new_home_converges_on_its_first_apply() {
    let instance = Instance::new();
    let manifest = serde_json::json!({
        "machine": { "skills": ["swiss"] },
        "plugins": {
            "jobs": { "config": { "definitions": { "e2e-job": {
                "trigger": { "kind": "manual" },
                "action": { "type": "process.exec", "input": { "program": "nosuchprogram" } },
            } } } },
            "llm": { "config": { "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:1/v1" } } } },
        },
        "mcps": { "e2e-echo": { "type": "echo" } },
    });
    let path = instance.home.join("swiss.setup.json");
    std::fs::write(&path, manifest.to_string()).expect("the manifest");
    let file = path.to_string_lossy().into_owned();

    let first = instance.run(&["setup", "apply", &file, "--no-check"], &[]);
    let document: serde_json::Value = serde_json::from_slice(&first.stdout).expect("one JSON document on stdout");
    assert_eq!(first.status.code(), Some(0), "{document}");
    assert_eq!(document["gateway"]["started"], serde_json::json!(true), "{document}");
    assert_eq!(document["summary"]["created"], serde_json::json!(2), "{document}");
    assert_eq!(document["summary"]["updated"], serde_json::json!(2), "{document}");

    let again = instance.run(&["setup", "apply", &file, "--no-check"], &[]);
    let document: serde_json::Value = serde_json::from_slice(&again.stdout).expect("one JSON document on stdout");
    assert_eq!(again.status.code(), Some(0), "{document}");
    assert_eq!(document["summary"]["unchanged"], serde_json::json!(4), "{document}");
}

/// The answer to one `swiss api` call, success or not, as text.
fn api_text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

// SPEC §host.config: "A generic guarantee": every plugin the real server composes refuses a key it does
// not read, at the top of its row and one level down in each closed object its schema names. A key saved
// and ignored is the failure a manifest cannot otherwise see.
#[test]
fn every_plugin_with_a_schema_refuses_an_unknown_key() {
    let instance = Instance::new();
    let path = instance.home.join("empty.json");
    std::fs::write(&path, "{}").expect("the manifest");
    let started = instance.run(&["setup", "plan", &path.to_string_lossy()], &[]);
    assert_eq!(started.status.code(), Some(0), "{}", api_text(&started));

    let listed = instance.run(&["api", "GET", "/api/plugins"], &[]);
    let inventory: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("the inventory");
    let ids: Vec<String> = inventory["plugins"]
        .as_array()
        .expect("plugins")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_string))
        .collect();
    assert!(ids.len() >= 5, "{ids:?}");

    let refuses = |id: &str, config: serde_json::Value, place: &str| {
        let body = serde_json::json!({ "config": config }).to_string();
        let answer = instance.run(&["api", "PUT", &format!("/api/plugins/{id}/config?dryRun=1"), &body], &[]);
        let text = api_text(&answer);
        assert!(!answer.status.success(), "{id} took an unknown key {place}: {text}");
        assert!(text.contains("zz-unknown"), "{id} did not name the key {place}: {text}");
    };
    let mut nested = 0;
    for id in &ids {
        refuses(id, serde_json::json!({ "zz-unknown": 1 }), "at the top");

        let held = instance.run(&["api", "GET", &format!("/api/plugins/{id}/config")], &[]);
        let held: serde_json::Value = serde_json::from_slice(&held.stdout).expect("the config answer");
        for (key, property) in held["schema"]["properties"].as_object().into_iter().flatten() {
            let closed = property["type"] == serde_json::json!("object")
                && property["properties"].is_object()
                && property.get("additionalProperties").is_none_or(|more| *more == serde_json::json!(false));
            if closed {
                refuses(id, serde_json::json!({ key.clone(): { "zz-unknown": 1 } }), &format!("under {key}"));
                nested += 1;
            }
        }
    }
    assert!(nested >= 3, "only {nested} closed objects were looked into");
}

fn bearer_of(text: &str) -> String {
    text.split("Bearer ")
        .nth(1)
        .map(|rest| rest.chars().take_while(|ch| ch.is_ascii_hexdigit()).collect())
        .unwrap_or_default()
}

fn plant_claude(dir: &Path) {
    std::fs::create_dir_all(dir).expect("the fake's directory");
    #[cfg(windows)]
    std::fs::write(
        dir.join("claude.cmd"),
        "@echo off\r\necho %*>>\"%~dp0calls.txt\"\r\nif \"%2\"==\"list\" (\r\n  if not \"%3\"==\"\" exit /b 1\r\n  if exist \"%~dp0names.txt\" type \"%~dp0names.txt\"\r\n  exit /b 0\r\n)\r\nif \"%2\"==\"add\" (\r\n  echo %7>>\"%~dp0names.txt\"\r\n  exit /b 0\r\n)\r\nif \"%2\"==\"remove\" exit /b 0\r\nexit /b 1\r\n",
    )
    .expect("the fake claude");
    #[cfg(not(windows))]
    {
        let path = dir.join("claude");
        std::fs::write(
            &path,
            "#!/bin/sh\ndir=$(CDPATH= cd -- \"$(dirname \"$0\")\" && pwd)\nprintf '%s\\n' \"$*\" >> \"$dir/calls.txt\"\nif [ \"$2\" = \"list\" ]; then\n  [ \"$#\" -eq 2 ] || exit 1\n  [ -f \"$dir/names.txt\" ] && cat \"$dir/names.txt\"\n  exit 0\nfi\nif [ \"$2\" = \"add\" ]; then\n  printf '%s\\n' \"$7\" >> \"$dir/names.txt\"\n  exit 0\nfi\nexit 0\n",
        )
        .expect("the fake claude");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("runnable");
    }
}

// SPEC §host.setup: neither a client token nor a one-time link reaches the output.
#[test]
fn no_token_or_link_reaches_the_output() {
    let instance = Instance::new();
    let bin = instance.home.join("bin");
    plant_claude(&bin);
    let manifest = serde_json::json!({
        "mcps": { "e2e-echo": { "type": "echo" } },
        "clients": { "claude": {}, "codex": {}, "cursor": {} },
    });
    let path = instance.home.join("swiss.setup.json");
    std::fs::write(&path, manifest.to_string()).expect("the manifest");
    #[cfg(windows)]
    let path_var = format!("{};{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    #[cfg(not(windows))]
    let path_var = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let file = path.to_string_lossy().into_owned();
    let apply = instance.run(&["setup", "apply", &file, "--no-check"], &[("PATH", path_var.as_str())]);
    let stdout = String::from_utf8_lossy(&apply.stdout);
    let stderr = String::from_utf8_lossy(&apply.stderr);
    assert!(apply.status.success(), "{stdout}{stderr}");
    let user = instance.home.join("user");
    let cursor = std::fs::read_to_string(user.join(".cursor/mcp.json")).expect("cursor mcp.json");
    let codex = std::fs::read_to_string(user.join(".codex/config.toml")).expect("codex config");
    let calls = std::fs::read_to_string(bin.join("calls.txt")).expect("claude's argv");
    for secret in [bearer_of(&cursor), bearer_of(&codex), bearer_of(&calls)] {
        assert!(secret.len() >= 16, "a client was given no token");
        assert!(!stdout.contains(&secret), "the token reached stdout");
        assert!(!stderr.contains(&secret), "the token reached stderr");
        assert!(!instance.logs().contains(&secret), "the token reached the gateway log");
    }
    assert!(!stdout.contains("token="), "a login link reached stdout");
    let sink = instance.home.join("opened.txt");
    let sink_arg = sink.to_string_lossy().into_owned();
    let asked = instance.run(&["secret", "ask", "e2e-ask-name"], &[("SWISS_OPEN_SINK", sink_arg.as_str())]);
    let asked_out = String::from_utf8_lossy(&asked.stdout);
    let asked_err = String::from_utf8_lossy(&asked.stderr);
    assert_eq!(asked.status.code(), Some(4), "{asked_out}{asked_err}");
    assert!(!asked_out.contains("token="), "a login link reached stdout");
    assert!(!asked_err.contains("token="), "a login link reached stderr");
    let opened = std::fs::read_to_string(&sink).expect("the opener's sink");
    assert!(opened.contains("#secrets?ask=e2e-ask-name"), "the page was not the asked names");
    assert!(opened.contains("token="), "the browser was not given a sign-in link");
}
