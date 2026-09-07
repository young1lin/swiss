//! The memory view behind the panel's memory chip — port of `mem.ts`.
//!
//! The Node build walked child subtrees by spawning a PowerShell (one is ~65 MB of working set
//! and ~350 ms of startup — the spike's whole motivation); this port walks the same tree with
//! direct Toolhelp32 syscalls (see `platform`), which costs neither. The JSON shape the panel
//! reads is unchanged field for field.
//!
//! Field mapping, honestly: `gatewayMb` is the working set of this process. There is no V8 here,
//! so the heap fields carry the private-commit figure instead (what the allocator asked the OS
//! for — the closest number Windows gives without allocator introspection), and `externalMb` is
//! 0. The panel only renders these in a tooltip; the numbers stay truthful and the shape stays
//! byte-identical.

use serde_json::{json, Value};

use crate::platform::{process_tree_working_set, self_private_bytes, self_working_set};

/// The JSON the admin API answers for `/api/mem` and `/api/mem?tree=1` — `MemoryInfo` in mem.ts,
/// camelCase with absent-not-null Optionals.
fn mb(bytes: u64) -> f64 {
    ((bytes as f64 / 1_048_576.0) * 10.0).round() / 10.0
}

/// Walked-tree state, cached (the walk itself is now cheap, but it is still a system call storm
/// on a busy machine, and the panel polls this endpoint).
#[derive(Clone)]
struct TreeResult {
    mb: f64,
    count: usize,
}

fn cache() -> &'static std::sync::Mutex<Option<(u64, String, TreeResult)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<(u64, String, TreeResult)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Walking the process tree is the expensive part, so a result is reused for this long.
const CACHE_MS: u64 = 20_000;

/// Drop the cached child measurement so the next walk re-measures (call after start/stop/restart).
pub fn invalidate_memory_cache() {
    if let Ok(mut c) = cache().lock() {
        *c = None;
    }
}

/// Report the gateway's memory footprint — port of `getMemoryInfo`.
///
/// The gateway's own numbers are read in-process — free and always current. Child subtrees are
/// only walked when proc-type MCPs actually exist AND the caller asks for it (`measure_children`):
/// the panel's poll passes false and gets `childrenPending: true`, and the memory chip's click
/// (and `/api/mem?tree=1`) passes true. A walk that found nothing reports `childrenPending`
/// rather than a confident-looking zero — "0 MB across 0 processes" reads exactly like "there
/// are no children".
pub fn get_memory_info(child_pids: &[u32], measure_children: bool) -> Value {
    let working = self_working_set();
    let private = self_private_bytes();
    let private = if private > 0 { private } else { working };
    let mut out = json!({
        "gatewayMb": mb(working),
        "heapUsedMb": mb(private),
        "heapTotalMb": mb(private),
        "externalMb": 0,
        "processCount": 1,
        "childrenPending": false,
        "at": crate::util::now_ms(),
    });

    if child_pids.is_empty() {
        out["childrenMb"] = json!(0);
        return out;
    }
    if !measure_children {
        out["childrenPending"] = json!(true);
        return out;
    }
    match measure_tree(child_pids) {
        None => out["childrenPending"] = json!(true),
        Some(tree) => {
            out["childrenMb"] = json!(tree.mb);
            out["processCount"] = json!(1 + tree.count);
            out["measuredAt"] = json!(crate::util::now_ms());
        }
    }
    out
}

fn measure_tree(roots: &[u32]) -> Option<TreeResult> {
    let key = {
        let mut sorted = roots.to_vec();
        sorted.sort_unstable();
        sorted
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let now = crate::util::now_ms();
    if let Ok(Some((at, cached_key, data))) = cache().lock().map(|c| c.clone()) {
        if cached_key == key && now.saturating_sub(at) < CACHE_MS {
            return Some(data);
        }
    }
    // Never a confident zero: the walk returns None when every sampled process was gone by walk
    // time, and get_memory_info turns that into childrenPending.
    let (bytes, count) = process_tree_working_set(roots)?;
    let data = TreeResult {
        mb: mb(bytes),
        count,
    };
    if let Ok(mut c) = cache().lock() {
        *c = Some((now, key, data.clone()));
    }
    Some(data)
}
