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

//! "swiss update" — a check, not a self-update. The binary the OS runs and the state the
//! gateway keeps are already separate (sealed files under the home directory), so updating is
//! stop, replace the exe, start. This command only compares the running build against the
//! newest GitHub release and prints how to do the replacement by hand; a downloader inside a
//! resident process would be the opposite of the memory budget this project exists for.

pub const REPO: &str = "young1lin/swiss";

/// One GitHub release, reduced to the two fields the operator acts on.
pub struct Release {
    pub tag: String,
    pub url: String,
}

/// The answer "swiss update" renders: what runs, what exists, whether what exists is newer.
pub struct UpdateInfo {
    pub current: String,
    /// None: the repository has published no releases yet — a pre-release repo's honest state.
    pub latest: Option<Release>,
    pub newer: bool,
}

/// The running build's name tag: version plus the build stamp's commit (docs/16 H3).
pub fn current_version() -> String {
    let hash = option_env!("SWISS_GIT_HASH").unwrap_or("unknown");
    format!("{} ({hash})", env!("CARGO_PKG_VERSION"))
}

/// Ask GitHub for the newest release. 404 is not an error — it means "nothing published yet".
/// Every other failure is reported as the honest string it is; the CLI turns it into exit 1.
pub async fn check() -> Result<UpdateInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(format!("swiss/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| format!("cannot build an http client: {err}"))?;
    let answer = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .send()
        .await
        .map_err(|err| format!("cannot reach github: {err}"))?;
    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateInfo {
            current: current_version(),
            latest: None,
            newer: false,
        });
    }
    if !answer.status().is_success() {
        return Err(format!("github answered HTTP {}", answer.status()));
    }
    let body: serde_json::Value = answer
        .json()
        .await
        .map_err(|err| format!("unreadable answer: {err}"))?;
    let Some(tag) = body.get("tag_name").and_then(|v| v.as_str()) else {
        return Err("the release carries no tag_name".to_string());
    };
    let tag = tag.to_string();
    let url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://github.com/young1lin/swiss/releases/latest")
        .to_string();
    let newer = is_newer(&tag, env!("CARGO_PKG_VERSION"));
    Ok(UpdateInfo {
        current: current_version(),
        latest: Some(Release { tag, url }),
        newer,
    })
}

/// "vMAJOR.MINOR.PATCH" as a tuple. A missing patch reads as .0; anything that does not parse
/// reads as None so an unparseable tag can never claim to be "newer" on its own.
fn version_tuple(tag: &str) -> Option<(u64, u64, u64)> {
    let trimmed = tag.trim().trim_start_matches(['v', 'V']);
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_head = parts
        .next()
        .unwrap_or("0")
        .split(['-', '+'])
        .next()
        .unwrap_or("0");
    let patch = patch_head.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True only when latest is strictly above current under the vMAJOR.MINOR.PATCH scheme.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (version_tuple(latest), version_tuple(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

impl UpdateInfo {
    /// The human answer, line by line — including the manual steps, because the manual steps
    /// ARE the update mechanism.
    pub fn render(&self) -> String {
        let mut out = format!("current: {}\n", self.current);
        match &self.latest {
            None => out.push_str("latest:  no releases published yet\n"),
            Some(release) => {
                out.push_str(&format!("latest:  {}", release.tag));
                out.push_str(if self.newer {
                    " — a newer release exists\n"
                } else {
                    " (up to date)\n"
                });
                out.push_str(&format!("download: {}\n", release.url));
            }
        }
        out.push_str(
            "\nto update (all state lives outside the binary — nothing migrates):\n  swiss stop\n  replace swiss(.exe) with the downloaded binary\n  swiss start\n",
        );
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
        assert!(
            !is_newer("nightly", "0.1.0"),
            "junk never claims to be newer"
        );
        assert!(!is_newer("v0.1.0", "custom-build"));
    }

    #[test]
    fn render_always_carries_the_manual_steps() {
        let info = UpdateInfo {
            current: "0.1.0 (deadbee)".into(),
            latest: None,
            newer: false,
        };
        let text = info.render();
        assert!(text.contains("current: 0.1.0 (deadbee)"));
        assert!(text.contains("no releases published yet"));
        assert!(text.contains("swiss stop"));
        assert!(text.contains("swiss start"));
    }

    #[test]
    fn render_names_the_download_when_one_exists() {
        let info = UpdateInfo {
            current: "0.1.0 (deadbee)".into(),
            latest: Some(Release {
                tag: "v0.2.0".into(),
                url: "https://example.com/rel".into(),
            }),
            newer: true,
        };
        let text = info.render();
        assert!(text.contains("latest:  v0.2.0 — a newer release exists"));
        assert!(text.contains("download: https://example.com/rel"));
    }

    #[test]
    fn current_version_carries_version_and_hash() {
        assert!(current_version().starts_with(concat!(env!("CARGO_PKG_VERSION"), " (")));
    }
}
