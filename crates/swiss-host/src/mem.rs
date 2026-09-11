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

use swiss_core::platform::{process_tree_working_set, self_private_bytes, self_working_set};

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
        "at": swiss_core::util::now_ms(),
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
            out["measuredAt"] = json!(swiss_core::util::now_ms());
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
    let now = swiss_core::util::now_ms();
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

#[cfg(test)]
mod tests {
    // Ported from test/mem.test.ts.
    //
    // The walk itself belongs to `platform` and is covered there; what these tests pin down is the
    // shape mem.ts promised the panel, and the cache in front of the walk. A planted cache entry
    // stands in for a measured subtree: this process cannot be its own child (the walker skips
    // its own pid by design), and spawning one just to be measured would put a subprocess in the
    // suite that the whole port exists to avoid.
    use super::*;

    /// The cache is process-wide, so the tests that plant or clear an entry take this first and
    /// start from an empty one. A panicking test must not lock the rest out.
    fn tree_cache() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        invalidate_memory_cache();
        guard
    }

    /// A pid nothing owns: the walk finds no such subtree, so anything that comes back for it came
    /// out of the cache.
    const GHOST: u32 = 2_147_483_647;

    /// Plant a measured subtree, as a walk `age_ms` ago would have left it.
    fn plant(key: &str, mb: f64, count: usize, age_ms: u64) {
        *cache().lock().expect("the cache is lockable") = Some((
            swiss_core::util::now_ms().saturating_sub(age_ms),
            key.to_string(),
            TreeResult { mb, count },
        ));
    }

    #[test]
    fn reports_megabytes_to_one_decimal() {
        // The panel renders these directly, so the rounding is part of the contract.
        assert_eq!(mb(0), 0.0);
        assert_eq!(mb(1_048_576), 1.0);
        assert_eq!(mb(1_572_864), 1.5);
        assert_eq!(mb(104_857_600), 100.0);
        // 1.0490... — rounded, not truncated, and never more than one decimal.
        assert_eq!(mb(1_100_000), 1.0);
        assert_eq!(mb(1_153_434), 1.1);
    }

    #[test]
    fn reports_the_gateways_own_footprint_without_walking_anything() {
        // The gateway's own numbers are read in-process: free, always current, no subprocess —
        // which is the whole reason this module exists in Rust rather than as a PowerShell query.
        let _lock = tree_cache();
        let info = get_memory_info(&[], false);

        assert!(info["gatewayMb"].as_f64().unwrap_or(0.0) > 0.0, "{info}");
        assert!(info["heapUsedMb"].as_f64().unwrap_or(0.0) > 0.0, "{info}");
        // No V8 here: both heap fields carry the private-commit figure, and nothing is external.
        assert_eq!(info["heapTotalMb"], info["heapUsedMb"]);
        assert_eq!(info["externalMb"], json!(0));
        assert_eq!(info["processCount"], json!(1));
        assert!(info["at"].as_u64().unwrap_or(0) > 0, "{info}");
    }

    #[test]
    fn a_gateway_with_no_children_reports_zero_rather_than_pending() {
        // Nothing was ever spawned, so "0 MB" is a fact — the panel renders it without a spinner.
        let _lock = tree_cache();
        let info = get_memory_info(&[], true);
        assert_eq!(info["childrenMb"], json!(0));
        assert_eq!(info["childrenPending"], json!(false));
        assert!(info.get("measuredAt").is_none(), "{info}");
    }

    #[test]
    fn the_panels_poll_never_pays_for_the_walk() {
        // The list poll passes false and gets `childrenPending`; only the memory chip's click
        // (and /api/mem?tree=1) asks for the walk.
        let _lock = tree_cache();
        let info = get_memory_info(&[GHOST], false);
        assert_eq!(info["childrenPending"], json!(true));
        assert!(info.get("childrenMb").is_none(), "{info}");
        assert!(info.get("measuredAt").is_none(), "{info}");
        assert_eq!(info["processCount"], json!(1));
    }

    #[test]
    fn a_walk_that_found_nothing_reports_pending_not_a_confident_zero() {
        // "0 MB across 0 processes" reads exactly like "there are no children", which is the one
        // thing it does not mean here: the children exist, they were just gone by walk time.
        let _lock = tree_cache();
        let info = get_memory_info(&[GHOST], true);
        assert_eq!(info["childrenPending"], json!(true));
        assert!(info.get("childrenMb").is_none(), "{info}");
        assert!(info.get("measuredAt").is_none(), "{info}");
    }

    #[test]
    fn counts_the_gateway_alongside_the_subtree_it_measured() {
        let _lock = tree_cache();
        plant(&GHOST.to_string(), 12.5, 2, 0);
        let info = get_memory_info(&[GHOST], true);

        assert_eq!(info["childrenPending"], json!(false));
        assert_eq!(info["childrenMb"], json!(12.5));
        // The gateway plus everything the walk found — never the children alone.
        assert_eq!(info["processCount"], json!(3));
        assert!(info["measuredAt"].as_u64().unwrap_or(0) > 0, "{info}");
        invalidate_memory_cache();
    }

    #[test]
    fn reuses_the_walk_within_the_cache_window_and_re_measures_after_an_invalidate() {
        // The walk is a system-call storm on a busy machine and the panel polls this endpoint.
        let _lock = tree_cache();
        plant(&GHOST.to_string(), 12.5, 2, 0);
        let hit = measure_tree(&[GHOST]).expect("the cached result stands in for a walk");
        assert_eq!(hit.mb, 12.5);
        assert_eq!(hit.count, 2);

        // An invalidate is what start/stop/restart calls; the next call must really walk, and for
        // a pid nothing owns that walk finds nothing.
        invalidate_memory_cache();
        assert!(measure_tree(&[GHOST]).is_none());
    }

    #[test]
    fn a_stale_entry_is_re_measured_rather_than_served() {
        let _lock = tree_cache();
        plant(&GHOST.to_string(), 12.5, 2, CACHE_MS + 1);
        assert!(measure_tree(&[GHOST]).is_none());
    }

    #[test]
    fn keys_the_cache_on_the_set_of_children_not_the_order_they_arrived_in() {
        // Adding an MCP must not be answered out of the cache; merely reordering the same pids
        // must not force a re-walk.
        let _lock = tree_cache();
        plant(&format!("1,{GHOST}"), 4321.0, 3, 0);

        assert_eq!(
            measure_tree(&[GHOST, 1]).map(|t| t.mb),
            Some(4321.0),
            "the same pids in another order are the same set"
        );
        assert!(
            measure_tree(&[GHOST]).is_none(),
            "a different set must be walked, not answered from the cache"
        );
    }

    #[test]
    fn the_gateway_is_never_its_own_child() {
        // The walker skips its own pid, so a stale record naming the gateway cannot make the
        // memory chip double-count the process it is describing.
        let _lock = tree_cache();
        assert!(swiss_core::platform::process_tree_working_set(&[std::process::id()]).is_none());
    }
}
