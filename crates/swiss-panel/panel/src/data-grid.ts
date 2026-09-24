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

import type { ApiDbColumn, ApiDbConnectionRow, ApiDbDataPage, ApiDbFkRow, DbQueryReply } from "./types/api.js";
import type { DbCellMeta, DbFilterTerm, DbGridConfig, DbInsert, DbTableTab } from "./types/state.js";
import type { MenuItem } from "./types/dom.js";
import { $, apiJson, dbReqGuard, el, emptyNode, errText, iconNode, lsMigrate, toast } from "./util.js";
import { DB_REDIS_TYPES, REDIS_THING_KEYS, dbIsRedis, dbRedisKeyMenu, dbRenderRedisValue } from "./data-browsers.js";
import { dbActivityLoad, dbActivityRender } from "./data-activity.js";
import { dbCloseAllTabs, dbOpenTab } from "./data-tabs.js";
import { dbCellMenu, dbCopyCsvCell, dbCopyText, dbExportCsv, dbOpenImport, dbResultCellMenu, dbRowForCopy, dbSelAll, dbSelectedForCopy } from "./data-csv.js";
import { dbOpenCellEditor, dbCellText, dbCellView } from "./data-cell.js";
import { dbEditCellEnter, dbTableMenu } from "./data-edit.js";
import { renderDbFilters, dbSqlPaint } from "./data-filters.js";
import { dbFavPush, dbFillConsole, dbFormatSql, dbRunSql, dbStatsSql, renderDbBar } from "./data-sql.js";
import { dbRenderTabs, renderDbDetailGrid } from "./data-structure.js";
import { renderDbFormView } from "./data-form.js";
import { h } from "./h.js";
import { DB_PAGE_SIZES, dbDropEdits, dbFkOpen, dbFocusedColumnValue, dbOkToDrop, dbPkKey, dbResultKey } from "./data-view.js";
import { dbConn, dbSqlTab, dbTab } from "./db-state.js";
import { locale, tr, trn } from "./i18n.js";
import { popupMenu } from "./ui/menu.js";
import { seg } from "./ui/seg.js";

/* --- one page of rows --------------------------------------------------------------------------- */

/* --- grid config: per-connection column widths and hidden columns (docs/22 W2.1) ---------------- */
/* dbgate keeps one GridConfig object per grid (GridConfig.ts + useGridConfig.ts); the equivalent
   here is ONE localStorage object per connection+table holding the widths the user dragged and
   the columns they hid. The helpers are pure (or storage-only) so the key format, the parse's
   tolerance of a corrupt entry and the clamp pin without a DOM. The grid reloads the config on
   every page load — a rename or a second tab's change is picked up, never cached stale. */
/* fix-plan #17: the swiss.dbGrid.* namespace replaced the mcp_gateway_db_grid_* prefix.
   The suffix after the prefix is unchanged, so dbGridConfigLoad can derive the old key and
   carry a stored config across on its first read. */
const DB_GRID_PREFIX = "swiss.dbGrid.";
const DB_GRID_PREFIX_OLD = "mcp_gateway_db_grid_";
const DB_COL_MIN = 48;  // narrower than the header's own name line and nothing reads
const DB_COL_MAX = 1200; // wider than the grid itself — a runaway drag helps nobody

function dbGridConfigKey(conn: string, schema: string, table: string): string {
  return DB_GRID_PREFIX + conn + "_" + (schema ? schema + "." : "") + table;
}

/** Parse one stored config. A missing, corrupt or wrongly-typed entry is a fresh config:
 *  storage is the only writer and quota errors can leave anything behind, but a broken grid
 *  is never an acceptable consequence. Widths outside the clamp are dropped, not clamped —
 *  a hand-edited 0 was never a width the user dragged. */
function dbGridConfigParse(raw: string | null): DbGridConfig {
  const out: DbGridConfig = { widths: {}, hidden: [] };
  if (!raw) return out;
  try {
    const j = JSON.parse(raw);
    if (!j || typeof j !== "object") return out;
    if (j.widths && typeof j.widths === "object") {
      Object.keys(j.widths).forEach((k: string): void => {
        const n = Number(j.widths[k]);
        if (isFinite(n) && n >= DB_COL_MIN && n <= DB_COL_MAX) out.widths[k] = Math.round(n);
      });
    }
    if (Array.isArray(j.hidden)) {
      out.hidden = j.hidden.filter((h: unknown) => { return typeof h === "string" && h; }) as string[];
    }
  } catch (e) { /* fresh config, see above */ }
  return out;
}

function dbGridConfigLoad(key: string | null): DbGridConfig {
  // New key first, old key only as the fallback (fix-plan #17); lsMigrate already answers
  // null for a blocked store, which parses to the fresh config below. A null key (no
  // object open) reads like the old code did: getItem(null) is just "absent".
  let raw: string | null = null;
  if (key != null && key.indexOf(DB_GRID_PREFIX) === 0) {
    raw = lsMigrate(key, DB_GRID_PREFIX_OLD + key.slice(DB_GRID_PREFIX.length));
  } else {
    try { raw = key == null ? null : localStorage.getItem(key); } catch (e) { raw = null; }
  }
  return dbGridConfigParse(raw);
}

function dbGridConfigSave(key: string, cfg: DbGridConfig): void {
  try { localStorage.setItem(key, JSON.stringify(cfg)); } catch (e) { /* quota/private mode: this session only */ }
}

/** The columns the grid shows after the config's hidden list. Pure. */
function dbGridVisibleColumns(columns: ApiDbColumn[], hidden: string[] | undefined): ApiDbColumn[] {
  if (!hidden || !hidden.length) return columns.slice();
  const drop: Record<string, boolean> = {};
  hidden.forEach((h: string): void => { drop[h] = true; });
  return columns.filter((c: ApiDbColumn): boolean => { return !drop[c.name]; });
}

/** The config's key for the table currently open (null when nothing is open). */
function dbGridKeyNow(): string | null {
  const c = dbConn();
  const t = dbTab();
  if (t.kind !== "table") return null; // a key tab has no grid geometry to address
  return c.conn && t.table ? dbGridConfigKey(c.conn, t.schema!, t.table) : null;
}

/** Hide one column (the header menu) / bring every hidden column back, writing through to
 *  storage so the choice survives a reload. */
function dbHideColumn(name: string): void {
  const c = dbConn();
  if (!c.gridCfg.hidden.includes(name)) c.gridCfg.hidden.push(name);
  const key = dbGridKeyNow();
  if (key) dbGridConfigSave(key, c.gridCfg);
  renderDbGrid();
}

function dbShowAllColumns(): void {
  const c = dbConn();
  c.gridCfg.hidden = [];
  const key = dbGridKeyNow();
  if (key) dbGridConfigSave(key, c.gridCfg);
  renderDbGrid();
}

/** Drag the column's right edge: width follows the pointer live across every cell of the
 *  column (cells captured at drag start — no repaint per mousemove), and the config is
 *  written once on release. The handle owns mousedown/click (stopPropagation) so a drag never
 *  sorts the column it is resizing. Each cell gets width AND maxWidth — under
 *  table-layout: auto a width alone is only a hint the nowrap content outvotes. */
function dbColResizeStart(e: MouseEvent, name: string, th: HTMLElement, cells: HTMLElement[]): void {
  e.preventDefault();
  e.stopPropagation();
  const c = dbConn();
  const startX = e.clientX;
  const startW = th.getBoundingClientRect().width;
  let lastW = startW;
  function move(ev: MouseEvent): void {
    lastW = Math.max(DB_COL_MIN, Math.min(DB_COL_MAX, Math.round(startW + ev.clientX - startX)));
    for (let i = 0; i < cells.length; i++) {
      cells[i].style.width = lastW + "px";
      cells[i].style.maxWidth = lastW + "px";
    }
  }
  function up(): void {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
    if (lastW !== Math.round(startW)) {
      c.gridCfg.widths[name] = lastW;
      const key = dbGridKeyNow();
      if (key) dbGridConfigSave(key, c.gridCfg);
    }
  }
  document.addEventListener("mousemove", move);
  document.addEventListener("mouseup", up);
}

/* --- keyboard navigation + TSV paste (docs/22 W2.2) --------------------------------------------- */
/* The grid owns a zero-size input that carries keyboard focus (dbgate's focus-field): the
   panel's tables are not focusable themselves, and keys have to land somewhere that neither
   scrolls the page nor starts a find. Clicking a cell, an arrow key or a typed character all
   move one focus cell; Enter/F2/typing open the same editors a double-click does; Ctrl+C
   copies the checked rows exactly like the row menu. Pasting TSV walks the edit buffers —
   Commit stays the only door to the database. */

/** Split clipboard text into a TSV grid. \r\n and lone \r both end a row (Excel and
 *  Windows clipboards speak them); a trailing newline ends the last row rather than opening
 *  an empty one. Pure. */
function dbTsvRows(text: string): string[][] {
  const t = String(text == null ? "" : text).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (!t) return [];
  const rows = t.split("\n");
  if (rows.length > 1 && rows[rows.length - 1] === "") rows.pop();
  return rows.map((r: string): string[] => { return r.split("\t"); });
}

/** Move the focus cell one key at a time, clamped to the grid's bounds. Pure. */
function dbKbdMove(r: number, c: number, key: string, maxR: number, maxC: number): { r: number; c: number } {
  if (key === "ArrowUp") return { r: Math.max(0, r - 1), c: c };
  if (key === "ArrowDown") return { r: Math.min(maxR, r + 1), c: c };
  if (key === "ArrowLeft") return { r: r, c: Math.max(0, c - 1) };
  if (key === "ArrowRight") return { r: r, c: Math.min(maxC, c + 1) };
  if (key === "Home") return { r: r, c: 0 };
  if (key === "End") return { r: r, c: maxC };
  return { r: r, c: c };
}

/** The grid row count the keyboard walks: buffered inserts first, then the page's rows. */
function dbGridRowsCount(): number {
  const t = dbTab();
  if (t.kind !== "table") return 0;
  return (t.data ? t.inserts.length + t.data.rows.length : 0);
}

/** Focus (or move) the focus cell and paint the ring — a single selection the keyboard owns.
 * Moving the ring must NOT rebuild the grid: the rebuild detached the cell mid-click, the
 * browser then never fired the click/dblclick that followed the mousedown, and double-click
 * editing could not open at all (docs/22 closeout audit P0-B). The ring moves between the
 * LIVE cells by their data-r/data-c address; a full repaint happens only on data changes
 * (edit, paste, commit, paging). */
function dbFocusCell(r: number, c: number): void {
  const t = dbTab();
  if (t.kind !== "table") return; // the focus ring belongs to a table tab's grid
  const maxR = dbGridRowsCount() - 1;
  const maxC = dbGridVisibleColumns(t.data ? t.data.columns : [], (dbConn().gridCfg || { hidden: [] }).hidden).length - 1;
  t.focus = { r: Math.max(0, Math.min(maxR, r)), c: Math.max(0, Math.min(maxC, c)) };
  const wrap = $("dbGridWrap");
  if (!wrap) return;
  const old = wrap.querySelector("td.db-focus");
  if (old) old.classList.remove("db-focus");
  const td = wrap.querySelector('td[data-r="' + t.focus.r + '"][data-c="' + t.focus.c + '"]');
  if (td) td.classList.add("db-focus");
  const kbd = $("dbKbd");
  // preventScroll: the input sits at the end of the scrolled content, so a plain focus()
  // would drag the pane to the bottom and then scrollIntoView would snap the clicked
  // row to the top - the pane must stay where the user's click left it.
  if (kbd) kbd.focus({ preventScroll: true });
  if (td && td.scrollIntoView) td.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Open the focused cell's editor the way a double-click does. seed (a typed character)
 *  replaces the current text; without one the editor starts from the cell's value. */
function dbFocusEdit(seed?: string | null): void {
  const td = document.querySelector("#dbGridWrap td.db-focus") as HTMLElement & { dbKbdEdit?: (seed: string | null) => void };
  if (!td || !td.dbKbdEdit) return;
  td.dbKbdEdit(seed == null ? null : String(seed));
}

/** Write one pasted cell into an insert row's buffer with the same semantics a typed edit
 *  has: an insert cell reverts to the column default on empty. */
function dbPasteInsertCell(t: DbTableTab, i: number, column: string, raw: string): void {
  const ins = t.inserts[i]!;   // a paste only targets a row that still exists
  if (raw === "") delete ins.values[column];
  else ins.values[column] = raw;
}

/** Write one pasted cell into an existing row's buffer: the update collapses back when the
 *  pasted text equals the original. */
function dbPasteUpdateCell(t: DbTableTab, key: string, column: string, meta: DbCellMeta, raw: string): void {
  // meta.pk is already the pk VALUES map the caller built (same object dbSaveInlineEdit
  // stores); running it through dbPkVals again would forEach over an object and throw.
  const upd = t.updates[key] || (t.updates[key] = { pk: meta.pk as Record<string, unknown>, changes: {} });
  const origTxt = meta.orig == null ? "" : String(meta.orig);
  if (raw === origTxt) {
    delete upd.changes[column];
    if (!Object.keys(upd.changes).length) delete t.updates[key];
  } else {
    upd.changes[column] = raw;
  }
}

/** Paste a TSV grid starting at the focused cell (docs/22 W2.2). Cells map onto the VISIBLE
 *  columns left to right; paste rows past the page's last row grow NEW buffered inserts; a
 *  wider paste than the grid simply drops its extra cells. Every value lands in the local
 *  buffers exactly as if it had been typed — Commit is still the only write. */
/** docs/22 W4.1 + W4.2: the values one buffered row ships to the commit. The pk map
 *  carries the WHOLE original row for every table now: a keyless table addresses by every
 *  column (a NULL in any column makes the row unaddressable — the server refuses those
 *  rows at commit), and a keyed table's server reads the key columns for addressing and
 *  the SAME map's other entries as the row's originals — the optimistic lock that a lost
 *  commit race reports as 409 with the moved column names. Every buffer writer (cell
 *  edit, dialog, paste, delete button) goes through here so the commit payload and the
 *  SQL preview keep one shape. */
function dbRowAddr(pkCols: string[], columns: ApiDbColumn[], row: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  columns.forEach((c: ApiDbColumn): void => { out[c.name] = row[c.name]; });
  return out;
}

/** Structural equality for two JSON values — the 409 body's row map against a buffered
 *  entry's pk, so the conflict lands on the exact buffered row (docs/22 W4.2). */
function dbSameJson(a: Record<string, unknown> | null, b: Record<string, unknown> | null): boolean {
  if (a === b) return true;
  if (a == null || b == null || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a), kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k: string): boolean => { return dbSameJson(a[k] as Record<string, unknown> | null, b[k] as Record<string, unknown> | null); });
}

/** docs/22 W4.2: a lost edit race answers 409 naming the moved columns — but the POST
 *  itself lives in data-sql.js's dbCommit, which this module cannot call back into. The
 *  narrowest honest seam: watch our own /edits responses at the fetch level (path match
 *  plus status; every other request passes through untouched). The failing commit already
 *  keeps the buffer and apiJson already toasts the body's error text with the column
 *  names; what only the grid can do is remember WHICH buffered row and columns lost, so
 *  the next render paints those cells red until the buffer for them changes. */
(() => {
  // The module also loads where no DOM exists (the Node vitest suite imports the grid
  // module for its state shape) — touching window at module level breaks that import
  // with a ReferenceError, so the observer only arms where a window with fetch is real.
  if (typeof window === "undefined" || !window.fetch) return;
  const orig = window.fetch;
  window.fetch = function (this: Window & typeof globalThis, input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    return orig.call(this, input, init).then((r: Response): Response => {
      try {
        const path = typeof input === "string" ? input : input instanceof Request ? input.url : String(input);
        if (/\/edits(\?|$)/.test(path)) {
          let d = dbTab();
          if (r.status === 409) {
            r.clone().json().then((j: { conflictColumns?: string[]; row?: Record<string, unknown> }): void => {
              const t = dbTab();
              d = t;
              const cols = (j && j.conflictColumns) || [];
              if (t.kind !== "table" || !cols.length || !j || !j.row) return;
              const hit = Object.keys(t.updates).find((k) => {
                return dbSameJson(t.updates[k]!.pk, j.row!);
              });
              if (hit) { t.conflict = { key: hit, columns: cols }; renderDbGrid(); }
            }).catch(() => { /* an observation never breaks the panel */ });
          } else if (d.kind === "table" && d.conflict) {
            d.conflict = null; // a clean answer (or a different failure) clears the marks
            renderDbGrid();
          }
        }
      } catch (e) { /* never */ }
      return r;
    });
  } as typeof fetch;
})();

function dbPasteApply(text: string, r0: number, c0: number): void {
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!d.data || !d.data.editable) {
    toast(tr("dataGrid.notEditablePaste", { note: d.data && d.data.editNote ? d.data.editNote : tr("dataGrid.noPrimaryKey") }), true);
    return;
  }
  const cfg = dbConn().gridCfg || { widths: {}, hidden: [] };
  const cols = dbGridVisibleColumns(d.data?.columns, cfg.hidden);
  const rows = dbTsvRows(text);
  if (!rows.length || !cols.length) return;
  const pkCols = d.data?.primaryKey || [];
  const keyOf = (row: Record<string, unknown>, i: number): string => { return pkCols.length ? dbPkKey(pkCols, row) : String(i); };
  for (let i = 0; i < rows.length; i++) {
    const target = r0 + i;
    for (let j = 0; j < rows[i].length; j++) {
      const colIdx = c0 + j;
      if (colIdx >= cols.length) break;
      const col = cols[colIdx].name;
      const raw = rows[i][j];
      const insertsBefore = d.inserts.length;
      if (target < insertsBefore) {
        dbPasteInsertCell(d, target, col, raw);
      } else {
        const ri = target - d.inserts.length;
        if (ri < d.data.rows.length) {
          const row = d.data?.rows[ri];
          const key = keyOf(row, ri);
          dbPasteUpdateCell(d, key, col, { pk: dbRowAddr(pkCols, d.data?.columns, row), orig: row[col] }, raw);
        } else {
          // Past the page: the paste row becomes a NEW buffered insert, values in column order.
          d.inserts.push({ values: {} });
          dbPasteInsertCell(d, d.inserts.length - 1, col, raw);
        }
      }
    }
  }
  renderDbGrid();
  renderDbBar();
}

/** Ctrl+C: the checked rows as CSV (the row menu's Copy), or — with nothing checked — the
 *  focused row alone, which is what a grid hand expects Ctrl+C to grab. */
function dbCopyChecked(): void {
  const d = dbTab();
  if (d.kind !== "table") return;
  const sel = dbSelectedForCopy();
  if (sel && sel.rows && sel.rows.length) {
    // Same shape the row menu's Copy-as-CSV builds (data-csv keeps dbRowsCsv private): a
    // header line plus one quoted line per checked row.
    const lines = [sel.cols.map(dbCopyCsvCell).join(",")];
    sel.rows.forEach((r: Record<string, unknown>): void => { lines.push(sel.cols.map((cn: string): string => { return dbCopyCsvCell(r[cn]); }).join(",")); });
    dbCopyText(lines.join("\n"));
    return;
  }
  if (!d.focus || !d.data) return;
  const nIns = d.inserts.length;
  if (d.focus.r < nIns) return; // a buffered insert has no committed row to copy
  const row = d.data?.rows[d.focus.r - nIns];
  const pkCols = d.data?.primaryKey || [];
  const key = pkCols.length ? dbPkKey(pkCols, row) : String(d.focus.r - nIns);
  const full = dbRowForCopy(row, key);
  const line = d.data?.columns.map((c: ApiDbColumn): string => {
    return dbCopyCsvCell(full == null ? null : full[c.name]);
  }).join(",");
  dbCopyText(line);
}

// One /data request chain: a slow answer for the table (or page) the user just left must be
// dropped, or it would repaint the grid — and rewrite d.schema — from the OLD table (docs/22
// closeout audit).
const dbDataReq = dbReqGuard();

async function dbLoadData(keepOffset?: boolean, keepEdits?: boolean): Promise<void> {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return; // a key tab has no row page to load
  if (!c.conn || !d.table) return;
  if (!keepOffset) d.offset = 0;
  d.loading = true;
  renderDbToolbar(); renderDbGrid();
  let q = "/api/db/" + encodeURIComponent(c.conn) + "/data?table=" + encodeURIComponent(d.table) +
    "&offset=" + d.offset + "&limit=" + d.pageSize;
  if (d.schema) q += "&schema=" + encodeURIComponent(d.schema);
  if (d.order) q += "&order=" + encodeURIComponent(d.order) + "&dir=" + d.dir;
  if (d.filters.length) q += "&filters=" + encodeURIComponent(JSON.stringify(d.filters));
  const token = dbDataReq.issue();
  const j = await apiJson<ApiDbDataPage>(q);
  if (!dbDataReq.accepts(token)) return; // superseded: a newer load owns the pane and the flag
  d.loading = false;
  if (!j) { renderDbToolbar(); renderDbGrid(); return; }
  // A Commit that emptied the last page (or a filter that shrank the set) can leave this
  // offset past the end of what remains: an empty page with rows behind it is one page
  // back — re-fetch there instead of painting an empty grid whose footer reads "51–50 of
  // 50" (docs/22 closeout audit).
  if (!j.rows.length && d.offset > 0) {
    d.offset = Math.max(0, d.offset - d.pageSize);
    void dbLoadData(true, keepEdits);
    return;
  }
  d.data = j;
  d.schema = j.schema;
  // The grid config reloads with every page: a rename (new key) or a second tab's hide lands
  // on the next paint instead of a cached opinion (docs/22 W2.1). It is connection state
  // (one geometry per conn+table), so the write lands on the connection record.
  const c2 = dbConn();
  c2.gridCfg = dbGridConfigLoad(dbGridKeyNow()!);
  // A fresh page is a fresh baseline — buffered edits never survive a RELOAD (docs/22). The
  // background tab's return fetch is not one: see dbRestoreData.
  if (!keepEdits) dbDropEdits();
  // A filter naming a column that no longer exists would 400 on every reload — drop it instead.
  const names = d.data?.columns.map((c: ApiDbColumn): string => { return c.name; });
  d.filters = d.filters.filter((f: DbFilterTerm): boolean => { return names.includes(f.column); });
  renderDbToolbar(); renderDbGrid(); renderDbBar(); renderDbFilters();
}

/** docs/42 D4's return fetch: the page a tab dropped when it went to the background, re-read at
 *  the same coordinates. What it must NOT do is apply the fresh-baseline rule above — going to
 *  the background is not a reload the user asked for, and the strip counts that tab's buffered
 *  writes the whole time it waits there (the card's dot, the page-leave guard, the close
 *  confirm). Dropping them on return would make switching tabs a silent discard of typed-in
 *  changes — found live on 19998 during the docs/42 T2 walk. The writes are keyed by primary
 *  key, so they land back on the same rows; a row that moved under them still answers at Commit,
 *  which is where a conflict belongs. */
async function dbRestoreData(): Promise<void> { await dbLoadData(true, true); }

/* Whole-table export in the chosen format (CSV, NDJSON, or SQL dump). The toolbar button carries the
   busy state: the format menu is gone by the time the download starts, so the button is the
   only place left on screen that can say "working". */
async function dbExportTable(btn: HTMLButtonElement, fmt: string): Promise<void> {
  const c2 = dbConn();
  const d2 = dbTab();
  if (d2.kind !== "table") return;
  const name = fmt === "json" ? "NDJSON" : fmt === "sql" ? "SQL dump" : "CSV";
  if (!confirm(tr("dataGrid.exportTableAs", { table: (d2.schema ? d2.schema + "." : "") + d2.table, format: name }))) return;
  let q = "/api/db/" + encodeURIComponent(c2.conn!) + "/export?table=" + encodeURIComponent(d2.table!) +
    "&format=" + fmt;
  if (d2.schema) q += "&schema=" + encodeURIComponent(d2.schema);
  // The grid's filters ride along: the download and the grid describe the same filtered
  // set, and the exported row count is the filtered total.
  if (d2.filters.length) q += "&filters=" + encodeURIComponent(JSON.stringify(d2.filters));
  btn.disabled = true;
  btn.textContent = tr("dataGrid.exporting");
  try {
    const resp = await fetch(q);
    if (!resp.ok) { toast(tr("dataGrid.exportFailedHttpN", { n: resp.status }), true); return; }
    const blob = await resp.blob();
    const disp = resp.headers.get("Content-Disposition") || "";
    const m = /filename="([^"]+)"/.exec(disp);
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = m ? m[1] : "export." + fmt;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => { URL.revokeObjectURL(a.href); }, 1000);
    const rows = resp.headers.get("X-Export-Rows");
    toast(rows != null ? tr("dataGrid.exportedNRows", { n: Number(rows).toLocaleString(locale()) }) : tr("dataGrid.exported"));
  } catch (e) {
    toast(tr("dataGrid.exportFailed"), true);
  } finally {
    btn.disabled = false;
    btn.textContent = tr("dataGrid.export");
  }
}

/* --- docs/43 M4: the toolbar draws ONLY the active tab's controls ----------------------------
   One primary action + one overflow menu per tab kind; the pager and page-size moved to
   the status bar (renderDbStatus). The non-icon .btn budget is ONE per toolbar — the
   machine gate in test/db-toolbar.test.ts counts exactly that. */

/** The overflow items for a TABLE tab: page + whole-table actions, then the Table menu's
 *  guarded DDL ops (they sat beside the segment before the fold). */
function dbMoreItemsForTable(more: HTMLElement, tt: DbTableTab): MenuItem[] {
  const items: MenuItem[] = [
    {
      label: tr("dataGrid.refresh"), title: tr("dataGrid.reloadPageDropsBuffered"),
      fn: (): void => { if (dbOkToDrop()) { dbDropEdits(); void dbLoadData(true); } },
    },
    { label: tr("dataGrid.csv"), title: tr("dataGrid.downloadCurrentPageCsv"), fn: (): void => { dbExportCsv(); } },
  ];
  if (tt.data) {
    items.push({ sep: true });
    items.push({ heading: true, label: tr("dataGrid.exportWholeTableCsv"), fn: (): void => {} });
    items.push({ label: tr("dataGrid.exportCsv"), fn: (): void => { void dbExportTable(more as HTMLButtonElement, "csv"); } });
    items.push({ label: tr("dataGrid.exportNdjson"), fn: (): void => { void dbExportTable(more as HTMLButtonElement, "json"); } });
    items.push({ label: tr("dataGrid.exportSqlDump"), fn: (): void => { void dbExportTable(more as HTMLButtonElement, "sql"); } });
    if (tt.data.editable) {
      items.push({ sep: true });
      items.push({ label: tr("dataGrid.import"), title: tr("dataGrid.insertCsvRowsOne"), fn: (): void => { dbOpenImport(); } });
    }
  }
  if (!dbIsRedis()) {
    items.push({ sep: true });
    // fix-plan #14: the down-caret the copy used to spell is the trailing chevron
    // affordance - the row opens ANOTHER menu (rename/truncate/drop), and menu.ts paints
    // that promise.
    items.push({ label: tr("dataStructure.table"), title: tr("dataStructure.renameTruncateDropTable"), affordance: "chevron-down", fn: (): void => { dbTableMenu(more); } });
  }
  return dbWithActivity(items);
}

/** One ⋯ per head (docs/46 P7): on a SQL connection the open object's overflow ends with the
 *  pane's own action, the Activity monitor, so the pane's ⋯ never stands beside it. */
function dbWithActivity(items: MenuItem[]): MenuItem[] {
  if (dbIsRedis()) return items;
  items.push({ sep: true });
  items.push({ label: tr("dataView.activity"), fn: (): void => { dbOpenTab({ kind: "activity" }); } });
  return items;
}

/** The overflow items for a SQL tab: the secondary run modes, the formatter, favorites and
 *  history — everything the console's old flat row carried, one hover deep. A Redis command
 *  has no plan and SQL formatting has nothing to say about it (docs/22 W5.4), so its console
 *  gets neither: the old flat row hid both buttons, and the fold into this menu had dropped
 *  that until docs/46 P7. */
function dbMoreItemsForSql(): MenuItem[] {
  const st = dbSqlTab();
  const d = dbConn();
  const items: MenuItem[] = dbIsRedis() ? [] : [
    { label: tr("dataView.explain"), fn: (): void => { void dbRunSql("plan"); } },
    { label: tr("dataView.explainAnalyze"), fn: (): void => { void dbRunSql("analyze"); } },
    { label: tr("dataView.format"), fn: (): void => { dbSqlFormatNow(); } },
  ];
  items.push({ label: tr("dataView.saveFavorites"), title: tr("dataView.saveConsoleTextFavorites"), fn: (): void => { if (st) dbFavPush(st.sqlText); } });
  const loadSql = (q: string): void => {
    const s2 = dbSqlTab();
    if (!s2) return;
    s2.sqlText = q;
    const ta = $<HTMLTextAreaElement>("dbSql");
    if (ta) { ta.value = q; dbSqlPaint(); }
  };
  if (d.favorites && d.favorites.length) {
    items.push({ sep: true });
    items.push({ heading: true, label: tr("dataView.favoritesTitle"), fn: (): void => {} });
    d.favorites.slice(0, 8).forEach((q: string): void => {
      items.push({ label: q.slice(0, 60), title: q, fn: (): void => { loadSql(q); } });
    });
  }
  if (d.history && d.history.length) {
    items.push({ sep: true });
    items.push({ heading: true, label: tr("dataView.history"), fn: (): void => {} });
    d.history.slice(0, 8).forEach((q: string): void => {
      items.push({ label: q.slice(0, 60), title: q, fn: (): void => { loadSql(q); } });
    });
  }
  return dbWithActivity(items);
}

/** The overflow items for a redis KEY tab: the key's own guarded menu plus the console. */
function dbMoreItemsForKey(more: HTMLElement): MenuItem[] {
  return [
    { label: tr("dataBrowsers.renameDeleteKey"), fn: (): void => { dbRedisKeyMenu(more); } },
    { label: dbIsRedis() ? tr("dataGrid.command") : tr("dataSql.sql"), fn: (): void => { dbOpenTab({ kind: "sql" }); } },
  ];
}

/** The overflow items for an ACTIVITY tab — the generic object actions; its Refresh IS the
 *  primary action, so it is not repeated here. */
function dbMoreItemsForActivity(): MenuItem[] {
  return [
    { label: tr("dataGrid.refresh"), fn: (): void => { void dbActivityLoad(); } },
    { label: tr("dataTabs.closeAll"), fn: (): void => { dbCloseAllTabs(); } },
  ];
}

/** One ellipsis button whose menu is built at CLICK time from live state. */
function dbMoreButton(build: (more: HTMLElement) => MenuItem[]): HTMLElement {
  const b = h("button", {
    class: "btn icon", type: "button",
    title: tr("dataView.moreActions"), aria: { haspopup: "menu" },
  }, iconNode("ellipsis"));
  b.onclick = (ev: MouseEvent): void => {
    ev.stopPropagation();
    popupMenu(b.getBoundingClientRect(), build(b));
  };
  return b;
}

/** docs/43 M4: format the console's text in place — the old #dbSqlFormat button's body. */
function dbSqlFormatNow(): void {
  const st = dbSqlTab();
  if (!st || !st.sqlText.trim()) return;
  st.sqlText = dbFormatSql(st.sqlText);
  const ta = $<HTMLTextAreaElement>("dbSql");
  if (ta) { ta.value = st.sqlText; dbSqlPaint(); ta.focus(); }
}

/* --- docs/43 M4: the status bar ---------------------------------------------------------------
   One line under the pane body: pager + page-size for table tabs, row/timing facts for
   the others, editability in the table's own words (editNote verbatim — a read-only
   foreign database says WHY), and the connection always named at the right edge. The
   commit bar (.db-bar) is a different line with a different job. */

function renderDbStatus(): void {
  const bar = $("dbStatus");
  if (!bar) return;
  bar.textContent = "";
  const c = dbConn();
  const t = dbTab();
  if (t.kind === "table" && t.data) {
    // The page-size select keeps its data-tb address — #pane's delegated change listener
    // answers it, and the refused-discard restore reads live state (docs/37 R5).
    bar.appendChild(h("select", { class: "db-pagesize", title: tr("dataGrid.rowsPage"), data: { tb: "pagesize" } },
      DB_PAGE_SIZES.map((n: number) => {
        return h("option", { value: String(n), selected: n === t.pageSize }, String(n));
      })));
    const first = t.offset + 1;
    const to = t.offset + t.data.rows.length;
    bar.appendChild(el("span", "db-pageinfo",
      t.data.total ? tr("dataGrid.bT", { a: first.toLocaleString(locale()), b: to.toLocaleString(locale()), t: t.data.total.toLocaleString(locale()) }) : tr("dataGrid.n0Rows")));
    bar.appendChild(h("button", {
      class: "btn icon", title: tr("dataGrid.previousPage"), disabled: t.offset === 0, data: { pg: "prev" },
    }, iconNode("chevron-left")));
    bar.appendChild(h("button", {
      class: "btn icon", title: tr("dataGrid.nextPage"),
      disabled: t.data.nextPage != null ? !t.data.nextPage : to >= t.data.total, data: { pg: "next" },
    }, iconNode("chevron-right")));
    // No editability sentence here (docs/46 §3.7): the head's meta line already says it in the
    // server's own words, and the same sentence twice on one screen read as two facts.
  } else if (t.kind === "sql" && t.sqlResult) {
    const res = t.sqlResult;
    bar.appendChild(el("span", "db-status-note",
      trn(res.rowCount, "dataGrid.nRows.one", "dataGrid.nRows.other") +
      (res.elapsedMs != null ? tr("dataGrid.msMs", { ms: res.elapsedMs }) : "")));
  } else if (t.kind === "key" && t.redisValue) {
    bar.appendChild(el("span", "db-status-note",
      t.redisValue.type + (t.redisValue.ttl != null && t.redisValue.ttl >= 0 ? tr("dataGrid.ttlSecs", { n: t.redisValue.ttl }) : "")));
  }
  bar.appendChild(el("span", "grow"));
  const conn0 = c.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === c.conn; });
  bar.appendChild(el("span", "db-status-conn", (conn0 ? conn0.label : c.conn) || ""));
}

function renderDbToolbar(): void {
  const d = dbConn();
  const t = dbTab();
  const head = $("dbHead");
  if (!head) return;
  head.textContent = "";
  const left = el("div", "db-head-left");
  if (t.kind === "activity") {
    const conn0 = d.conns.find((c: ApiDbConnectionRow): boolean => { return c.name === d.conn; });
    left.appendChild(el("h2", "db-title pane-title", tr("dataActivity.title")));
    left.appendChild(el("div", "db-meta", (conn0 ? conn0.label : d.conn || "") + " · " + tr("dataActivity.refreshesWhileOpen")));
  } else if (t.kind === "sql" && t.sqlResult) {
    const res = t.sqlResult;
    left.appendChild(el("h2", "db-title pane-title", res.explained ? tr("dataGrid.executionPlan") : (dbIsRedis() ? tr("dataGrid.commandReply") : tr("dataGrid.sqlResults"))));
    left.appendChild(el("div", "db-meta", trn(res.rowCount, "dataGrid.nRows.one", "dataGrid.nRows.other") +
      (res.note ? tr("dataGrid.note", { note: res.note }) : "") +
      (res.elapsedMs != null ? tr("dataGrid.msMs", { ms: res.elapsedMs }) : "")));
    if ((t.sqlResults || []).length > 1) {
      // The library's seg (docs/46 §3.7), inside a strip that scrolls sideways: eight results
      // fit, a longer script's ninth scrolls the strip instead of wrapping the head.
      left.appendChild(h("div", { class: "db-rtabs" },
        seg((t.sqlResults || []).map((r: DbQueryReply, ti: number) => {
          return { id: String(ti), label: r.tabLabel || tr("dataGrid.resultN", { n: ti + 1 }) };
        }), String(t.resultTab), { key: "rtab" })));
    }
  } else if (t.kind === "table" && t.data) {
    left.appendChild(el("h2", "db-title pane-title", (t.data.schema ? t.data.schema + "." : "") + t.data.table));
    const bits = [tr("dataGrid.nRows2", { n: t.data.total.toLocaleString(locale()) })];
    const pkCols0 = t.data.primaryKey || [];
    bits.push(t.data.editable
      ? (pkCols0.length ? tr("dataGrid.editableChangesBufferUntil") : tr("dataGrid.editableNote", { note: t.data.editNote || tr("dataGrid.rowsAddressedAllColumns") }))
      : (t.data.editNote || tr("dataGrid.browsingOnly")));
    left.appendChild(el("div", "db-meta", bits.join("  ·  ")));
  } else if (t.kind === "sql") {
    left.appendChild(el("h2", "db-title pane-title", dbIsRedis() ? tr("dataGrid.command") : tr("dataSql.sql")));
    left.appendChild(el("div", "db-meta", dbIsRedis() ? tr("dataGrid.commandConsoleTitle") : tr("dataGrid.sqlConsoleTitle")));
  } else if (dbIsRedis()) {
    const kt = t.kind === "key" ? t : null;
    left.appendChild(el("h2", "db-title pane-title", kt && kt.redisKey ? kt.redisKey : tr("dataGrid.keys")));
    const conn2 = d.conns.find((c: ApiDbConnectionRow): boolean => { return c.name === d.conn; });
    left.appendChild(el("div", "db-meta",
      (conn2 ? conn2.label : "") +
      (d.redis && d.redis.total != null ? tr("dataGrid.nKeys", { n: Number(d.redis.total).toLocaleString(locale()) }) : "") +
      tr("dataGrid.browsingEditValuesPlace")));
  } else {
    const table = t.kind === "table" ? t.table : null;
    left.appendChild(el("h2", "db-title pane-title", tr("dataGrid.data")));
    left.appendChild(el("div", "db-meta", d.conn ? (table ? tr("dataGrid.loading") : tr("dataGrid.selectTableLeft")) : tr("dataGrid.databaseMcpRegistered")));
  }
  head.appendChild(left);

  // docs/43 M4: the control side is per-kind — the segment (table tabs), ONE primary
  // action, and the overflow. The pager and page-size are the status bar's now; the
  // console opener rides the overflow of every non-sql tab.
  const ctl = el("div", "db-head-ctl");
  const nosql = dbIsRedis();
  const tt = t.kind === "table" ? t : null;
  let objMore = true; // every branch below that draws controls ends with the object's ⋯
  if (tt && tt.data && !nosql) dbRenderTabs(ctl);
  if (tt && tt.data) {
    if (tt.data.editable) {
      ctl.appendChild(h("button", {
        class: "btn", title: tr("dataGrid.bufferNewRowInserted"), data: { tb: "addrow" },
      }, tr("dataGrid.row")));
    }
    ctl.appendChild(dbMoreButton((more: HTMLElement): MenuItem[] => { return dbMoreItemsForTable(more, tt); }));
  } else if (t.kind === "sql") {
    ctl.appendChild(h("button", {
      class: "btn", id: "dbSqlRun", title: tr("dataView.statementsSplitCtrlEnter"),
    }, tr("dataView.run")));
    ctl.appendChild(dbMoreButton((): MenuItem[] => { return dbMoreItemsForSql(); }));
  } else if (t.kind === "key" && t.redisValue) {
    const cfg = DB_REDIS_TYPES[t.redisValue.type];
    if (cfg && cfg.ins.length) {
      ctl.appendChild(h("button", {
        class: "btn", type: "button", title: tr("dataBrowsers.bufferNewThingApplied", { thing: tr(REDIS_THING_KEYS[cfg.thing] ?? cfg.thing) }), data: { radd: "" },
      }, tr(cfg.add)));
    }
    ctl.appendChild(dbMoreButton((more: HTMLElement): MenuItem[] => { return dbMoreItemsForKey(more); }));
  } else if (t.kind === "activity") {
    ctl.appendChild(h("button", {
      class: "btn", title: tr("dataActivity.title"), data: { actrefresh: "" },
    }, tr("dataGrid.refresh")));
    ctl.appendChild(dbMoreButton((): MenuItem[] => { return dbMoreItemsForActivity(); }));
  } else objMore = false;
  head.appendChild(ctl);
  // One ⋯ per head (docs/46 P7): the pane's ⋯ - the Activity monitor, a SQL connection's page
  // action - stands in only while no open object has a ⋯ of its own; an object's ⋯ carries
  // Activity… as its last row instead. Two identical glyphs a hairline apart, opening two
  // different menus, was what the docs/43 M4 regression fix first put on screen.
  const paneMore = $("dbMore");
  if (paneMore) paneMore.hidden = !d.conn || nosql || objMore;
}



/** docs/22 W1.4: fill the console with one generated stats statement and run it, so the SQL
 *  is on screen (and in history) rather than hidden behind a one-off request. */
function dbRunColumnStats(column: string, kind: string): void {
  const c = dbConn();
  const t = dbTab();
  if (t.kind !== "table") return; // stats run against a table tab's column
  const dialect = (c.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === c.conn; }) || {} as { dialect?: string }).dialect || "mysql";
  let sql: string;
  try {
    sql = dbStatsSql(dialect, t.schema!, t.table!, column, kind);
  } catch (err) { toast(errText(err), true); return; }
  dbFillConsole(sql);
  void dbRunSql();
}

/** Paint one cell from its typed view (docs/22 W2.3). Returns the value's long-text title
 *  (the folded JSON's full text; null when the value needs none) so callers can merge it with
 *  their own hint instead of clobbering it. */
function dbPaintCell(td: HTMLTableCellElement, v: unknown, has: boolean, colType?: string): string | null {
  if (!has || v === undefined) return null; // nothing set yet (an insert stub cell)
  if (v === null) { td.appendChild(el("span", "db-null", tr("dataGrid.null"))); return null; }
  const view = dbCellView(v, colType);
  if (view.href) {
    const a = el("a", "db-link", view.text);
    a.href = view.href;
    a.target = "_blank";
    a.rel = "noopener noreferrer"; // a data grid can point anywhere; the panel vouches for none of it
    td.appendChild(a);
  } else td.textContent = view.text;
  if (view.cls) td.classList.add(view.cls);
  if (view.title) td.title = view.title;
  return view.title || null;
}

/* --- column header hover card -------------------------------------------------------------------- */

const dbTip: { node: HTMLElement | null; timer: ReturnType<typeof setTimeout> | number } = { node: null, timer: 0 };

function dbTipHide(): void {
  // Cancel a pending show too (docs/22 closeout audit): dbTipHide is what a grid rebuild
  // and the scroll-dismiss both run through, and a 260ms timer left alive fired dbTipShow
  // for a header that was no longer in the document — a ghost card pointing nowhere.
  clearTimeout(dbTip.timer);
  if (dbTip.node) { dbTip.node.remove(); dbTip.node = null; }
}

/** The header's hover card, in the panel's own chrome rather than the OS tooltip: name, the
 *  type/nullability line, the FULL comment (the header caption truncates; this never does) and
 *  the sort hint. Fixed under the th and clamped to the viewport; pointer-events none so it can
 *  never steal the hover it is explaining. */
function dbTipShow(th: HTMLElement, c: ApiDbColumn): void {
  const tip = el("div", "db-tip");
  tip.appendChild(el("div", "t-name", c.name));
  tip.appendChild(el("div", "t-type", c.dataType + (c.nullable ? " · " + tr("dataGrid.nullable") : " · " + tr("dataGrid.notNull")) +
    (c.isPrimaryKey ? " · " + tr("dataSql.primaryKey") : "")));
  if (c.comment) tip.appendChild(el("div", "t-comment", c.comment!));
  tip.appendChild(el("div", "t-hint", tr("dataGrid.clickSortsCycle")));
  document.body.appendChild(tip);
  const r = th.getBoundingClientRect();
  tip.style.left = Math.max(8, Math.min(r.left, window.innerWidth - tip.offsetWidth - 8)) + "px";
  tip.style.top = (r.bottom + 6) + "px";
  dbTip.node = tip;
}

// The card is fixed to the viewport, so a scrolled grid leaves it pointing nowhere — dismiss on
// any scroll, capture phase, once for the module's lifetime.
document.addEventListener("scroll", dbTipHide, true);


function renderDbGrid(): void {
  const c = dbConn();
  const t = dbTab();
  const wrap = $("dbGridWrap");
  if (!wrap) return;
  // docs/42 T2: the console is the sql tab's BODY, so it is on screen exactly when that tab
  // is the open object — no flag, no toggle, nothing to get out of sync with the strip.
  const con = $("dbConsole");
  if (con) con.hidden = t.kind !== "sql";
  // Full rebuild inside a scrolled pane. The wrap carries overflow-anchor:none (views.css):
  // Chrome's scroll anchoring otherwise re-picks its anchor when this wipe destroys the old
  // one and compensates by scrolling, which snaps the row the user just clicked to the top.
  wrap.textContent = "";
  dbTipHide(); // a rebuilt grid invalidates any header card still open
  renderDbStatus(); // docs/43 M4: the status line follows every body repaint

  // docs/22 W4.2: conflict marks live exactly as long as the buffered change they name —
  // reverting the cell, dropping the buffer or reloading the table clears them (a new
  // commit answer re-sets or clears them in the fetch observer above).
  if (t.kind === "table" && t.conflict) {
    const cu = t.updates && t.updates[t.conflict.key];
    const live = !!cu && t.conflict.columns.some((col) => {
      return Object.prototype.hasOwnProperty.call(cu.changes, col);
    });
    if (!live) t.conflict = null;
  }

  // One open object, one body (docs/42 T2): the kind picks the painter instead of a chain of
  // flags deciding which view is standing on top of which.
  if (t.kind === "sql") { renderDbResultGrid(wrap); return; }
  if (t.kind === "activity") { dbActivityRender(); return; }
  if (t.kind === "key") { dbRenderRedisValue(wrap); return; }
  const d = t;
  // docs/22 W5.1: the Form tab paints the same rows as the grid, one record at a time.
  if (d.pane === "form") { renderDbFormView(wrap); return; }
  if (d.pane !== "data") { renderDbDetailGrid(wrap); return; }
  if (!c.conn) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.databaseMcpRegisteredAdd"))); return; }
  if (!d.table || !d.data) {
    if (d.table) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.loading"))); return; }
    // The shared empty state (docs/18 V7); "Loading…" stays a quiet one-liner.
    wrap.appendChild(emptyNode({ icon: "database", title: tr("dataGrid.selectTable"), hint: tr("dataGrid.pickTableLeftBrowse") }));
    return;
  }
  if (d.loading) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.loading"))); return; }

  const pkCols = d.data.primaryKey || [];
  const editable = d.data.editable;
  // docs/22 W2.1: the per-connection grid config picks the columns the table shows and the
  // width each dragged column keeps. A config older than a column set (columns added since)
  // simply misses those names — an unknown width is no width, an unknown hidden name hides
  // nothing.
  const cfg = c.gridCfg || { widths: {}, hidden: [] };
  const cols = dbGridVisibleColumns(d.data?.columns, cfg.hidden);
  const widthOf = (c: ApiDbColumn, cell: HTMLElement): void => {
    const w = cfg.widths[c.name];
    if (w) { cell.style.width = w + "px"; cell.style.maxWidth = w + "px"; }
  };

  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  const keyOf = (row: Record<string, unknown>, i: number): string => { return pkCols.length ? dbPkKey(pkCols, row) : String(i); };
  const thAll = el("th", "db-rowctl");
  const allOn = d.data?.rows.length > 0 && d.data?.rows.every((row: Record<string, unknown>, i: number): boolean => { return !!d.sel[keyOf(row, i)]; });
  // The select-all box carries a data-selall address — #pane's delegated change listener
  // answers it (docs/37 R5), so no per-render handler and no re-attach on repaint.
  thAll.appendChild(h("input", {
    type: "checkbox", class: "db-selbox", checked: allOn,
    title: tr("dataGrid.selectEveryRowPage"), data: { selall: "" },
  }));
  hr.appendChild(thAll);
  // One caption line under every column name when ANY column carries a comment: the meaning is
  // then ON the grid instead of hidden behind a hover, and giving every header the line (empty
  // where there is nothing to say) keeps the header row one even height.
  const hasComments = cols.some((c: ApiDbColumn): boolean => { return !!c.comment; });
  cols.forEach((c: ApiDbColumn): void => {
    const sorted = c.name === d.order;
    // The header sorts through #pane's delegated click via its data-sort address (docs/37 R5);
    // onmouseenter/onmouseleave/oncontextmenu stay per-node — they are not click events and
    // the tip/menus they drive need the header element itself.
    const th = el("th", "db-col" + (sorted ? " db-sorted" + (d.dir === "desc" ? " db-sorted-desc" : "") : ""));
    th.setAttribute("data-sort", c.name);
    const main = el("div", "db-col-main");
    main.appendChild(el("span", "db-col-name", c.name));
    // fix-plan #14: the PK marker is the i-key sprite (the key-shaped unicode glyph it
    // retires was the last one here); the hover keeps saying "primary key" in words.
    if (c.isPrimaryKey) {
      const keyMark = el("span", "db-key");
      keyMark.title = tr("dataSql.primaryKey");
      keyMark.appendChild(iconNode("key"));
      main.appendChild(keyMark);
    }
    main.appendChild(el("span", "db-col-type", c.dataType));
    // docs/22 W5.2: the FK column's jump — one small straight arrow (the chevron belongs to
    // pagination) that opens the referenced table with the FOCUSED row's value as an eq
    // filter, the same channel a typed filter or W1.5's cell menu uses. The detail (and its
    // foreignKeys) loads with the table; until it answers there is no arrow, which is the
    // honest state — and a table with no FKs never draws one.
    const fk = ((d.detail && d.detail.foreignKeys) || []).find((f: ApiDbFkRow): boolean => { return f.column === c.name; });
    if (fk) {
      // The jump answers through #pane's delegated click via its data-fkjump address; the FK
      // row is re-resolved from live d.detail at event time (docs/37 §10.1).
      main.appendChild(h("button", {
        class: "db-col-fk", type: "button",
        title: tr("dataGrid.openRefFilteredColumns", { ref: (fk.refSchema ? fk.refSchema + "." : "") + fk.refTable }),
        aria: { label: tr("dataGrid.jumpReferencedRow") },
        data: { fkjump: c.name },
      }, iconNode("arrow-right")));
    }
    if (sorted) main.appendChild(el("span", "db-sort"));
    th.appendChild(main);
    if (hasComments) th.appendChild(el("div", "db-col-comment", c.comment || ""));
    // docs/22 W2.1: the resize grip hugs the header's right edge. It owns mousedown and click
    // so a drag neither sorts the column nor fights the hover card for the pointer.
    const grip = el("span", "db-col-grip");
    grip.title = tr("dataGrid.dragResize");
    grip.onmousedown = (ev: MouseEvent): void => {
      const idx = Array.prototype.indexOf.call(hr.children, th);
      const cells: HTMLElement[] = [th];
      const trs = tbl.querySelectorAll("tbody tr");
      for (let r = 0; r < trs.length; r++) {
        // The column's body cells are td elements; children[] only promises Element.
        const cell = trs[r].children[idx];
        if (cell) cells.push(cell as HTMLElement);
      }
      dbColResizeStart(ev, c.name, th, cells);
    };
    // No grip.onclick stopPropagation any more: #pane's delegated click consumes grip clicks
    // BEFORE the sort check (a grip click resizes, it must never sort).
    th.appendChild(grip);
    widthOf(c, th);
    th.onmouseenter = (): void => {
      clearTimeout(dbTip.timer);
      dbTip.timer = setTimeout((): void => { dbTipShow(th, c); }, 260);
    };
    th.onmouseleave = (): void => { clearTimeout(dbTip.timer); dbTipHide(); };
    // docs/22 W1.4: the header's own right-click runs this column's stats — top values or
    // COUNT/MIN/MAX/AVG. The statement is generated behind the identifier gate and lands in
    // the console (visible, in history) with its result in the standing result grid.
    th.oncontextmenu = (e: MouseEvent): void => {
      e.preventDefault();
      dbTipHide();
      // docs/22 W2.1: hiding lives in the same menu, one separator down, and the recovery
      // entry ("Show all columns") appears exactly while something is hidden — one place for
      // both directions. The items ride inline at the call (not through a local) so the
      // bare-literal gate keeps seeing every label.
      popupMenu((e.currentTarget as HTMLElement).getBoundingClientRect(), [
        { label: tr("dataGrid.valueDistribution"), fn: (): void => { dbRunColumnStats(c.name, "dist"); } },
        { label: tr("dataGrid.numericStats"), fn: (): void => { dbRunColumnStats(c.name, "num"); } },
        { sep: true },
        { label: tr("dataGrid.hideColumn"), fn: (): void => { dbHideColumn(c.name); } },
        ...(cfg.hidden.length
          ? [{ label: tr("dataGrid.showAllColumns"), fn: dbShowAllColumns }]
          : []),
      ]);
    };
    hr.appendChild(th);
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);

  const tbody = el("tbody");

  // docs/22 W2.1: a table with every column hidden still has to read as a place, not a
  // collapsed box — one quiet row says what happened and carries the way back. Rows and
  // buffered inserts are not drawn under it: with no visible column there is nothing to show
  // or edit. (The header keeps the select-all corner for shape.)
  if (!cols.length) {
    const trh = el("tr");
    const tdh = el("td", "db-grid-hiddenall");
    const inner = el("div");
    inner.appendChild(el("span", "", tr("dataGrid.everyColumnTHidden", { t: (d.schema ? d.schema + "." : "") + d.table })));
    inner.appendChild(h("button", { class: "btn", data: { colsall: "" } }, tr("dataGrid.showAllColumns")));
    tdh.appendChild(inner);
    trh.appendChild(tdh);
    tbody.appendChild(trh);
    tbl.appendChild(tbody);
    wrap.appendChild(tbl);
    return;
  }

  // Buffered inserts first, so they read as "the row you are about to add".
  d.inserts.forEach((ins: DbInsert, i: number): void => {
    const tri = el("tr", "db-ins");
    const rc = el("td", "db-rowctl");
    rc.appendChild(h("button", { class: "db-act", title: tr("dataGrid.removeBufferedInsert"), data: { irm: String(i) } }, iconNode("x")));
    tri.appendChild(rc);
    cols.forEach((c: ApiDbColumn, ci: number): void => {
      const has = Object.prototype.hasOwnProperty.call(ins.values, c.name);
      const td = el("td", "db-cell") as HTMLTableCellElement & { dbKbdEdit?: (seed: string | null) => void };
      // The cell's grid address: the focus ring moves between live cells by it (P0-B).
      td.setAttribute("data-r", String(i));
      td.setAttribute("data-c", String(ci));
      widthOf(c, td);
      const long = dbPaintCell(td, has ? ins.values[c.name] : undefined, has, c.dataType);
      // docs/22 W2.2: one click puts the keyboard's focus cell here; the ring marks it.
      // preventDefault (P0-A): the mousedown's default focus move lands on <body> AFTER
      // dbFocusCell focused #dbKbd, and arrows/Enter/F2/typing/Esc/Ctrl+C/paste would all
      // go nowhere. Click and dblclick still fire — preventing the default does not
      // suppress them.
      td.onmousedown = (ev: MouseEvent): void => { ev.preventDefault(); dbFocusCell(i, ci); };
      if (d.focus && d.focus.r === i && d.focus.c === ci) td.classList.add("db-focus");
      if (editable) {
        td.classList.add("db-cell-edit");
        td.title = long || tr("dataGrid.doubleClickEditMenuCopy");
        td.ondblclick = (): void => {
          const cur = has ? dbCellText(ins.values[c.name]) : "";
          dbEditCellEnter("insert", null, i, c.name, null, td, cur == null ? "" : cur);
        };
        td.dbKbdEdit = (seed: string | null): void => {
          const cur = seed != null ? seed : has ? dbCellText(ins.values[c.name]) : "";
          dbEditCellEnter("insert", null, i, c.name, null, td, cur == null ? "" : cur);
        };
        // docs/22 closeout B2: the editInDialog callback is dbCellMenu's FIFTH parameter —
        // a stray null before it parked the callback in an unread sixth slot, so the insert
        // row's menu never offered the dialog path a data row's menu always had.
        td.oncontextmenu = (e: MouseEvent): void => { dbCellMenu(e, null, null, c.name, (): void => { dbOpenCellEditor("insert", null, i, c.name, null); }); };
      }
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });

  d.data?.rows.forEach((row: Record<string, unknown>, rowIdx: number): void => {
    const key = keyOf(row, rowIdx);
    const deleted = !!d.deletes[key];
    const upd = d.updates[key];
    const tri = el("tr", (deleted ? "db-del " : "") + (d.sel[key] ? "db-sel" : ""));
    // The row's controls carry data-srow/data-rdel addresses instead of handlers (docs/37
    // R5): #pane's delegated change/click resolve the row and its delete state from live
    // state at event time — the shift-range and the undo/remove glyph both recompute, never capture.
    const rc = el("td", "db-rowctl");
    rc.appendChild(h("input", {
      type: "checkbox", class: "db-selbox", checked: !!d.sel[key],
      title: tr("dataGrid.selectRowCopyShift"), data: { srow: String(rowIdx) },
    }));
    if (editable) {
      rc.appendChild(h("button", {
        class: "db-act", title: deleted ? tr("dataGrid.undoBufferedDelete") : tr("dataGrid.bufferDeleteAppliedOnly"),
        data: { rdel: String(rowIdx) },
      }, iconNode(deleted ? "undo" : "x")));
    }
    tri.appendChild(rc);
    cols.forEach((c: ApiDbColumn, ci: number): void => {
      const orig = row[c.name];
      const pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name);
      const v = pending ? upd.changes[c.name] : orig;
      // docs/22 W4.2: the cells a lost commit race named stay red while their buffered
      // change is still pending — amber says "differs from the loaded row", red says
      // "the server refused this change; the row moved under it".
      const lost = pending && !!d.conflict && d.conflict.key === key && d.conflict.columns.includes(c.name);
      const td = el("td", "db-cell" + (pending ? " db-dirty" : "") + (lost ? " db-conflict" : "")) as HTMLTableCellElement & { dbKbdEdit?: (seed: string | null) => void };
      // docs/22 W2.2: same focus wiring as insert cells; the row index counts the inserts
      // above. The address rides the cell so the ring can move without a rebuild (P0-B).
      const gridRow = d.inserts.length + rowIdx;
      td.setAttribute("data-r", String(gridRow));
      td.setAttribute("data-c", String(ci));
      widthOf(c, td);
      const long = dbPaintCell(td, v, true, c.dataType);
      td.onmousedown = (ev: MouseEvent): void => { ev.preventDefault(); dbFocusCell(gridRow, ci); }; // P0-A: see the insert cells
      if (d.focus && d.focus.r === gridRow && d.focus.c === ci) td.classList.add("db-focus");
      if (editable && !deleted) {
        td.classList.add("db-cell-edit");
        td.title = long || tr("dataGrid.doubleClickEditMenuCopy");
        const meta: DbCellMeta = { pk: dbRowAddr(pkCols, d!.data!.columns, row), orig: orig };
        td.ondblclick = (): void => {
          const cur = dbCellText(orig);
          dbEditCellEnter("update", key, -1, c.name, meta, td, cur == null ? "" : cur);
        };
        td.dbKbdEdit = (seed: string | null): void => {
          const cur = seed != null ? seed : dbCellText(orig);
          dbEditCellEnter("update", key, -1, c.name, meta, td, cur == null ? "" : cur);
        };
        td.oncontextmenu = (e: MouseEvent): void => {
          dbCellMenu(e, row, key, c.name, (): void => { dbOpenCellEditor("update", key, -1, c.name, meta); });
        };
      }
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });

  tbl.appendChild(tbody);
  wrap.appendChild(tbl);

  // docs/22 W2.2: the zero-size input that carries the grid's keyboard focus. It sits inside
  // the scrolling wrap and owns paste directly; keydown (arrows/Enter/F2/Esc/Home/End/
  // typing, Ctrl+C) answers through #pane's delegated listener (dbGridKeydown, docs/37 R5)
  // with the visible-column set recomputed at event time. Every focus() call on it must pass
  // preventScroll: it lives at the end of the scrolled content, and a plain focus() drags the
  // pane to the bottom.
  const kbd = el("input", "db-kbd") as HTMLInputElement;
  kbd.type = "text";
  kbd.id = "dbKbd";
  kbd.tabIndex = -1;
  kbd.setAttribute("aria-label", tr("dataGrid.kbdNav"));
  kbd.onpaste = (ev: ClipboardEvent): void => {
    const t2 = dbTab();
    if (t2.kind !== "table" || !t2.data) return;
    const text = ev.clipboardData ? ev.clipboardData.getData("text/plain") : "";
    if (!text) return;
    ev.preventDefault();
    dbPasteApply(text, t2.focus ? t2.focus.r : 0, t2.focus ? t2.focus.c : 0);
  };
  wrap.appendChild(kbd);
}

function renderDbResultGrid(wrap: HTMLElement): void {
  const d = dbTab();
  if (d.kind !== "sql") return; // the reply grid is the console tab's body (docs/42 T2)
  if (d.sqlBusy) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.running"))); return; }
  const res = d.sqlResult;
  if (!res) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.runQuerySeeRows"))); return; }
  const tab = d.resultTab || 0; // docs/22 W4.3: this grid is one tab of the strip — its selection keys are that tab's
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  const thAll2 = el("th", "db-rowctl");
  const allOn2 = res.rows.length > 0 && res.rows.every((_: Record<string, unknown>, i: number): boolean => { return !!d.sel[dbResultKey(tab, i)]; });
  thAll2.appendChild(h("input", {
    type: "checkbox", class: "db-selbox", checked: allOn2,
    title: tr("dataGrid.selectEveryResultRow"), data: { selall: "" },
  }));
  hr.appendChild(thAll2);
  res.columns.forEach((c: string): void => { hr.appendChild(el("th", "db-col", c)); });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  res.rows.forEach((row: Record<string, unknown>, i: number): void => {
    const qkey = dbResultKey(tab, i);
    const tri = el("tr", d.sel[qkey] ? "db-sel" : "");
    const rc2 = el("td", "db-rowctl");
    // data-qrow addresses the row for #pane's delegated change (docs/37 R5); the shift-range
    // and selection keys recompute from live state at event time.
    rc2.appendChild(h("input", {
      type: "checkbox", class: "db-selbox", checked: !!d.sel[qkey],
      title: tr("dataGrid.selectRowCopyShift"), data: { qrow: String(i) },
    }));
    rc2.appendChild(document.createTextNode(String(i + 1)));
    tri.appendChild(rc2);
    res?.columns.forEach((c: string): void => {
      const td = el("td", "db-cell");
      td.oncontextmenu = (e: MouseEvent): void => { dbResultCellMenu(e, row, c); };
      // No column types on a result grid: dbCellView still words booleans, folds long JSON,
      // links URLs and right-aligns JS numbers off the value alone.
      const v = row[c];
      dbPaintCell(td, v === undefined ? null : v, true);
      tri.appendChild(td);
    });
    tbody.appendChild(tri);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** The three-state cycle a header click walks: a fresh column sorts ascending, the next click
    flips to descending, and a third click clears the order (null) back to the table's natural
    order. Pure so the cycle can be tested without a DOM. */
function dbNextSort(order: string | null, dir: string | null, name: string): { order: string; dir: string } | null {
  if (order !== name) return { order: name, dir: "asc" };
  if (dir === "desc") return null;
  return { order: name, dir: "desc" };
}

/* --- #pane's delegated listeners for the toolbar and the grids (docs/37 R5) -----------------------
   Behavior notes (docs/37 §10.1), all from state resolved at EVENT time rather than render
   time: the pager and refresh re-read dbOkToDrop() when the click lands (a buffer that grew
   between render and click is still counted); the delete/undo button re-derives its key from
   the live page, so its glyph and its action can never disagree; the result-tab switch
   re-reads d.sqlResults instead of a captured array. */

/** The selection/edit key of the rowIdx'th row of the LIVE page — the render-time keyOf,
 *  lifted out for the delegated listeners. Null when the address no longer names a row. */
function dbRowKeyAt(rowIdx: number): string | null {
  const t = dbTab();
  if (t.kind !== "table") return null;
  const data = t.data;
  if (!data || !data.rows[rowIdx]) return null;
  const pk = data.primaryKey || [];
  return pk.length ? dbPkKey(pk, data.rows[rowIdx]) : String(rowIdx);
}

function dbToolbarClick(t: Element, ev: MouseEvent): boolean {
  const d = dbTab();
  // docs/43 M4: the activity tab's primary action — one guarded refresh of the live view.
  if (t.closest("[data-actrefresh]")) { void dbActivityLoad(); return true; }
  const tb = t.closest<HTMLElement>("[data-tb]");
  if (tb) {
    const which = tb.dataset.tb;
    // No "back" button any more (docs/42 T2): the reply lives in the console's own tab, so
    // the way back to the table is the table's tab — already on the strip, one click away.
    if (which === "refresh") {
      if (!dbOkToDrop()) return true;
      dbDropEdits();
      void dbLoadData(true);
      return true;
    }
    if (d.kind !== "table") {
      // The row-buffer actions below belong to a table tab; the console opener does not.
      if (which === "sql") { dbOpenTab({ kind: "sql" }); return true; }
      return false;
    }
    if (which === "addrow") {
      d.inserts.push({ values: {} });
      renderDbGrid(); renderDbBar();
      return true;
    }
    if (which === "csv") { dbExportCsv(); return true; }
    if (which === "export") {
      // stopPropagation FIRST: connect.js closes any open menu on clicks that reach document,
      // and without this the very click that opens the menu also tears it down.
      ev.stopPropagation();
      popupMenu(tb.getBoundingClientRect(), [
        { label: tr("dataGrid.exportCsv"), fn: (): void => { void dbExportTable(tb as HTMLButtonElement, "csv"); } },
        { label: tr("dataGrid.exportNdjson"), fn: (): void => { void dbExportTable(tb as HTMLButtonElement, "json"); } },
        { label: tr("dataGrid.exportSqlDump"), fn: (): void => { void dbExportTable(tb as HTMLButtonElement, "sql"); } },
      ]);
      return true;
    }
    if (which === "import") { dbOpenImport(); return true; }
    if (which === "sql") { dbOpenTab({ kind: "sql" }); return true; }
  }
  const rtab = t.closest<HTMLElement>("[data-rtab]");
  if (rtab && d.kind === "sql") {
    const ti = Number(rtab.dataset.rtab);
    if (d.resultTab === ti) return true;
    const all = d.sqlResults || [];
    if (!all[ti]) return true; // a stale strip's address — the run that named it is gone
    d.resultTab = ti;
    d.sqlResult = all[ti];
    d.selAnchor = -1; // a Shift-range never spans tabs
    renderDbToolbar();
    renderDbGrid();
    return true;
  }
  const pg = t.closest<HTMLElement>("[data-pg]");
  if (pg && d.kind === "table") {
    if (!dbOkToDrop()) return true;
    if (pg.dataset.pg === "prev") d.offset = Math.max(0, d.offset - d.pageSize);
    else d.offset += d.pageSize;
    dbDropEdits();
    void dbLoadData(true);
    return true;
  }
  const irm = t.closest<HTMLElement>("[data-irm]");
  if (irm && d.kind === "table") {
    d.inserts.splice(Number(irm.dataset.irm), 1);
    renderDbGrid(); renderDbBar();
    return true;
  }
  const rdel = t.closest<HTMLElement>("[data-rdel]");
  if (rdel && d.kind === "table") {
    const rowIdx = Number(rdel.dataset.rdel);
    const data = d.data;
    const key = dbRowKeyAt(rowIdx);
    if (!data || key == null) return true;
    if (d.deletes[key]) delete d.deletes[key];
    else {
      const pkCols = data.primaryKey || [];
      d.deletes[key] = dbRowAddr(pkCols, data.columns, data.rows[rowIdx]);
      delete d.updates[key]; // a deleted row's cell edits are moot
    }
    renderDbGrid(); renderDbBar();
    return true;
  }
  return false;
}

function dbGridClick(t: Element): boolean {
  const d = dbTab();
  if (d.kind !== "table") return false; // the grid's own affordances belong to a table tab
  // Order is the stopPropagation the per-node handlers used to do: the grip and the FK jump
  // live INSIDE a sort header, and their clicks must never fall through to the sort.
  if (t.closest(".db-col-grip")) return true; // the grip resizes (mousedown owns it)
  const fkj = t.closest<HTMLElement>("[data-fkjump]");
  if (fkj) {
    const col = fkj.dataset.fkjump ?? "";
    const fk = ((d.detail && d.detail.foreignKeys) || []).find((f: ApiDbFkRow): boolean => { return f.column === col; });
    if (!fk) return true; // the detail changed under the click — nothing to jump through
    const v = dbFocusedColumnValue(d, col);
    if (v === undefined) {
      toast(tr("dataGrid.clickCellFirst", { col }), true);
      return true;
    }
    if (v === null) {
      toast(tr("dataGrid.focusedColNull", { col }), true);
      return true;
    }
    dbFkOpen(fk, v);
    return true;
  }
  if (t.closest("[data-colsall]")) { dbShowAllColumns(); return true; }
  const th = t.closest<HTMLElement>("th[data-sort]");
  if (th) {
    dbTipHide();
    if (!dbOkToDrop()) return true;
    const next = dbNextSort(d.order, d.dir, th.dataset.sort ?? "");
    d.order = next ? next.order : null;
    d.dir = next ? next.dir : "asc";
    d.offset = 0;
    dbDropEdits();
    void dbLoadData(true);
    return true;
  }
  return false;
}

function dbGridChange(t: Element, ev: Event): boolean {
  const d = dbTab();
  if (t.closest("[data-selall]")) {
    dbSelAll((t as HTMLInputElement).checked);
    return true;
  }
  const srow = t.closest<HTMLInputElement>("[data-srow]");
  if (srow && d.kind === "table") {
    const rowIdx = Number(srow.dataset.srow);
    const key = dbRowKeyAt(rowIdx);
    if (key != null) {
      const mse = ev as Event & { shiftKey?: boolean };
      if (mse.shiftKey && d.selAnchor >= 0 && d.selAnchor !== rowIdx) {
        const a = Math.min(d.selAnchor, rowIdx);
        const b2 = Math.max(d.selAnchor, rowIdx);
        const pk = d.data!.primaryKey || [];
        for (let k = a; k <= b2; k++) d.sel[pk.length ? dbPkKey(pk, d.data!.rows[k]) : String(k)] = true;
      } else if (srow.checked) d.sel[key] = true;
      else delete d.sel[key];
    }
    d.selAnchor = rowIdx;
    renderDbToolbar(); renderDbGrid();
    return true;
  }
  const qrow = t.closest<HTMLInputElement>("[data-qrow]");
  if (qrow && d.kind === "sql") {
    const tab = d.resultTab || 0; // docs/22 W4.3: selection keys are the ACTIVE tab's
    const i = Number(qrow.dataset.qrow);
    const mse = ev as Event & { shiftKey?: boolean };
    if (mse.shiftKey && d.selAnchor >= 0 && d.selAnchor !== i) {
      const a2 = Math.min(d.selAnchor, i);
      const b3 = Math.max(d.selAnchor, i);
      for (let k2 = a2; k2 <= b3; k2++) d.sel[dbResultKey(tab, k2)] = true;
    } else if (qrow.checked) d.sel[dbResultKey(tab, i)] = true;
    else delete d.sel[dbResultKey(tab, i)];
    d.selAnchor = i;
    renderDbToolbar(); renderDbGrid();
    return true;
  }
  const size = t.closest<HTMLSelectElement>('[data-tb="pagesize"]');
  if (size && d.kind === "table") {
    if (!dbOkToDrop()) { size.value = String(d.pageSize); return true; }
    d.pageSize = Number(size.value);
    d.offset = 0;
    dbDropEdits();
    void dbLoadData(true);
    return true;
  }
  return false;
}

/** The grid's keyboard layer (#dbKbd) — arrows/Enter/F2/Esc/Ctrl+C/typing, on the live
 *  column set. Delegated from #pane (docs/37 R5): focus stays in the zero-size input and
 *  the key event bubbles up to the pane. */
function dbGridKeydown(t: Element, ev: KeyboardEvent): boolean {
  if (!t.closest("#dbKbd")) return false;
  const d = dbTab();
  if (d.kind !== "table" || !d.data) return true;
  const cfg = dbConn().gridCfg || { widths: {}, hidden: [] };
  const cols = dbGridVisibleColumns(d.data.columns, cfg.hidden);
  const maxR = dbGridRowsCount() - 1;
  const maxC = cols.length - 1;
  if (!d.focus) {
    if (ev.key === "ArrowUp" || ev.key === "ArrowDown" || ev.key === "ArrowLeft" ||
      ev.key === "ArrowRight" || ev.key === "Home" || ev.key === "End") {
      ev.preventDefault();
      dbFocusCell(0, 0);
    }
    return true;
  }
  if (ev.key === "ArrowUp" || ev.key === "ArrowDown" || ev.key === "ArrowLeft" ||
    ev.key === "ArrowRight" || ev.key === "Home" || ev.key === "End") {
    ev.preventDefault();
    const m = dbKbdMove(d.focus.r, d.focus.c, ev.key, maxR, maxC);
    dbFocusCell(m.r, m.c); // moves the ring on the live grid — no rebuild mid-navigation
    return true;
  }
  if (ev.key === "Enter" || ev.key === "F2") {
    ev.preventDefault();
    dbFocusEdit();
    return true;
  }
  if (ev.key === "Escape") {
    // Esc's one meaning here is "put the keyboard down" — the cell keeps its value and its
    // ring goes away. (The inline editor and the sheet handle their own Esc first.) The
    // ring leaves by class move, not a rebuild (P0-B); blur leaves the keyboard exactly
    // where the rebuild used to drop it.
    ev.preventDefault();
    d.focus = null;
    const wrapEsc = $("dbGridWrap");
    const ring = wrapEsc ? wrapEsc.querySelector("td.db-focus") : null;
    if (ring) ring.classList.remove("db-focus");
    const kb = $("dbKbd");
    if (kb && kb.blur) kb.blur();
    return true;
  }
  if ((ev.ctrlKey || ev.metaKey) && (ev.key === "c" || ev.key === "C")) {
    dbCopyChecked();
    return true;
  }
  if (!ev.ctrlKey && !ev.metaKey && !ev.altKey && ev.key.length === 1) {
    // Typing into a cell starts the edit seeded with the character, exactly the dblclick
    // editor with a head start.
    ev.preventDefault();
    dbFocusEdit(ev.key);
  }
  return true;
}

export { DB_COL_MAX, DB_COL_MIN, dbColResizeStart, dbCopyChecked, dbFocusCell, dbGridChange, dbGridClick, dbGridConfigKey, dbGridConfigLoad, dbGridConfigParse, dbGridConfigSave, dbGridKeydown, dbGridVisibleColumns, dbHideColumn, dbKbdMove, dbLoadData, dbNextSort, dbPaintCell, dbPasteApply, dbRestoreData, dbRowAddr, dbToolbarClick, renderDbGrid, renderDbResultGrid, renderDbStatus, renderDbToolbar, dbShowAllColumns, dbTsvRows };
