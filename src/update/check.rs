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

//! The release check `swiss update` prints. Applying the release is [`super::apply`], and
//! only a short-lived CLI process does that.

use async_trait::async_trait;

pub const REPO: &str = "young1lin/swiss";

pub struct Release {
    pub tag: String,
    pub url: String,
}

pub struct UpdateInfo {
    pub current: String,
    pub latest: Option<Release>,
    pub newer: bool,
    /// The download `update.json` selected, when it named one.
    pub plan: Option<super::manifest::DownloadPlan>,
}

#[async_trait]
pub trait UpdateCheck: Send + Sync {
    async fn check(&self) -> Result<UpdateInfo, String>;
}

pub struct GithubUpdateCheck;

#[async_trait]
impl UpdateCheck for GithubUpdateCheck {
    async fn check(&self) -> Result<UpdateInfo, String> {
        check().await
    }
}

pub fn current_version() -> String {
    let hash = option_env!("SWISS_GIT_HASH").unwrap_or("unknown");
    format!("{} ({hash})", env!("CARGO_PKG_VERSION"))
}

pub fn channel() -> &'static str {
    match option_env!("SWISS_CHANNEL") {
        Some(value) if !value.is_empty() => value,
        _ => "dev",
    }
}

/// GitHub's latest release, plus the download its `update.json` selects. A loopback
/// `SWISS_UPDATE_SOURCE` replaces both: the local manifest names the release, so a walk
/// against a served directory never asks GitHub.
pub async fn check() -> Result<UpdateInfo, String> {
    let source = super::fetch::source()?;
    if let super::fetch::Source::Loopback(_) = source {
        let body = manifest_bytes(&source).await?;
        let head = super::manifest::head(&body)?;
        let mut info = info_for(head.tag, head.notes.unwrap_or_else(|| super::fetch::manifest_url(&source)));
        info.plan = describe_plan(&body).await?;
        return Ok(info);
    }
    let mut info = check_release().await?;
    if info.latest.is_some() {
        if let Ok(body) = manifest_bytes(&source).await {
            if let Ok(plan) = describe_plan(&body).await {
                info.plan = plan;
            }
        }
    }
    Ok(info)
}

fn info_for(tag: String, url: String) -> UpdateInfo {
    let newer = is_newer(&tag, env!("CARGO_PKG_VERSION"));
    UpdateInfo {
        current: current_version(),
        latest: Some(Release { tag, url }),
        newer,
        plan: None,
    }
}

async fn check_release() -> Result<UpdateInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(format!("swiss/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| format!("cannot build an http client: {err}"))?;
    let answer = client
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .send()
        .await
        .map_err(|err| format!("cannot reach github: {err}"))?;
    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateInfo {
            current: current_version(),
            latest: None,
            newer: false,
            plan: None,
        });
    }
    if !answer.status().is_success() {
        return Err(format!("github answered HTTP {}", answer.status()));
    }
    let body: serde_json::Value = answer.json().await.map_err(|err| format!("unreadable answer: {err}"))?;
    let Some(tag) = body.get("tag_name").and_then(|v| v.as_str()) else {
        return Err("the release carries no tag_name".to_string());
    };
    let tag = tag.to_string();
    let url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://github.com/young1lin/swiss/releases/latest")
        .to_string();
    Ok(info_for(tag, url))
}

async fn manifest_bytes(source: &super::fetch::Source) -> Result<Vec<u8>, String> {
    let client = super::fetch::http_client(source)?;
    super::fetch::get_bytes(&client, &super::fetch::manifest_url(source), super::fetch::MANIFEST_LIMIT).await
}

async fn describe_plan(body: &[u8]) -> Result<Option<super::manifest::DownloadPlan>, String> {
    let exe = std::env::current_exe().map_err(|err| format!("cannot find this exe: {err}"))?;
    // Hashing the exe is tens of megabytes. Keep it off the runtime thread.
    let sha = tokio::task::spawn_blocking(move || super::patch::sha256_file(&exe))
        .await
        .map_err(|err| format!("sha256 task: {err}"))??;
    match super::manifest::select(body, env!("SWISS_TARGET"), &sha, env!("CARGO_PKG_VERSION"), false)? {
        super::manifest::Choice::Install(plan) => Ok(Some(plan.download())),
        _ => Ok(None),
    }
}

pub fn version_tuple(tag: &str) -> Option<(u64, u64, u64)> {
    let trimmed = tag.trim().trim_start_matches(['v', 'V']);
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_head = parts.next().unwrap_or("0").split(['-', '+']).next().unwrap_or("0");
    let patch = patch_head.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (version_tuple(latest), version_tuple(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

impl UpdateInfo {
    pub fn render(&self) -> String {
        let mut out = format!("current: {}\n", self.current);
        match &self.latest {
            None => out.push_str("latest:  no releases published yet\n"),
            Some(release) => {
                out.push_str(&format!("latest:  {}", release.tag));
                out.push_str(if self.newer { " — a newer release exists\n" } else { " (up to date)\n" });
                out.push_str(&format!("download: {}\n", release.url));
            }
        }
        if let Some(plan) = &self.plan {
            out.push_str(&format!("download: {}\n", plan.line()));
            if channel() == "dev" {
                out.push_str("this build was made from source (channel dev); --apply refuses it\n");
            } else {
                out.push_str("run \"swiss update --apply\" to install it\n");
            }
        } else {
            out.push_str(
                "\nto update (all state lives outside the binary — nothing migrates):\n  swiss stop\n  replace swiss(.exe) with the downloaded binary\n  swiss skill install --installed   (skills ship in the binary; this refreshes the copies you have)\n  swiss start\n",
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_recognized() {
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2", "0.1.9"), "a missing patch reads as .0");
    }

    #[test]
    fn equal_older_and_unparseable_are_not_newer() {
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("v0.1.0", "custom-build"));
    }

    #[test]
    fn render_always_carries_the_manual_steps() {
        let info = UpdateInfo {
            current: "0.1.0 (deadbee)".into(),
            latest: None,
            newer: false,
            plan: None,
        };
        let text = info.render();
        assert!(text.contains("current: 0.1.0 (deadbee)"));
        assert!(text.contains("no releases published yet"));
        assert!(text.contains("swiss stop"));
        assert!(text.contains("swiss start"));
        assert!(text.contains("swiss skill install --installed"));
    }

    #[test]
    fn render_names_the_download_when_one_exists() {
        let info = UpdateInfo {
            current: "0.1.0 (deadbee)".into(),
            latest: Some(Release { tag: "v0.2.0".into(), url: "https://example.com/rel".into() }),
            newer: true,
            plan: None,
        };
        let text = info.render();
        assert!(text.contains("latest:  v0.2.0 — a newer release exists"));
        assert!(text.contains("download: https://example.com/rel"));
    }

    #[test]
    fn render_names_a_patch_and_the_dev_channel_refusal() {
        let info = UpdateInfo {
            current: "0.2.2 (abc)".into(),
            latest: Some(Release { tag: "v0.2.3".into(), url: "https://example.com/rel".into() }),
            newer: true,
            plan: Some(crate::update::manifest::DownloadPlan {
                via: "patch",
                bytes: 1_400_000,
                full: 9_900_000,
                from: Some("0.2.2".into()),
            }),
        };
        let text = info.render();
        assert!(text.contains("download: patch from 0.2.2, 1.4 MB (full 9.9 MB)"));
        if channel() == "dev" {
            assert!(text.contains("channel dev"));
        }
    }

    #[test]
    fn current_version_carries_version_and_hash() {
        assert!(current_version().starts_with(concat!(env!("CARGO_PKG_VERSION"), " (")));
    }
}
