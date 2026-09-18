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

                                                                      
import { $, apiJson, el, errText, esc, state, targetEl, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbApplyFilters, renderDbFilters } from "./data-filters.js";
import { dbOpenValueSheet } from "./data-value.js";
import { dbClearSel, dbDropEdits, dbOkToDrop, dbPending, dbPkKey, dbResultKey } from "./data-view.js";
import { clampMenuPos } from "./menu.js";
import { setMenuOpen } from "./ui-state.js";

/* --- CSV import wizard -------------------------------------------------------------------------- */
/* Paste or upload CSV, map its columns to table columns, preview the first rows, then commit.
   The batch runs server-side in ONE transaction; a failure rolls the whole file back. The
   Insert/Upsert segmented control picks the statement form (docs/22 W4.5): Insert keeps the
   plain INSERT (a duplicate key aborts the file), Upsert maps conflicts onto existing rows. */
function dbParseCsvLine(line        )           {
  const out           = []; let cur = "", inQ = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (inQ) {
      if (c === String.fromCharCode(34) && line[i + 1] === String.fromCharCode(34)) { cur += c; i++; }
      else if (c === String.fromCharCode(34)) inQ = false;
      else cur += c;
    } else {
      if (c === ",") { out.push(cur); cur = ""; }
      else if (c === String.fromCharCode(34) && cur === "") inQ = true;
      else cur += c;
    }
  }
  out.push(cur);
  return out;
}

function dbOpenImport()       {
  const d = state.db;
  const d_ = d ;
  if (!d_.conn || !d_.table || !d_.data) { toast("Open a table first", true); return; }
  if (!d_.data.editable) { toast("This table is not editable (" + (d_.data.editNote || "no primary key") + ")", true); return; }
  if (dbPending() && !dbOkToDrop()) return;
  let header           = [], lines           = [], mapping                    = [];
  let mode = "insert"; // docs/22 W4.5: "insert" | "upsert" — the statement form the commit uses
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Import CSV">' +
      '<div class="sheet-head"><h2>Import CSV into ' + esc((d_.schema ? d_.schema + "." : "") + d_.table ) + "</h2></div>" +
      '<div class="sheet-body">' +
        '<div class="db-console-row" style="margin-bottom:var(--s2)">' +
          '<div class="seg" role="tablist" id="dbImpMode" style="margin-bottom:0">' +
            '<button type="button" role="tab" data-mode="insert" aria-selected="true">Insert</button>' +
            '<button type="button" role="tab" data-mode="upsert" aria-selected="false">Upsert</button>' +
          "</div>" +
          '<span class="hint" id="dbImpModeSay">Every row inserts \u2014 a duplicate key aborts the whole file.</span>' +
        "</div>" +
        '<div class="db-console-row" style="margin-bottom:var(--s2)">' +
          '<input type="file" id="dbImpFile" accept=".csv,text/csv" style="width:auto">' +
          '<span class="hint">…or paste below (first row = header)</span>' +
        "</div>" +
        '<textarea id="dbImpText" placeholder="id,name\n1,alice\n2,bob" style="min-height:120px"></textarea>' +
        '<div id="dbImpMap" style="margin-top:var(--s3)"></div>' +
        '<div id="dbImpPreview" style="margin-top:var(--s3)"></div>' +
      "</div>" +
      '<div class="sheet-foot"><span class="grow"></span>' +
        '<button class="btn" id="dbImpCancel">Cancel</button>' +
        '<button class="btn primary" id="dbImpRun">Import (one transaction)</button></div>' +
    "</div>";
  $("sheet").hidden = false;

  function parse()       {
    const text = $                  ("dbImpText").value.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
    const rows = text.split("\n").filter((l        )          => { return l.trim() !== ""; });
    if (!rows.length) { header = []; lines = []; mapping = []; paint(); return; }
    header = dbParseCsvLine(rows[0]);
    lines = rows.slice(1, 10001); // cap mirrors IMPORT_ROW_CAP
    // default mapping: match by name, skip otherwise
    const names = (d?.data?.columns.map((c             )         => { return c.name; }) ?? []);
    mapping = header.map((h        )                => { return names.indexOf(h) >= 0 ? h : null; });
    paint();
  }

  function paint()       {
    const mapBox = $("dbImpMap");
    mapBox.innerHTML = "";
    if (!header.length) return;
    const names = (d?.data?.columns.map((c             )         => { return c.name; }) ?? []);
    header.forEach((h        , i        )       => {
      const row = el("div", "db-console-row");
      row.style.marginBottom = "2px";
      row.appendChild(el("span", "db-filter-hint", "CSV " + String.fromCharCode(34) + h + String.fromCharCode(34) + " \u2192 "));
      const sel = el("select");
      sel.style.width = "auto";
      const skip = el("option", "", "(skip)")                     ;
      skip.value = "";
      sel.appendChild(skip);
      names.forEach((n        )       => {
        const o = el("option", "", n)                     ;
        o.value = n;
        o.selected = n === mapping[i];
        sel.appendChild(o);
      });
      sel.onchange = (e) => { mapping[i] = (e.currentTarget                     ).value || null; preview(); };
      row.appendChild(sel);
      mapBox.appendChild(row);
    });
    preview();
  }

  function preview()       {
    const box = $("dbImpPreview");
    box.innerHTML = "";
    if (!header.length) return;
    const mapped         = mapping.filter(Boolean).length;
    if (!mapped) { box.appendChild(el("div", "hint", "No columns mapped — pick at least one target column.")); return; }
    const sample = lines.slice(0, 3).map((l        )                          => {
      const cells = dbParseCsvLine(l);
      const o                          = {};
      mapping.forEach((m               , i        )       => { if (m) o[m] = cells[i] === "" ? null : cells[i]; });
      return o;
    });
    box.appendChild(el("div", "hint", lines.length.toLocaleString() + " row" + (lines.length > 1 ? "s" : "") +
      " \u00b7 " + mapped + " of " + header.length + " CSV columns mapped. First rows as they will be inserted:"));
    const pre = el("pre", "db-ddl");
    pre.style.position = "static";
    pre.style.margin = "var(--s1) 0 0";
    pre.textContent = sample.map((o                         )         => { return JSON.stringify(o); }).join("\n");
    box.appendChild(pre);
  }

  $                  ("dbImpText").oninput = parse;
  function setMode(m        )       {
    mode = m;
    $("dbImpMode").querySelectorAll("button").forEach((b) => {
      b.setAttribute("aria-selected", String(b.dataset.mode === m));
    });
    // One sentence beside the control names the cost of the picked mode (docs/22 W4.5).
    $("dbImpModeSay").textContent = m === "upsert"
      ? "Rows that match an existing key update it; the rest insert \u2014 still one transaction."
      : "Every row inserts \u2014 a duplicate key aborts the whole file.";
  }
  $("dbImpMode").onclick = (e            )       => {
    const b                     = targetEl(e)?.closest             ("button[data-mode]") ?? null;
    if (b) setMode(b.dataset.mode );
  };
  $                  ("dbImpFile").onchange = (e) => {
    const f = (e.currentTarget                    ).files;
    if (!f || !f[0]) return;
    const rd = new FileReader();
    rd.onload = ()       => { $                  ("dbImpText").value = String(rd.result); parse(); };
    rd.readAsText(f[0]);
  };
  $("dbImpCancel").onclick = closeSheet;
  $                   ("dbImpRun").onclick = async (e)                => {
    if (!header.length || !lines.length) { toast("Paste or upload a CSV first", true); return; }
    if (!mapping.some(Boolean)) { toast("Map at least one column", true); return; }
    const upsert = mode === "upsert";
    const d_ = d ;
    if (!confirm((upsert ? "Upsert " : "Insert ") + lines.length.toLocaleString() + " rows into " +
        (d_.schema ? d_.schema + "." : "") + d_.table  + " in ONE transaction? A failure rolls the whole file back.")) return;
    const t = e.currentTarget                     ;
    t.disabled = true;
    t.textContent = "Importing\u2026";
    const j = await apiJson                                     ("/api/db/" + encodeURIComponent(d_.conn ) + "/import", {
      method: "POST",
      // mode rides the payload only when upsert — a default import stays byte-identical to
      // what a pre-W4.5 panel sent (docs/22 W4.5).
      body: JSON.stringify(Object.assign(
        { table: d_.table, schema: d_.schema, header: header, lines: lines, mapping: mapping },
        upsert ? { mode: "upsert" } : {}
      )),
    });
    const btn = $                   ("dbImpRun");
    if (btn) { btn.disabled = false; btn.textContent = "Import (one transaction)"; }
    if (!j) return; // server rolled back; the sheet stays for fixing
    // j.note is the server's degrade explanation (a Postgres table with no primary key); the
    // count is still the truth, the sentence beside it says what actually ran.
    toast("Imported " + j.inserted + " row" + (j.inserted > 1 ? "s" : "") + (j.note ? " \u2014 " + j.note : ""));
    closeSheet();
    dbDropEdits();
    void dbLoadData(true);
  };
  $("sheet").onclick = (e            )       => { if (e.target === $("sheet")) closeSheet(); };
}
/* --- copy to clipboard -------------------------------------------------------------------------- */
/* Right-click a cell: copy the value, or the whole row as JSON / CSV / INSERT. Values are taken
   from the CURRENT BUFFER when the cell has a pending edit, so what you copy is what you see. */
function dbCopyText(text        )       {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(()       => { toast("Copied"); }, ()       => { dbCopyFallback(text); });
  } else dbCopyFallback(text);
}

function dbCopyFallback(text        )       {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); toast("Copied"); } catch (e) { toast("Copy failed", true); }
  ta.remove();
}

function dbRowForCopy(row                         , key        )                          {
  const d = state.db;
  const d_ = d ;
  const upd = d_.updates[key];
  const out                          = {};
  d_.data?.columns.forEach((c             )       => {
    out[c.name] = upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name) ? upd.changes[c.name] : row[c.name];
  });
  return out;
}

function dbCopyCsvCell(v         )         {
  if (v === null || v === undefined) return "";
  const s = typeof v === "object" ? JSON.stringify(v) : String(v);
  return String.fromCharCode(34) + s.split(String.fromCharCode(34)).join(String.fromCharCode(34) + String.fromCharCode(34)) + String.fromCharCode(34);
}

/** docs/22 W1.5: one filter straight from a cell value — push it, paint the row, apply.
 * docs/22 closeout B6: a REFUSED discard takes the pushed row back — a filter the user just
 * said no to must not stay on screen as if it had been accepted. */
function dbPushCellFilter(column        , op        , value         )       {
  const d = state.db;
  const d_ = d ;
  d_.filters.push({ column: column, op: op, value: value                                     });
  renderDbFilters();
  if (!dbApplyFilters()) { d_.filters.pop(); renderDbFilters(); }
}

function dbCellMenu(e            , row                                , key        , column        , editInDialog                     )       {
  e.preventDefault();
  const d = state.db;
  const d_ = d ;
  if (!d_.data) return;
  const upd = d_.updates[key];
  const pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, column);
  const value = pending ? upd.changes[column] : row ? row[column] : undefined;
  const full = row ? dbRowForCopy(row, key) : null;
  const names = d_.data?.columns.map((c             )         => { return c.name; });
  const dialect = (d_.conns.find((c                    )          => { return c.name === d?.conn; }) || {}                        ).dialect || "mysql";

  const menu = el("div", "ctx-menu");
  function item(label        , fn            )       {
    const b = el("button", "", label)                     ;
    b.onclick = ()       => { closeMenu2(); fn(); };
    menu.appendChild(b);
  }
  item("Copy value", ()       => { dbCopyText(value === null || value === undefined ? "NULL" : String(value)); });
  // docs/22 W5.3: the read-only viewer — full text, a JSON tree, hex or the link. NULL has
  // no content to view, so it gets no item (same rule as the filter items below).
  if (value !== null && value !== undefined) {
    item("View value\u2026", ()       => {
      const d_ = d ;
      dbOpenValueSheet(column, value, (d_.schema ? d_.schema + "." : "") + d_.table );
    });
  }
  if (editInDialog) {
    item("Edit in dialog\u2026", editInDialog); // long text / JSON: the user-chosen dialog path
  }
  // docs/22 W1.5: filter-by-value straight off a cell. NULL cells show none of these (there is
  // no value to equal); the pushed filter lands in the standing filter row like a typed one.
  if (!d_.sqlResult && row && value !== null && value !== undefined) {
    item("Filter = value", ()       => { dbPushCellFilter(column, "eq", value); });
    item("Filter \u2260 value", ()       => { dbPushCellFilter(column, "ne", value); });
    item("Filter contains", ()       => { dbPushCellFilter(column, "like", value); });
  }
  // Checked-row copies live in the SAME menu — one right-click reaches every format.
  if (!d_.sqlResult) {
    const hint = dbAppendSelItems(item, dbSelectedForCopy());
    if (hint) menu.appendChild(hint);
    item("Select all on page", ()       => { dbSelAll(true); });
    if (Object.keys(d_.sel).length) item("Clear selection", ()       => { dbSelAll(false); });
  }
  if (full) {
    item("Copy row as JSON", ()       => { dbCopyText(JSON.stringify(full, null, 2)); });
    item("Copy row as CSV", ()       => { dbCopyText(names.map((n        )         => { return dbCopyCsvCell(full?.[n]); }).join(",")); });
    item("Copy row as INSERT", ()       => {
      const d_ = d ;
      try {
        const q = (n        )         => { return dialect === "mysql" ? "`" + n + "`" : String.fromCharCode(34) + n + String.fromCharCode(34); };
        const cols = names.filter((n        )          => { return full [n] !== undefined; });
        const lit = (v         )         => {
          if (v === null || v === undefined) return "NULL";
          if (typeof v === "number" || typeof v === "boolean") return String(v);
          const qq = String.fromCharCode(39);
          return qq + String(v).split(qq).join(qq + qq) + qq;
        };
        const table = (d_.schema ? q(d_.schema) + "." : "") + q(d_.table );
        dbCopyText("INSERT INTO " + table + " (" + cols.map(q).join(", ") + ") VALUES (" +
          cols.map((n        )         => { return lit(full?.[n]); }).join(", ") + ");");
      } catch (err) { toast(errText(err), true); }
    });
  }
  document.body.appendChild(menu);
  // The cursor point is the anchor, clamped to the viewport (docs/22 closeout audit): a
  // right-click at the right or bottom edge used to strand the menu off-screen. Measured
  // AFTER the append — offsetWidth is zero until the menu is in the document.
  const box1 = menu.getBoundingClientRect();
  const pos1 = clampMenuPos({ left: e.clientX, top: e.clientY, bottom: e.clientY }, box1.width, box1.height, window.innerWidth, window.innerHeight);
  menu.style.left = pos1.left + "px";
  menu.style.top = pos1.top + "px";
  setMenuOpen(true);
  function closeMenu2() { menu.remove(); setMenuOpen(false); }
  setTimeout(() => {
    document.addEventListener("mousedown", function h(ev) {
      if (!menu.contains(ev.target        )) { menu.remove(); setMenuOpen(false); document.removeEventListener("mousedown", h); }
    });
  }, 0);
}

/* --- multi-row copy ------------------------------------------------------------------------------ */
/* Checked rows (the rowctl checkboxes) copied to the clipboard in a paste-anywhere format.
   The row list mirrors what the grid SHOWS: pending buffered edits ride along, deleted-buffered
   rows keep their original values. For a query-result grid the keys are dbResultKey(tab, index) —
   the ACTIVE tab's namespace (docs/22 W4.3), so a copy never crosses tabs. */
function dbSelectedForCopy()                                                      {
  const d = state.db;
  const d_ = d ;
  if (d_.sqlResult) {
    const tab = d_.sqlTab || 0;
    const qrows                            = [];
    d_.sqlResult.rows.forEach((r                         , i        )       => { if (d?.sel[dbResultKey(tab, i)]) qrows.push(r); });
    return { cols: d_.sqlResult.columns, rows: qrows };
  }
  if (!d_.data) return { cols: [], rows: [] };
  const pkCols = d_.data.primaryKey || [];
  const cols = d_.data.columns.map((c             )         => { return c.name; });
  const rows                            = [];
  d_.data.rows.forEach((row                         , i        )       => {
    const key = pkCols.length ? dbPkKey(pkCols, row) : String(i);
    if (d?.sel[key]) rows.push(dbRowForCopy(row, key));
  });
  return { cols: cols, rows: rows };
}

function dbFlatCell(v         )         {
  if (v === null || v === undefined) return "";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

function dbRowsCsv(sel                                                     )         {
  const lines = [sel.cols.map(dbCopyCsvCell).join(",")];
  sel.rows.forEach((r                         )       => {
    lines.push(sel.cols.map((c        )         => { return dbCopyCsvCell(r[c]); }).join(","));
  });
  return lines.join("\n");
}

function dbRowsTsv(sel                                                     )         {
  const cell = (v         )         => { return dbFlatCell(v).replace(/\t/g, " ").replace(/\r?\n/g, " "); };
  const lines = [sel.cols.join("\t")];
  sel.rows.forEach((r                         )       => {
    lines.push(sel.cols.map((c        )         => { return cell(r[c]); }).join("\t"));
  });
  return lines.join("\n");
}

function dbRowsMarkdown(sel                                                     )         {
  const cell = (v         )         => { return dbFlatCell(v).replace(/\|/g, "\\|").replace(/\r?\n/g, " "); };
  const lines = ["| " + sel.cols.join(" | ") + " |"];
  lines.push("|" + sel.cols.map(()         => { return " --- |"; }).join(""));
  sel.rows.forEach((r                         )       => {
    lines.push("| " + sel.cols.map((c        )         => { return cell(r[c]); }).join(" | ") + " |");
  });
  return lines.join("\n");
}

function dbRowsJson(sel                                                     )         { return JSON.stringify(sel.rows, null, 2); }

/** Tick or clear every row on screen (the header checkbox). Keys match dbSelectedForCopy. */
function dbSelAll(on         )       {
  const d = state.db;
  const d_ = d ;
  if (d_.sqlResult) {
    const tab = d_.sqlTab || 0;
    d_.sqlResult.rows.forEach((r                         , i        )       => {
      const k = dbResultKey(tab, i);
      const d_ = d ;
      if (on) d_.sel[k] = true; else delete d_.sel[k];
    });
  } else if (d_.data) {
    const pkCols = d_.data?.primaryKey || [];
    d_.data?.rows.forEach((row                         , i        )       => {
      const key = pkCols.length ? dbPkKey(pkCols, row) : String(i);
      const d_ = d ;
      if (on) d_.sel[key] = true; else delete d_.sel[key];
    });
  }
  d_.selAnchor = -1;
  renderDbToolbar(); renderDbGrid();
}

function dbAppendSelItems(item                                         , sel                                                     )                     {
  const n = sel.rows.length;
  if (!n) {
    const hint = el("button", "ctx-hint", "No rows checked \u2014 tick boxes on the left")                     ;
    hint.disabled = true;
    // item() only makes enabled buttons; the hint is appended by the caller's menu directly
    return hint;
  }
  const noun = n + " checked row" + (n > 1 ? "s" : "");
  item("Copy " + noun + " as CSV", ()       => { dbCopyText(dbRowsCsv(sel)); });
  item("Copy " + noun + " as TSV", ()       => { dbCopyText(dbRowsTsv(sel)); });
  item("Copy " + noun + " as Markdown table", ()       => { dbCopyText(dbRowsMarkdown(sel)); });
  item("Copy " + noun + " as JSON", ()       => { dbCopyText(dbRowsJson(sel)); });
  return null;
}

/* Right-click a QUERY-RESULT cell: the value copy plus the same checked-row section.
   The row's checkbox can also be toggled from here — right-click needs no mouse travel. */
function dbResultCellMenu(e            , row                                , column        )       {
  e.preventDefault();
  const d = state.db;
  const d_ = d ;
  const i = d_.sqlResult ? d_.sqlResult.rows.indexOf(row ) : -1;
  const menu = el("div", "ctx-menu");
  function item(label        , fn            )       {
    const b = el("button", "", label)                     ;
    b.onclick = ()       => { closeMenu(); fn(); };
    menu.appendChild(b);
  }
  const v = row ? row[column] : undefined;
  item("Copy value", ()       => { dbCopyText(v === null || v === undefined ? "NULL" : String(v)); });
  // docs/22 W5.3: the same read-only viewer on a console-result cell (no column types there —
  // the value alone picks the presentation, and the \\x wire form still says hex).
  if (v !== null && v !== undefined) {
    item("View value\u2026", ()       => { dbOpenValueSheet(column, v, "SQL result"); });
  }
  if (i >= 0) {
    const rk = dbResultKey(d_.sqlTab || 0, i);
    const on = !!d_.sel[rk];
    item(on ? "Uncheck this row" : "Check this row", ()       => {
      const d_ = d ;
      if (on) delete d_.sel[rk]; else d_.sel[rk] = true;
      d_.selAnchor = i;
      renderDbToolbar(); renderDbGrid();
    });
  }
  const hint = dbAppendSelItems(item, dbSelectedForCopy());
  if (hint) menu.appendChild(hint);
  item("Select all on page", ()       => { dbSelAll(true); });
  if (Object.keys(d_.sel).length) item("Clear selection", ()       => { dbSelAll(false); });
  document.body.appendChild(menu);
  // Same clamp as dbCellMenu (docs/22 closeout audit) — measured after the append.
  const box2 = menu.getBoundingClientRect();
  const pos2 = clampMenuPos({ left: e.clientX, top: e.clientY, bottom: e.clientY }, box2.width, box2.height, window.innerWidth, window.innerHeight);
  menu.style.left = pos2.left + "px";
  menu.style.top = pos2.top + "px";
  setMenuOpen(true);
  function closeMenu() { menu.remove(); setMenuOpen(false); }
  setTimeout(() => {
    document.addEventListener("mousedown", function h(ev) {
      if (!menu.contains(ev.target        )) { menu.remove(); setMenuOpen(false); document.removeEventListener("mousedown", h); }
    });
  }, 0);
}

/* --- CSV export of whatever the grid is showing --------------------------------------------------- */

function dbExportCsv()       {
  const d = state.db;
  let cols          , rows                           , name        ;
  const d_ = d ;
  if (d_.sqlResult) {
    cols = d_.sqlResult.columns;
    rows = d_.sqlResult.rows;
    name = "query.csv";
  } else if (d_.data) {
    cols = d_.data.columns.map((c             )         => { return c.name; });
    rows = d_.data.rows;
    name = d_.data.table + ".csv";
  } else return;
  function cell(v         )         {
    if (v === null || v === undefined) return "";
    const s = typeof v === "object" ? JSON.stringify(v) : String(v);
    return '"' + s.replace(/"/g, '""') + '"';
  }
  const lines = [cols.map((c        )         => { return '"' + String(c).replace(/"/g, '""') + '"'; }).join(",")];
  rows.forEach((r                         )       => {
    lines.push(cols.map((c        )         => { return cell(r[c]); }).join(","));
  });
  const blob = new Blob(["\uFEFF" + lines.join("\r\n")], { type: "text/csv;charset=utf-8" });
  const a = document.createElement("a")                     ;
  a.href = URL.createObjectURL(blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => { URL.revokeObjectURL(a.href); }, 1000);
}

export { dbCellMenu, dbCopyCsvCell, dbCopyFallback, dbCopyText, dbExportCsv, dbOpenImport, dbParseCsvLine, dbPushCellFilter, dbResultCellMenu, dbRowForCopy, dbSelAll, dbSelectedForCopy };
