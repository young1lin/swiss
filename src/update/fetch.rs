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

//! Downloads for `swiss update`. The manifest and the assets come from GitHub, or from
//! `SWISS_UPDATE_SOURCE` when that is a loopback URL. Redirects stay on the allowlist.

use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

const MANIFEST_MAX: u64 = 1024 * 1024;
const ASSET_MAX: u64 = 256 * 1024 * 1024;
const SOURCE_ENV: &str = "SWISS_UPDATE_SOURCE";
const SOURCE_REFUSAL: &str = "SWISS_UPDATE_SOURCE must be a loopback URL";

#[derive(Clone, Debug)]
pub enum Source {
    Github,
    Loopback(String),
}

pub fn source() -> Result<Source, String> {
    match std::env::var(SOURCE_ENV) {
        Err(_) => Ok(Source::Github),
        Ok(raw) if raw.is_empty() => Ok(Source::Github),
        Ok(raw) => loopback_base(&raw).map(Source::Loopback),
    }
}

/// `host_str` keeps an IPv6 address in its brackets, so `[::1]` is the spelling to match.
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// The base URL a loopback `SWISS_UPDATE_SOURCE` names, with a trailing slash. Anything that is
/// not plain http to a loopback host with an explicit port is refused.
pub(super) fn loopback_base(raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| SOURCE_REFUSAL.to_string())?;
    if url.scheme() != "http" || url.port().is_none() {
        return Err(SOURCE_REFUSAL.to_string());
    }
    if !is_loopback_host(url.host_str().unwrap_or("")) {
        return Err(SOURCE_REFUSAL.to_string());
    }
    let mut base = url.clone();
    base.set_query(None);
    base.set_fragment(None);
    let mut text = base.to_string();
    if !text.ends_with('/') {
        text.push('/');
    }
    Ok(text)
}

pub fn manifest_url(source: &Source) -> String {
    match source {
        Source::Github => "https://github.com/young1lin/swiss/releases/latest/download/update.json".to_string(),
        Source::Loopback(base) => format!("{base}update.json"),
    }
}

pub fn asset_url(source: &Source, tag: &str, asset: &str) -> String {
    match source {
        Source::Github => format!("https://github.com/young1lin/swiss/releases/download/{tag}/{asset}"),
        Source::Loopback(base) => format!("{base}{asset}"),
    }
}

fn host_allowed(url: &reqwest::Url, source: &Source) -> bool {
    let host = url.host_str().unwrap_or("");
    match source {
        Source::Github => {
            url.scheme() == "https" && (host == "github.com" || host.ends_with(".githubusercontent.com"))
        }
        Source::Loopback(_) => url.scheme() == "http" && is_loopback_host(host),
    }
}

pub fn http_client(source: &Source) -> Result<reqwest::Client, String> {
    let source = source.clone();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 5 || !host_allowed(attempt.url(), &source) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10 * 60))
        .redirect(policy)
        .user_agent(format!("swiss/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| format!("cannot build an http client: {err}"))
}

pub async fn get_bytes(client: &reqwest::Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().await.map_err(|err| format!("cannot reach {url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered HTTP {}", response.status()));
    }
    let mut out = Vec::new();
    let mut response = response;
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
            .await
            .map_err(|_| format!("timed out reading {url}"))?
            .map_err(|err| format!("cannot read {url}: {err}"))?;
        let Some(chunk) = chunk else {
            break;
        };
        if out.len() as u64 + chunk.len() as u64 > limit {
            return Err(format!("{url} is over the download limit"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

pub async fn get_to_file(client: &reqwest::Client, url: &str, dest: &Path, limit: u64, expected_sha: &str) -> Result<u64, String> {
    let response = client.get(url).send().await.map_err(|err| format!("cannot reach {url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered HTTP {}", response.status()));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let mut file = std::fs::File::create(dest).map_err(|err| format!("cannot write {}: {err}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut response = response;
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
            .await
            .map_err(|_| format!("timed out reading {url}"))?
            .map_err(|err| format!("cannot read {url}: {err}"))?;
        let Some(chunk) = chunk else {
            break;
        };
        total += chunk.len() as u64;
        if total > limit {
            let _ = std::fs::remove_file(dest);
            return Err(format!("{url} is over the download limit"));
        }
        hasher.update(&chunk);
        std::io::Write::write_all(&mut file, &chunk).map_err(|err| format!("cannot write {}: {err}", dest.display()))?;
    }
    drop(file);
    let got = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if !got.eq_ignore_ascii_case(expected_sha) {
        let _ = std::fs::remove_file(dest);
        return Err(format!("sha256 mismatch for {url}"));
    }
    Ok(total)
}

pub const MANIFEST_LIMIT: u64 = MANIFEST_MAX;
pub const ASSET_LIMIT: u64 = ASSET_MAX;
