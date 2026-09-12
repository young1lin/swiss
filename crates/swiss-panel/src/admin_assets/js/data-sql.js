import { $, apiJson, el, state, toast } from "./util.js";
import { dbIsRedis } from "./data-browsers.js";
import { dbHighlightSql, dbSqlPaint } from "./data-filters.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { DB_HISTORY_KEY, DB_HISTORY_MAX, dbClearSel, dbDropEdits, dbPending, dbPkKey } from "./data-view.js";

/* --- pending-SQL preview ------------------------------------------------------------------------ */
/* The exact statements the server will run on Commit, mirrored from buildEditStatements
   (dbbrowser.ts) with values INLINED for display only — the real batch runs with bound
   parameters. Used by the bar's SQL button so "what will Commit do" is inspectable before
   it does it. */
function dbQuoteIdentSafe(name) {
  return /^[A-Za-z0-9_$]{1,64}$/.test(name) ? name : null; // same gate as the server
}

function dbSqlLiteral(v) {
  if (v === null) return "NULL";
  if (typeof v === "number") return String(v);
  return "'" + String(v).replace(/'/g, "''") + "'";
}

/** docs/22 W1.4: the one-column stats statements the header's right-click menu runs. Built with
 *  the same identifier gate as the Commit preview — a column name that is not a bare word is
 *  refused, never bare-spliced into SQL. `kind` picks the shape:
 *  "dist" -> value + frequency (top 50); "num" -> COUNT/MIN/MAX/AVG. */
function dbStatsSql(dialect, schema, table, column, kind) {
  var q = function (n) {
    var safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  var target = (schema ? q(schema) + "." : "") + q(table);
  var c = q(column);
  if (kind === "num") {
    return "SELECT COUNT(" + c + ") AS count, MIN(" + c + ") AS min, MAX(" + c + ") AS max, " +
      "AVG(" + c + ") AS avg\nFROM " + target;
  }
  return "SELECT " + c + ", COUNT(*) AS count\nFROM " + target + "\nGROUP BY 1\nORDER BY 2 DESC\nLIMIT 50";
}

function dbPendingSql() {
  var d = state.db;
  if (!d.data) return [];
  var dialect = (d.conns.find(function (c) { return c.name === d.conn; }) || {}).dialect || "mysql";
  var q = function (n) {
    var safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  var table = (d.schema ? q(d.schema) + "." : "") + q(d.table);
  var pkCols = d.data.primaryKey || [];
  var out = [];
  Object.keys(d.deletes).forEach(function (k) {
    var pk = d.deletes[k];
    var where = pkCols.map(function (c) { return q(c) + " = " + dbSqlLiteral(pk[c]); }).join(" AND ");
    out.push("DELETE FROM " + table + " WHERE " + where + ";");
  });
  Object.keys(d.updates).forEach(function (k) {
    var e = d.updates[k];
    var set = Object.keys(e.changes).map(function (c) { return q(c) + " = " + dbSqlLiteral(e.changes[c]); }).join(", ");
    var where = pkCols.map(function (c) { return q(c) + " = " + dbSqlLiteral(e.pk[c]); }).join(" AND ");
    out.push("UPDATE " + table + " SET " + set + " WHERE " + where + ";");
  });
  d.inserts.forEach(function (ins) {
    var cols = Object.keys(ins.values);
    if (!cols.length) return;
    out.push("INSERT INTO " + table + " (" + cols.map(q).join(", ") + ") VALUES (" +
      cols.map(function (c) { return dbSqlLiteral(ins.values[c]); }).join(", ") + ");");
  });
  return out;
}
/* --- buffered edits: the commit / discard bar ---------------------------------------------------- */

function renderDbBar() {
  var d = state.db;
  var bar = $("dbBar");
  if (!bar) return;
  var n = dbPending();
  if (!n || !d.data) { bar.hidden = true; return; }
  bar.hidden = false;
  bar.style.flexWrap = "nowrap";
  bar.innerHTML = "";
  var u = Object.keys(d.updates).length;
  var del = Object.keys(d.deletes).length;
  var ins = d.inserts.length;
  var parts = [];
  if (u) parts.push(u + " update" + (u > 1 ? "s" : ""));
  if (del) parts.push(del + " delete" + (del > 1 ? "s" : ""));
  if (ins) parts.push(ins + " insert" + (ins > 1 ? "s" : ""));
  bar.appendChild(el("span", "", parts.join(", ") + " — LOCAL ONLY, not yet in the database. Commit sends them as ONE transaction (rows addressed by primary key); Discard deletes them without a single query."));
  var sqlBtn = el("button", "btn", d.sqlPreview ? "Hide SQL" : "SQL");
  sqlBtn.title = "Show the exact statements Commit will run";
  sqlBtn.onclick = function () {
    d.sqlPreview = !d.sqlPreview;
    renderDbBar();
  };
  var discard = el("button", "btn", "Discard");
  discard.onclick = function () {
    if (!confirm("Discard " + n + " buffered change" + (n > 1 ? "s" : "") + "? Nothing has been written.")) return;
    dbDropEdits();
    renderDbGrid(); renderDbBar();
  };
  var commitBtn = el("button", "btn commit", "Commit (1 transaction)");
  commitBtn.onclick = dbCommit;
  bar.appendChild(sqlBtn);
  bar.appendChild(discard);
  bar.appendChild(commitBtn);
  if (d.sqlPreview) {
    var pre = el("pre", "db-ddl");
    pre.style.position = "static";
    pre.style.margin = "0";
    pre.style.marginTop = "var(--s2)";
    pre.style.width = "100%";
    try {
      var stmts = dbPendingSql();
      pre.innerHTML = dbHighlightSql("-- " + stmts.length + " statement" + (stmts.length > 1 ? "s" : "") +
        ", executed inside BEGIN ... COMMIT\n" + stmts.join("\n"));
    } catch (e) {
      pre.textContent = String(e && e.message ? e.message : e);
    }
    bar.style.flexWrap = "wrap";
    bar.appendChild(pre);
  }
}

/** docs/22 W1.7: fold one committed row's read-back into the page's rows. The commit reply
 *  carries what the SERVER kept — silent truncation, DEFAULTs, trigger rewrites — so patching it
 *  in shows the truth immediately, whatever the reload race does next. Matching uses the same
 *  pk key the grid's edit buffer uses. */
function dbApplyReadback(rows, pkCols, pk, row) {
  if (!rows || !pkCols || !pkCols.length || !row) return rows;
  var want = dbPkKey(pkCols, pk);
  rows.forEach(function (r) {
    if (dbPkKey(pkCols, r) === want) {
      Object.keys(row).forEach(function (c) { r[c] = row[c]; });
    }
  });
  return rows;
}

/** docs/22 W1.4 / W1.10: put a generated statement into the console — visible, editable, and
 *  in history once run, instead of hiding behind a one-off request. */
function dbFillConsole(sql) {
  var d = state.db;
  d.sqlText = sql;
  d.sqlOpen = true;
  var con = $("dbConsole");
  if (con) con.hidden = false;
  var ta = $("dbSql");
  if (ta) ta.value = sql;
  dbSqlPaint();
  if (ta) ta.focus();
}

async function dbCommit() {
  var d = state.db;
  if (!d.conn || !d.table || !d.data) return;
  var edits = [];
  Object.keys(d.deletes).forEach(function (k) { edits.push({ op: "delete", pk: d.deletes[k] }); });
  Object.keys(d.updates).forEach(function (k) {
    var e = d.updates[k];
    edits.push({ op: "update", pk: e.pk, changes: e.changes });
  });
  d.inserts.forEach(function (ins) { edits.push({ op: "insert", values: ins.values }); });
  if (!edits.length) return;
  // The explicit transaction gate: nothing leaves this browser until the user says so HERE too.
  // The summary names the table and counts, because "commit 3 changes" must be a decision, not a reflex.
  var ups = Object.keys(d.updates).length, dels = Object.keys(d.deletes).length, ins = d.inserts.length;
  var parts = [];
  if (ups) parts.push(ups + " update" + (ups > 1 ? "s" : ""));
  if (dels) parts.push(dels + " delete" + (dels > 1 ? "s" : ""));
  if (ins) parts.push(ins + " insert" + (ins > 1 ? "s" : ""));
  var tableLabel = (d.schema ? d.schema + "." : "") + d.table;
  if (!confirm("Commit " + parts.join(", ") + " to " + tableLabel + "?\n" +
      "One transaction: every row is addressed by its primary key, and any failure rolls the whole batch back.")) {
    return; // cancelled — the buffer stays, nothing was sent
  }
  var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/edits", {
    method: "POST",
    body: JSON.stringify({ table: d.table, schema: d.schema, edits: edits }),
  });
  if (!j) return; // the server rolled back; the buffer stays exactly as it was
  var affected = (j.results || []).reduce(function (a, r) { return a + (r.affected || 0); }, 0);
  toast("Committed " + edits.length + " change" + (edits.length > 1 ? "s" : "") + " · " + affected + " row" + (affected === 1 ? "" : "s") + " affected");
  // docs/22 W1.7: each update's read-back row lands on the page before the reload, so the
  // committed truth (truncated, defaulted, trigger-rewritten) is what the grid shows next.
  var pkCols = (d.data && d.data.primaryKey) || [];
  (j.results || []).forEach(function (r, i) {
    if (r && r.row && edits[i] && edits[i].op === "update" && d.data && d.data.rows) {
      dbApplyReadback(d.data.rows, pkCols, edits[i].pk, r.row);
    }
  });
  dbDropEdits();
  dbLoadData(true);
}

/** EXPLAIN-prefix a console statement — mode "analyze" spells EXPLAIN ANALYZE, which runs
    the statement and times every plan node. Same rules as withExplain on the server:
    idempotent, one trailing terminator stripped. Mirrored here because the server helper is
    TypeScript. */
function dbWithExplain(sql, mode) {
  // ;\s*$ is the server's (and the Rust port's) chain: the old ;\s+$ here could never match
  // once the trailing whitespace had already been stripped, so the terminator survived.
  var s = sql.replace(/\s+$/, "").replace(/;\s*$/, "").replace(/\s+$/, "");
  if (/^explain\b/i.test(s)) return s;
  return mode === "analyze" ? "EXPLAIN ANALYZE " + s : "EXPLAIN " + s;
}

/* --- query history (per-browser) ---------------------------------------------------------------- */

function dbHistoryLoad() {
  try { state.db.history = JSON.parse(localStorage.getItem(DB_HISTORY_KEY)) || []; }
  catch (e) { state.db.history = []; }
}

function dbHistorySave() {
  try { localStorage.setItem(DB_HISTORY_KEY, JSON.stringify(state.db.history.slice(0, DB_HISTORY_MAX))); }
  catch (e) { /* full or blocked — history is a convenience, not state */ }
}

/** Record a successful run: newest first, a repeat of the current head is a no-op, and the
 *  list stays bounded. */
function dbHistoryPush(sql) {
  var d = state.db;
  if (d.history[0] === sql) return;
  d.history.unshift(sql);
  d.history = d.history.slice(0, DB_HISTORY_MAX);
  dbHistorySave();
  dbHistoryRender();
}

function dbHistoryRender() {
  var sel = $("dbSqlHistory");
  if (!sel) return;
  sel.innerHTML = "";
  var head = el("option", "", "History");
  head.value = "";
  sel.appendChild(head);
  state.db.history.forEach(function (sql, i) {
    var o = el("option", "", sql.replace(/\s+/g, " ").slice(0, 80));
    o.value = String(i);
    o.title = sql;
    sel.appendChild(o);
  });
}

/* --- SQL console --------------------------------------------------------------------------------- */

/** docs/22 W1.10: the table menu's SQL templates. Column names pass the identifier whitelist
 *  (never a bare splice), value positions are ? placeholders, and one comment line says what to
 *  do with them — the template lands in the console runnable after the ?s are filled in. */
function dbTemplateSql(kind, dialect, schema, table, columns, pk) {
  var q = function (n) {
    var safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  var t = (schema ? q(schema) + "." : "") + q(table);
  var cols = columns.map(q);
  var key = pk.map(q);
  var hint = "-- replace each ? with a value before running";
  if (kind === "select") {
    return hint + "\nSELECT " + cols.join(", ") + "\nFROM " + t +
      (key.length ? "\nWHERE " + key.map(function (c) { return c + " = ?"; }).join(" AND ") : "") + ";";
  }
  if (kind === "insert") {
    return hint + "\nINSERT INTO " + t + " (" + cols.join(", ") + ")\nVALUES (" +
      cols.map(function () { return "?"; }).join(", ") + ");";
  }
  if (kind === "update") {
    var keyNames = {};
    pk.forEach(function (p) { keyNames[p] = true; });
    var set = columns.filter(function (n) { return !keyNames[n]; }).map(q);
    if (!set.length || !key.length) {
      throw new Error("an UPDATE template needs a non-key column and a primary key");
    }
    return hint + "\nUPDATE " + t + "\nSET " + set.map(function (c) { return c + " = ?"; }).join(", ") +
      "\nWHERE " + key.map(function (c) { return c + " = ?"; }).join(" AND ") + ";";
  }
  if (!key.length) throw new Error("a DELETE template needs a primary key");
  return hint + "\nDELETE FROM " + t + "\nWHERE " + key.map(function (c) { return c + " = ?"; }).join(" AND ") + ";";
}

/** docs/22 W1.8: split SQL text into blank-line-separated blocks and return the one the caret
 *  sits in — Ctrl+Enter on a three-block script runs only the second block. A caret inside a
 *  blank gap belongs to the block AFTER it (that is where the cursor visually rests); a missing
 *  caret means the end of the text. No blank lines means one block: exactly the whole box, the
 *  behaviour the console always had. */
function dbSubqueryAt(text, caret) {
  var t = String(text);
  var seps = [];
  t.replace(/\n[ \t]*\n/g, function (m, i) { seps.push([i, i + m.length]); return m; });
  if (!seps.length) return t;
  var at = typeof caret === "number" && caret >= 0 && caret <= t.length ? caret : t.length;
  var start = 0, end = t.length;
  for (var i = 0; i < seps.length; i++) {
    if (at < seps[i][0]) { end = seps[i][0]; break; }
    start = seps[i][1];
  }
  return t.slice(start, end);
}

async function dbRunSql(explain) { // falsy runs the statement; "plan"|"analyze" prefix EXPLAIN
  var d = state.db;
  if (!d.conn) { toast("No database connection", true); return; }
  // docs/22 W1.8: the run covers the block the caret is in — one block per run keeps the
  // single-statement guard honest on multi-part scripts.
  var ta = $("dbSql");
  var sql = dbSubqueryAt(d.sqlText || "", ta ? ta.selectionStart : null).trim();
  if (!sql) { toast("Type a command first", true); return; }
  // The redis console: one command per run; the server-side guard still refuses what would
  // break the shared connection or the server. Writes (SET, DEL, EXPIRE…) run.
  if (dbIsRedis()) {
    d.sqlBusy = true;
    renderDbToolbar();
    renderDbGrid();
    var cj = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/command", {
      method: "POST",
      body: JSON.stringify({ command: sql }),
    });
    d.sqlBusy = false;
    if (!cj) { d.sqlResult = null; renderDbToolbar(); renderDbGrid(); return; }
    dbClearSel(); // a new result grid starts unselected
    d.sqlResult = { columns: ["reply"], rows: [{ reply: cj.reply }], rowCount: 1, explained: false,
      elapsedMs: cj.elapsedMs,
      note: typeof cj.reply === "object" && cj.reply && cj.reply.length != null ? cj.reply.length + " items" : undefined };
    dbHistoryPush(sql);
    renderDbToolbar();
    renderDbGrid();
    return;
  }
  // The plan view runs EXPLAIN (or EXPLAIN ANALYZE) on the same statement; dbWithExplain is
  // idempotent, so a query that already explains itself is sent as-is.
  var toSend = explain ? dbWithExplain(sql, explain) : sql;
  d.sqlBusy = true;
  renderDbToolbar();
  renderDbGrid();
  var j = await apiJson("/api/db/" + encodeURIComponent(d.conn) + "/query", {
    method: "POST",
    body: JSON.stringify({ sql: toSend, limit: d.pageSize }),
  });
  d.sqlBusy = false;
  if (!j) { d.sqlResult = null; renderDbToolbar(); renderDbGrid(); return; }
  dbClearSel(); // a new result grid starts unselected
  d.sqlResult = j;
  d.sqlResult.explained = !!explain;
  if (!explain) dbHistoryPush(sql); // the plan of a query is not a query — only runs are history
  renderDbToolbar();
  renderDbGrid();
}

export { dbApplyReadback, dbCommit, dbFillConsole, dbHistoryLoad, dbHistoryPush, dbHistoryRender, dbHistorySave, dbPendingSql, dbQuoteIdentSafe, dbRunSql, dbSqlLiteral, dbStatsSql, dbSubqueryAt, dbTemplateSql, dbWithExplain, renderDbBar };
