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

import type { ApiDbStreamEntry, ApiDbStreamWindow } from "./types/api.js";
import type { ApiDbRedisValue } from "./types/api.js";
import { apiJson, dbReqGuard, el } from "./util.js";
import { h } from "./h.js";
import { renderDbGrid } from "./data-grid.js";
import { dbConn, dbTab } from "./db-state.js";
import { tr } from "./i18n.js";
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
    wrap.appendChild(el("div", "db-hint", tr("dataStream.empty")));
    return;
  }
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
  if (t.redisStreamMore) {
    const btn = h("button", { class: "btn", type: "button" }, tr("dataStream.loadEarlier"));
    btn.onclick = (): void => { void dbStreamLoadEarlier(); };
    wrap.appendChild(btn);
  } else {
    wrap.appendChild(el("div", "db-hint", tr("dataStream.start")));
  }
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
    j = await apiJson<ApiDbStreamWindow>(
      "/api/db/" + encodeURIComponent(c.conn) + "/stream?key=" + encodeURIComponent(t.redisKey)
        + "&before=" + encodeURIComponent(oldest),
    );
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
