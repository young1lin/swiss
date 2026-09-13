import { $, apiJson, el, emptyHtml, icon, state, toast } from "./util.js";
import { dbIsRedis, dbRenderRedisValue } from "./data-browsers.js";
import { dbCellMenu, dbCopyCsvCell, dbCopyText, dbExportCsv, dbOpenImport, dbResultCellMenu, dbRowForCopy, dbSelAll, dbSelectedForCopy } from "./data-csv.js";
import { dbOpenCellEditor, dbCellText, dbCellView } from "./data-cell.js";
import { dbEditCellEnter } from "./data-edit.js";
import { dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbFillConsole, dbRunSql, dbStatsSql, renderDbBar } from "./data-sql.js";
import { dbRenderTabs, renderDbDetailGrid } from "./data-structure.js";
import { DB_PAGE_SIZES, dbClearSel, dbDropEdits, dbOkToDrop, dbPkKey, dbPkVals, dbResultKey } from "./data-view.js";
import { popupMenu } from "./menu.js";
import { act } from "./detail.js";

/* --- one page of rows --------------------------------------------------------------------------- */

/* --- grid config: per-connection column widths and hidden columns (docs/22 W2.1) ---------------- */
/* dbgate keeps one GridConfig object per grid (GridConfig.ts + useGridConfig.ts); the equivalent
   here is ONE localStorage object per connection+table holding the widths the user dragged and
   the columns they hid. The helpers are pure (or storage-only) so the key format, the parse's
   tolerance of a corrupt entry and the clamp pin without a DOM. The grid reloads the config on
   every page load — a rename or a second tab's change is picked up, never cached stale. */
var DB_GRID_PREFIX = "mcp_gateway_db_grid_";
var DB_COL_MIN = 48;  // narrower than the header's own name line and nothing reads
var DB_COL_MAX = 1200; // wider than the grid itself — a runaway drag helps nobody

function dbGridConfigKey(conn, schema, table) {
  return DB_GRID_PREFIX + conn + "_" + (schema ? schema + "." : "") + table;
}

/** Parse one stored config. A missing, corrupt or wrongly-typed entry is a fresh config:
 *  storage is the only writer and quota errors can leave anything behind, but a broken grid
 *  is never an acceptable consequence. Widths outside the clamp are dropped, not clamped —
 *  a hand-edited 0 was never a width the user dragged. */
function dbGridConfigParse(raw) {
  var out = { widths: {}, hidden: [] };
  if (!raw) return out;
  try {
    var j = JSON.parse(raw);
    if (!j || typeof j !== "object") return out;
    if (j.widths && typeof j.widths === "object") {
      Object.keys(j.widths).forEach(function (k) {
        var n = Number(j.widths[k]);
        if (isFinite(n) && n >= DB_COL_MIN && n <= DB_COL_MAX) out.widths[k] = Math.round(n);
      });
    }
    if (Array.isArray(j.hidden)) {
      out.hidden = j.hidden.filter(function (h) { return typeof h === "string" && h; });
    }
  } catch (e) { /* fresh config, see above */ }
  return out;
}

function dbGridConfigLoad(key) {
  var raw = null;
  try { raw = localStorage.getItem(key); } catch (e) { /* private mode: defaults */ }
  return dbGridConfigParse(raw);
}

function dbGridConfigSave(key, cfg) {
  try { localStorage.setItem(key, JSON.stringify(cfg)); } catch (e) { /* quota/private mode: this session only */ }
}

/** The columns the grid shows after the config's hidden list. Pure. */
function dbGridVisibleColumns(columns, hidden) {
  if (!hidden || !hidden.length) return columns.slice();
  var drop = {};
  hidden.forEach(function (h) { drop[h] = true; });
  return columns.filter(function (c) { return !drop[c.name]; });
}

/** The config's key for the table currently open (null when nothing is open). */
function dbGridKeyNow() {
  var d = state.db;
  return d.conn && d.table ? dbGridConfigKey(d.conn, d.schema, d.table) : null;
}

/** Hide one column (the header menu) / bring every hidden column back, writing through to
 *  storage so the choice survives a reload. */
function dbHideColumn(name) {
  var d = state.db;
  if (d.gridCfg.hidden.indexOf(name) < 0) d.gridCfg.hidden.push(name);
  var key = dbGridKeyNow();
  if (key) dbGridConfigSave(key, d.gridCfg);
  renderDbGrid();
}

function dbShowAllColumns() {
  var d = state.db;
  d.gridCfg.hidden = [];
  var key = dbGridKeyNow();
  if (key) dbGridConfigSave(key, d.gridCfg);
  renderDbGrid();
}

/** Drag the column's right edge: width follows the pointer live across every cell of the
 *  column (cells captured at drag start — no repaint per mousemove), and the config is
 *  written once on release. The handle owns mousedown/click (stopPropagation) so a drag never
 *  sorts the column it is resizing. Each cell gets width AND maxWidth — under
 *  table-layout: auto a width alone is only a hint the nowrap content outvotes. */
function dbColResizeStart(e, name, th, cells) {
  e.preventDefault();
  e.stopPropagation();
  var d = state.db;
  var startX = e.clientX;
  var startW = th.getBoundingClientRect().width;
  var lastW = startW;
  function move(ev) {
    lastW = Math.max(DB_COL_MIN, Math.min(DB_COL_MAX, Math.round(startW + ev.clientX - startX)));
    for (var i = 0; i < cells.length; i++) {
      cells[i].style.width = lastW + "px";
      cells[i].style.maxWidth = lastW + "px";
    }
  }
  function up() {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
    if (lastW !== Math.round(startW)) {
      d.gridCfg.widths[name] = lastW;
      var key = dbGridKeyNow();
      if (key) dbGridConfigSave(key, d.gridCfg);
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
function dbTsvRows(text) {
  var t = String(text == null ? "" : text).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (!t) return [];
  var rows = t.split("\n");
  if (rows.length > 1 && rows[rows.length - 1] === "") rows.pop();
  return rows.map(function (r) { return r.split("\t"); });
}

/** Move the focus cell one key at a time, clamped to the grid's bounds. Pure. */
function dbKbdMove(r, c, key, maxR, maxC) {
  if (key === "ArrowUp") return { r: Math.max(0, r - 1), c: c };
  if (key === "ArrowDown") return { r: Math.min(maxR, r + 1), c: c };
  if (key === "ArrowLeft") return { r: r, c: Math.max(0, c - 1) };
  if (key === "ArrowRight") return { r: r, c: Math.min(maxC, c + 1) };
  if (key === "Home") return { r: r, c: 0 };
  if (key === "End") return { r: r, c: maxC };
  return { r: r, c: c };
}

/** The grid row count the keyboard walks: buffered inserts first, then the page's rows. */
function dbGridRowsCount() {
  var d = state.db;
  return (d.data ? d.inserts.length + d.data.rows.length : 0);
}

/** Focus (or move) the focus cell and repaint — a single selection the keyboard owns. */
function dbFocusCell(r, c) {
  var d = state.db;
  var maxR = dbGridRowsCount() - 1;
  var maxC = dbGridVisibleColumns(d.data ? d.data.columns : [], (d.gridCfg || { hidden: [] }).hidden).length - 1;
  d.focus = { r: Math.max(0, Math.min(maxR, r)), c: Math.max(0, Math.min(maxC, c)) };
  renderDbGrid();
  var kbd = $("dbKbd");
  if (kbd) kbd.focus();
  var td = document.querySelector("#dbGridWrap td.db-focus");
  if (td && td.scrollIntoView) td.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Open the focused cell's editor the way a double-click does. seed (a typed character)
 *  replaces the current text; without one the editor starts from the cell's value. */
function dbFocusEdit(seed) {
  var td = document.querySelector("#dbGridWrap td.db-focus");
  if (!td || !td.dbKbdEdit) return;
  td.dbKbdEdit(seed == null ? null : String(seed));
}

/** Write one pasted cell into the buffers with the same semantics a typed edit has:
 *  an insert cell reverts to the column default on empty, an update collapses back when the
 *  pasted text equals the original. */
function dbPasteCell(kind, key, i, column, meta, raw) {
  var d = state.db;
  if (kind === "insert") {
    var ins = d.inserts[i];
    if (raw === "") delete ins.values[column];
    else ins.values[column] = raw;
    return;
  }
  var row = d.data.rows[i];
  // meta.pk is already the pk VALUES map the caller built (same object dbSaveInlineEdit
  // stores); running it through dbPkVals again would forEach over an object and throw.
  var upd = d.updates[key] || (d.updates[key] = { pk: meta.pk, changes: {} });
  var origTxt = meta.orig == null ? "" : String(meta.orig);
  if (raw === origTxt) {
    delete upd.changes[column];
    if (!Object.keys(upd.changes).length) delete d.updates[key];
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
function dbRowAddr(pkCols, columns, row) {
  var out = {};
  columns.forEach(function (c) { out[c.name] = row[c.name]; });
  return out;
}

/** Structural equality for two JSON values — the 409 body's row map against a buffered
 *  entry's pk, so the conflict lands on the exact buffered row (docs/22 W4.2). */
function dbSameJson(a, b) {
  if (a === b) return true;
  if (a == null || b == null || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  var ka = Object.keys(a), kb = Object.keys(b);
  return ka.length === kb.length && ka.every(function (k) { return dbSameJson(a[k], b[k]); });
}

/** docs/22 W4.2: a lost edit race answers 409 naming the moved columns — but the POST
 *  itself lives in data-sql.js's dbCommit, which this module cannot call back into. The
 *  narrowest honest seam: watch our own /edits responses at the fetch level (path match
 *  plus status; every other request passes through untouched). The failing commit already
 *  keeps the buffer and apiJson already toasts the body's error text with the column
 *  names; what only the grid can do is remember WHICH buffered row and columns lost, so
 *  the next render paints those cells red until the buffer for them changes. */
(function () {
  // The module also loads where no DOM exists (the Node vitest suite imports the grid
  // module for its state shape) — touching window at module level breaks that import
  // with a ReferenceError, so the observer only arms where a window with fetch is real.
  if (typeof window === "undefined" || !window.fetch) return;
  var orig = window.fetch;
  window.fetch = function (input, init) {
    return orig.apply(this, arguments).then(function (r) {
      try {
        var path = typeof input === "string" ? input : (input && input.url) || "";
        if (/\/edits(\?|$)/.test(path)) {
          var d = state.db;
          if (r.status === 409) {
            r.clone().json().then(function (j) {
              d = state.db;
              var cols = (j && j.conflictColumns) || [];
              if (!d || !d.updates || !cols.length || !j || !j.row) return;
              var hit = Object.keys(d.updates).filter(function (k) {
                return dbSameJson(d.updates[k].pk, j.row);
              })[0];
              if (hit) { d.conflict = { key: hit, columns: cols }; renderDbGrid(); }
            }).catch(function () { /* an observation never breaks the panel */ });
          } else if (d && d.conflict) {
            d.conflict = null; // a clean answer (or a different failure) clears the marks
            renderDbGrid();
          }
        }
      } catch (e) { /* never */ }
      return r;
    });
  };
})();

function dbPasteApply(text, r0, c0) {
  var d = state.db;
  if (!d.data || !d.data.editable) {
    toast("This table is not editable (" + (d.data && d.data.editNote ? d.data.editNote : "no primary key") + ") — paste needs the edit buffer", true);
    return;
  }
  var cfg = d.gridCfg || { widths: {}, hidden: [] };
  var cols = dbGridVisibleColumns(d.data.columns, cfg.hidden);
  var rows = dbTsvRows(text);
  if (!rows.length || !cols.length) return;
  var pkCols = d.data.primaryKey || [];
  var keyOf = function (row, i) { return pkCols.length ? dbPkKey(pkCols, row) : String(i); };
  for (var i = 0; i < rows.length; i++) {
    var target = r0 + i;
    for (var j = 0; j < rows[i].length; j++) {
      var colIdx = c0 + j;
      if (colIdx >= cols.length) break;
      var col = cols[colIdx].name;
      var raw = rows[i][j];
      var insertsBefore = d.inserts.length;
      if (target < insertsBefore) {
        dbPasteCell("insert", null, target, col, null, raw);
      } else {
        var ri = target - d.inserts.length;
        if (ri < d.data.rows.length) {
          var row = d.data.rows[ri];
          var key = keyOf(row, ri);
          dbPasteCell("update", key, ri, col, { pk: dbRowAddr(pkCols, d.data.columns, row), orig: row[col] }, raw);
        } else {
          // Past the page: the paste row becomes a NEW buffered insert, values in column order.
          d.inserts.push({ values: {} });
          dbPasteCell("insert", null, d.inserts.length - 1, col, null, raw);
        }
      }
    }
  }
  renderDbGrid();
  renderDbBar();
}

/** Ctrl+C: the checked rows as CSV (the row menu's Copy), or — with nothing checked — the
 *  focused row alone, which is what a grid hand expects Ctrl+C to grab. */
function dbCopyChecked() {
  var d = state.db;
  var sel = dbSelectedForCopy();
  if (sel && sel.rows && sel.rows.length) {
    // Same shape the row menu's Copy-as-CSV builds (data-csv keeps dbRowsCsv private): a
    // header line plus one quoted line per checked row.
    var lines = [sel.cols.map(dbCopyCsvCell).join(",")];
    sel.rows.forEach(function (r) { lines.push(sel.cols.map(function (cn) { return dbCopyCsvCell(r[cn]); }).join(",")); });
    dbCopyText(lines.join("\n"));
    return;
  }
  if (!d.focus || !d.data) return;
  var nIns = d.inserts.length;
  if (d.focus.r < nIns) return; // a buffered insert has no committed row to copy
  var row = d.data.rows[d.focus.r - nIns];
  var pkCols = d.data.primaryKey || [];
  var key = pkCols.length ? dbPkKey(pkCols, row) : String(d.focus.r - nIns);
  var full = dbRowForCopy(row, key);
  var line = d.data.columns.map(function (c) {
    return dbCopyCsvCell(full == null ? null : full[c.name]);
  }).join(",");
  dbCopyText(line);
}

async function dbLoadData(keepOffset) {
  var d = state.db;
  if (!d.conn || !d.table) return;
  if (!keepOffset) d.offset = 0;
  d.loading = true;
  renderDbToolbar(); renderDbGrid();
  var q = "/api/db/" + encodeURIComponent(d.conn) + "/data?table=" + encodeURIComponent(d.table) +
    "&offset=" + d.offset + "&limit=" + d.pageSize;
  if (d.schema) q += "&schema=" + encodeURIComponent(d.schema);
  if (d.order) q += "&order=" + encodeURIComponent(d.order) + "&dir=" + d.dir;
  if (d.filters.length) q += "&filters=" + encodeURIComponent(JSON.stringify(d.filters));
  var j = await apiJson(q);
  d.loading = false;
  if (!j) { renderDbToolbar(); renderDbGrid(); return; }
  d.data = j;
  d.schema = j.schema;
  // The grid config reloads with every page: a rename (new key) or a second tab's hide lands
  // on the next paint instead of a cached opinion (docs/22 W2.1).
  d.gridCfg = dbGridConfigLoad(dbGridKeyNow());
  dbDropEdits(); // a fresh page is a fresh baseline — buffered edits never survive a reload
  // A filter naming a column that no longer exists would 400 on every reload — drop it instead.
  var names = d.data.columns.map(function (c) { return c.name; });
  d.filters = d.filters.filter(function (f) { return names.indexOf(f.column) >= 0; });
  renderDbToolbar(); renderDbGrid(); renderDbBar(); renderDbFilters();
}

/* Whole-table export in the chosen format (CSV, NDJSON, or SQL dump). The toolbar button carries the
   busy state: the format menu is gone by the time the download starts, so the button is the
   only place left on screen that can say "working". */
async function dbExportTable(btn, fmt) {
  var d2 = state.db;
  var name = fmt === "json" ? "NDJSON" : fmt === "sql" ? "SQL dump" : "CSV";
  if (!confirm("Export " + (d2.schema ? d2.schema + "." : "") + d2.table + " as " + name +
      "?\nCapped at 100,000 rows — filter first if you need less.")) return;
  var q = "/api/db/" + encodeURIComponent(d2.conn) + "/export?table=" + encodeURIComponent(d2.table) +
    "&format=" + fmt;
  if (d2.schema) q += "&schema=" + encodeURIComponent(d2.schema);
  // The grid's filters ride along: the download and the grid describe the same filtered
  // set, and the exported row count is the filtered total.
  if (d2.filters.length) q += "&filters=" + encodeURIComponent(JSON.stringify(d2.filters));
  btn.disabled = true;
  btn.textContent = "Exporting\u2026";
  try {
    var resp = await fetch(q);
    if (!resp.ok) { toast("Export failed: HTTP " + resp.status, true); return; }
    var blob = await resp.blob();
    var disp = resp.headers.get("Content-Disposition") || "";
    var m = /filename="([^"]+)"/.exec(disp);
    var a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = m ? m[1] : "export." + fmt;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(function () { URL.revokeObjectURL(a.href); }, 1000);
    var rows = resp.headers.get("X-Export-Rows");
    toast("Exported " + (rows != null ? Number(rows).toLocaleString() + " rows" : ""));
  } catch (e) {
    toast("Export failed", true);
  } finally {
    btn.disabled = false;
    btn.textContent = "Export\u2026";
  }
}

function renderDbToolbar() {
  var d = state.db;
  var head = $("dbHead");
  if (!head) return;
  head.innerHTML = "";
  var left = el("div", "db-head-left");
  if (d.sqlResult) {
    left.appendChild(el("h2", "db-title pane-title", d.sqlResult.explained ? "Execution plan" : (dbIsRedis() ? "Command reply" : "SQL results")));
    left.appendChild(el("div", "db-meta", d.sqlResult.rowCount + " row" + (d.sqlResult.rowCount === 1 ? "" : "s") +
      (d.sqlResult.note ? " · " + d.sqlResult.note : "") +
      (d.sqlResult.elapsedMs != null ? " · " + d.sqlResult.elapsedMs + " ms" : "")));
    // docs/22 W4.3: one tab per statement reply, in the pane's segmented-control vocabulary
    // (.db-tabs — the same strip the Structure tabs use). Eight fit the pane; past that the
    // strip scrolls sideways instead of wrapping. Switching only repoints the active result;
    // every tab keeps its own checked-row keys (dbResultKey namespaces d.sel by tab), so a
    // Shift-range or a copy never crosses tabs.
    if ((d.sqlResults || []).length > 1) {
      var seg = el("div", "db-tabs");
      seg.setAttribute("role", "tablist");
      d.sqlResults.forEach(function (r, ti) {
        var b = el("button", "", r.tabLabel || "Result " + (ti + 1));
        b.setAttribute("role", "tab");
        b.setAttribute("aria-selected", ti === d.sqlTab ? "true" : "false");
        b.onclick = function () {
          if (d.sqlTab === ti) return;
          d.sqlTab = ti;
          d.sqlResult = d.sqlResults[ti];
          d.selAnchor = -1; // a Shift-range never spans tabs
          renderDbToolbar();
          renderDbGrid();
        };
        seg.appendChild(b);
      });
      left.appendChild(seg);
    }
  } else if (d.data) {
    left.appendChild(el("h2", "db-title pane-title", (d.data.schema ? d.data.schema + "." : "") + d.data.table));
    var bits = [d.data.total.toLocaleString() + " rows"];
    // docs/22 W4.1: a keyless table edits by every-column addressing — the server's note
    // says how, and it belongs in the editable branch now (this same line used to explain
    // why such a table could not be edited at all).
    var pkCols0 = d.data.primaryKey || [];
    bits.push(d.data.editable
      ? (pkCols0.length ? "editable — changes buffer until Commit" : "editable — " + (d.data.editNote || "rows are addressed by all columns"))
      : (d.data.editNote || "browsing only"));
    left.appendChild(el("div", "db-meta", bits.join("  ·  ")));
  } else if (dbIsRedis()) {
    left.appendChild(el("h2", "db-title pane-title", d.redisKey ? d.redisKey : "Keys"));
    var conn2 = d.conns.find(function (c) { return c.name === d.conn; });
    left.appendChild(el("div", "db-meta",
      (conn2 ? conn2.label : "") +
      (d.redis && d.redis.total != null ? " · " + Number(d.redis.total).toLocaleString() + " keys" : "") +
      " · browsing · edit values in place or run commands"));
  } else {
    left.appendChild(el("h2", "db-title pane-title", "Data"));
    left.appendChild(el("div", "db-meta", d.conn ? (d.table ? "Loading…" : "Select a table on the left") : "No database MCP registered"));
  }
  head.appendChild(left);

  var ctl = el("div", "db-head-ctl");
  var nosql = dbIsRedis();
  if (d.data && !d.sqlResult && !nosql) dbRenderTabs(ctl);
  if (d.sqlResult) {
    var back = el("button", "btn", dbIsRedis() ? "Back to keys" : "Back to table");
    back.onclick = function () {
      d.sqlResult = null;
      d.sqlResults = null; d.sqlTab = 0; // docs/22 W4.3: leaving the results closes every tab
      dbClearSel(); // "q"-prefixed query keys must not leak into the table grid
      renderDbToolbar(); renderDbGrid();
    };
    ctl.appendChild(back);
  } else if (d.data) {
    // Row-grid controls: page size, pager, refresh, insert, CSV. Rendered on EVERY tab but hidden
    // with visibility (keeps the width) off the Data tab, so the tab segment never shifts.
    var dataCtl = el("div", "db-data-ctl");
    if (d.tab !== "data") dataCtl.style.visibility = "hidden";
    var size = el("select", "db-pagesize");
    size.title = "Rows per page";
    DB_PAGE_SIZES.forEach(function (n) {
      var o = el("option", "", String(n));
      o.value = String(n);
      o.selected = n === d.pageSize;
      size.appendChild(o);
    });
    size.onchange = function () {
      if (!dbOkToDrop()) { this.value = String(d.pageSize); return; }
      d.pageSize = Number(this.value);
      d.offset = 0;
      dbDropEdits();
      dbLoadData(true);
    };
    dataCtl.appendChild(size);

    var first = d.offset + 1;
    var to = d.offset + d.data.rows.length;
    dataCtl.appendChild(el("span", "db-pageinfo",
      d.data.total ? first.toLocaleString() + "–" + to.toLocaleString() + " of " + d.data.total.toLocaleString() : "0 rows"));
    var prev = el("button", "btn icon");
    prev.innerHTML = icon("chevron-left");
    prev.title = "Previous page";
    prev.disabled = d.offset === 0;
    prev.onclick = function () {
      if (!dbOkToDrop()) return;
      d.offset = Math.max(0, d.offset - d.pageSize);
      dbDropEdits();
      dbLoadData(true);
    };
    var next = el("button", "btn icon");
    next.innerHTML = icon("chevron-right");
    next.title = "Next page";
    // docs/22 W1.9: the limit+1 probe answers "is there another page" from the rows actually
    // fetched; the old offset-vs-total arithmetic stays as the fallback when the flag is absent.
    next.disabled = d.data.nextPage != null ? !d.data.nextPage : to >= d.data.total;
    next.onclick = function () {
      if (!dbOkToDrop()) return;
      d.offset += d.pageSize;
      dbDropEdits();
      dbLoadData(true);
    };
    dataCtl.appendChild(prev);
    dataCtl.appendChild(next);

    var refresh = el("button", "btn", "Refresh");
    refresh.title = "Reload this page (drops buffered edits)";
    refresh.onclick = function () {
      if (!dbOkToDrop()) return;
      dbDropEdits();
      dbLoadData(true);
    };
    dataCtl.appendChild(refresh);

    if (d.data.editable) {
      var addRow = el("button", "btn", "+ Row");
      addRow.title = "Buffer a new row — inserted only on Commit";
      addRow.onclick = function () {
        d.inserts.push({ values: {} });
        renderDbGrid(); renderDbBar();
      };
      dataCtl.appendChild(addRow);
    }

    var csv = el("button", "btn", "CSV");
    csv.title = "Download the current page as CSV";
    csv.onclick = dbExportCsv;
    dataCtl.appendChild(csv);

    var exp = el("button", "btn", "Export…");
    exp.title = "Export the whole table as CSV, NDJSON, or SQL dump (capped at 100k rows)";
    exp.onclick = function (ev) {
      // stopPropagation FIRST: connect.js closes any open menu on clicks that reach document,
      // and without this the very click that opens the menu also tears it down.
      ev.stopPropagation();
      popupMenu(this.getBoundingClientRect(), [
        { label: "Export CSV…", fn: function () { dbExportTable(exp, "csv"); } },
        { label: "Export NDJSON…", fn: function () { dbExportTable(exp, "json"); } },
        { label: "Export SQL dump…", fn: function () { dbExportTable(exp, "sql"); } },
      ]);
    };
    dataCtl.appendChild(exp);

    if (d.data.editable) {
      var imp = el("button", "btn", "Import\u2026");
      imp.title = "Insert CSV rows in one transaction";
      imp.onclick = dbOpenImport;
      dataCtl.appendChild(imp);
    }
    ctl.appendChild(dataCtl);
  }
  var sql = el("button", "btn", d.sqlOpen ? (nosql ? "Hide Command" : "Hide SQL") : (nosql ? "Command" : "SQL"));
  sql.title = nosql ? "Run one command (SET, GET, DEL, HGETALL, TTL, TYPE…)" : "SQL console — one statement per run";
  sql.onclick = function () {
    d.sqlOpen = !d.sqlOpen;
    var con = $("dbConsole");
    if (con) con.hidden = !d.sqlOpen;
    sql.textContent = d.sqlOpen ? (nosql ? "Hide Command" : "Hide SQL") : (nosql ? "Command" : "SQL");
    if (d.sqlOpen && $("dbSql")) $("dbSql").focus();
  };
  ctl.appendChild(sql);
  head.appendChild(ctl);
}

/** docs/22 W1.4: fill the console with one generated stats statement and run it, so the SQL
 *  is on screen (and in history) rather than hidden behind a one-off request. */
function dbRunColumnStats(column, kind) {
  var d = state.db;
  var dialect = (d.conns.find(function (c) { return c.name === d.conn; }) || {}).dialect || "mysql";
  var sql;
  try {
    sql = dbStatsSql(dialect, d.schema, d.table, column, kind);
  } catch (err) { toast(String(err), true); return; }
  dbFillConsole(sql);
  void dbRunSql();
}

/** Paint one cell from its typed view (docs/22 W2.3). Returns the value's long-text title
 *  (the folded JSON's full text; null when the value needs none) so callers can merge it with
 *  their own hint instead of clobbering it. */
function dbPaintCell(td, v, has, colType) {
  if (!has || v === undefined) return null; // nothing set yet (an insert stub cell)
  if (v === null) { td.appendChild(el("span", "db-null", "NULL")); return null; }
  var view = dbCellView(v, colType);
  if (view.href) {
    var a = el("a", "db-link", view.text);
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

var dbTip = { node: null, timer: 0 };

function dbTipHide() {
  if (dbTip.node) { dbTip.node.remove(); dbTip.node = null; }
}

/** The header's hover card, in the panel's own chrome rather than the OS tooltip: name, the
 *  type/nullability line, the FULL comment (the header caption truncates; this never does) and
 *  the sort hint. Fixed under the th and clamped to the viewport; pointer-events none so it can
 *  never steal the hover it is explaining. */
function dbTipShow(th, c) {
  var tip = el("div", "db-tip");
  tip.appendChild(el("div", "t-name", c.name));
  tip.appendChild(el("div", "t-type", c.dataType + (c.nullable ? " · nullable" : " · not null") +
    (c.isPrimaryKey ? " · primary key" : "")));
  if (c.comment) tip.appendChild(el("div", "t-comment", c.comment));
  tip.appendChild(el("div", "t-hint", "Click sorts: ascending → descending → off"));
  document.body.appendChild(tip);
  var r = th.getBoundingClientRect();
  tip.style.left = Math.max(8, Math.min(r.left, window.innerWidth - tip.offsetWidth - 8)) + "px";
  tip.style.top = (r.bottom + 6) + "px";
  dbTip.node = tip;
}

// The card is fixed to the viewport, so a scrolled grid leaves it pointing nowhere — dismiss on
// any scroll, capture phase, once for the module's lifetime.
document.addEventListener("scroll", dbTipHide, true);


function renderDbGrid() {
  var d = state.db;
  var wrap = $("dbGridWrap");
  if (!wrap) return;
  var con = $("dbConsole");
  if (con) con.hidden = !d.sqlOpen;
  wrap.innerHTML = "";
  dbTipHide(); // a rebuilt grid invalidates any header card still open

  // docs/22 W4.2: conflict marks live exactly as long as the buffered change they name —
  // reverting the cell, dropping the buffer or reloading the table clears them (a new
  // commit answer re-sets or clears them in the fetch observer above).
  if (d.conflict) {
    var cu = d.updates && d.updates[d.conflict.key];
    var live = !!cu && d.conflict.columns.some(function (c) {
      return Object.prototype.hasOwnProperty.call(cu.changes, c);
    });
    if (!live) d.conflict = null;
  }

  if (d.sqlResult || d.sqlBusy) { renderDbResultGrid(wrap); return; }
  if (dbIsRedis()) { dbRenderRedisValue(wrap); return; }
  if (d.tab !== "data") { renderDbDetailGrid(wrap); return; }
  if (!d.conn) { wrap.appendChild(el("div", "db-hint", "No database MCP registered — add a mysql or pg MCP first.")); return; }
  if (!d.table || !d.data) {
    if (d.table) { wrap.appendChild(el("div", "db-hint", "Loading…")); return; }
    // The shared empty state (docs/18 V7); "Loading…" stays a quiet one-liner.
    wrap.innerHTML = emptyHtml({ icon: "database", title: "Select a table", hint: "Pick a table on the left to browse its rows." });
    return;
  }
  if (d.loading) { wrap.appendChild(el("div", "db-hint", "Loading…")); return; }

  var pkCols = d.data.primaryKey || [];
  var editable = d.data.editable;
  // docs/22 W2.1: the per-connection grid config picks the columns the table shows and the
  // width each dragged column keeps. A config older than a column set (columns added since)
  // simply misses those names — an unknown width is no width, an unknown hidden name hides
  // nothing.
  var cfg = d.gridCfg || { widths: {}, hidden: [] };
  var cols = dbGridVisibleColumns(d.data.columns, cfg.hidden);
  var widthOf = function (c, cell) {
    var w = cfg.widths[c.name];
    if (w) { cell.style.width = w + "px"; cell.style.maxWidth = w + "px"; }
  };

  var tbl = el("table", "db-grid");
  var thead = el("thead");
  var hr = el("tr");
  var keyOf = function (row, i) { return pkCols.length ? dbPkKey(pkCols, row) : String(i); };
  var thAll = el("th", "db-rowctl");
  var allOn = d.data.rows.length > 0 && d.data.rows.every(function (row, i) { return !!d.sel[keyOf(row, i)]; });
  var cbAll = document.createElement("input");
  cbAll.type = "checkbox";
  cbAll.className = "db-selbox";
  cbAll.checked = allOn;
  cbAll.title = "Select every row on this page (for copy)";
  cbAll.onclick = function (e) { e.stopPropagation(); };
  cbAll.onchange = function () { dbSelAll(this.checked); };
  thAll.appendChild(cbAll);
  hr.appendChild(thAll);
  // One caption line under every column name when ANY column carries a comment: the meaning is
  // then ON the grid instead of hidden behind a hover, and giving every header the line (empty
  // where there is nothing to say) keeps the header row one even height.
  var hasComments = cols.some(function (c) { return !!c.comment; });
  cols.forEach(function (c) {
    var sorted = c.name === d.order;
    var th = el("th", "db-col" + (sorted ? " db-sorted" + (d.dir === "desc" ? " db-sorted-desc" : "") : ""));
    var main = el("div", "db-col-main");
    main.appendChild(el("span", "db-col-name", c.name));
    if (c.isPrimaryKey) main.appendChild(el("span", "db-key", "⚿"));
    main.appendChild(el("span", "db-col-type", c.dataType));
    if (sorted) main.appendChild(el("span", "db-sort"));
    th.appendChild(main);
    if (hasComments) th.appendChild(el("div", "db-col-comment", c.comment || ""));
    // docs/22 W2.1: the resize grip hugs the header's right edge. It owns mousedown and click
    // so a drag neither sorts the column nor fights the hover card for the pointer.
    var grip = el("span", "db-col-grip");
    grip.title = "Drag to resize";
    grip.onmousedown = function (ev) {
      var idx = Array.prototype.indexOf.call(hr.children, th);
      var cells = [th];
      var trs = tbl.querySelectorAll("tbody tr");
      for (var r = 0; r < trs.length; r++) if (trs[r].children[idx]) cells.push(trs[r].children[idx]);
      dbColResizeStart(ev, c.name, th, cells);
    };
    grip.onclick = function (ev) { ev.stopPropagation(); };
    th.appendChild(grip);
    widthOf(c, th);
    th.onmouseenter = function () {
      clearTimeout(dbTip.timer);
      dbTip.timer = setTimeout(function () { dbTipShow(th, c); }, 260);
    };
    th.onmouseleave = function () { clearTimeout(dbTip.timer); dbTipHide(); };
    th.onclick = function () {
      dbTipHide();
      if (!dbOkToDrop()) return;
      var next = dbNextSort(d.order, d.dir, c.name);
      d.order = next ? next.order : null;
      d.dir = next ? next.dir : "asc";
      d.offset = 0;
      dbDropEdits();
      dbLoadData(true);
    };
    // docs/22 W1.4: the header's own right-click runs this column's stats — top values or
    // COUNT/MIN/MAX/AVG. The statement is generated behind the identifier gate and lands in
    // the console (visible, in history) with its result in the standing result grid.
    th.oncontextmenu = function (e) {
      e.preventDefault();
      dbTipHide();
      // docs/22 W2.1: hiding lives in the same menu, one separator down, and the recovery
      // entry ("Show all columns") appears exactly while something is hidden — one place for
      // both directions.
      var items = [
        { label: "Value distribution\u2026", fn: function () { dbRunColumnStats(c.name, "dist"); } },
        { label: "Numeric stats\u2026", fn: function () { dbRunColumnStats(c.name, "num"); } },
        { sep: true },
        { label: "Hide column", fn: function () { dbHideColumn(c.name); } },
      ];
      if (d.gridCfg && d.gridCfg.hidden.length) {
        items.push({ label: "Show all columns", fn: dbShowAllColumns });
      }
      popupMenu(this.getBoundingClientRect(), items);
    };
    hr.appendChild(th);
  });
  thead.appendChild(hr);
  tbl.appendChild(thead);

  var tbody = el("tbody");

  // docs/22 W2.1: a table with every column hidden still has to read as a place, not a
  // collapsed box — one quiet row says what happened and carries the way back. Rows and
  // buffered inserts are not drawn under it: with no visible column there is nothing to show
  // or edit. (The header keeps the select-all corner for shape.)
  if (!cols.length) {
    var trh = el("tr");
    var tdh = el("td", "db-grid-hiddenall");
    var inner = el("div");
    inner.appendChild(el("span", "", "Every column of " +
      (d.schema ? d.schema + "." : "") + d.table + " is hidden."));
    var showAll = el("button", "btn", "Show all columns");
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
  d.inserts.forEach(function (ins, i) {
    var tr = el("tr", "db-ins");
    var rc = el("td", "db-rowctl");
    var rm = el("button", "db-act", "✕");
    rm.title = "Remove this buffered insert";
    rm.onclick = function () {
      d.inserts.splice(i, 1);
      renderDbGrid(); renderDbBar();
    };
    rc.appendChild(rm);
    tr.appendChild(rc);
    cols.forEach(function (c, ci) {
      var has = Object.prototype.hasOwnProperty.call(ins.values, c.name);
      var td = el("td", "db-cell");
      widthOf(c, td);
      var long = dbPaintCell(td, has ? ins.values[c.name] : undefined, has, c.dataType);
      // docs/22 W2.2: one click puts the keyboard's focus cell here; the ring marks it.
      td.onmousedown = function () { dbFocusCell(i, ci); };
      if (d.focus && d.focus.r === i && d.focus.c === ci) td.classList.add("db-focus");
      if (editable) {
        td.classList.add("db-cell-edit");
        td.title = long || "Double-click to edit · right-click for dialog/copy";
        (function (col, present) {
          td.ondblclick = function () {
            var cur = present ? dbCellText(ins.values[col]) : "";
            dbEditCellEnter("insert", null, i, col, {}, td, cur == null ? "" : cur);
          };
          td.dbKbdEdit = function (seed) {
            var cur = seed != null ? seed : present ? dbCellText(ins.values[col]) : "";
            dbEditCellEnter("insert", null, i, col, {}, td, cur == null ? "" : cur);
          };
          td.oncontextmenu = function (e) { dbCellMenu(e, null, null, col, null, function () { dbOpenCellEditor("insert", null, i, col, {}); }); };
        })(c.name, has);
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });

  d.data.rows.forEach(function (row, rowIdx) {
    var key = keyOf(row, rowIdx);
    var deleted = !!d.deletes[key];
    var upd = d.updates[key];
    var tr = el("tr", (deleted ? "db-del " : "") + (d.sel[key] ? "db-sel" : ""));
    var rc = el("td", "db-rowctl");
    var cb = document.createElement("input");
    cb.type = "checkbox";
    cb.className = "db-selbox";
    cb.checked = !!d.sel[key];
    cb.title = "Select row for copy (Shift-click for a range)";
    cb.onclick = function (e) { e.stopPropagation(); };
    cb.onchange = function (ev) {
      if (ev.shiftKey && d.selAnchor >= 0 && d.selAnchor !== rowIdx) {
        var a = Math.min(d.selAnchor, rowIdx);
        var b2 = Math.max(d.selAnchor, rowIdx);
        for (var k = a; k <= b2; k++) d.sel[keyOf(d.data.rows[k], k)] = true;
      } else if (this.checked) d.sel[key] = true;
      else delete d.sel[key];
      d.selAnchor = rowIdx;
      renderDbToolbar(); renderDbGrid();
    };
    rc.appendChild(cb);
    if (editable) {
      var b = el("button", "db-act", deleted ? "↩" : "✕");
      b.title = deleted ? "Undo this buffered delete" : "Buffer a delete — applied only on Commit";
      b.onclick = function () {
        if (deleted) delete d.deletes[key];
        else {
          d.deletes[key] = dbRowAddr(pkCols, d.data.columns, row);
          delete d.updates[key]; // a deleted row's cell edits are moot
        }
        renderDbGrid(); renderDbBar();
      };
      rc.appendChild(b);
    }
    tr.appendChild(rc);
    cols.forEach(function (c, ci) {
      var orig = row[c.name];
      var pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name);
      var v = pending ? upd.changes[c.name] : orig;
      // docs/22 W4.2: the cells a lost commit race named stay red while their buffered
      // change is still pending — amber says "differs from the loaded row", red says
      // "the server refused this change; the row moved under it".
      var lost = pending && !!d.conflict && d.conflict.key === key && d.conflict.columns.indexOf(c.name) >= 0;
      var td = el("td", "db-cell" + (pending ? " db-dirty" : "") + (lost ? " db-conflict" : ""));
      widthOf(c, td);
      var long = dbPaintCell(td, v, true, c.dataType);
      // docs/22 W2.2: same focus wiring as insert cells; the row index counts the inserts above.
      var gridRow = d.inserts.length + rowIdx;
      td.onmousedown = function () { dbFocusCell(gridRow, ci); };
      if (d.focus && d.focus.r === gridRow && d.focus.c === ci) td.classList.add("db-focus");
      if (editable && !deleted) {
        td.classList.add("db-cell-edit");
        td.title = long || "Double-click to edit · right-click for dialog/copy";
        (function (col, orig) {
          var meta = { pk: dbRowAddr(pkCols, d.data.columns, row), orig: orig };
          td.ondblclick = function () {
            var cur = dbCellText(orig);
            dbEditCellEnter("update", key, -1, col, meta, td, cur == null ? "" : cur);
          };
          td.dbKbdEdit = function (seed) {
            var cur = seed != null ? seed : dbCellText(orig);
            dbEditCellEnter("update", key, -1, col, meta, td, cur == null ? "" : cur);
          };
          td.oncontextmenu = function (e) {
            dbCellMenu(e, row, key, col, function () { dbOpenCellEditor("update", key, -1, col, meta); });
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
  // the scrolling wrap (not above it) so focusing it never scrolls the pane away, and it owns
  // keydown + paste — arrows/Enter/F2/Esc/Home/End/typing, Ctrl+C, and TSV paste.
  var kbd = el("input", "db-kbd");
  kbd.type = "text";
  kbd.id = "dbKbd";
  kbd.tabIndex = -1;
  kbd.setAttribute("aria-label", "Data grid keyboard navigation");
  kbd.onkeydown = function (ev) {
    var d = state.db;
    if (!d.data) return;
    var maxR = dbGridRowsCount() - 1;
    var maxC = cols.length - 1;
    if (!d.focus) {
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
      var m = dbKbdMove(d.focus.r, d.focus.c, ev.key, maxR, maxC);
      d.focus = m;
      renderDbGrid();
      var k = $("dbKbd");
      if (k) k.focus();
      var td = document.querySelector("#dbGridWrap td.db-focus");
      if (td && td.scrollIntoView) td.scrollIntoView({ block: "nearest", inline: "nearest" });
      return;
    }
    if (ev.key === "Enter" || ev.key === "F2") {
      ev.preventDefault();
      dbFocusEdit();
      return;
    }
    if (ev.key === "Escape") {
      // Esc's one meaning here is "put the keyboard down" — the cell keeps its value and its
      // ring goes away. (The inline editor and the sheet handle their own Esc first.)
      ev.preventDefault();
      d.focus = null;
      renderDbGrid();
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
  kbd.onpaste = function (ev) {
    var d = state.db;
    if (!d.data) return;
    var text = ev.clipboardData ? ev.clipboardData.getData("text/plain") : "";
    if (!text) return;
    ev.preventDefault();
    dbPasteApply(text, d.focus ? d.focus.r : 0, d.focus ? d.focus.c : 0);
  };
  wrap.appendChild(kbd);
}

function renderDbResultGrid(wrap) {
  var d = state.db;
  if (d.sqlBusy) { wrap.appendChild(el("div", "db-hint", "Running…")); return; }
  var res = d.sqlResult;
  if (!res) { wrap.appendChild(el("div", "db-hint", "Run a query to see rows here.")); return; }
  var tab = d.sqlTab || 0; // docs/22 W4.3: this grid is one tab of the strip — its selection keys are that tab's
  var tbl = el("table", "db-grid");
  var thead = el("thead");
  var hr = el("tr");
  var thAll2 = el("th", "db-rowctl");
  var allOn2 = res.rows.length > 0 && res.rows.every(function (_, i) { return !!d.sel[dbResultKey(tab, i)]; });
  var cbAll2 = document.createElement("input");
  cbAll2.type = "checkbox";
  cbAll2.className = "db-selbox";
  cbAll2.checked = allOn2;
  cbAll2.title = "Select every result row (for copy)";
  cbAll2.onclick = function (e) { e.stopPropagation(); };
  cbAll2.onchange = function () { dbSelAll(this.checked); };
  thAll2.appendChild(cbAll2);
  hr.appendChild(thAll2);
  res.columns.forEach(function (c) { hr.appendChild(el("th", "db-col", c)); });
  thead.appendChild(hr);
  tbl.appendChild(thead);
  var tbody = el("tbody");
  res.rows.forEach(function (row, i) {
    var qkey = dbResultKey(tab, i);
    var tr = el("tr", d.sel[qkey] ? "db-sel" : "");
    var rc2 = el("td", "db-rowctl");
    var cb2 = document.createElement("input");
    cb2.type = "checkbox";
    cb2.className = "db-selbox";
    cb2.checked = !!d.sel[qkey];
    cb2.title = "Select row for copy (Shift-click for a range)";
    cb2.onclick = function (e) { e.stopPropagation(); };
    cb2.onchange = function (ev) {
      if (ev.shiftKey && d.selAnchor >= 0 && d.selAnchor !== i) {
        var a2 = Math.min(d.selAnchor, i);
        var b3 = Math.max(d.selAnchor, i);
        for (var k2 = a2; k2 <= b3; k2++) d.sel[dbResultKey(tab, k2)] = true;
      } else if (this.checked) d.sel[qkey] = true;
      else delete d.sel[qkey];
      d.selAnchor = i;
      renderDbToolbar(); renderDbGrid();
    };
    rc2.appendChild(cb2);
    rc2.appendChild(document.createTextNode(String(i + 1)));
    tr.appendChild(rc2);
    res.columns.forEach(function (c) {
      var td = el("td", "db-cell");
      td.oncontextmenu = function (e) { dbResultCellMenu(e, row, c); };
      // No column types on a result grid: dbCellView still words booleans, folds long JSON,
      // links URLs and right-aligns JS numbers off the value alone.
      var v = row[c];
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
function dbNextSort(order, dir, name) {
  if (order !== name) return { order: name, dir: "asc" };
  if (dir === "desc") return null;
  return { order: name, dir: "desc" };
}

export { DB_COL_MAX, DB_COL_MIN, dbColResizeStart, dbCopyChecked, dbFocusCell, dbGridConfigKey, dbGridConfigLoad, dbGridConfigParse, dbGridConfigSave, dbGridVisibleColumns, dbHideColumn, dbKbdMove, dbLoadData, dbNextSort, dbPaintCell, dbPasteApply, renderDbGrid, renderDbResultGrid, renderDbToolbar, dbShowAllColumns, dbTsvRows };
