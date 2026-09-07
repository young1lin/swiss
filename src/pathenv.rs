//! Login-like PATH restoration and launch-command resolution — port of `pathenv.ts`.
//!
//! The Node spec file itself only rebuilds PATH (the user-level bin dirs a detached `lmg start`
//! often loses). What it does NOT have is command resolution: in the Node build the SDK's
//! `StdioClientTransport` spawns through `cross-spawn`, which silently resolves a bare `npx`
//! against PATH + PATHEXT (finding `npx.cmd`). Rust's `Command` does none of that — CreateProcess
//! appends `.exe` but never tries `.cmd` — so the same walk is implemented here by hand, no new
//! dependencies. Without it, every `npx -y @pkg` def would fail as "program not found".

use std::path::{Path, PathBuf};

/// The PATH element separator (`;` on Windows, `:` elsewhere — `path.delimiter` in Node).
fn delimiter() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

/// User-level bin dirs that interactive shells usually have on PATH, but a detached `lmg start`
/// (or Cursor's agent shell) often does not. uv/uvx lands in `~/.local/bin` on Windows; npm's
/// global bins in `%APPDATA%\npm`. Without these, `uvx mcp-server-fetch` fails as
/// 「不是内部或外部命令」 even though the binary is installed.
pub fn extra_bin_dirs() -> Vec<PathBuf> {
    let home = crate::paths::home_dir();
    let mut candidates = vec![home.join(".local").join("bin")];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("npm"));
    }
    candidates.push(home.join(".cargo").join("bin"));
    // existsSync in the Node build — a plain existence check, not an is-dir check.
    candidates.into_iter().filter(|d| d.exists()).collect()
}

/// Whether two PATH entries name the same directory. Windows path comparisons are
/// case-insensitive (`C:\BIN` and `c:\bin` are one entry); POSIX ones are not.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let a = a.to_string_lossy();
    let b = b.to_string_lossy();
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// Prepend existing extra bin dirs that are missing from `path`, so a daemon inherits a
/// login-like PATH — port of `loginPath(path, extras)`. The input is returned unchanged when
/// nothing is missing (no spurious re-join, empty entries dropped as Node's `.filter(Boolean)`
/// did).
pub fn login_path(path: &str, extras: &[PathBuf]) -> String {
    let sep = delimiter();
    let parts: Vec<&str> = path.split(sep).filter(|p| !p.is_empty()).collect();
    let missing: Vec<&PathBuf> = extras
        .iter()
        .filter(|d| !parts.iter().any(|p| same_dir(Path::new(p), d)))
        .collect();
    if missing.is_empty() {
        return path.to_string();
    }
    let mut out: Vec<String> = missing
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    out.extend(parts.iter().map(|s| s.to_string()));
    out.join(&sep.to_string())
}

/// `loginPath()` with Node's default arguments: the process PATH plus [`extra_bin_dirs`]. This
/// is what boot sets and what the proc adapter re-applies to each child's environment.
pub fn login_path_from_env() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    login_path(&current, &extra_bin_dirs())
}

/// The PATHEXT list Windows uses to resolve a bare command name. From the environment when set
/// (a machine may add `.PY` etc.), else the documented default. Not consulted off Windows.
#[cfg(windows)]
fn path_exts() -> Vec<String> {
    const DEFAULT: &str = ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC";
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| DEFAULT.to_string())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// One directory of the resolution search for a BARE command name: the name with each PATHEXT
/// extension appended (`.cmd` shims like `npx.cmd` are found this way — CreateProcess alone
/// would miss them). The extensionless spelling is deliberately NOT tried: npm's install dir
/// ships a POSIX sh `npx` right beside `npx.cmd` for git-bash's benefit, and that file "is a
/// file" — it would win the scan and then die at CreateProcess with `os error 193` (not a
/// Win32 image). cross-spawn's which-side only matched PATHEXT extensions for the same reason.
#[cfg(windows)]
fn candidates_in(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    for ext in path_exts() {
        out.push(dir.join(format!("{name}{}", ext.to_ascii_lowercase())));
    }
}

/// One directory of the resolution search off Windows: the command exactly as given (the
/// executable bit is left for the OS to judge, as execve will anyway).
#[cfg(not(windows))]
fn candidates_in(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    out.push(dir.join(name));
}

/// Whether a launch name is a path-shaped rather than a bare command.
fn looks_like_path(name: &str) -> bool {
    if name.contains('/') || name.contains('\\') {
        return true;
    }
    #[cfg(windows)]
    {
        // A drive-prefixed name (`C:tool`) is a path even without a separator.
        name.as_bytes().get(1) == Some(&b':')
    }
    #[cfg(not(windows))]
    {
        name.starts_with('/')
    }
}

/// Whether `name` already ends in one of the PATHEXT extensions — appending another (`.cmd.exe`)
/// would only ever produce nonsense.
#[cfg(windows)]
fn has_pathext_extension(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    path_exts()
        .iter()
        .any(|e| lower.ends_with(&e.to_ascii_lowercase()))
}

/// Resolve a launch command to the file that should be handed to spawn — the hand-rolled
/// equivalent of what `cross-spawn`/`which` did inside the Node SDK's stdio spawn.
///
/// - a path-shaped name is honored exactly as written first (an explicit path is the user's own
///   resolution), with the PATHEXT forms after it as fallback — a path-shaped
///   `...\bin\mytool` finds `mytool.cmd` beside it the way cmd's own lookup would;
/// - a bare name walks `path` in order; per directory a name that already carries a dotted
///   extension (`mytool.cmd`) is tried verbatim first, then with each PATHEXT extension appended
///   (see `candidates_in` for why the extensionless spelling is skipped for dotless names).
///
/// `None` means nothing matched; the caller still spawns the raw name so the OS error names the
/// command (the analogue of Node's `spawn ENOENT`).
pub fn resolve_command(name: &str, path: &str) -> Option<PathBuf> {
    let sep = delimiter();
    let mut candidates = Vec::new();
    if looks_like_path(name) {
        candidates.push(PathBuf::from(name));
        #[cfg(windows)]
        if !has_pathext_extension(name) {
            for ext in path_exts() {
                candidates.push(PathBuf::from(format!("{name}{}", ext.to_ascii_lowercase())));
            }
        }
    } else {
        for dir in path.split(sep).filter(|p| !p.is_empty()) {
            #[cfg(windows)]
            if name.contains('.') {
                candidates.push(Path::new(dir).join(name));
            }
            candidates_in(Path::new(dir), name, &mut candidates);
        }
    }
    candidates.into_iter().find(|c| c.is_file())
}

#[cfg(test)]
mod tests {
    // Ported from test/pathenv.test.ts (PATH restoration assertions) plus the resolution walk,
    // which the Node build never had to test (cross-spawn owned it).
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("lmg-pathenv-{tag}-{}", crate::util::random_hex(6)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn login_path_prepends_missing_dirs_and_keeps_existing_order() {
        let a = PathBuf::from("/definitely/missing/a");
        let b = PathBuf::from("/definitely/missing/b");
        let out = login_path("/usr/bin:/bin", &[a.clone(), b.clone()]);
        let sep = delimiter().to_string();
        let expected = format!(
            "{}{}{}{}{}",
            a.display(),
            sep,
            b.display(),
            sep,
            "/usr/bin:/bin"
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn login_path_returns_the_input_when_nothing_is_missing() {
        let dir = temp_dir("dup");
        let path = format!("{}{}x", dir.display(), delimiter());
        // The same dir twice (one by a different spelling is covered by same_dir) adds nothing.
        assert_eq!(login_path(&path, std::slice::from_ref(&dir)), path);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_dir_is_case_insensitive_only_on_windows() {
        let (a, b) = (
            Path::new("C:\\Users\\X\\bin"),
            Path::new("c:\\users\\x\\BIN"),
        );
        if cfg!(windows) {
            assert!(same_dir(a, b));
        } else {
            // On POSIX the same-shaped strings are still equal here; a case difference is not.
            assert!(same_dir(Path::new("/a"), Path::new("/a")));
            assert!(!same_dir(Path::new("/A"), Path::new("/a")));
        }
    }

    #[test]
    fn empty_path_entries_are_dropped() {
        let dir = PathBuf::from("/missing/bin");
        let out = login_path(
            &format!("{}a{}", delimiter(), delimiter()),
            std::slice::from_ref(&dir),
        );
        assert!(out.starts_with(&dir.to_string_lossy().to_string()));
        assert!(!out.contains(&format!("{}{}", delimiter(), delimiter())));
    }

    #[test]
    fn resolve_command_finds_a_cmd_shim_through_pathext() {
        // Needs a Windows PATHEXT extension so the walk exercises the same branch `npx` hits.
        if !cfg!(windows) {
            return;
        }
        let dir = temp_dir("which");
        std::fs::write(dir.join("npx.cmd"), b"@echo off").unwrap();
        let path = format!("{}{}elsewhere", dir.display(), delimiter());
        let resolved = resolve_command("npx", &path).expect("npx.cmd resolves through PATHEXT");
        assert_eq!(resolved, dir.join("npx.cmd"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_command_tries_dotted_bare_names_verbatim() {
        if !cfg!(windows) {
            return;
        }
        let dir = temp_dir("which-dotted");
        std::fs::write(dir.join("mytool.cmd"), b"@echo off").unwrap();
        let path = format!("{}{}elsewhere", dir.display(), delimiter());
        // `mytool.cmd` typed bare must not resolve to a nonexistent `mytool.cmd.exe`.
        let resolved =
            resolve_command("mytool.cmd", &path).expect("dotted bare name resolves verbatim");
        assert_eq!(resolved, dir.join("mytool.cmd"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_command_applies_pathext_to_extensionless_paths() {
        if !cfg!(windows) {
            return;
        }
        let dir = temp_dir("which-pathshaped");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("tool.cmd"), b"@echo off").unwrap();
        // Path-shaped and extensionless: the verbatim spelling does not exist, the .cmd beside it
        // does — the fallback cmd's own lookup would have found.
        let bare = bin.join("tool").to_string_lossy().to_string();
        let resolved =
            resolve_command(&bare, "elsewhere").expect("PATHEXT applies to path-shaped names");
        assert_eq!(resolved, bin.join("tool.cmd"));
        // An explicit extension is honored exactly, never re-suffixed.
        let exact = bin.join("tool.cmd").to_string_lossy().to_string();
        assert_eq!(
            resolve_command(&exact, "elsewhere").as_deref(),
            Some(bin.join("tool.cmd").as_path())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_command_finds_bare_files_off_windows() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("which-posix");
        std::fs::write(dir.join("uvx"), b"#!/bin/sh\n").unwrap();
        let path = format!("{}{}elsewhere", dir.display(), delimiter());
        let resolved = resolve_command("uvx", &path).expect("bare name resolves on the PATH");
        assert_eq!(resolved, dir.join("uvx"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_command_uses_first_match_in_path_order() {
        let first = temp_dir("which-first");
        let second = temp_dir("which-second");
        let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
        std::fs::write(first.join(name), b"x").unwrap();
        std::fs::write(second.join(name), b"x").unwrap();
        let path = format!("{}{}{}", first.display(), delimiter(), second.display());
        assert_eq!(resolve_command("tool", &path), Some(first.join(name)));
        std::fs::remove_dir_all(&first).ok();
        std::fs::remove_dir_all(&second).ok();
    }

    #[test]
    fn resolve_command_takes_a_separated_name_as_given() {
        // A path-shaped name must never go through the PATH walk; the file itself is the
        // candidate (an absolute spelling, so the test needs no cwd games).
        let dir = temp_dir("which-rel");
        let name = if cfg!(windows) {
            "server.exe"
        } else {
            "server"
        };
        let target = dir.join(name);
        std::fs::write(&target, b"x").unwrap();
        let spelling = target.to_string_lossy().into_owned();
        assert_eq!(resolve_command(&spelling, ""), Some(target));
        // And a miss on a path-shaped name stays a miss even when PATH has a same-named file.
        assert_eq!(
            resolve_command(&dir.join("missing").to_string_lossy(), ""),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_command_misses_cleanly() {
        assert_eq!(
            resolve_command("definitely-not-a-real-tool-xyz", "/nowhere"),
            None
        );
    }
}
