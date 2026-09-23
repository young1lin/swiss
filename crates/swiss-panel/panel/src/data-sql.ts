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

import type { ApiDbColumn, ApiDbConnectionRow, DbQueryReply } from "./types/api.js";
import type { DbInsert } from "./types/state.js";
import { $, apiJson, dbReqGuard, el, errText, lsMigrate, toast } from "./util.js";
import { fill, h } from "./h.js";
import type { HChild } from "./h.js";
import {
  dbIsRedis, dbRedisCommandText, dbRedisCommands, dbRedisCommit, dbRedisDiscard,
  dbRedisPendingCount,
} from "./data-browsers.js";
import { SQL_TOKEN_RE, dbHighlightNodes, dbSqlPaint } from "./data-filters.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { DB_HISTORY_KEY, DB_HISTORY_MAX, dbClearSel, dbDropEdits, dbPending, dbPkKey } from "./data-view.js";
import { dbConn, dbSqlTab, dbTab } from "./db-state.js";
import { dbLastTableTab, dbOpenTab, renderDbTabs } from "./data-tabs.js";
import type { DbKeyTab } from "./types/state.js";
import { tr, trn } from "./i18n.js";

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
  const q = (n: string): string => {
    const safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  const target = (schema ? q(schema) + "." : "") + q(table);
  const c = q(column);
  if (kind === "num") {
    return "SELECT COUNT(" + c + ") AS count, MIN(" + c + ") AS min, MAX(" + c + ") AS max, " +
      "AVG(" + c + ") AS avg\nFROM " + target;
  }
  return "SELECT " + c + ", COUNT(*) AS count\nFROM " + target + "\nGROUP BY 1\nORDER BY 2 DESC\nLIMIT 50";
}

function dbPendingSql(): string[] {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return [];
  if (!d.data) return [];
  const dialect = (c.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === c.conn; }) || {} as { dialect?: string }).dialect || "mysql";
  const q = (n: string): string => {
    const safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  const table = (d.schema ? q(d.schema) + "." : "") + q(d.table!);
  const pkCols = d.data.primaryKey || [];
  // docs/22 W4.1 (W4b follow-up): the preview sketches the address the server really builds —
  // pk columns for a pk table, EVERY column for a keyless one (the row buffer carries whole
  // rows there), with the md5 fold the server applies (EDIT_ADDR_MD5_MIN = 64). md5() here is
  // a sketch of what gets digested, not the digest itself: the browser has no md5 and the
  // server computes the real one. A NULL or missing column cannot address anything (col =
  // NULL matches nothing) — the server refuses the row, so the preview says so instead of
  // printing an empty or lying WHERE.
  const addrCols = pkCols.length ? pkCols : (d.data.columns || []).map((c: ApiDbColumn): string => { return c.name; });
  const addrTerm = (c: string, v: unknown): string => {
    if (!pkCols.length && v === null) throw new Error("no primary key: " + c + " is NULL — a row needs a non-NULL value for every column");
    if (!pkCols.length && v === undefined) throw new Error("no primary key: " + c + " is missing — a row needs a value for every column");
    const s = typeof v === "string" ? v : String(v);
    if (/^\\x[0-9a-fA-F]*$/.test(s) || s.length > 64) return "MD5(" + q(c) + ") = md5(" + dbSqlLiteral(s) + ")";
    return q(c) + " = " + dbSqlLiteral(v);
  };
  const rowWhere = (addr: Record<string, unknown>): string => {
    return addrCols.map((c: string): string => { return addrTerm(c, addr[c]); }).join(" AND ");
  };
  const out: string[] = [];
  Object.keys(d.deletes).forEach((k: string): void => {
    out.push("DELETE FROM " + table + " WHERE " + rowWhere(d!.deletes[k]) + ";");
  });
  Object.keys(d.updates).forEach((k: string): void => {
    const e = d!.updates[k];
    const set = Object.keys(e.changes).map((c: string): string => { return q(c) + " = " + dbSqlLiteral(e.changes[c]); }).join(", ");
    out.push("UPDATE " + table + " SET " + set + " WHERE " + rowWhere(e.pk) + ";");
  });
  d.inserts.forEach((ins: DbInsert): void => {
    const cols = Object.keys(ins.values);
    if (!cols.length) return;
    out.push("INSERT INTO " + table + " (" + cols.map(q).join(", ") + ") VALUES (" +
      cols.map((c: string): string => { return dbSqlLiteral(ins.values[c]); }).join(", ") + ");");
  });
  return out;
}
/* --- buffered edits: the commit / discard bar ---------------------------------------------------- */

/** The redis arm of the bar (docs/22 W3.3): same slot, same words, same Discard, with
 *  "Commands" in place of "SQL" and one pipeline in place of the transaction. The command
 *  list is the very list Commit posts — the preview is the payload, not a paraphrase. */
function renderDbRedisBar(d: DbKeyTab, bar: HTMLElement): void {
  const b = d.redisEdits!;
  const u = Object.keys(b.updates).length;
  const del = Object.keys(b.deletes).length;
  const ins = b.inserts.length;
  const parts = [];
  if (u) parts.push(trn(u, "dataSql.nUpdates.one", "dataSql.nUpdates.other"));
  if (del) parts.push(trn(del, "dataSql.nDeletes.one", "dataSql.nDeletes.other"));
  if (ins) parts.push(trn(ins, "dataSql.nInserts.one", "dataSql.nInserts.other"));
  bar.appendChild(h("span", null, tr("dataSql.partsLocalOnlyRedis", { parts: parts.join(", ") })));
  // No per-button handlers (docs/37 R5): the buttons carry data-bar addresses and #pane's
  // delegated click answers them from live state.
  bar.appendChild(h("button", { class: "btn", title: tr("dataSql.showExactCommandsCommit"), data: { bar: "preview" } },
    d.sqlPreview ? tr("dataSql.hideCommands") : tr("dataSql.commands")));
  bar.appendChild(h("button", { class: "btn", data: { bar: "discard" } }, tr("dataSql.discard")));
  bar.appendChild(h("button", { class: "btn commit", data: { bar: "commit" } }, tr("dataSql.commitN1Pipeline")));
  if (d.sqlPreview) {
    // The command list is plain text, not SQL — no highlight pass (unlike the SQL bar below).
    let body: string;
    try {
      const cmds = dbRedisCommands(d.redisKey!, b.type, b);
      body = "-- " + trn(cmds.length, "dataSql.commandsPreviewHead.one", "dataSql.commandsPreviewHead.other") +
        "\n" + cmds.map(dbRedisCommandText).join("\n");
    } catch (e) {
      body = errText(e);
    }
    const pre = h("pre", { class: "db-ddl", style: "position:static;margin:0;margin-top:var(--s2);width:100%" }, body);
    bar.style.flexWrap = "wrap";
    bar.appendChild(pre);
  }
}

function renderDbBar(): void {
  const d = dbTab();
  const bar = $("dbBar");
  if (!bar) return;
  // The card's dirty dot counts exactly what this bar summarises (docs/42 T2), so the strip is
  // repainted with it — and at the TOP, because the bar has several exits (a tab with nothing
  // buffered hides it and returns early) and the dot must go when the count does.
  renderDbTabs();
  const redis = dbIsRedis();
  // The redis bar owns the same slot (docs/22 W3.3): the typed value view buffers edits the
  // way the row grid does, and its Commit is ONE guarded pipeline instead of a transaction.
  // The counts and the buffers both read the ACTIVE tab (docs/42 T1) — a key tab's edits, a
  // table tab's — so the wrong kind's tab hides the bar exactly like a missing page did.
  const n = redis ? dbRedisPendingCount() : dbPending();
  const payload = redis ? (d.kind === "key" ? d.redisValue : null) : (d.kind === "table" ? d.data : null);
  if (!n || !payload) { bar.hidden = true; return; }
  bar.hidden = false;
  bar.style.flexWrap = "nowrap";
  fill(bar);
  if (redis) {
    if (d.kind === "key") renderDbRedisBar(d, bar);
    return;
  }
  if (d.kind !== "table" || !d.data) { bar.hidden = true; return; }
  const dt = d; // narrowed by the guard above — a plain alias, not a cast
  const u = Object.keys(dt.updates).length;
  const del = Object.keys(dt.deletes).length;
  const ins = dt.inserts.length;
  const parts: string[] = [];
  if (u) parts.push(trn(u, "dataSql.nUpdates.one", "dataSql.nUpdates.other"));
  if (del) parts.push(trn(del, "dataSql.nDeletes.one", "dataSql.nDeletes.other"));
  if (ins) parts.push(trn(ins, "dataSql.nInserts.one", "dataSql.nInserts.other"));
  // Same addressing honesty as the Commit gate (docs/22 W4b follow-up): the bar names the
  // WHERE the server will build, pk or whole-row.
  const pkColsB = (dt.data && dt.data.primaryKey) || [];
  bar.appendChild(h("span", null, tr("dataSql.partsLocalOnlyDatabase", { parts: parts.join(", "), how: pkColsB.length ? tr("dataSql.primaryKey") : tr("dataSql.allColumnsTablePrimary") })));
  // No per-button handlers (docs/37 R5): data-bar addresses, answered by #pane's delegated
  // click with the counts read from live state at event time.
  bar.appendChild(h("button", { class: "btn", title: tr("dataSql.showExactStatementsCommit"), data: { bar: "preview" } },
    dt.sqlPreview ? tr("dataSql.hideSql") : tr("dataSql.sql")));
  bar.appendChild(h("button", { class: "btn", data: { bar: "discard" } }, tr("dataSql.discard")));
  bar.appendChild(h("button", { class: "btn commit", data: { bar: "commit" } }, tr("dataSql.commitN1Transaction")));
  if (dt.sqlPreview) {
    let body: HChild;
    try {
      const stmts = dbPendingSql();
      body = dbHighlightNodes("-- " + trn(stmts.length, "dataSql.statementsPreviewHead.one", "dataSql.statementsPreviewHead.other") +
        "\n" + stmts.join("\n"));
    } catch (e) {
      body = errText(e);
    }
    bar.style.flexWrap = "wrap";
    bar.appendChild(h("pre", { class: "db-ddl", style: "position:static;margin:0;margin-top:var(--s2);width:100%" }, body));
  }
}

/** #pane's delegated click for the edit bar (docs/37 R5). Behavior note (docs/37 §10.1): the
 *  Discard confirm's count and the preview toggle's label are resolved from LIVE state at
 *  event time — the render-time closure could ask "Discard 3 changes?" about a buffer a
 *  keyboard paste had already grown to 4. */
function dbBarClick(t: Element): boolean {
  const b = t.closest<HTMLElement>("[data-bar]");
  if (!b) return false;
  const d = dbTab();
  const which = b.dataset.bar;
  if (which === "preview") {
    d.sqlPreview = !d.sqlPreview;
    renderDbBar();
    return true;
  }
  if (which === "discard") {
    const n = dbIsRedis() ? dbRedisPendingCount() : dbPending();
    if (dbIsRedis()) dbRedisDiscard();
    else {
      if (!confirm(tr("dataSql.discardBufferedConfirm", { n: trn(n, "dataSql.nBufferedChanges.one", "dataSql.nBufferedChanges.other") }))) return true;
      dbDropEdits();
      renderDbGrid(); renderDbBar();
    }
    return true;
  }
  if (which === "commit") {
    if (dbIsRedis()) void dbRedisCommit();
    else void dbCommit();
    return true;
  }
  return false;
}

/** docs/22 W1.7: fold one committed row's read-back into the page's rows. The commit reply
 *  carries what the SERVER kept — silent truncation, DEFAULTs, trigger rewrites — so patching it
 *  in shows the truth immediately, whatever the reload race does next. Matching uses the same
 *  pk key the grid's edit buffer uses. */
function dbApplyReadback(rows: Record<string, unknown>[] | null, pkCols: string[], pk: Record<string, unknown> | null, row: Record<string, unknown> | null): Record<string, unknown>[] | null {
  if (!rows || !pkCols || !pkCols.length || !row) return rows;
  const want = dbPkKey(pkCols, pk!);
  rows.forEach((r: Record<string, unknown>): void => {
    if (dbPkKey(pkCols, r) === want) {
      Object.keys(row).forEach((c: string): void => { r[c] = row[c]; });
    }
  });
  return rows;
}

/** docs/22 W1.4 / W1.10: put a generated statement into the console — visible, editable, and
 *  in history once run, instead of hiding behind a one-off request. docs/42 T2: the console is
 *  an object, so this OPENS it (or activates the one already open) and then writes into it. */
function dbFillConsole(sql: string): void {
  dbOpenTab({ kind: "sql" });
  const st = dbSqlTab();
  if (!st) return; // the strip refused the open — the toast has already said why
  st.sqlText = sql;
  const ta = $<HTMLTextAreaElement>("dbSql");
  if (ta) ta.value = sql;
  dbSqlPaint();
  if (ta) ta.focus();
}

async function dbCommit(): Promise<void> {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!c.conn || !d.table || !d.data) return;
  const edits: { op: string; pk?: Record<string, unknown>; changes?: Record<string, unknown>; values?: Record<string, unknown> }[] = [];
  Object.keys(d.deletes).forEach((k: string): void => { edits.push({ op: "delete", pk: d.deletes[k] }); });
  Object.keys(d.updates).forEach((k: string): void => {
    const e = d!.updates[k];
    edits.push({ op: "update", pk: e.pk, changes: e.changes });
  });
  d.inserts.forEach((ins: DbInsert): void => { edits.push({ op: "insert", values: ins.values }); });
  if (!edits.length) return;
  // The explicit transaction gate: nothing leaves this browser until the user says so HERE too.
  // The summary names the table and counts, because "commit 3 changes" must be a decision, not a reflex.
  const ups = Object.keys(d.updates).length, dels = Object.keys(d.deletes).length, ins = d.inserts.length;
  const parts: string[] = [];
  if (ups) parts.push(trn(ups, "dataSql.nUpdates.one", "dataSql.nUpdates.other"));
  if (dels) parts.push(trn(dels, "dataSql.nDeletes.one", "dataSql.nDeletes.other"));
  if (ins) parts.push(trn(ins, "dataSql.nInserts.one", "dataSql.nInserts.other"));
  const tableLabel = (d.schema ? d.schema + "." : "") + d.table!;
  // docs/22 W4.1 (W4b follow-up): the gate names the address the server will really use — a
  // keyless table commits with whole-row WHEREs, and the user deserves that in the decision.
  const pkColsC = (d.data && d.data.primaryKey) || [];
  const addressed = pkColsC.length
    ? tr("dataSql.addressedByPk")
    : tr("dataSql.addressedByAllColumns");
  if (!confirm(tr("dataSql.commitToTable", { parts: parts.join(", "), table: tableLabel, how: addressed }))) {
    return; // cancelled — the buffer stays, nothing was sent
  }
  const j = await apiJson<{ results?: { affected?: number; row?: Record<string, unknown> }[] }>("/api/db/" + encodeURIComponent(c.conn!) + "/edits", {
    method: "POST",
    body: JSON.stringify({ table: d.table, schema: d.schema, edits: edits }),
  });
  if (!j) return; // the server rolled back; the buffer stays exactly as it was
  const affected = (j.results || []).reduce((a: number, r: { affected?: number }): number => { return a + (r.affected || 0); }, 0);
  toast(trn(edits.length, "dataSql.committedChange.one", "dataSql.committedChange.other",
    { rows: trn(affected, "dataSql.nRowsAffected.one", "dataSql.nRowsAffected.other") }));
  // docs/22 W1.7: each update's read-back row lands on the page before the reload, so the
  // committed truth (truncated, defaulted, trigger-rewritten) is what the grid shows next.
  const pkCols = (d.data && d.data.primaryKey) || [];
  (j.results || []).forEach((r: { affected?: number; row?: Record<string, unknown> }, i: number): void => {
    if (r && r.row && edits[i] && edits[i].op === "update" && d.data && d.data.rows) {
      dbApplyReadback(d.data.rows, pkCols, edits[i].pk!, r.row);
    }
  });
  dbDropEdits();
  // docs/22 closeout B4: the reload rebuilds the grid and scroll anchoring is OFF by design
  // (docs/22 W2.2 — a repaint must never jump the pane), so the commit would otherwise snap
  // the user back to the top, away from the row they just committed. Capture before the
  // reload, restore after it lands; a superseding load owns the pane by then and a stale
  // restore is a harmless scroll to where the user was anyway.
  const wrap = $("dbGridWrap");
  const scrollTop = wrap ? wrap.scrollTop : 0;
  void dbLoadData(true).then((): void => {
    const w2 = $("dbGridWrap");
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
  const s = sql.replace(/\s+$/, "").replace(/;\s*$/, "").replace(/\s+$/, "");
  if (/^explain\b/i.test(s)) return s;
  return mode === "analyze" ? "EXPLAIN ANALYZE " + s : "EXPLAIN " + s;
}

/* --- query history (per-browser) ---------------------------------------------------------------- */

function dbHistoryLoad(): void {
  const d = dbConn();
  // fix-plan #17: swiss.dbSqlHistory replaced mcp_gateway_db_sql_history; the first read
  // carries an old value across and deletes the old key, a fresh browser stays untouched.
  try { d.history = JSON.parse(lsMigrate(DB_HISTORY_KEY, "mcp_gateway_db_sql_history") as string) || []; }
  catch (e) { d.history = []; }
}

function dbHistorySave(): void {
  try { localStorage.setItem(DB_HISTORY_KEY, JSON.stringify(dbConn().history.slice(0, DB_HISTORY_MAX))); }
  catch (e) { /* full or blocked — history is a convenience, not state */ }
}

/** Record a successful run: newest first, a repeat of the current head is a no-op, and the
 *  list stays bounded. */
function dbHistoryPush(sql: string): void {
  const d = dbConn();
  if (d.history[0] === sql) return;
  d.history.unshift(sql);
  d.history = d.history.slice(0, DB_HISTORY_MAX);
  dbHistorySave();
  dbHistoryRender();
}

function dbHistoryRender(): void {
  const sel = $("dbSqlHistory");
  if (!sel) return;
  sel.textContent = "";
  const head = el("option", "", tr("dataView.history")) as HTMLOptionElement;
  head.value = "";
  sel.appendChild(head);
  const d = dbConn();
  // docs/22 W5.4: one dropdown, two groups — what ran (history) and what was starred
  // (favorites). The value carries the group: a plain index is history, "f"+i a favorite.
  if (d.history && d.history.length) {
    const og = el("optgroup") as HTMLOptGroupElement;
    og.label = tr("dataView.history");
    d.history.forEach((sql: string, i: number): void => {
      const o = el("option", "", sql.replace(/\s+/g, " ").slice(0, 80)) as HTMLOptionElement;
      o.value = String(i);
      o.title = sql;
      og.appendChild(o);
    });
    sel.appendChild(og);
  }
  if (d.favorites && d.favorites.length) {
    const fg = el("optgroup") as HTMLOptGroupElement;
    fg.label = tr("dataSql.favorites");
    d.favorites.forEach((sql: string, i: number): void => {
      const o = el("option", "", dbFavoriteName(sql)) as HTMLOptionElement;
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

const DB_FAV_KEY = "swiss.dbFavorites"; // fix-plan #17: was mcp_gateway_db_favorites; migrated on read
const DB_FAV_MAX = 50;

function dbFavLoad(): void {
  const d = dbConn();
  try { d.favorites = JSON.parse(lsMigrate(DB_FAV_KEY, "mcp_gateway_db_favorites") as string) || []; }
  catch (e) { d.favorites = []; }
}

function dbFavSave(): void {
  try { localStorage.setItem(DB_FAV_KEY, JSON.stringify(dbConn().favorites.slice(0, DB_FAV_MAX))); }
  catch (e) { /* full or blocked — favorites are a convenience, not state */ }
}

/** The option's name: the query's FIRST line, whitespace-folded, cut at the history's 80. */
function dbFavoriteName(sql: string): string {
  const first = String(sql).split("\n")[0].replace(/\s+/g, " ").trim();
  return first.slice(0, 80);
}

function dbFavPush(sql: string): void {
  const d = dbConn();
  const s = String(sql == null ? "" : sql);
  if (!s.trim()) { toast(tr("dataSql.nothingSaveConsoleEmpty"), true); return; }
  d.favorites = (d.favorites || []).filter((f: string): boolean => { return f !== s; });
  d.favorites.unshift(s);
  d.favorites = d.favorites.slice(0, DB_FAV_MAX);
  dbFavSave();
  dbHistoryRender();
  toast(tr("dataSql.savedFavorites"));
}

/* --- the lightweight SQL formatter (docs/22 W5.4) ------------------------------------------------ */
/* Lexical only — the console's own SQL_TOKEN_RE stream, never a parse: top-level clause
   keywords break onto their own lines (a join lead breaks with its join), AND/OR continue
   two spaces under their clause, statements split at the top-level semicolon, a line
   comment ends its line, and punctuation spacing is normalized (no space before , ) ; .,
   none after ( .). Case, tokens, strings and comments ride byte-identical, so a formatted
   statement runs exactly as it did — the whitespace-equivalence the round-trip test pins. */

const DB_FORMAT_CLAUSES = "select from where group order having limit offset fetch union except intersect values set insert update delete with join".split(" ");
const DB_FORMAT_JOIN_LEADS = "left right inner outer full cross natural".split(" ");

function dbFormatSql(text: string): string {
  const tokens: string[] = [];
  let m: RegExpExecArray | null;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(String(text == null ? "" : text))) !== null) {
    if (m[5]) continue; // whitespace is rebuilt, never carried
    // Punctuation runs split into characters: "((" and ")," are two spacing decisions each.
    if (m[6]) { for (let ci = 0; ci < m[0].length; ci++) tokens.push(m[0][ci]); }
    else tokens.push(m[0]);
  }
  let out = "";
  let depth = 0;
  let prev: string | null = null; // the last token on the CURRENT line; null = a fresh line start
  for (let i = 0; i < tokens.length; i++) {
    const tok = tokens[i];
    const low = tok.toLowerCase();
    if (depth === 0 && prev !== null && out !== "" && /^[A-Za-z_]/.test(tok)) {
      const nxt = tokens[i + 1];
      // "join" does not break again when its lead (left/right/inner/...) just broke.
      const joinAgain = low === "join" && DB_FORMAT_JOIN_LEADS.includes(prev.toLowerCase());
      const clause = !joinAgain && (DB_FORMAT_CLAUSES.includes(low) ||
        (DB_FORMAT_JOIN_LEADS.includes(low) && nxt && nxt.toLowerCase() === "join"));
      const cont = low === "and" || low === "or";
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
  const q = (n: string): string => {
    const safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  const t = (schema ? q(schema) + "." : "") + q(table!);
  const cols = columns.map(q);
  const key = pk.map(q);
  const hint = "-- " + tr("dataSql.replaceEachQ");
  if (kind === "select") {
    return hint + "\nSELECT " + cols.join(", ") + "\nFROM " + t +
      (key.length ? "\nWHERE " + key.map((c) => { return c + " = ?"; }).join(" AND ") : "") + ";";
  }
  if (kind === "insert") {
    return hint + "\nINSERT INTO " + t + " (" + cols.join(", ") + ")\nVALUES (" +
      cols.map((): string => { return "?"; }).join(", ") + ");";
  }
  if (kind === "update") {
    const keyNames: Record<string, boolean> = {};
    pk.forEach((p: string): void => { keyNames[p] = true; });
    const set = columns.filter((n: string): boolean => { return !keyNames[n]; }).map(q);
    if (!set.length || !key.length) {
      throw new Error(tr("dataSql.updateTemplateNeeds"));
    }
    return hint + "\nUPDATE " + t + "\nSET " + set.map((c: string): string => { return c + " = ?"; }).join(", ") +
      "\nWHERE " + key.map((c: string): string => { return c + " = ?"; }).join(" AND ") + ";";
  }
  if (!key.length) throw new Error(tr("dataSql.deleteTemplateNeeds"));
  return hint + "\nDELETE FROM " + t + "\nWHERE " + key.map((c: string): string => { return c + " = ?"; }).join(" AND ") + ";";
}

/** docs/22 W1.8: split SQL text into blank-line-separated blocks and return the one the caret
 *  sits in — Ctrl+Enter on a three-block script runs only the second block. A caret inside a
 *  blank gap belongs to the block AFTER it (that is where the cursor visually rests); a missing
 *  caret means the end of the text. No blank lines means one block: exactly the whole box, the
 *  behaviour the console always had. */
function dbSubqueryAt(text: string, caret: number | null): string {
  const t = String(text);
  const seps: number[][] = [];
  t.replace(/\n[ \t]*\n/g, (m: string, i: number): string => { seps.push([i, i + m.length]); return m; });
  if (!seps.length) return t;
  const at = typeof caret === "number" && caret >= 0 && caret <= t.length ? caret : t.length;
  let start = 0, end = t.length;
  for (let i = 0; i < seps.length; i++) {
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
  const t = String(text);
  const out: string[] = [];
  let start = 0;
  // A piece holding no token but comments is not a statement — "SELECT 1; -- note\n; SELECT 2"
  // must run two statements, not three, and never ask the server to execute a note.
  const hasCode = (piece: string): boolean => {
    // SQL_TOKEN_RE is shared with the outer scan and keeps ITS position in lastIndex —
    // borrow it, then put the position back, or the outer loop restarts from the top of
    // the text at every semicolon and never terminates.
    const resume = SQL_TOKEN_RE.lastIndex;
    SQL_TOKEN_RE.lastIndex = 0;
    try {
      let m: RegExpExecArray | null;
      while ((m = SQL_TOKEN_RE.exec(piece)) !== null) {
        if (!m[1]) return true; // anything that is not a comment is code
      }
      return false;
    } finally {
      SQL_TOKEN_RE.lastIndex = resume;
    }
  };
  const cut = (at: number): void => {
    const piece = t.slice(start, at).replace(/^\s+|\s+$/g, "");
    if (piece && hasCode(piece)) out.push(piece);
    start = at + 1;
  };
  let m: RegExpExecArray | null;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(t)) !== null) {
    if (m[6]) {
      for (let k = 0; k < m[0].length; k++) {
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
  const s = String(stmt).replace(/^(\s|--[^\n]*\n|#[^\n]*\n|\/\*[\s\S]*?\*\/)+/, "");
  let w = (/^([A-Za-z_][A-Za-z0-9_]*)/.exec(s) || [])[1] || "?";
  w = w.toUpperCase();
  return typeof rowCount === "number" ? w + " \u00b7 " + rowCount : w;
}

// One run at a time owns the results pane: a slow run's answer that lands after a newer Run
// started must be dropped, or it would overwrite the newer run's tabs with the old ones
// (docs/22 closeout audit). The token is issued only when a request is actually about to
// fire — an early guard refusal (empty console) must not invalidate a run in flight.
const dbRunReq = dbReqGuard();

/* The console's row cap (docs/22 W5.3): the rows-per-page the operator picked on whatever
   table tab they were last reading, or the fresh default when the strip holds no table at
   all. It is a density preference, not one table's property — which is why the console keeps
   honoring it now that it sits in a tab of its own (docs/42 T2). */
function dbRunLimit(): number {
  const t = dbLastTableTab();
  return t ? t.pageSize : 50;
}

async function dbRunSql(explain?: string | false): Promise<void> { // falsy runs the statement(s); "plan"|"analyze" prefix EXPLAIN
  const c = dbConn();
  // The console's text, its busy flag and its replies all live on the sql TAB (docs/42 T2):
  // `d` below is that tab, so a run that outlives a tab switch writes where it came from and
  // the request guard decides whether the pane repaints.
  const d = dbSqlTab();
  if (!d) return; // Run reached us with no console open — nothing to run
  if (!c.conn) { toast(tr("dataSql.databaseConnection"), true); return; }
  // docs/22 W1.8: the run covers the block the caret is in — one block per run keeps the
  // single-statement guard honest on multi-part scripts.
  const ta = $<HTMLTextAreaElement>("dbSql");
  const block = dbSubqueryAt(d.sqlText || "", ta ? ta.selectionStart : null).trim();
  if (!block) { toast(tr("dataSql.typeCommandFirst"), true); return; }
  // The redis console: one command per run; the server-side guard still refuses what would
  // break the shared connection or the server. Writes (SET, DEL, EXPIRE…) run.
  if (dbIsRedis()) {
    const rtoken = dbRunReq.issue();
    d.sqlBusy = true;
    renderDbToolbar();
    renderDbGrid();
    const cj = await apiJson<{ reply?: { length?: number } | string | number | boolean | null; elapsedMs?: number }>("/api/db/" + encodeURIComponent(c.conn!) + "/command", {
      method: "POST",
      body: JSON.stringify({ command: block }),
    });
    if (!dbRunReq.accepts(rtoken)) return; // superseded: a newer run owns the pane and the flag
    d.sqlBusy = false;
    if (!cj) { d.sqlResult = null; d.sqlResults = null; d.resultTab = 0; renderDbToolbar(); renderDbGrid(); return; }
    dbClearSel(); // a new result grid starts unselected
    d.sqlResult = { columns: ["reply"], rows: [{ reply: cj.reply }], rowCount: 1, explained: false,
      elapsedMs: cj.elapsedMs,
      note: typeof cj.reply === "object" && cj.reply && cj.reply.length != null
        ? trn(cj.reply.length, "dataSql.nReplyItems.one", "dataSql.nReplyItems.other") : undefined };
    d.sqlResults = [d.sqlResult!]; // docs/22 W4.3: the tab strip reads the list — one reply, one tab
    d.resultTab = 0;
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
  const stmts = dbSplitStatements(block);
  if (!stmts.length) { toast(tr("dataSql.typeCommandFirst"), true); return; }
  const token = dbRunReq.issue();
  d.sqlBusy = true;
  d.sqlResult = null; // "Running…" paints in place of the previous grid
  d.sqlResults = null;
  renderDbToolbar();
  renderDbGrid();
  const results: DbQueryReply[] = [];
  let ok = true;
  for (let si = 0; si < stmts.length; si++) {
    // The plan view runs EXPLAIN (or EXPLAIN ANALYZE) on each statement; dbWithExplain is
    // idempotent, so a query that already explains itself is sent as-is.
    const toSend = explain ? dbWithExplain(stmts[si], explain) : stmts[si];
    const j = await apiJson<DbQueryReply>("/api/db/" + encodeURIComponent(c.conn!) + "/query", {
      method: "POST",
      // The row cap for a console query is the open table tab's page size — the operator's
      // chosen density — or the fresh default when no table tab is open (docs/42 T1).
      body: JSON.stringify({ sql: toSend, limit: dbRunLimit() }),
    });
    if (!dbRunReq.accepts(token)) return; // superseded mid-batch: stop quietly, the newer run owns the pane
    if (!j) { ok = false; break; } // apiJson already showed the error; the answered tabs stay
    j.explained = !!explain;
    j.tabLabel = dbResultTabLabel(toSend, j.rowCount);
    results.push(j);
  }
  d.sqlBusy = false;
  dbClearSel(); // a new result grid starts unselected
  d.sqlResults = results;
  d.resultTab = 0;
  d.sqlResult = results.length ? results[0] : null;
  // The plan of a query is not a query — only whole, real runs are history.
  if (ok && !explain) dbHistoryPush(block);
  renderDbToolbar();
  renderDbGrid();
}

export { dbApplyReadback, dbBarClick, dbCommit, dbFavoriteName, dbFavLoad, dbFavPush, dbFormatSql, dbFillConsole, dbHistoryLoad, dbHistoryPush, dbHistoryRender, dbHistorySave, dbPendingSql, dbQuoteIdentSafe, dbResultTabLabel, dbRunSql, dbSqlLiteral, dbSplitStatements, dbStatsSql, dbSubqueryAt, dbTemplateSql, dbWithExplain, renderDbBar };
