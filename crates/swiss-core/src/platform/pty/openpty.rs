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

//! `openpty`: the unix half of [`crate::platform::pty`].
//!
//! Shorter than the Windows half by a wide margin, because the kernel does the parts conhost
//! does not: the master fd is a normal file descriptor, reading it blocks the way any read
//! blocks, and when the last slave fd closes the read ends by itself. So there is no polling
//! loop and no teardown ordering puzzle here — killing the process group is enough, and the
//! session ends because the tty really is gone.
//!
//! Two things still have to be right:
//!
//! - **`setsid` + `TIOCSCTTY` in the child, between fork and exec.** Without a controlling
//!   terminal there is no job control: Ctrl-C reaches nothing, and interactive programs behave
//!   as if their input were a file.
//! - **The parent drops its slave fd right after the spawn.** Keeping it open would hold the
//!   pty alive after the child exits, which is exactly the Windows bug this platform is spared.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{PtyCommand, PtyGeometry, ShellCandidate};

/// What both halves reach.
struct Shared {
    /// The pty master: read by the pump, written and resized through the handle.
    master: File,
    child: Mutex<Child>,
    /// The child's pid, which `setsid` also makes its process-group id — so one `kill(-pid)`
    /// reaches everything the shell started, the way the Windows job object does.
    pid: u32,
    stop: AtomicBool,
}

impl Shared {
    fn exit_code(&self) -> Option<i32> {
        let mut child = self.child.lock().expect("pty child lock");
        // try_wait reaps and remembers, so asking twice is free and never blocks.
        match child.try_wait() {
            Ok(Some(status)) => Some(exit_code_of(&status)),
            _ => None,
        }
    }

    fn kill(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if self.exit_code().is_some() {
            return;
        }
        // SAFETY: a negative pid targets the process group, which is what `setsid` in the child
        // made it the leader of; SIGKILL takes no pointers and cannot be blocked.
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
    }
}

fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    // A killed shell has no exit code; report it the way a shell does, so "128 + signal" reaches
    // the user instead of a bare "unknown".
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// The async side of a local pty: write, resize, ask after the child, kill it.
#[derive(Clone)]
pub struct PtyHandle {
    shared: Arc<Shared>,
}

impl PtyHandle {
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        (&self.shared.master).write_all(bytes)
    }

    pub fn resize(&self, size: PtyGeometry) -> io::Result<()> {
        let ws = winsize(size);
        // SAFETY: a live master fd owned by `shared`, and a live local winsize; TIOCSWINSZ only
        // reads it. The kernel also raises SIGWINCH on the far side, which is the point.
        let rc = unsafe { libc::ioctl(self.shared.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.shared.exit_code()
    }

    pub fn pid(&self) -> u32 {
        self.shared.pid
    }

    /// End the session now. Killing the group closes the last slave fd, so the pump's blocking
    /// read returns on its own — no cancellation machinery needed on this platform.
    pub fn kill(&self) {
        self.shared.kill();
    }
}

/// The blocking side of a local pty. Move it into a `spawn_blocking` thread; dropping it tears
/// the session down.
pub struct PtyPump {
    shared: Arc<Shared>,
}

impl PtyPump {
    /// Read whatever the terminal has produced. `Ok(0)` means the far side is gone for good.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.shared.stop.load(Ordering::SeqCst) {
            return Ok(0);
        }
        match (&self.shared.master).read(buf) {
            // Linux reports EIO on the master once the last slave is closed. That is this
            // platform's EOF, not a fault to report to the user.
            Err(err) if err.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

impl Drop for PtyPump {
    fn drop(&mut self) {
        self.shared.kill();
        // Reap, so a closed tab does not leave a zombie behind. SIGKILL cannot be caught, so
        // this wait is bounded by the scheduler, not by the child's cooperation.
        let _ = self.shared.child.lock().expect("pty child lock").wait();
    }
}

/// The shell a local session gets when the configuration names none: $SHELL, then
/// /bin/sh. Unix is unchanged by docs/15 §2.1 (the pwsh default order is a Windows gap).
pub fn default_shell() -> PtyCommand {
    PtyCommand::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
}

/// The one shell this host offers: $SHELL (already absolute), so the settings sheet has a
/// candidate to show rather than an empty dropdown.
pub fn shell_candidates() -> Vec<ShellCandidate> {
    let program = default_shell().program;
    let label = std::path::Path::new(&program)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.clone());
    vec![ShellCandidate { program, label }]
}

/// Unix needs no PATH rewrite: exec semantics resolve at spawn, and $SHELL arrives
/// absolute. The identity keeps the panel's label exactly what the user configured.
pub fn resolve_program(program: &str) -> String {
    program.to_string()
}

/// Open a pty and start `command` on the far side of it.
pub fn open_pty(command: &PtyCommand, size: PtyGeometry) -> io::Result<(PtyHandle, PtyPump)> {
    // `mut` for the call below only: glibc declares openpty's termios and winsize as *const,
    // Apple's libc as *mut. Raw *mut pointers coerce to either, so one call compiles on both
    // (a `&mut` would too, but clippy's unnecessary_mut_passed rejects it against glibc's
    // *const); openpty only reads them.
    let mut ws = winsize(size);
    let mut master_fd: RawFd = -1;
    let mut slave_fd: RawFd = -1;
    // SAFETY: both out-params are live locals; a null termios asks for the kernel's default line
    // discipline, which is what an interactive shell expects, and the winsize outlives the call.
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut::<libc::termios>(),
            &raw mut ws,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: two fresh fds this call owns; File takes ownership and is the only closer.
    let (master, slave) = unsafe { (File::from_raw_fd(master_fd), File::from_raw_fd(slave_fd)) };
    // openpty leaves the master inheritable. Every other child this gateway spawns would then
    // hold the terminal open — close-on-exec keeps a session's lifetime its own.
    set_cloexec(&master)?;

    let mut spawn = Command::new(&command.program);
    spawn.args(&command.args);
    for key in &command.env_remove {
        spawn.env_remove(key);
    }
    for (key, value) in &command.env {
        spawn.env(key, value);
    }
    if let Some(dir) = &command.cwd {
        spawn.current_dir(dir);
    }
    // The child's three standard fds ARE the tty; std dup2s these before running the hook below.
    spawn.stdin(Stdio::from(slave.try_clone()?));
    spawn.stdout(Stdio::from(slave.try_clone()?));
    spawn.stderr(Stdio::from(slave.try_clone()?));
    // SAFETY: this closure runs between fork and exec, where only async-signal-safe calls are
    // allowed — setsid and ioctl are both on that list, and neither allocates.
    unsafe {
        spawn.pre_exec(|| {
            // A new session, so the child leads its own process group: job control works, and
            // one kill(-pgid) later reaches everything it started.
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            // fd 0 is the slave by now. Claiming it as the controlling terminal is what makes
            // Ctrl-C a signal rather than a byte nobody acts on.
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = spawn.spawn()?;
    // Only the child holds the tty now, so its exit is the pump's EOF.
    drop(slave);

    let pid = child.id();
    let shared = Arc::new(Shared {
        master,
        child: Mutex::new(child),
        pid,
        stop: AtomicBool::new(false),
    });
    Ok((
        PtyHandle {
            shared: shared.clone(),
        },
        PtyPump { shared },
    ))
}

fn winsize(size: PtyGeometry) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn set_cloexec(file: &File) -> io::Result<()> {
    // SAFETY: a live fd owned by `file`; both fcntl calls take no pointers.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
