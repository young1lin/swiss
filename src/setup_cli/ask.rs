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

//! `swiss secret ask` (SPEC §host.setup). The one-time link is what the browser opens. It is
//! never printed, logged, or put in the document.

use std::future::Future;

use serde_json::{json, Value};

use super::report::{EXIT_FAILED, EXIT_NEEDS_HUMAN};
use crate::remote_cli::Gateway;
use swiss_core::secure::secretstore::valid_name;

const USAGE: &str = "usage: swiss secret ask <name>... [--wait <seconds>]\n\
    Opens the panel on those names. The sign-in link is not printed. \
    --wait polls until each name is stored, or the seconds run out.";

const BY_HAND: &str = "Forward this machine's port over SSH and open the panel, \
    or pipe each value into swiss secret set <name> from your own shell.";

pub struct AskArgs {
    pub names: Vec<String>,
    pub wait: Option<u64>,
}

/// Names, and an optional wait. A word that does not belong is not quoted: it may be a value.
pub fn parse(argv: &[String]) -> Result<AskArgs, String> {
    let mut names = Vec::new();
    let mut wait = None;
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        if word == "--help" || word == "-h" {
            return Err(USAGE.into());
        }
        if word == "--wait" {
            let Some(value) = words.next() else {
                return Err(format!("--wait needs a number of seconds\n{USAGE}"));
            };
            let Ok(seconds) = value.parse::<u64>() else {
                return Err(format!("--wait needs a number of seconds\n{USAGE}"));
            };
            if wait.is_some() {
                return Err(format!("--wait is once\n{USAGE}"));
            }
            wait = Some(seconds);
            continue;
        }
        if word.starts_with('-') {
            return Err(format!("unknown option\n{USAGE}"));
        }
        if !valid_name(word) {
            return Err(format!(
                "a secret's name is a lowercase letter, then lowercase letters, digits or -, 64 at most\n{USAGE}"
            ));
        }
        if !names.iter().any(|have| have == word) {
            names.push(word.clone());
        }
    }
    if names.is_empty() {
        return Err(USAGE.into());
    }
    Ok(AskArgs { names, wait })
}

/// Ask for `names` on the gateway at `port`. `mint` returns the panel's sign-in URL and `open`
/// shows it. Neither result is kept in the document.
pub async fn ask_on<Mint, MintFut, Open>(
    port: u16,
    started: bool,
    names: &[String],
    wait: Option<u64>,
    mint: Mint,
    open: Open,
) -> (Value, i32)
where
    Mint: FnOnce() -> MintFut,
    MintFut: Future<Output = Result<String, String>>,
    Open: FnOnce(&str) -> Result<(), String>,
{
    let gw = Gateway::at(port);
    let have = match secret_names(&gw).await {
        Ok(names) => names,
        Err(error) => return (json!({ "ok": false, "mode": "ask", "error": error }), EXIT_FAILED),
    };
    let missing: Vec<&str> = names.iter().map(String::as_str).filter(|name| !have.iter().any(|got| got == name)).collect();
    let gateway = json!({ "port": port, "started": started });
    if missing.is_empty() {
        return (
            json!({
                "ok": true,
                "mode": "ask",
                "gateway": gateway,
                "items": items(names, &[]),
                "warnings": [],
                "next": [],
            }),
            0,
        );
    }
    let mut warnings = Vec::new();
    match mint().await {
        Ok(base) => {
            let url = format!("{base}#secrets?ask={}", missing.join(","));
            if let Err(error) = open(&url) {
                warnings.push(by_hand("ask.no-desktop", &error));
            }
        }
        Err(error) => warnings.push(by_hand("ask.no-desktop", &error)),
    }
    let mut still: Vec<String> = missing.iter().map(|name| (*name).to_string()).collect();
    if let Some(seconds) = wait.filter(|seconds| *seconds > 0) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline && !still.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if let Ok(now) = secret_names(&gw).await {
                still.retain(|name| !now.iter().any(|got| got == name));
            }
        }
    }
    let waiting: Vec<&str> = still.iter().map(String::as_str).collect();
    let pending = !waiting.is_empty();
    let mut next = Vec::new();
    if pending {
        next.push(json!({
            "who": "human",
            "do": format!("swiss secret ask {}", names.join(" ")),
            "why": "no source for this secret",
        }));
    }
    (
        json!({
            "ok": !pending,
            "mode": "ask",
            "gateway": gateway,
            "items": items(names, &waiting),
            "warnings": warnings,
            "next": next,
        }),
        if pending { EXIT_NEEDS_HUMAN } else { 0 },
    )
}

fn by_hand(code: &str, error: &str) -> Value {
    json!({
        "code": code,
        "message": format!("{error}. {BY_HAND}"),
    })
}

fn items(names: &[String], waiting: &[&str]) -> Vec<Value> {
    names
        .iter()
        .map(|name| {
            let status = if waiting.contains(&name.as_str()) { "needs-input" } else { "unchanged" };
            json!({ "kind": "secret", "name": name, "status": status })
        })
        .collect()
}

async fn secret_names(gw: &Gateway) -> Result<Vec<String>, String> {
    let listed = gw.get("/api/secrets").await?;
    Ok(listed["secrets"].as_array().into_iter().flatten().filter_map(|name| name.as_str().map(str::to_string)).collect())
}
