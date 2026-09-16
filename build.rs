/*
 * Copyright 2026 The swiss authors
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

// The build stamp (docs/16 H3): which commit this binary was built from, and when.
//
// `git rev-parse --short HEAD` + a dirty marker from `git status --porcelain`, stamped into
// SWISS_GIT_HASH, and an RFC 3339 UTC SWISS_BUILD_TIME computed from the epoch by hand (no
// chrono here: the build script must not grow dependencies). Everything else is std.
//
// Failure is not an option: a machine with no git (or a tarball checkout) builds fine with
// both values as "unknown" - the stamp is metadata, and metadata must never break a build.
// The rerun-if-changed list names .git/HEAD and .git/refs/heads, so the crate re-stamps when
// the commit moves and not on every edit; the -dirty marker can therefore lag a `git add`
// until the next commit - accepted, because the hash (the fact deploy.ps1 asserts on) is
// still exact.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let hash = git_hash().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=SWISS_GIT_HASH={hash}");
    println!("cargo:rustc-env=SWISS_BUILD_TIME={}", rfc3339_now());
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}

/// The short hash of HEAD, with a `-dirty` suffix when the worktree is not clean. None means
/// "no git here" (or not a repo), which the caller turns into "unknown" rather than an error.
fn git_hash() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut hash = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if hash.is_empty() {
        return None;
    }
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if dirty.status.success()
        && !String::from_utf8_lossy(&dirty.stdout).trim().is_empty()
    {
        hash.push_str("-dirty");
    }
    Some(hash)
}

/// The current instant as RFC 3339 UTC (2026-09-11T13:16:40Z). Days-since-epoch to civil
/// date is Howard Hinnant's `civil_from_days` - the one algorithm that is both correct over
/// the whole range and small enough to justify no dependency.
fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (time / 3600, (time % 3600) / 60, time % 60);
    format!("{year}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: cargo never compiles a build script as a test target, so `cargo test` does not run
    // these. They run by hand — `rustc --test build.rs -o <scratch>/build-tests.exe`, then
    // execute it — which is how the constants below were checked against Python's
    // `date(1970,1,1) + timedelta(days=n)`. The algorithm is the kind of code that is
    // "obviously right" until a leap year disagrees.
    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(13_514), (2007, 1, 1));
        assert_eq!(civil_from_days(13_515), (2007, 1, 2));
        assert_eq!(civil_from_days(20_650), (2026, 7, 16));
        // A leap day: 2024-02-29 is day 19_782, and the day after it is March.
        assert_eq!(civil_from_days(19_781), (2024, 2, 28));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }

    #[test]
    fn the_stamp_is_rfc3339_utc() {
        let now = rfc3339_now();
        assert_eq!(now.len(), 20, "{now}");
        assert_eq!(&now[4..5], "-", "{now}");
        assert_eq!(&now[7..8], "-", "{now}");
        assert_eq!(&now[10..11], "T", "{now}");
        assert_eq!(&now[13..14], ":", "{now}");
        assert_eq!(&now[16..17], ":", "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert!(
            now.bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'T' || b == b':' || b == b'Z'),
            "{now}"
        );
    }
}

