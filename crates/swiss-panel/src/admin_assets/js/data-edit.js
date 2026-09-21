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

                                                                      
                                                   
import { $, apiJson, el, errText, toast } from "./util.js";
import { dbIsRedis, dbLoadKeys } from "./data-browsers.js";
import { dbOpenCellEditor } from "./data-cell.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { renderDbFilters } from "./data-filters.js";
import { dbFillConsole, dbTemplateSql, renderDbBar } from "./data-sql.js";
import { dbDropEdits, dbLoadTables, renderDbTables } from "./data-view.js";
// The strip's policy module: DROP closes the tabs the dropped table owned. Same accepted cycle
// shape as the rest of the data-* edges — the call crosses inside a function, never at module
// scope.
import { dbDropTableTabs } from "./data-tabs.js";
import { clampMenuPos } from "./menu.js";
import { setMenuOpen } from "./ui-state.js";
import { dbConn, dbTab } from "./db-state.js";
import { tr } from "./i18n.js";

/* --- structure operations (rename / truncate / drop) ---------------------------------------------- */
/* A Table menu beside the tabs. Truncate and drop demand a TYPED confirmation — the user
   retypes the table name — because both destroy data with no transaction to roll back to. */
/** docs/22 W1.10: build one template for the open table and drop it into the console. */
function dbGenerateSql(kind        )       {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!d.data || !d.data.columns || !d.data.columns.length) {
    toast(tr("dataEdit.openTableFirstTemplate"), true);
    return;
  }
  const dialect = (c.conns.find((x                    )          => { return x.name === c.conn; }) || {}                        ).dialect || "mysql";
  let sql;
  try {
    sql = dbTemplateSql(kind, dialect, d.schema , d.table ,
      d.data?.columns.map((col             )         => { return col.name; }), d.data?.primaryKey || []);
  } catch (err) { toast(errText(err), true); return; }
  dbFillConsole(sql);
}

function dbTableMenu(anchorEl             )       {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!c.conn || !d.table) return;
  const menu = el("div", "ctx-menu");
  function item(label        , fn            )       {
    const b = el("button", "", label)                     ;
    b.onclick = () => { closeMenu2(); fn(); };
    menu.appendChild(b);
  }
  // docs/22 W1.10: generate this table's four statements from the column set the page already
  // carries (the describe_table shape). Identifiers pass the whitelist, values are ?
  // placeholders, and the template lands in the console — fill the ?s, run, and it is history.
  ["select", "insert", "update", "delete"].forEach((kind        )       => {
    item(tr("dataEdit.generateKind", { kind: kind.toUpperCase() }), () => { dbGenerateSql(kind); });
  });
  menu.appendChild(document.createElement("hr"));
  item(tr("dataEdit.renameTable"), () => {
    const to = prompt(tr("dataEdit.renameTo", { name: (d.schema ? d.schema + "." : "") + d.table }), d.table );
    if (!to || to === d.table) return;
    if (!/^[A-Za-z0-9_$]{1,64}$/.test(to)) { toast(tr("dataEdit.validTableName"), true); return;}
    void dbRunDdl("rename", to);
  });
  item(tr("dataEdit.truncateTable"), () => {
    dbTypedConfirm({ what: tr("dataEdit.whatTruncate"), name: (d.schema ? d.schema + "." : "") + d.table, kind: "table", typed: d.table }, ()       => { void dbRunDdl("truncate"); });
  });
  item(tr("dataEdit.dropTable"), () => {
    dbTypedConfirm({ what: tr("dataEdit.whatDrop"), name: (d.schema ? d.schema + "." : "") + d.table, kind: "table", typed: d.table }, ()       => { void dbRunDdl("drop"); });
  });
  document.body.appendChild(menu);
  // docs/22 closeout audit: the Table menu now clamps to the viewport like popupMenu — a
  // button near the bottom edge used to drop its menu off-screen. Measured after the append.
  const r = anchorEl.getBoundingClientRect();
  const box = menu.getBoundingClientRect();
  const pos = clampMenuPos(r, box.width, box.height, window.innerWidth, window.innerHeight);
  menu.style.left = pos.left + "px";
  menu.style.top = pos.top + "px";
  setMenuOpen(true);
  function closeMenu2()       { menu.remove(); setMenuOpen(false); }
  setTimeout(() => {
    document.addEventListener("mousedown", function h(ev) {
      if (!menu.contains(ev.target        )) { menu.remove(); setMenuOpen(false); document.removeEventListener("mousedown", h); }
    });
  }, 0);
}

/* The typed-name confirm the destructive acts share (a W1 audit follow-up): the prompt
   names the act and the exact target, the operator types the name back, and anything else
   leaves the world untouched. Parameterized, not table-shaped — the redis key delete reuses
   it word for word. `typed` is what must be typed and defaults to `name`: the table flavor
   SHOWS a qualified name but demands the bare one; the key flavor's display name is the
   thing itself. */
function dbTypedConfirm(o                                                                     , fn            )       {
  const typed = prompt(tr("dataEdit.typedConfirm", { what: o.what, name: o.name, kind: o.kind }), "");
  if (typed !== (o.typed != null ? o.typed : o.name)) {
    if (typed !== null) toast(tr("dataEdit.nameMatchNothingDone"), true);
    return;
  }
  fn();
}

async function dbRunDdl(op        , to         )                {
  const c = dbConn();
  const d0 = dbTab();
  if (d0.kind !== "table") return;
  const d = d0;
  const j = await apiJson                 ("/api/db/" + encodeURIComponent(c.conn ) + "/ddl", {
    method: "POST",
    body: JSON.stringify({ op: op, table: d.table, schema: d.schema, to: to }),
  });
  if (!j) return;
  toast(tr("dataEdit.ran", { sql: j.ran }));
  if (op === "drop") {
    // The table is gone, so every tab open on it goes with it — this one and any background
    // tab holding the same table under a different filter. What is left is whatever else was
    // open, or the strip's placeholder, whose empty state IS the repaint the right pane needs:
    // renderDbTables refreshes only the LEFT list (docs/22 closeout audit, under docs/42 T2).
    dbDropTableTabs(d.table, d.schema);
    c.tablesPage = 0;
    if (dbIsRedis()) void dbLoadKeys(true);
    else void dbLoadTables();
    return;
  }
  if (op === "rename" && to) { d.table = to; d.data = null; }
  if (op === "truncate") { dbDropEdits(); }
  c.tablesPage = 0;
  if (dbIsRedis()) void dbLoadKeys(true);
  else void dbLoadTables();
  if (d.table) void dbLoadData(true);
  else {
    renderDbTables();
    // and the RIGHT pane, whose last paint still shows the dropped table's rows
    renderDbToolbar(); renderDbFilters(); renderDbGrid(); renderDbBar();
  }
}
/* --- inline cell editor (the default path) ------------------------------------------------------- */
/* A floating overlay sized to the cell: edits short values directly in place WITHOUT touching
   the cell's box — the grid cannot shift. Enter/blur saves to the LOCAL buffer, Esc cancels.
   Long content (> INLINE_MAX chars or multi-line) opens the dialog instead, on the theory that
   a 4 KB JSON blob is exactly what the dialog was built for — and the context menu always
   offers "Edit in dialog" so the choice stays with the user. */
const DB_INLINE_MAX = 80;

/* An update edit carries its row's key and meta; an insert edit has neither (its row is
   d.inserts[i]), so key/meta are nullable and the update paths narrow before reading them. */
let dbInlineEdit                                                                                                                                            = null;

function dbCloseInlineEdit()       {
  if (!dbInlineEdit) return;
  const e = dbInlineEdit;
  dbInlineEdit = null;
  e.ta.remove();
  document.removeEventListener("mousedown", dbInlineDismiss, true);
}

function dbInlineDismiss(ev            )       {
  if (!dbInlineEdit) return;
  if (ev.target === dbInlineEdit.td || dbInlineEdit.td.contains(ev.target        ) || ev.target === dbInlineEdit.ta) return;
  dbSaveInlineEdit();
}

function dbOpenInlineEdit(kind                     , key               , i        , column        , meta                   , td             , current        )       {
  if (dbInlineEdit) dbCloseInlineEdit();
  const rect = td.getBoundingClientRect();
  const wrap = $("dbGridWrap");
  const wrapRect = wrap ? wrap.getBoundingClientRect() : { left: 0, top: 0 };
  const ta = el("textarea", "db-inline-edit")                       ;
  ta.rows = 1;
  ta.value = current == null ? "" : String(current);
  ta.style.left = (rect.left - wrapRect.left + wrap.scrollLeft) + "px";
  ta.style.top = (rect.top - wrapRect.top + wrap.scrollTop) + "px";
  ta.style.minHeight = Math.max(rect.height, 22) + "px";
  ta.style.width = Math.max(rect.width + 24, 96) + "px";
  wrap.appendChild(ta);
  dbInlineEdit = { td: td, ta: ta, kind: kind, key: key, i: i, column: column, meta: meta };

  function autoSize()       {
    ta.style.height = "auto";
    ta.style.height = Math.min(Math.max(ta.scrollHeight, 22), 320) + "px";
  }
  ta.oninput = autoSize;
  ta.onkeydown = (e               )       => {
    e.stopPropagation();
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); dbSaveInlineEdit(); }
    else if (e.key === "Escape") { e.preventDefault(); dbCloseInlineEdit(); renderDbGrid(); }
    else if (e.key === "Tab") {
      e.preventDefault();
      dbSaveInlineEdit();
      // move to the next editable cell in this row, dialog-free
      const tds = Array.prototype.slice.call(td.parentNode?.querySelectorAll("td.db-cell-edit"));
      const idx = tds.indexOf(td);
      const next = tds[idx + 1] || tds[0];
      if (next && next !== td) next.dispatchEvent(new MouseEvent("dblclick", { bubbles: false }));
    }
  };
  ta.onblur = null; // mousedown-capture handles outside clicks; blur alone fires on our own menu
  document.addEventListener("mousedown", dbInlineDismiss, true);
  autoSize();
  ta.focus();
  ta.select();
}

function dbSaveInlineEdit()       {
  const e = dbInlineEdit;
  if (!e) return;
  const raw = e.ta.value;
  dbCloseInlineEdit();
  const t = dbTab();
  if (t.kind !== "table") return;
  const d = t;
  if (e.kind === "insert") {
    const ins = d.inserts[e.i] ;   // an insert edit only fires for a row that still exists
    if (raw === "") delete ins.values[e.column];
    else ins.values[e.column] = raw;
  } else {
    // An update edit always carries its row key and meta (only the grid's real rows build
    // updates); a null pair would mean the caller lied, and dropping the edit is the
    // honest answer.
    if (e.key == null || !e.meta) return;
    const upd = d.updates[e.key] || (d.updates[e.key] = { pk: e.meta.pk                           , changes: {} });
    const origTxt = e.meta.orig == null ? "" : String(e.meta.orig);
    if (raw === origTxt) {
      delete upd.changes[e.column];
      if (!Object.keys(upd.changes).length) delete d.updates[e.key];
    } else upd.changes[e.column] = raw;
  }
  renderDbGrid();
  renderDbBar();
}

/** The single entry point: short values edit inline, long ones open the dialog. */
function dbEditCellEnter(kind                     , key               , i        , column        , meta                   , td             , current         )       {
  const s = current == null ? "" : String(current);
  if (s.length > DB_INLINE_MAX || s.includes("\n")) {
    dbOpenCellEditor(kind, key, i, column, meta);
    return;
  }
  dbOpenInlineEdit(kind, key, i, column, meta, td, s);
}

export { DB_INLINE_MAX, dbCloseInlineEdit, dbEditCellEnter, dbInlineDismiss, dbInlineEdit, dbOpenInlineEdit, dbRunDdl, dbSaveInlineEdit, dbTableMenu, dbTypedConfirm };
