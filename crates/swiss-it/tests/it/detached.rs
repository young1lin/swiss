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

//! L4: the detached spawn against real processes (SPEC §process.detached, §process.verify).
//!
//! Every helper is the `it-app` binary in a role, and every helper a test starts is
//! killed by a drop guard - these tests may never leave a process behind, because a
//! leaked `flood` sleeps for a minute in a temp directory nothing cleans.
//!
//! Two structural answers back that up: an ownership read accepts only an it-app pid
//! (this binary's hundred-plus sibling tests bind and drop ephemeral ports around us,
//! and a same-numbered foreign listener is a collision to retry on a fresh port,
//! never a false owner), and the lock's guard sweeps every it-app on its way out -
//! panic or not - so one failing test cannot leave the live serve that fails every
//! later test's opening precondition.
//!
//! What this group pins, on Windows unless noted:
//!
//! - breakaway: a child spawned by a process inside a kill-on-close job ESCAPES the
//!   job, so closing the job handle (the "IDE died" shape) leaves the app alive;
//! - setsid: on Unix the child is its own session, so a terminal hangup cannot reach
//!   it (the /proc fields are read directly, not inferred);
//! - no pipes: a megabyte of output flows to the capture file while the writer keeps
//!   living - nobody ever reads what the app writes;
//! - no inherited log: the spawner's own log file can be renamed while the app runs,
//!   which is only true if the app never held it open;
//! - port ownership: a launcher that exits 0 leaves a listener whose owner is the
//!   child, found through the real listener table;
//! - identity: the creation time is stable for a live pid and absent for a dead one;
//! - the end: tree_kill ends a detached tree and reports no survivors.

use std::path::PathBuf;
use std::time::Duration;

use swiss_core::platform::{process_created_at, tcp_listeners, tree_kill};
use swiss_host::services::detached::{spawn_detached, DetachedChild, DetachedSpec};

/// The helper binary, cargo-guaranteed for a same-package bin.
const IT_APP: &str = env!("CARGO_BIN_EXE_it-app");

/// The detached group spawns it-app processes and asserts on their COUNT; two tests
/// running at once would read each other's. One lock, held for the whole test body by
/// DetachedGuard, which mops on release.
static DETACHED: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// DETACHED held through the test body with the mop armed: a panicking test's own drop
/// guards kill only the pids they know - the launcher's pid, the handle's child - which
/// stops covering the process tree once a launcher handed a serve off. The sweep runs
/// while the lock is still held, and nothing outside this module starts an it-app, so
/// it cannot touch another test's process.
struct DetachedGuard(Option<tokio::sync::MutexGuard<'static, ()>>);

impl DetachedGuard {
    /// Take the module's lock; whatever happens next, release means a world with no
    /// it-app left in it.
    async fn enter() -> Self {
        Self(Some(DETACHED.lock().await))
    }
}

impl Drop for DetachedGuard {
    fn drop(&mut self) {
        mop_all_it_apps();
        // Release the lock only after the mop: the sweep owns the world it cleans.
        drop(self.0.take());
    }
}

/// A port nothing holds right now: bind :0, read the port, drop the listener. The
/// caller re-binds it within milliseconds through the helper it spawns - and a sibling
/// test's own bind can steal that window, which the port-bearing starter below answers
/// with a fresh port, not a false owner.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|addr| addr.port())
        .unwrap_or_else(|err| panic!("could not pick a free port: {err}"))
}

/// A capture file per spawn, under the temp dir, named for the test that asked.
fn capture(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "swiss-it-detached-{tag}-{}.log",
        std::process::id()
    ))
}

/// Start one it-app role detached, exactly the way a managed app start does.
async fn start_role(tag: &str, args: &[&str]) -> DetachedChild {
    let spec = DetachedSpec {
        program: IT_APP.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        cwd: None,
        env: Vec::new(),
        capture: capture(tag),
    };
    spawn_detached(&spec)
        .await
        .unwrap_or_else(|err| panic!("{tag}: the spawn failed: {err}"))
}

/// Start a port-bearing role (`serve`, or `launcher` for the handed-off tree) on a port
/// that is still OURS when it listens. free_port's bind→drop→rebind window can be
/// stolen by a sibling test's listener; a stolen port kills serve on EADDRINUSE while
/// the listener table shows a foreign owner. That is not a verdict on the spawn seam -
/// it is a port collision, answered with a fresh port, up to three times.
async fn start_ported(tag: &str, role: &str) -> (DetachedChild, u16) {
    for _ in 0..3 {
        let port = free_port();
        let mut child = start_role(tag, &[role, &port.to_string()]).await;
        if wait_listening(port).await.is_some() {
            return (child, port);
        }
        // Dead on a stolen bind, or never listening: either way not the witness.
        let _ = tree_kill(child.pid);
        let _ = child.child.start_kill();
    }
    panic!("{tag}: no port stayed ours through three attempts");
}

/// The drop guard every helper is started under: tree-kill first (the tree, not the
/// direct child), then the handle's own kill as the belt to the braces.
struct KillOnDrop(Option<DetachedChild>);

impl KillOnDrop {
    fn leash(child: DetachedChild) -> Self {
        KillOnDrop(Some(child))
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = tree_kill(child.pid);
            let _ = child.child.start_kill();
        }
    }
}

/// The bare-pid belt when the handle belongs to a process we did not spawn (a
/// launcher's child): tree-kill on the way out, nothing else.
struct PidKillOnDrop(u32);

impl Drop for PidKillOnDrop {
    fn drop(&mut self) {
        let _ = tree_kill(self.0);
    }
}

/// Poll f every 100 ms until it answers Some or the budget runs out.
async fn wait_for<T>(ms: u64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(found) = f() {
            return Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The pid of OUR helper listening on the port, straight from the platform table: the
/// row must both match the port and name an it-app process. A same-numbered listener
/// of another test reads as nobody - a stolen port is a collision to retry on a fresh
/// port, never a false owner.
fn our_listener(port: u16) -> Option<u32> {
    tcp_listeners()
        .ok()?
        .into_iter()
        .find(|(p, pid)| *p == port && is_it_app(*pid))
        .map(|(_, pid)| pid)
}

/// Wait until our helper owns a LISTEN on the port and hand its pid over.
async fn wait_listening(port: u16) -> Option<u32> {
    wait_for(10_000, || our_listener(port)).await
}

/// One snapshot of the it-app pids alive right now; None when the process table itself
/// could not be read.
fn it_app_snapshot() -> Option<Vec<u32>> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut seen = Vec::new();
            let mut ok = Process32FirstW(snap, &mut entry);
            while ok != 0 {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                if name.eq_ignore_ascii_case("it-app.exe") {
                    seen.push(entry.th32ProcessID);
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
            Some(seen)
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::read_dir("/proc")
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit())
                    })
                    .filter_map(|e| {
                        let pid = e.file_name().to_string_lossy().parse::<u32>().ok()?;
                        std::fs::read_to_string(e.path().join("cmdline"))
                            .ok()
                            .filter(|cmd| cmd.contains("it-app"))
                            .map(|_| pid)
                    })
                    .collect()
            })
    }
}

/// How many it-app processes are alive right now. Failure reads as a huge count,
/// never zero: "cannot tell" must not fake "gone".
fn it_app_count() -> usize {
    it_app_snapshot().map_or(usize::MAX, |pids| pids.len())
}

/// The pids behind the count, for the mop; a table that cannot be read mops nothing -
/// the count's contract above keeps that from faking "gone" where it is asserted.
fn it_app_pids() -> Vec<u32> {
    it_app_snapshot().unwrap_or_default()
}

/// Whether a pid is one of ours - the helper binary, by its own name or cmdline. The
/// ownership answers accept only such a pid: a row that names anything else (or a pid
/// whose /proc cannot be read, another user's listener) is nobody's witness.
fn is_it_app(pid: u32) -> bool {
    #[cfg(windows)]
    {
        it_app_pids().contains(&pid)
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .map(|cmd| cmd.contains("it-app"))
            .unwrap_or(false)
    }
}

/// Kill every it-app alive - the panicking path's answer to the module's own rule that
/// these tests never leave a process behind. Runs under the DETACHED lock, and nothing
/// outside this module starts an it-app, so the sweep is exactly the module's own.
fn mop_all_it_apps() {
    for pid in it_app_pids() {
        let _ = tree_kill(pid);
    }
}

/// A leftover from an earlier panicked round must not fake a survivor here.
async fn wait_no_it_app() {
    let _ = wait_for(15_000, || (it_app_count() == 0).then_some(())).await;
    assert_eq!(it_app_count(), 0, "it-app processes survived their test");
}

#[tokio::test]
async fn a_detached_child_survives_its_spawners_kill_on_close_job() {
    #[cfg(windows)]
    {
        let _guard = DetachedGuard::enter().await;
        wait_no_it_app().await;
        let port = free_port();
        // The spawner is a REAL launcher process placed in a kill-on-close job, the
        // "started from an IDE" shape: the job dies with its handle, and the app must
        // not be in it. BREAKAWAY_OK is the honest variant - the escape must come from
        // the spawn's own flag, not from a job that forbade it and an error 5 retry.
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };
            use windows_sys::Win32::System::Threading::{
                CreateProcessW, ResumeThread, WaitForSingleObject, CREATE_NO_WINDOW,
                CREATE_SUSPENDED, PROCESS_INFORMATION, STARTUPINFOW,
            };

            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            assert_ne!(job, std::ptr::null_mut(), "could not create the job");
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            assert_eq!(set, 1, "could not set the job limits");

            // Launcher, born suspended so job membership is settled before it runs.
            let mut line: Vec<u16> = format!("\"{}\" launcher {port}", IT_APP).encode_utf16().collect();
            line.push(0);
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_NO_WINDOW,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut info,
            );
            assert_eq!(ok, 1, "could not start the suspended launcher");
            let assigned = AssignProcessToJobObject(job, info.hProcess);
            assert_eq!(assigned, 1, "could not put the launcher in the job");
            assert_ne!(ResumeThread(info.hThread), u32::MAX, "could not resume the launcher");

            // The launcher hands the port to its detached serve child and exits 0.
            let owner = wait_listening(port)
                .await
                .expect("the launcher's serve child never listened");
            // 0 is WAIT_OBJECT_0: the launcher exited (the exit CODE itself is not
            // the point here; the owner test pins the exit's aftermath).
            assert_eq!(WaitForSingleObject(info.hProcess, 15_000), 0, "the launcher never exited");
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            assert_ne!(owner, std::process::id(), "the owner must be the child, not us");

            // The proof: destroying the job (last handle closed, KILL_ON_CLOSE fires on
            // whatever is still a member) leaves the app alive on its port.
            CloseHandle(job);
            let still = wait_for(3_000, || our_listener(port).filter(|_| process_created_at(owner).is_some()))
                .await
                .unwrap_or_default();
            assert_eq!(still, owner, "the app died with its spawner's job");
            tree_kill(owner);
            wait_no_it_app().await;
        }
    }
    #[cfg(not(windows))]
    {
        // The Windows trap has no analogue here; the session test below is the Unix
        // survival proof. Keeping the name in the file on every platform (SPEC
        // §process.verify) while running nothing would be the lie - so this arm is
        // cfg'd to exactly the platforms that have nothing to prove.
    }
}

#[cfg(not(windows))]
#[tokio::test]
async fn a_detached_child_survives_its_spawners_session() {
    let _guard = DetachedGuard::enter().await;
    wait_no_it_app().await;
    let (child, port) = start_ported("session", "serve").await;
    let pid = child.pid;
    let _leash = KillOnDrop::leash(child);
    let owner = our_listener(port).expect("serve never listened");
    assert_eq!(owner, pid);

    // setsid proof, read from the kernel: field 5 is pgrp, field 6 is session, both
    // counted after the last ')' because comm may contain spaces (SPEC §process.detached).
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    let tail = &stat[stat.rfind(')').unwrap() + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let pgrp: u32 = fields[2].parse().expect("pgrp");
    let session: u32 = fields[3].parse().expect("session");
    assert_eq!(pgrp, pid, "the child shares a process group with its spawner");
    assert_eq!(session, pid, "the child shares a session with its spawner");
}

#[tokio::test]
async fn a_megabyte_of_output_never_blocks_the_app() {
    let _guard = DetachedGuard::enter().await;
    wait_no_it_app().await;
    let cap = capture("flood");
    let _ = std::fs::remove_file(&cap);
    let child = start_role("flood", &["flood"]).await;
    let pid = child.pid;
    let _leash = KillOnDrop::leash(child);
    // The capture file grows to the megabyte while the writer lives: a pipe nobody
    // reads would have blocked (or killed on EPIPE) long before.
    let grew = wait_for(15_000, || {
        let size = std::fs::metadata(&cap).ok()?.len();
        (size >= 1024 * 1024).then_some(size)
    })
    .await;
    assert!(grew.is_some(), "the capture file never reached 1 MiB");
    assert!(process_created_at(pid).is_some(), "flood died writing its megabyte");
    let _ = std::fs::remove_file(&cap);
}

#[tokio::test]
async fn an_app_never_holds_the_gateway_log_open() {
    #[cfg(windows)]
    {
        let _guard = DetachedGuard::enter().await;
        wait_no_it_app().await;
        // The spawner's own stdout stands in for gateway-<port>.log, aliased to a real
        // file exactly the way the daemon's boot does, and marked inheritable so the
        // probe below would catch a leak through plain handle inheritance.
        let log = capture("gateway-log");
        let renamed = log.with_extension("renamed");
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&renamed);
        unsafe {
            use windows_sys::Win32::Foundation::{
                CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, GENERIC_WRITE, HANDLE,
            };
            use windows_sys::Win32::Storage::FileSystem::{
                CreateFileW, CREATE_ALWAYS, FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_OUTPUT_HANDLE};

            fn wide(path: &std::path::Path) -> Vec<u16> {
                let mut v: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
                v.push(0);
                v
            }
            // The harness's own stdout, to put back before asserting anything: once
            // the stand-in log IS the process stdout, every later print ("test ... ok"
            // included) would land in it and vanish with its cleanup.
            let harness_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut wide_log = wide(&log);
            let opened = CreateFileW(
                wide_log.as_mut_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                CREATE_ALWAYS,
                0,
                std::ptr::null_mut(),
            );
            assert_ne!(opened, std::ptr::null_mut(), "could not open the stand-in log");
            let mut inheritable: HANDLE = std::ptr::null_mut();
            let this = -1isize as HANDLE;
            let duped = DuplicateHandle(
                this,
                opened,
                this,
                &mut inheritable,
                0,
                1, // bInheritHandle: the shape the daemon's stdio really has
                DUPLICATE_SAME_ACCESS,
            );
            assert_eq!(duped, 1, "could not make the log handle inheritable");
            assert_eq!(SetStdHandle(STD_OUTPUT_HANDLE, inheritable), 1);

            // What the serve path does at boot (SPEC §host.daemon): no handle the
            // gateway holds as stdio may be inheritable by a child.
            swiss_core::platform::keep_std_handles_from_children();

            let child = start_role("log", &["flood"]).await;
            let _leash = KillOnDrop::leash(child);

            // Let go ourselves, then probe by renaming: while ANY handle without
            // FILE_SHARE_DELETE holds the file, the rename fails (os error 5).
            CloseHandle(inheritable);
            CloseHandle(opened);
            let probed = std::fs::rename(&log, &renamed);
            // Restore the real stdout first, so the assertion can actually speak.
            SetStdHandle(STD_OUTPUT_HANDLE, harness_stdout);
            probed.unwrap_or_else(|err| panic!("the app held the gateway log open: {err}"));
            let _ = std::fs::remove_file(&renamed);
        }
    }
    #[cfg(not(windows))]
    {
        let _guard = DetachedGuard::enter().await;
        wait_no_it_app().await;
        // The Unix proof: read the child's ACTUAL descriptor table and assert none of
        // it resolves to the spawner's log - not "probably", file by file.
        let log = capture("gateway-log");
        let _ = std::fs::remove_file(&log);
        std::fs::write(&log, b"the gateway's own log\n").expect("write the stand-in log");
        let child = start_role("log", &["flood"]).await;
        let pid = child.pid;
        let _leash = KillOnDrop::leash(child);
        let leaked = wait_for(5_000, || {
            let holds = std::fs::read_dir(format!("/proc/{pid}/fd"))
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| std::fs::read_link(e.path()).ok())
                .any(|target| target == log);
            (!holds).then_some(())
        })
        .await;
        assert!(leaked.is_some(), "the app holds the gateway log open");
        let _ = std::fs::remove_file(&log);
    }
}

#[tokio::test]
async fn a_launcher_child_is_found_as_the_owner_of_its_port() {
    let _guard = DetachedGuard::enter().await;
    wait_no_it_app().await;
    let (launcher, port) = start_ported("owner", "launcher").await;
    let launcher_pid = launcher.pid;
    let leash = KillOnDrop::leash(launcher);

    // The launcher exits 0 by design; the listener it left behind must be owned by
    // the CHILD pid, found through the same listener table a status probe reads.
    let owner = our_listener(port).expect("the launcher's child never listened");
    let owner_leash = PidKillOnDrop(owner);
    assert_ne!(
        owner, launcher_pid,
        "the port's owner is the dead launcher, not its child"
    );
    assert!(process_created_at(owner).is_some(), "the owner is not alive");

    // And the launcher really is gone, not merely second in the table. Its handle
    // must be let go first: an exited process whose handle someone still holds keeps
    // answering OpenProcess, and the leash holds exactly that handle.
    drop(leash);
    let launcher_gone = wait_for(10_000, || process_created_at(launcher_pid).is_none().then_some(()))
        .await;
    assert!(launcher_gone.is_some(), "the launcher never exited");

    drop(owner_leash);
    wait_no_it_app().await;
}

#[tokio::test]
async fn creation_time_is_stable_for_a_live_pid_and_absent_for_a_dead_one() {
    let _guard = DetachedGuard::enter().await;
    wait_no_it_app().await;
    let (child, _port) = start_ported("stable", "serve").await;
    let pid = child.pid;
    let _leash = KillOnDrop::leash(child);

    let first = process_created_at(pid).expect("no creation time for a live pid");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = process_created_at(pid).expect("the creation time vanished while alive");
    assert_eq!(first, second, "the creation time moved for a live pid");

    drop(_leash); // tree kill, now
    let gone = wait_for(10_000, || process_created_at(pid).is_none().then_some(())).await;
    assert!(gone.is_some(), "a dead pid still reports a creation time");
    wait_no_it_app().await;
}

#[tokio::test]
async fn tree_kill_ends_a_detached_tree_and_reports_no_survivors() {
    let _guard = DetachedGuard::enter().await;
    wait_no_it_app().await;
    // A real tree: launcher spawns serve through the detach seam; on Windows serve's
    // hidden console adds a grandchild to the walk.
    let (launcher, port) = start_ported("tree", "launcher").await;
    let _leash = KillOnDrop::leash(launcher);
    let owner = our_listener(port).expect("the tree never listened");

    let survivors = tree_kill(owner);
    assert!(
        survivors.is_empty(),
        "tree_kill reported survivors: {survivors:?}"
    );

    // The tree is gone: no it-app owner on the port (a same-numbered listener of
    // another test is not our survivor), no it-app anywhere - the whole point of the
    // creation-time-checked walk being that nothing is missed and nothing extra dies.
    let freed = wait_for(10_000, || our_listener(port).is_none().then_some(())).await;
    assert!(freed.is_some(), "the port still has an owner after the tree kill");
    wait_no_it_app().await;
}
