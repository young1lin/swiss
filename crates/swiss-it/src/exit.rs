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

//! The exit story for the containers engine.rs starts (docs/44 SS2.2). The locked
//! testcontainers 0.27.3 has no ryuk reaper; cleanup there is `ContainerAsync`'s
//! Drop, which needs a live tokio runtime and therefore never runs for a value held
//! in a static.
//!
//! Two cleaners cover the exit paths, cheapest first:
//!
//! - **The atexit hook** removes every registered id with a raw blocking DELETE
//!   (docker_raw). It fires on a normal green run - but a FAILED libtest run exits
//!   through `std::process::exit(101)`, which on Windows is `ExitProcess` and skips
//!   the CRT's atexit list entirely, so the hook alone leaks on exactly the runs a
//!   developer repeats most.
//! - **The it-reaper watchdog** covers every exit path, because it does not depend on
//!   the parent running any code: a child process whose stdin is the parent's pipe,
//!   spawned once on the first container. The OS closes the pipe on every exit -
//!   normal, ExitProcess, killed, crashed - and the reaper then force-removes every
//!   id it was fed. The duplicate DELETE from the atexit hook reads 404 and is fine.
//!
//! What neither survives is the reaper dying WITH the parent before EOF matters (a
//! power cut, a tree kill that takes the breakaway child too); engine.rs's startup
//! prune of hour-old leftovers owns that tail.

use std::io::Write;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Mutex, Once};

use crate::docker_raw;

/// One registered container: the docker endpoint to talk to and the id to remove.
struct Lease {
    endpoint: String,
    id: String,
}

static LEASES: Mutex<Vec<Lease>> = Mutex::new(Vec::new());
static HOOK: Once = Once::new();
/// The reaper's stdin, held for the life of the process: never dropped means the
/// child never sees EOF while this process still runs, and sees it the moment it stops.
static REAPER: Mutex<Option<ChildStdin>> = Mutex::new(None);

/// Record a started container, arm both cleaners, and feed the id to the reaper.
/// Idempotent per process by construction.
pub(crate) fn arm(endpoint: &str, id: &str) {
    LEASES
        .lock()
        .expect("the swiss-it lease list")
        .push(Lease {
            endpoint: endpoint.to_string(),
            id: id.to_string(),
        });
    HOOK.call_once(|| {
        // The watchdog first: its spawn must succeed BEFORE any container can be
        // forgotten by a failed run. A missing binary degrades to atexit-only,
        // loudly - never a panic in a harness.
        *REAPER.lock().expect("the swiss-it reaper slot") = spawn_reaper(endpoint);
        // The CRT's atexit on both platforms this suite runs on; the hook must be a
        // plain extern "C" function and nothing may unwind across it.
        if unsafe { atexit(fire) } != 0 {
            eprintln!("swiss-it: could not arm the container exit hook");
        }
    });
    // Every arm feeds the live reaper (the first arm's spawn already consumed the
    // endpoint line); a reaper-less process simply has nothing to feed.
    let mut slot = REAPER.lock().expect("the swiss-it reaper slot");
    if let Some(stdin) = slot.as_mut() {
        if let Err(e) = writeln!(stdin, "{id}").and_then(|()| stdin.flush()) {
            eprintln!("swiss-it: could not tell the reaper about container {id}: {e}");
        }
    }
}

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

/// extern "C" shim: unwinding across atexit is undefined, and the tests have already
/// reported their verdict by the time this runs - swallow everything.
extern "C" fn fire() {
    let _ = std::panic::catch_unwind(remove_all);
}

/// Force-remove every registered container, one blocking call each. The fast green
/// path; the reaper repeats the same DELETEs moments later and reads 404s.
fn remove_all() {
    let leases = std::mem::take(&mut *LEASES.lock().expect("the swiss-it lease list"));
    for lease in leases {
        match docker_raw::raw_delete(&lease.endpoint, &lease.id) {
            Ok(()) => eprintln!(
                "swiss-it: removed container {} at exit",
                &lease.id[..lease.id.len().min(12)]
            ),
            Err(e) => eprintln!(
                "swiss-it: could not remove container {} at exit: {e} (endpoint {}); \
                 the reaper repeats the delete, and the next run prunes it anyway",
                &lease.id[..lease.id.len().min(12)],
                lease.endpoint
            ),
        }
    }
}

/// Where cargo puts a sibling binary from the same profile: the test harness runs as
/// `target/<profile>/deps/it-<hash>[.exe]`, the bins build next to that profile's
/// root. `CARGO_BIN_EXE_*` cannot be used here - it only exists in test targets, and
/// this spawn lives in the lib.
fn reaper_binary() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    let name = if cfg!(windows) {
        "it-reaper.exe"
    } else {
        "it-reaper"
    };
    let path = profile_dir.join(name);
    path.is_file().then_some(path)
}

/// Spawn the watchdog once. Best-effort at every step, each failure named: a missing
/// binary (a harness built without the bin target), a refused spawn, a broken pipe -
/// the atexit hook still owns the green path in all of those cases.
fn spawn_reaper(endpoint: &str) -> Option<ChildStdin> {
    let path = match reaper_binary() {
        Some(p) => p,
        None => {
            eprintln!(
                "swiss-it: no it-reaper binary beside the test binary; \
                 cleanup falls back to the atexit hook, and a red run on Windows leaks \
                 (docs/44-wsl-docker-setup.md SS4)"
            );
            return None;
        }
    };
    let mut cmd = Command::new(&path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let spawned = {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const NEW_GROUP: u32 = 0x0000_0200;
            const BREAKAWAY: u32 = 0x0100_0000;
            // A new process group keeps Ctrl+C off the reaper. Breakaway goes further:
            // a job that kills children on close (CI runners use them) must not take
            // the reaper with the parent. A job that forbids breakaway refuses the
            // flag at spawn, and the retry without it keeps the group - a tree kill
            // then takes the reaper too, which the hour-old prune in engine.rs owns.
            cmd.creation_flags(NEW_GROUP | BREAKAWAY);
            match cmd.spawn() {
                Ok(child) => Ok(child),
                // Breakaway refused (a job without JOB_OBJECT_LIMIT_BREAKAWAY_OK):
                // fall back to the group alone; a tree kill then takes the reaper
                // with the parent, which the hour-old prune in engine.rs owns.
                Err(_) => {
                    cmd.creation_flags(NEW_GROUP);
                    cmd.spawn()
                }
            }
        }
        #[cfg(not(windows))]
        {
            cmd.spawn()
        }
    };
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            eprintln!("swiss-it: could not spawn the it-reaper watchdog: {e}");
            return None;
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        eprintln!("swiss-it: the it-reaper has no stdin to inherit");
        return None;
    };
    // Line one is the docker endpoint; the ids follow one per line.
    if writeln!(stdin, "{endpoint}").and_then(|()| stdin.flush()).is_err() {
        eprintln!("swiss-it: could not hand the endpoint to the it-reaper");
        return None;
    }
    // The Child itself is dropped on purpose: its stdin lives on in REAPER, and EOF -
    // not the handle - is the signal.
    drop(child);
    Some(stdin)
}
