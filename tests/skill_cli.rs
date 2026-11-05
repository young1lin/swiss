/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

//! `swiss skill` as the real binary, on a home of the test's own and with no terminal on stdin,
//! which is how an Agent runs it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "swiss-skill-cli-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&base).expect("a home");
        Home(base)
    }

    fn file(&self, root: &str, name: &str) -> PathBuf {
        self.0.join(root).join("skills").join(name).join("SKILL.md")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn swiss(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_swiss"))
        .arg("skill")
        .args(args)
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env("SWISS_HOME", home.join("swiss-home"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run swiss")
}

fn listing(home: &Path) -> Value {
    let out = swiss(home, &["list", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("one JSON document")
}

/// SPEC §host.cli: choose by name, an update refreshes what is there, and what the user kept
/// beside a skill stays.
#[test]
fn an_agent_installs_what_the_user_chose_and_refreshes_it_after_an_update() {
    let home = Home::new();

    let bare = swiss(&home.0, &["install"]);
    assert_eq!(bare.status.code(), Some(2), "{}", String::from_utf8_lossy(&bare.stderr));
    assert!(bare.stdout.is_empty());
    assert!(String::from_utf8_lossy(&bare.stderr).contains("swiss-memory"));
    assert!(!home.0.join(".claude").exists() && !home.0.join(".agents").exists());

    let named = swiss(&home.0, &["install", "swiss-memory"]);
    assert!(named.status.success(), "{}", String::from_utf8_lossy(&named.stderr));
    assert!(home.file(".claude", "swiss-memory").is_file());
    assert!(home.file(".agents", "swiss-memory").is_file());
    assert!(!home.file(".agents", "swiss").exists());

    let beside = home.0.join(".agents/skills/swiss-memory/notes.txt");
    std::fs::write(&beside, "keep me").expect("a file of the user's");
    std::fs::write(home.file(".agents", "swiss-memory"), "an older copy").expect("age the copy");
    let memory = |doc: &Value, at: usize| doc["skills"][2]["locations"][at]["state"].clone();
    assert_eq!(memory(&listing(&home.0), 1), "outdated");

    let refresh = swiss(&home.0, &["install", "--installed"]);
    assert!(refresh.status.success(), "{}", String::from_utf8_lossy(&refresh.stderr));
    let after = listing(&home.0);
    assert_eq!(memory(&after, 0), "current");
    assert_eq!(memory(&after, 1), "current");
    assert_eq!(after["skills"][0]["locations"][1]["state"], "absent", "a refresh adds nothing");
    assert_eq!(std::fs::read_to_string(&beside).expect("kept"), "keep me");
}
