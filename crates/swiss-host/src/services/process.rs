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

//! The shared process supervisor (docs/10 §6: ownership of the Windows process-tree Job
//! Object and the Unix process group belongs to the SHARED process service) — the one place
//! that owns how this gateway spawns, watches, captures and tears down an external command.
//! Two consumers plug into it: the proc MCP adapter's long-lived children and Jobs' one-shot
//! commands share the platform teardown and the bounded-capture discipline, while their
//! lifecycles stay separate (a resident MCP child is not a run).
//!
//! Why this exists beside adapters/proc.rs (which stays untouched):
//! - Bounded WHILE reading: the old jobs runner read its pipes with read_to_end into an
//!   unbounded Vec and trimmed the tail only afterwards — a chatty child could balloon the
//!   process before the trim ever ran. Here every chunk lands in a fixed-size tail buffer
//!   as it arrives; retained memory is capped at the configured bytes, always.
//! - Readers are drained, cancelable and JOINED, never detached: the run future owns the
//!   two reader JoinHandles and awaits (or aborts) them before returning, so no output
//!   task outlives the run that started it.
//! - Platform-safe subtree teardown, the same mechanics the proc adapter uses: a Windows
//!   kill-on-close Job Object assigned at spawn (one handle owns the whole tree, PID-reuse
//!   safe) and a Unix private process group killed via kill(-pgid). The Win32 seam moved
//!   DOWN to `swiss_core::platform::KillOnCloseJob` when the local terminal needed it too
//!   (docs/14 T3), so this crate now holds no unsafe at all; the proc adapter, the jobs
//!   runner and the terminal all assign through that one function, and every child of this
//!   gateway dies with the same guarantee.
//!
//! Environment references in typed input are resolved STRICTLY by the action layer: a
//! missing required env var is an error naming the variable (docs/10 §6), never a silent
//! empty string. Captured output is masked for every value a run resolved, so a child that
//! echoes its own credentials does not write them into run history.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

#[cfg(windows)]
use swiss_core::platform::KillOnCloseJob;

use crate::services::action::CancelHandle;

/// How long a subtree kill waits for the root to report its exit status (TerminateProcess /
/// SIGKILL cannot be ignored; this only bounds a wedged wait) — the proc adapter's KILL_WAIT.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// Grace period for draining the output pipes after the process is gone: a descendant that
/// inherited a pipe can hold it open briefly, and that must not become a second hang. When
/// the grace lapses the reader is aborted — everything already read is kept.
pub const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// The default per-run capture budget (the docs' 16 KiB tail default).
pub const DEFAULT_OUTPUT_MAX_BYTES: usize = 16 * 1024;

/// A fixed-size tail of a byte stream. Bytes are appended AS THEY ARE READ (never
/// accumulated first): retained memory is capped at max_bytes no matter how much the child
/// writes. The running total is kept so a reader can report "there was more".
#[derive(Debug)]
pub struct BoundedCapture {
    max_bytes: usize,
    buf: Vec<u8>,
    total: u64,
}

impl BoundedCapture {
    pub fn new(max_bytes: usize) -> Self {
        BoundedCapture {
            max_bytes: max_bytes.max(1),
            buf: Vec::new(),
            total: 0,
        }
    }

    /// Append one read chunk, keeping only the tail. O(chunk + max) per overflow: the trim
    /// moves at most max_bytes — a 16 KiB cap with 8 KiB chunks makes that a memcpy of the
    /// cap, which is background capture work, not a hot path.
    pub fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.max_bytes {
            let keep_from = self.buf.len() - self.max_bytes;
            self.buf.drain(..keep_from);
        }
    }

    /// The retained tail plus the byte total of the FULL stream.
    pub fn take_tail(&mut self) -> (Vec<u8>, u64) {
        (std::mem::take(&mut self.buf), self.total)
    }

    /// Bytes seen so far (the full-stream count, not the retained size).
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Bytes currently retained.
    pub fn retained(&self) -> usize {
        self.buf.len()
    }
}

/// Read one pipe to EOF (or cancellation) into a shared [BoundedCapture]. THE bounded
/// reader — what replaced read_to_end. It drains the pipe even though the buffer keeps
/// only the tail, so the child never blocks on a full pipe and the memory never grows.
pub async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    capture: Arc<Mutex<BoundedCapture>>,
    cancel: CancelHandle,
) {
    let mut chunk = [0u8; 8 * 1024];
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Cancellation: stop reading. Dropping the task drops the read end of the
                // pipe, so a still-writing writer gets EPIPE instead of blocking forever.
                return;
            }
            read = reader.read(&mut chunk) => {
                match read {
                    Ok(0) => return, // EOF: pipe fully drained
                    Ok(n) => capture
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(&chunk[..n]),
                    Err(_) => return, // broken pipe etc.; the exit status tells the story
                }
            }
        }
    }
}

/// One command to run: program (already PATH-resolved), argv, working directory, and env
/// ADDITIONS on top of the inherited environment.
#[derive(Debug, Clone)]
pub struct ProcSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    /// Values that must never survive into captured output (resolved credentials and other
    /// substituted refs). Values shorter than 8 chars are not worth masking and are ignored.
    pub secrets: Vec<String>,
}

impl ProcSpec {
    pub fn simple(program: String, args: Vec<String>) -> Self {
        ProcSpec {
            program,
            args,
            cwd: None,
            env: Vec::new(),
            secrets: Vec::new(),
        }
    }
}

/// Run limits: the whole-run deadline and the retained-output budget.
#[derive(Debug, Clone, Copy)]
pub struct ProcLimits {
    pub timeout_ms: u64,
    pub output_max_bytes: usize,
}

impl Default for ProcLimits {
    fn default() -> Self {
        ProcLimits {
            timeout_ms: 10 * 60 * 1000,
            output_max_bytes: DEFAULT_OUTPUT_MAX_BYTES,
        }
    }
}

/// Everything one supervised execution produced. "ok" is exactly "exit code 0 within the
/// deadline, not cancelled"; every other path carries its own flag so a caller can record
/// WHICH kind of not-ok happened.
#[derive(Debug, Clone)]
pub struct ProcResult {
    pub ok: bool,
    pub ms: u64,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub canceled: bool,
    /// A failure to run at all (spawn error); None means the process itself ran.
    pub error: Option<String>,
    /// Combined output (stdout, then a labelled stderr section), decoded, tail-capped to
    /// the capture budget, resolved secrets masked.
    pub output: String,
    /// Byte length of the FULL combined stream before capping.
    pub chars: usize,
    /// True when bytes were dropped by the tail cap.
    pub output_truncated: bool,
}

/// Scoped registration of a live child's root pid: the guard unregisters on drop, so even
/// a panicked run cannot leak an entry from [Supervisor::child_pids].
struct PidGuard<'a> {
    pid: u32,
    supervisor: &'a Supervisor,
}

impl Drop for PidGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut pids) = self.supervisor.pids.lock() {
            pids.remove(&self.pid);
        }
    }
}

/// Tracks the root pids of currently-live supervised children so the host can report them
/// (via RuntimeServices::child_pids). Cheap: an empty supervisor costs one Mutex and one
/// empty set, so one-shot callers can afford their own instance.
#[derive(Default)]
pub struct Supervisor {
    pids: Mutex<HashSet<u32>>,
}

impl Supervisor {
    pub fn new() -> Arc<Self> {
        Arc::new(Supervisor::default())
    }

    /// The root pids of all children currently supervised by THIS instance, sorted for
    /// stable output.
    pub fn child_pids(&self) -> Vec<u32> {
        let pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<u32> = pids.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// Run one command to completion (natural exit, deadline or cancellation) and report.
    /// Never panics on a misbehaving child; a spawn failure is a recorded error. When the
    /// returned future resolves, the child is reaped, both pipe readers are joined (or
    /// explicitly aborted) and the pid registration is gone — nothing of the run outlives it.
    pub async fn run(
        &self,
        spec: ProcSpec,
        limits: ProcLimits,
        cancel: CancelHandle,
    ) -> ProcResult {
        let started = Instant::now();
        let fail = |error: String| ProcResult {
            ok: false,
            ms: started.elapsed().as_millis() as u64,
            pid: None,
            exit_code: None,
            timed_out: false,
            canceled: false,
            error: Some(error),
            output: String::new(),
            chars: 0,
            output_truncated: false,
        };

        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW — a console-less daemon must not flash a console per child.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            // Private process group (pgid == the child's pid): one kill(-pgid) later reaches
            // descendants at any depth, and insulates the run from signals aimed at the
            // gateway's own group — the Unix counterpart of the Windows job object.
            use std::os::unix::process::CommandExt as _;
            cmd.as_std_mut().process_group(0);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => return fail(format!("spawn {:?} failed: {err}", spec.program)),
        };
        let pid = child.id();
        // Scoped while the child lives; dropped at every return path below.
        let _pid_guard = pid.map(|p| PidGuard {
            pid: p,
            supervisor: self,
        });

        // The subtree guard, assigned at spawn: everything the command goes on to create
        // inherits the job, so one handle owns the whole tree — for the deadline kill AND
        // for the moment the run ends while a backgrounded descendant is still alive.
        #[cfg(windows)]
        let job = child.raw_handle().and_then(KillOnCloseJob::assign);

        // Readers as tracked tasks JOINED by this future (never detached): a deadline kill
        // cannot discard what was already read, because the bytes live in the shared
        // BoundedCapture, and no reader outlives the run that owns it.
        let out_capture = Arc::new(Mutex::new(BoundedCapture::new(limits.output_max_bytes)));
        let err_capture = Arc::new(Mutex::new(BoundedCapture::new(limits.output_max_bytes)));
        let out_reader = child
            .stdout
            .take()
            .map(|s| tokio::spawn(read_bounded(s, out_capture.clone(), cancel.clone())));
        let err_reader = child
            .stderr
            .take()
            .map(|s| tokio::spawn(read_bounded(s, err_capture.clone(), cancel.clone())));

        enum End {
            Exited(i32),
            WaitFailed(String),
            TimedOut,
            Canceled,
        }
        // biased + cancel first: when a cancel and a natural exit land on the same tick,
        // cancellation is the requested outcome — first-wins from the caller's view.
        let end = tokio::select! {
            biased;
            _ = cancel.cancelled() => End::Canceled,
            _ = tokio::time::sleep(Duration::from_millis(limits.timeout_ms.max(1))) => End::TimedOut,
            status = child.wait() => match status {
                Ok(s) => End::Exited(s.code().unwrap_or(-1)),
                Err(err) => End::WaitFailed(format!("wait failed: {err}")),
            },
        };

        let (timed_out, canceled) = match end {
            End::Exited(_) | End::WaitFailed(_) => (false, false),
            End::TimedOut => (true, false),
            End::Canceled => (false, true),
        };
        // Windows: release the kill-on-close guard BEFORE the bounded reap — the drop is
        // what terminates the whole tree (descendants still holding the pipes included),
        // and kill_tree only has to make sure the ROOT is gone and reaped either way.
        // Natural exits drop it just the same, so inherited pipes can close.
        #[cfg(windows)]
        drop(job);
        if timed_out || canceled || matches!(end, End::WaitFailed(_)) {
            kill_tree(&mut child).await;
        }

        // Drain: the process is gone by now, so the readers only wait for the pipes to
        // close (a straggler descendant is cut loose after the grace). Whichever way this
        // goes, both tasks are joined or aborted+reaped here.
        finish_reader(out_reader).await;
        finish_reader(err_reader).await;
        let (out_tail, out_total) = out_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_tail();
        let (err_tail, err_total) = err_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_tail();

        let mut stdout = decode_child_output(&out_tail);
        let stderr = decode_child_output(&err_tail);
        mask_secrets(&mut stdout, &spec.secrets);
        let mut combined = String::with_capacity(stdout.len() + stderr.len() + 10);
        combined.push_str(&stdout);
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("[stderr]\n");
            combined.push_str(&stderr);
        }
        let chars = (out_total + err_total) as usize;
        let kept = combined.chars().count();
        let mut result = ProcResult {
            ok: false,
            ms: started.elapsed().as_millis() as u64,
            pid,
            exit_code: None,
            timed_out,
            canceled,
            error: None,
            output_truncated: chars > kept,
            chars,
            output: combined,
        };
        match end {
            End::Exited(code) => {
                result.exit_code = Some(code);
                result.ok = code == 0;
            }
            End::WaitFailed(err) => result.error = Some(err),
            End::TimedOut | End::Canceled => {}
        }
        result
    }
}

/// Join one reader handle, aborting it when it is still alive after the drain grace (a
/// descendant holding the pipe must not hang the run) and reaping the aborted task so no
/// work is left unjoined.
async fn finish_reader(reader: Option<JoinHandle<()>>) {
    let Some(mut handle) = reader else { return };
    if tokio::time::timeout(DRAIN_GRACE, &mut handle)
        .await
        .is_err()
    {
        handle.abort();
        let _ = handle.await;
    }
}

/// Kill the whole subtree of a still-live child, then reap it so the OS carries no zombie.
/// Windows: the kill-on-close job handle (dropped by the caller just before this) already
/// terminated the tree; start_kill takes the root and the bounded wait reaps it.
/// Unix: kill(-pgid) takes the private group created at spawn — descendants at any depth;
/// the group dies with its last member, so the pid-reuse window is vanishingly narrow and
/// a stale signal simply returns ESRCH.
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) on a process group this run created at spawn; the return value is
        // ignored because the wait below is the source of truth for the root's death.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(KILL_WAIT, child.wait()).await;
}

/// Replace every masked-value occurrence in captured output with the panel's mask. Only
/// values of length >= 8 are considered secrets (short strings would over-redact: "1",
/// "true", short paths). Plain scanning keeps the regex engine out of the process.
pub fn mask_secrets(text: &mut String, secrets: &[String]) {
    for secret in secrets {
        if secret.len() >= 8 && text.contains(secret.as_str()) {
            *text = text.replace(secret.as_str(), crate::mask::MASK);
        }
    }
}

/// Resolve env references in ONE string, STRICTLY (typed process.exec input): a reference
/// to an unset variable is an ERROR naming the variable — never a silent empty string, and
/// never an inference to the current directory (docs/10 §3). Malformed text (an unclosed
/// or empty reference) stays literal, the same reading the shared lenient scanner uses.
pub fn resolve_refs_strict(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let mut j = i + 2;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' && j > i + 2 {
                let name = &value[i + 2..j];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        return Err(format!(
                            "environment variable {name} is not set (referenced as ${{{name}}})"
                        ))
                    }
                }
                i = j + 1;
                continue;
            }
        }
        // Advance one character (not one byte) so multi-byte text survives.
        let ch_len = value[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&value[i..i + ch_len]);
        i += ch_len;
    }
    Ok(out)
}

/// Split a command string into argv, honoring single/double quotes. On Windows a backslash is a
/// path separator, NOT an escape — so backslashes are kept literal and the only recognized
/// escapes inside double quotes are `\"` and `\\` (port of `tokenizeCommand`; hand-rolled, no
/// regex per the crate rules).
pub fn tokenize_command(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            // Inside double quotes, only \" and \\ are escapes; everything else (incl. \a) is
            // literal. Inside single quotes nothing is escaped at all.
            if c == '\\'
                && q == '"'
                && i + 1 < chars.len()
                && (chars[i + 1] == '"' || chars[i + 1] == '\\')
            {
                cur.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
                i += 1;
                continue;
            }
            cur.push(c);
        } else {
            if c == '"' || c == '\'' {
                quote = Some(c);
                i += 1;
                continue;
            }
            if c.is_whitespace() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                i += 1;
                continue;
            }
            cur.push(c);
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
/// Decode a child-process stderr chunk. Valid UTF-8 is kept (npx, Python, most MCP servers).
/// Bytes that are not UTF-8 — typical of cmd.exe on a Chinese Windows, CP936/GBK — are decoded
/// as GBK so the panel shows 「不是内部或外部命令」 instead of mojibake. ASCII-only English cmd
/// errors are valid UTF-8 and take the first branch.
pub fn decode_child_output(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    // The crate carries encoding_rs with default features off, so the alloc-gated convenience
    // `decode()` is unavailable — drive the streaming decoder by hand into a caller-allocated
    // buffer. Malformed sequences come back as U+FFFD, exactly what Node's non-fatal
    // TextDecoder("gbk") produced (a per-chunk decoder, like here, splits a multibyte char
    // across chunk boundaries the same way Node's did).
    let mut decoder = encoding_rs::GBK.new_decoder();
    let cap = decoder
        .max_utf8_buffer_length(bytes.len())
        .unwrap_or(bytes.len() * 3 + 3);
    let mut out = vec![0u8; cap];
    let (_result, _read, written, had_errors) = decoder.decode_to_utf8(bytes, &mut out, true);
    if had_errors {
        // Bytes GBK cannot decode either — the lossy-UTF-8 last resort (Node's third branch,
        // reachable there only when the "gbk" label itself was unsupported).
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        String::from_utf8_lossy(&out[..written]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::action::CancelSource;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn the_tail_buffer_keeps_only_the_last_max_bytes() {
        let mut cap = BoundedCapture::new(16);
        for i in 0u8..8 {
            cap.push(&[i; 8]); // 64 bytes total through a 16-byte window
        }
        let (tail, total) = cap.take_tail();
        assert_eq!(
            total, 64,
            "the count is of the FULL stream, not the retention"
        );
        // The window holds the last 16 bytes: the two newest chunks, in arrival order.
        let mut expected = vec![6u8; 8];
        expected.extend_from_slice(&[7u8; 8]);
        assert_eq!(tail, expected, "the newest bytes win, oldest first");
    }

    #[test]
    fn secret_masking_needs_a_worthwhile_value() {
        let mut text = "token=abcdef123456 end".to_string();
        mask_secrets(&mut text, &["abcdef123456".to_string()]);
        assert_eq!(text, format!("token={} end", crate::mask::MASK));
        // A short value is NOT masked: masking "1" would shred the whole output.
        let mut text = "n=1".to_string();
        mask_secrets(&mut text, &["1".to_string()]);
        assert_eq!(text, "n=1");
    }

    #[test]
    fn strict_env_resolution_fails_naming_the_variable() {
        unsafe { std::env::set_var("SWISS_SVC_STRICT_OK", "v") };
        assert_eq!(
            resolve_refs_strict("a-${SWISS_SVC_STRICT_OK}-b").unwrap(),
            "a-v-b"
        );
        let err = resolve_refs_strict("${SWISS_SVC_DEFINITELY_UNSET_9}").unwrap_err();
        assert!(err.contains("SWISS_SVC_DEFINITELY_UNSET_9"), "{err}");
        // Malformed stays literal, like the shared lenient scanner.
        let literal = "${} ${lower_x}";
        assert_eq!(resolve_refs_strict(literal).unwrap(), literal);
    }

    #[tokio::test]
    async fn a_reader_stops_promptly_on_cancellation() {
        // Cancel BEFORE any data: the reader must end promptly without reading, which is
        // the deterministic version of "cancel a wedged reader now".
        let (_tx, rx) = tokio::io::duplex(64);
        let capture = Arc::new(Mutex::new(BoundedCapture::new(1024)));
        let src = CancelSource::new();
        let reader = tokio::spawn(read_bounded(rx, capture.clone(), src.handle()));
        src.cancel();
        tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .expect("the reader stops on cancel")
            .expect("join");
        assert_eq!(capture.lock().unwrap().total(), 0);
    }

    #[tokio::test]
    async fn a_reader_drains_to_eof_even_past_the_capture_cap() {
        // 1 MiB through a 64-byte window: the pipe is drained (total counts everything)
        // while retention stays at the cap — the whole point of bounded WHILE reading.
        let (mut tx, rx) = tokio::io::duplex(64 * 1024);
        let capture = Arc::new(Mutex::new(BoundedCapture::new(64)));
        let reader = tokio::spawn(read_bounded(rx, capture.clone(), CancelHandle::never()));
        let payload = vec![b'x'; 1024 * 1024];
        tx.write_all(&payload).await.expect("write");
        drop(tx); // EOF
        tokio::time::timeout(Duration::from_secs(10), reader)
            .await
            .expect("drains promptly")
            .expect("join");
        let cap = capture.lock().unwrap();
        assert_eq!(cap.total(), payload.len() as u64, "everything was read");
        assert!(cap.retained() <= 64, "retention never exceeds the cap");
    }
}
