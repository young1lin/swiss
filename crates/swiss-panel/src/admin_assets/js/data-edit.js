import { $, apiJson, el, state, toast } from "./util.js";
import { dbIsRedis, dbLoadKeys } from "./data-browsers.js";
import { dbOpenCellEditor } from "./data-cell.js";
import { dbLoadData, renderDbGrid } from "./data-grid.js";
import { dbFillConsole, dbTemplateSql, renderDbBar } from "./data-sql.js";
import { dbDropEdits, dbLoadTables, renderDbTables } from "./data-view.js";

/* --- structure operations (rename / truncate / drop) ---------------------------------------------- */
/* A Table menu beside the tabs. Truncate and drop demand a TYPED confirmation — the user
   retypes the table name — because both destroy data with no transaction to roll back to. */
/** docs/22 W1.10: build one template for the open table and drop it into the console. */
function dbGenerateSql(kind) {
  var d = state.db;
  if (!d.data || !d.data.columns || !d.data.columns.length) {
    toast("Open the table first — the template needs its column set", true);
    return;
  }
  var dialect = (d.conns.find(function (c) { return c.name === d.conn; }) || {}).dialect || "mysql";
  var sql;
  try {
    sql = dbTemplateSql(kind, dialect, d.schema, d.table,
      d.data.columns.map(function (c) { return c.name; }), d.data.primaryKey || []);
  } catch (err) { toast(String(err), true); return; }
  dbFillConsole(sql);
}

function dbTableMenu(anchorEl) {
  var d = state.db;
  if (!d.conn || !d.table) return;
  var menu = el("div", "ctx-menu");
  var r = anchorEl.getBoundingClientRect();
  menu.style.left = r.left + "px";
  menu.style.top = (r.bottom + 4) + "px";
  function item(label, fn) {
    var b = el("button", "", label);
    b.onclick = function () { closeMenu2(); fn(); };
    menu.appendChild(b);
  }
  // docs/22 W1.10: generate this table's four statements from the column set the page already
  // carries (the describe_table shape). Identifiers pass the whitelist, values are ?
  // placeholders, and the template lands in the console — fill the ?s, run, and it is history.
  ["select", "insert", "update", "delete"].forEach(function (kind) {
    item("Generate " + kind.toUpperCase(), function () { dbGenerateSql(kind); });
  });
  menu.appendChild(document.createElement("hr"));
  item("Rename table\u2026", function () {
    var to = prompt("Rename " + (d.schema ? d.schema + "." : "") + d.table + " to:", d.table);
    if (!to || to === d.table) return;
    if (!/^[A-Za-z0-9_$]{1,64}$/.test(to)) { toast("Not a valid table name", true); return; }
    dbRunDdl("rename", to);
  });
  item("Truncate table\u2026", function () { dbTypedConfirm("TRUNCATE", function () { dbRunDdl("truncate"); }); });
  item("Drop table\u2026", function () { dbTypedConfirm("DROP", function () { dbRunDdl("drop"); }); });
  document.body.appendChild(menu);
  state.menuOpen = true;
  function closeMenu2() { menu.remove(); state.menuOpen = false; }
  setTimeout(function () {
    document.addEventListener("mousedown", function h(ev) {
      if (!menu.contains(ev.target)) { menu.remove(); state.menuOpen = false; document.removeEventListener("mousedown", h); }
    });
  }, 0);
}

function dbTypedConfirm(word, fn) {
  var d = state.db;
  var what = word === "DROP" ? "DROP (permanently delete)" : "TRUNCATE (delete every row)";
  var typed = prompt(what + " " + (d.schema ? d.schema + "." : "") + d.table + "\n" +
    "This cannot be undone. Type the table name to confirm:", "");
  if (typed !== d.table) { if (typed !== null) toast("Name did not match — nothing was done", true); return; }
  fn();
}

async function dbRunDdl(op, to) {
  var d = state.db;
  var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/ddl", {
    method: "POST",
    body: JSON.stringify({ op: op, table: d.table, schema: d.schema, to: to }),
  });
  if (!j) return;
  toast("Ran: " + j.ran);
  if (op === "drop") { d.table = null; d.data = null; d.detail = null; dbDropEdits(); }
  if (op === "rename" && to) { d.table = to; d.data = null; }
  if (op === "truncate") { dbDropEdits(); }
  d.tablesPage = 0;
  if (dbIsRedis()) dbLoadKeys(true);
  else dbLoadTables();
  if (d.table) dbLoadData(true);
  else renderDbTables();
}
/* --- inline cell editor (the default path) ------------------------------------------------------- */
/* A floating overlay sized to the cell: edits short values directly in place WITHOUT touching
   the cell's box — the grid cannot shift. Enter/blur saves to the LOCAL buffer, Esc cancels.
   Long content (> INLINE_MAX chars or multi-line) opens the dialog instead, on the theory that
   a 4 KB JSON blob is exactly what the dialog was built for — and the context menu always
   offers "Edit in dialog" so the choice stays with the user. */
var DB_INLINE_MAX = 80;

var dbInlineEdit = null; // { td, ta, kind, key, i, column, meta, closed }

function dbCloseInlineEdit() {
  if (!dbInlineEdit) return;
  var e = dbInlineEdit;
  dbInlineEdit = null;
  e.ta.remove();
  document.removeEventListener("mousedown", dbInlineDismiss, true);
}

function dbInlineDismiss(ev) {
  if (!dbInlineEdit) return;
  if (ev.target === dbInlineEdit.td || dbInlineEdit.td.contains(ev.target) || ev.target === dbInlineEdit.ta) return;
  dbSaveInlineEdit();
}

function dbOpenInlineEdit(kind, key, i, column, meta, td, current) {
  var d = state.db;
  if (dbInlineEdit) dbCloseInlineEdit();
  var rect = td.getBoundingClientRect();
  var wrap = $("dbGridWrap");
  var wrapRect = wrap ? wrap.getBoundingClientRect() : { left: 0, top: 0 };
  var ta = el("textarea", "db-inline-edit");
  ta.rows = 1;
  ta.value = current == null ? "" : String(current);
  ta.style.left = (rect.left - wrapRect.left + wrap.scrollLeft) + "px";
  ta.style.top = (rect.top - wrapRect.top + wrap.scrollTop) + "px";
  ta.style.minHeight = Math.max(rect.height, 22) + "px";
  ta.style.width = Math.max(rect.width + 24, 96) + "px";
  wrap.appendChild(ta);
  dbInlineEdit = { td: td, ta: ta, kind: kind, key: key, i: i, column: column, meta: meta };

  function autoSize() {
    ta.style.height = "auto";
    ta.style.height = Math.min(Math.max(ta.scrollHeight, 22), 320) + "px";
  }
  ta.oninput = autoSize;
  ta.onkeydown = function (e) {
    e.stopPropagation();
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); dbSaveInlineEdit(); }
    else if (e.key === "Escape") { e.preventDefault(); dbCloseInlineEdit(); renderDbGrid(); }
    else if (e.key === "Tab") {
      e.preventDefault();
      dbSaveInlineEdit();
      // move to the next editable cell in this row, dialog-free
      var tds = Array.prototype.slice.call(td.parentNode.querySelectorAll("td.db-cell-edit"));
      var idx = tds.indexOf(td);
      var next = tds[idx + 1] || tds[0];
      if (next && next !== td) next.dispatchEvent(new MouseEvent("dblclick", { bubbles: false }));
    }
  };
  ta.onblur = null; // mousedown-capture handles outside clicks; blur alone fires on our own menu
  document.addEventListener("mousedown", dbInlineDismiss, true);
  autoSize();
  ta.focus();
  ta.select();
}

function dbSaveInlineEdit() {
  var e = dbInlineEdit;
  if (!e) return;
  var raw = e.ta.value;
  dbCloseInlineEdit();
  var d = state.db;
  if (e.kind === "insert") {
    var ins = d.inserts[e.i];
    if (raw === "") delete ins.values[e.column];
    else ins.values[e.column] = raw;
  } else {
    var upd = d.updates[e.key] || (d.updates[e.key] = { pk: e.meta.pk, changes: {} });
    var origTxt = e.meta.orig == null ? "" : String(e.meta.orig);
    if (raw === origTxt) {
      delete upd.changes[e.column];
      if (!Object.keys(upd.changes).length) delete d.updates[e.key];
    } else upd.changes[e.column] = raw;
  }
  renderDbGrid();
  renderDbBar();
}

/** The single entry point: short values edit inline, long ones open the dialog. */
function dbEditCellEnter(kind, key, i, column, meta, td, current) {
  var s = current == null ? "" : String(current);
  if (s.length > DB_INLINE_MAX || s.indexOf("\n") >= 0) {
    dbOpenCellEditor(kind, key, i, column, meta);
    return;
  }
  dbOpenInlineEdit(kind, key, i, column, meta, td, s);
}

export { DB_INLINE_MAX, dbCloseInlineEdit, dbEditCellEnter, dbInlineDismiss, dbInlineEdit, dbOpenInlineEdit, dbRunDdl, dbSaveInlineEdit, dbTableMenu, dbTypedConfirm };
