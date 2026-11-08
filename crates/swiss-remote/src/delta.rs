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

//! Delta upload (SPEC §remote.actions): send only what changed against a copy the
//! target already holds - the rsync algorithm, without needing rsync on either side.
//!
//! 1. The target cuts the base file into fixed blocks and answers one weak checksum
//!    (zlib's adler32) and one strong one (sha256, first 16 bytes) per block, plus the
//!    base's whole sha256. A python3 script sent as the exec's stdin does this: the
//!    remote half needs nothing installed but python3.
//! 2. The gateway rolls the weak checksum over the local file one byte at a time, so a
//!    block that merely moved (a rebuilt jar: one class grew, every library after it
//!    shifted) is still found. Each hit is confirmed by the strong checksum. What
//!    matches becomes a COPY of base blocks, the rest literal bytes, streamed into one
//!    patch file over the same chunked write a full upload uses.
//! 3. The same script rebuilds the file next to its destination from base + patch,
//!    checks the result's sha256 against the local file's, and only then renames it
//!    over the destination. A mismatch (the base changed meanwhile, a broken patch)
//!    leaves the destination untouched and the caller falls back to a whole upload.
//!
//! Wire formats, all integers big-endian:
//! - signature: `SWSG` 0x01, base length u64, block u32, block count u32, then per
//!   block weak u32 + strong 16 bytes, then the base's sha256 (32 bytes).
//! - patch: `SWDL` 0x01, block u32, base length u64, then ops - `C` first u32 count u32
//!   (copy `count` base blocks from block `first`), `L` length u32 + bytes (literal),
//!   `E` + the result's sha256 (end).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use swiss_host::services::action::CancelHandle;
use swiss_host::services::remote::{
    RemoteExecEvent, RemoteExecRequest, RemoteTransportRegistry, EXEC_EVENT_QUEUE,
    REMOTE_CHUNK_BYTES,
};

use crate::target::RemoteTarget;

/// Files smaller than this always go up whole: the two extra round trips a delta costs
/// are worth it only when the file itself takes seconds to send.
pub const DELTA_MIN_BYTES: u64 = 1024 * 1024;

/// The strong checksum's length: sha256 cut to 16 bytes. Collisions are not a
/// correctness risk anyway - the rebuilt file's full sha256 is checked before the
/// rename.
const STRONG_LEN: usize = 16;
const SIG_MAGIC: &[u8; 4] = b"SWSG";
const PATCH_MAGIC: &[u8; 4] = b"SWDL";
const FORMAT_VERSION: u8 = 1;
const SIG_HEAD: usize = 4 + 1 + 8 + 4 + 4;
const SIG_ROW: usize = 4 + STRONG_LEN;
const ADLER_MOD: u32 = 65521;
/// The longest literal one op carries; longer runs are cut into several ops.
const LITERAL_MAX: usize = 32 * 1024;
/// How much of the local file the rolling scan holds at once.
const SCAN_WINDOW: usize = 4 * 1024 * 1024;
/// A signature bigger than this is refused (a 128 KiB block makes it ~1/6500 of the
/// base, so this only trips on a target answering something else entirely).
const SIG_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Equal blocks (zero padding, a repeated resource) share one weak checksum; the scan
/// needs any one of them, so the index keeps a few and a degenerate file stays linear.
const MAX_CANDIDATES: usize = 16;
/// Bits in the weak-checksum prefilter: most windows that match nothing are rejected
/// by one bit test before any hash map lookup.
const FILTER_BITS: u32 = 20;

/// The remote half: `python3 - sig BASE BLOCK` answers the signature on stdout;
/// `python3 - apply BASE PATCH DEST` rebuilds DEST. Python 3.5+ only, standard library
/// only, so a stock RHEL/Debian/Ubuntu server runs it as is.
const REMOTE_SCRIPT: &str = r#"import hashlib, os, stat, struct, sys, tempfile, zlib

def sig(path, bs):
    out = sys.stdout.buffer
    whole = hashlib.sha256()
    with open(path, 'rb') as f:
        size = os.fstat(f.fileno()).st_size
        count = (size + bs - 1) // bs
        out.write(b'SWSG\x01' + struct.pack('>QII', size, bs, count))
        rows = []
        for _ in range(count):
            b = f.read(bs)
            if not b:
                break
            whole.update(b)
            rows.append(struct.pack('>I', zlib.adler32(b) & 0xffffffff) + hashlib.sha256(b).digest()[:16])
            if len(rows) >= 4096:
                out.write(b''.join(rows))
                rows = []
        out.write(b''.join(rows))
    out.write(whole.digest())
    out.flush()

def apply(base, patch, dest):
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(dest) or '.',
                               prefix='.' + os.path.basename(dest) + '.', suffix='.swiss-part')
    try:
        h = hashlib.sha256()
        want = b''
        with open(patch, 'rb') as p, open(base, 'rb') as b, os.fdopen(fd, 'wb') as o:
            head = p.read(17)
            if len(head) != 17 or head[:5] != b'SWDL\x01':
                raise SystemExit('swiss delta: not a patch file')
            bs, base_len = struct.unpack('>IQ', head[5:])
            if os.fstat(b.fileno()).st_size != base_len:
                raise SystemExit('swiss delta: the base file changed size')
            while True:
                op = p.read(1)
                if op == b'C':
                    first, count = struct.unpack('>II', p.read(8))
                    b.seek(first * bs)
                    left = count * bs
                    while left > 0:
                        chunk = b.read(min(left, 1 << 20))
                        if not chunk:
                            break
                        o.write(chunk)
                        h.update(chunk)
                        left -= len(chunk)
                elif op == b'L':
                    (n,) = struct.unpack('>I', p.read(4))
                    chunk = p.read(n)
                    if len(chunk) != n:
                        raise SystemExit('swiss delta: the patch is truncated')
                    o.write(chunk)
                    h.update(chunk)
                elif op == b'E':
                    want = p.read(32)
                    break
                else:
                    raise SystemExit('swiss delta: the patch is truncated')
            o.flush()
            os.fsync(o.fileno())
        if h.digest() != want:
            raise SystemExit('swiss delta: the rebuilt file does not match (sha256)')
        try:
            mode = stat.S_IMODE(os.stat(dest).st_mode)
        except FileNotFoundError:
            mask = os.umask(0)
            os.umask(mask)
            mode = 0o666 & ~mask
        os.chmod(tmp, mode)
        os.replace(tmp, dest)
        tmp = None
    finally:
        if tmp is not None:
            try:
                os.unlink(tmp)
            except OSError:
                pass
        try:
            os.unlink(patch)
        except OSError:
            pass

if sys.argv[1] == 'sig':
    sig(sys.argv[2], int(sys.argv[3]))
elif sys.argv[1] == 'apply':
    apply(sys.argv[2], sys.argv[3], sys.argv[4])
else:
    raise SystemExit('swiss delta: unknown mode')
"#;

/// The block size for a file of `len` bytes: about its square root (rsync's rule),
/// rounded up to a power of two, between 4 KiB and 128 KiB. Measured on a 171 MB
/// Spring Boot jar: 16 KiB sent 0.93 MB (signature + literals), 8 KiB 1.1 MB.
pub fn block_size_for(len: u64) -> u32 {
    let root = (len as f64).sqrt() as u64;
    root.max(1).next_power_of_two().clamp(4096, 128 * 1024) as u32
}

/// zlib's adler32, bit for bit (python's `zlib.adler32` computes the remote side).
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the longest run whose sums cannot overflow u32 before the modulo.
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= ADLER_MOD;
        b %= ADLER_MOD;
    }
    (b << 16) | a
}

fn strong(data: &[u8]) -> [u8; STRONG_LEN] {
    let digest = Sha256::digest(data);
    let mut out = [0u8; STRONG_LEN];
    out.copy_from_slice(&digest[..STRONG_LEN]);
    out
}

/// adler32 over a fixed-length window that slides one byte at a time.
#[derive(Clone, Copy)]
struct Rolling {
    a: u32,
    b: u32,
    len: u32,
}

impl Rolling {
    fn new(window: &[u8]) -> Self {
        let sum = adler32(window);
        Rolling {
            a: sum & 0xffff,
            b: sum >> 16,
            len: window.len() as u32,
        }
    }

    fn digest(&self) -> u32 {
        (self.b << 16) | self.a
    }

    /// Drop `out` from the front of the window and take `inn` at its end. With A the
    /// byte sum plus one and B the sum of every prefix's A: A' = A - out + in and
    /// B' = B - len*out - 1 + A' (mod 65521).
    fn roll(&mut self, out: u8, inn: u8) {
        let m = ADLER_MOD as u64;
        let a = (self.a as u64 + m - out as u64 + inn as u64) % m;
        let drop = (self.len as u64 * out as u64) % m;
        let b = (self.b as u64 + 2 * m - drop - 1 + a) % m;
        self.a = a as u32;
        self.b = b as u32;
    }
}

/// One base file's block signature, parsed from what the target answered.
pub struct Signature {
    base_len: u64,
    block: u32,
    weak: Vec<u32>,
    strong: Vec<[u8; STRONG_LEN]>,
}

impl Signature {
    pub fn parse(bytes: &[u8]) -> Result<Signature, String> {
        if bytes.len() < SIG_HEAD + 32 || &bytes[..4] != SIG_MAGIC {
            return Err("the target did not answer a block signature".into());
        }
        if bytes[4] != FORMAT_VERSION {
            return Err(format!("unknown signature version {}", bytes[4]));
        }
        let base_len = u64::from_be_bytes(bytes[5..13].try_into().unwrap_or_default());
        let block = u32::from_be_bytes(bytes[13..17].try_into().unwrap_or_default());
        let count = u32::from_be_bytes(bytes[17..21].try_into().unwrap_or_default()) as usize;
        if block == 0 || base_len.div_ceil(block as u64) != count as u64 {
            return Err("the block signature does not add up".into());
        }
        if bytes.len() != SIG_HEAD + count * SIG_ROW + 32 {
            // The base shrank while it was read, or the output was cut.
            return Err("the block signature is truncated".into());
        }
        let mut weak = Vec::with_capacity(count);
        let mut strong = Vec::with_capacity(count);
        for row in bytes[SIG_HEAD..SIG_HEAD + count * SIG_ROW].chunks_exact(SIG_ROW) {
            weak.push(u32::from_be_bytes(row[..4].try_into().unwrap_or_default()));
            let mut s = [0u8; STRONG_LEN];
            s.copy_from_slice(&row[4..]);
            strong.push(s);
        }
        // The trailing 32 bytes are the base's sha256: the apply step checks the
        // result, so the scan has no use for it.
        Ok(Signature {
            base_len,
            block,
            weak,
            strong,
        })
    }

    fn block_len(&self, index: usize) -> usize {
        let start = index as u64 * self.block as u64;
        self.base_len.saturating_sub(start).min(self.block as u64) as usize
    }

    /// The base's last block when it is shorter than the others: a window of full
    /// length never matches it, so the scan checks it once against the file's end.
    fn short_tail(&self) -> Option<usize> {
        let last = self.weak.len().checked_sub(1)?;
        (self.block_len(last) < self.block as usize).then_some(last)
    }
}

/// The weak-checksum lookup the scan asks once per window.
struct Index {
    filter: Vec<u64>,
    blocks: HashMap<u32, Vec<u32>>,
}

impl Index {
    fn new(sig: &Signature) -> Self {
        let mut filter = vec![0u64; (1usize << FILTER_BITS) / 64];
        let mut blocks: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, &weak) in sig.weak.iter().enumerate() {
            if sig.block_len(i) != sig.block as usize {
                continue; // the short tail is checked on its own
            }
            let slot = Self::slot(weak);
            filter[slot / 64] |= 1 << (slot % 64);
            let list = blocks.entry(weak).or_default();
            if list.len() < MAX_CANDIDATES {
                list.push(i as u32);
            }
        }
        Index { filter, blocks }
    }

    fn slot(weak: u32) -> usize {
        (weak.wrapping_mul(0x9E37_79B1) >> (32 - FILTER_BITS)) as usize
    }

    /// The base block equal to `window`, preferring the block right after the last
    /// copied one so consecutive matches merge into one COPY op.
    fn find(&self, sig: &Signature, weak: u32, window: &[u8], after: Option<u32>) -> Option<u32> {
        let slot = Self::slot(weak);
        if self.filter[slot / 64] & (1 << (slot % 64)) == 0 {
            return None;
        }
        let candidates = self.blocks.get(&weak)?;
        let sum = strong(window);
        if let Some(next) = after.map(|i| i as usize + 1) {
            if next < sig.weak.len()
                && sig.weak[next] == weak
                && sig.block_len(next) == window.len()
                && sig.strong[next] == sum
            {
                return Some(next as u32);
            }
        }
        candidates
            .iter()
            .copied()
            .find(|&i| sig.strong[i as usize] == sum)
    }
}

/// What one encode produced.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DeltaStats {
    /// The local file's length (copied + literal).
    pub len: u64,
    /// Bytes the target copies out of its base.
    pub copied: u64,
    /// Bytes the patch carries as literals.
    pub literal: u64,
    /// The patch's own size, ops and headers included.
    pub patch_bytes: u64,
    pub sha256: [u8; 32],
}

/// The patch being written: ops accumulate in `buf` and leave in chunks of at most
/// [REMOTE_CHUNK_BYTES], the transport seam's ceiling.
struct PatchOut<F: FnMut(Vec<u8>) -> Result<(), String>> {
    buf: Vec<u8>,
    sink: F,
    written: u64,
    run: Option<(u32, u32)>,
}

impl<F: FnMut(Vec<u8>) -> Result<(), String>> PatchOut<F> {
    fn new(sink: F, sig: &Signature) -> Self {
        let mut buf = Vec::with_capacity(2 * REMOTE_CHUNK_BYTES);
        buf.extend_from_slice(PATCH_MAGIC);
        buf.push(FORMAT_VERSION);
        buf.extend_from_slice(&sig.block.to_be_bytes());
        buf.extend_from_slice(&sig.base_len.to_be_bytes());
        PatchOut {
            buf,
            sink,
            written: 0,
            run: None,
        }
    }

    fn copy(&mut self, index: u32) -> Result<(), String> {
        if let Some((first, count)) = &mut self.run {
            if *first + *count == index {
                *count += 1;
                return Ok(());
            }
        }
        self.end_run()?;
        self.run = Some((index, 1));
        Ok(())
    }

    fn literal(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.end_run()?;
        for piece in bytes.chunks(LITERAL_MAX) {
            self.buf.push(b'L');
            self.buf
                .extend_from_slice(&(piece.len() as u32).to_be_bytes());
            self.buf.extend_from_slice(piece);
            self.spill(false)?;
        }
        Ok(())
    }

    fn end_run(&mut self) -> Result<(), String> {
        if let Some((first, count)) = self.run.take() {
            self.buf.push(b'C');
            self.buf.extend_from_slice(&first.to_be_bytes());
            self.buf.extend_from_slice(&count.to_be_bytes());
            self.spill(false)?;
        }
        Ok(())
    }

    fn spill(&mut self, all: bool) -> Result<(), String> {
        while self.buf.len() >= REMOTE_CHUNK_BYTES || (all && !self.buf.is_empty()) {
            let take = self.buf.len().min(REMOTE_CHUNK_BYTES);
            let chunk: Vec<u8> = self.buf.drain(..take).collect();
            self.written += chunk.len() as u64;
            (self.sink)(chunk)?;
        }
        Ok(())
    }

    fn finish(mut self, sha256: &[u8; 32]) -> Result<u64, String> {
        self.end_run()?;
        self.buf.push(b'E');
        self.buf.extend_from_slice(sha256);
        self.spill(true)?;
        Ok(self.written)
    }
}

fn read_some(reader: &mut impl Read, buf: &mut [u8]) -> Result<usize, String> {
    loop {
        match reader.read(buf) {
            Ok(n) => return Ok(n),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("read: {err}")),
        }
    }
}

/// Scan `reader` (the new file) against `sig` (the base) and stream the patch into
/// `sink`. Holds at most [SCAN_WINDOW] of the file at once, however big it is.
pub fn encode(
    reader: &mut impl Read,
    sig: &Signature,
    sink: impl FnMut(Vec<u8>) -> Result<(), String>,
) -> Result<DeltaStats, String> {
    let block = sig.block as usize;
    let index = Index::new(sig);
    let mut out = PatchOut::new(sink, sig);
    let cap = SCAN_WINDOW.max(block * 4);
    let mut buf = vec![0u8; cap];
    // buf[..filled] holds file bytes; the window starts at pos; buf[lit..pos] is the
    // literal run not yet written.
    let (mut filled, mut pos, mut lit) = (0usize, 0usize, 0usize);
    let mut eof = false;
    let mut hasher = Sha256::new();
    let mut stats = DeltaStats::default();
    let mut rolling: Option<Rolling> = None;
    let mut last_copy: Option<u32> = None;
    loop {
        // Rolling needs the window plus the byte after it.
        if !eof && filled - pos <= block {
            out.literal(&buf[lit..pos])?;
            stats.literal += (pos - lit) as u64;
            buf.copy_within(pos..filled, 0);
            filled -= pos;
            pos = 0;
            lit = 0;
            while filled < cap && !eof {
                let n = read_some(reader, &mut buf[filled..])?;
                if n == 0 {
                    eof = true;
                } else {
                    hasher.update(&buf[filled..filled + n]);
                    filled += n;
                }
            }
        }
        if filled - pos < block {
            break;
        }
        let window = &buf[pos..pos + block];
        let r = rolling.take().unwrap_or_else(|| Rolling::new(window));
        if let Some(i) = index.find(sig, r.digest(), window, last_copy) {
            out.literal(&buf[lit..pos])?;
            stats.literal += (pos - lit) as u64;
            out.copy(i)?;
            stats.copied += block as u64;
            last_copy = Some(i);
            pos += block;
            lit = pos;
            continue;
        }
        if pos + block < filled {
            let mut r = r;
            r.roll(buf[pos], buf[pos + block]);
            rolling = Some(r);
            pos += 1;
            if pos - lit >= LITERAL_MAX {
                out.literal(&buf[lit..pos])?;
                stats.literal += (pos - lit) as u64;
                lit = pos;
            }
        } else if eof {
            break;
        } else {
            rolling = Some(r); // the refill above keeps the window's bytes
        }
    }
    // What is left is literal, except that its end may be the base's short last block.
    if let Some(tail) = sig.short_tail() {
        let len = sig.block_len(tail);
        if len > 0 && filled - lit >= len {
            let end = &buf[filled - len..filled];
            if adler32(end) == sig.weak[tail] && strong(end) == sig.strong[tail] {
                out.literal(&buf[lit..filled - len])?;
                stats.literal += (filled - len - lit) as u64;
                out.copy(tail as u32)?;
                stats.copied += len as u64;
                lit = filled;
            }
        }
    }
    out.literal(&buf[lit..filled])?;
    stats.literal += (filled - lit) as u64;
    let sha256: [u8; 32] = hasher.finalize().into();
    stats.len = stats.copied + stats.literal;
    stats.sha256 = sha256;
    stats.patch_bytes = out.finish(&sha256)?;
    Ok(stats)
}

/// Why a delta upload did not happen. Every variant but `Canceled` sends the caller
/// to the whole-file upload, which is always correct, only slower.
#[derive(Debug)]
pub enum DeltaError {
    /// The target has no python3: no point trying again for the rest of this sync.
    NoPython,
    /// Anything else that went wrong; the reason is for the run's output.
    Fallback(String),
    Canceled,
}

/// One delta upload's account.
#[derive(Debug, Clone)]
pub struct DeltaSent {
    /// Bytes that crossed the link: the signature down plus the patch up.
    pub wire_bytes: u64,
    pub stats: DeltaStats,
}

/// Upload `local` to `remote_path` as a delta against `base_path`, a file already on
/// the target (the destination's previous version, or a sibling). On any failure the
/// destination is exactly as it was.
pub async fn upload_delta(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    local: &Path,
    remote_path: &str,
    base_path: &str,
    cancel: &CancelHandle,
) -> Result<DeltaSent, DeltaError> {
    let local_len = std::fs::metadata(local)
        .map_err(|err| DeltaError::Fallback(format!("read {}: {err}", local.display())))?
        .len();
    let block = block_size_for(local_len);
    let (code, sig_bytes, stderr) = run_script(
        registry,
        target,
        holder,
        vec!["sig".into(), base_path.into(), block.to_string()],
        SIG_MAX_BYTES,
        cancel,
    )
    .await?;
    if code == 127 {
        return Err(DeltaError::NoPython);
    }
    if code != 0 {
        return Err(DeltaError::Fallback(format!(
            "the block signature failed (exit {code}): {stderr}"
        )));
    }
    let sig = Signature::parse(&sig_bytes).map_err(DeltaError::Fallback)?;
    if sig.block != block {
        return Err(DeltaError::Fallback("the block signature does not add up".into()));
    }
    if cancel.is_cancelled() {
        return Err(DeltaError::Canceled);
    }

    // The patch lands beside the destination under a dot name the apply step removes.
    let (dir, name) = match remote_path.rsplit_once('/') {
        Some((dir, name)) => (if dir.is_empty() { "/" } else { dir }, name),
        None => (".", remote_path),
    };
    let patch_path = format!(
        "{}/.{name}.{}.swiss-delta",
        dir.trim_end_matches('/'),
        swiss_core::util::random_hex(4)
    );
    let sent = match send_patch(registry, target, holder, local, &patch_path, sig, cancel).await {
        Ok(stats) => stats,
        Err(err) => {
            remove_quietly(registry, target, holder, &patch_path).await;
            return Err(err);
        }
    };
    let (code, _, stderr) = run_script(
        registry,
        target,
        holder,
        vec![
            "apply".into(),
            base_path.into(),
            patch_path.clone(),
            remote_path.into(),
        ],
        64 * 1024,
        cancel,
    )
    .await?;
    if code != 0 {
        // The script removes its patch itself; this covers a script that never ran.
        remove_quietly(registry, target, holder, &patch_path).await;
        return Err(DeltaError::Fallback(format!(
            "rebuilding {remote_path} failed (exit {code}): {stderr}"
        )));
    }
    Ok(DeltaSent {
        wire_bytes: sig_bytes.len() as u64 + sent.patch_bytes,
        stats: sent,
    })
}

/// Encode on a blocking thread (the scan is CPU work over the whole file) and write
/// its chunks to the patch file as they come.
async fn send_patch(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    local: &Path,
    patch_path: &str,
    sig: Signature,
    cancel: &CancelHandle,
) -> Result<DeltaStats, DeltaError> {
    let mut writer = registry
        .create(&target.endpoint, holder, patch_path)
        .await
        .map_err(|err| DeltaError::Fallback(format!("create {patch_path}: {err}")))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    let local = local.to_path_buf();
    let encoder = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&local)
            .map_err(|err| format!("read {}: {err}", local.display()))?;
        encode(&mut file, &sig, |chunk| {
            tx.blocking_send(chunk)
                .map_err(|_| "the patch upload stopped".to_string())
        })
    });
    let mut write_error: Option<String> = None;
    while let Some(chunk) = rx.recv().await {
        if cancel.is_cancelled() {
            return Err(DeltaError::Canceled); // dropping rx stops the encoder
        }
        if let Err(err) = writer.write_chunk(&chunk).await {
            write_error = Some(format!("write {patch_path}: {err}"));
            break;
        }
    }
    drop(rx);
    let encoded = encoder
        .await
        .map_err(|err| DeltaError::Fallback(format!("the delta scan stopped: {err}")))?;
    if let Some(err) = write_error {
        return Err(DeltaError::Fallback(err));
    }
    let stats = encoded.map_err(DeltaError::Fallback)?;
    writer
        .finish()
        .await
        .map_err(|err| DeltaError::Fallback(format!("commit {patch_path}: {err}")))?;
    Ok(stats)
}

/// Run the remote script in one mode; answers (exit code, stdout, stderr's first line).
async fn run_script(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    args: Vec<String>,
    stdout_cap: usize,
    cancel: &CancelHandle,
) -> Result<(i32, Vec<u8>, String), DeltaError> {
    let mut argv = vec!["python3".to_string(), "-".to_string()];
    argv.extend(args);
    let request = RemoteExecRequest {
        argv,
        env: Vec::new(),
        cwd: None,
        stdin: Some(REMOTE_SCRIPT.to_string()),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(EXEC_EVENT_QUEUE);
    let exec = registry.exec(&target.endpoint, holder, request, tx, cancel.clone());
    tokio::pin!(exec);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut overflow = false;
    let mut take = |event: RemoteExecEvent| match event {
        RemoteExecEvent::Stdout(bytes) => {
            if stdout.len() + bytes.len() > stdout_cap {
                overflow = true;
            } else {
                stdout.extend_from_slice(&bytes);
            }
        }
        RemoteExecEvent::Stderr(bytes) => {
            if stderr.len() < 4096 {
                stderr.extend_from_slice(&bytes);
            }
        }
    };
    let mut open = true;
    let result = loop {
        tokio::select! {
            result = &mut exec => break result,
            event = rx.recv(), if open => match event {
                Some(event) => take(event),
                None => open = false,
            },
        }
    };
    while let Ok(event) = rx.try_recv() {
        take(event);
    }
    let end = match result {
        Ok(end) => end,
        Err(swiss_host::services::remote::RemoteError::Canceled(_)) => {
            return Err(DeltaError::Canceled)
        }
        Err(err) => return Err(DeltaError::Fallback(format!("the delta step failed: {err}"))),
    };
    if overflow {
        return Err(DeltaError::Fallback(
            "the target answered more than a block signature".into(),
        ));
    }
    let reason = String::from_utf8_lossy(&stderr)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("no error text")
        .to_string();
    Ok((end.exit_code, stdout, reason))
}

async fn remove_quietly(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    path: &str,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(EXEC_EVENT_QUEUE);
    let request = RemoteExecRequest {
        argv: vec!["rm".into(), "-f".into(), "--".into(), path.into()],
        env: Vec::new(),
        cwd: None,
        stdin: None,
    };
    let drain = async { while rx.recv().await.is_some() {} };
    let exec = registry.exec(
        &target.endpoint,
        holder,
        request,
        tx,
        CancelHandle::never(),
    );
    let _ = tokio::join!(exec, drain);
}

/// The remote half in Rust, for the fake transport and the tests: the same bytes
/// the python script answers and the same rebuild it performs.
#[cfg(any(test, feature = "test-utils"))]
pub mod mirror {
    use super::*;

    /// What `python3 - sig` prints for `base`.
    pub fn signature(base: &[u8], block: u32) -> Vec<u8> {
        let count = (base.len() as u64).div_ceil(block as u64) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(SIG_MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&(base.len() as u64).to_be_bytes());
        out.extend_from_slice(&block.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        for chunk in base.chunks(block as usize) {
            out.extend_from_slice(&adler32(chunk).to_be_bytes());
            out.extend_from_slice(&strong(chunk));
        }
        out.extend_from_slice(&Sha256::digest(base));
        out
    }

    /// What `python3 - apply` writes for `base` + `patch`, or why it refuses.
    pub fn apply(base: &[u8], patch: &[u8]) -> Result<Vec<u8>, String> {
        let bad = || "swiss delta: the patch is truncated".to_string();
        if patch.len() < 17 || &patch[..4] != PATCH_MAGIC || patch[4] != FORMAT_VERSION {
            return Err("swiss delta: not a patch file".into());
        }
        let block = u32::from_be_bytes(patch[5..9].try_into().map_err(|_| bad())?) as usize;
        let base_len = u64::from_be_bytes(patch[9..17].try_into().map_err(|_| bad())?);
        if base.len() as u64 != base_len {
            return Err("swiss delta: the base file changed size".into());
        }
        let mut at = 17;
        let mut out = Vec::new();
        let u32_at = |at: usize| -> Result<u32, String> {
            Ok(u32::from_be_bytes(
                patch.get(at..at + 4).ok_or_else(bad)?.try_into().map_err(|_| bad())?,
            ))
        };
        loop {
            let op = *patch.get(at).ok_or_else(bad)?;
            at += 1;
            match op {
                b'C' => {
                    let first = u32_at(at)? as usize;
                    let count = u32_at(at + 4)? as usize;
                    at += 8;
                    let start = (first * block).min(base.len());
                    let end = ((first + count) * block).min(base.len());
                    out.extend_from_slice(&base[start..end]);
                }
                b'L' => {
                    let n = u32_at(at)? as usize;
                    at += 4;
                    out.extend_from_slice(patch.get(at..at + n).ok_or_else(bad)?);
                    at += n;
                }
                b'E' => {
                    let want = patch.get(at..at + 32).ok_or_else(bad)?;
                    if Sha256::digest(&out).as_slice() != want {
                        return Err(
                            "swiss delta: the rebuilt file does not match (sha256)".into()
                        );
                    }
                    return Ok(out);
                }
                _ => return Err(bad()),
            }
        }
    }

    /// The script text, for the test that runs it under a real python.
    pub fn script() -> &'static str {
        REMOTE_SCRIPT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic bytes that do not repeat: xorshift64.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    /// Encode `new` against `base`, check every patch chunk fits the transport seam,
    /// rebuild through the mirror and insist on the exact bytes.
    fn roundtrip(base: &[u8], new: &[u8], block: u32) -> DeltaStats {
        let sig = Signature::parse(&mirror::signature(base, block)).expect("the signature parses");
        let mut patch = Vec::new();
        let stats = encode(&mut &new[..], &sig, |chunk| {
            assert!(chunk.len() <= REMOTE_CHUNK_BYTES, "chunk of {}", chunk.len());
            patch.extend_from_slice(&chunk);
            Ok(())
        })
        .expect("the encode runs");
        assert_eq!(stats.patch_bytes, patch.len() as u64);
        assert_eq!(stats.len, new.len() as u64);
        let rebuilt = mirror::apply(base, &patch).expect("the patch applies");
        assert!(rebuilt == new, "the rebuild differs from the new file");
        stats
    }

    #[test]
    fn adler32_is_zlibs() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        // Past one 5552-byte modulo run, where an overflow would show.
        let long = vec![0xffu8; 20_000];
        let (mut a, mut b) = (1u64, 0u64);
        for &x in &long {
            a = (a + x as u64) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(&long), ((b << 16) | a) as u32);
    }

    #[test]
    fn the_rolling_checksum_equals_a_fresh_one_at_every_offset() {
        let data = noise(5000, 7);
        let width = 257;
        let mut rolling = Rolling::new(&data[..width]);
        for at in 0..data.len() - width {
            assert_eq!(rolling.digest(), adler32(&data[at..at + width]), "offset {at}");
            rolling.roll(data[at], data[at + width]);
        }
    }

    #[test]
    fn the_block_size_follows_the_square_root_within_bounds() {
        assert_eq!(block_size_for(0), 4096);
        assert_eq!(block_size_for(1 << 20), 4096);
        assert_eq!(block_size_for(171_634_451), 16384);
        assert_eq!(block_size_for(1 << 40), 128 * 1024);
    }

    #[test]
    fn an_identical_file_is_all_copies() {
        let base = noise(100_000, 1); // 24 full blocks and a short tail
        let stats = roundtrip(&base, &base, 4096);
        assert_eq!(stats.literal, 0);
        assert_eq!(stats.copied, base.len() as u64);
        assert!(stats.patch_bytes < 100, "one copy run, got {}", stats.patch_bytes);
    }

    #[test]
    fn shifted_content_is_found_again() {
        let base = noise(200_000, 2);
        // Bytes inserted near the front shift everything after them, which is what a
        // rebuilt jar looks like: the rolling checksum must find the shifted blocks.
        let mut new = noise(1000, 3);
        new.extend_from_slice(&base);
        let stats = roundtrip(&base, &new, 4096);
        assert_eq!(stats.literal, 1000, "only the inserted bytes travel");

        // An edit in the middle costs at most the blocks it touches.
        let mut edited = base.clone();
        edited[100_000..100_010].copy_from_slice(&[0u8; 10]);
        let stats = roundtrip(&base, &edited, 4096);
        assert!(stats.literal <= 2 * 4096, "literal {}", stats.literal);

        // Removed bytes: the rest still matches.
        let mut shorter = base[..50_000].to_vec();
        shorter.extend_from_slice(&base[60_000..]);
        let stats = roundtrip(&base, &shorter, 4096);
        assert!(stats.literal <= 2 * 4096, "literal {}", stats.literal);
    }

    #[test]
    fn the_bases_short_last_block_is_reused_at_the_end() {
        let base = noise(10_000, 4); // two full blocks and a 1808-byte tail
        let mut new = b"prefix".to_vec();
        new.extend_from_slice(&base);
        let stats = roundtrip(&base, &new, 4096);
        assert_eq!(stats.literal, 6, "the short tail is copied, not resent");
    }

    #[test]
    fn edge_sizes_rebuild_exactly() {
        let base = noise(50_000, 5);
        roundtrip(&base, b"", 4096);
        roundtrip(&base, b"short", 4096);
        roundtrip(b"", &base, 4096);
        roundtrip(b"", b"", 4096);
        let unrelated = noise(50_000, 6);
        let stats = roundtrip(&base, &unrelated, 4096);
        assert_eq!(stats.copied, 0);
        // Appended bytes after the base's short tail.
        let mut appended = base.clone();
        appended.extend_from_slice(&noise(3000, 8));
        let stats = roundtrip(&base, &appended, 4096);
        assert!(stats.literal <= 3000 + 4096, "literal {}", stats.literal);
    }

    #[test]
    fn equal_blocks_merge_into_one_copy_run() {
        let base = vec![0u8; 20 * 4096];
        let new = vec![0u8; 10 * 4096];
        let stats = roundtrip(&base, &new, 4096);
        assert_eq!(stats.literal, 0);
        assert!(stats.patch_bytes < 100, "one copy run, got {}", stats.patch_bytes);
    }

    #[test]
    fn a_file_bigger_than_the_scan_window_rebuilds_exactly() {
        let base = noise(SCAN_WINDOW + SCAN_WINDOW / 2, 9);
        let mut new = base.clone();
        // One edit straddles the first refill, one sits on each side of it.
        for at in [1000, SCAN_WINDOW - 50, SCAN_WINDOW + 777_777] {
            new[at..at + 100].copy_from_slice(&[0xAB; 100]);
        }
        new.splice(2_000_000..2_000_000, noise(5000, 10));
        let stats = roundtrip(&base, &new, 16384);
        assert!(stats.literal < 10 * 16384, "literal {}", stats.literal);
    }

    #[test]
    fn a_broken_signature_or_patch_is_refused() {
        let base = noise(10_000, 11);
        let sig = mirror::signature(&base, 4096);
        assert!(Signature::parse(&sig[..sig.len() - 1]).is_err(), "truncated");
        assert!(Signature::parse(b"Traceback (most recent call last)").is_err());
        let mut wrong = sig.clone();
        wrong[0] = b'X';
        assert!(Signature::parse(&wrong).is_err(), "bad magic");

        let parsed = Signature::parse(&sig).expect("parses");
        let mut new = b"0123456789".to_vec();
        new.extend_from_slice(&base);
        let mut patch = Vec::new();
        encode(&mut &new[..], &parsed, |chunk| {
            patch.extend_from_slice(&chunk);
            Ok(())
        })
        .expect("encodes");
        // Flip one literal byte: the rebuild's sha256 no longer matches.
        // The first op after the 17-byte header is the leading literal.
        assert_eq!(patch[17], b'L');
        let at = 17 + 5;
        patch[at] ^= 1;
        let err = mirror::apply(&base, &patch).expect_err("a corrupted patch is refused");
        assert!(err.contains("sha256"), "{err}");
        // A base that changed size is refused before anything is written.
        let err = mirror::apply(&base[1..], &patch).expect_err("a resized base is refused");
        assert!(err.contains("changed size"), "{err}");
    }

    /// The python interpreter on this machine, when there is one: the cross-check
    /// below skips (and says so) without it.
    fn python() -> Option<String> {
        ["python3", "python"].into_iter().find_map(|name| {
            let out = std::process::Command::new(name).arg("--version").output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr);
            (out.status.success() && text.starts_with("Python 3")).then(|| name.to_string())
        })
    }

    fn run_python(python: &str, args: &[&str]) -> std::process::Output {
        use std::io::Write;
        let mut child = std::process::Command::new(python)
            .arg("-")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("python starts");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(mirror::script().as_bytes())
            .expect("the script goes in");
        child.wait_with_output().expect("python ends")
    }

    #[test]
    fn the_python_script_agrees_with_the_rust_mirror() {
        let Some(python) = python() else {
            eprintln!("skipped: no python 3 on this machine");
            return;
        };
        let dir = std::env::temp_dir().join(format!("swiss-delta-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let base = noise(70_000, 12);
        let base_path = dir.join("app-1.jar");
        std::fs::write(&base_path, &base).expect("base");

        // sig: byte for byte what the Rust side parses.
        let out = run_python(&python, &["sig", &base_path.to_string_lossy(), "4096"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == mirror::signature(&base, 4096), "the signatures differ");

        // apply: the rebuild lands at dest, the patch is gone, no temporary is left.
        let mut new = noise(3000, 13);
        new.extend_from_slice(&base[10_000..]);
        let sig = Signature::parse(&out.stdout).expect("parses");
        let mut patch = Vec::new();
        encode(&mut &new[..], &sig, |chunk| {
            patch.extend_from_slice(&chunk);
            Ok(())
        })
        .expect("encodes");
        let patch_path = dir.join(".app-2.jar.0000.swiss-delta");
        let dest = dir.join("app-2.jar");
        std::fs::write(&patch_path, &patch).expect("patch");
        let paths = [
            "apply".to_string(),
            base_path.to_string_lossy().into_owned(),
            patch_path.to_string_lossy().into_owned(),
            dest.to_string_lossy().into_owned(),
        ];
        let args: Vec<&str> = paths.iter().map(String::as_str).collect();
        let out = run_python(&python, &args);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(std::fs::read(&dest).expect("dest") == new, "the python rebuild differs");
        assert!(!patch_path.exists(), "the patch is removed");

        // A corrupted patch fails and leaves the destination as it was.
        // The first op after the 17-byte header is the leading literal.
        assert_eq!(patch[17], b'L');
        let at = 17 + 5;
        patch[at] ^= 1;
        std::fs::write(&patch_path, &patch).expect("patch");
        let out = run_python(&python, &args);
        assert!(!out.status.success(), "a corrupted patch must fail");
        assert!(String::from_utf8_lossy(&out.stderr).contains("sha256"));
        assert!(std::fs::read(&dest).expect("dest") == new, "dest is untouched");
        let left: Vec<_> = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "temporaries left behind: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
