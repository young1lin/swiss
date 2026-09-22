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

/* --- the stream value view (docs/45 S2) ----------------------------------------------------------- */
/* A stream key opens on its newest-first window (the /key answer for a stream IS the
   window), walks history through Load-earlier (an exclusive Before cursor, prepended
   pages, no row lost or repeated at a seam), and unions its field columns in
   first-seen-scanning-newest-first order exactly the way the server derives them — the
   panel re-runs the same algorithm over the merged rows so a column an older page
   introduces appears at the table's tail, never reshuffling what the user is reading.
   Everything here is read-only: the stream has no field grid to edit against, so the
   value header's typed-add button simply does not exist for this type. */

import type { ApiDbStreamEntry, ApiDbStreamGroupRow, ApiDbStreamWindow } from "./types/api.js";
import type { ApiDbRedisValue } from "./types/api.js";
import type { DbKeyTab } from "./types/state.js";
import { $, apiJson, dbReqGuard, el } from "./util.js";
import { h } from "./h.js";
import { renderDbGrid } from "./data-grid.js";
import { dbConn, dbTab } from "./db-state.js";
import { tr, trn } from "./i18n.js";
// Cycle with data-browsers.js (it renders this view's branch; this module reuses its
// display decode and cell menu): function declarations, runtime-only use — the same
// shape as the data-edit.js import over there.
import { dbRedisCellMenu, dbRedisDisplayText } from "./data-browsers.js";

/* One stream id's compare: the ms half first, then the seq half, each BigInt. A plain
   string compare breaks the day an id's ms length changes; fusing the halves into one
   number (ms*65536 + seq, the first cut) collides the moment seq reaches 65536 - the
   fused key of "X-65536" equals the fused key of "X+1-0". Tuple-compare instead. */
function streamIdCompare(a: string, b: string): number {
  const pa = a.split("-");
  const pb = b.split("-");
  const ma = BigInt(pa[0] || "0"), mb = BigInt(pb[0] || "0");
  if (ma !== mb) return ma < mb ? -1 : 1;
  const sa = BigInt(pa[1] || "0"), sb = BigInt(pb[1] || "0");
  return sa < sb ? -1 : sa > sb ? 1 : 0;
}

/** Merge one page — Load-earlier (older) or a live-edge poll (newer) — into the grown
 *  row cache. Pure. The seam is deduped by id, then the whole cache sorts newest-first
 *  by id: both directions land in display order without the caller saying which it
 *  meant. (The first cut prepended the page unconditionally, which stood a Before
 *  page on its head — the docs/45 walk on 19998 caught it live, and the sort makes
 *  the order structural instead of load-bearing on where the page came from.) */
export function streamMerge(rows: ApiDbStreamEntry[], page: ApiDbStreamEntry[]): ApiDbStreamEntry[] {
  const have = new Set(rows.map((e: ApiDbStreamEntry): string => e.id));
  const fresh = page.filter((e: ApiDbStreamEntry): boolean => !have.has(e.id));
  return rows.concat(fresh).sort(
    (a: ApiDbStreamEntry, b: ApiDbStreamEntry): number => streamIdCompare(b.id, a.id),
  );
}

/* The DOM row budget: the table never holds more than this, and WHICH end falls out
   is the walk's whole problem — see streamCap. */
const STREAM_ROW_CAP = 500;

/** The rate readout's number: entries per second over the sample window, one decimal.
 *  Pure; junk (no elapsed time) reads as "" so a reset baseline paints nothing rather
 *  than a fabricated spike or a negative rate. */
export function streamRateText(dLen: number, dtMs: number): string {
  if (!(dtMs > 0) || !isFinite(dtMs) || !isFinite(dLen)) return "";
  return String(Math.round((dLen / (dtMs / 1000)) * 10) / 10);
}

/** The row cache's trim, one cap and two directions (docs/45 §2.2): a walk of older
 *  pages keeps the OLDEST cap rows — the trim drops from the TOP, so the page that
 *  just landed survives and the Before cursor keeps moving; a live-edge Follow keeps
 *  the NEWEST cap rows — the live edge is what Follow exists for, and history can
 *  always be walked again. Pure. */
export function streamCap(
  rows: ApiDbStreamEntry[], cap: number, keep: "newest" | "oldest",
): ApiDbStreamEntry[] {
  if (!(cap > 0) || rows.length <= cap) return rows;
  return keep === "newest" ? rows.slice(0, cap) : rows.slice(rows.length - cap);
}

/** The holdback pool's trim (docs/45 S3 follow-up): the pool behind the pill is
 *  bounded by the same STREAM_ROW_CAP as the table itself — a reader deep in history
 *  while the stream runs hot would otherwise park every tick's page in state without
 *  end (50 entries/s read for ten minutes is 30,000 rows). Overflow keeps the NEWEST
 *  cap rows (the pool is a held live edge, so streamCap's keep "newest" is its rule
 *  too), and the drop reports itself: what fell out is a hole in the middle of what
 *  the pill would splice, exactly like a truncated page, so the gap bar lights and
 *  "jump to latest" stays the honest exit. Pure. */
export function streamPool(
  pending: ApiDbStreamEntry[], page: ApiDbStreamEntry[], cap: number,
): { pending: ApiDbStreamEntry[]; dropped: boolean } {
  const merged = streamMerge(pending, page);
  if (!(cap > 0) || merged.length <= cap) return { pending: merged, dropped: false };
  return { pending: merged.slice(0, cap), dropped: true };
}

/** True when the stream table sits at its live edge (docs/45 §2.3): scrollTop no
 *  deeper than one row below the top. Junk counts as pinned — an unreadable row
 *  height or a negative offset is a view we cannot prove is reading history, and
 *  the edge must never be missed on a technicality (the terminal's own pinned rule,
 *  axis flipped). Pure. */
export function isTopPinned(scrollTop: number, rowHeight: number): boolean {
  if (!(rowHeight > 0)) return true; // unreadable: assume the edge
  return scrollTop <= rowHeight;
}

/** The field columns of the merged view: first-seen scanning newest-first — the same
 *  derivation the server runs per window (docs/45 §2.1), re-run here over the whole
 *  grown cache so an older page's new field lands at the table's tail. Pure. */
export function streamColumns(rows: ApiDbStreamEntry[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  rows.forEach((e: ApiDbStreamEntry): void => {
    Object.keys(e.fields).forEach((k: string): void => {
      if (!seen.has(k)) {
        seen.add(k);
        out.push(k);
      }
    });
  });
  return out;
}

/** The stream's value body: a read-only table in the typed table's own vocabulary — id
 *  and ts lead, then one column per field, every cell carrying the docs/22 W5.3 copy/view
 *  menu. The row cache seeds from the first window (the /key answer) and grows through
 *  dbStreamLoadEarlier; a `more` that went false ends the walk and paints the
 *  beginning-of-stream line. */
export function dbRenderStream(wrap: HTMLElement, v: ApiDbRedisValue): void {
  const t = dbTab();
  if (t.kind !== "key") return;
  const win = v as ApiDbStreamWindow;
  if (t.redisStreamRows == null) {
    t.redisStreamRows = (win.entries || []).slice();
    t.redisStreamMore = !!win.more;
  }
  const rows = t.redisStreamRows;
  if (!rows.length) {
    // An empty stream is still a followable one (docs/45 §2.3): the bar mounts, the
    // first tick polls after=0-0, and the first append ever lands without a re-key.
    dbStreamFollowBar(wrap, t);
    wrap.appendChild(el("div", "db-hint", tr("dataStream.empty")));
    return;
  }
  dbStreamFollowBar(wrap, t);
  const cols = streamColumns(rows);
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  const head = (label: string): HTMLElement => {
    const th = el("th", "db-col db-rowctl", label); // no sort affordance on a stream window
    return th;
  };
  hr.appendChild(head(tr("dataStream.colId")));
  hr.appendChild(head(tr("dataStream.colTime")));
  cols.forEach((c: string): void => {
    hr.appendChild(head(c)); // a field name is wire data, not copy
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  rows.forEach((e: ApiDbStreamEntry): void => {
    const trNode = el("tr");
    const where = v.key + " · stream";
    const tdId = el("td", "db-cell", e.id);
    tdId.oncontextmenu = (ev: MouseEvent): void => {
      dbRedisCellMenu(ev, tr("dataStream.colId"), e.id, where);
    };
    trNode.appendChild(tdId);
    const tdTs = el("td", "db-cell", e.ts == null ? "" : e.ts);
    tdTs.oncontextmenu = (ev: MouseEvent): void => {
      dbRedisCellMenu(ev, tr("dataStream.colTime"), e.ts == null ? "" : e.ts, where);
    };
    trNode.appendChild(tdTs);
    cols.forEach((c: string): void => {
      const raw = e.fields[c] == null ? "" : String(e.fields[c]);
      const td = el("td", "db-cell", dbRedisDisplayText(raw));
      td.title = td.textContent || ""; // long values truncate in the cell; hover shows all
      td.oncontextmenu = (ev: MouseEvent): void => {
        dbRedisCellMenu(ev, c, td.textContent || "", where);
      };
      trNode.appendChild(td);
    });
    tbody.appendChild(trNode);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
  if (t.redisStreamGroupsOpen) dbStreamGroupsTable(wrap, t);
  if (t.redisStreamMore) {
    const btn = h("button", { class: "btn", type: "button" }, tr("dataStream.loadEarlier"));
    btn.onclick = (): void => { void dbStreamLoadEarlier(); };
    wrap.appendChild(btn);
  } else {
    wrap.appendChild(el("div", "db-hint", tr("dataStream.start")));
  }
  // The poller runs exactly while a followed stream view is on screen; every other
  // path (tab left, key closed, follow off) is caught by the tick's own guard.
  if (t.redisStreamFollow) {
    if (dbFollowTimer == null) dbStreamFollowStart();
  } else {
    dbStreamFollowStop();
  }
}

/* --- Follow (docs/45 S3) ------------------------------------------------------------------------ */

/* The fold cadence: the groups fold pays its one extra command every fifth tick — the
   per-tick budget stays 3, the fold is the rare 4th (docs/45 S3; the L1 commandstats
   test pins xinfo|groups separately). STREAM_ROW_CAP is declared with the pure folds
   above — one cap, both directions. */
const STREAM_GROUPS_EVERY = 5;
const STREAM_EVERY_MS = [1000, 2000, 5000];

/* One Follow timer for the whole panel: the tick re-checks the live tab, so a timer left
   behind by a tab switch costs at most one guarded no-op. */
let dbFollowTimer: ReturnType<typeof setInterval> | undefined;
let dbFollowTicks = 0;
const dbFollowReq = dbReqGuard();

function dbStreamFollowStop(): void {
  if (dbFollowTimer != null) {
    clearInterval(dbFollowTimer);
    dbFollowTimer = undefined;
  }
  dbFollowTicks = 0;
}

function dbStreamFollowStart(): void {
  dbStreamFollowStop();
  const t = dbTab();
  const every = t.kind === "key" && t.redisStreamEvery ? t.redisStreamEvery : STREAM_EVERY_MS[0];
  dbFollowTimer = setInterval((): void => { void dbStreamTick(); }, every);
}

function dbStreamUrl(path: string, key: string, extra: string): string {
  const c = dbConn();
  return "/api/db/" + encodeURIComponent(c.conn || "") + path + "?key=" + encodeURIComponent(key) + extra;
}

/** One Follow tick (docs/45 S3): one After page for the live edge. A hidden tab
 *  skips the whole fetch (the data-activity visibility idiom), never just the paint;
 *  a tab scrolled into history still fetches — the page pools behind the pill and the
 *  table holds still. Exported for the acceptance suite: the suite drives ticks the
 *  way the interval would, against the same parked-fetch stubs as the loaders. */
export async function dbStreamTick(): Promise<void> {
  const t = dbTab();
  if (t.kind !== "key" || !t.redisKey || !t.redisStreamFollow) {
    dbStreamFollowStop();
    return;
  }
  if (typeof document !== "undefined" && document.hidden) return; // zero fetch, not zero paint
  const rows = t.redisStreamRows;
  // An empty stream still gets its edge: the poll starts at the stream's very
  // beginning (after=0-0), so the first append ever lands without a re-key.
  const newest = rows && rows.length ? rows[0].id : "0-0";
  dbFollowTicks++;
  const token = dbFollowReq.issue();
  const j = await apiJson<ApiDbStreamWindow>(dbStreamUrl("/stream", t.redisKey, "&after=" + encodeURIComponent(newest)));
  if (!dbFollowReq.accepts(token)) return; // superseded by a newer tick or a reload
  const after = dbTab();
  if (after !== t) { dbStreamFollowStop(); return; } // the tab moved on mid-flight
  if (!j) {
    // An error STOPS Follow (docs/45 §2.3): a dead tail polled once a second
    // forever is noise, not persistence — the toggle resets so the state is
    // honest, and the bar carries the reason. The rate baseline dies with it
    // (a stale lastLen would fabricate a spike on a later manual restart);
    // apiJson has already toasted the server's own text for the operator.
    t.redisStreamFollow = false;
    t.redisStreamErr = tr("dataStream.followStopped");
    t.redisStreamLen = null;
    t.redisStreamLenAt = null;
    dbStreamFollowStop();
    renderDbGrid();
    return;
  }
  // more=true on a live-edge page means more than one page arrived between ticks:
  // the window truncated, a middle chunk is missing, and splicing the two ends
  // together would lie about the stream's order. The flag STAYS once set — a later
  // untruncated tick heals nothing (the hole is still between the rows) — until the
  // operator jumps to the latest window, the one move that reopens the truth.
  if (j.more) t.redisStreamGap = true;
  let pinned = true;
  if (j.entries && j.entries.length) {
    // The live edge is the TOP of a newest-first table. A view at the top rides the
    // edge (insert, repaint, stay at 0). A view reading history holds still
    // instead: nothing inserts, the page pools in redisStreamPending behind the
    // pill, and the table the operator is reading is not rebuilt under them —
    // a repaint here would rebuild the scroller and yank the row under their eyes.
    const wrap = $("dbGridWrap");
    const top = wrap ? (wrap as HTMLElement).scrollTop : 0;
    let rowH = 0;
    if (wrap) {
      const r0 = (wrap as HTMLElement).querySelector("tbody tr") as HTMLElement | null;
      rowH = r0 ? r0.offsetHeight : 0;
    }
    pinned = isTopPinned(top, rowH);
    if (pinned) {
      t.redisStreamRows = streamCap(
        streamMerge(streamMerge(t.redisStreamRows || [], t.redisStreamPending || []), j.entries),
        STREAM_ROW_CAP, "newest",
      );
      t.redisStreamPending = []; // the pooled page rode the edge with this one
      t.redisStreamPendingDropped = false; // the pool is consumed; a refill starts honest
    } else {
      const pooled = streamPool(t.redisStreamPending || [], j.entries, STREAM_ROW_CAP);
      t.redisStreamPending = pooled.pending;
      if (pooled.dropped) {
        // The pool overflowed (docs/45 S3 follow-up): the rows that fell out are a
        // hole in the middle no later tick can heal — the same gap flag a truncated
        // page sets, and the pill switches to the "500+" copy for the same reason.
        t.redisStreamGap = true;
        t.redisStreamPendingDropped = true;
      }
    }
  }
  const now = Date.now();
  if (t.redisStreamLen != null && t.redisStreamLenAt != null) {
    t.redisStreamRate = streamRateText((j.length ?? 0) - (t.redisStreamLen as number), now - (t.redisStreamLenAt as number));
  }
  t.redisStreamLen = j.length ?? 0;
  t.redisStreamLenAt = now;
  if (dbFollowTicks % STREAM_GROUPS_EVERY === 0) {
    const g = await apiJson<{ groups: ApiDbStreamGroupRow[] }>(dbStreamUrl("/stream/groups", t.redisKey, ""));
    if (g) t.redisStreamGroups = g.groups || [];
  }
  if (pinned) {
    renderDbGrid(); // at the edge a rebuild is the paint, and the top stays the top
    return;
  }
  dbStreamBarDyn(t); // pill count / rate / gap in place — the table never moves
}

/* The Follow bar's live texts (docs/45 §2.3). renderDbGrid rebuilds the bar with
   every pinned repaint, so these refs are re-taken per paint; a not-pinned tick
   updates them in place — the pill count, the rate readout, the gap bar's
   visibility — without touching the table the operator is reading. */
let dbStreamPill: HTMLElement | null = null;
let dbStreamRateEl: HTMLElement | null = null;
let dbStreamGapEl: HTMLElement | null = null;

function dbStreamBarDyn(t: DbKeyTab): void {
  const n = (t.redisStreamPending || []).length;
  if (dbStreamPill) {
    dbStreamPill.hidden = n === 0;
    if (n) {
      dbStreamPill.textContent = t.redisStreamPendingDropped
        ? tr("dataStream.pendingNewOver", { n: STREAM_ROW_CAP })
        : trn(n, "dataStream.pendingNew.one", "dataStream.pendingNew.other");
    }
  }
  if (dbStreamRateEl) {
    dbStreamRateEl.hidden = !t.redisStreamRate;
    dbStreamRateEl.textContent = t.redisStreamRate ? tr("dataStream.rate", { r: t.redisStreamRate }) : "";
  }
  if (dbStreamGapEl) dbStreamGapEl.hidden = !t.redisStreamGap;
}

/** The gap bar's action (docs/45 §2.3): reopen the LATEST window — no cursor, the
 *  newest page straight from the server — and let it replace the rows. Splicing the
 *  two ends of a truncated window would present a contiguous stream that never was. */
async function dbStreamJumpLatest(): Promise<void> {
  const t = dbTab();
  if (t.kind !== "key" || !t.redisKey) return;
  // Jumping reopens the follow window itself: any tick still in flight speaks for
  // the window this jump is about to replace — letting its answer land would pool a
  // page behind a pill whose splice target no longer exists (docs/45 S3 follow-up).
  // Issuing on the same guard voids those ticks, exactly as a newer tick voids an
  // older one; the converse is accepted — a tick issued AFTER the jump supersedes
  // it, and dedup-plus-sort makes a late live-edge page a no-op at worst.
  const token = dbFollowReq.issue();
  const j = await apiJson<ApiDbStreamWindow>(dbStreamUrl("/stream", t.redisKey, ""));
  if (!dbFollowReq.accepts(token)) return; // a newer tick or jump won the race
  if (!j) return; // apiJson toasted it; the gap bar stays and says the same thing
  if (dbTab() !== t) return; // the tab moved on mid-flight; the answer belongs to nobody
  t.redisStreamRows = streamCap((j.entries || []).slice(), STREAM_ROW_CAP, "newest");
  t.redisStreamPending = [];
  t.redisStreamPendingDropped = false; // the pool it described is gone
  t.redisStreamGap = false;
  t.redisStreamMore = !!j.more;
  renderDbGrid();
}
/** The Follow bar: on/off, the interval, the rate readout, and the groups fold's toggle —
 *  one row in the value header's own vocabulary, painted above the table. */
function dbStreamFollowBar(wrap: HTMLElement, t: DbKeyTab): void {
  const bar = el("div", "db-detail-meta");
  const followBtn = h("button", {
    class: "btn", type: "button",
    title: t.redisStreamFollow ? tr("dataStream.pause") : tr("dataStream.follow"),
  }, t.redisStreamFollow ? tr("dataStream.pause") : tr("dataStream.follow"));
  followBtn.onclick = (): void => {
    t.redisStreamFollow = !t.redisStreamFollow;
    if (t.redisStreamFollow) {
      t.redisStreamLen = null; // a fresh start samples the rate from THIS tick, not history
      t.redisStreamLenAt = null;
      t.redisStreamErr = null; // a restart is a new opinion; the old reason stops applying
      dbStreamFollowStart();
    } else {
      dbStreamFollowStop();
    }
    renderDbGrid();
  };
  bar.appendChild(followBtn);
  const sel = el("select", "db-stream-every") as HTMLSelectElement;
  STREAM_EVERY_MS.forEach((ms: number): void => {
    const opt = el("option", "", trn(ms / 1000, "dataStream.nSeconds.one", "dataStream.nSeconds.other")) as HTMLOptionElement;
    opt.value = String(ms);
    if ((t.redisStreamEvery || STREAM_EVERY_MS[0]) === ms) opt.selected = true;
    sel.appendChild(opt);
  });
  sel.onchange = (): void => {
    const ms = Number(sel.value);
    t.redisStreamEvery = STREAM_EVERY_MS.includes(ms) ? ms : STREAM_EVERY_MS[0];
    if (t.redisStreamFollow) dbStreamFollowStart(); // the interval change takes effect now
  };
  bar.appendChild(sel);
  if (t.redisStreamErr) {
    bar.appendChild(el("span", "db-hint", t.redisStreamErr)); // why Follow stopped, in place
  }
  // The pending pill (docs/45 §2.3): the not-pinned holdback's one visible fact. Click
  // IS the flush — pool merges at the live edge, the view returns to the top.
  const pendN = (t.redisStreamPending || []).length;
  const pill = h("button", { class: "btn db-stream-pill", type: "button" }, "");
  pill.hidden = pendN === 0;
  if (pendN) {
    pill.textContent = t.redisStreamPendingDropped
      ? tr("dataStream.pendingNewOver", { n: STREAM_ROW_CAP })
      : trn(pendN, "dataStream.pendingNew.one", "dataStream.pendingNew.other");
  }
  pill.onclick = (): void => {
    const cur = dbTab();
    if (cur.kind !== "key") return;
    const pool = cur.redisStreamPending || [];
    if (pool.length) {
      cur.redisStreamRows = streamCap(streamMerge(cur.redisStreamRows || [], pool), STREAM_ROW_CAP, "newest");
      cur.redisStreamPending = [];
      cur.redisStreamPendingDropped = false; // flushed at the edge; a new pool starts honest
    }
    renderDbGrid();
    const wrap2 = $("dbGridWrap");
    if (wrap2) (wrap2 as HTMLElement).scrollTop = 0; // the flush lands at the edge
  };
  bar.appendChild(pill);
  dbStreamPill = pill;
  // The gap bar: a truncated live-edge page skipped a middle chunk; the honest move is
  // reopening the latest window, not splicing two ends into a lie.
  const gap = h("button", { class: "btn db-stream-gap", type: "button" },
    tr("dataStream.gapSkipped") + " · " + tr("dataStream.jumpLatest"));
  gap.hidden = !t.redisStreamGap;
  gap.onclick = (): void => { void dbStreamJumpLatest(); };
  bar.appendChild(gap);
  dbStreamGapEl = gap;
  const rateEl = el("span", "db-hint", t.redisStreamRate ? tr("dataStream.rate", { r: t.redisStreamRate }) : "");
  rateEl.hidden = !t.redisStreamRate;
  bar.appendChild(rateEl);
  dbStreamRateEl = rateEl;
  bar.appendChild(el("span", "grow"));
  const groupsLabel = tr("dataStream.groups")
    + (t.redisStreamGroups ? " (" + String(t.redisStreamGroups.length) + ")" : "");
  const groupsBtn = h("button", { class: "btn", type: "button" }, groupsLabel);
  groupsBtn.onclick = (): void => {
    t.redisStreamGroupsOpen = !t.redisStreamGroupsOpen;
    renderDbGrid();
  };
  bar.appendChild(groupsBtn);
  wrap.appendChild(bar);
}

/** The consumer-group fold (docs/45 §2.4): a read-only table under the stream, fed by
 *  every fifth Follow tick — pending, lag (null is the honest pre-7.0 answer), and the
 *  group's own delivered cursor. */
function dbStreamGroupsTable(wrap: HTMLElement, t: DbKeyTab): void {
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  [tr("dataStream.colGroup"), tr("dataStream.colConsumers"), tr("dataStream.colPending"),
    tr("dataStream.colLag"), tr("dataStream.colLastDelivered")].forEach((label: string): void => {
    hr.appendChild(el("th", "db-col db-rowctl", label));
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  const groups = t.redisStreamGroups || [];
  if (!groups.length) {
    tbl.appendChild(tbody);
    wrap.appendChild(tbl);
    wrap.appendChild(el("div", "db-hint", tr("dataStream.groupsNone")));
    return;
  }
  groups.forEach((g: ApiDbStreamGroupRow): void => {
    const r = el("tr");
    [g.name, String(g.consumers), String(g.pending), g.lag == null ? "—" : String(g.lag),
      g["last-delivered-id"] == null ? "" : String(g["last-delivered-id"])].forEach((cell: string): void => {
      r.appendChild(el("td", "db-cell", cell));
    });
    tbody.appendChild(r);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/* One Load-earlier in flight: re-entry while a page loads would re-send the same Before
   cursor — harmless after the merge dedupe, but a wasted round trip per impatient click. */
let dbStreamLoading = false;

/* One /stream request chain: a slow page landing after the key was left (or re-read)
   must not splice rows into a view that no longer owns them. */
const dbStreamReq = dbReqGuard();

/** Fetch the next-older page and grow the cache. The Before cursor is the OLDEST id
 *  in the grown cache, exclusive on the server; `more` from the answer ends the walk.
 *  The cap keeps the OLDEST rows here (docs/45 §2.2): a walk that trimmed the top of
 *  the table would drop the page it just fetched, the oldest id would never move, and
 *  every further click would re-request the same page - a walk stuck in place. */
export async function dbStreamLoadEarlier(): Promise<void> {
  const c = dbConn();
  const t = dbTab();
  if (!c.conn || t.kind !== "key" || !t.redisKey) return;
  const rows = t.redisStreamRows;
  if (dbStreamLoading || !rows || !rows.length || !t.redisStreamMore) return;
  const oldest = rows[rows.length - 1].id;
  dbStreamLoading = true;
  const token = dbStreamReq.issue();
  let j: ApiDbStreamWindow | null = null;
  try {
    j = await apiJson<ApiDbStreamWindow>(dbStreamUrl("/stream", t.redisKey, "&before=" + encodeURIComponent(oldest)));
  } finally {
    dbStreamLoading = false;
  }
  if (!dbStreamReq.accepts(token) || !j) return; // superseded, or apiJson toasted it
  const after = dbTab();
  if (after !== t) return; // the tab moved on; the answer belongs to nobody
  t.redisStreamRows = streamCap(
    streamMerge(t.redisStreamRows || [], j.entries || []), STREAM_ROW_CAP, "oldest",
  );
  t.redisStreamMore = !!j.more;
  renderDbGrid();
}
