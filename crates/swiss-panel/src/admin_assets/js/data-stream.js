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

                                                                                               
                                                      
                                                 
import { $, apiJson, dbReqGuard, el } from "./util.js";
import { h } from "./h.js";
import { filterInput, sw } from "./ui/index.js";
import { renderDbGrid } from "./data-grid.js";
import { dbConn, dbTab } from "./db-state.js";
import { tr, trn } from "./i18n.js";
import { btn } from "./ui/button.js";
// Cycle with data-browsers.js (it renders this view's branch; this module reuses its
// display decode and cell menu): function declarations, runtime-only use — the same
// shape as the data-edit.js import over there.
import { dbRedisCellMenu, dbRedisDisplayText } from "./data-browsers.js";

/* One stream id's compare: the ms half first, then the seq half, each BigInt. A plain
   string compare breaks the day an id's ms length changes; fusing the halves into one
   number (ms*65536 + seq, the first cut) collides the moment seq reaches 65536 - the
   fused key of "X-65536" equals the fused key of "X+1-0". Tuple-compare instead. */
function streamIdCompare(a        , b        )         {
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
export function streamMerge(rows                    , page                    )                     {
  const have = new Set(rows.map((e                  )         => e.id));
  const fresh = page.filter((e                  )          => !have.has(e.id));
  return rows.concat(fresh).sort(
    (a                  , b                  )         => streamIdCompare(b.id, a.id),
  );
}

/* The DOM row budget: the table never holds more than this, and WHICH end falls out
   is the walk's whole problem — see streamCap. */
const STREAM_ROW_CAP = 500;

/** The rate readout's number: entries per second over the sample window, one decimal.
 *  Pure; junk (no elapsed time, a clock that went backwards, a length that never arrived)
 *  reads as "" so a reset baseline paints nothing rather than a fabricated spike.
 *  A REAL shrink, though, reads as a real negative number (docs/45 §2.3): XTRIM or XDEL
 *  between two ticks is exactly the event an operator watching a feed wants to see, and
 *  clamping it to 0 would hide "where did my entries go" behind a calm zero. */
export function streamRateText(dLen        , dtMs        )         {
  if (!(dtMs > 0) || !isFinite(dtMs) || !isFinite(dLen)) return "";
  return String(Math.round((dLen / (dtMs / 1000)) * 10) / 10);
}

/** The row cache's trim, one cap and two directions (docs/45 §2.2): a walk of older
 *  pages keeps the OLDEST cap rows — the trim drops from the TOP, so the page that
 *  just landed survives and the Before cursor keeps moving; a live-edge Follow keeps
 *  the NEWEST cap rows — the live edge is what Follow exists for, and history can
 *  always be walked again. Pure. */
export function streamCap(
  rows                    , cap        , keep                     ,
)                     {
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
  pending                    , page                    , cap        ,
)                                                    {
  const merged = streamMerge(pending, page);
  if (!(cap > 0) || merged.length <= cap) return { pending: merged, dropped: false };
  return { pending: merged.slice(0, cap), dropped: true };
}

/** True when the stream table sits at its live edge (docs/45 §2.3): scrollTop no
 *  deeper than one row below the top. Junk counts as pinned — an unreadable row
 *  height or a negative offset is a view we cannot prove is reading history, and
 *  the edge must never be missed on a technicality (the terminal's own pinned rule,
 *  axis flipped). Pure. */
export function isTopPinned(scrollTop        , rowHeight        )          {
  if (!(rowHeight > 0)) return true; // unreadable: assume the edge
  return scrollTop <= rowHeight;
}

/** One id's millisecond half as a number, or NaN when the id is not one. Pure. */
export function streamIdMs(id        )         {
  return Number((id || "").split("-")[0]);
}

/** One row of the summary strip (docs/49 §2.3): a value of the grouped field, how
 *  many of the held rows carry it, and that value's own rate. */
;                                  
                
            
               
 

/** The summary strip over the rows the table holds (docs/49 §2.3). Ninety rows a
 *  second is not readable; nine numbers are, and this is those nine — count and
 *  rate per value of one field, busiest first. The denominator is the whole held
 *  window's own time span, so the rates add up to the stream's rate instead of
 *  each being measured over a different stretch. Pure: the strip is a reading of
 *  what is already on screen, not another request. */
export function streamSummary(rows                    , field        )                     {
  const n = new Map                ();
  rows.forEach((e                  )       => {
    const raw = e.fields[field];
    if (raw == null) return;
    const v = String(raw);
    n.set(v, (n.get(v) || 0) + 1);
  });
  // Newest-first rows: the span is the first id's ms minus the last's. One sample
  // (or a stream whose ids share a millisecond) has no span and reports no rate —
  // a count over zero seconds is infinity, and infinity is not a readout.
  const span = rows.length > 1
    ? (streamIdMs(rows[0].id) - streamIdMs(rows[rows.length - 1].id)) / 1000
    : 0;
  const out                     = [];
  n.forEach((count        , value        )       => {
    out.push({
      value: value,
      n: count,
      rate: span > 0 && isFinite(span) ? String(Math.round((count / span) * 10) / 10) : "",
    });
  });
  // Busiest first, then by value so two equal counts keep a stable order between ticks.
  return out.sort((a                  , b                  )         => {
    return b.n - a.n || (a.value < b.value ? -1 : a.value > b.value ? 1 : 0);
  });
}

/* What counts as a dimension rather than a measurement (docs/49 §2.3): a column
   worth grouping by has a handful of repeated values, not one per row. A feed's
   `symbol` has nine; its `price` and `seq` have one each per entry, and grouping
   by those would draw 500 chips nobody can read. */
const STREAM_GROUP_MAX_VALUES = 24;

/** The field the summary strip opens on, or null when no column looks like a
 *  dimension (docs/49 §2.3). First column that repeats itself wins — column order
 *  is the stream's own, so the first repeating field is the one its author put
 *  first. Pure. */
export function streamAutoField(rows                    , cols          )                {
  if (rows.length < 4) return null; // too few rows to tell a dimension from an id
  for (const c of cols) {
    const seen = new Set        ();
    let have = 0;
    for (const e of rows) {
      const raw = e.fields[c];
      if (raw == null) continue;
      have++;
      seen.add(String(raw));
      if (seen.size > STREAM_GROUP_MAX_VALUES) break;
    }
    if (seen.size >= 2 && seen.size <= STREAM_GROUP_MAX_VALUES && seen.size * 2 <= have) return c;
  }
  return null;
}

/** The field columns of the merged view: first-seen scanning newest-first — the same
 *  derivation the server runs per window (docs/45 §2.1), re-run here over the whole
 *  grown cache so an older page's new field lands at the table's tail. Pure. */
export function streamColumns(rows                    )           {
  const seen = new Set        ();
  const out           = [];
  rows.forEach((e                  )       => {
    Object.keys(e.fields).forEach((k        )       => {
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
export function dbRenderStream(wrap             , v                 )       {
  const t = dbTab();
  if (t.kind !== "key") return;
  const win = v                     ;
  if (t.redisStreamRows == null) {
    t.redisStreamRows = (win.entries || []).slice();
    t.redisStreamMore = !!win.more;
  }
  const rows = t.redisStreamRows;
  dbStreamPainted = null;
  if (!rows.length) {
    // An empty stream is still a followable one (docs/45 §2.3): the bar mounts, the
    // first tick polls after=0-0, and the first append ever lands without a re-key.
    // A FILTER that matched nothing keeps its box too (docs/49) — the one control
    // that can undo the emptiness must not vanish with the rows it hid.
    dbStreamFollowBar(wrap, t);
    dbStreamReadBar(wrap, t);
    wrap.appendChild(el("div", "db-hint",
      t.redisStreamMatch ? tr("dataStream.noMatch") : tr("dataStream.empty")));
    return;
  }
  dbStreamFollowBar(wrap, t);
  dbStreamReadBar(wrap, t);
  const tbl = dbStreamTableNode(rows, v.key);
  wrap.appendChild(tbl);
  const groups = t.redisStreamGroupsOpen ? dbStreamGroupsNode(t) : null;
  if (groups) wrap.appendChild(groups);
  dbStreamPainted = { tab: t, table: tbl, groups: groups };
  if (t.redisStreamMore) {
    const earlier = btn(tr("dataStream.loadEarlier"));
    earlier.onclick = ()       => { void dbStreamLoadEarlier(); };
    wrap.appendChild(earlier);
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

/* What the last paint put on screen for the stream in front: a Follow tick replaces the TABLE
   (and the groups fold) through these, never the pane. A full renderDbGrid per tick rebuilt
   the Follow bar too, and an interval picker the operator had open snapped shut every
   second (2026-09-28). Re-taken by every paint; null when the view has no table yet. */
let dbStreamPainted                                                                           = null;

/** The stream table for a window of rows, newest first. */
function dbStreamTableNode(rows                    , key        )              {
  const cols = streamColumns(rows);
  const tbl = el("table", "db-grid");
  // docs/49 §2.4: the pointer resting on the table means someone is reading it. A
  // fast stream replaces every row on screen in a few seconds, so Follow holds
  // while the pointer is there — the tick still fetches, the page pools behind the
  // pill it already has, and moving away resumes at the live edge. Scrolling into
  // history does the same thing; this is the same rule for a reader who has not
  // scrolled anywhere, and it needs no control of its own.
  tbl.onpointerenter = ()       => { dbStreamHold(true); };
  tbl.onpointerleave = ()       => { dbStreamHold(false); };
  const thead = el("thead");
  const hr = el("tr");
  const head = (label        )              => {
    const th = el("th", "db-col db-rowctl", label); // no sort affordance on a stream window
    return th;
  };
  hr.appendChild(head(tr("dataStream.colId")));
  hr.appendChild(head(tr("dataStream.colTime")));
  cols.forEach((c        )       => {
    hr.appendChild(head(c)); // a field name is wire data, not copy
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  rows.forEach((e                  )       => {
    const trNode = el("tr");
    const where = key + " · stream";
    const tdId = el("td", "db-cell", e.id);
    tdId.oncontextmenu = (ev            )       => {
      dbRedisCellMenu(ev, tr("dataStream.colId"), e.id, where);
    };
    trNode.appendChild(tdId);
    const tdTs = el("td", "db-cell", e.ts == null ? "" : e.ts);
    tdTs.oncontextmenu = (ev            )       => {
      dbRedisCellMenu(ev, tr("dataStream.colTime"), e.ts == null ? "" : e.ts, where);
    };
    trNode.appendChild(tdTs);
    cols.forEach((c        )       => {
      const raw = e.fields[c] == null ? "" : String(e.fields[c]);
      const td = el("td", "db-cell", dbRedisDisplayText(raw));
      td.title = td.textContent || ""; // long values truncate in the cell; hover shows all
      td.oncontextmenu = (ev            )       => {
        dbRedisCellMenu(ev, c, td.textContent || "", where);
      };
      trNode.appendChild(td);
    });
    tbody.appendChild(trNode);
  });
  tbl.appendChild(tbody);
  return tbl;
}

/** A tick's paint (see dbStreamPainted): swap in a fresh table for the rows held now, and the
 *  groups fold when it is open. False when there is no painted table of THIS tab to replace -
 *  the first rows of an empty stream, or a paint of something else since - and the caller
 *  falls back to the full paint. */
function dbStreamRepaint(t          , rowsMoved         )          {
  const p = dbStreamPainted;
  const rows = t.redisStreamRows || [];
  if (!p || p.tab !== t || !t.redisKey || !rows.length || p.table.isConnected === false) return false;
  if (rowsMoved) {
    const next = dbStreamTableNode(rows, t.redisKey);
    p.table.replaceWith(next);
    p.table = next;
  }
  if (p.groups && t.redisStreamGroupsOpen) {
    const next = dbStreamGroupsNode(t);
    p.groups.replaceWith(next);
    p.groups = next;
  }
  return true;
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
let dbFollowTimer                                            ;
let dbFollowTicks = 0;
const dbFollowReq = dbReqGuard();

function dbStreamFollowStop()       {
  if (dbFollowTimer != null) {
    clearInterval(dbFollowTimer);
    dbFollowTimer = undefined;
  }
  dbFollowTicks = 0;
}

function dbStreamFollowStart()       {
  dbStreamFollowStop();
  const t = dbTab();
  const every = t.kind === "key" && t.redisStreamEvery ? t.redisStreamEvery : STREAM_EVERY_MS[0];
  dbFollowTimer = setInterval(()       => { void dbStreamTick(); }, every);
}

function dbStreamUrl(path        , key        , extra        )         {
  const c = dbConn();
  const t = dbTab();
  // docs/49 §2.2: every window request carries the filter, so the tick, the opening
  // page and Load-earlier all ask the same question. The groups fold does not: it is
  // about the stream, not about which of its entries someone is reading.
  const line = t.kind === "key" && t.redisStreamMatch ? t.redisStreamMatch.trim() : "";
  const match = line && path === "/stream" ? "&match=" + encodeURIComponent(line) : "";
  return "/api/db/" + encodeURIComponent(c.conn || "") + path + "?key=" + encodeURIComponent(key) + extra + match;
}

/** Take a whole window as the view's new truth (docs/45 §2.3, docs/49 §2.2): the
 *  rows replace what was held, the holdback and the gap are reset because the
 *  answer they described is gone, and the filter's own cursors come along — a
 *  window that walked says how far it walked, one that did not answers null. */
function dbStreamTake(t          , j                   )       {
  t.redisStreamRows = streamCap((j.entries || []).slice(), STREAM_ROW_CAP, "newest");
  t.redisStreamPending = [];
  t.redisStreamPendingDropped = false;
  t.redisStreamGap = false;
  t.redisStreamMore = !!j.more;
  t.redisStreamScanned = j.scanned == null ? null : j.scanned;
  t.redisStreamSeen = j.scannedFrom == null ? null : j.scannedFrom;
  t.redisStreamScanTo = j.scannedTo == null ? null : j.scannedTo;
}

/** One Follow tick (docs/45 S3): one After page for the live edge. A hidden tab
 *  skips the whole fetch (the data-activity visibility idiom), never just the paint;
 *  a tab scrolled into history still fetches — the page pools behind the pill and the
 *  table holds still. Exported for the acceptance suite: the suite drives ticks the
 *  way the interval would, against the same parked-fetch stubs as the loaders. */
export async function dbStreamTick()                {
  const t = dbTab();
  if (t.kind !== "key" || !t.redisKey || !t.redisStreamFollow) {
    dbStreamFollowStop();
    return;
  }
  if (typeof document !== "undefined" && document.hidden) return; // zero fetch, not zero paint
  const rows = t.redisStreamRows;
  // An empty stream still gets its edge: the poll starts at the stream's very
  // beginning (after=0-0), so the first append ever lands without a re-key.
  // Under a filter the cursor is the newest id the server EXAMINED, not the newest
  // it returned (docs/49 §2.2): a filter matching one entry a minute would otherwise
  // re-read every non-matching entry since that match, every single tick.
  const newest = t.redisStreamSeen || (rows && rows.length ? rows[0].id : "0-0");
  dbFollowTicks++;
  const token = dbFollowReq.issue();
  const j = await apiJson                   (dbStreamUrl("/stream", t.redisKey, "&after=" + encodeURIComponent(newest)));
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
  // The filter's live cursor moves whether or not anything matched — that is the
  // whole point of it (docs/49 §2.2). It only ever moves FORWARD: a tick that
  // examined nothing answers null and leaves the last one standing.
  if (typeof j.scannedFrom === "string" && j.scannedFrom) t.redisStreamSeen = j.scannedFrom;
  let pinned = true;
  if (j.entries && j.entries.length) {
    // The live edge is the TOP of a newest-first table. A view at the top rides the
    // edge (insert, repaint, stay at 0). A view reading history holds still
    // instead: nothing inserts, the page pools in redisStreamPending behind the
    // pill, and the table the operator is reading is not rebuilt under them —
    // a repaint here would rebuild the scroller and yank the row under their eyes.
    const wrap = $("dbGridWrap");
    const top = wrap ? (wrap               ).scrollTop : 0;
    let rowH = 0;
    if (wrap) {
      const r0 = (wrap               ).querySelector("tbody tr")                      ;
      rowH = r0 ? r0.offsetHeight : 0;
    }
    // Reading holds the edge exactly as scrolling away does (docs/49 §2.4).
    pinned = isTopPinned(top, rowH) && !t.redisStreamHold;
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
    t.redisStreamRate = streamRateText((j.length ?? 0) - (t.redisStreamLen          ), now - (t.redisStreamLenAt          ));
  }
  t.redisStreamLen = j.length ?? 0;
  t.redisStreamLenAt = now;
  if (dbFollowTicks % STREAM_GROUPS_EVERY === 0) {
    const g = await apiJson                                   (dbStreamUrl("/stream/groups", t.redisKey, ""));
    if (g) t.redisStreamGroups = g.groups || [];
  }
  // At the edge the new rows are the paint - the TABLE is replaced and the top stays the top;
  // off the edge the table never moves. Either way the bar is not rebuilt: its live texts
  // update in place, so a picker the operator has open stays open. The first rows of an
  // empty stream have no table to replace yet and take the full paint.
  const arrived = pinned && !!(j.entries && j.entries.length);
  if (!dbStreamRepaint(t, arrived) && arrived) {
    renderDbGrid();
    return;
  }
  dbStreamBarDyn(t); // pill count / rate / gap / groups count in place
}

/* The Follow bar's live texts (docs/45 §2.3). renderDbGrid rebuilds the bar with
   every pinned repaint, so these refs are re-taken per paint; a not-pinned tick
   updates them in place — the pill count, the rate readout, the gap bar's
   visibility — without touching the table the operator is reading. */
let dbStreamHeldEl                     = null;
let dbStreamHitsEl                     = null;
let dbStreamReadBarEl                     = null;
let dbStreamPill                     = null;
let dbStreamPillN                     = null; // the count text; the arrow icon sits beside it, painted once (fix-plan #14)
let dbStreamRateEl                     = null;
let dbStreamGapEl                     = null;
let dbStreamGroupsBtn                     = null;

/** The groups button's label: the count once an answer has landed. */
function dbStreamGroupsLabel(t          )         {
  return tr("dataStream.groups") + (t.redisStreamGroups ? " (" + String(t.redisStreamGroups.length) + ")" : "");
}

function dbStreamBarDyn(t          )       {
  const n = (t.redisStreamPending || []).length;
  if (dbStreamPill) {
    dbStreamPill.hidden = n === 0;
    if (n && dbStreamPillN) {
      dbStreamPillN.textContent = t.redisStreamPendingDropped
        ? tr("dataStream.pendingNewOver", { n: STREAM_ROW_CAP })
        : trn(n, "dataStream.pendingNew.one", "dataStream.pendingNew.other");
    }
  }
  if (dbStreamRateEl) {
    dbStreamRateEl.hidden = !t.redisStreamRate;
    dbStreamRateEl.textContent = t.redisStreamRate ? tr("dataStream.rate", { r: t.redisStreamRate }) : "";
  }
  if (dbStreamGapEl) dbStreamGapEl.hidden = !t.redisStreamGap;
  if (dbStreamGroupsBtn) dbStreamGroupsBtn.textContent = dbStreamGroupsLabel(t);
  if (dbStreamHeldEl) dbStreamHeldEl.hidden = !t.redisStreamHold || !t.redisStreamFollow;
  if (dbStreamHitsEl) {
    const hits = dbStreamHitsText(t);
    dbStreamHitsEl.hidden = !hits;
    dbStreamHitsEl.textContent = hits;
  }
  dbStreamChipsPaint(t);
}

/** What a filtered window admits to (docs/49 §2.2): how many of the entries it
 *  examined matched. Empty when nothing is filtered, or when the answer carried no
 *  scan report — redis cannot count matches it never read, and a filter box with no
 *  number beside it beats one with a number nobody can trust. Pure. */
export function dbStreamHitsText(t          )         {
  if (!t.redisStreamMatch || t.redisStreamScanned == null) return "";
  return tr("dataStream.filterHits", {
    n: (t.redisStreamRows || []).length,
    s: t.redisStreamScanned,
  });
}

/** The pointer resting on the table, or leaving it (docs/49 §2.4). Nothing is
 *  fetched or repainted here: the flag is read by the next tick, which is where
 *  holding the edge actually happens. */
function dbStreamHold(on         )       {
  const t = dbTab();
  if (t.kind !== "key" || !!t.redisStreamHold === on) return;
  t.redisStreamHold = on;
  dbStreamBarDyn(t);
}

/** The summary strip's chips, rebuilt in place (docs/49 §2.3). They live at the end
 *  of the read bar and are addressed by their data hook, so a tick replaces the
 *  counts without touching the filter box the operator may be typing in. */
function dbStreamChipsPaint(t          )       {
  const bar = dbStreamReadBarEl;
  if (!bar || bar.isConnected === false) return;
  // The chips are held by reference rather than found by selector: they are the only
  // part of this bar a tick rewrites, and a query would also be a way to pick up
  // something else's node.
  dbStreamChipNodes.forEach((n             )       => { n.remove(); });
  dbStreamChipNodes = [];
  const by = dbStreamGroupField(t);
  if (!by) return;
  const all = streamSummary(t.redisStreamRows || [], by);
  // The strip is a summary, so it is bounded: nine chips wrap to two rows on the 19998
  // walk, and a field with two dozen values would push the table off the screen it is
  // summarising. The busiest are kept (they are sorted that way) and the rest are counted.
  all.slice(0, STREAM_CHIP_CAP).forEach((r                  )       => {
    const label = r.value + " " + String(r.n) + (r.rate ? " · " + tr("dataStream.rate", { r: r.rate }) : "");
    const chip = btn(label, {
      title: tr("dataStream.chipTitle", { f: by, v: r.value }),
      data: { schip: r.value },
    });
    chip.onclick = ()       => { void dbStreamApplyFilter(by + "=" + dbStreamQuote(r.value)); };
    bar.appendChild(chip);
    dbStreamChipNodes.push(chip);
  });
  if (all.length > STREAM_CHIP_CAP) {
    const rest = el("span", "db-hint", tr("dataStream.moreValues", { n: all.length - STREAM_CHIP_CAP }));
    bar.appendChild(rest);
    dbStreamChipNodes.push(rest); // it is part of the strip, and goes with it
  }
}

/* The chips painted last, so the next paint can take them out again. */
let dbStreamChipNodes                = [];
/* How many values the strip draws before it starts counting the rest. */
const STREAM_CHIP_CAP = 12;

/** A value on its way into a filter line: quoted when it carries a space or a quote,
 *  because the line is split the way a shell splits it (docs/49 §2.1). Pure. */
export function dbStreamQuote(v        )         {
  if (!/[\s"']/.test(v)) return v;
  return "\"" + v.replace(/(["\\])/g, "\\$1") + "\"";
}

/** Which field the summary strip groups by (docs/49 §2.3): the operator's choice
 *  when they made one — "" is a deliberate off — and otherwise the first column
 *  that looks like a dimension. Choosing for them is the point: a strip nobody
 *  opened is a strip nobody sees, and 90 rows a second is unreadable by default. */
function dbStreamGroupField(t          )         {
  const rows = t.redisStreamRows || [];
  if (t.redisStreamBy != null) return t.redisStreamBy;
  const cols = streamColumns(rows);
  // An automatic pick STAYS picked while its column is still there (found on the
  // 19998 walk): filtering to symbol=NVDA leaves that column one value, which stops
  // looking like a dimension, and the strip jumped to `price` — a different question
  // answered by the same strip, at the moment the operator narrowed the first one.
  const last = t.redisStreamByAuto;
  if (last && cols.indexOf(last) >= 0) return last;
  const pick = streamAutoField(rows, cols) || "";
  t.redisStreamByAuto = pick || null;
  return pick;
}

/** The gap bar's action (docs/45 §2.3): reopen the LATEST window — no cursor, the
 *  newest page straight from the server — and let it replace the rows. Splicing the
 *  two ends of a truncated window would present a contiguous stream that never was. */
async function dbStreamJumpLatest()                {
  const t = dbTab();
  if (t.kind !== "key" || !t.redisKey) return;
  // Jumping reopens the follow window itself: any tick still in flight speaks for
  // the window this jump is about to replace — letting its answer land would pool a
  // page behind a pill whose splice target no longer exists (docs/45 S3 follow-up).
  // Issuing on the same guard voids those ticks, exactly as a newer tick voids an
  // older one; the converse is accepted — a tick issued AFTER the jump supersedes
  // it, and dedup-plus-sort makes a late live-edge page a no-op at worst.
  const token = dbFollowReq.issue();
  const j = await apiJson                   (dbStreamUrl("/stream", t.redisKey, ""));
  if (!dbFollowReq.accepts(token)) return; // a newer tick or jump won the race
  if (!j) return; // apiJson toasted it; the gap bar stays and says the same thing
  if (dbTab() !== t) return; // the tab moved on mid-flight; the answer belongs to nobody
  dbStreamTake(t, j);
  renderDbGrid();
}
/** The reading bar (docs/49): the filter line, what it cost, and the summary strip.
 *  Its own row under Follow's, because the two answer different questions — Follow is
 *  about the live edge, this is about which of the entries are worth looking at. The
 *  input is built ONCE per paint and never rewritten by a tick: a box that rebuilt
 *  itself every second could not be typed in. */
function dbStreamReadBar(wrap             , t          )       {
  const bar = el("div", "db-detail-meta");
  // The box the operator is typing in is KEPT across the repaint, not rebuilt: a
  // filter applies 400 ms after the last keystroke and repaints the pane, and a
  // rebuilt input is a new node — focus, caret and IME state gone, the next
  // keystroke landing nowhere. (The MCP logs search learned this at docs/31.)
  // Moving a node still blurs it, so the focus is put back once the bar is in.
  const live = dbStreamBox && typeof document !== "undefined" && document.activeElement === dbStreamBox
    ? dbStreamBox
    : null;
  const caret = live ? live.selectionStart : null;
  const box = live || filterInput({
    placeholder: tr("dataStream.filterHint"),
    label: tr("dataStream.filter"),
    value: t.redisStreamMatch || "",
  });
  dbStreamBox = box;
  box.oninput = ()       => {
    // The same debounce the key-pattern box uses: a filter is a server walk, and
    // one walk per keystroke would spend the budget on words half typed.
    if (dbStreamFilterTimer != null) clearTimeout(dbStreamFilterTimer);
    dbStreamFilterTimer = setTimeout(()       => {
      dbStreamFilterTimer = null;
      void dbStreamApplyFilter(box.value);
    }, STREAM_FILTER_DEBOUNCE_MS);
  };
  box.onkeydown = (ev               )       => {
    if (ev.key !== "Enter") return;
    ev.preventDefault();
    if (dbStreamFilterTimer != null) clearTimeout(dbStreamFilterTimer);
    dbStreamFilterTimer = null;
    void dbStreamApplyFilter(box.value);
  };
  bar.appendChild(box);
  const hits = el("span", "db-hint", dbStreamHitsText(t));
  hits.hidden = !dbStreamHitsText(t);
  bar.appendChild(hits);
  dbStreamHitsEl = hits;
  const cols = streamColumns(t.redisStreamRows || []);
  if (cols.length) {
    bar.appendChild(h("span", null, tr("dataStream.groupBy")));
    const by = dbStreamGroupField(t);
    const sel = el("select", "db-stream-by")                     ;
    const add = (value        , label        )       => {
      const opt = el("option", "", label)                     ;
      opt.value = value;
      if (value === by) opt.selected = true;
      sel.appendChild(opt);
    };
    add("", tr("dataStream.groupOff"));
    cols.forEach((c        )       => { add(c, c); }); // a field name is wire data, not copy
    sel.onchange = ()       => {
      const cur = dbTab();
      if (cur.kind !== "key") return;
      cur.redisStreamBy = sel.value; // "" is a deliberate off, and outlasts the auto pick
      dbStreamChipsPaint(cur);
    };
    bar.appendChild(sel);
  }
  wrap.appendChild(bar);
  dbStreamReadBarEl = bar;
  if (live) {
    live.focus();
    // type=search supports the range; a stub or an exotic input might not, and a
    // caret that could not be restored is not worth failing a paint over.
    try { live.setSelectionRange(caret, caret); } catch { /* nothing to restore */ }
  }
  dbStreamChipsPaint(t);
}

/* One debounce for the filter box, module-scoped like the poller's own timer: the
   box is rebuilt on every full paint, so the timer cannot live on the element.
   dbStreamBox is the live node the paint above hands forward. */
let dbStreamFilterTimer                                       = null;
let dbStreamBox                          = null;
const STREAM_FILTER_DEBOUNCE_MS = 400;

/** Apply a filter line (docs/49 §2.2): a new filter is a NEW window, so the rows on
 *  screen go — they answered the old question, and splicing the two would present a
 *  table that never existed. Exported for the acceptance suite, which drives it the
 *  way the box's debounce would. */
export async function dbStreamApplyFilter(line        )                {
  const t = dbTab();
  if (t.kind !== "key" || !t.redisKey) return;
  if ((t.redisStreamMatch || "") === line) return;
  t.redisStreamMatch = line;
  // Issued on the follow guard: a tick in flight speaks for the window this is
  // about to replace, exactly as at a jump to latest.
  const token = dbFollowReq.issue();
  const j = await apiJson                   (dbStreamUrl("/stream", t.redisKey, ""));
  if (!dbFollowReq.accepts(token)) return;
  if (!j) return; // apiJson toasted it; the box keeps the line so it can be corrected
  if (dbTab() !== t) return; // the tab moved on mid-flight
  dbStreamTake(t, j);
  renderDbGrid();
}

/** The Follow bar: on/off, the interval, the rate readout, and the groups fold's toggle —
 *  one row in the value header's own vocabulary, painted above the table. */
function dbStreamFollowBar(wrap             , t          )       {
  const bar = el("div", "db-detail-meta");
  // The state IS the control (2026-09-28): a word that flipped Follow/Pause made the reader
  // work out which one the button reported and which one it would do. A switch says both.
  const on = !!t.redisStreamFollow;
  bar.appendChild(h("span", null, tr("dataStream.follow")));
  const followBtn = sw(on, tr("dataStream.follow"), {
    title: tr(on ? "dataStream.followOnTitle" : "dataStream.followOffTitle"),
    data: { stream: "follow" },
  });
  followBtn.onclick = ()       => {
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
  const sel = el("select", "db-stream-every")                     ;
  STREAM_EVERY_MS.forEach((ms        )       => {
    const opt = el("option", "", trn(ms / 1000, "dataStream.nSeconds.one", "dataStream.nSeconds.other"))                     ;
    opt.value = String(ms);
    if ((t.redisStreamEvery || STREAM_EVERY_MS[0]) === ms) opt.selected = true;
    sel.appendChild(opt);
  });
  sel.onchange = ()       => {
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
  // fix-plan #14: the "new rows above" arrow is the i-arrow-up sprite, so the count lives
  // in its own span — a repaint updates the text without rebuilding the icon.
  const pillN = h("span", null, pendN
    ? (t.redisStreamPendingDropped
      ? tr("dataStream.pendingNewOver", { n: STREAM_ROW_CAP })
      : trn(pendN, "dataStream.pendingNew.one", "dataStream.pendingNew.other"))
    : "");
  // The count is its own span: the poller rewrites it in place (dbStreamPillN) between renders.
  const pill = btn("", { icon: "arrow-up", data: { stream: "pill" } });
  pill.appendChild(pillN);
  pill.hidden = pendN === 0;
  pill.onclick = ()       => {
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
    if (wrap2) (wrap2               ).scrollTop = 0; // the flush lands at the edge
  };
  bar.appendChild(pill);
  dbStreamPill = pill;
  dbStreamPillN = pillN;
  // docs/49 §2.4: why the table stopped moving. Without this the hold reads as a
  // stall — the one thing a live view must never look like.
  const held = el("span", "db-hint", tr("dataStream.held"));
  held.hidden = !t.redisStreamHold || !t.redisStreamFollow;
  bar.appendChild(held);
  dbStreamHeldEl = held;
  // The gap bar: a truncated live-edge page skipped a middle chunk; the honest move is
  // reopening the latest window, not splicing two ends into a lie.
  const gap = btn(tr("dataStream.gapSkipped") + " · " + tr("dataStream.jumpLatest"), { data: { stream: "gap" } });
  gap.hidden = !t.redisStreamGap;
  gap.onclick = ()       => { void dbStreamJumpLatest(); };
  bar.appendChild(gap);
  dbStreamGapEl = gap;
  const rateEl = el("span", "db-hint", t.redisStreamRate ? tr("dataStream.rate", { r: t.redisStreamRate }) : "");
  rateEl.hidden = !t.redisStreamRate;
  bar.appendChild(rateEl);
  dbStreamRateEl = rateEl;
  bar.appendChild(el("span", "grow"));
  const groupsBtn = btn(dbStreamGroupsLabel(t));
  dbStreamGroupsBtn = groupsBtn;
  groupsBtn.onclick = ()       => {
    t.redisStreamGroupsOpen = !t.redisStreamGroupsOpen;
    // Opening the fold FETCHES (found on the 2026-09-23 walk): the groups answer used to
    // arrive only on Follow's fifth tick, so a reader who opened the fold with Follow off
    // read "no consumer groups" about a stream that has one - an empty state stating a
    // fact nobody had checked. The fold pays its own one command when it opens; Follow's
    // fifth-tick refresh keeps it current from there, and a closed fold still costs zero.
    if (t.redisStreamGroupsOpen) void dbStreamGroupsLoad(t);
    renderDbGrid();
  };
  bar.appendChild(groupsBtn);
  wrap.appendChild(bar);
}

/** The fold's own read: one XINFO GROUPS, behind the same tab guard every other stream
 *  request uses, so an answer that lands after the reader moved on repaints nothing. Its
 *  failure is deliberately quiet — apiJson already toasted, and a fold that keeps its last
 *  good rows beats one that blanks because a single poll missed. */
async function dbStreamGroupsLoad(t          )                {
  if (!t.redisKey) return;
  const g = await apiJson                                   (dbStreamUrl("/stream/groups", t.redisKey, ""));
  if (!g) return;
  if (dbTab() !== t) return; // the tab moved on mid-flight; the answer belongs to nobody
  t.redisStreamGroups = g.groups || [];
  renderDbGrid();
}

/** The consumer-group fold (docs/45 §2.4): a read-only table under the stream, fed by the
 *  fold's own open and then by every fifth Follow tick — pending, lag (null is the honest
 *  pre-7.0 answer), and the group's own delivered cursor. One node, table and hint together,
 *  so a tick replaces it whole. */
function dbStreamGroupsNode(t          )              {
  const wrap = el("div");
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  [tr("dataStream.colGroup"), tr("dataStream.colConsumers"), tr("dataStream.colPending"),
    tr("dataStream.colLag"), tr("dataStream.colLastDelivered")].forEach((label        )       => {
    hr.appendChild(el("th", "db-col db-rowctl", label));
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  const groups = t.redisStreamGroups || [];
  if (!groups.length) {
    tbl.appendChild(tbody);
    wrap.appendChild(tbl);
    // null is "the answer has not landed yet", [] is "the server says there are none":
    // saying "none" for the first is how the fold lied before the fetch existed.
    wrap.appendChild(el("div", "db-hint",
      t.redisStreamGroups == null ? tr("dataStream.groupsLoading") : tr("dataStream.groupsNone")));
    return wrap;
  }
  groups.forEach((g                     )       => {
    const r = el("tr");
    [g.name, String(g.consumers), String(g.pending), g.lag == null ? "—" : String(g.lag),
      g["last-delivered-id"] == null ? "" : String(g["last-delivered-id"])].forEach((cell        )       => {
      r.appendChild(el("td", "db-cell", cell));
    });
    tbody.appendChild(r);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
  return wrap;
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
export async function dbStreamLoadEarlier()                {
  const c = dbConn();
  const t = dbTab();
  if (!c.conn || t.kind !== "key" || !t.redisKey) return;
  const rows = t.redisStreamRows;
  if (dbStreamLoading || !rows || !rows.length || !t.redisStreamMore) return;
  // Under a filter the walk resumes where the last one STOPPED looking, not at the
  // oldest row it kept (docs/49 §2.2): a filter that found three matches in twenty
  // thousand entries would otherwise re-read the same twenty thousand on every click.
  const oldest = t.redisStreamScanTo || rows[rows.length - 1].id;
  dbStreamLoading = true;
  const token = dbStreamReq.issue();
  let j                           = null;
  try {
    j = await apiJson                   (dbStreamUrl("/stream", t.redisKey, "&before=" + encodeURIComponent(oldest)));
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
  if (typeof j.scannedTo === "string" && j.scannedTo) t.redisStreamScanTo = j.scannedTo;
  // The hits line counts the whole walk backwards, not this page: "3 in the newest
  // 40,000" is the fact an operator needs before deciding to keep clicking.
  if (j.scanned != null) t.redisStreamScanned = (t.redisStreamScanned || 0) + j.scanned;
  renderDbGrid();
}
