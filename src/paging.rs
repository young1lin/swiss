//! Incremental page caches for tools/resources/prompts browsing — port of `paging.ts`.

use std::sync::Arc;

use base64::Engine;
use serde_json::Value;

use crate::adapters::McpProbe;

/// Gateway page size for tools/resources browsing.
pub const PAGE_SIZE: usize = 50;

/// Ceiling on server round trips inside one fill.
///
/// Following `nextCursor` is an unbounded loop driven entirely by the far side, so a server that
/// never exhausts itself would spin here and grow `acc` until the process dies. The cap turns that
/// into a truncated list and a log line. 200 pages is far past any real server — a schema with
/// thousands of table resources arrives in a single dump, and a remote that paginates sends tens
/// per page.
const MAX_FILL_PAGES: usize = 200;

/// An idle page cache is dropped after this long. A server that dumps everything at once (a mysql
/// schema with thousands of table resources) otherwise keeps that whole list resident for the
/// lifetime of the process.
pub const PAGE_CACHE_TTL_MS: u64 = 60_000;

/// Which list kind a page cache serves.
const KINDS: [&str; 3] = ["tools", "resources", "prompts"];

/// The cache's mutable state, behind a short std lock: fills hold the FILL lock across their
/// awaits, so this lock only guards synchronous snapshots and mutations.
struct PageState {
    acc: Vec<Value>,
    server_next: Option<String>,
    started: bool,
    touched_at: u64,
}

/// Incremental, server-side page cache — port of `PageCache`. `acc` grows as the user pages
/// forward; `server_next` is the MCP cursor to continue fetching from the underlying server (None
/// once exhausted). Lives on the registry entry so it persists across page requests and is cleared
/// on (re)start. The fill lock serializes fills (two panel tabs paging the same MCP+kind).
pub struct PageCache {
    state: std::sync::Mutex<PageState>,
    fill: tokio::sync::Mutex<()>,
}

impl Default for PageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PageCache {
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(PageState {
                acc: Vec::new(),
                server_next: None,
                started: false,
                touched_at: crate::util::now_ms(),
            }),
            fill: tokio::sync::Mutex::new(()),
        }
    }
}

pub fn is_page_cache_stale(cache: &PageCache, now: u64) -> bool {
    now - cache.state.lock().map(|s| s.touched_at).unwrap_or(0) > PAGE_CACHE_TTL_MS
}

/// Encode/decode the gateway cursor as an opaque offset token (base64url, URL-safe, unpadded —
/// the exact `Buffer.toString("base64url")` wire format, so cursors survive a Node→Rust cutover).
fn enc_offset(o: usize) -> String {
    let json = format!("{{\"o\":{o}}}");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
}
fn dec_offset(cursor: Option<&str>) -> usize {
    let Some(cursor) = cursor else { return 0 };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .ok();
    let o = bytes
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("o").and_then(Value::as_i64));
    // `| 0` in the Node build truncates toward zero AND maps NaN to 0; a negative result is
    // clamped by the caller's Math.max(0, …) — reproduced by saturating at 0 here.
    o.map(|n| n.max(0) as usize).unwrap_or(0)
}

async fn fetch_from_server(
    probe: &Arc<dyn McpProbe>,
    kind: &str,
    cursor: Option<String>,
) -> Result<(Vec<Value>, Option<String>), String> {
    probe.list(kind, cursor).await
}

/// One page of a listing.
pub struct PageOut {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
    pub total: Option<usize>,
}

/// Return one page (≤ PAGE_SIZE items) using MCP cursor pagination — port of `listPage`. The
/// cache is filled lazily: servers that paginate are followed page by page as the offset
/// advances; servers that dump everything fill `acc` once, then paging is pure slicing — the full
/// list never leaves the gateway. `total` is included only once the server is fully fetched.
pub async fn list_page(
    probe: Arc<dyn McpProbe>,
    kind: &str,
    cache: &Arc<PageCache>,
    cursor: Option<&str>,
) -> Result<PageOut, String> {
    debug_assert!(KINDS.contains(&kind), "caller picks a known kind");
    let offset = dec_offset(cursor);
    {
        let mut s = cache.state.lock().map_err(|_| "page cache poisoned")?;
        s.touched_at = crate::util::now_ms();
    }
    // One cache is shared by every request against this MCP+kind; serialize the fill so two
    // readers cannot duplicate rows. Held across awaits — the state lock never is.
    let _fill = cache.fill.lock().await;
    {
        // Snapshot-then-fill: the guard is dropped BEFORE the fetch, because a std MutexGuard
        // is not Send and must not ride across the await.
        let needs_first = {
            let s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            !s.started
        };
        if needs_first {
            let (items, next) = fetch_from_server(&probe, kind, None).await?;
            let mut s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            s.acc = items;
            s.server_next = next;
            s.started = true;
        }
    }
    // Pull more from the server until the requested window is covered or the server is exhausted.
    // A server that hands back the cursor it was just given makes no progress: treat a repeated
    // cursor as exhausted, and cap the round trips either way — both are guards against the far
    // side, which is why they live here rather than in any one adapter.
    let mut page = 0usize;
    loop {
        let need_more = {
            let s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            offset + PAGE_SIZE > s.acc.len() && s.server_next.is_some()
        };
        if !need_more {
            break;
        }
        if page >= MAX_FILL_PAGES {
            crate::log::warn(
                "list paging stopped at the page cap",
                Some(serde_json::json!({ "kind": kind })),
            );
            let mut s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            s.server_next = None;
            break;
        }
        let from = {
            let s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            s.server_next.clone()
        };
        let (items, next) = fetch_from_server(&probe, kind, from.clone()).await?;
        if next == from {
            crate::log::warn(
                "list paging stopped: server repeated its cursor",
                Some(serde_json::json!({ "kind": kind })),
            );
            let mut s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            s.server_next = None;
            break;
        }
        {
            let mut s = cache.state.lock().map_err(|_| "page cache poisoned")?;
            s.acc.extend(items);
            s.server_next = next;
        }
        page += 1;
    }
    let (items_slice, more, total) = {
        let s = cache.state.lock().map_err(|_| "page cache poisoned")?;
        let items = s.acc[offset.min(s.acc.len())..(offset + PAGE_SIZE).min(s.acc.len())].to_vec();
        let more = offset + PAGE_SIZE < s.acc.len() || s.server_next.is_some();
        let total = if s.server_next.is_some() {
            None
        } else {
            Some(s.acc.len())
        };
        (items, more, total)
    };
    Ok(PageOut {
        items: items_slice,
        next_cursor: more.then(|| enc_offset(offset + PAGE_SIZE)),
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_the_node_wire_format() {
        assert_eq!(enc_offset(0), "eyJvIjowfQ");
        assert_eq!(enc_offset(50), "eyJvIjo1MH0");
        assert_eq!(dec_offset(Some("eyJvIjo1MH0")), 50);
        assert_eq!(dec_offset(Some("eyJvIjotOH0")), 0, "negative clamps to 0");
        assert_eq!(dec_offset(Some("not base64!")), 0);
        assert_eq!(dec_offset(None), 0);
    }
}
