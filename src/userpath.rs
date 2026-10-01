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

//! "Put swiss on my PATH" for the CURRENT user — the panel switch and `swiss path [on|off]`,
//! the second machine setting beside start-at-sign-in (src/autostart.rs). Per-user on purpose:
//! no elevation, and nothing another account on the machine sees.
//!
//! - Windows: this exe's folder as an entry of the user's own `Path` value (HKCU\Environment),
//!   the same value `setx` and the System Properties dialog write. An entry the operator added
//!   by hand counts as on, however it is written (`%LOCALAPPDATA%\…`, quoted, a trailing `\`,
//!   any case) — and "off" removes exactly those entries, leaving every other one as stored.
//! - Unix: a `~/.local/bin/swiss` symlink to this exe — the per-user bin directory most
//!   distributions already put on PATH. Only a symlink that resolves to this exe is "on", and
//!   only that is ever removed; a real file in the way is refused, never replaced.
//!
//! Already-running programs keep the PATH they started with; a terminal opened afterwards sees
//! the change.

use serde_json::{json, Value};

/// What the panel and the CLI both read.
#[derive(Debug, Clone, PartialEq)]
pub struct UserPathState {
    pub enabled: bool,
    /// Where the setting lives — the registry value, or the symlink — plus anything the
    /// operator has to know for it to work.
    pub detail: String,
    /// The folder (Windows) or the exe (unix) a terminal will find swiss through.
    pub dir: String,
}

impl UserPathState {
    /// The /api/user-path shape, same keys on every platform.
    pub fn to_json(&self) -> Value {
        json!({ "enabled": self.enabled, "detail": self.detail, "dir": self.dir })
    }
}

/// The seam /api/user-path reads through (AppContext::user_path) — a test composition never
/// mounts [`OsUserPath`], so no suite run rewrites the operator's PATH.
pub trait UserPath: Send + Sync {
    fn status(&self) -> UserPathState;
    fn set(&self, enabled: bool) -> Result<UserPathState, String>;
}

/// The real setting, for the current user of this machine.
pub struct OsUserPath;

impl UserPath for OsUserPath {
    fn status(&self) -> UserPathState {
        status()
    }

    fn set(&self, enabled: bool) -> Result<UserPathState, String> {
        set(enabled)
    }
}

// --- PATH entries, as Windows compares them ------------------------------------------------------
//
// Pure, so the suite pins them on every host; the Windows provider is their only caller.

/// One `;`-separated PATH entry as Windows resolves it: `%VAR%` references expanded, blanks and
/// surrounding quotes dropped, a trailing separator ignored, case folded.
#[cfg(any(windows, test))]
fn normalize(entry: &str, expand: &dyn Fn(&str) -> String) -> String {
    let expanded = expand(entry.trim());
    expanded
        .trim()
        .trim_matches('"')
        .trim()
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// Whether `path` already holds `dir`, written any way that resolves to it.
#[cfg(any(windows, test))]
fn path_has(path: &str, dir: &str, expand: &dyn Fn(&str) -> String) -> bool {
    let want = normalize(dir, expand);
    !want.is_empty() && path.split(';').any(|entry| normalize(entry, expand) == want)
}

/// `path` with `dir` appended — last, so it never shadows anything the operator already had.
#[cfg(any(windows, test))]
fn path_with(path: &str, dir: &str) -> String {
    let kept = path.trim_end_matches(';');
    if kept.trim().is_empty() {
        dir.to_string()
    } else {
        format!("{kept};{dir}")
    }
}

/// `path` without every entry that resolves to `dir`; all other entries stay exactly as stored.
#[cfg(any(windows, test))]
fn path_without(path: &str, dir: &str, expand: &dyn Fn(&str) -> String) -> String {
    let want = normalize(dir, expand);
    path.split(';')
        .filter(|entry| normalize(entry, expand) != want)
        .collect::<Vec<_>>()
        .join(";")
}

// --- the providers -------------------------------------------------------------------------------

#[cfg(windows)]
mod provider {
    use super::{path_has, path_with, path_without, UserPathState};
    use swiss_core::platform::{expand_env, user_path_read, user_path_write};

    const DETAIL: &str = "registry: HKCU\\Environment (value: Path)";

    fn exe_dir() -> Result<String, String> {
        let exe =
            std::env::current_exe().map_err(|err| format!("cannot locate this exe: {err}"))?;
        exe.parent()
            .map(|dir| dir.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{} has no folder", exe.display()))
    }

    fn state(dir: &str, path: &str) -> UserPathState {
        UserPathState {
            enabled: path_has(path, dir, &expand_env),
            detail: DETAIL.to_string(),
            dir: dir.to_string(),
        }
    }

    pub fn status() -> UserPathState {
        let dir = exe_dir().unwrap_or_default();
        match user_path_read() {
            Ok(stored) => state(&dir, &stored.map(|(path, _)| path).unwrap_or_default()),
            Err(err) => UserPathState {
                enabled: false,
                detail: format!("{DETAIL}: {err}"),
                dir,
            },
        }
    }

    pub fn set(enabled: bool) -> Result<UserPathState, String> {
        let dir = exe_dir()?;
        // A user with no Path of their own gets one of the type Windows creates (REG_EXPAND_SZ);
        // an existing one keeps its type, so a %USERPROFILE% entry in it stays live.
        let (path, expand) = user_path_read()?.unwrap_or((String::new(), true));
        if path_has(&path, &dir, &expand_env) != enabled {
            let next = if enabled {
                path_with(&path, &dir)
            } else {
                path_without(&path, &dir, &expand_env)
            };
            user_path_write(&next, expand)?;
        }
        Ok(status())
    }
}

#[cfg(unix)]
mod provider {
    use super::UserPathState;
    use std::path::{Path, PathBuf};

    fn link_path() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".local").join("bin").join("swiss"))
    }

    fn exe() -> Result<PathBuf, String> {
        std::env::current_exe().map_err(|err| format!("cannot locate this exe: {err}"))
    }

    /// The link is "on" only when it is a symlink that resolves to this very exe.
    fn points_here(link: &Path, exe: &Path) -> bool {
        let is_link = std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink());
        is_link
            && match (std::fs::canonicalize(link), std::fs::canonicalize(exe)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
    }

    pub fn status() -> UserPathState {
        let Some(link) = link_path() else {
            return UserPathState {
                enabled: false,
                detail: "cannot find the home directory (HOME)".to_string(),
                dir: String::new(),
            };
        };
        let exe = exe().unwrap_or_default();
        let bin = link.parent().map(Path::to_path_buf).unwrap_or_default();
        let on_path = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|entry| entry == bin));
        let mut detail = format!("symlink: {}", link.display());
        if !on_path {
            detail.push_str(&format!(" ({} must be on your PATH)", bin.display()));
        }
        UserPathState {
            enabled: points_here(&link, &exe),
            detail,
            dir: exe.display().to_string(),
        }
    }

    pub fn set(enabled: bool) -> Result<UserPathState, String> {
        let link = link_path().ok_or("cannot find the home directory (HOME)")?;
        let exe = exe()?;
        let existing = std::fs::symlink_metadata(&link).ok();
        if enabled {
            if points_here(&link, &exe) {
                return Ok(status());
            }
            match existing {
                Some(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&link)
                    .map_err(|err| format!("cannot replace {}: {err}", link.display()))?,
                Some(_) => {
                    return Err(format!(
                        "{} exists and is not a symlink - move it aside first",
                        link.display()
                    ))
                }
                None => {}
            }
            if let Some(dir) = link.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
            }
            std::os::unix::fs::symlink(&exe, &link)
                .map_err(|err| format!("cannot link {}: {err}", link.display()))?;
        } else if points_here(&link, &exe) {
            std::fs::remove_file(&link)
                .map_err(|err| format!("cannot remove {}: {err}", link.display()))?;
        }
        Ok(status())
    }
}

#[cfg(not(any(windows, unix)))]
mod provider {
    use super::UserPathState;

    pub fn status() -> UserPathState {
        UserPathState {
            enabled: false,
            detail: "not supported on this platform".to_string(),
            dir: String::new(),
        }
    }

    pub fn set(enabled: bool) -> Result<UserPathState, String> {
        if enabled {
            Err("the PATH setting is not supported on this platform".to_string())
        } else {
            Ok(status())
        }
    }
}

/// Read the setting. Never fails — an unreadable PATH reads as "off", with the detail saying why.
pub fn status() -> UserPathState {
    provider::status()
}

/// Put swiss on (true) or take it off (false) the current user's PATH. The answer is read back
/// from the OS, never echoed from what we meant to write.
pub fn set(enabled: bool) -> Result<UserPathState, String> {
    provider::set(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for ExpandEnvironmentStringsW with one known variable.
    fn expand(s: &str) -> String {
        s.replace("%LOCALAPPDATA%", "C:\\Users\\jdoe\\AppData\\Local")
    }

    const DIR: &str = "C:\\Users\\jdoe\\AppData\\Local\\swiss\\bin";

    #[test]
    fn an_entry_written_any_way_that_resolves_to_the_folder_counts() {
        for path in [
            DIR,
            "C:\\tools;C:\\Users\\jdoe\\AppData\\Local\\swiss\\bin\\",
            "\"C:\\Users\\jdoe\\AppData\\Local\\swiss\\bin\";C:\\tools",
            "c:\\users\\JDOE\\appdata\\local\\SWISS\\bin",
            "%LOCALAPPDATA%\\swiss\\bin",
            "  C:\\Users\\jdoe\\AppData\\Local\\swiss\\bin  ;",
        ] {
            assert!(path_has(path, DIR, &expand), "{path}");
        }
    }

    #[test]
    fn a_neighbouring_folder_is_not_the_folder() {
        for path in [
            "",
            ";;",
            "C:\\Users\\jdoe\\AppData\\Local\\swiss",
            "C:\\Users\\jdoe\\AppData\\Local\\swiss\\bin2",
            "%UNKNOWN%\\swiss\\bin",
        ] {
            assert!(!path_has(path, DIR, &expand), "{path}");
        }
        assert!(!path_has("C:\\tools", "", &expand), "an empty folder is never on PATH");
    }

    #[test]
    fn adding_appends_last_after_any_trailing_separators() {
        assert_eq!(path_with("", DIR), DIR);
        assert_eq!(path_with("C:\\tools", DIR), format!("C:\\tools;{DIR}"));
        assert_eq!(path_with("C:\\tools;", DIR), format!("C:\\tools;{DIR}"));
        assert_eq!(
            path_with("%USERPROFILE%\\bin;C:\\tools;;", DIR),
            format!("%USERPROFILE%\\bin;C:\\tools;{DIR}")
        );
    }

    #[test]
    fn removing_takes_every_spelling_of_the_folder_and_nothing_else() {
        let path = concat!(
            "%USERPROFILE%\\bin;%LOCALAPPDATA%\\swiss\\bin;C:\\tools;;",
            "c:\\users\\jdoe\\appdata\\local\\swiss\\bin\\"
        );
        assert_eq!(path_without(path, DIR, &expand), "%USERPROFILE%\\bin;C:\\tools;");
        assert_eq!(path_without(DIR, DIR, &expand), "");
        assert_eq!(path_without("C:\\tools", DIR, &expand), "C:\\tools", "absent: untouched");
    }

    #[test]
    fn adding_then_removing_gives_back_the_stored_path() {
        let path = "%USERPROFILE%\\bin;C:\\tools";
        let added = path_with(path, DIR);
        assert!(path_has(&added, DIR, &expand));
        assert_eq!(path_without(&added, DIR, &expand), path);
    }

    #[test]
    fn state_json_shape_is_three_keys() {
        let v = UserPathState {
            enabled: true,
            detail: "d".into(),
            dir: "x".into(),
        }
        .to_json();
        assert_eq!(v, json!({ "enabled": true, "detail": "d", "dir": "x" }));
    }
}
