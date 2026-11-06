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

//! The AI skills embedded in the binary, and the two directories they go to (SPEC §host.cli):
//! `.claude` for Claude Code and `.agents` for every other client. The general `swiss` skill covers
//! safe setup; `swiss-remote` holds the remote CLI contract; `swiss-memory` holds the memory tool
//! and CLI contract (SPEC §memory). A single-file release cannot read any skill from beside the
//! binary.
//!
//! Which skills land is the user's answer, so nothing here installs by default and a refresh never
//! adds one. An install replaces `SKILL.md` and, for the `swiss` skill, `SETUP.md` beside it. The
//! rest of the directory is the user's, and a replacement that fails leaves the copy that was there.

use std::path::PathBuf;

/// All shipped skills are embedded at compile time; Node is never needed to build the exe.
const SKILL_MD: &str = include_str!("skill_assets/SKILL.md");
const SETUP_MD: &str = include_str!("../SETUP.md");
const REMOTE_SKILL_MD: &str = include_str!("skill_assets/remote/SKILL.md");
const MEMORY_SKILL_MD: &str = include_str!("skill_assets/memory/SKILL.md");

const SKILL_NAME: &str = "swiss";
const REMOTE_SKILL_NAME: &str = "swiss-remote";
const MEMORY_SKILL_NAME: &str = "swiss-memory";
const SHIPPED_SKILLS: [(&str, &str); 3] = [
    (SKILL_NAME, SKILL_MD),
    (REMOTE_SKILL_NAME, REMOTE_SKILL_MD),
    (MEMORY_SKILL_NAME, MEMORY_SKILL_MD),
];

/// The only directories a skill goes to. Copies an older build left under `.cursor` or `.codex`
/// are neither read nor refreshed.
const ROOTS: [&str; 2] = [".claude", ".agents"];

/// A copy on disk, against the one in this binary.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CopyState {
    Absent,
    Current,
    Outdated,
    Unreadable,
}

#[derive(serde::Serialize)]
pub struct SkillLocation {
    pub root: String,
    pub path: String,
    pub installed: Option<bool>,
    pub state: CopyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(serde::Serialize)]
pub struct SkillStatus {
    pub name: String,
    pub description: String,
    #[serde(rename = "loadsByItself")]
    pub loads_by_itself: bool,
    pub locations: Vec<SkillLocation>,
}

/// What a refresh did: the copies it replaced, and why any it could not reach were left.
#[derive(Debug, Default)]
pub struct Refresh {
    pub replaced: Vec<String>,
    pub failed: Vec<String>,
}

/// One frontmatter value, from a single line or a `>-` block of indented lines. The skill's own
/// text is the one place these are written, so the list cannot drift from what a client reads.
fn frontmatter(body: &str, key: &str) -> Option<String> {
    let block = body.strip_prefix("---\n")?.split("\n---").next()?;
    let mut lines = block.lines();
    let first = lines.by_ref().find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))?.trim();
    if !first.starts_with('>') {
        return Some(first.to_string());
    }
    let folded: Vec<&str> = lines
        .take_while(|line| line.starts_with(' '))
        .map(str::trim)
        .collect();
    Some(folded.join(" "))
}

/// A skill loads by itself unless its text says only the user may invoke it.
fn loads_by_itself(body: &str) -> bool {
    frontmatter(body, "disable-model-invocation").as_deref() != Some("true")
}

fn shipped_body(name: &str) -> Option<&'static str> {
    SHIPPED_SKILLS
        .iter()
        .find(|(shipped, _)| *shipped == name)
        .map(|(_, body)| *body)
}

/// Every shipped name, for `--all` and for the refusal that lists them.
pub fn shipped_names() -> Vec<String> {
    SHIPPED_SKILLS.iter().map(|(name, _)| (*name).to_string()).collect()
}

pub fn valid_target(name: &str, root: &str) -> bool {
    shipped_body(name).is_some() && ROOTS.contains(&root)
}

/// This user's skill directories.
pub struct Places {
    pub home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, when it names something. Claude Code then reads its user skills and
    /// settings under it and nothing under `~/.claude`.
    pub claude_config: Option<PathBuf>,
}

impl Places {
    pub fn of_this_user() -> Result<Self, String> {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .ok_or_else(|| "cannot find the user home directory".to_string())?;
        let claude_config = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|c| !c.to_string_lossy().trim().is_empty())
            .map(PathBuf::from);
        Ok(Self { home: PathBuf::from(home), claude_config })
    }

    /// Where a skill root lives. `.claude` is the only one that can move: with `CLAUDE_CONFIG_DIR`
    /// set, that IS the directory Claude Code reads.
    fn root_dir(&self, root: &str) -> PathBuf {
        match (root, &self.claude_config) {
            (".claude", Some(config)) => config.join("skills"),
            _ => self.home.join(root).join("skills"),
        }
    }

    fn skill_file(&self, root: &str, name: &str) -> PathBuf {
        self.root_dir(root).join(name).join("SKILL.md")
    }

    fn setup_file(&self, root: &str) -> PathBuf {
        self.root_dir(root).join(SKILL_NAME).join("SETUP.md")
    }

    fn setup_current(&self, root: &str) -> bool {
        std::fs::read(self.setup_file(root)).ok().is_some_and(|found| found == SETUP_MD.as_bytes())
    }

    fn location(&self, root: &str, name: &str, body: &str) -> SkillLocation {
        let path = self.skill_file(root, name);
        let (installed, state, error) = match std::fs::read(&path) {
            Ok(found) if found == body.as_bytes() && (name != SKILL_NAME || self.setup_current(root)) => {
                (Some(true), CopyState::Current, None)
            }
            Ok(_) => (Some(true), CopyState::Outdated, None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (Some(false), CopyState::Absent, None)
            }
            Err(err) => (None, CopyState::Unreadable, Some(err.to_string())),
        };
        SkillLocation {
            root: root.into(),
            path: path.to_string_lossy().into_owned(),
            installed,
            state,
            error,
        }
    }

    /// Inspect only the shipped names; never enumerate or read unrelated user skills.
    pub fn status(&self) -> Vec<SkillStatus> {
        SHIPPED_SKILLS
            .iter()
            .map(|(name, body)| SkillStatus {
                name: (*name).into(),
                description: frontmatter(body, "description").unwrap_or_default(),
                loads_by_itself: loads_by_itself(body),
                locations: ROOTS.iter().map(|root| self.location(root, name, body)).collect(),
            })
            .collect()
    }

    /// Replace one `SKILL.md`, staged beside it. User additions and other skills remain untouched,
    /// and a failed staged write or rename leaves the previous installed document intact.
    pub fn install_one(&self, name: &str, root: &str) -> Result<(), String> {
        if !valid_target(name, root) {
            return Err("unknown skill or skill directory".into());
        }
        let body = shipped_body(name).ok_or_else(|| "unknown skill".to_string())?;
        let target = self.skill_file(root, name);
        let directory = target
            .parent()
            .ok_or_else(|| "a skill file has a directory".to_string())?;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let staged = directory.join(format!(".SKILL-{}.tmp", swiss_core::util::random_hex(12)));
        let result = std::fs::write(&staged, body)
            .and_then(|_| retry_busy(|| std::fs::rename(&staged, &target)))
            .map_err(|e| format!("could not install {name} in {root}: {e}"));
        if result.is_err() {
            let _ = std::fs::remove_file(&staged);
            return result;
        }
        if name == SKILL_NAME {
            return self.write_setup(root);
        }
        result
    }

    /// `SETUP.md` beside the swiss skill, staged the same way `SKILL.md` is.
    fn write_setup(&self, root: &str) -> Result<(), String> {
        let target = self.setup_file(root);
        let directory = target.parent().ok_or_else(|| "SETUP.md has a directory".to_string())?;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let staged = directory.join(format!(".SETUP-{}.tmp", swiss_core::util::random_hex(12)));
        let result = std::fs::write(&staged, SETUP_MD)
            .and_then(|_| retry_busy(|| std::fs::rename(&staged, &target)))
            .map_err(|e| format!("could not install SETUP.md in {root}: {e}"));
        if result.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        result
    }

    /// Exactly these skills, in both directories. A name this binary does not ship refuses the
    /// whole call before anything is written. Returns the files written.
    pub fn install_named(&self, names: &[String]) -> Result<Vec<String>, String> {
        if let Some(unknown) = names.iter().find(|name| shipped_body(name).is_none()) {
            return Err(format!(
                "unknown skill: {unknown} (this binary ships {})",
                shipped_names().join(", ")
            ));
        }
        let mut written = Vec::new();
        for name in names {
            for root in ROOTS {
                self.install_one(name, root)?;
                written.push(self.skill_file(root, name).to_string_lossy().into_owned());
            }
        }
        Ok(written)
    }

    /// Bring the copies already there up to this binary. Never adds one: a skill the user declined
    /// stays absent through every update, directory by directory. One directory that refuses does
    /// not stop the other.
    pub fn refresh_installed(&self) -> Refresh {
        let mut refresh = Refresh::default();
        for skill in self.status() {
            for place in &skill.locations {
                match place.state {
                    CopyState::Absent | CopyState::Current => {}
                    CopyState::Outdated => match self.install_one(&skill.name, &place.root) {
                        Ok(()) => refresh.replaced.push(place.path.clone()),
                        Err(err) => refresh.failed.push(err),
                    },
                    CopyState::Unreadable => refresh.failed.push(format!(
                        "could not check {}: {}",
                        place.path,
                        place.error.as_deref().unwrap_or("unknown error")
                    )),
                }
            }
        }
        refresh
    }
}

pub fn status() -> Result<Vec<SkillStatus>, String> {
    Ok(Places::of_this_user()?.status())
}

pub fn install_selected(name: &str, root: &str) -> Result<(), String> {
    Places::of_this_user()?.install_one(name, root)
}

/// How often, and how far apart, a denied replace is tried again (about a second in all).
const BUSY_ATTEMPTS: u32 = 40;
const BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(25);

/// Whether another process holding a file open explains this refusal. Windows answers access
/// denied or a sharing violation depending on how the file was opened, and std has no `ErrorKind`
/// for the second. The number means something else elsewhere (EPIPE on Linux), hence the cfg.
fn is_busy(err: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    err.kind() == std::io::ErrorKind::PermissionDenied
        || (cfg!(windows) && err.raw_os_error() == Some(ERROR_SHARING_VIOLATION))
}

/// Run a filesystem step, trying again while it is denied. Windows refuses to rename a file
/// while another process holds it open without delete sharing, and a skill watcher (an AI
/// client scanning its skills directory) opens a freshly written SKILL.md for a few
/// milliseconds: measured on ~/.agents/skills, the staged rename was denied once and went
/// through 25 ms later.
fn retry_busy<T>(mut step: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match step() {
            Err(e) if is_busy(&e) && attempt < BUSY_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(BUSY_BACKOFF);
            }
            done => return done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(PathBuf);

    impl TempHome {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "swiss-skill-install-{}-{}",
                std::process::id(),
                swiss_core::util::random_hex(8)
            )))
        }

        fn places(&self) -> Places {
            Places { home: self.0.clone(), claude_config: None }
        }

        fn file(&self, root: &str, name: &str) -> PathBuf {
            self.0.join(root).join("skills").join(name).join("SKILL.md")
        }

        /// A copy some earlier build left, with text this binary does not ship.
        fn plant_old_copy(&self, root: &str, name: &str) -> PathBuf {
            let file = self.file(root, name);
            std::fs::create_dir_all(file.parent().expect("a skill directory")).expect("mkdir");
            std::fs::write(&file, "an older copy").expect("write the old copy");
            file
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn state_of(rows: &[SkillStatus], name: &str, root: &str) -> CopyState {
        rows.iter()
            .find(|skill| skill.name == name)
            .and_then(|skill| skill.locations.iter().find(|l| l.root == root))
            .map(|location| location.state)
            .expect("a shipped skill in a known root")
    }

    #[test]
    fn status_checks_each_shipped_skill_in_the_two_roots_without_writing() {
        let home = TempHome::new();
        let installed = home.plant_old_copy(".agents", "swiss");
        std::fs::create_dir_all(home.0.join(".claude/skills/swiss-memory")).expect("mkdir");
        let rows = home.places().status();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "swiss");
        assert_eq!(
            rows[0].locations.iter().map(|l| l.root.as_str()).collect::<Vec<_>>(),
            [".claude", ".agents"]
        );
        assert_eq!(
            rows[0].locations.iter().map(|l| l.installed).collect::<Vec<_>>(),
            [Some(false), Some(true)]
        );
        assert!(rows.iter().skip(1).flat_map(|s| &s.locations).all(|l| l.installed == Some(false)));
        assert!(!home.0.join(".claude/skills/swiss").exists(), "checking never creates a directory");
        assert_eq!(std::fs::read_to_string(installed).expect("read fixture"), "an older copy");
    }

    #[test]
    fn a_moved_claude_config_dir_is_the_one_both_status_and_install_use() {
        // Claude Code reads `$CLAUDE_CONFIG_DIR` and nothing under `~/.claude` when it is set.
        // Status and every install have to follow it, or the panel reports "installed" for a
        // directory the client never opens (2026-11-01).
        let home = TempHome::new();
        let config = TempHome::new();
        let places = Places { home: home.0.clone(), claude_config: Some(config.0.clone()) };
        let rows = places.status();
        assert_eq!(
            rows[0].locations[0].path,
            config.0.join("skills").join("swiss").join("SKILL.md").to_string_lossy()
        );
        places.install_one("swiss", ".claude").expect("install");
        assert!(config.0.join("skills/swiss/SKILL.md").is_file(), "the client's directory got the skill");
        assert!(!home.0.join(".claude").exists(), "and the one it does not read stayed untouched");
        assert_eq!(state_of(&places.status(), "swiss", ".claude"), CopyState::Current);
    }

    // SPEC §host.cli: two directories, one list for the CLI and the panel's route.
    #[test]
    fn skills_go_to_exactly_the_two_directories() {
        let home = TempHome::new();
        let written = home.places().install_named(&shipped_names()).expect("install every skill");
        assert_eq!(written.len(), 2 * SHIPPED_SKILLS.len());
        for (name, body) in SHIPPED_SKILLS {
            for root in [".claude", ".agents"] {
                assert_eq!(std::fs::read(home.file(root, name)).expect("installed"), body.as_bytes());
            }
        }
        assert!(!home.0.join(".cursor").exists());
        assert!(!home.0.join(".codex").exists());

        for (name, root) in [("../other", ".claude"), ("swiss", "../other"), ("swiss", ".cursor"), ("swiss", ".codex")] {
            assert!(!valid_target(name, root), "{name} in {root}");
            assert!(home.places().install_one(name, root).is_err(), "{name} in {root}");
        }
    }

    #[test]
    fn a_config_dir_replaces_dot_claude_for_every_skill() {
        let home = TempHome::new();
        let config = home.0.join(".claude-account");
        let places = Places { home: home.0.clone(), claude_config: Some(config.clone()) };
        let written = places.install_named(&shipped_names()).expect("install with a config dir");
        assert_eq!(written.len(), 2 * SHIPPED_SKILLS.len());
        for (name, body) in SHIPPED_SKILLS {
            let path = config.join("skills").join(name).join("SKILL.md");
            assert_eq!(std::fs::read(&path).expect("the config dir's skill"), body.as_bytes());
        }
        assert!(!home.0.join(".claude").exists());
    }

    // SPEC §host.cli: the old bulk install emptied the directory first, so everything a user kept
    // beside a skill went with it.
    #[test]
    fn a_reinstall_keeps_what_the_user_put_beside_the_skill() {
        let home = TempHome::new();
        home.plant_old_copy(".agents", "swiss");
        let folder = home.0.join(".agents/skills/swiss");
        std::fs::write(folder.join("notes.txt"), "keep me").expect("a file of the user's");
        std::fs::create_dir_all(folder.join("references")).expect("a directory of the user's");
        std::fs::write(folder.join("references/own.md"), "keep me too").expect("a nested file");

        home.places().install_named(&names(&["swiss"])).expect("install");
        home.places().refresh_installed();

        assert_eq!(std::fs::read_to_string(folder.join("SKILL.md")).expect("skill"), SKILL_MD);
        assert_eq!(std::fs::read_to_string(folder.join("notes.txt")).expect("kept"), "keep me");
        assert_eq!(std::fs::read_to_string(folder.join("references/own.md")).expect("kept"), "keep me too");
    }

    // SPEC §host.cli: a replacement that cannot land leaves what was there and nothing beside it.
    #[test]
    fn a_failed_replacement_keeps_the_old_copy() {
        let home = TempHome::new();
        let file = home.file(".agents", "swiss");
        // A directory where the file goes: every platform refuses the rename onto it.
        std::fs::create_dir_all(file.join("inside")).expect("mkdir");
        std::fs::write(file.join("inside/own.md"), "kept").expect("a file inside");

        assert!(home.places().install_one("swiss", ".agents").is_err());

        assert_eq!(std::fs::read_to_string(file.join("inside/own.md")).expect("kept"), "kept");
        let left: Vec<_> = std::fs::read_dir(file.parent().expect("the skill directory"))
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(left, ["SKILL.md"], "the staged file does not stay behind");
    }

    #[test]
    fn naming_skills_installs_exactly_those() {
        let home = TempHome::new();
        home.places().install_named(&names(&["swiss-memory"])).expect("install");
        let rows = home.places().status();
        assert_eq!(state_of(&rows, "swiss-memory", ".claude"), CopyState::Current);
        assert_eq!(state_of(&rows, "swiss-memory", ".agents"), CopyState::Current);
        assert!(!home.0.join(".claude/skills/swiss").exists());
        assert!(!home.0.join(".agents/skills/swiss-remote").exists());

        let other = TempHome::new();
        let err = other.places().install_named(&names(&["swiss", "nope"])).unwrap_err();
        assert!(err.contains("nope") && err.contains("swiss-memory"), "{err}");
        assert!(!other.0.exists(), "an unknown name refuses the whole call: {err}");
    }

    // SPEC §host.cli: what `swiss skill install --installed` does after an update.
    #[test]
    fn installed_refreshes_and_never_adds() {
        let home = TempHome::new();
        home.plant_old_copy(".agents", "swiss");
        home.plant_old_copy(".claude", "swiss-memory");

        let refresh = home.places().refresh_installed();

        assert_eq!(refresh.replaced.len(), 2, "{refresh:?}");
        assert!(refresh.failed.is_empty(), "{refresh:?}");
        let rows = home.places().status();
        assert_eq!(state_of(&rows, "swiss", ".agents"), CopyState::Current);
        assert_eq!(state_of(&rows, "swiss-memory", ".claude"), CopyState::Current);
        for (name, root) in [
            ("swiss", ".claude"),
            ("swiss-memory", ".agents"),
            ("swiss-remote", ".claude"),
            ("swiss-remote", ".agents"),
        ] {
            assert_eq!(state_of(&rows, name, root), CopyState::Absent, "{name} in {root}");
            assert!(!home.file(root, name).parent().expect("a directory").exists(), "{name} in {root}");
        }

        assert!(home.places().refresh_installed().replaced.is_empty(), "a current copy stays put");
    }

    // SPEC §host.cli: copies an older build left outside the two directories stay as they are.
    #[test]
    fn a_refresh_leaves_a_copy_outside_the_two_directories() {
        let home = TempHome::new();
        let cursor = home.plant_old_copy(".cursor", "swiss");
        let codex = home.plant_old_copy(".codex", "swiss");
        assert!(home.places().refresh_installed().replaced.is_empty());
        assert_eq!(std::fs::read_to_string(cursor).expect("kept"), "an older copy");
        assert_eq!(std::fs::read_to_string(codex).expect("kept"), "an older copy");
    }

    #[test]
    fn one_directory_that_refuses_does_not_stop_the_other() {
        let home = TempHome::new();
        std::fs::create_dir_all(home.file(".claude", "swiss")).expect("a directory where the file goes");
        let other = home.plant_old_copy(".agents", "swiss");

        let refresh = home.places().refresh_installed();

        assert_eq!(refresh.replaced, [other.to_string_lossy().into_owned()]);
        assert_eq!(refresh.failed.len(), 1, "{refresh:?}");
        assert_eq!(std::fs::read_to_string(other).expect("refreshed"), SKILL_MD);
    }

    #[test]
    fn list_says_which_skills_load_by_themselves() {
        let rows = TempHome::new().places().status();
        let loads: Vec<_> = rows.iter().map(|s| (s.name.as_str(), s.loads_by_itself)).collect();
        assert_eq!(loads, [("swiss", false), ("swiss-remote", false), ("swiss-memory", true)]);
        for skill in &rows {
            assert!(!skill.description.is_empty(), "{}", skill.name);
            assert!(!skill.description.starts_with('>'), "{}: the block's text, not its marker", skill.name);
        }
        let memory = rows.iter().find(|s| s.name == "swiss-memory").expect("shipped");
        assert!(memory.description.ends_with("memory_save."), "the whole block: {}", memory.description);
    }

    #[test]
    fn list_marks_a_copy_older_than_this_binary() {
        let home = TempHome::new();
        home.places().install_one("swiss", ".agents").expect("install");
        assert_eq!(state_of(&home.places().status(), "swiss", ".agents"), CopyState::Current);

        std::fs::write(home.file(".agents", "swiss"), "an older copy").expect("age the copy");
        let rows = home.places().status();
        assert_eq!(state_of(&rows, "swiss", ".agents"), CopyState::Outdated);
        assert_eq!(state_of(&rows, "swiss", ".claude"), CopyState::Absent);

        home.places().refresh_installed();
        assert_eq!(state_of(&home.places().status(), "swiss", ".agents"), CopyState::Current);
    }

    #[test]
    fn a_denied_step_is_tried_again_and_another_error_is_not() {
        let mut calls = 0;
        let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let out = retry_busy(|| {
            calls += 1;
            if calls < 3 { Err(denied()) } else { Ok(calls) }
        });
        assert_eq!(out.expect("cleared"), 3);
        let mut calls = 0;
        let out: std::io::Result<()> = retry_busy(|| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!((out.unwrap_err().kind(), calls), (std::io::ErrorKind::NotFound, 1));
        let mut calls = 0;
        let out: std::io::Result<()> = retry_busy(|| {
            calls += 1;
            Err(denied())
        });
        assert_eq!((out.unwrap_err().kind(), calls), (std::io::ErrorKind::PermissionDenied, BUSY_ATTEMPTS));
    }

    /// The case met on ~/.agents/skills: a reader holds the staged SKILL.md open without
    /// delete sharing, so its rename is denied until it lets go.
    #[cfg(windows)]
    #[test]
    fn a_staged_skill_held_open_for_a_moment_still_lands() {
        use std::os::windows::fs::OpenOptionsExt;
        let home = TempHome::new();
        std::fs::create_dir_all(&home.0).expect("home");
        let staged = home.0.join(".SKILL-staged.tmp");
        let target = home.0.join("SKILL.md");
        std::fs::write(&staged, SKILL_MD).expect("write");
        // FILE_SHARE_READ only: no delete sharing, as a scanner opens it.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0x1).open(&staged).expect("hold");
        let denied = std::fs::rename(&staged, &target).expect_err("a held file blocks the rename");
        assert!(is_busy(&denied), "{denied:?}");
        let reader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(held);
        });
        retry_busy(|| std::fs::rename(&staged, &target)).expect("lands once the reader lets go");
        reader.join().expect("reader");
        assert_eq!(std::fs::read_to_string(&target).expect("installed"), SKILL_MD);
    }

    #[test]
    fn the_shipped_skills_have_distinct_names_and_bounded_roles() {
        assert!(SKILL_MD.starts_with("---\nname: swiss\n"));
        assert!(REMOTE_SKILL_MD.starts_with("---\nname: swiss-remote\n"));
        for body in [SKILL_MD, REMOTE_SKILL_MD] {
            assert!(body.contains("disable-model-invocation: true"));
        }
        assert!(SKILL_MD.contains("SETUP.md"));
        assert!(SETUP_MD.contains("SHA256SUMS"));
        assert!(!SKILL_MD.contains("swiss remote exec"));
        assert!(REMOTE_SKILL_MD.contains("swiss remote exec"));
        assert!(!REMOTE_SKILL_MD.contains("SHA256SUMS"));
    }

    // SPEC §host.cli: an update is two steps, and the skill that walks an update says so.
    #[test]
    fn the_swiss_skill_says_how_an_update_reaches_the_skills() {
        assert!(SKILL_MD.contains("SETUP.md"));
        assert!(SKILL_MD.contains("swiss skill list"));
        assert!(SETUP_MD.contains("swiss skill install --installed"));
    }

    #[test]
    fn the_swiss_skill_points_at_setup_md() {
        assert!(SKILL_MD.contains("disable-model-invocation: true"));
        assert!(SKILL_MD.contains("SETUP.md"));
        assert!(SKILL_MD.contains("swiss setup guide"));
        assert!(!loads_by_itself(SKILL_MD));
    }

    #[test]
    fn both_install_paths_write_setup_md_beside_the_skill() {
        let home = TempHome::new();
        // `swiss skill install` and POST /api/skills/install both land here.
        home.places().install_named(&names(&["swiss"])).expect("the cli path");
        home.places().install_one("swiss", ".claude").expect("the route path");
        for root in [".claude", ".agents"] {
            let setup = home.0.join(root).join("skills").join("swiss").join("SETUP.md");
            assert_eq!(std::fs::read_to_string(&setup).expect("SETUP.md"), SETUP_MD);
        }
        home.places().install_one("swiss-remote", ".claude").expect("another skill");
        assert!(!home.0.join(".claude/skills/swiss-remote/SETUP.md").exists());
    }

    #[test]
    fn the_release_carries_setup_md() {
        let workflow = include_str!("../.github/workflows/build.yml");
        assert!(workflow.contains("cp SETUP.md dist-stage/SETUP.md"));
        assert!(workflow.contains("cp SETUP.md dist-stage/skills/swiss/SETUP.md"));
        assert!(workflow.contains("cp SETUP.md dist-bin/SETUP.md"));
        assert!(workflow.contains("SETUP.md > SHA256SUMS"));
    }

    #[test]
    fn the_memory_skill_is_the_one_for_reading_and_writing_and_names_every_tool() {
        assert!(MEMORY_SKILL_MD.starts_with("---\nname: swiss-memory\n"));
        for tool in swiss_plugins::memory_mcp::TOOLS {
            assert!(
                MEMORY_SKILL_MD.contains(&format!("| `{tool}` |")),
                "the skill's tool table misses {tool}"
            );
        }
        for command in ["swiss memory ask", "swiss memory docs search", "swiss memory code find", "swiss memory save"] {
            assert!(MEMORY_SKILL_MD.contains(command), "the skill misses {command}");
        }
    }

    /// The installed skill carries what the save and the brief now say (SPEC §memory.capture,
    /// §memory.brief): a preference in the user's dated words, its `user_words` hint, the
    /// contradiction hint and the rejected precedent.
    #[test]
    fn the_memory_skill_asks_for_the_user_s_words_and_reads_the_history() {
        for wanted in [
            "quoted letter for letter and dated",
            "- stated by the user, 2026-10-21",
            "`user_words`",
            "**Contradictions:**",
            "**Rejected before:**",
        ] {
            assert!(MEMORY_SKILL_MD.contains(wanted), "the skill misses {wanted:?}");
        }
        assert!(!MEMORY_SKILL_MD.contains("The content says \"stated by the user\"."), "the old paraphrase rule is gone");
    }
}
