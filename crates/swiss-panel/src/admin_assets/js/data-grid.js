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

                                                                                                               
                                               
                                                                                         
import { $, apiJson, dbReqGuard, el, emptyHtml, errText, icon, state, toast } from "./util.js";
import { dbIsRedis, dbRenderRedisValue } from "./data-browsers.js";
import { dbCellMenu, dbCopyCsvCell, dbCopyText, dbExportCsv, dbOpenImport, dbResultCellMenu, dbRowForCopy, dbSelAll, dbSelectedForCopy } from "./data-csv.js";
import { dbOpenCellEditor, dbCellText, dbCellView } from "./data-cell.js";
import { dbEditCellEnter } from "./data-edit.js";
import { dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbFillConsole, dbRunSql, dbStatsSql, renderDbBar } from "./data-sql.js";
import { dbRenderTabs, renderDbDetailGrid } from "./data-structure.js";
import { renderDbFormView } from "./data-form.js";
import { DB_PAGE_SIZES, dbClearSel, dbDropEdits, dbFkOpen, dbFocusedColumnValue, dbOkToDrop, dbPkKey, dbPkVals, dbResultKey } from "./data-view.js";
import { popupMenu } from "./menu.js";
import { act } from "./detail.js";

/* --- one page of rows --------------------------------------------------------------------------- */

/* --- grid config: per-connection column widths and hidden columns (docs/22 W2.1) ---------------- */
/* dbgate keeps one GridConfig object per grid (GridConfig.ts + useGridConfig.ts); the equivalent
   here is ONE localStorage object per connection+table holding the widths the user dragged and
   the columns they hid. The helpers are pure (or storage-only) so the key format, the parse's
   tolerance of a corrupt entry and the clamp pin without a DOM. The grid reloads the config on
   every page load — a rename or a second tab's change is picked up, never cached stale. */
const DB_GRID_PREFIX = "mcp_gateway_db_grid_";
const DB_COL_MIN = 48;  // narrower than the header's own name line and nothing reads
const DB_COL_MAX = 1200; // wider than the grid itself — a runaway drag helps nobody

function dbGridConfigKey(conn        , schema        , table        )         {
  return DB_GRID_PREFIX + conn + "_" + (schema ? schema + "." : "") + table;
}

/** Parse one stored config. A missing, corrupt or wrongly-typed entry is a fresh config:
 *  storage is the only writer and quota errors can leave anything behind, but a broken grid
 *  is never an acceptable consequence. Widths outside the clamp are dropped, not clamped —
 *  a hand-edited 0 was never a width the user dragged. */
function dbGridConfigParse(raw               )               {
  const out               = { widths: {}, hidden: [] };
  if (!raw) return out;
  try {
    const j = JSON.parse(raw);
    if (!j || typeof j !== "object") return out;
    if (j.widths && typeof j.widths === "object") {
      Object.keys(j.widths).forEach((k        )       => {
        const n = Number(j.widths[k]);
        if (isFinite(n) && n >= DB_COL_MIN && n <= DB_COL_MAX) out.widths[k] = Math.round(n);
      });
    }
    if (Array.isArray(j.hidden)) {
      out.hidden = j.hidden.filter((h         ) => { return typeof h === "string" && h; })            ;
    }
  } catch (e) { /* fresh config, see above */ }
  return out;
}

function dbGridConfigLoad(key        )               {
  let raw                = null;
  try { raw = localStorage.getItem(key); } catch (e) { /* private mode: defaults */ }
  return dbGridConfigParse(raw);
}

function dbGridConfigSave(key        , cfg              )       {
  try { localStorage.setItem(key, JSON.stringify(cfg)); } catch (e) { /* quota/private mode: this session only */ }
}

/** The columns the grid shows after the config's hidden list. Pure. */
function dbGridVisibleColumns(columns               , hidden                      )                {
  if (!hidden || !hidden.length) return columns.slice();
  const drop                          = {};
  hidden.forEach((h        )       => { drop[h] = true; });
  return columns.filter((c             )          => { return !drop[c.name]; });
}

/** The config's key for the table currently open (null when nothing is open). */
function dbGridKeyNow()                {
  const d = state.db;
  const d_ = d ;
  return d_.conn && d_.table ? dbGridConfigKey(d_.conn, d_.schema , d_.table) : null;
}

/** Hide one column (the header menu) / bring every hidden column back, writing through to
 *  storage so the choice survives a reload. */
function dbHideColumn(name        )       {
  const d = state.db;
  const d_ = d ;
  if (d_.gridCfg.hidden.indexOf(name) < 0) d_.gridCfg.hidden.push(name);
  const key = dbGridKeyNow();
  if (key) dbGridConfigSave(key, d_.gridCfg);
  renderDbGrid();
}

function dbShowAllColumns()       {
  const d = state.db;
  const d_ = d ;
  d_.gridCfg.hidden = [];
  const key = dbGridKeyNow();
  if (key) dbGridConfigSave(key, d_.gridCfg);
  renderDbGrid();
}

/** Drag the column's right edge: width follows the pointer live across every cell of the
 *  column (cells captured at drag start — no repaint per mousemove), and the config is
 *  written once on release. The handle owns mousedown/click (stopPropagation) so a drag never
 *  sorts the column it is resizing. Each cell gets width AND maxWidth — under
 *  table-layout: auto a width alone is only a hint the nowrap content outvotes. */
function dbColResizeStart(e            , name        , th             , cells                         )       {
  e.preventDefault();
  e.stopPropagation();
  const d = state.db;
  const startX = e.clientX;
  const startW = th.getBoundingClientRect().width;
  let lastW = startW;
  function move(ev            )       {
    lastW = Math.max(DB_COL_MIN, Math.min(DB_COL_MAX, Math.round(startW + ev.clientX - startX)));
    for (let i = 0; i < cells.length; i++) {
      cells[i].style.width = lastW + "px";
      cells[i].style.maxWidth = lastW + "px";
    }
  }
  function up()       {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
    const d_ = d ;
    if (lastW !== Math.round(startW)) {
      d_.gridCfg.widths[name] = lastW;
      const key = dbGridKeyNow();
      if (key) dbGridConfigSave(key, d_.gridCfg);
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
function dbTsvRows(text        )             {
  const t = String(text == null ? "" : text).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (!t) return [];
  const rows = t.split("\n");
  if (rows.length > 1 && rows[rows.length - 1] === "") rows.pop();
  return rows.map((r        )           => { return r.split("\t"); });
}

/** Move the focus cell one key at a time, clamped to the grid's bounds. Pure. */
function dbKbdMove(r        , c        , key        , maxR        , maxC        )                           {
  if (key === "ArrowUp") return { r: Math.max(0, r - 1), c: c };
  if (key === "ArrowDown") return { r: Math.min(maxR, r + 1), c: c };
  if (key === "ArrowLeft") return { r: r, c: Math.max(0, c - 1) };
  if (key === "ArrowRight") return { r: r, c: Math.min(maxC, c + 1) };
  if (key === "Home") return { r: r, c: 0 };
  if (key === "End") return { r: r, c: maxC };
  return { r: r, c: c };
}

/** The grid row count the keyboard walks: buffered inserts first, then the page's rows. */
function dbGridRowsCount()         {
  const d = state.db;
  const d_ = d ;
  return (d_.data ? d_.inserts.length + d_.data.rows.length : 0);
}

/** Focus (or move) the focus cell and paint the ring — a single selection the keyboard owns.
 * Moving the ring must NOT rebuild the grid: the rebuild detached the cell mid-click, the
 * browser then never fired the click/dblclick that followed the mousedown, and double-click
 * editing could not open at all (docs/22 closeout audit P0-B). The ring moves between the
 * LIVE cells by their data-r/data-c address; a full repaint happens only on data changes
 * (edit, paste, commit, paging). */
function dbFocusCell(r        , c        )       {
  const d = state.db;
  const maxR = dbGridRowsCount() - 1;
  const d_ = d ;
  const maxC = dbGridVisibleColumns(d_.data ? d_.data.columns : [], (d_.gridCfg || { hidden: [] }).hidden).length - 1;
  d_.focus = { r: Math.max(0, Math.min(maxR, r)), c: Math.max(0, Math.min(maxC, c)) };
  const wrap = $("dbGridWrap");
  if (!wrap) return;
  const old = wrap.querySelector("td.db-focus");
  if (old) old.classList.remove("db-focus");
  const td = wrap.querySelector('td[data-r="' + d_.focus.r + '"][data-c="' + d_.focus.c + '"]');
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
function dbFocusEdit(seed                )       {
  const td = document.querySelector("#dbGridWrap td.db-focus")                                                               ;
  if (!td || !td.dbKbdEdit) return;
  td.dbKbdEdit(seed == null ? null : String(seed));
}

/** Write one pasted cell into the buffers with the same semantics a typed edit has:
 *  an insert cell reverts to the column default on empty, an update collapses back when the
 *  pasted text equals the original. */
function dbPasteCell(kind        , key        , i        , column        , meta            , raw        )       {
  const d = state.db;
  const d_ = d ;
  if (kind === "insert") {
    const ins = d_.inserts[i] ;   // a paste only targets a row that still exists
    if (raw === "") delete ins.values[column];
    else ins.values[column] = raw;
    return;
  }
  const row = d_.data?.rows[i];
  // meta.pk is already the pk VALUES map the caller built (same object dbSaveInlineEdit
  // stores); running it through dbPkVals again would forEach over an object and throw.
  const upd = d_.updates[key] || (d_.updates[key] = { pk: meta.pk                           , changes: {} });
  const origTxt = meta.orig == null ? "" : String(meta.orig);
  if (raw === origTxt) {
    delete upd.changes[column];
    if (!Object.keys(upd.changes).length) delete d_.updates[key];
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
function dbRowAddr(pkCols          , columns               , row                         )                          {
  const out                          = {};
  columns.forEach((c             )       => { out[c.name] = row[c.name]; });
  return out;
}

/** Structural equality for two JSON values — the 409 body's row map against a buffered
 *  entry's pk, so the conflict lands on the exact buffered row (docs/22 W4.2). */
function dbSameJson(a                                , b                                )          {
  if (a === b) return true;
  if (a == null || b == null || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a), kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k        )          => { return dbSameJson(a[k]                                  , b[k]                                  ); });
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
  window.fetch = function (                                  input             , init              )                    {
    return orig.apply(this, arguments                                                     ).then((r          )           => {
      try {
        const path = typeof input === "string" ? input : (input && input.url) || "";
        if (/\/edits(\?|$)/.test(path)) {
          let d = state.db;
          if (r.status === 409) {
            r.clone().json().then((j                                                               )       => {
              d = state.db;
              const cols = (j && j.conflictColumns) || [];
              if (!d || !d.updates || !cols.length || !j || !j.row) return;
              const hit = Object.keys(d?.updates).filter((k) => {
                return dbSameJson(d .updates[k].pk, j.row );
              })[0];
              if (hit) { d .conflict = { key: hit, columns: cols }; renderDbGrid(); }
            }).catch(() => { /* an observation never breaks the panel */ });
          } else if (d && d?.conflict) {
            d .conflict = null; // a clean answer (or a different failure) clears the marks
            renderDbGrid();
          }
        }
      } catch (e) { /* never */ }
      return r;
    });
  }                ;
})();

function dbPasteApply(text        , r0        , c0        )       {
  const d = state.db;
  const d_ = d ;
  if (!d_.data || !d_.data.editable) {
    toast("This table is not editable (" + (d_.data && d_.data.editNote ? d_.data.editNote : "no primary key") + ") — paste needs the edit buffer", true);
    return;
  }
  const cfg = d_.gridCfg || { widths: {}, hidden: [] };
  const cols = dbGridVisibleColumns(d_.data?.columns, cfg.hidden);
  const rows = dbTsvRows(text);
  if (!rows.length || !cols.length) return;
  const pkCols = d_.data?.primaryKey || [];
  const keyOf = (row                         , i        )         => { return pkCols.length ? dbPkKey(pkCols, row) : String(i); };
  for (let i = 0; i < rows.length; i++) {
    const target = r0 + i;
    for (let j = 0; j < rows[i].length; j++) {
      const colIdx = c0 + j;
      if (colIdx >= cols.length) break;
      const col = cols[colIdx].name;
      const raw = rows[i][j];
      const insertsBefore = d_.inserts.length;
      if (target < insertsBefore) {
        dbPasteCell("insert", null                     , target, col, null                         , raw);
      } else {
        const ri = target - d_.inserts.length;
        if (ri < d_.data.rows.length) {
          const row = d_.data?.rows[ri];
          const key = keyOf(row, ri);
          dbPasteCell("update", key, ri, col, { pk: dbRowAddr(pkCols, d_.data?.columns, row), orig: row[col] }, raw);
        } else {
          // Past the page: the paste row becomes a NEW buffered insert, values in column order.
          d_.inserts.push({ values: {} });
          dbPasteCell("insert", null                     , d_.inserts.length - 1, col, null                         , raw);
        }
      }
    }
  }
  renderDbGrid();
  renderDbBar();
}

/** Ctrl+C: the checked rows as CSV (the row menu's Copy), or — with nothing checked — the
 *  focused row alone, which is what a grid hand expects Ctrl+C to grab. */
function dbCopyChecked()       {
  const d = state.db;
  const sel = dbSelectedForCopy();
  if (sel && sel.rows && sel.rows.length) {
    // Same shape the row menu's Copy-as-CSV builds (data-csv keeps dbRowsCsv private): a
    // header line plus one quoted line per checked row.
    const lines = [sel.cols.map(dbCopyCsvCell).join(",")];
    sel.rows.forEach((r                         )       => { lines.push(sel.cols.map((cn        )         => { return dbCopyCsvCell(r[cn]); }).join(",")); });
    dbCopyText(lines.join("\n"));
    return;
  }
  const d_ = d ;
  if (!d_.focus || !d_.data) return;
  const nIns = d_.inserts.length;
  if (d_.focus.r < nIns) return; // a buffered insert has no committed row to copy
  const row = d_.data?.rows[d_.focus.r - nIns];
  const pkCols = d_.data?.primaryKey || [];
  const key = pkCols.length ? dbPkKey(pkCols, row) : String(d_.focus.r - nIns);
  const full = dbRowForCopy(row, key);
  const line = d_.data?.columns.map((c             )         => {
    return dbCopyCsvCell(full == null ? null : full[c.name]);
  }).join(",");
  dbCopyText(line);
}

// One /data request chain: a slow answer for the table (or page) the user just left must be
// dropped, or it would repaint the grid — and rewrite d.schema — from the OLD table (docs/22
// closeout audit).
const dbDataReq = dbReqGuard();

async function dbLoadData(keepOffset          )                {
  const d = state.db;
  const d_ = d ;
  if (!d_.conn || !d_.table) return;
  if (!keepOffset) d_.offset = 0;
  d_.loading = true;
  renderDbToolbar(); renderDbGrid();
  let q = "/api/db/" + encodeURIComponent(d_.conn) + "/data?table=" + encodeURIComponent(d_.table) +
    "&offset=" + d_.offset + "&limit=" + d_.pageSize;
  if (d_.schema) q += "&schema=" + encodeURIComponent(d_.schema);
  if (d_.order) q += "&order=" + encodeURIComponent(d_.order) + "&dir=" + d_.dir;
  if (d_.filters.length) q += "&filters=" + encodeURIComponent(JSON.stringify(d_.filters));
  const token = dbDataReq.issue();
  const j = await apiJson               (q);
  if (!dbDataReq.accepts(token)) return; // superseded: a newer load owns the pane and the flag
  d_.loading = false;
  if (!j) { renderDbToolbar(); renderDbGrid(); return; }
  // A Commit that emptied the last page (or a filter that shrank the set) can leave this
  // offset past the end of what remains: an empty page with rows behind it is one page
  // back — re-fetch there instead of painting an empty grid whose footer reads "51–50 of
  // 50" (docs/22 closeout audit).
  if (!j.rows.length && d_.offset > 0) {
    d_.offset = Math.max(0, d_.offset - d_.pageSize);
    void dbLoadData(true);
    return;
  }
  d_.data = j;
  d_.schema = j.schema;
  // The grid config reloads with every page: a rename (new key) or a second tab's hide lands
  // on the next paint instead of a cached opinion (docs/22 W2.1).
  d_.gridCfg = dbGridConfigLoad(dbGridKeyNow() );
  dbDropEdits(); // a fresh page is a fresh baseline — buffered edits never survive a reload
  // A filter naming a column that no longer exists would 400 on every reload — drop it instead.
  const names = d_.data?.columns.map((c             )         => { return c.name; });
  d_.filters = d_.filters.filter((f              )          => { return names.indexOf(f.column) >= 0; });
  renderDbToolbar(); renderDbGrid(); renderDbBar(); renderDbFilters();
}

/* Whole-table export in the chosen format (CSV, NDJSON, or SQL dump). The toolbar button carries the
   busy state: the format menu is gone by the time the download starts, so the button is the
   only place left on screen that can say "working". */
async function dbExportTable(btn                   , fmt        )                {
  const d2 = state.db;
  const name = fmt === "json" ? "NDJSON" : fmt === "sql" ? "SQL dump" : "CSV";
  const d2_ = d2 ;
  if (!confirm("Export " + (d2_.schema ? d2_.schema + "." : "") + d2_.table + " as " + name +
      "?\nCapped at 100,000 rows — filter first if you need less.")) return;
  let q = "/api/db/" + encodeURIComponent(d2_.conn ) + "/export?table=" + encodeURIComponent(d2_.table ) +
    "&format=" + fmt;
  if (d2_.schema) q += "&schema=" + encodeURIComponent(d2_.schema);
  // The grid's filters ride along: the download and the grid describe the same filtered
  // set, and the exported row count is the filtered total.
  if (d2_.filters.length) q += "&filters=" + encodeURIComponent(JSON.stringify(d2_.filters));
  btn.disabled = true;
  btn.textContent = "Exporting\u2026";
  try {
    const resp = await fetch(q);
    if (!resp.ok) { toast("Export failed: HTTP " + resp.status, true); return; }
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
    toast("Exported " + (rows != null ? Number(rows).toLocaleString() + " rows" : ""));
  } catch (e) {
    toast("Export failed", true);
  } finally {
    btn.disabled = false;
    btn.textContent = "Export\u2026";
  }
}

function renderDbToolbar()       {
  const d = state.db;
  const head = $("dbHead");
  if (!head) return;
  head.innerHTML = "";
  const left = el("div", "db-head-left");
  const d_ = d ;
  if (d_.sqlResult) {
    left.appendChild(el("h2", "db-title pane-title", d_.sqlResult.explained ? "Execution plan" : (dbIsRedis() ? "Command reply" : "SQL results")));
    left.appendChild(el("div", "db-meta", d_.sqlResult.rowCount + " row" + (d_.sqlResult.rowCount === 1 ? "" : "s") +
      (d_.sqlResult.note ? " · " + d_.sqlResult.note : "") +
      (d_.sqlResult.elapsedMs != null ? " · " + d_.sqlResult.elapsedMs + " ms" : "")));
    // docs/22 W4.3: one tab per statement reply, in the pane's segmented-control vocabulary
    // (.db-tabs — the same strip the Structure tabs use). Eight fit the pane; past that the
    // strip scrolls sideways instead of wrapping. Switching only repoints the active result;
    // every tab keeps its own checked-row keys (dbResultKey namespaces d.sel by tab), so a
    // Shift-range or a copy never crosses tabs.
    if ((d_.sqlResults || []).length > 1) {
      const seg = el("div", "db-tabs");
      seg.setAttribute("role", "tablist");
      d_.sqlResults?.forEach((r              , ti        )       => {
        const b = el("button", "", r.tabLabel || "Result " + (ti + 1));
        b.setAttribute("role", "tab");
        b.setAttribute("aria-selected", ti === d?.sqlTab ? "true" : "false");
        b.onclick = () => {
          const d_ = d ;
          if (d_.sqlTab === ti) return;
          d_.sqlTab = ti;
          d_.sqlResult = d_.sqlResults [ti];
          d_.selAnchor = -1; // a Shift-range never spans tabs
          renderDbToolbar();
          renderDbGrid();
        };
        seg.appendChild(b);
      });
      left.appendChild(seg);
    }
  } else if (d_.data) {
    left.appendChild(el("h2", "db-title pane-title", (d_.data.schema ? d_.data.schema + "." : "") + d_.data.table));
    const bits = [d_.data.total.toLocaleString() + " rows"];
    // docs/22 W4.1: a keyless table edits by every-column addressing — the server's note
    // says how, and it belongs in the editable branch now (this same line used to explain
    // why such a table could not be edited at all).
    const pkCols0 = d_.data.primaryKey || [];
    bits.push(d_.data.editable
      ? (pkCols0.length ? "editable — changes buffer until Commit" : "editable — " + (d_.data.editNote || "rows are addressed by all columns"))
      : (d_.data.editNote || "browsing only"));
    left.appendChild(el("div", "db-meta", bits.join("  ·  ")));
  } else if (dbIsRedis()) {
    left.appendChild(el("h2", "db-title pane-title", d_.redisKey ? d_.redisKey : "Keys"));
    const conn2 = d_.conns.find((c                    )          => { return c.name === d?.conn; });
    left.appendChild(el("div", "db-meta",
      (conn2 ? conn2.label : "") +
      (d_.redis && d_.redis.total != null ? " · " + Number(d_.redis.total).toLocaleString() + " keys" : "") +
      " · browsing · edit values in place or run commands"));
  } else {
    left.appendChild(el("h2", "db-title pane-title", "Data"));
    left.appendChild(el("div", "db-meta", d_.conn ? (d_.table ? "Loading…" : "Select a table on the left") : "No database MCP registered"));
  }
  head.appendChild(left);

  const ctl = el("div", "db-head-ctl");
  const nosql = dbIsRedis();
  if (d_.data && !d_.sqlResult && !nosql) dbRenderTabs(ctl);
  if (d_.sqlResult) {
    const back = el("button", "btn", dbIsRedis() ? "Back to keys" : "Back to table");
    back.onclick = () => {
      const d_ = d ;
      d_.sqlResult = null;
      d_.sqlResults = null; d_.sqlTab = 0; // docs/22 W4.3: leaving the results closes every tab
      dbClearSel(); // "q"-prefixed query keys must not leak into the table grid
      renderDbToolbar(); renderDbGrid();
    };
    ctl.appendChild(back);
  } else if (d_.data) {
    // Row-grid controls: page size, pager, refresh, insert, CSV. Rendered on EVERY tab but hidden
    // with visibility (keeps the width) off the Data tab, so the tab segment never shifts.
    const dataCtl = el("div", "db-data-ctl");
    if (d_.tab !== "data") dataCtl.style.visibility = "hidden";
    const size = el("select", "db-pagesize")                     ;
    size.title = "Rows per page";
    DB_PAGE_SIZES.forEach((n        )       => {
      const o = el("option", "", String(n))                     ;
      o.value = String(n);
      o.selected = n === d?.pageSize;
      size.appendChild(o);
    });
    size.onchange = (e) => {
      const t = e.currentTarget                     ;
      const d_ = d ;
      if (!dbOkToDrop()) { t.value = String(d_.pageSize); return; }
      d_.pageSize = Number(t.value);
      d_.offset = 0;
      dbDropEdits();
      void dbLoadData(true);
    };
    dataCtl.appendChild(size);

    const first = d_.offset + 1;
    const to = d_.offset + d_.data.rows.length;
    dataCtl.appendChild(el("span", "db-pageinfo",
      d_.data.total ? first.toLocaleString() + "–" + to.toLocaleString() + " of " + d_.data.total.toLocaleString() : "0 rows"));
    const prev = el("button", "btn icon")                     ;
    prev.innerHTML = icon("chevron-left");
    prev.title = "Previous page";
    prev.disabled = d_.offset === 0;
    prev.onclick = () => {
      if (!dbOkToDrop()) return;
      const d_ = d ;
      d_.offset = Math.max(0, d_.offset - d_.pageSize);
      dbDropEdits();
      void dbLoadData(true);
    };
    const next = el("button", "btn icon")                     ;
    next.innerHTML = icon("chevron-right");
    next.title = "Next page";
    // docs/22 W1.9: the limit+1 probe answers "is there another page" from the rows actually
    // fetched; the old offset-vs-total arithmetic stays as the fallback when the flag is absent.
    next.disabled = d_.data.nextPage != null ? !d_.data.nextPage : to >= d_.data.total;
    next.onclick = () => {
      if (!dbOkToDrop()) return;
      const d_ = d ;
      d_.offset += d_.pageSize;
      dbDropEdits();
      void dbLoadData(true);
    };
    dataCtl.appendChild(prev);
    dataCtl.appendChild(next);

    const refresh = el("button", "btn", "Refresh")                     ;
    refresh.title = "Reload this page (drops buffered edits)";
    refresh.onclick = ()       => {
      if (!dbOkToDrop()) return;
      dbDropEdits();
      void dbLoadData(true);
    };
    dataCtl.appendChild(refresh);

    if (d_.data.editable) {
      const addRow = el("button", "btn", "+ Row")                     ;
      addRow.title = "Buffer a new row — inserted only on Commit";
      addRow.onclick = ()       => {
        d?.inserts.push({ values: {} });
        renderDbGrid(); renderDbBar();
      };
      dataCtl.appendChild(addRow);
    }

    const csv = el("button", "btn", "CSV")                     ;
    csv.title = "Download the current page as CSV";
    csv.onclick = dbExportCsv;
    dataCtl.appendChild(csv);

    const exp = el("button", "btn", "Export…");
    exp.title = "Export the whole table as CSV, NDJSON, or SQL dump (capped at 100k rows)";
    exp.onclick = (ev            )       => {
      // stopPropagation FIRST: connect.js closes any open menu on clicks that reach document,
      // and without this the very click that opens the menu also tears it down.
      ev.stopPropagation();
      popupMenu((ev.currentTarget                     ).getBoundingClientRect(), [
        { label: "Export CSV…", fn: ()       => { void dbExportTable(exp, "csv"); } },
        { label: "Export NDJSON…", fn: ()       => { void dbExportTable(exp, "json"); } },
        { label: "Export SQL dump…", fn: ()       => { void dbExportTable(exp, "sql"); } },
      ]);
    };
    dataCtl.appendChild(exp);

    if (d_.data.editable) {
      const imp = el("button", "btn", "Import\u2026")                     ;
      imp.title = "Insert CSV rows in one transaction";
      imp.onclick = dbOpenImport;
      dataCtl.appendChild(imp);
    }
    ctl.appendChild(dataCtl);
  }
  const sql = el("button", "btn", d_.sqlOpen ? (nosql ? "Hide Command" : "Hide SQL") : (nosql ? "Command" : "SQL"))                     ;
  sql.title = nosql ? "Run one command (SET, GET, DEL, HGETALL, TTL, TYPE…)" : "SQL console — statements split on ; get a tab each";
  sql.onclick = ()       => {
    const d_ = d ;
    d_.sqlOpen = !d_.sqlOpen;
    const con = $("dbConsole");
    if (con) con.hidden = !d_.sqlOpen;
    sql.textContent = d_.sqlOpen ? (nosql ? "Hide Command" : "Hide SQL") : (nosql ? "Command" : "SQL");
    if (d_.sqlOpen && $("dbSql")) $("dbSql").focus();
  };
  ctl.appendChild(sql);
  head.appendChild(ctl);
}

/** docs/22 W1.4: fill the console with one generated stats statement and run it, so the SQL
 *  is on screen (and in history) rather than hidden behind a one-off request. */
function dbRunColumnStats(column        , kind        )       {
  const d = state.db;
  const d_ = d ;
  const dialect = (d_.conns.find((c                    )          => { return c.name === d?.conn; }) || {}                        ).dialect || "mysql";
  let sql        ;
  try {
    sql = dbStatsSql(dialect, d_.schema , d_.table , column, kind);
  } catch (err) { toast(errText(err), true); return; }
  dbFillConsole(sql);
  void dbRunSql();
}

/** Paint one cell from its typed view (docs/22 W2.3). Returns the value's long-text title
 *  (the folded JSON's full text; null when the value needs none) so callers can merge it with
 *  their own hint instead of clobbering it. */
function dbPaintCell(td                      , v         , has         , colType         )                {
  if (!has || v === undefined) return null; // nothing set yet (an insert stub cell)
  if (v === null) { td.appendChild(el("span", "db-null", "NULL")); return null; }
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

const dbTip                                                                              = { node: null, timer: 0 };

function dbTipHide()       {
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
function dbTipShow(th             , c             )       {
  const tip = el("div", "db-tip");
  tip.appendChild(el("div", "t-name", c.name));
  tip.appendChild(el("div", "t-type", c.dataType + (c.nullable ? " · nullable" : " · not null") +
    (c.isPrimaryKey ? " · primary key" : "")));
  if (c.comment) tip.appendChild(el("div", "t-comment", c.comment ));
  tip.appendChild(el("div", "t-hint", "Click sorts: ascending → descending → off"));
  document.body.appendChild(tip);
  const r = th.getBoundingClientRect();
  tip.style.left = Math.max(8, Math.min(r.left, window.innerWidth - tip.offsetWidth - 8)) + "px";
  tip.style.top = (r.bottom + 6) + "px";
  dbTip.node = tip;
}

// The card is fixed to the viewport, so a scrolled grid leaves it pointing nowhere — dismiss on
// any scroll, capture phase, once for the module's lifetime.
document.addEventListener("scroll", dbTipHide, true);


function renderDbGrid()       {
  const d = state.db;
  const wrap = $("dbGridWrap");
  if (!wrap) return;
  const con = $("dbConsole");
  const d_ = d ;
  if (con) con.hidden = !d_.sqlOpen;
  // Full rebuild inside a scrolled pane. The wrap carries overflow-anchor:none (views.css):
  // Chrome's scroll anchoring otherwise re-picks its anchor when this wipe destroys the old
  // one and compensates by scrolling, which snaps the row the user just clicked to the top.
  wrap.innerHTML = "";
  dbTipHide(); // a rebuilt grid invalidates any header card still open

  // docs/22 W4.2: conflict marks live exactly as long as the buffered change they name —
  // reverting the cell, dropping the buffer or reloading the table clears them (a new
  // commit answer re-sets or clears them in the fetch observer above).
  if (d_.conflict) {
    const cu = d_.updates && d_.updates[d_.conflict.key];
    const live = !!cu && d_.conflict.columns.some((c) => {
      return Object.prototype.hasOwnProperty.call(cu.changes, c);
    });
    if (!live) d_.conflict = null;
  }

  if (d_.sqlResult || d_.sqlBusy) { renderDbResultGrid(wrap); return; }
  if (dbIsRedis()) { dbRenderRedisValue(wrap); return; }
  // docs/22 W5.1: the Form tab paints the same rows as the grid, one record at a time.
  if (d_.tab === "form") { renderDbFormView(wrap); return; }
  if (d_.tab !== "data") { renderDbDetailGrid(wrap); return; }
  if (!d_.conn) { wrap.appendChild(el("div", "db-hint", "No database MCP registered — add a mysql or pg MCP first.")); return; }
  if (!d_.table || !d_.data) {
    if (d_.table) { wrap.appendChild(el("div", "db-hint", "Loading…")); return; }
    // The shared empty state (docs/18 V7); "Loading…" stays a quiet one-liner.
    wrap.innerHTML = emptyHtml({ icon: "database", title: "Select a table", hint: "Pick a table on the left to browse its rows." });
    return;
  }
  if (d_.loading) { wrap.appendChild(el("div", "db-hint", "Loading…")); return; }

  const pkCols = d_.data.primaryKey || [];
  const editable = d_.data.editable;
  // docs/22 W2.1: the per-connection grid config picks the columns the table shows and the
  // width each dragged column keeps. A config older than a column set (columns added since)
  // simply misses those names — an unknown width is no width, an unknown hidden name hides
  // nothing.
  const cfg = d_.gridCfg || { widths: {}, hidden: [] };
  const cols = dbGridVisibleColumns(d_.data?.columns, cfg.hidden);
  const widthOf = (c             , cell             )       => {
    const w = cfg.widths[c.name];
    if (w) { cell.style.width = w + "px"; cell.style.maxWidth = w + "px"; }
  };

  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  const keyOf = (row                         , i        )         => { return pkCols.length ? dbPkKey(pkCols, row) : String(i); };
  const thAll = el("th", "db-rowctl");
  const allOn = d_.data?.rows.length > 0 && d_.data?.rows.every((row                         , i        )          => { return !!d?.sel[keyOf(row, i)]; });
  const cbAll = document.createElement("input");
  cbAll.type = "checkbox";
  cbAll.className = "db-selbox";
  cbAll.checked = allOn;
  cbAll.title = "Select every row on this page (for copy)";
  cbAll.onclick = (e) => { e.stopPropagation(); };
  cbAll.onchange = (e) => { dbSelAll((e.currentTarget                    ).checked); };
  thAll.appendChild(cbAll);
  hr.appendChild(thAll);
  // One caption line under every column name when ANY column carries a comment: the meaning is
  // then ON the grid instead of hidden behind a hover, and giving every header the line (empty
  // where there is nothing to say) keeps the header row one even height.
  const hasComments = cols.some((c             )          => { return !!c.comment; });
  cols.forEach((c             )       => {
    const d_ = d ;
    const sorted = c.name === d_.order;
    const th = el("th", "db-col" + (sorted ? " db-sorted" + (d_.dir === "desc" ? " db-sorted-desc" : "") : ""));
    const main = el("div", "db-col-main");
    main.appendChild(el("span", "db-col-name", c.name));
    if (c.isPrimaryKey) main.appendChild(el("span", "db-key", "⚿"));
    main.appendChild(el("span", "db-col-type", c.dataType));
    // docs/22 W5.2: the FK column's jump — one small straight arrow (the chevron belongs to
    // pagination) that opens the referenced table with the FOCUSED row's value as an eq
    // filter, the same channel a typed filter or W1.5's cell menu uses. The detail (and its
    // foreignKeys) loads with the table; until it answers there is no arrow, which is the
    // honest state — and a table with no FKs never draws one.
    const fk = ((d_.detail && d_.detail.foreignKeys) || []).filter((f            )          => { return f.column === c.name; })[0];
    if (fk) {
      const jump = el("button", "db-col-fk")                     ;
      jump.type = "button";
      jump.setAttribute("aria-label", "Jump to referenced row");
      jump.title = "Open " + (fk.refSchema ? fk.refSchema + "." : "") + fk.refTable +
        " filtered to this column's value in the focused row";
      jump.innerHTML = icon("arrow-right");
      jump.onclick = (ev            )       => {
        ev.stopPropagation(); // the header click sorts; this click jumps
        const v = dbFocusedColumnValue(state.db, c.name);
        if (v === undefined) {
          toast("Click a cell in " + c.name + " first — the jump uses that row's value", true);
          return;
        }
        if (v === null) {
          toast("The focused row's " + c.name + " is NULL — nothing to jump to", true);
          return;
        }
        dbFkOpen(fk, v);
      };
      main.appendChild(jump);
    }
    if (sorted) main.appendChild(el("span", "db-sort"));
    th.appendChild(main);
    if (hasComments) th.appendChild(el("div", "db-col-comment", c.comment || ""));
    // docs/22 W2.1: the resize grip hugs the header's right edge. It owns mousedown and click
    // so a drag neither sorts the column nor fights the hover card for the pointer.
    const grip = el("span", "db-col-grip");
    grip.title = "Drag to resize";
    grip.onmousedown = (ev            )       => {
      const idx = Array.prototype.indexOf.call(hr.children, th);
      const cells = [th]                            ;
      const trs = tbl.querySelectorAll("tbody tr");
      for (let r = 0; r < trs.length; r++) if (trs[r].children[idx]) cells.push(trs[r].children[idx]                          );
      dbColResizeStart(ev, c.name, th, cells                                      );
    };
    grip.onclick = (ev            )       => { ev.stopPropagation(); };
    th.appendChild(grip);
    widthOf(c, th);
    th.onmouseenter = ()       => {
      clearTimeout(dbTip.timer);
      dbTip.timer = setTimeout(()       => { dbTipShow(th, c); }, 260);
    };
    th.onmouseleave = ()       => { clearTimeout(dbTip.timer); dbTipHide(); };
    th.onclick = ()       => {
      dbTipHide();
      if (!dbOkToDrop()) return;
      const d_ = d ;
      const next = dbNextSort(d_.order, d_.dir, c.name);
      d_.order = next ? next.order : null;
      d_.dir = next ? next.dir : "asc";
      d_.offset = 0;
      dbDropEdits();
      void dbLoadData(true);
    };
    // docs/22 W1.4: the header's own right-click runs this column's stats — top values or
    // COUNT/MIN/MAX/AVG. The statement is generated behind the identifier gate and lands in
    // the console (visible, in history) with its result in the standing result grid.
    th.oncontextmenu = (e            )       => {
      e.preventDefault();
      dbTipHide();
      // docs/22 W2.1: hiding lives in the same menu, one separator down, and the recovery
      // entry ("Show all columns") appears exactly while something is hidden — one place for
      // both directions.
      const items             = [
        { label: "Value distribution\u2026", fn: ()       => { dbRunColumnStats(c.name, "dist"); } },
        { label: "Numeric stats\u2026", fn: ()       => { dbRunColumnStats(c.name, "num"); } },
        { sep: true },
        { label: "Hide column", fn: ()       => { dbHideColumn(c.name); } },
      ];
      const d_ = d ;
      if (d_.gridCfg && d_.gridCfg.hidden.length) {
        items.push({ label: "Show all columns", fn: dbShowAllColumns });
      }
      popupMenu((e.currentTarget               ).getBoundingClientRect(), items);
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
    inner.appendChild(el("span", "", "Every column of " +
      (d_.schema ? d_.schema + "." : "") + d_.table + " is hidden."));
    const showAll = el("button", "btn", "Show all columns")                     ;
    showAll.onclick = dbShowAllColumns;
    inner.appendChild(showAll);
    tdh.appendChild(inner);
    trh.appendChild(tdh);
    tbody.appendChild(trh);
    tbl.appendChild(tbody);
    wrap.appendChild(tbl);
    return;
  }

  // Buffered inserts first, so they read as "the row you are about to add".
  d_.inserts.forEach((ins          , i        )       => {
    const tr = el("tr", "db-ins");
    const rc = el("td", "db-rowctl");
    const rm = el("button", "db-act", "✕")                     ;
    rm.title = "Remove this buffered insert";
    rm.onclick = ()       => {
      d?.inserts.splice(i, 1);
      renderDbGrid(); renderDbBar();
    };
    rc.appendChild(rm);
    tr.appendChild(rc);
    cols.forEach((c             , ci        )       => {
      const has = Object.prototype.hasOwnProperty.call(ins.values, c.name);
      const td = el("td", "db-cell")                                                                        ;
      // The cell's grid address: the focus ring moves between live cells by it (P0-B).
      td.setAttribute("data-r", i                     );
      td.setAttribute("data-c", ci                     );
      widthOf(c, td);
      const long = dbPaintCell(td, has ? ins.values[c.name] : undefined, has, c.dataType);
      // docs/22 W2.2: one click puts the keyboard's focus cell here; the ring marks it.
      // preventDefault (P0-A): the mousedown's default focus move lands on <body> AFTER
      // dbFocusCell focused #dbKbd, and arrows/Enter/F2/typing/Esc/Ctrl+C/paste would all
      // go nowhere. Click and dblclick still fire — preventing the default does not
      // suppress them.
      td.onmousedown = (ev            )       => { ev.preventDefault(); dbFocusCell(i, ci); };
      const d_ = d ;
      if (d_.focus && d_.focus.r === i && d_.focus.c === ci) td.classList.add("db-focus");
      if (editable) {
        td.classList.add("db-cell-edit");
        td.title = long || "Double-click to edit · right-click for dialog/copy";
        ((col        , present         )       => {
          td.ondblclick = ()       => {
            const cur = present ? dbCellText(ins.values[col]) : "";
            dbEditCellEnter("insert", null                     , i, col, {}              , td, cur == null ? "" : cur);
          };
          td.dbKbdEdit = (seed               )       => {
            const cur = seed != null ? seed : present ? dbCellText(ins.values[col]) : "";
            dbEditCellEnter("insert", null                     , i, col, {}              , td, cur == null ? "" : cur);
          };
          // docs/22 closeout B2: the editInDialog callback is dbCellMenu's FIFTH parameter —
          // a stray null before it parked the callback in an unread sixth slot, so the insert
          // row's menu never offered the dialog path a data row's menu always had.
          td.oncontextmenu = (e            )       => { dbCellMenu(e, null, null                     , col, ()       => { dbOpenCellEditor("insert", null                     , i, col, {}              ); }); };
        })(c.name, has);
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });

  d_.data?.rows.forEach((row                         , rowIdx        )       => {
    const key = keyOf(row, rowIdx);
    const d_ = d ;
    const deleted = !!d_.deletes[key];
    const upd = d_.updates[key];
    const tr = el("tr", (deleted ? "db-del " : "") + (d_.sel[key] ? "db-sel" : ""));
    const rc = el("td", "db-rowctl");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.className = "db-selbox";
    cb.checked = !!d_.sel[key];
    cb.title = "Select row for copy (Shift-click for a range)";
    cb.onclick = (e            )       => { e.stopPropagation(); };
    cb.onchange = (ev                                )       => {
      const d_ = d ;
      if (ev.shiftKey && d_.selAnchor >= 0 && d_.selAnchor !== rowIdx) {
        const a = Math.min(d_.selAnchor, rowIdx);
        const b2 = Math.max(d_.selAnchor, rowIdx);
        for (let k = a; k <= b2; k++) d_.sel[keyOf(d_.data .rows[k], k)] = true;
      } else if ((ev.currentTarget                    ).checked) d_.sel[key] = true;
      else delete d_.sel[key];
      d_.selAnchor = rowIdx;
      renderDbToolbar(); renderDbGrid();
    };
    rc.appendChild(cb);
    if (editable) {
      const b = el("button", "db-act", deleted ? "↩" : "✕")                     ;
      b.title = deleted ? "Undo this buffered delete" : "Buffer a delete — applied only on Commit";
      b.onclick = ()       => {
        const d_ = d ;
        if (deleted) delete d_.deletes[key];
        else {
          d_.deletes[key] = dbRowAddr(pkCols, d_.data .columns, row);
          delete d_.updates[key]; // a deleted row's cell edits are moot
        }
        renderDbGrid(); renderDbBar();
      };
      rc.appendChild(b);
    }
    tr.appendChild(rc);
    cols.forEach((c             , ci        )       => {
      const orig = row[c.name];
      const pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name);
      const v = pending ? upd.changes[c.name] : orig;
      // docs/22 W4.2: the cells a lost commit race named stay red while their buffered
      // change is still pending — amber says "differs from the loaded row", red says
      // "the server refused this change; the row moved under it".
      const d_ = d ;
      const lost = pending && !!d_.conflict && d_.conflict.key === key && d_.conflict.columns.indexOf(c.name) >= 0;
      const td = el("td", "db-cell" + (pending ? " db-dirty" : "") + (lost ? " db-conflict" : ""))                                                                        ;
      // docs/22 W2.2: same focus wiring as insert cells; the row index counts the inserts
      // above. The address rides the cell so the ring can move without a rebuild (P0-B).
      const gridRow = d_.inserts.length + rowIdx;
      td.setAttribute("data-r", gridRow                     );
      td.setAttribute("data-c", ci                     );
      widthOf(c, td);
      const long = dbPaintCell(td, v, true, c.dataType);
      td.onmousedown = (ev            )       => { ev.preventDefault(); dbFocusCell(gridRow, ci); }; // P0-A: see the insert cells
      if (d_.focus && d_.focus.r === gridRow && d_.focus.c === ci) td.classList.add("db-focus");
      if (editable && !deleted) {
        td.classList.add("db-cell-edit");
        td.title = long || "Double-click to edit · right-click for dialog/copy";
        ((col        , orig         )       => {
          const meta             = { pk: dbRowAddr(pkCols, d .data .columns, row), orig: orig };
          td.ondblclick = ()       => {
            const cur = dbCellText(orig);
            dbEditCellEnter("update", key, -1, col, meta, td, cur == null ? "" : cur);
          };
          td.dbKbdEdit = (seed               )       => {
            const cur = seed != null ? seed : dbCellText(orig);
            dbEditCellEnter("update", key, -1, col, meta, td, cur == null ? "" : cur);
          };
          td.oncontextmenu = (e            )       => {
            dbCellMenu(e, row, key, col, ()       => { dbOpenCellEditor("update", key, -1, col, meta); });
          };
        })(c.name, orig);
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });

  tbl.appendChild(tbody);
  wrap.appendChild(tbl);

  // docs/22 W2.2: the zero-size input that carries the grid's keyboard focus. It sits inside
  // the scrolling wrap and owns keydown + paste — arrows/Enter/F2/Esc/Home/End/typing,
  // Ctrl+C, and TSV paste. Every focus() call on it must pass preventScroll: it lives at
  // the end of the scrolled content, and a plain focus() drags the pane to the bottom.
  const kbd = el("input", "db-kbd")                    ;
  kbd.type = "text";
  kbd.id = "dbKbd";
  kbd.tabIndex = -1;
  kbd.setAttribute("aria-label", "Data grid keyboard navigation");
  kbd.onkeydown = (ev               )       => {
    const d = state.db;
    const d_ = d ;
    if (!d_.data) return;
    const maxR = dbGridRowsCount() - 1;
    const maxC = cols.length - 1;
    if (!d_.focus) {
      if (ev.key === "ArrowUp" || ev.key === "ArrowDown" || ev.key === "ArrowLeft" ||
        ev.key === "ArrowRight" || ev.key === "Home" || ev.key === "End") {
        ev.preventDefault();
        dbFocusCell(0, 0);
      }
      return;
    }
    if (ev.key === "ArrowUp" || ev.key === "ArrowDown" || ev.key === "ArrowLeft" ||
      ev.key === "ArrowRight" || ev.key === "Home" || ev.key === "End") {
      ev.preventDefault();
      const m = dbKbdMove(d_.focus.r, d_.focus.c, ev.key, maxR, maxC);
      dbFocusCell(m.r, m.c); // moves the ring on the live grid — no rebuild mid-navigation
      return;
    }
    if (ev.key === "Enter" || ev.key === "F2") {
      ev.preventDefault();
      dbFocusEdit();
      return;
    }
    if (ev.key === "Escape") {
      // Esc's one meaning here is "put the keyboard down" — the cell keeps its value and its
      // ring goes away. (The inline editor and the sheet handle their own Esc first.) The
      // ring leaves by class move, not a rebuild (P0-B); blur leaves the keyboard exactly
      // where the rebuild used to drop it.
      ev.preventDefault();
      d_.focus = null;
      const wrapEsc = $("dbGridWrap");
      const ring = wrapEsc ? wrapEsc.querySelector("td.db-focus") : null;
      if (ring) ring.classList.remove("db-focus");
      const kb = ev.currentTarget               ;
      if (kb.blur) kb.blur();
      return;
    }
    if ((ev.ctrlKey || ev.metaKey) && (ev.key === "c" || ev.key === "C")) {
      dbCopyChecked();
      return;
    }
    if (!ev.ctrlKey && !ev.metaKey && !ev.altKey && ev.key.length === 1) {
      // Typing into a cell starts the edit seeded with the character, exactly the dblclick
      // editor with a head start.
      ev.preventDefault();
      dbFocusEdit(ev.key);
    }
  };
  kbd.onpaste = (ev                )       => {
    const d = state.db;
    const d_ = d ;
    if (!d_.data) return;
    const text = ev.clipboardData ? ev.clipboardData.getData("text/plain") : "";
    if (!text) return;
    ev.preventDefault();
    dbPasteApply(text, d_.focus ? d_.focus.r : 0, d_.focus ? d_.focus.c : 0);
  };
  wrap.appendChild(kbd);
}

function renderDbResultGrid(wrap             )       {
  const d = state.db;
  const d_ = d ;
  if (d_.sqlBusy) { wrap.appendChild(el("div", "db-hint", "Running…")); return; }
  const res = d_.sqlResult;
  if (!res) { wrap.appendChild(el("div", "db-hint", "Run a query to see rows here.")); return; }
  const tab = d_.sqlTab || 0; // docs/22 W4.3: this grid is one tab of the strip — its selection keys are that tab's
  const tbl = el("table", "db-grid");
  const thead = el("thead");
  const hr = el("tr");
  const thAll2 = el("th", "db-rowctl");
  const allOn2 = res.rows.length > 0 && res.rows.every((_                         , i        )          => { return !!d?.sel[dbResultKey(tab, i)]; });
  const cbAll2 = document.createElement("input");
  cbAll2.type = "checkbox";
  cbAll2.className = "db-selbox";
  cbAll2.checked = allOn2;
  cbAll2.title = "Select every result row (for copy)";
  cbAll2.onclick = (e            )       => { e.stopPropagation(); };
  cbAll2.onchange = (e) => { dbSelAll((e.currentTarget                    ).checked); };
  thAll2.appendChild(cbAll2);
  hr.appendChild(thAll2);
  res.columns.forEach((c        )       => { hr.appendChild(el("th", "db-col", c)); });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  const tbody = el("tbody");
  res.rows.forEach((row                         , i        )       => {
    const qkey = dbResultKey(tab, i);
    const tr = el("tr", d?.sel[qkey] ? "db-sel" : "");
    const rc2 = el("td", "db-rowctl");
    const cb2 = document.createElement("input");
    cb2.type = "checkbox";
    cb2.className = "db-selbox";
    cb2.checked = !!d?.sel[qkey];
    cb2.title = "Select row for copy (Shift-click for a range)";
    cb2.onclick = (e            )       => { e.stopPropagation(); };
    cb2.onchange = (ev                                )       => {
      const d_ = d ;
      if (ev.shiftKey && d_.selAnchor >= 0 && d_.selAnchor !== i) {
        const a2 = Math.min(d_.selAnchor, i);
        const b3 = Math.max(d_.selAnchor, i);
        for (let k2 = a2; k2 <= b3; k2++) d_.sel[dbResultKey(tab, k2)] = true;
      } else if ((ev.currentTarget                    ).checked) d_.sel[qkey] = true;
      else delete d_.sel[qkey];
      d_.selAnchor = i;
      renderDbToolbar(); renderDbGrid();
    };
    rc2.appendChild(cb2);
    rc2.appendChild(document.createTextNode(String(i + 1)));
    tr.appendChild(rc2);
    res?.columns.forEach((c        )       => {
      const td = el("td", "db-cell");
      td.oncontextmenu = (e            )       => { dbResultCellMenu(e, row, c); };
      // No column types on a result grid: dbCellView still words booleans, folds long JSON,
      // links URLs and right-aligns JS numbers off the value alone.
      const v = row[c];
      dbPaintCell(td, v === undefined ? null : v, true);
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });
  tbl.appendChild(tbody);
  wrap.appendChild(tbl);
}

/** The three-state cycle a header click walks: a fresh column sorts ascending, the next click
    flips to descending, and a third click clears the order (null) back to the table's natural
    order. Pure so the cycle can be tested without a DOM. */
function dbNextSort(order               , dir               , name        )                                        {
  if (order !== name) return { order: name, dir: "asc" };
  if (dir === "desc") return null;
  return { order: name, dir: "desc" };
}

export { DB_COL_MAX, DB_COL_MIN, dbColResizeStart, dbCopyChecked, dbFocusCell, dbGridConfigKey, dbGridConfigLoad, dbGridConfigParse, dbGridConfigSave, dbGridVisibleColumns, dbHideColumn, dbKbdMove, dbLoadData, dbNextSort, dbPaintCell, dbPasteApply, dbRowAddr, renderDbGrid, renderDbResultGrid, renderDbToolbar, dbShowAllColumns, dbTsvRows };
