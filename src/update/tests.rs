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

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;

use super::apply::{apply, ApplyEnv, Fetch, Gateway};
use super::manifest::{self, Choice};
use super::patch::{self, sha256_bytes};
use super::replace::{self, SwapIo};

struct MemFetch {
    hits: AtomicUsize,
    manifest: Vec<u8>,
    asset: Vec<u8>,
}

#[async_trait]
impl Fetch for MemFetch {
    async fn manifest(&self) -> Result<Vec<u8>, String> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(self.manifest.clone())
    }
    async fn asset(&self, _tag: &str, _name: &str, sha: &str, dest: &Path) -> Result<u64, String> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        if sha256_bytes(&self.asset) != sha {
            return Err("test asset sha mismatch".to_string());
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::write(dest, &self.asset).map_err(|err| err.to_string())?;
        Ok(self.asset.len() as u64)
    }
}

struct FakeGate {
    entry: Mutex<Option<String>>,
    starts: Mutex<Vec<PathBuf>>,
    health_ok: Mutex<bool>,
    skills_ok: Mutex<bool>,
    /// "stop" and "start" in call order.
    events: Mutex<Vec<String>>,
    /// How many of the first starts fail.
    start_fails: Mutex<usize>,
}

#[async_trait]
impl Gateway for FakeGate {
    fn record_entry(&self, _port: u16) -> Option<String> {
        self.entry.lock().unwrap().clone()
    }
    async fn stop(&self, _port: u16) -> Result<(), String> {
        self.events.lock().unwrap().push("stop".to_string());
        Ok(())
    }
    async fn start(&self, exe: &Path, _port: u16) -> Result<(), String> {
        self.starts.lock().unwrap().push(exe.to_path_buf());
        self.events.lock().unwrap().push("start".to_string());
        let mut fails = self.start_fails.lock().unwrap();
        if *fails > 0 {
            *fails -= 1;
            return Err("gateway did not start: test".to_string());
        }
        Ok(())
    }
    async fn health_hash(&self, _port: u16) -> Result<String, String> {
        if *self.health_ok.lock().unwrap() {
            Ok("abc123".to_string())
        } else {
            Err("health failed".to_string())
        }
    }
    fn probe(&self, _exe: &Path) -> Result<String, String> {
        Ok("swiss 9.9.9 (abc123, 2026-11-20T10:00:00Z)\n".to_string())
    }
    fn refresh_skills(&self, _exe: &Path) -> Result<String, String> {
        if *self.skills_ok.lock().unwrap() {
            Ok("skills: 1 copies refreshed in 1 seats".to_string())
        } else {
            Err("warning: skill refresh exited 1 - the update itself succeeded".to_string())
        }
    }
}

fn env_at(dir: &Path, exe: &Path, channel: &str, force: bool) -> ApplyEnv {
    ApplyEnv {
        exe: exe.to_path_buf(),
        channel: channel.to_string(),
        version: "0.2.2".to_string(),
        hash: "oldhash".to_string(),
        target: "test-target".to_string(),
        port: 19998,
        data_dir: dir.join("home"),
        force,
        by: "cli".to_string(),
    }
}

fn manifest_for(exe_sha: &str, asset_sha: &str, asset_size: u64) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "version": "9.9.9",
        "tag": "v9.9.9",
        "notes": "https://example.invalid/v9.9.9",
        "ignored": true,
        "targets": {
            "test-target": {
                "exe": "swiss.exe",
                "sha256": exe_sha,
                "size": 4,
                "full": { "asset": "full.zst", "sha256": asset_sha, "size": asset_size },
                "patches": []
            }
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn a_dev_build_makes_zero_fetch_calls_without_force() {
    let dir = std::env::temp_dir().join(format!("swiss-upd-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"old").unwrap();
    let fetch = MemFetch { hits: AtomicUsize::new(0), manifest: Vec::new(), asset: Vec::new() };
    let gate = FakeGate {
        entry: Mutex::new(None),
        starts: Mutex::new(Vec::new()),
        health_ok: Mutex::new(false),
        skills_ok: Mutex::new(true),
        events: Mutex::new(Vec::new()),
        start_fails: Mutex::new(0),
    };
    let outcome = apply(&env_at(&dir, &exe, "dev", false), &fetch, &gate).await;
    assert_eq!(outcome.code, 2);
    assert_eq!(fetch.hits.load(Ordering::SeqCst), 0);
    assert!(outcome.lines[0].contains("channel dev"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_failed_health_check_rolls_the_exe_back() {
    let dir = std::env::temp_dir().join(format!("swiss-upd-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join("swiss.exe");
    let old = b"old-binary-contents";
    let new = b"new-binary-contents-v2";
    std::fs::write(&exe, old).unwrap();
    let packed = zstd::encode_all(&new[..], 3).unwrap();
    let fetch = MemFetch {
        hits: AtomicUsize::new(0),
        manifest: manifest_for(&sha256_bytes(new), &sha256_bytes(&packed), packed.len() as u64),
        asset: packed,
    };
    let gate = FakeGate {
        entry: Mutex::new(Some(exe.display().to_string())),
        starts: Mutex::new(Vec::new()),
        health_ok: Mutex::new(false),
        skills_ok: Mutex::new(true),
        events: Mutex::new(Vec::new()),
        start_fails: Mutex::new(0),
    };
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &gate).await;
    assert_eq!(outcome.code, 1, "{:?}", outcome.lines);
    assert!(outcome.rolled_back, "{:?}", outcome.lines);
    assert_eq!(std::fs::read(&exe).unwrap(), old);
    assert!(!replace::sibling(&exe, ".new").exists());
    let starts = gate.starts.lock().unwrap().clone();
    assert!(starts.iter().all(|path| path == &exe), "{starts:?}");
    assert!(starts.len() >= 2, "{starts:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_failed_skill_refresh_still_exits_zero() {
    let dir = std::env::temp_dir().join(format!("swiss-upd-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join("swiss.exe");
    let old = b"old-binary-contents";
    let new = b"new-binary-contents-v2";
    std::fs::write(&exe, old).unwrap();
    let packed = zstd::encode_all(&new[..], 3).unwrap();
    let fetch = MemFetch {
        hits: AtomicUsize::new(0),
        manifest: manifest_for(&sha256_bytes(new), &sha256_bytes(&packed), packed.len() as u64),
        asset: packed,
    };
    let gate = FakeGate {
        entry: Mutex::new(Some(exe.display().to_string())),
        starts: Mutex::new(Vec::new()),
        health_ok: Mutex::new(true),
        skills_ok: Mutex::new(false),
        events: Mutex::new(Vec::new()),
        start_fails: Mutex::new(0),
    };
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &gate).await;
    assert_eq!(outcome.code, 0, "{:?}", outcome.lines);
    assert!(outcome.lines.iter().any(|line| line.contains("warning: skill refresh")));
    assert_eq!(std::fs::read(&exe).unwrap(), new);
    assert_eq!(gate.starts.lock().unwrap().as_slice(), &[exe.clone()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_selection_matches_a_patch_or_falls_back_to_full() {
    let body = r#"{"schema":1,"version":"0.2.3","tag":"v0.2.3","extra":1,"targets":{
        "t":{"exe":"swiss","sha256":"ee","size":1,
             "full":{"asset":"full.zst","sha256":"ff","size":10},
             "patches":[{"from":"0.2.2","fromSha256":"aa","asset":"p.zst","sha256":"pp","size":2,"windowLog":27}]}
    }}"#;
    match manifest::select(body.as_bytes(), "t", "aa", "0.2.2", false).unwrap() {
        Choice::Install(plan) => assert!(matches!(plan.via, manifest::Via::Patch { .. })),
        other => panic!("{other:?}"),
    }
    match manifest::select(body.as_bytes(), "t", "nope", "0.2.2", false).unwrap() {
        Choice::Install(plan) => assert!(matches!(plan.via, manifest::Via::Full { .. })),
        other => panic!("{other:?}"),
    }
    assert!(matches!(manifest::select(body.as_bytes(), "missing", "aa", "0.2.2", false).unwrap(), Choice::UnknownTarget));
    assert!(matches!(manifest::select(body.as_bytes(), "t", "aa", "0.2.3", false).unwrap(), Choice::AlreadyNewest));
    assert!(matches!(manifest::select(body.as_bytes(), "t", "aa", "0.2.3", true).unwrap(), Choice::Install(_)));
    assert!(matches!(manifest::select(body.as_bytes(), "t", "aa", "0.2.4", true).unwrap(), Choice::Older));
    let higher = body.replacen("\"schema\":1", "\"schema\":2", 1);
    assert!(matches!(manifest::select(higher.as_bytes(), "t", "aa", "0.2.2", false).unwrap(), Choice::SchemaTooNew));
}

// The three loopback spellings with a port are honoured; anything else is refused. The parse is
// tested directly so no test mutates the process environment under parallel runs.
#[test]
fn a_non_loopback_update_source_is_refused() {
    for refused in [
        "https://example.invalid/updates/",
        "http://example.invalid:8080/",
        "https://127.0.0.1:8080/",
        "http://127.0.0.1/",
        "not a url",
    ] {
        let err = super::fetch::loopback_base(refused).unwrap_err();
        assert!(err.contains("SWISS_UPDATE_SOURCE must be a loopback URL"), "{refused}: {err}");
    }
    for (given, base) in [
        ("http://127.0.0.1:8123", "http://127.0.0.1:8123/"),
        ("http://localhost:8123/rel?x=1", "http://localhost:8123/rel/"),
        ("http://[::1]:8123/", "http://[::1]:8123/"),
    ] {
        assert_eq!(super::fetch::loopback_base(given).unwrap(), base);
    }
}

#[test]
fn version_line_parses_the_number_and_the_hash() {
    let line = crate::cli::version_line();
    assert_eq!(crate::daemon::parse_version_number(&line), Some(env!("CARGO_PKG_VERSION")));
    assert!(crate::daemon::parse_version_hash(&line).is_some());
}

#[test]
fn the_cli_patch_fixture_decodes_to_the_new_bytes() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/update");
    let old = std::fs::read(dir.join("old.bin")).unwrap();
    let new = std::fs::read(dir.join("new.bin")).unwrap();
    let out = std::env::temp_dir().join(format!("swiss-fixture-{}", swiss_core::util::random_hex(6)));
    let how = patch::Packed::Patch { old: &old, window_log: 27 };
    patch::decode_to_file(&dir.join("p.patch.zst"), how, &out, &sha256_bytes(&new)).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), new);
    let _ = std::fs::remove_file(&out);
}

// ADR-041 step 8: a start that failed may have left its child holding the new exe, so the
// updater stops it before swapping back, then starts the previous exe.
#[tokio::test]
async fn a_failed_start_stops_then_rolls_back() {
    let dir = std::env::temp_dir().join(format!("swiss-upd-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join("swiss.exe");
    let old = b"old-binary-contents";
    let new = b"new-binary-contents-v2";
    std::fs::write(&exe, old).unwrap();
    let packed = zstd::encode_all(&new[..], 3).unwrap();
    let fetch = MemFetch {
        hits: AtomicUsize::new(0),
        manifest: manifest_for(&sha256_bytes(new), &sha256_bytes(&packed), packed.len() as u64),
        asset: packed,
    };
    let gate = fresh_gate(Some(exe.display().to_string()));
    *gate.start_fails.lock().unwrap() = 1;
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &gate).await;
    assert_eq!(outcome.code, 1, "{:?}", outcome.lines);
    assert!(outcome.rolled_back, "{:?}", outcome.lines);
    assert_eq!(std::fs::read(&exe).unwrap(), old);
    assert_eq!(gate.events.lock().unwrap().as_slice(), ["stop", "start", "stop", "start"]);
    let last: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("home").join("update").join("last.json")).unwrap()).unwrap();
    assert_eq!(last["phase"], json!("start"));
    assert_eq!(last["rolledBack"], json!(true));
    let _ = std::fs::remove_dir_all(&dir);
}

// The patch path: the manifest's patch matches the local exe's hash, the download is the
// patch, and .new is the decode of it against the old exe.
#[tokio::test]
async fn a_matching_patch_is_applied_and_last_json_says_patch() {
    let dir = std::env::temp_dir().join(format!("swiss-upd-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join("swiss.exe");
    let old = b"old-binary-contents".repeat(4096);
    let mut new = old.clone();
    new.extend_from_slice(b"and a new tail");
    std::fs::write(&exe, &old).unwrap();
    let patch_bytes = patch::encode_patch(&old, &new).unwrap();
    let full = zstd::encode_all(&new[..], 3).unwrap();
    let manifest = serde_json::to_vec(&json!({
        "schema": 1, "version": "9.9.9", "tag": "v9.9.9",
        "targets": { "test-target": {
            "exe": "swiss.exe", "sha256": sha256_bytes(&new), "size": new.len(),
            "full": { "asset": "full.zst", "sha256": sha256_bytes(&full), "size": full.len() },
            "patches": [{ "from": "0.2.2", "fromSha256": sha256_bytes(&old), "asset": "p.patch.zst",
                          "sha256": sha256_bytes(&patch_bytes), "size": patch_bytes.len(), "windowLog": 27 }]
        }}
    }))
    .unwrap();
    let fetch = MemFetch { hits: AtomicUsize::new(0), manifest, asset: patch_bytes };
    let gate = fresh_gate(None);
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &gate).await;
    assert_eq!(outcome.code, 0, "{:?}", outcome.lines);
    assert_eq!(std::fs::read(&exe).unwrap(), new);
    assert!(outcome.lines.iter().any(|line| line.starts_with("download: patch from 0.2.2")), "{:?}", outcome.lines);
    assert!(outcome.lines.iter().any(|line| line == "gateway: not running, left stopped"), "{:?}", outcome.lines);
    assert!(gate.events.lock().unwrap().is_empty(), "a stopped gateway is neither stopped nor started");
    let last: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("home").join("update").join("last.json")).unwrap()).unwrap();
    assert_eq!(last["via"], json!("patch"));
    assert_eq!(last["ok"], json!(true));
    assert!(std::fs::read_dir(dir.join("home").join("update")).unwrap().all(|entry| {
        !entry.unwrap().file_name().to_string_lossy().ends_with(".part")
    }), "the download is deleted");
    let _ = std::fs::remove_dir_all(&dir);
}

// A wrong exe hash in the manifest: .new is removed and nothing is renamed.
#[tokio::test]
async fn a_wrong_exe_sha_renames_nothing() {
    let dir = std::env::temp_dir().join(format!("swiss-upd-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join("swiss.exe");
    std::fs::write(&exe, b"old").unwrap();
    let packed = zstd::encode_all(&b"new"[..], 3).unwrap();
    let fetch = MemFetch {
        hits: AtomicUsize::new(0),
        manifest: manifest_for(&"0".repeat(64), &sha256_bytes(&packed), packed.len() as u64),
        asset: packed,
    };
    let gate = fresh_gate(Some(exe.display().to_string()));
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &gate).await;
    assert_eq!(outcome.code, 1, "{:?}", outcome.lines);
    assert!(outcome.lines.iter().any(|line| line.contains("sha256 mismatch")), "{:?}", outcome.lines);
    assert_eq!(std::fs::read(&exe).unwrap(), b"old");
    assert!(!replace::sibling(&exe, ".new").exists());
    assert!(gate.events.lock().unwrap().is_empty(), "the gateway was never stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

struct ScriptedFs {
    fail_on: usize,
    calls: usize,
    removed: Vec<PathBuf>,
    renames: Vec<(PathBuf, PathBuf)>,
    old_exists: bool,
    remove_kind: io::ErrorKind,
}

impl SwapIo for ScriptedFs {
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.calls += 1;
        self.renames.push((from.to_path_buf(), to.to_path_buf()));
        if self.calls == self.fail_on {
            return Err(io::Error::other(format!("fail {} -> {}", from.display(), to.display())));
        }
        Ok(())
    }
    fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        self.removed.push(path.to_path_buf());
        Err(io::Error::new(self.remove_kind, "absent"))
    }
    fn exists(&mut self, path: &Path) -> bool {
        path.to_string_lossy().ends_with(".old") && self.old_exists
    }
}

fn scripted(fail_on: usize) -> ScriptedFs {
    ScriptedFs {
        fail_on,
        calls: 0,
        removed: Vec::new(),
        renames: Vec::new(),
        old_exists: true,
        remove_kind: io::ErrorKind::NotFound,
    }
}

#[test]
fn the_first_rename_failure_stops_and_the_second_rolls_back() {
    let exe = PathBuf::from("bin/swiss.exe");
    let mut first = scripted(1);
    let err = replace::swap(&exe, &mut first).unwrap_err();
    assert!(err.contains("cannot rename"), "{err}");
    assert_eq!(first.calls, 1);
    let mut second = scripted(2);
    let err = replace::swap(&exe, &mut second).unwrap_err();
    assert!(err.contains("cannot rename"), "{err}");
    assert_eq!(second.calls, 3, "second failure plus the rollback rename");
}

#[test]
fn boot_cleanup_removes_old_siblings() {
    let dir = std::env::temp_dir().join(format!("swiss-old-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"exe").unwrap();
    std::fs::write(dir.join("swiss.exe.old"), b"old").unwrap();
    std::fs::write(dir.join("swiss.exe.old.ab"), b"aside").unwrap();
    std::fs::write(dir.join("keep.txt"), b"keep").unwrap();
    replace::cleanup_old_siblings(&exe);
    assert!(!dir.join("swiss.exe.old").exists());
    assert!(!dir.join("swiss.exe.old.ab").exists());
    assert!(dir.join("keep.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_refuses_dev_and_another_exe_and_status_reads_last_json() {
    let dir = std::env::temp_dir().join(format!("swiss-pre-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("home").join("update")).unwrap();
    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"exe").unwrap();
    let dev = env_at(&dir, &exe, "dev", false);
    let err = super::apply::preflight(&dev, None).unwrap_err();
    assert_eq!(err.code, "update.devBuild");
    let forced = env_at(&dir, &exe, "dev", true);
    let err = super::apply::preflight(&forced, Some(Path::new("other.exe"))).unwrap_err();
    assert_eq!(err.code, "update.otherExe");
    assert_eq!(err.status, 409);
    assert!(super::apply::preflight(&forced, None).is_ok());
    std::fs::write(dir.join("home").join("update").join("last.json"), br#"{"ok":true}"#).unwrap();
    let body = super::apply::status_body(&dir.join("home"), "release", None, false);
    assert_eq!(body["last"]["ok"], json!(true));
    assert_eq!(body["running"], json!(false));
    let missing = super::apply::status_body(&dir.join("absent"), "release", None, false);
    assert!(missing["last"].is_null());
    assert_eq!(body["canApply"], json!(true));
    assert!(body["refusal"].is_null());
    let running = super::apply::status_body(&dir.join("home"), "dev", Some("update.devBuild"), true);
    assert_eq!(running["running"], json!(true));
    assert_eq!(running["canApply"], json!(false));
    assert_eq!(running["refusal"], json!("update.devBuild"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn fresh_gate(entry: Option<String>) -> FakeGate {
    FakeGate {
        entry: Mutex::new(entry),
        starts: Mutex::new(Vec::new()),
        health_ok: Mutex::new(true),
        skills_ok: Mutex::new(true),
        events: Mutex::new(Vec::new()),
        start_fails: Mutex::new(0),
    }
}

fn empty_fetch() -> MemFetch {
    MemFetch { hits: AtomicUsize::new(0), manifest: Vec::new(), asset: Vec::new() }
}

fn hold_process() -> std::process::Child {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("ping");
        command.args(["-n", "30", "127.0.0.1"]);
        command
    } else {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a process whose pid can hold update.lock")
}

#[tokio::test]
async fn a_bundle_a_readonly_dir_a_held_lock_and_another_exe_exit_2() {
    let dir = std::env::temp_dir().join(format!("swiss-refuse-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let bundle = dir.join("Demo.app").join("Contents").join("MacOS").join("swiss");
    std::fs::create_dir_all(bundle.parent().unwrap()).unwrap();
    std::fs::write(&bundle, b"app").unwrap();
    let fetch = empty_fetch();
    let outcome = apply(&env_at(&dir, &bundle, "release", false), &fetch, &fresh_gate(None)).await;
    assert_eq!(outcome.code, 2, "{:?}", outcome.lines);
    assert_eq!(fetch.hits.load(Ordering::SeqCst), 0);
    assert!(outcome.lines.iter().any(|line| line.contains("app bundle")), "{:?}", outcome.lines);

    std::fs::write(dir.join("not-a-dir"), b"x").unwrap();
    let blocked = dir.join("not-a-dir").join("swiss.exe");
    let fetch = empty_fetch();
    let outcome = apply(&env_at(&dir, &blocked, "release", false), &fetch, &fresh_gate(None)).await;
    assert_eq!(outcome.code, 2, "{:?}", outcome.lines);
    assert_eq!(fetch.hits.load(Ordering::SeqCst), 0);
    assert!(outcome.lines.iter().any(|line| line.contains("not writable")), "{:?}", outcome.lines);

    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"exe").unwrap();
    let mut child = hold_process();
    std::fs::create_dir_all(dir.join("home").join("update")).unwrap();
    std::fs::write(dir.join("home").join("update").join("update.lock"), child.id().to_string()).unwrap();
    let fetch = empty_fetch();
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &fresh_gate(None)).await;
    assert_eq!(outcome.code, 2, "{:?}", outcome.lines);
    assert_eq!(fetch.hits.load(Ordering::SeqCst), 0);
    assert!(outcome.lines.iter().any(|line| line.contains("update.lock")), "{:?}", outcome.lines);
    let _ = child.kill();
    let _ = child.wait();

    let fetch = empty_fetch();
    let outcome = apply(
        &env_at(&dir, &exe, "release", false),
        &fetch,
        &fresh_gate(Some("D:/other/swiss.exe".into())),
    )
    .await;
    assert_eq!(outcome.code, 2, "{:?}", outcome.lines);
    assert_eq!(fetch.hits.load(Ordering::SeqCst), 0);
    assert!(outcome.lines.iter().any(|line| line.contains("another exe")), "{:?}", outcome.lines);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_unreadable_lock_pid_is_stale_and_the_fetch_runs() {
    let dir = std::env::temp_dir().join(format!("swiss-stale-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("home").join("update")).unwrap();
    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"exe").unwrap();
    std::fs::write(dir.join("home").join("update").join("update.lock"), b"not-a-pid").unwrap();
    let fetch = empty_fetch();
    let outcome = apply(&env_at(&dir, &exe, "release", false), &fetch, &fresh_gate(None)).await;
    assert!(fetch.hits.load(Ordering::SeqCst) >= 1, "a stale lock must not skip the manifest");
    assert!(outcome.lines.iter().all(|line| !line.contains("update.lock")), "{:?}", outcome.lines);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn swap_replaces_the_exe_and_rollback_leaves_it_when_old_is_missing() {
    let dir = std::env::temp_dir().join(format!("swiss-swap-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"old").unwrap();
    std::fs::write(replace::sibling(&exe, ".new"), b"new").unwrap();
    replace::swap(&exe, &mut replace::RealFs).unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    assert_eq!(std::fs::read(replace::sibling(&exe, ".old")).unwrap(), b"old");

    let bare = dir.join("bare.exe");
    std::fs::write(&bare, b"stay").unwrap();
    let err = replace::rollback(&bare, &mut replace::RealFs).unwrap_err();
    assert!(err.contains("found no"), "{err}");
    assert_eq!(std::fs::read(&bare).unwrap(), b"stay");
    assert!(!replace::sibling(&bare, ".new").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_old_that_cannot_be_deleted_is_parked_aside() {
    let exe = PathBuf::from("bin/swiss.exe");
    let mut fs = scripted(usize::MAX);
    fs.remove_kind = io::ErrorKind::PermissionDenied;
    replace::swap(&exe, &mut fs).unwrap();
    assert!(
        fs.renames.iter().any(|(_, to)| to.to_string_lossy().contains(".old.")),
        "{:?}",
        fs.renames
    );
}

#[test]
fn boot_cleanup_skips_a_held_lock_and_an_unreadable_pid_does_not() {
    let dir = std::env::temp_dir().join(format!("swiss-boot-{}", swiss_core::util::random_hex(6)));
    let home = dir.join("home");
    std::fs::create_dir_all(home.join("update")).unwrap();
    let exe = dir.join("swiss.exe");
    std::fs::write(&exe, b"exe").unwrap();
    std::fs::write(dir.join("swiss.exe.old"), b"old").unwrap();
    std::fs::write(home.join("update").join("update.lock"), b"not-a-pid").unwrap();
    replace::cleanup_after_boot(&exe, &home);
    assert!(!dir.join("swiss.exe.old").exists(), "an unreadable pid is a stale lock");

    std::fs::write(dir.join("swiss.exe.old"), b"old").unwrap();
    let mut child = hold_process();
    std::fs::write(home.join("update").join("update.lock"), child.id().to_string()).unwrap();
    replace::cleanup_after_boot(&exe, &home);
    assert!(dir.join("swiss.exe.old").exists(), "a live lock keeps the parked exe");
    let _ = child.kill();
    let _ = child.wait();
    replace::cleanup_after_boot(&exe, &home);
    assert!(!dir.join("swiss.exe.old").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
