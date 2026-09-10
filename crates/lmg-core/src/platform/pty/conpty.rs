//! ConPTY: the Windows half of [`crate::platform::pty`].
//!
//! Four moving parts, in the order `open_pty` builds them: two anonymous pipes, a pseudoconsole
//! that takes one end of each, a child spawned with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` so
//! conhost is its console, and a kill-on-close job holding the child's whole subtree (ADR-008).
//!
//! Two ordering rules are load-bearing, and both are the difference between a working terminal
//! and a hang:
//!
//! 1. **Our copies of the two ends conhost took are closed immediately after
//!    `CreatePseudoConsole`.** Conhost duplicates them; if we keep ours, the output pipe still
//!    has a live writer after the child exits, so the reader never sees EOF and a finished
//!    session holds its blocking thread for ever.
//! 2. **Teardown closes the read handle before the pseudoconsole.** `ClosePseudoConsole` waits
//!    for pending output to be drained, so closing it while nobody is reading is the documented
//!    way to deadlock. With the read end gone, conhost's writes fail instead of blocking.
//!
//! And one non-obvious consequence of conhost's ownership: the child exiting does NOT close the
//! output pipe, so a blocking `ReadFile` would never return for a session the user closed. The
//! pump therefore polls with `PeekNamedPipe` and reads only what is already there — the read
//! itself never blocks, which is what makes `kill()` from the async side able to end the
//! blocking thread at all.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{PtyCommand, PtyGeometry};
use crate::platform::job::KillOnCloseJob;

/// Output pipe buffer. The 4 KB default makes conhost block on roughly every screenful of a
/// `dir` or a build log, which shows up as visible stutter; 64 KB is one commit of a page-file
/// backed buffer per session and removes it.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// How long the reader keeps draining after it first sees the child gone. Conhost writes the
/// child's last screenful after the process object is already signalled, so an immediate EOF
/// would cut off the tail of `echo hi`.
const EXIT_DRAIN: Duration = Duration::from_millis(150);

/// Poll intervals for the reader. The fast one bounds keystroke echo latency (well under a
/// frame); the slow one is what an idle session actually costs — one `PeekNamedPipe` every
/// 25 ms, which is a syscall, not a wakeup anyone can measure.
const FAST_POLL: Duration = Duration::from_millis(5);
const SLOW_POLL: Duration = Duration::from_millis(25);
/// Empty polls before backing off — about a second of silence.
const IDLE_POLLS: u32 = 200;

/// An owned pseudoconsole. Closing it is what makes conhost exit.
struct PseudoConsole(HPCON);

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        // SAFETY: the HPCON is owned by this wrapper, closed exactly once, and by the teardown
        // ordering above the output pipe's read end is already gone — so this cannot block.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

/// An owned process handle: the child's, kept for exit status and for the forced kill.
struct ProcessHandle(HANDLE);

// SAFETY: the HANDLE is owned exclusively by this wrapper and never cloned out; a Win32 process
// handle is a plain kernel object reference usable from any thread.
unsafe impl Send for ProcessHandle {}
unsafe impl Sync for ProcessHandle {}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed exactly once here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// What both halves reach: the console (for resize), the child (for status and kill) and the
/// stop flag the pump watches.
struct Shared {
    /// `None` once the pump has torn the session down — a resize after that is a no-op, not a
    /// call on a freed HPCON.
    console: Mutex<Option<PseudoConsole>>,
    process: ProcessHandle,
    pid: u32,
    stop: AtomicBool,
}

impl Shared {
    fn exit_code(&self) -> Option<i32> {
        // SAFETY: an owned, live process handle; both calls only write to the local out-param.
        unsafe {
            // A zero-timeout wait, not `GetExitCodeProcess == STILL_ACTIVE`: 259 is also a
            // perfectly legal exit code, and a shell that exits with it must not read as running.
            if WaitForSingleObject(self.process.0, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.process.0, &mut code).ok()?;
            Some(code as i32)
        }
    }

    fn kill(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // SAFETY: an owned, live process handle; terminating an already-dead process is an error
        // this deliberately ignores.
        unsafe {
            let _ = TerminateProcess(self.process.0, 1);
        }
    }
}

/// The async side of a local pty: write, resize, ask after the child, kill it.
#[derive(Clone)]
pub struct PtyHandle {
    shared: Arc<Shared>,
    writer: Arc<File>,
}

impl PtyHandle {
    /// Send keystrokes. Bytes go through as typed — no line discipline on this side; the
    /// pseudoconsole is the terminal.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        (&*self.writer).write_all(bytes)
    }

    /// Tell the far side the window changed. A no-op once the session has been torn down.
    pub fn resize(&self, size: PtyGeometry) -> io::Result<()> {
        let console = self.shared.console.lock().expect("pty console lock");
        let Some(console) = console.as_ref() else {
            return Ok(());
        };
        // SAFETY: the HPCON is alive for as long as the guard holds the lock — teardown takes it
        // out under the same lock before closing it.
        unsafe { ResizePseudoConsole(console.0, coord(size)) }
            .map_err(|err| io::Error::other(format!("resize pty: {err}")))
    }

    /// The child's exit code, or `None` while it is still running.
    pub fn exit_code(&self) -> Option<i32> {
        self.shared.exit_code()
    }

    pub fn pid(&self) -> u32 {
        self.shared.pid
    }

    /// End the session now: the child dies and the pump returns EOF within one poll interval.
    /// The child's subtree goes with the pump's job handle when the pump drops.
    pub fn kill(&self) {
        self.shared.kill();
    }
}

/// The blocking side of a local pty. Move it into a `spawn_blocking` thread; dropping it tears
/// the session down.
pub struct PtyPump {
    shared: Arc<Shared>,
    /// `Option` so teardown can close the read end FIRST — see the module header.
    reader: Option<File>,
    /// The child's subtree guard. Dropping it kills every process the shell started.
    job: Option<KillOnCloseJob>,
    /// When the child was first seen gone, so the tail of its output still gets through.
    exited_at: Option<Instant>,
}

impl PtyPump {
    /// Read whatever the terminal has produced. `Ok(0)` means the far side is gone for good.
    ///
    /// Never blocks on the pipe: it polls, so a `kill()` from the async side ends this thread
    /// instead of stranding it inside conhost (module header, rule 2).
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut idle: u32 = 0;
        loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                return Ok(0);
            }
            let Some(reader) = self.reader.as_mut() else {
                return Ok(0);
            };
            match available(reader) {
                // A broken pipe is conhost telling us the session is over, not a fault.
                Err(_) => return Ok(0),
                Ok(0) => {
                    match self.exited_at {
                        Some(gone) if gone.elapsed() > EXIT_DRAIN => return Ok(0),
                        Some(_) => {}
                        None => {
                            if self.shared.exit_code().is_some() {
                                self.exited_at = Some(Instant::now());
                            }
                        }
                    }
                    idle = idle.saturating_add(1);
                    std::thread::sleep(if idle > IDLE_POLLS {
                        SLOW_POLL
                    } else {
                        FAST_POLL
                    });
                }
                Ok(avail) => {
                    let want = (avail as usize).min(buf.len());
                    // Only this thread reads the handle, so the bytes Peek just counted are
                    // still there: the read returns at once rather than blocking for more.
                    return reader.read(&mut buf[..want]);
                }
            }
        }
    }
}

impl Drop for PtyPump {
    fn drop(&mut self) {
        // The order is the whole point. Closing the read end first means conhost's writes fail
        // instead of blocking, which is what keeps ClosePseudoConsole from hanging below.
        self.shared.stop.store(true, Ordering::SeqCst);
        drop(self.reader.take());
        // Kill the shell and everything it started, before the console goes: a child that
        // ignores the console closing must not survive the tab that owned it (ADR-008).
        self.shared.kill();
        drop(self.job.take());
        let console = self.shared.console.lock().expect("pty console lock").take();
        drop(console);
    }
}

/// How many bytes the output pipe already holds. `Err` means the pipe is gone.
fn available(reader: &File) -> io::Result<u32> {
    let mut avail = 0u32;
    // SAFETY: a live handle owned by `reader`; the only out-param is a local u32, and passing
    // None for the buffer is the documented "just tell me the count" form.
    unsafe {
        PeekNamedPipe(
            HANDLE(reader.as_raw_handle()),
            None,
            0,
            None,
            Some(&mut avail),
            None,
        )
    }
    .map_err(|err| io::Error::other(format!("peek pty output: {err}")))?;
    Ok(avail)
}

/// The shell a local session gets when the configuration names none.
pub fn default_shell() -> PtyCommand {
    // COMSPEC is what every Windows shell launcher uses, and it is set even in a service
    // context; cmd.exe is the fallback because it is the one program guaranteed to be there.
    PtyCommand::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()))
}

/// Open a pseudoconsole and start `command` attached to it.
pub fn open_pty(command: &PtyCommand, size: PtyGeometry) -> io::Result<(PtyHandle, PtyPump)> {
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;

    // SAFETY: both handles are live and owned by the Files above; the pseudoconsole duplicates
    // what it needs, which is exactly why the originals are dropped right after.
    let console = unsafe {
        CreatePseudoConsole(
            coord(size),
            HANDLE(input_read.as_raw_handle()),
            HANDLE(output_write.as_raw_handle()),
            0,
        )
    }
    .map_err(|err| io::Error::other(format!("create pseudoconsole: {err}")))?;
    let console = PseudoConsole(console);
    // Rule 1 from the module header: keeping either of these open costs the reader its EOF.
    drop(input_read);
    drop(output_write);

    let (process, pid, job) = spawn_attached(command, &console)?;
    let shared = Arc::new(Shared {
        console: Mutex::new(Some(console)),
        process,
        pid,
        stop: AtomicBool::new(false),
    });
    Ok((
        PtyHandle {
            shared: shared.clone(),
            writer: Arc::new(input_write),
        },
        PtyPump {
            shared,
            reader: Some(output_read),
            job,
            exited_at: None,
        },
    ))
}

/// An anonymous pipe as two owned Files: (read end, write end).
fn pipe() -> io::Result<(File, File)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: two live out-params; None attributes is the documented "default security,
    // non-inheritable" form, which is what we want — the child reaches this pipe through the
    // pseudoconsole, never by inheriting a handle.
    unsafe { CreatePipe(&mut read, &mut write, None, PIPE_BUFFER_BYTES) }
        .map_err(|err| io::Error::other(format!("create pty pipe: {err}")))?;
    // SAFETY: both handles were just created and are owned here; File takes ownership and is
    // the only thing that will close them.
    unsafe {
        Ok((
            File::from_raw_handle(read.0),
            File::from_raw_handle(write.0),
        ))
    }
}

fn coord(size: PtyGeometry) -> COORD {
    COORD {
        X: size.cols.min(i16::MAX as u16) as i16,
        Y: size.rows.min(i16::MAX as u16) as i16,
    }
}

/// The proc-thread attribute list carrying `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, owned so that
/// every exit path deletes it.
struct AttrList {
    buf: Vec<u8>,
}

impl AttrList {
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        // SAFETY: only ever constructed AFTER a successful InitializeProcThreadAttributeList, so
        // there is always a list here to delete, and it is deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn attribute_list(console: &PseudoConsole) -> io::Result<AttrList> {
    let mut needed = 0usize;
    // SAFETY: the sizing call is documented to fail with ERROR_INSUFFICIENT_BUFFER and write the
    // required size; a null list pointer is what that form takes.
    unsafe {
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            1,
            0,
            &mut needed,
        );
    }
    if needed == 0 {
        return Err(io::Error::other("attribute list size came back as zero"));
    }
    let mut buf = vec![0u8; needed];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
    // SAFETY: the buffer is the size the call above asked for and outlives the list (it moves
    // into the guard below, which moves the Vec, not its heap allocation).
    unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut needed) }
        .map_err(|err| io::Error::other(format!("init attribute list: {err}")))?;
    let mut guard = AttrList { buf };
    // SAFETY: the HPCON is passed BY VALUE as the attribute (that is the documented form for
    // PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, not a pointer to it), and it outlives the spawn.
    unsafe {
        UpdateProcThreadAttribute(
            guard.ptr(),
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(console.0 .0 as *const core::ffi::c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )
    }
    .map_err(|err| io::Error::other(format!("attach pseudoconsole: {err}")))?;
    Ok(guard)
}

fn spawn_attached(
    command: &PtyCommand,
    console: &PseudoConsole,
) -> io::Result<(ProcessHandle, u32, Option<KillOnCloseJob>)> {
    let mut attrs = attribute_list(console)?;
    let mut startup = STARTUPINFOEXW {
        lpAttributeList: attrs.ptr(),
        ..Default::default()
    };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    // Three NULL std handles, declared present. This looks like a no-op and is the single
    // most load-bearing line in the file.
    //
    // Without it, CreateProcessW copies OUR standard handles into the child. Microsoft's ConPTY
    // sample gets away with that because its parent is a console program: its std handles are
    // console pseudo-handles, and the same value re-resolves against whatever console the child
    // ends up attached to — the pseudoconsole. This gateway is not that program. Started as a
    // service, from a scheduled task, or with its output redirected to a log, its stdout is a
    // pipe or a file, and the child inherits THAT: measured here, output written to stdout
    // inside the terminal went to the gateway's own stdout, while only direct-to-console output
    // reached the terminal. Half a shell, with the missing half in the log file.
    //
    // Declaring the handles NULL leaves the child's std handles empty at creation, so the
    // console connect fills them in from the pseudoconsole — which is what happens for any
    // console child of a windowed parent, and is the state the sample was quietly relying on.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = HANDLE::default();
    startup.StartupInfo.hStdOutput = HANDLE::default();
    startup.StartupInfo.hStdError = HANDLE::default();

    let mut command_line = wide(&command_line(command));
    let environment = environment_block(&command.env);
    let cwd = command.cwd.as_ref().map(|dir| wide(&dir.to_string_lossy()));
    let mut info = PROCESS_INFORMATION::default();

    // SAFETY: every pointer below is a live local outliving the call; the command line is
    // mutable because CreateProcessW is documented to write into it. Handle inheritance is off:
    // the pseudoconsole is the child's only channel, so nothing else of ours can leak into it.
    // CREATE_SUSPENDED closes the window in which a child could spawn descendants before the job
    // exists — the supervisor's version of this spawn cannot, but here we own the resume.
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            PWSTR(command_line.as_mut_ptr()),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            Some(environment.as_ptr() as *const core::ffi::c_void),
            cwd.as_ref()
                .map(|dir| PCWSTR(dir.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            &startup.StartupInfo,
            &mut info,
        )
    }
    .map_err(|err| io::Error::other(format!("start {}: {err}", command.program)))?;

    let job = KillOnCloseJob::assign(info.hProcess.0);
    // SAFETY: the thread handle is ours from the spawn; resuming a suspended main thread starts
    // the process, and the handle is closed straight after — the process handle is the one we
    // keep.
    unsafe {
        ResumeThread(info.hThread);
        let _ = CloseHandle(info.hThread);
    }
    Ok((ProcessHandle(info.hProcess), info.dwProcessId, job))
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// The environment the child gets: this process's, with the caller's entries replacing
/// same-named ones. Sorted case-insensitively and NUL-terminated twice, which is the block
/// format `CREATE_UNICODE_ENVIRONMENT` expects.
fn environment_block(overrides: &[(String, String)]) -> Vec<u16> {
    let mut merged: Vec<(String, String)> = std::env::vars().collect();
    for (key, value) in overrides {
        merged.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
        merged.push((key.clone(), value.clone()));
    }
    merged.sort_by_key(|(key, _)| key.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (key, value) in merged {
        block.extend(std::ffi::OsStr::new(&key).encode_wide());
        block.push(u16::from(b'='));
        block.extend(std::ffi::OsStr::new(&value).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Program plus arguments as one command line, quoted the way `CommandLineToArgvW` will take it
/// back apart. Hand-rolled because `CreateProcessW` takes a string, not an argv — get this wrong
/// and a path with a space silently becomes two arguments.
fn command_line(command: &PtyCommand) -> String {
    let mut line = quote(&command.program);
    for arg in &command.args {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    line
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes are only special in front of a quote: double them, then escape
                // the quote itself.
                out.push_str(&"\\".repeat(backslashes + 1));
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        out.push(c);
    }
    // Trailing backslashes would escape the closing quote if left single.
    out.push_str(&"\\".repeat(backslashes));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_argument_is_left_alone() {
        assert_eq!(
            command_line(&PtyCommand::new("cmd.exe").arg("/c")),
            "cmd.exe /c"
        );
    }

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        // The failure this prevents: "C:\Program Files\..." arriving as two arguments, so the
        // shell reports a file that does not exist and the user cannot tell why.
        let line = command_line(&PtyCommand::new(r"C:\Program Files\Git\bin\bash.exe").arg("-l"));
        assert_eq!(line, r#""C:\Program Files\Git\bin\bash.exe" -l"#);
    }

    #[test]
    fn quotes_and_the_backslashes_in_front_of_them_are_escaped() {
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn a_trailing_backslash_does_not_escape_the_closing_quote() {
        // r"C:\dir with space\" — the naive version ends up quoting the rest of the line.
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
    }

    #[test]
    fn an_empty_argument_survives_as_an_empty_argument() {
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn an_override_replaces_the_inherited_variable_rather_than_adding_a_second() {
        let block = environment_block(&[("PATH".into(), "only-this".into())]);
        let text = String::from_utf16_lossy(&block);
        let entries: Vec<&str> = text.split('\0').filter(|e| !e.is_empty()).collect();
        let paths: Vec<&&str> = entries
            .iter()
            .filter(|e| e.to_uppercase().starts_with("PATH="))
            .collect();
        assert_eq!(paths.len(), 1, "two PATHs in one block: {paths:?}");
        assert_eq!(*paths[0], "PATH=only-this");
        // And the rest of the environment is still there — a shell with no SystemRoot is not a
        // shell anyone can use.
        assert!(entries.len() > 1, "the inherited environment was dropped");
    }

    #[test]
    fn the_block_ends_with_the_double_nul_createprocess_looks_for() {
        let block = environment_block(&[("LMG_PTY_TEST".into(), "1".into())]);
        assert_eq!(block.last(), Some(&0));
        assert_eq!(block[block.len() - 2], 0);
    }
}
