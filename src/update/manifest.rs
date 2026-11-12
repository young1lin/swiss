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

//! `update.json` (SPEC §release.ci). Unknown fields are ignored. A schema this binary does
//! not know means the client is too old and the operator gets the manual steps.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::check::version_tuple;

pub const MANUAL: &str = "\
to update (all state lives outside the binary — nothing migrates):
  swiss stop
  replace swiss(.exe) with the downloaded binary
  swiss skill install --installed
  swiss start";

#[derive(Debug)]
pub struct Plan {
    pub version: String,
    pub tag: String,
    pub exe_sha256: String,
    pub full_bytes: u64,
    pub via: Via,
}

#[derive(Debug)]
pub enum Via {
    Patch {
        from: String,
        asset: String,
        sha256: String,
        size: u64,
        window_log: u32,
    },
    Full { asset: String, sha256: String, size: u64 },
}

impl Plan {
    /// The check command's extra download line, including the full size beside a patch.
    pub fn check_line(&self) -> String {
        match &self.via {
            Via::Patch { from, size, .. } => {
                format!("patch from {from}, {} (full {})", mb(*size), mb(self.full_size()))
            }
            Via::Full { size, .. } => format!("full, {}", mb(*size)),
        }
    }

    /// The apply command's download line.
    pub fn apply_line(&self) -> String {
        match &self.via {
            Via::Patch { from, size, .. } => format!("download: patch from {from}, {}", mb(*size)),
            Via::Full { size, .. } => format!("download: full, {}", mb(*size)),
        }
    }

    pub fn asset(&self) -> &str {
        match &self.via {
            Via::Patch { asset, .. } | Via::Full { asset, .. } => asset,
        }
    }

    pub fn asset_sha(&self) -> &str {
        match &self.via {
            Via::Patch { sha256, .. } | Via::Full { sha256, .. } => sha256,
        }
    }

    pub fn asset_size(&self) -> u64 {
        match &self.via {
            Via::Patch { size, .. } | Via::Full { size, .. } => *size,
        }
    }

    pub fn via_name(&self) -> &'static str {
        match self.via {
            Via::Patch { .. } => "patch",
            Via::Full { .. } => "full",
        }
    }

    fn full_size(&self) -> u64 {
        self.full_bytes
    }

    /// What `GET /api/update/check` puts in `plan`, plus `from` for the CLI line.
    pub fn download(&self) -> DownloadPlan {
        match &self.via {
            Via::Patch { from, size, .. } => DownloadPlan {
                via: "patch",
                bytes: *size,
                full: self.full_bytes,
                from: Some(from.clone()),
            },
            Via::Full { size, .. } => DownloadPlan {
                via: "full",
                bytes: *size,
                full: self.full_bytes,
                from: None,
            },
        }
    }
}

/// The download the check names. `bytes` is the asset that would be fetched.
#[derive(Clone, Debug)]
pub struct DownloadPlan {
    pub via: &'static str,
    pub bytes: u64,
    pub full: u64,
    pub from: Option<String>,
}

impl DownloadPlan {
    pub fn line(&self) -> String {
        match self.via {
            "patch" => format!(
                "patch from {}, {} (full {})",
                self.from.as_deref().unwrap_or("?"),
                mb(self.bytes),
                mb(self.full)
            ),
            _ => format!("full, {}", mb(self.bytes)),
        }
    }
}

pub fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

#[derive(Debug)]
pub enum Choice {
    Install(Plan),
    AlreadyNewest,
    Older,
    UnknownTarget,
    SchemaTooNew,
}

#[derive(Debug, Deserialize)]
struct Raw {
    schema: u64,
    version: String,
    tag: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    targets: BTreeMap<String, RawTarget>,
}

/// The release a manifest names: its tag and its notes page.
pub struct Head {
    pub tag: String,
    pub notes: Option<String>,
}

pub fn head(bytes: &[u8]) -> Result<Head, String> {
    let raw = parse(bytes)?;
    Ok(Head { tag: raw.tag, notes: raw.notes })
}

#[derive(Debug, Deserialize)]
struct RawTarget {
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    full: Option<RawAsset>,
    #[serde(default)]
    patches: Vec<RawPatch>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    asset: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct RawPatch {
    from: String,
    #[serde(rename = "fromSha256")]
    from_sha256: String,
    asset: String,
    sha256: String,
    size: u64,
    #[serde(rename = "windowLog")]
    window_log: u32,
}

fn parse(bytes: &[u8]) -> Result<Raw, String> {
    serde_json::from_slice(bytes).map_err(|err| format!("update.json: {err}"))
}

pub fn select(bytes: &[u8], target: &str, local_sha: &str, current: &str, force: bool) -> Result<Choice, String> {
    let raw = parse(bytes)?;
    if raw.schema != 1 {
        return Ok(Choice::SchemaTooNew);
    }
    let Some(entry) = raw.targets.get(target) else {
        return Ok(Choice::UnknownTarget);
    };
    let latest = version_tuple(&raw.version).ok_or_else(|| {
        format!("release version {} is not MAJOR.MINOR.PATCH", raw.version)
    })?;
    let running = version_tuple(current).ok_or_else(|| {
        format!("this build's version {current} is not MAJOR.MINOR.PATCH")
    })?;
    if latest < running {
        return Ok(Choice::Older);
    }
    if latest == running && !force {
        return Ok(Choice::AlreadyNewest);
    }
    let full = entry.full.as_ref().ok_or_else(|| format!("update.json has no full asset for {target}"))?;
    let via = entry
        .patches
        .iter()
        .find(|patch| patch.from_sha256.eq_ignore_ascii_case(local_sha))
        .map(|patch| Via::Patch {
            from: patch.from.clone(),
            asset: patch.asset.clone(),
            sha256: patch.sha256.clone(),
            size: patch.size,
            window_log: patch.window_log,
        })
        .unwrap_or(Via::Full {
            asset: full.asset.clone(),
            sha256: full.sha256.clone(),
            size: full.size,
        });
    let _ = entry.sha256;
    Ok(Choice::Install(Plan {
        version: raw.version,
        tag: raw.tag,
        exe_sha256: entry.sha256.clone(),
        full_bytes: full.size,
        via,
    }))
}
