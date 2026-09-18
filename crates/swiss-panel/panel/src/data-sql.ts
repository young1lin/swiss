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

import { $, apiJson, dbReqGuard, el, state, toast } from "./util.js";
import {
  dbIsRedis, dbRedisCommandText, dbRedisCommands, dbRedisCommit, dbRedisDiscard,
  dbRedisPendingCount,
} from "./data-browsers.js";
import { SQL_TOKEN_RE, dbHighlightSql, dbSqlPaint } from "./data-filters.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { DB_HISTORY_KEY, DB_HISTORY_MAX, dbClearSel, dbDropEdits, dbPending, dbPkKey } from "./data-view.js";

/* --- pending-SQL preview ------------------------------------------------------------------------ */
/* The exact statements the server will run on Commit, mirrored from buildEditStatements
   (dbbrowser.ts) with values INLINED for display only — the real batch runs with bound
   parameters. Used by the bar's SQL button so "what will Commit do" is inspectable before
   it does it. */
function dbQuoteIdentSafe(name: string): string | null {
  return /^[A-Za-z0-9_$]{1,64}$/.test(name) ? name : null; // same gate as the server
}

function dbSqlLiteral(v: unknown): string {
  if (v === null) return "NULL";
  if (typeof v === "number") return String(v);
  return "'" + String(v).replace(/'/g, "''") + "'";
}

/** docs/22 W1.4: the one-column stats statements the header's right-click menu runs. Built with
 *  the same identifier gate as the Commit preview — a column name that is not a bare word is
 *  refused, never bare-spliced into SQL. `kind` picks the shape:
 *  "dist" -> value + frequency (top 50); "num" -> COUNT/MIN/MAX/AVG. */
function dbStatsSql(dialect: string, schema: string, table: string, column: string, kind: string): string {
  var q = function (n: string): string {
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

function dbPendingSql(): string[] {
  var d = state.db;
  if (!d!.data) return [];
  var dialect = (d!.conns.find(function (c: ApiDbConnectionRow): boolean { return c.name === d!.conn; }) || {} as { dialect?: string }).dialect || "mysql";
  var q = function (n: string): string {
    var safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  var table = (d!.schema ? q(d!.schema!) + "." : "") + q(d!.table!);
  var pkCols = d!.data.primaryKey || [];
  // docs/22 W4.1 (W4b follow-up): the preview sketches the address the server really builds —
  // pk columns for a pk table, EVERY column for a keyless one (the row buffer carries whole
  // rows there), with the md5 fold the server applies (EDIT_ADDR_MD5_MIN = 64). md5() here is
  // a sketch of what gets digested, not the digest itself: the browser has no md5 and the
  // server computes the real one. A NULL or missing column cannot address anything (col =
  // NULL matches nothing) — the server refuses the row, so the preview says so instead of
  // printing an empty or lying WHERE.
  var addrCols = pkCols.length ? pkCols : (d!.data.columns || []).map(function (c: ApiDbColumn): string { return c.name; });
  var addrTerm = function (c: string, v: unknown): string {
    if (!pkCols.length && v === null) throw new Error("no primary key: " + c + " is NULL — a row needs a non-NULL value for every column");
    if (!pkCols.length && v === undefined) throw new Error("no primary key: " + c + " is missing — a row needs a value for every column");
    var s = typeof v === "string" ? v : String(v);
    if (/^\\x[0-9a-fA-F]*$/.test(s) || s.length > 64) return "MD5(" + q(c) + ") = md5(" + dbSqlLiteral(s) + ")";
    return q(c) + " = " + dbSqlLiteral(v);
  };
  var rowWhere = function (addr: Record<string, unknown>): string {
    return addrCols.map(function (c: string): string { return addrTerm(c, addr[c]); }).join(" AND ");
  };
  var out: string[] = [];
  Object.keys(d!.deletes).forEach(function (k: string): void {
    out.push("DELETE FROM " + table + " WHERE " + rowWhere(d!.deletes[k]) + ";");
  });
  Object.keys(d!.updates).forEach(function (k: string): void {
    var e = d!.updates[k];
    var set = Object.keys(e.changes).map(function (c: string): string { return q(c) + " = " + dbSqlLiteral(e.changes[c]); }).join(", ");
    out.push("UPDATE " + table + " SET " + set + " WHERE " + rowWhere(e.pk) + ";");
  });
  d!.inserts.forEach(function (ins: DbInsert): void {
    var cols = Object.keys(ins.values);
    if (!cols.length) return;
    out.push("INSERT INTO " + table + " (" + cols.map(q).join(", ") + ") VALUES (" +
      cols.map(function (c: string): string { return dbSqlLiteral(ins.values[c]); }).join(", ") + ");");
  });
  return out;
}
/* --- buffered edits: the commit / discard bar ---------------------------------------------------- */

/** The redis arm of the bar (docs/22 W3.3): same slot, same words, same Discard, with
 *  "Commands" in place of "SQL" and one pipeline in place of the transaction. The command
 *  list is the very list Commit posts — the preview is the payload, not a paraphrase. */
function renderDbRedisBar(d: DbState, bar: HTMLElement, n: number): void {
  var b = d.redisEdits!;
  var u = Object.keys(b.updates).length;
  var del = Object.keys(b.deletes).length;
  var ins = b.inserts.length;
  var parts = [];
  if (u) parts.push(u + " update" + (u > 1 ? "s" : ""));
  if (del) parts.push(del + " delete" + (del > 1 ? "s" : ""));
  if (ins) parts.push(ins + " insert" + (ins > 1 ? "s" : ""));
  bar.appendChild(el("span", "", parts.join(", ") + " — LOCAL ONLY, not yet in redis. Commit sends them as ONE pipelined round trip (every command guard-checked); Discard deletes them without a single command."));
  var cmdBtn = el("button", "btn", d.sqlPreview ? "Hide commands" : "Commands") as HTMLButtonElement;
  cmdBtn.title = "Show the exact commands Commit will run";
  cmdBtn.onclick = function (): void {
    d.sqlPreview = !d.sqlPreview;
    renderDbBar();
  };
  var discard = el("button", "btn", "Discard") as HTMLButtonElement;
  discard.onclick = dbRedisDiscard;
  var commitBtn = el("button", "btn commit", "Commit (1 pipeline)") as HTMLButtonElement;
  commitBtn.onclick = function (): void { void dbRedisCommit(); };
  bar.appendChild(cmdBtn);
  bar.appendChild(discard);
  bar.appendChild(commitBtn);
  if (d.sqlPreview) {
    var pre = el("pre", "db-ddl");
    pre.style.position = "static";
    pre.style.margin = "0";
    pre.style.marginTop = "var(--s2)";
    pre.style.width = "100%";
    try {
      var cmds = dbRedisCommands(d.redisKey!, b.type, b);
      pre.textContent = "-- " + cmds.length + " command" + (cmds.length > 1 ? "s" : "") +
        ", one pipelined round trip — each guard-checked before the socket is touched\n" +
        cmds.map(dbRedisCommandText).join("\n");
    } catch (e) {
      pre.textContent = String(e && e.message ? e.message : e);
    }
    bar.style.flexWrap = "wrap";
    bar.appendChild(pre);
  }
}

function renderDbBar(): void {
  var d = state.db;
  var bar = $("dbBar");
  if (!bar) return;
  var redis = dbIsRedis();
  var n = redis ? dbRedisPendingCount() : dbPending();
  // The redis bar owns the same slot (docs/22 W3.3): the typed value view buffers edits the
  // way the row grid does, and its Commit is ONE guarded pipeline instead of a transaction.
  if (!n || (redis ? !d!.redisValue : !d!.data)) { bar.hidden = true; return; }
  bar.hidden = false;
  bar.style.flexWrap = "nowrap";
  bar.innerHTML = "";
  if (redis) {
    renderDbRedisBar(d!, bar, n);
    return;
  }
  var u = Object.keys(d!.updates).length;
  var del = Object.keys(d!.deletes).length;
  var ins = d!.inserts.length;
  var parts: string[] = [];
  if (u) parts.push(u + " update" + (u > 1 ? "s" : ""));
  if (del) parts.push(del + " delete" + (del > 1 ? "s" : ""));
  if (ins) parts.push(ins + " insert" + (ins > 1 ? "s" : ""));
  // Same addressing honesty as the Commit gate (docs/22 W4b follow-up): the bar names the
  // WHERE the server will build, pk or whole-row.
  var pkColsB = (d!.data && d!.data.primaryKey) || [];
  bar.appendChild(el("span", "", parts.join(", ") + " — LOCAL ONLY, not yet in the database. Commit sends them as ONE transaction (rows addressed by " + (pkColsB.length ? "primary key" : "all columns — the table has no primary key") + "); Discard deletes them without a single query."));
  var sqlBtn = el("button", "btn", d!.sqlPreview ? "Hide SQL" : "SQL") as HTMLButtonElement;
  sqlBtn.title = "Show the exact statements Commit will run";
  sqlBtn.onclick = function (): void {
    d!.sqlPreview = !d!.sqlPreview;
    renderDbBar();
  };
  var discard = el("button", "btn", "Discard") as HTMLButtonElement;
  discard.onclick = function (): void {
    if (!confirm("Discard " + n + " buffered change" + (n > 1 ? "s" : "") + "? Nothing has been written.")) return;
    dbDropEdits();
    renderDbGrid(); renderDbBar();
  };
  var commitBtn = el("button", "btn commit", "Commit (1 transaction)") as HTMLButtonElement;
  commitBtn.onclick = dbCommit;
  bar.appendChild(sqlBtn);
  bar.appendChild(discard);
  bar.appendChild(commitBtn);
  if (d!.sqlPreview) {
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
function dbApplyReadback(rows: Record<string, unknown>[] | null, pkCols: string[], pk: Record<string, unknown> | null, row: Record<string, unknown> | null): Record<string, unknown>[] | null {
  if (!rows || !pkCols || !pkCols.length || !row) return rows;
  var want = dbPkKey(pkCols, pk!);
  rows.forEach(function (r: Record<string, unknown>): void {
    if (dbPkKey(pkCols, r) === want) {
      Object.keys(row).forEach(function (c: string): void { r[c] = row[c]; });
    }
  });
  return rows;
}

/** docs/22 W1.4 / W1.10: put a generated statement into the console — visible, editable, and
 *  in history once run, instead of hiding behind a one-off request. */
function dbFillConsole(sql: string): void {
  var d = state.db;
  d!.sqlText = sql;
  d!.sqlOpen = true;
  var con = $("dbConsole");
  if (con) con.hidden = false;
  var ta = $<HTMLTextAreaElement>("dbSql");
  if (ta) ta.value = sql;
  dbSqlPaint();
  if (ta) ta.focus();
}

async function dbCommit(): Promise<void> {
  var d = state.db;
  if (!d!.conn || !d!.table || !d!.data) return;
  var edits: { op: string; pk?: Record<string, unknown>; changes?: Record<string, unknown>; values?: Record<string, unknown> }[] = [];
  Object.keys(d!.deletes).forEach(function (k: string): void { edits.push({ op: "delete", pk: d!.deletes[k] }); });
  Object.keys(d!.updates).forEach(function (k: string): void {
    var e = d!.updates[k];
    edits.push({ op: "update", pk: e.pk, changes: e.changes });
  });
  d!.inserts.forEach(function (ins: DbInsert): void { edits.push({ op: "insert", values: ins.values }); });
  if (!edits.length) return;
  // The explicit transaction gate: nothing leaves this browser until the user says so HERE too.
  // The summary names the table and counts, because "commit 3 changes" must be a decision, not a reflex.
  var ups = Object.keys(d!.updates).length, dels = Object.keys(d!.deletes).length, ins = d!.inserts.length;
  var parts: string[] = [];
  if (ups) parts.push(ups + " update" + (ups > 1 ? "s" : ""));
  if (dels) parts.push(dels + " delete" + (dels > 1 ? "s" : ""));
  if (ins) parts.push(ins + " insert" + (ins > 1 ? "s" : ""));
  var tableLabel = (d!.schema ? d!.schema + "." : "") + d!.table!;
  // docs/22 W4.1 (W4b follow-up): the gate names the address the server will really use — a
  // keyless table commits with whole-row WHEREs, and the user deserves that in the decision.
  var pkColsC = (d!.data && d!.data.primaryKey) || [];
  var addressed = pkColsC.length
    ? "every row is addressed by its primary key"
    : "the table has no primary key — every row is addressed by all its columns";
  if (!confirm("Commit " + parts.join(", ") + " to " + tableLabel + "?\n" +
      "One transaction: " + addressed + ", and any failure rolls the whole batch back.")) {
    return; // cancelled — the buffer stays, nothing was sent
  }
  var j = await apiJson<{ results?: { affected?: number; row?: Record<string, unknown> }[] }>("/api/db/" + encodeURIComponent(d!.conn!) + "/edits", {
    method: "POST",
    body: JSON.stringify({ table: d!.table, schema: d!.schema, edits: edits }),
  });
  if (!j) return; // the server rolled back; the buffer stays exactly as it was
  var affected = (j.results || []).reduce(function (a: number, r: { affected?: number }): number { return a + (r.affected || 0); }, 0);
  toast("Committed " + edits.length + " change" + (edits.length > 1 ? "s" : "") + " · " + affected + " row" + (affected === 1 ? "" : "s") + " affected");
  // docs/22 W1.7: each update's read-back row lands on the page before the reload, so the
  // committed truth (truncated, defaulted, trigger-rewritten) is what the grid shows next.
  var pkCols = (d!.data && d!.data.primaryKey) || [];
  (j.results || []).forEach(function (r: { affected?: number; row?: Record<string, unknown> }, i: number): void {
    if (r && r.row && edits[i] && edits[i].op === "update" && d!.data && d!.data.rows) {
      dbApplyReadback(d!.data.rows, pkCols, edits[i].pk!, r.row);
    }
  });
  dbDropEdits();
  // docs/22 closeout B4: the reload rebuilds the grid and scroll anchoring is OFF by design
  // (docs/22 W2.2 — a repaint must never jump the pane), so the commit would otherwise snap
  // the user back to the top, away from the row they just committed. Capture before the
  // reload, restore after it lands; a superseding load owns the pane by then and a stale
  // restore is a harmless scroll to where the user was anyway.
  var wrap = $("dbGridWrap");
  var scrollTop = wrap ? wrap.scrollTop : 0;
  void dbLoadData(true).then(function (): void {
    var w2 = $("dbGridWrap");
    if (w2) w2.scrollTop = scrollTop;
  });
}

/** EXPLAIN-prefix a console statement — mode "analyze" spells EXPLAIN ANALYZE, which runs
    the statement and times every plan node. Same rules as withExplain on the server:
    idempotent, one trailing terminator stripped. Mirrored here because the server helper is
    TypeScript. */
function dbWithExplain(sql: string, mode: string): string {
  // ;\s*$ is the server's (and the Rust port's) chain: the old ;\s+$ here could never match
  // once the trailing whitespace had already been stripped, so the terminator survived.
  var s = sql.replace(/\s+$/, "").replace(/;\s*$/, "").replace(/\s+$/, "");
  if (/^explain\b/i.test(s)) return s;
  return mode === "analyze" ? "EXPLAIN ANALYZE " + s : "EXPLAIN " + s;
}

/* --- query history (per-browser) ---------------------------------------------------------------- */

function dbHistoryLoad(): void {
  try { state.db!.history = JSON.parse(localStorage.getItem(DB_HISTORY_KEY) as string) || []; }
  catch (e) { state.db!.history = []; }
}

function dbHistorySave(): void {
  try { localStorage.setItem(DB_HISTORY_KEY, JSON.stringify(state.db!.history.slice(0, DB_HISTORY_MAX))); }
  catch (e) { /* full or blocked — history is a convenience, not state */ }
}

/** Record a successful run: newest first, a repeat of the current head is a no-op, and the
 *  list stays bounded. */
function dbHistoryPush(sql: string): void {
  var d = state.db;
  if (d!.history[0] === sql) return;
  d!.history.unshift(sql);
  d!.history = d!.history.slice(0, DB_HISTORY_MAX);
  dbHistorySave();
  dbHistoryRender();
}

function dbHistoryRender(): void {
  var sel = $("dbSqlHistory");
  if (!sel) return;
  sel.innerHTML = "";
  var head = el("option", "", "History") as HTMLOptionElement;
  head.value = "";
  sel.appendChild(head);
  var d = state.db;
  // docs/22 W5.4: one dropdown, two groups — what ran (history) and what was starred
  // (favorites). The value carries the group: a plain index is history, "f"+i a favorite.
  if (d!.history && d!.history.length) {
    var og = el("optgroup") as HTMLOptGroupElement;
    og.label = "History";
    d!.history.forEach(function (sql: string, i: number): void {
      var o = el("option", "", sql.replace(/\s+/g, " ").slice(0, 80)) as HTMLOptionElement;
      o.value = String(i);
      o.title = sql;
      og.appendChild(o);
    });
    sel.appendChild(og);
  }
  if (d!.favorites && d!.favorites.length) {
    var fg = el("optgroup") as HTMLOptGroupElement;
    fg.label = "Favorites";
    d!.favorites.forEach(function (sql: string, i: number): void {
      var o = el("option", "", dbFavoriteName(sql)) as HTMLOptionElement;
      o.value = "f" + i;
      o.title = sql;
      fg.appendChild(o);
    });
    sel.appendChild(fg);
  }
}

/* --- favorites (docs/22 W5.4) -------------------------------------------------------------------- */
/* The starred list beside the history: same localStorage-per-browser treatment, same
   bounded list, the history's own rules (newest first, a repeat save moves to the top
   rather than duplicating). */

var DB_FAV_KEY = "mcp_gateway_db_favorites";
var DB_FAV_MAX = 50;

function dbFavLoad(): void {
  try { state.db!.favorites = JSON.parse(localStorage.getItem(DB_FAV_KEY) as string) || []; }
  catch (e) { state.db!.favorites = []; }
}

function dbFavSave(): void {
  try { localStorage.setItem(DB_FAV_KEY, JSON.stringify(state.db!.favorites!.slice(0, DB_FAV_MAX))); }
  catch (e) { /* full or blocked — favorites are a convenience, not state */ }
}

/** The option's name: the query's FIRST line, whitespace-folded, cut at the history's 80. */
function dbFavoriteName(sql: string): string {
  var first = String(sql).split("\n")[0].replace(/\s+/g, " ").trim();
  return first.slice(0, 80);
}

function dbFavPush(sql: string): void {
  var d = state.db;
  var s = String(sql == null ? "" : sql);
  if (!s.trim()) { toast("Nothing to save \u2014 the console is empty", true); return; }
  d!.favorites = (d!.favorites || []).filter(function (f: string): boolean { return f !== s; });
  d!.favorites.unshift(s);
  d!.favorites = d!.favorites.slice(0, DB_FAV_MAX);
  dbFavSave();
  dbHistoryRender();
  toast("Saved to favorites");
}

/* --- the lightweight SQL formatter (docs/22 W5.4) ------------------------------------------------ */
/* Lexical only — the console's own SQL_TOKEN_RE stream, never a parse: top-level clause
   keywords break onto their own lines (a join lead breaks with its join), AND/OR continue
   two spaces under their clause, statements split at the top-level semicolon, a line
   comment ends its line, and punctuation spacing is normalized (no space before , ) ; .,
   none after ( .). Case, tokens, strings and comments ride byte-identical, so a formatted
   statement runs exactly as it did — the whitespace-equivalence the round-trip test pins. */

var DB_FORMAT_CLAUSES = "select from where group order having limit offset fetch union except intersect values set insert update delete with join".split(" ");
var DB_FORMAT_JOIN_LEADS = "left right inner outer full cross natural".split(" ");

function dbFormatSql(text: string): string {
  var tokens: string[] = [];
  var m: RegExpExecArray | null;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(String(text == null ? "" : text))) !== null) {
    if (m[5]) continue; // whitespace is rebuilt, never carried
    // Punctuation runs split into characters: "((" and ")," are two spacing decisions each.
    if (m[6]) { for (var ci = 0; ci < m[0].length; ci++) tokens.push(m[0][ci]); }
    else tokens.push(m[0]);
  }
  var out = "";
  var depth = 0;
  var prev: string | null = null; // the last token on the CURRENT line; null = a fresh line start
  for (var i = 0; i < tokens.length; i++) {
    var tok = tokens[i];
    var low = tok.toLowerCase();
    if (depth === 0 && prev !== null && out !== "" && /^[A-Za-z_]/.test(tok)) {
      var nxt = tokens[i + 1];
      // "join" does not break again when its lead (left/right/inner/...) just broke.
      var joinAgain = low === "join" && DB_FORMAT_JOIN_LEADS.indexOf(prev.toLowerCase()) >= 0;
      var clause = !joinAgain && (DB_FORMAT_CLAUSES.indexOf(low) >= 0 ||
        (DB_FORMAT_JOIN_LEADS.indexOf(low) >= 0 && nxt && nxt.toLowerCase() === "join"));
      var cont = low === "and" || low === "or";
      if (clause || cont) { out += "\n" + (cont ? "  " : ""); prev = null; }
    }
    if (prev && !(prev === "(" || tok === ")" || tok === "," || tok === ";" || tok === "." || prev === "." || prev === ";")) out += " ";
    out += tok;
    prev = tok;
    if (tok === "(") depth++;
    else if (tok === ")") depth = depth > 0 ? depth - 1 : 0;
    else if (tok === ";" && depth === 0) { out += "\n"; prev = null; }
    else if (tok.charAt(0) === "-" && tok.charAt(1) === "-" || tok.charAt(0) === "#") { out += "\n"; prev = null; }
  }
  return out.replace(/\s+$/, "");
}

/* --- SQL console --------------------------------------------------------------------------------- */

/** docs/22 W1.10: the table menu's SQL templates. Column names pass the identifier whitelist
 *  (never a bare splice), value positions are ? placeholders, and one comment line says what to
 *  do with them — the template lands in the console runnable after the ?s are filled in. */
function dbTemplateSql(kind: string, dialect: string, schema: string | undefined, table: string | undefined, columns: string[], pk: string[]): string {
  var q = function (n: string): string {
    var safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  var t = (schema ? q(schema) + "." : "") + q(table!);
  var cols = columns.map(q);
  var key = pk.map(q);
  var hint = "-- replace each ? with a value before running";
  if (kind === "select") {
    return hint + "\nSELECT " + cols.join(", ") + "\nFROM " + t +
      (key.length ? "\nWHERE " + key.map(function (c) { return c + " = ?"; }).join(" AND ") : "") + ";";
  }
  if (kind === "insert") {
    return hint + "\nINSERT INTO " + t + " (" + cols.join(", ") + ")\nVALUES (" +
      cols.map(function (): string { return "?"; }).join(", ") + ");";
  }
  if (kind === "update") {
    var keyNames: Record<string, boolean> = {};
    pk.forEach(function (p: string): void { keyNames[p] = true; });
    var set = columns.filter(function (n: string): boolean { return !keyNames[n]; }).map(q);
    if (!set.length || !key.length) {
      throw new Error("an UPDATE template needs a non-key column and a primary key");
    }
    return hint + "\nUPDATE " + t + "\nSET " + set.map(function (c: string): string { return c + " = ?"; }).join(", ") +
      "\nWHERE " + key.map(function (c: string): string { return c + " = ?"; }).join(" AND ") + ";";
  }
  if (!key.length) throw new Error("a DELETE template needs a primary key");
  return hint + "\nDELETE FROM " + t + "\nWHERE " + key.map(function (c: string): string { return c + " = ?"; }).join(" AND ") + ";";
}

/** docs/22 W1.8: split SQL text into blank-line-separated blocks and return the one the caret
 *  sits in — Ctrl+Enter on a three-block script runs only the second block. A caret inside a
 *  blank gap belongs to the block AFTER it (that is where the cursor visually rests); a missing
 *  caret means the end of the text. No blank lines means one block: exactly the whole box, the
 *  behaviour the console always had. */
function dbSubqueryAt(text: string, caret: number | null): string {
  var t = String(text);
  var seps: number[][] = [];
  t.replace(/\n[ \t]*\n/g, function (m: string, i: number): string { seps.push([i, i + m.length]); return m; });
  if (!seps.length) return t;
  var at = typeof caret === "number" && caret >= 0 && caret <= t.length ? caret : t.length;
  var start = 0, end = t.length;
  for (var i = 0; i < seps.length; i++) {
    if (at < seps[i][0]) { end = seps[i][0]; break; }
    start = seps[i][1];
  }
  return t.slice(start, end);
}

/** docs/22 W4.3: split a console block into statements at top-level semicolons — the
 *  panel's half of multi-result tabs. A naive text.split(";") breaks on the first literal or
 *  comment that carries one ('a;b', "-- note;", 'it''s;'), so this walks the SAME token stream
 *  the syntax highlighter lexes (SQL_TOKEN_RE: quotes ', \" and ` with backslash and
 *  doubled-quote escapes, -- and # line comments, slash-star block comments) — a semicolon the
 *  tokenizer files under punctuation (group 6) is the only kind that ends a statement, because
 *  the string and comment groups swallow theirs whole. Trimmed empties (a trailing ;, a
 *  comment-only stretch) yield nothing. Pure. */
function dbSplitStatements(text: string): string[] {
  var t = String(text);
  var out: string[] = [];
  var start = 0;
  // A piece holding no token but comments is not a statement — "SELECT 1; -- note\n; SELECT 2"
  // must run two statements, not three, and never ask the server to execute a note.
  var hasCode = function (piece: string): boolean {
    // SQL_TOKEN_RE is shared with the outer scan and keeps ITS position in lastIndex —
    // borrow it, then put the position back, or the outer loop restarts from the top of
    // the text at every semicolon and never terminates.
    var resume = SQL_TOKEN_RE.lastIndex;
    SQL_TOKEN_RE.lastIndex = 0;
    try {
      var m: RegExpExecArray | null;
      while ((m = SQL_TOKEN_RE.exec(piece)) !== null) {
        if (!m[1]) return true; // anything that is not a comment is code
      }
      return false;
    } finally {
      SQL_TOKEN_RE.lastIndex = resume;
    }
  };
  var cut = function (at: number): void {
    var piece = t.slice(start, at).replace(/^\s+|\s+$/g, "");
    if (piece && hasCode(piece)) out.push(piece);
    start = at + 1;
  };
  var m: RegExpExecArray | null;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(t)) !== null) {
    if (m[6]) {
      for (var k = 0; k < m[0].length; k++) {
        if (m[0].charAt(k) === ";") cut(m.index + k);
      }
    }
  }
  cut(t.length); // the tail after the last semicolon (cut ignores the separator itself here)
  return out;
}

/** docs/22 W4.3: a result tab's name — the statement's first word plus its row count
 *  ("SELECT · 42"), the shape dbgate's ResultTabs use. Leading comments are skipped (they are
 *  not the word the user recognises); EXPLAIN answers name themselves because the prefix ran
 *  too. A missing count renders the word alone; a statement with no word at all still gets a
 *  name. Pure. */
function dbResultTabLabel(stmt: string, rowCount: number | null | undefined): string {
  var s = String(stmt).replace(/^(\s|--[^\n]*\n|#[^\n]*\n|\/\*[\s\S]*?\*\/)+/, "");
  var w = (/^([A-Za-z_][A-Za-z0-9_]*)/.exec(s) || [])[1] || "?";
  w = w.toUpperCase();
  return typeof rowCount === "number" ? w + " \u00b7 " + rowCount : w;
}

// One run at a time owns the results pane: a slow run's answer that lands after a newer Run
// started must be dropped, or it would overwrite the newer run's tabs with the old ones
// (docs/22 closeout audit). The token is issued only when a request is actually about to
// fire — an early guard refusal (empty console) must not invalidate a run in flight.
var dbRunReq = dbReqGuard();

async function dbRunSql(explain?: string | false): Promise<void> { // falsy runs the statement(s); "plan"|"analyze" prefix EXPLAIN
  var d = state.db;
  if (!d!.conn) { toast("No database connection", true); return; }
  // docs/22 W1.8: the run covers the block the caret is in — one block per run keeps the
  // single-statement guard honest on multi-part scripts.
  var ta = $<HTMLTextAreaElement>("dbSql");
  var block = dbSubqueryAt(d!.sqlText || "", ta ? ta.selectionStart : null).trim();
  if (!block) { toast("Type a command first", true); return; }
  // The redis console: one command per run; the server-side guard still refuses what would
  // break the shared connection or the server. Writes (SET, DEL, EXPIRE…) run.
  if (dbIsRedis()) {
    var rtoken = dbRunReq.issue();
    d!.sqlBusy = true;
    renderDbToolbar();
    renderDbGrid();
    var cj = await apiJson<{ reply?: { length?: number } | string | number | boolean | null; elapsedMs?: number }>("/api/db/" + encodeURIComponent(d!.conn!) + "/command", {
      method: "POST",
      body: JSON.stringify({ command: block }),
    });
    if (!dbRunReq.accepts(rtoken)) return; // superseded: a newer run owns the pane and the flag
    d!.sqlBusy = false;
    if (!cj) { d!.sqlResult = null; d!.sqlResults = null; d!.sqlTab = 0; renderDbToolbar(); renderDbGrid(); return; }
    dbClearSel(); // a new result grid starts unselected
    d!.sqlResult = { columns: ["reply"], rows: [{ reply: cj.reply }], rowCount: 1, explained: false,
      elapsedMs: cj.elapsedMs,
      note: typeof cj.reply === "object" && cj.reply && cj.reply.length != null ? cj.reply.length + " items" : undefined };
    d!.sqlResults = [d!.sqlResult!]; // docs/22 W4.3: the tab strip reads the list — one reply, one tab
    d!.sqlTab = 0;
    dbHistoryPush(block);
    renderDbToolbar();
    renderDbGrid();
    return;
  }
  // docs/22 W4.3: the block is split on statement-level semicolons and sent ONE STATEMENT PER
  // REQUEST — the server's single-statement contract is untouched, and every reply gets its
  // own result tab. Empty stretches (a trailing ;, a comment-only piece) never run; the first
  // failure stops the batch with the tabs that already answered kept on screen; history
  // records the whole block, and only when every statement answered.
  var stmts = dbSplitStatements(block);
  if (!stmts.length) { toast("Type a command first", true); return; }
  var token = dbRunReq.issue();
  d!.sqlBusy = true;
  d!.sqlResult = null; // "Running…" paints in place of the previous grid
  d!.sqlResults = null;
  renderDbToolbar();
  renderDbGrid();
  var results: DbQueryReply[] = [];
  var ok = true;
  for (var si = 0; si < stmts.length; si++) {
    // The plan view runs EXPLAIN (or EXPLAIN ANALYZE) on each statement; dbWithExplain is
    // idempotent, so a query that already explains itself is sent as-is.
    var toSend = explain ? dbWithExplain(stmts[si], explain) : stmts[si];
    var j = await apiJson<DbQueryReply>("/api/db/" + encodeURIComponent(d!.conn!) + "/query", {
      method: "POST",
      body: JSON.stringify({ sql: toSend, limit: d!.pageSize }),
    });
    if (!dbRunReq.accepts(token)) return; // superseded mid-batch: stop quietly, the newer run owns the pane
    if (!j) { ok = false; break; } // apiJson already showed the error; the answered tabs stay
    j.explained = !!explain;
    j.tabLabel = dbResultTabLabel(toSend, j.rowCount);
    results.push(j);
  }
  d!.sqlBusy = false;
  dbClearSel(); // a new result grid starts unselected
  d!.sqlResults = results;
  d!.sqlTab = 0;
  d!.sqlResult = results.length ? results[0] : null;
  // The plan of a query is not a query — only whole, real runs are history.
  if (ok && !explain) dbHistoryPush(block);
  renderDbToolbar();
  renderDbGrid();
}

export { dbApplyReadback, dbCommit, dbFavoriteName, dbFavLoad, dbFavPush, dbFormatSql, dbFillConsole, dbHistoryLoad, dbHistoryPush, dbHistoryRender, dbHistorySave, dbPendingSql, dbQuoteIdentSafe, dbResultTabLabel, dbRunSql, dbSqlLiteral, dbSplitStatements, dbStatsSql, dbSubqueryAt, dbTemplateSql, dbWithExplain, renderDbBar };
