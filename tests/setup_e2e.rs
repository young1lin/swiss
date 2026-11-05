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

    // This first plan starts the gateway. A new home may hold the MCP plugin off, so turn it on here,
    // until a manifest's `plugins` section can say so itself.
    let _ = instance.run(&["setup", "plan", &file], &env);
    let enabled = instance.run(&["api", "POST", "/api/plugins/mcp/enable", "{}"], &[]);
    assert!(enabled.status.success(), "{}", String::from_utf8_lossy(&enabled.stderr));

    let plan = instance.run(&["setup", "plan", &file], &env);
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
