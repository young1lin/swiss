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

                                                                                    
                                                                  
import { $, api, apiJson, dbReqGuard, errText, toast } from "./util.js";
import { fill, h } from "./h.js";
                                     
import {
  dbIsRedis, dbRedisCommandText, dbRedisCommands, dbRedisCommit, dbRedisDiscard,
  dbRedisPendingCount, dbRefreshKeyspace,
} from "./data-browsers.js";
import { SQL_TOKEN_RE, dbHighlightNodes, dbSqlPaint } from "./data-filters.js";
import { dbFormatSql } from "./data-sql-format.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbClearSel, dbDropEdits, dbPending, dbPkKey } from "./data-view.js";
import { dbSetSqlText, dbConn, dbSqlTab, dbTab, dbTabs } from "./db-state.js";
import { dbActivateTab, dbClaimClosed, dbOpenTab, dbOpenTabForce, dbTabTitle, renderDbTabs } from "./data-tabs.js";
import { dbClosedEntry, dbConsolePut, dbConsoleStoreOn } from "./data-console-store.js";
import { dbClosedAdd, dbClosedById, dbClosedFor, dbClosedSetText } from "./data-closed.js";
import { dbHistoryRefresh } from "./data-history.js";
import { dbIsMongo, mongoCurrentDb, mongoFormatCommand, mongoRunCommand, mongoRunSupersede } from "./data-mongo.js";
import { dockShow } from "./ui/dock.js";
                                                 
import { moduleDbMessage, tr, trn } from "./i18n.js";
import { btn } from "./ui/button.js";

/* --- pending-SQL preview ------------------------------------------------------------------------ */
/* The exact statements the server will run on Commit, mirrored from buildEditStatements
   (dbbrowser.ts) with values INLINED for display only — the real batch runs with bound
   parameters. Used by the bar's SQL button so "what will Commit do" is inspectable before
   it does it. */
function dbQuoteIdentSafe(name        )                {
  return /^[A-Za-z0-9_$]{1,64}$/.test(name) ? name : null; // same gate as the server
}

function dbSqlLiteral(v         )         {
  if (v === null) return "NULL";
  if (typeof v === "number") return String(v);
  return "'" + String(v).replace(/'/g, "''") + "'";
}

/** SPEC §data.grid: the one-column stats statements the header's right-click menu runs. Built with
 *  the same identifier gate as the Commit preview — a column name that is not a bare word is
 *  refused, never bare-spliced into SQL. `kind` picks the shape:
 *  "dist" -> value + frequency (top 50); "num" -> COUNT/MIN/MAX/AVG. */
function dbStatsSql(dialect        , schema        , table        , column        , kind        )         {
  const q = (n        )         => {
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

function dbPendingSql()           {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return [];
  if (!d.data) return [];
  const dialect = (c.conns.find((x                    )          => { return x.name === c.conn; }) || {}                        ).dialect || "mysql";
  const q = (n        )         => {
    const safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  const table = (d.schema ? q(d.schema) + "." : "") + q(d.table );
  const pkCols = d.data.primaryKey || [];
  // SPEC §data.edits (W4b follow-up): the preview sketches the address the server really builds —
  // pk columns for a pk table, EVERY column for a keyless one (the row buffer carries whole
  // rows there), with the md5 fold the server applies (EDIT_ADDR_MD5_MIN = 64). md5() here is
  // a sketch of what gets digested, not the digest itself: the browser has no md5 and the
  // server computes the real one. A NULL or missing column cannot address anything (col =
  // NULL matches nothing) — the server refuses the row, so the preview says so instead of
  // printing an empty or lying WHERE.
  const addrCols = pkCols.length ? pkCols : (d.data.columns || []).map((c             )         => { return c.name; });
  const addrTerm = (c        , v         )         => {
    if (!pkCols.length && v === null) throw new Error("no primary key: " + c + " is NULL — a row needs a non-NULL value for every column");
    if (!pkCols.length && v === undefined) throw new Error("no primary key: " + c + " is missing — a row needs a value for every column");
    const s = typeof v === "string" ? v : String(v);
    if (/^\\x[0-9a-fA-F]*$/.test(s) || s.length > 64) return "MD5(" + q(c) + ") = md5(" + dbSqlLiteral(s) + ")";
    return q(c) + " = " + dbSqlLiteral(v);
  };
  const rowWhere = (addr                         )         => {
    return addrCols.map((c        )         => { return addrTerm(c, addr[c]); }).join(" AND ");
  };
  const out           = [];
  Object.keys(d.deletes).forEach((k        )       => {
    out.push("DELETE FROM " + table + " WHERE " + rowWhere(d .deletes[k]) + ";");
  });
  Object.keys(d.updates).forEach((k        )       => {
    const e = d .updates[k];
    const set = Object.keys(e.changes).map((c        )         => { return q(c) + " = " + dbSqlLiteral(e.changes[c]); }).join(", ");
    out.push("UPDATE " + table + " SET " + set + " WHERE " + rowWhere(e.pk) + ";");
  });
  d.inserts.forEach((ins          )       => {
    const cols = Object.keys(ins.values);
    if (!cols.length) return;
    out.push("INSERT INTO " + table + " (" + cols.map(q).join(", ") + ") VALUES (" +
      cols.map((c        )         => { return dbSqlLiteral(ins.values[c]); }).join(", ") + ");");
  });
  return out;
}
/* --- buffered edits: the commit / discard bar ---------------------------------------------------- */

/** The redis arm of the bar (SPEC §data.redis): same slot, same words, same Discard, with
 *  "Commands" in place of "SQL" and one pipeline in place of the transaction. The command
 *  list is the very list Commit posts — the preview is the payload, not a paraphrase. */
function renderDbRedisBar(d          , bar             )       {
  const b = d.redisEdits ;
  const u = Object.keys(b.updates).length;
  const del = Object.keys(b.deletes).length;
  const ins = b.inserts.length;
  const parts = [];
  if (u) parts.push(trn(u, "dataSql.nUpdates.one", "dataSql.nUpdates.other"));
  if (del) parts.push(trn(del, "dataSql.nDeletes.one", "dataSql.nDeletes.other"));
  if (ins) parts.push(trn(ins, "dataSql.nInserts.one", "dataSql.nInserts.other"));
  bar.appendChild(h("span", { class: "db-bar-sum" }, tr("dataSql.partsLocalOnlyRedis", { parts: parts.join(", ") })));
  // No per-button handlers (SPEC §panel.toolchain): the buttons carry data-bar addresses and #pane's
  // delegated click answers them from live state.
  bar.appendChild(btn(d.sqlPreview ? tr("dataSql.hideCommands") : tr("dataSql.commands"), { title: tr("dataSql.showExactCommandsCommit"), data: { bar: "preview" } }));
  bar.appendChild(btn(tr("dataSql.discard"), { data: { bar: "discard" } }));
  bar.appendChild(btn(tr("dataSql.commitN1Pipeline"), { kind: "primary", data: { bar: "commit" } }));
  if (d.sqlPreview) {
    // The command list is plain text, not SQL — no highlight pass (unlike the SQL bar below).
    let body        ;
    try {
      const cmds = dbRedisCommands(d.redisKey , b.type, b);
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

function renderDbBar()       {
  const d = dbTab();
  const bar = $("dbBar");
  if (!bar) return;
  // The card's dirty dot counts exactly what this bar summarises (SPEC §data.tabs), so the strip is
  // repainted with it — and at the TOP, because the bar has several exits (a tab with nothing
  // buffered hides it and returns early) and the dot must go when the count does.
  renderDbTabs();
  const redis = dbIsRedis();
  // The redis bar owns the same slot (SPEC §data.redis): the typed value view buffers edits the
  // way the row grid does, and its Commit is ONE guarded pipeline instead of a transaction.
  // The counts and the buffers both read the ACTIVE tab (SPEC §data.tabs) — a key tab's edits, a
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
  const parts           = [];
  if (u) parts.push(trn(u, "dataSql.nUpdates.one", "dataSql.nUpdates.other"));
  if (del) parts.push(trn(del, "dataSql.nDeletes.one", "dataSql.nDeletes.other"));
  if (ins) parts.push(trn(ins, "dataSql.nInserts.one", "dataSql.nInserts.other"));
  // Same addressing honesty as the Commit gate (SPEC §data.edits): the bar names the
  // WHERE the server will build, pk or whole-row.
  const pkColsB = (dt.data && dt.data.primaryKey) || [];
  bar.appendChild(h("span", { class: "db-bar-sum" }, tr("dataSql.partsLocalOnlyDatabase", { parts: parts.join(", "), how: pkColsB.length ? tr("dataSql.primaryKey") : tr("dataSql.allColumnsTablePrimary") })));
  // No per-button handlers (SPEC §panel.toolchain): data-bar addresses, answered by #pane's delegated
  // click with the counts read from live state at event time.
  bar.appendChild(btn(dt.sqlPreview ? tr("dataSql.hideSql") : tr("dataSql.sql"), { title: tr("dataSql.showExactStatementsCommit"), data: { bar: "preview" } }));
  bar.appendChild(btn(tr("dataSql.discard"), { data: { bar: "discard" } }));
  bar.appendChild(btn(tr("dataSql.commitN1Transaction"), { kind: "primary", data: { bar: "commit" } }));
  if (dt.sqlPreview) {
    let body        ;
    try {
      const stmts = dbPendingSql();
      body = dbHighlightNodes("-- " + trn(stmts.length, "dataSql.statementsPreviewHead.one", "dataSql.statementsPreviewHead.other") +
        "\n" + stmts.join("\n"), { lined: true });
    } catch (e) {
      body = errText(e);
    }
    bar.style.flexWrap = "wrap";
    bar.appendChild(h("pre", { class: "db-ddl", style: "position:static;margin:0;margin-top:var(--s2);width:100%" }, body));
  }
}

/** #pane's delegated click for the edit bar (SPEC §panel.toolchain). Behavior note: the
 *  Discard confirm's count and the preview toggle's label are resolved from LIVE state at
 *  event time — the render-time closure could ask "Discard 3 changes?" about a buffer a
 *  keyboard paste had already grown to 4. */
function dbBarClick(t         )          {
  const b = t.closest             ("[data-bar]");
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

/** SPEC §data.edits: fold one committed row's read-back into the page's rows. The commit reply
 *  carries what the SERVER kept — silent truncation, DEFAULTs, trigger rewrites — so patching it
 *  in shows the truth immediately, whatever the reload race does next. Matching uses the same
 *  pk key the grid's edit buffer uses. */
function dbApplyReadback(rows                                  , pkCols          , pk                                , row                                )                                   {
  if (!rows || !pkCols || !pkCols.length || !row) return rows;
  const want = dbPkKey(pkCols, pk );
  rows.forEach((r                         )       => {
    if (dbPkKey(pkCols, r) === want) {
      Object.keys(row).forEach((c        )       => { r[c] = row[c]; });
    }
  });
  return rows;
}

/** SPEC §data.grid / W1.10: put a generated statement into the console — visible, editable, and
 *  in history once run, instead of hiding behind a one-off request. SPEC §data.tabs: the console is
 *  an object, so this OPENS it (or activates the one already open) and then writes into it. */
function dbFillConsole(sql        )       {
  dbOpenTab({ kind: "sql" });
  const st = dbSqlTab();
  if (!st) return; // the strip refused the open — the toast has already said why
  dbSetSqlText(st, sql);
  const ta = $                     ("dbSql");
  if (ta) ta.value = sql;
  dbSqlPaint();
  if (ta) ta.focus();
}

async function dbCommit()                {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!c.conn || !d.table || !d.data) return;
  const edits                                                                                                                      = [];
  Object.keys(d.deletes).forEach((k        )       => { edits.push({ op: "delete", pk: d.deletes[k] }); });
  Object.keys(d.updates).forEach((k        )       => {
    const e = d .updates[k];
    edits.push({ op: "update", pk: e.pk, changes: e.changes });
  });
  d.inserts.forEach((ins          )       => { edits.push({ op: "insert", values: ins.values }); });
  if (!edits.length) return;
  // The explicit transaction gate: nothing leaves this browser until the user says so HERE too.
  // The summary names the table and counts, because "commit 3 changes" must be a decision, not a reflex.
  const ups = Object.keys(d.updates).length, dels = Object.keys(d.deletes).length, ins = d.inserts.length;
  const parts           = [];
  if (ups) parts.push(trn(ups, "dataSql.nUpdates.one", "dataSql.nUpdates.other"));
  if (dels) parts.push(trn(dels, "dataSql.nDeletes.one", "dataSql.nDeletes.other"));
  if (ins) parts.push(trn(ins, "dataSql.nInserts.one", "dataSql.nInserts.other"));
  const tableLabel = (d.schema ? d.schema + "." : "") + d.table ;
  // SPEC §data.edits (W4b follow-up): the gate names the address the server will really use — a
  // keyless table commits with whole-row WHEREs, and the user deserves that in the decision.
  const pkColsC = (d.data && d.data.primaryKey) || [];
  const addressed = pkColsC.length
    ? tr("dataSql.addressedByPk")
    : tr("dataSql.addressedByAllColumns");
  if (!confirm(tr("dataSql.commitToTable", { parts: parts.join(", "), table: tableLabel, how: addressed }))) {
    return; // cancelled — the buffer stays, nothing was sent
  }
  const j = await apiJson                                                                      ("/api/db/" + encodeURIComponent(c.conn ) + "/edits", {
    method: "POST",
    body: JSON.stringify({ table: d.table, schema: d.schema, edits: edits }),
  });
  if (!j) return; // the server rolled back; the buffer stays exactly as it was
  const affected = (j.results || []).reduce((a        , r                       )         => { return a + (r.affected || 0); }, 0);
  toast(trn(edits.length, "dataSql.committedChange.one", "dataSql.committedChange.other",
    { rows: trn(affected, "dataSql.nRowsAffected.one", "dataSql.nRowsAffected.other") }));
  // SPEC §data.edits: each update's read-back row lands on the page before the reload, so the
  // committed truth (truncated, defaulted, trigger-rewritten) is what the grid shows next.
  const pkCols = (d.data && d.data.primaryKey) || [];
  (j.results || []).forEach((r                                                      , i        )       => {
    if (r && r.row && edits[i] && edits[i].op === "update" && d.data && d.data.rows) {
      dbApplyReadback(d.data.rows, pkCols, edits[i].pk , r.row);
    }
  });
  dbDropEdits();
  // SPEC §data: the reload rebuilds the grid and scroll anchoring is OFF by design
  // (SPEC §data.grid — a repaint must never jump the pane), so the commit would otherwise snap
  // the user back to the top, away from the row they just committed. Capture before the
  // reload, restore after it lands; a superseding load owns the pane by then and a stale
  // restore is a harmless scroll to where the user was anyway.
  const wrap = $("dbGridWrap");
  const scrollTop = wrap ? wrap.scrollTop : 0;
  void dbLoadData(true).then(()       => {
    const w2 = $("dbGridWrap");
    if (w2) w2.scrollTop = scrollTop;
  });
}

/** EXPLAIN-prefix a console statement — mode "analyze" spells EXPLAIN ANALYZE, which runs
    the statement and times every plan node. Same rules as withExplain on the server:
    idempotent, one trailing terminator stripped. Mirrored here because the server helper is
    TypeScript. */
function dbWithExplain(sql        , mode        )         {
  // ;\s*$ is the server's (and the Rust port's) chain: the old ;\s+$ here could never match
  // once the trailing whitespace had already been stripped, so the terminator survived.
  const s = sql.replace(/\s+$/, "").replace(/;\s*$/, "").replace(/\s+$/, "");
  if (/^explain\b/i.test(s)) return s;
  return mode === "analyze" ? "EXPLAIN ANALYZE " + s : "EXPLAIN " + s;
}

/* --- favorites (SPEC §data.console) -------------------------------------------------------------------- */
/* The starred list: the operator's own, per browser in localStorage, bounded, newest first, a
   repeat save moving to the top rather than duplicating. What RAN is the server's history,
   filed per database (data-history.ts, SPEC §data.history). */

const DB_FAV_KEY = "swiss.dbFavorites";
const DB_FAV_MAX = 50;

function dbFavLoad()       {
  const d = dbConn();
  try { d.favorites = JSON.parse(localStorage.getItem(DB_FAV_KEY)          ) || []; }
  catch (e) { d.favorites = []; }
}

function dbFavSave()       {
  try { localStorage.setItem(DB_FAV_KEY, JSON.stringify(dbConn().favorites.slice(0, DB_FAV_MAX))); }
  catch (e) { /* full or blocked — favorites are a convenience, not state */ }
}

/** A favorite's name: the query's FIRST line, whitespace-folded, cut at 80. */
function dbFavoriteName(sql        )         {
  const first = String(sql).split("\n")[0].replace(/\s+/g, " ").trim();
  return first.slice(0, 80);
}

function dbFavPush(sql        )       {
  const d = dbConn();
  const s = String(sql == null ? "" : sql);
  if (!s.trim()) { toast(tr("dataSql.nothingSaveConsoleEmpty"), true); return; }
  d.favorites = (d.favorites || []).filter((f        )          => { return f !== s; });
  d.favorites.unshift(s);
  d.favorites = d.favorites.slice(0, DB_FAV_MAX);
  dbFavSave();
  toast(tr("dataSql.savedFavorites"));
}

/* --- SQL console --------------------------------------------------------------------------------- */

/** SPEC §data.console: the table menu's SQL templates. Column names pass the identifier whitelist
 *  (never a bare splice), value positions are ? placeholders, and one comment line says what to
 *  do with them — the template lands in the console runnable after the ?s are filled in. */
function dbTemplateSql(kind        , dialect        , schema                    , table                    , columns          , pk          )         {
  const q = (n        )         => {
    const safe = dbQuoteIdentSafe(n);
    if (!safe) throw new Error("not a valid identifier: " + n);
    return dialect === "mysql" ? "`" + safe + "`" : '"' + safe + '"';
  };
  const t = (schema ? q(schema) + "." : "") + q(table );
  const cols = columns.map(q);
  const key = pk.map(q);
  const hint = "-- " + tr("dataSql.replaceEachQ");
  if (kind === "select") {
    return hint + "\nSELECT " + cols.join(", ") + "\nFROM " + t +
      (key.length ? "\nWHERE " + key.map((c) => { return c + " = ?"; }).join(" AND ") : "") + ";";
  }
  if (kind === "insert") {
    return hint + "\nINSERT INTO " + t + " (" + cols.join(", ") + ")\nVALUES (" +
      cols.map(()         => { return "?"; }).join(", ") + ");";
  }
  if (kind === "update") {
    const keyNames                          = {};
    pk.forEach((p        )       => { keyNames[p] = true; });
    const set = columns.filter((n        )          => { return !keyNames[n]; }).map(q);
    if (!set.length || !key.length) {
      throw new Error(tr("dataSql.updateTemplateNeeds"));
    }
    return hint + "\nUPDATE " + t + "\nSET " + set.map((c        )         => { return c + " = ?"; }).join(", ") +
      "\nWHERE " + key.map((c        )         => { return c + " = ?"; }).join(" AND ") + ";";
  }
  if (!key.length) throw new Error(tr("dataSql.deleteTemplateNeeds"));
  return hint + "\nDELETE FROM " + t + "\nWHERE " + key.map((c        )         => { return c + " = ?"; }).join(" AND ") + ";";
}

/** SPEC §data.console: split SQL text into blank-line-separated blocks and return the one the caret
 *  sits in — Ctrl+Enter on a three-block script runs only the second block. A caret inside a
 *  blank gap belongs to the block AFTER it (that is where the cursor visually rests); a missing
 *  caret means the end of the text. No blank lines means one block: exactly the whole box, the
 *  behaviour the console always had. */
function dbSubqueryAt(text        , caret               )         {
  const t = String(text);
  const seps             = [];
  t.replace(/\n[ \t]*\n/g, (m        , i        )         => { seps.push([i, i + m.length]); return m; });
  if (!seps.length) return t;
  const at = typeof caret === "number" && caret >= 0 && caret <= t.length ? caret : t.length;
  let start = 0, end = t.length;
  for (let i = 0; i < seps.length; i++) {
    if (at < seps[i][0]) { end = seps[i][0]; break; }
    start = seps[i][1];
  }
  return t.slice(start, end);
}

/** SPEC §data.console: split a console block into statements at top-level semicolons — the
 *  panel's half of multi-result tabs. A naive text.split(";") breaks on the first literal or
 *  comment that carries one ('a;b', "-- note;", 'it''s;'), so this walks the SAME token stream
 *  the syntax highlighter lexes (SQL_TOKEN_RE: quotes ', \" and ` with backslash and
 *  doubled-quote escapes, -- and # line comments, slash-star block comments) — a semicolon the
 *  tokenizer files under punctuation (group 6) is the only kind that ends a statement, because
 *  the string and comment groups swallow theirs whole. Trimmed empties (a trailing ;, a
 *  comment-only stretch) yield nothing. Pure. */
function dbSplitStatements(text        )           {
  const t = String(text);
  const out           = [];
  let start = 0;
  // A piece holding no token but comments is not a statement — "SELECT 1; -- note\n; SELECT 2"
  // must run two statements, not three, and never ask the server to execute a note.
  const hasCode = (piece        )          => {
    // SQL_TOKEN_RE is shared with the outer scan and keeps ITS position in lastIndex —
    // borrow it, then put the position back, or the outer loop restarts from the top of
    // the text at every semicolon and never terminates.
    const resume = SQL_TOKEN_RE.lastIndex;
    SQL_TOKEN_RE.lastIndex = 0;
    try {
      let m                        ;
      while ((m = SQL_TOKEN_RE.exec(piece)) !== null) {
        if (!m[1]) return true; // anything that is not a comment is code
      }
      return false;
    } finally {
      SQL_TOKEN_RE.lastIndex = resume;
    }
  };
  const cut = (at        )       => {
    const piece = t.slice(start, at).replace(/^\s+|\s+$/g, "");
    if (piece && hasCode(piece)) out.push(piece);
    start = at + 1;
  };
  let m                        ;
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

/* The table a statement names, per first word; a quoted or schema-qualified name is one name.
   A statement these miss (a subquery first, a CTE, DDL) keeps its word alone. */
const DB_TAB_IDENT = /([`"[]?[\w$]+[`"\]]?(?:\.[`"[]?[\w$]+[`"\]]?)?)/.source;
const DB_TAB_TARGET                                     = {
  SELECT: new RegExp("\\bFROM\\s+" + DB_TAB_IDENT, "i"),
  DELETE: new RegExp("\\bFROM\\s+" + DB_TAB_IDENT, "i"),
  UPDATE: new RegExp("^UPDATE\\s+(?:(?:LOW_PRIORITY|IGNORE|ONLY)\\s+)*" + DB_TAB_IDENT, "i"),
  INSERT: new RegExp("\\bINTO\\s+" + DB_TAB_IDENT, "i"),
  REPLACE: new RegExp("\\bINTO\\s+" + DB_TAB_IDENT, "i"),
};

/** SPEC §data.console: a result tab's name — the table the statement reads or writes, a write
 *  keeping its verb before it ("t_user · 10", "UPDATE orders · 3"), else the statement's
 *  first word ("SELECT · 42"), plus its row count. Two SELECTs side by side read "SELECT · 10"
 *  twice when only the word named them. Leading comments are skipped (they are not the word the
 *  user recognises); EXPLAIN answers name themselves because the prefix ran too. A missing
 *  count renders the name alone; a statement with no word at all still gets a name. Pure. */
function dbResultTabLabel(stmt        , rowCount                           )         {
  const s = String(stmt).replace(/^(\s|--[^\n]*\n|#[^\n]*\n|\/\*[\s\S]*?\*\/)+/, "");
  let w = (/^([A-Za-z_][A-Za-z0-9_]*)/.exec(s) || [])[1] || "?";
  w = w.toUpperCase();
  const at = DB_TAB_TARGET[w]?.exec(s);
  let name = at ? at[1].replace(/[`"[\]]/g, "") : "";
  if (name.length > 32) name = name.slice(0, 31) + "…";
  const head = !name ? w : w === "SELECT" ? name : w + " " + name;
  return typeof rowCount === "number" ? head + " · " + rowCount : head;
}

/** SPEC §data.console: the text a run covers. `all` (Run all, Ctrl+Shift+Enter, Alt+X) is the
 *  whole console; otherwise a selection is exactly what runs, and with none the caret's block -
 *  the scratchpad habit of a desktop SQL client: select what you mean, or stand in it. Pure. */
function dbRunText(text        , selStart               , selEnd               , all         )         {
  const t = String(text);
  if (all) return t;
  if (selStart !== null && selEnd !== null && selEnd > selStart) return t.slice(selStart, selEnd);
  return dbSubqueryAt(t, selStart);
}

/** SPEC §data.console: a script's blank-line blocks, trimmed, empties dropped - the unit Run all
 *  walks before each block is split on ; (statements a blank line apart with no ; between them
 *  are still two statements). Pure. */
function dbConsoleBlocks(text        )           {
  return String(text).split(/\n[ \t]*\n/).map((b        )         => { return b.trim(); }).filter(Boolean);
}

/* --- one statement, addressed (SPEC §data.agent Tabs) -------------------------------------------- */

/* The console keeps ONE text per tab (sqlText), so "the third statement" has to be a position in
   that text. The unit is the blank-line block: the block Run all walks (dbConsoleBlocks) and the
   one the agent writes each statement in, a blank line apart (dbAgentStatement). The spans below
   are character ranges in the original text rather than a split and a re-join, because an edit
   must leave every other statement byte for byte as the reader typed it - indentation, the blank
   lines they chose, the trailing comment and all. */

/** One statement of a console: its 0-based position, its text, and the name its first line
 *  comments it with, when it has one. */
;                                    
                
              
                 
 

/** Where each block's trimmed content sits in the text, in order. Pure. */
function dbConsoleSpans(text        )                                   {
  const t = String(text);
  const spans                                   = [];
  const push = (from        , to        )       => {
    const piece = t.slice(from, to);
    if (!piece.trim()) return;
    const lead = piece.length - piece.trimStart().length;
    const tail = piece.length - piece.trimEnd().length;
    spans.push({ start: from + lead, end: to - tail });
  };
  const sep = /\n[ \t]*\n/g;
  let last = 0;
  let m                        ;
  while ((m = sep.exec(t)) !== null) {
    push(last, m.index);
    last = m.index + m[0].length;
  }
  push(last, t.length);
  return spans;
}

/** The name a statement's first line comments it with ("-- Clean up stale rows", "# orders"):
 *  how the agent labels a block it writes and how a reader tags their own. None when the first
 *  line is code, or the comment says nothing. Pure. */
function dbStatementTitle(sql        )                     {
  const first = String(sql).split("\n")[0].trim();
  const m = /^(?:--|#)\s*(.+)$/.exec(first);
  const title = m ? m[1].trim() : "";
  if (!title) return undefined;
  return title.length > 60 ? title.slice(0, 59) + "…" : title;
}

/** The tab's statements, in the order the console shows them. A tab holding one command line,
 *  or no blank line at all, holds exactly one. Pure. */
function dbConsoleStatements(text        )                       {
  const t = String(text);
  return dbConsoleSpans(t).map((s                                , index        )                     => {
    const sql = t.slice(s.start, s.end);
    const title = dbStatementTitle(sql);
    return title ? { index, sql, title } : { index, sql };
  });
}

/** The text with statement `index` replaced whole. Null when there is no such statement - the
 *  caller says so rather than writing the text somewhere else. Pure. */
function dbConsoleReplace(text        , index        , sql        )                {
  const t = String(text);
  const span = dbConsoleSpans(t)[index];
  if (!span) return null;
  return t.slice(0, span.start) + String(sql).trim() + t.slice(span.end);
}

/** The text with statement `index` gone, and the blank lines that separated it with it. The
 *  last statement leaves an empty tab, never a closed one: the tab is the reader's, and an
 *  empty console is a place to write the next statement. Null when there is no such statement. */
function dbConsoleDelete(text        , index        )                {
  const t = String(text);
  const spans = dbConsoleSpans(t);
  const at = spans[index];
  if (!at) return null;
  const next = spans[index + 1];
  if (next) return t.slice(0, at.start) + t.slice(next.start);
  const prev = spans[index - 1];
  if (prev) return t.slice(0, prev.end);
  return "";
}

/** SPEC §data.redis-console: a redis script's commands, one a line. The server reads a command's
 *  words the way a shell does (shlex), where a newline is only a space - "SET a 1\nGET a" would
 *  reach redis as one SET with two stray words - so the console cuts the lines itself. A newline
 *  inside quotes stays in its argument ("..." with \ escapes, '...' literal, shlex's rules);
 *  blank lines yield nothing. Pure. */
function dbSplitCommands(text        )           {
  const t = String(text);
  const out           = [];
  let q = "", start = 0;
  const cut = (at        )       => {
    const piece = t.slice(start, at).trim();
    if (piece) out.push(piece);
    start = at + 1;
  };
  for (let i = 0; i < t.length; i++) {
    const ch = t.charAt(i);
    if (q) {
      if (q === "\"" && ch === "\\") i++;
      else if (ch === q) q = "";
    } else if (ch === "\"" || ch === "'") q = ch;
    else if (ch === "\n") cut(i);
  }
  cut(t.length);
  return out;
}

/** SPEC §data.redis-console: a command's result tab - its word and its first argument (the key,
 *  mostly), "GET user:1", cut to fit the strip. A quoted argument is one argument, quotes and
 *  all: ECHO "hello swiss", not ECHO "hello. Pure. */
function dbCommandTabLabel(cmd        )         {
  const words = String(cmd).trim().match(/"(?:\\.|[^"\\])*"?|'[^']*'?|\S+/g) || [""];
  const label = words[0].toUpperCase() + (words.length > 1 ? " " + words[1] : "");
  return label.length > 28 ? label.slice(0, 27) + "…" : label;
}

;                                                                                                            

/** One request of a run: the reply, or the server's words for its refusal - the batch shows them
 *  in a tab of their own (and the toast says them too, as apiJson would have). */
async function dbRunOne   (path        , body         )                                                             {
  try {
    const r = await api(path, { method: "POST", body: JSON.stringify(body) });
    const j          = await r.json().catch(()          => { return {}; });
    // The one trust every caller grants the admin API: an ok body is the T the caller declared.
    if (r.ok) return { ok: true, j: j      };
    const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : "";
    return { ok: false, error: msg || tr("util.httpN", { n: r.status }) };
  } catch (e) {
    return { ok: false, error: tr("util.requestFailedGatewayRunning") };
  }
}

/** SPEC §data.console: the tab a failed statement or command leaves - the server's words and
 *  what was sent, named "UPDATE · failed" - so a script that stopped halfway says where. */
function dbFailedReply(what        , sent        , error        , label        , notRun        )               {
  const row                          = { error };
  row[what] = sent;
  return {
    columns: ["error", what], rows: [row], rowCount: 0, explained: false, tabLabel: tr("dataSql.failedTab", { label }), stmt: sent,
    note: notRun > 0 ? trn(notRun, "dataSql.nNotRun.one", "dataSql.nNotRun.other") : undefined,
  };
}

// One run at a time owns the results pane: a slow run's answer that lands after a newer Run
// started must be dropped, or it would overwrite the newer run's tabs with the old ones
// (SPEC §data). The token is issued only when a request is actually about to
// fire — an early guard refusal (empty console) must not invalidate a run in flight.
const dbRunReq = dbReqGuard();

/* The console's row limit (SPEC §data.console): the most rows a MySQL or PostgreSQL statement
   without a LIMIT of its own brings back - 500 unless the operator picked another in the status
   line, remembered per browser. The server writes it into the statement (with_row_limit), so
   the database stops there instead of building the whole set; a LIMIT the statement writes
   itself always wins, and 10 000 is the server's ceiling (MAX_ROW_LIMIT). */
const DB_ROW_LIMITS = [100, 500, 1000, 2000, 5000, 10000];
const DB_ROW_LIMIT_DEFAULT = 500;
const DB_ROW_LIMIT_KEY = "swiss.dbRowLimit";
// The pick of this page, for a browser whose storage refuses it.
let dbRowLimitHeld                = null;

function dbRowLimit()         {
  if (dbRowLimitHeld !== null) return dbRowLimitHeld;
  try {
    const n = Number(localStorage.getItem(DB_ROW_LIMIT_KEY));
    if (DB_ROW_LIMITS.includes(n)) return n;
  } catch (e) { /* blocked storage reads as the default */ }
  return DB_ROW_LIMIT_DEFAULT;
}

function dbSetRowLimit(n        )       {
  if (!DB_ROW_LIMITS.includes(n)) return;
  dbRowLimitHeld = n;
  try { localStorage.setItem(DB_ROW_LIMIT_KEY, String(n)); }
  catch (e) { /* full or blocked - the pick holds for this page */ }
}

/** `explain`: falsy runs the statement(s); "plan" | "analyze" prefixes EXPLAIN. `all`: the whole
 *  console instead of the selection or the caret's block (dbRunText). */
async function dbRunSql(explain                 , all          )                {
  const c = dbConn();
  // The console's text, its busy flag and its replies all live on the sql TAB (SPEC §data.tabs):
  // `d` below is that tab, so a run that outlives a tab switch writes where it came from and
  // the request guard decides whether the pane repaints.
  const d = dbSqlTab();
  if (!d) return; // Run reached us with no console open — nothing to run
  if (!c.conn) { toast(tr("dataSql.databaseConnection"), true); return; }
  const ta = $                     ("dbSql");
  // A MongoDB command is one document (SPEC §data.mongo): Run all has nothing to add there.
  const script = dbRunText(d.sqlText || "", ta ? ta.selectionStart : null, ta ? ta.selectionEnd : null, !!all && !dbIsMongo()).trim();
  if (!script) { toast(tr("dataSql.typeCommandFirst"), true); return; }
  // A run answers into the results: folded down under the console, they come back.
  dockShow("db-console", "bottom");
  // The MongoDB console: the text is ONE command document in shell syntax (SPEC §data.mongo).
  if (dbIsMongo()) {
    dbClearSel();
    await mongoRunCommand(d, script, ()       => { renderDbToolbar(); renderDbGrid(); });
    void dbHistoryRefresh(); // the server filed the command, answered or refused
    return;
  }
  // SPEC §data.console: every statement (on redis, every line) is ONE REQUEST - the server's
  // single-statement contract is untouched, and every reply gets its own result tab. Empty
  // stretches (a trailing ;, a comment-only piece, a blank line) never run; the first failure
  // stops the batch, its own tab saying why, with the tabs that already answered kept. The server
  // files every statement it was sent in this database's history (SPEC §data.history).
  const redis = dbIsRedis();
  const pieces = redis
    ? dbSplitCommands(script)
    : dbConsoleBlocks(script).reduce((acc          , b        )           => { return acc.concat(dbSplitStatements(b)); }, []);
  if (!pieces.length) { toast(tr("dataSql.typeCommandFirst"), true); return; }
  const token = dbRunReq.issue();
  d.sqlBusy = true;
  d.sqlResult = null; // "Running…" paints in place of the previous grid
  d.sqlResults = null;
  renderDbToolbar();
  renderDbGrid();
  const base = "/api/db/" + encodeURIComponent(c.conn);
  const results                 = [];
  let ok = true;
  for (let si = 0; si < pieces.length; si++) {
    // The plan view runs EXPLAIN (or EXPLAIN ANALYZE) on each statement; dbWithExplain is
    // idempotent, so a query that already explains itself is sent as-is.
    const toSend = !redis && explain ? dbWithExplain(pieces[si], explain) : pieces[si];
    // A statement without a LIMIT gets the console's row limit (dbRowLimit).
    const r = redis
      ? await dbRunOne                (base + "/command", { command: toSend })
      : await dbRunOne              (base + "/query", { sql: toSend, limit: dbRowLimit() });
    if (!dbRunReq.accepts(token)) return; // superseded mid-batch: stop quietly, the newer run owns the pane
    if (!r.ok) {
      ok = false;
      toast(moduleDbMessage(r.error), true);
      const label = redis ? dbCommandTabLabel(toSend) : dbResultTabLabel(toSend, null);
      results.push(dbFailedReply(redis ? "command" : "statement", toSend, r.error, label, pieces.length - si - 1));
      break;
    }
    if (redis) {
      const cj = r.j                  ;
      results.push({
        columns: ["reply"], rows: [{ reply: cj.reply }], rowCount: 1, explained: false, elapsedMs: cj.elapsedMs,
        note: typeof cj.reply === "object" && cj.reply && cj.reply.length != null
          ? trn(cj.reply.length, "dataSql.nReplyItems.one", "dataSql.nReplyItems.other") : undefined,
        tabLabel: pieces.length > 1 ? dbCommandTabLabel(toSend) : undefined, stmt: toSend,
      });
    } else {
      const j = r.j                ;
      j.explained = !!explain;
      j.tabLabel = dbResultTabLabel(toSend, j.rowCount);
      j.stmt = toSend;
      results.push(j);
    }
  }
  d.sqlBusy = false;
  dbClearSel(); // a new result grid starts unselected
  d.sqlResults = results;
  // A failure is the tab to read first; otherwise the first answer.
  d.resultTab = ok ? 0 : results.length - 1;
  d.sqlResult = results.length ? results[d.resultTab] : null;
  // The ⋯'s recent statements read again: the server filed this batch as it ran.
  void dbHistoryRefresh();
  renderDbToolbar();
  renderDbGrid();
  // A command may have written (SET, DEL, RENAME…): the sidebar re-reads the keyspace quietly,
  // so a key the console just made is in the tree without a refresh.
  if (redis && results.length > (ok ? 0 : 1)) dbRefreshKeyspace();
}

/** SPEC §data.agent: a MongoDB command's result tab - its command word and the collection it
 *  names, "find posts", after the database when that is not the one the sidebar shows. */
function dbMongoTabLabel(text        , db                    )         {
  let label = text;
  try {
    const cmd          = JSON.parse(text);
    const first = cmd && typeof cmd === "object" && !Array.isArray(cmd) ? Object.entries(cmd)[0] : undefined;
    if (first) label = first[0] + (typeof first[1] === "string" ? " " + first[1] : "");
  } catch (e) { /* not JSON: the text itself, cut */ }
  if (db && db !== mongoCurrentDb()) label = db + " · " + label;
  return label.length > 28 ? label.slice(0, 27) + "…" : label;
}

;                                                                                         

/** A MongoDB or Redis answer as the console shows one: the reply whole in one cell (MongoDB's
 *  painted as its code block), on the meta line the documents a cursor brought or the items a
 *  Redis reply holds, and where the row limit cut it. */
function dbAgentCommandReply(j                     , mongo         , label        , stmt        )               {
  const r = j.reply;
  let note                    ;
  if (mongo) {
    const cursor = r && typeof r === "object" && "cursor" in r ? r.cursor : undefined;
    const batch = cursor && typeof cursor === "object" && "firstBatch" in cursor ? cursor.firstBatch : undefined;
    if (Array.isArray(batch)) note = tr("dataMongo.nDocs", { n: batch.length });
  } else if (Array.isArray(r)) note = trn(r.length, "dataSql.nReplyItems.one", "dataSql.nReplyItems.other");
  return {
    columns: ["reply"], rows: [{ reply: r }], rowCount: 1, explained: false, elapsedMs: j.elapsedMs,
    limitApplied: typeof j.limitApplied === "number" ? j.limitApplied : undefined, note, tabLabel: label, stmt,
  };
}

/** The reserved wire name Agent identifies the default slot in every locale. */
function dbAgentTabName(tab         )         {
  const name = (tab || "").trim();
  return name.toLowerCase() === "agent" ? "" : name;
}

/* SPEC §data.agent Tabs: the agent's console tab that name - or that id - names, brought to the
   front, or opened under the name when the page has none - its Agent tab when it names neither.
   The id wins over every name: two consoles can be called "SQL", and one the reader renamed no
   longer answers to the name the agent opened it under, yet a run or a stage coming back from the
   server carries the id it was addressed by. A named one the reader closed comes back with its
   text and its own id (Recently closed, data-closed.ts); the Agent tab starts empty again. */
function dbAgentTab(tab         , id         )                  {
  const name = dbAgentTabName(tab);
  const byId = id ? dbConsoleById(id) : null;
  if (byId) return dbAgentOwn(byId, name);
  if (id) { toast(tr("dataTabs.idMissing"), true); return null; }
  const at = dbTabs().findIndex((t)          => { return t.kind === "sql" && !t.analysisId && t.agent === true && (t.agentTab || "") === name; });
  // The tab comes forward and the caret stays with the reader (SPEC §data.agent), here and below.
  if (at >= 0) dbActivateTab(at, true);
  else {
    // A rename the agent never made: the card's own title is the only name that tab answers to.
    const renamed = name ? dbTabs().findIndex((t)          => { return t.kind === "sql" && !t.analysisId && t.agent === true && dbTabTitle(t) === name; }) : -1;
    if (renamed >= 0) dbActivateTab(renamed, true);
    else {
      const conn = dbConn().conn;
      const back = name && conn ? dbClosedFor(conn).find(x => x.agent && x.agentTab === name) : null;
      if (back) { const restored = dbClaimClosed(back); return restored ? dbAgentOwn(restored, name) : null; }
      dbOpenTabForce({ kind: "sql" }, true);
      const made = dbSqlTab();
      if (!made) return null;
      made.agent = true;
      if (name) made.agentTab = name;
      made.custom = name || tr("dataAgent.name");
      renderDbTabs();
    }
  }
  const d = dbSqlTab();
  return d && d.agent ? d : null;
}

/** A tab an id found, made one of the agent's. The name a frame carries is a label: it is adopted
 *  only where the tab has none, so the reader's rename and the agent's own first name both stand. */
function dbAgentOwn(t          , name        )           {
  if (!t.agent && name) t.agentTab = name;
  t.agent = true;
  if (!t.custom && name) t.custom = name;
  renderDbTabs();
  return t;
}

/** The agent opened a console tab (open_console with nothing to place in it): it comes to the
 *  front with the console showing. */
function dbAgentConsole(tab        )       {
  if (dbAgentTab(tab)) dockShow("db-console", "bottom");
}

/* --- the console by id (SPEC §data.agent Tabs) ---------------------------------------------------- */

/** One console of the connection, as the request body sends it: the id a tool addresses, the
 *  name on the card, whether it is open, and its statements. */
;                                  
             
              
                
                                                                                                
                                                                                                   
                 
                    
                              
                                                
 

/** The console tabs the page holds on the connection, open ones in strip order then the closed
 *  ones, newest first (SPEC §data.agent Tabs). The page is the only place they exist, so this
 *  snapshot rides every send (data-agent.ts) and is what list_console_tabs, read_console_tab and
 *  open_console's tabId read. An edit the agent makes after this send is not in it until the
 *  next one. */
function dbConsoleTabs()                     {
  const rows                     = [];
  const wire = (id        , tab        , open         , text        , agent         )                   => {
    const statements = dbConsoleStatements(text).map((s                    )                                  => {
      return s.title ? { sql: s.sql, title: s.title } : { sql: s.sql };
    });
    const omit = statements.length > 1000 || statements.some(s => Array.from(s.sql).length > 20_000);
    return { id, tab: Array.from(tab).slice(0, 500).join(""), open, agent,
      statements: omit ? [] : statements, ...(omit ? { statementsOmitted: true } : {}) };
  };
  // One pass, one lookup per tab: a hundred open consoles are a page the reader actually has.
  const seen = new Set        ();
  for (const t of dbTabs()) if (t.kind === "sql" && !t.analysisId) {
    const row = wire(t.id, dbTabTitle(t), true, t.sqlText, t.agent === true);
    if (t.agent) row.agentTab = t.agentTab || "";
    seen.add(t.id);
    rows.push(row);
  }
  const conn = dbConn().conn;
  for (const e of dbClosedFor(conn)) {
    if (seen.has(e.id)) continue;
    const row = wire(e.id, e.custom || tr(dbIsRedis() || dbIsMongo() ? "dataTabs.command" : "dataTabs.sql"), false, e.text, e.agent === true);
    if (e.agent) row.agentTab = e.agentTab || "";
    rows.push(row);
  }
  // Leave headroom for JSON escaping and other request fields. Omission is explicit and never
  // changes the page's text; the server refuses statement edits when it lacks their indices.
  const encoder = new TextEncoder();
  let bytes = 2;
  for (const row of rows) {
    let size = encoder.encode(JSON.stringify(row)).length + 1;
    if (bytes + size > 1_800_000 && row.statements.length) {
      row.statements = []; row.statementsOmitted = true;
      size = encoder.encode(JSON.stringify(row)).length + 1;
    }
    bytes += size;
  }
  return rows;
}

/** The console tab this id names: open (brought to the front) or closed (restored with its
 *  text under the same id, data-closed.ts). Null when the page has neither - the tab was
 *  forgotten, or another browser tab closed and cleared it. */
function dbConsoleById(id        , restore = true)                  {
  const at = dbTabs().findIndex((t       )          => t.kind === "sql" && !t.analysisId && t.id === id);
  if (at >= 0) {
    dbActivateTab(at, true);
  } else {
    if (!restore) return null;
    const closed = dbClosedById(dbConn().conn, id);
    if (!closed) return null;
    if (!dbClaimClosed(closed)) return null;
  }
  const t = dbSqlTab();
  return t && t.id === id ? t : null;
}

/** open_console addressed by id: the same gesture as by name, on the address that outlives a
 *  close and a reload. `name` is what the frame carried, so a console the agent did not open
 *  becomes one of its own and later runs naming it land there. */
function dbAgentConsoleId(id        , name         )       {
  const t = dbConsoleById(id);
  if (!t) { console.warn("console: no tab with this id on the page - ignored", { id }); return; }
  dbAgentOwn(t, dbAgentTabName(name));
  dockShow("db-console", "bottom");
}

/** Format only the Agent's incoming SQL; unrelated drafts keep their original text. */
function dbAgentFormat(text        , format = true)         {
  if (!format || dbIsRedis() || dbIsMongo()) return text;
  const dialect = dbConn().conns.find(c => c.name === dbConn().conn)?.dialect;
  try { return dbFormatSql(text, { dialect: dialect === "pg" ? "pg" : "mysql" }); }
  catch { toast(tr("dataTabs.formatFailed"), true); return text; }
}

/** Open first, then place and format. Only a creation frame may introduce an unknown ID. */
export function dbAgentOpenConsole(frame                         )          {
  if (frame.format !== undefined && typeof frame.format !== "boolean") { toast(tr("dataTabs.formatFailed"), true); return false; }
  const id = typeof frame.tabId === "string" ? frame.tabId : undefined;
  const name = typeof frame.tab === "string" ? frame.tab : undefined;
  if (id && typeof frame.consoleRevision === "number" && dbConsoleStoreOn(dbConn().conn)) {
    if (!dbConsoleById(id, false)) return false;
    dockShow("db-console", "bottom");
    return true;
  }
  let t                 ;
  if (id && frame.create === true && !dbTabs().some(t => t.kind === "sql" && t.id === id) && !dbClosedById(dbConn().conn, id)) {
    dbOpenTabForce({ kind: "sql" }, true);
    t = dbSqlTab();
    if (!t) return false;
    t.id = id;
    if (!name) t.custom = tr("dataAgent.name");
    dbAgentOwn(t, dbAgentTabName(name));
  } else t = dbAgentTab(name, id);
  if (!t) return false;
  // The record's revision travels with the frame (SPEC §data.agent Tabs): the page's formatted
  // text is written against exactly the record the server just saved, never against a stale one.
  const revision = typeof frame.consoleRevision === "number" && Number.isSafeInteger(frame.consoleRevision) ? frame.consoleRevision : null;
  if (revision != null) t.revision = revision;
  if (typeof frame.sql === "string" && frame.sql.trim()) {
    // Only the incoming block is formatted (SPEC §data.agent): formatting the whole console here
    // rewrote a manual draft's spacing when a tool merely pointed at it by id.
    const incoming = dbIsMongo() ? mongoFormatCommand(frame.sql) || frame.sql : frame.sql;
    const text = frame.format === false || dbIsRedis() ? incoming : dbAgentFormat(incoming);
    dbSetSqlText(t, t.sqlText ? t.sqlText + "\n\n" + text : text);
  }
  const ta = $                     ("dbSql");
  if (ta) ta.value = t.sqlText;
  dbSqlPaint();
  renderDbTabs();
  dockShow("db-console", "bottom");
  return true;
}

/** Edit (`replace`) or drop (`delete`) one statement of one console, found by id: an open one
 *  is rewritten in place and repainted, a closed one has its filed text rewritten. False when
 *  the id is unknown or the index no longer exists - the caller says why, never guesses which
 *  statement was meant.
 *
 *  `expect` is the statement's text as the page last reported it to the agent. A frame delivered
 *  twice, or one whose tab moved on since, therefore cannot hit the wrong statement: a replace
 *  finding its own text already there is a no-op, and anything else is refused rather than
 *  applied to whatever now sits at that index. */
function dbConsoleEdit(tabId        , op        , index        , sql         , expect         , revision                )          {
  const next = (text        )                => op === "delete" ? dbConsoleDelete(text, index) : dbConsoleReplace(text, index, sql || "");
  /** Does the statement at `index` still read as the frame expected? A repeat of a replace
   *  addressed the same statement it already wrote, which is done, not stale. */
  const placed = (text        )          => {
    const at = dbConsoleStatements(text)[index];
    if (!at) return false;
    if (!expect || at.sql === expect) return true;
    if (dbIsMongo() && at.sql === mongoFormatCommand(expect)) return true;
    if (!dbIsRedis() && !dbIsMongo()) {
      const dialect = dbConn().conns.find(c => c.name === dbConn().conn)?.dialect === "pg" ? "pg" : "mysql";
      try { if (dbFormatSql(at.sql, { dialect }) === dbFormatSql(expect, { dialect })) return true; }
      catch { /* Invalid SQL still permits an exact match or an idempotent replacement below. */ }
    }
    return op === "replace" && at.sql === (sql || "").trim();
  };
  const open = dbTabs().find((t       )                => t.kind === "sql" && !t.analysisId && t.id === tabId);
  if (open) {
    if (revision != null) open.revision = revision;
    if (!placed(open.sqlText)) return false;
    const text = next(open.sqlText);
    if (text === null) return false;
    if (text === open.sqlText) return true; // already what was asked for
    dbSetSqlText(open, text);
    if (dbSqlTab() === open) {
      const ta = $                     ("dbSql");
      if (ta) ta.value = text;
      dbSqlPaint();
    }
    renderDbTabs();
    return true;
  }
  const closed = dbClosedById(dbConn().conn, tabId);
  if (!closed) return false;
  if (!placed(closed.text)) return false;
  const text = next(closed.text);
  if (text === null) return false;
  if (!dbClosedSetText(tabId, text, dbConn().conn || undefined)) return false;
  renderDbTabs();
  return true;
}

/** The consoleEdit frame of the Data agent (edit_console_sql, delete_console_sql). Applied to
 *  the console it names; an unknown tabId or a statement that is gone is ignored with the
 *  reason on the browser console, because there is nothing to show the reader and no better
 *  guess to make. The server has already said the request was SENT, not done. */
function dbAgentConsoleEdit(frame                         )       {
  if (typeof frame.consoleRevision === "number" && dbConsoleStoreOn(dbConn().conn)) return;
  const tabId = typeof frame.tabId === "string" ? frame.tabId : "";
  const raw = typeof frame.index === "number" ? frame.index : -1;
  const index = raw >= 0 ? Math.floor(raw) : -1;
  const op = frame.op === "delete" ? "delete" : "replace";
  if (!tabId || index < 0) { console.warn("consoleEdit: the frame names no console or no statement - ignored", frame); return; }
  const text = typeof frame.sql === "string" ? frame.sql : "";
  const revision = typeof frame.consoleRevision === "number" && Number.isSafeInteger(frame.consoleRevision) ? frame.consoleRevision : null;
  if (dbConsoleEdit(tabId, op, index, op === "replace" ? dbAgentFormat(text, frame.format !== false) : text, typeof frame.expect === "string" ? frame.expect : undefined, revision)) {
    // A closed console's change is the server's record now: write the page's formatted text
    // (or its deletion) against the revision the frame named.
    const conn = dbConn().conn;
    const entry = conn ? dbClosedById(conn, tabId) : null;
    if (conn && entry && dbConsoleStoreOn(conn)) {
      void dbConsolePut(conn, {
        id: entry.id, tab: entry.custom || "", agent: entry.agent === true, agentTab: entry.agentTab || null,
        open: false, sql: entry.text, revision: revision ?? entry.revision ?? 0,
        createdAt: entry.createdAt || 0, updatedAt: entry.updatedAt || 0, closedAt: entry.at,
      }, "update").then((result) => {
        if (result.ok) dbClosedAdd(conn, dbClosedEntry(conn, result.console));
        else toast(result.error || tr("dataTabs.consoleSaveFailed"), true);
      });
    }
    return;
  }
  toast(tr("dataTabs.editFailed"), true);
  console.warn("consoleEdit: no console tab, no statement at that index, or one that moved on - ignored", { tabId, op, index });
}

/* SPEC §data.agent: the Data agent works on the page the way the reader would. What it runs
   lands in a console of its own - its Agent tab, or the tab of its own it names (open_console) -
   each its own block after the ones before, so the reader's own console text is never touched
   and standing in a block reruns it: a statement ended by its ;, a MongoDB command document in
   the console's shell syntax, a Redis command line. A run's answer is a result tab like a Run's -
   a failed one the failed tab - the runs of one send gathering as tabs with the newest in front,
   a send's first run in a tab starting that tab's over (`ran`: the tabs this send has run in).
   A staged one (a write the agent leaves to the reader) only lands in the console. The agent's
   run supersedes a Run still in flight on the results. `db`: the database a MongoDB command ran on. */
function dbAgentStatement(sql        , outcome                                                            , ran             , db         , tab         , id         , format = true, revision                )       {
  const committed = revision != null && dbConsoleStoreOn(dbConn().conn);
  const d = committed && id ? dbConsoleById(id, false) : dbAgentTab(tab, id);
  if (!d) return;
  const mongo = dbIsMongo();
  const redis = dbIsRedis();
  const text = sql.trim();
  // A MongoDB command reads as the console's shell syntax, one block (no blank line inside); a
  // Redis line is the line. A statement's trailing line comment would swallow the semicolon: it
  // goes on a line of its own then.
  const raw = mongo ? (mongoFormatCommand(text) || text).replace(/\n\s*\n/g, "\n")
    : redis ? text
    : /;\s*$/.test(text) ? text : text + (/--[^\n]*$/.test(text) ? "\n;" : ";");
  const stmt = dbAgentFormat(raw, format);
  const before = d.sqlText;
  if (!committed) dbSetSqlText(d, before ? before + "\n\n" + stmt : stmt);
  const ta = $                     ("dbSql");
  if (ta) {
    ta.value = d.sqlText;
    // The caret stands in the new block, so Ctrl+Enter runs it again - unless the reader is typing there.
    if (document.activeElement !== ta) {
      ta.selectionStart = ta.selectionEnd = Math.max(0, d.sqlText.length - stmt.length);
      ta.scrollTop = ta.scrollHeight;
    }
  }
  dbSqlPaint();
  if (!outcome) return;
  dbRunReq.issue();
  if (mongo) mongoRunSupersede();
  let reply              ;
  if (mongo || redis) {
    const label = mongo ? dbMongoTabLabel(text, db) : dbCommandTabLabel(stmt);
    reply = outcome.reply ? dbAgentCommandReply(outcome.reply                       , mongo, label, stmt) : dbFailedReply("command", stmt, outcome.error || "", label, 0);
  } else {
    // The server's answer to a statement is the console's reply shape.
    const r = outcome.reply                            ;
    reply = r
      ? { ...r, explained: false, tabLabel: dbResultTabLabel(stmt, r.affectedRows ?? r.rowCount), stmt }
      : dbFailedReply("statement", stmt, outcome.error || "", dbResultTabLabel(stmt, null), 0);
  }
  // Keyed by the tab's id, not its name: two consoles called "SQL" are two tabs, and the first
  // run in each has to start that tab's results over.
  const key = d.id;
  const fresh = !ran.has(key);
  ran.add(key);
  const results = (fresh ? [] : d.sqlResults || []).concat([reply]);
  d.sqlBusy = false;
  d.sqlResults = results;
  d.resultTab = results.length - 1;
  d.sqlResult = reply;
  dbClearSel();
  dockShow("db-console", "bottom");
  renderDbToolbar();
  renderDbGrid();
}

export { DB_ROW_LIMITS, dbAgentConsole, dbAgentConsoleEdit, dbAgentConsoleId, dbAgentStatement, dbApplyReadback, dbBarClick, dbCommandTabLabel, dbCommit, dbConsoleBlocks, dbConsoleDelete, dbConsoleEdit, dbConsoleReplace, dbConsoleStatements, dbConsoleTabs, dbFavoriteName, dbFavLoad, dbFavPush, dbFormatSql, dbFillConsole, dbPendingSql, dbQuoteIdentSafe, dbResultTabLabel, dbRowLimit, dbRunSql, dbRunText, dbSetRowLimit, dbSqlLiteral, dbSplitCommands, dbSplitStatements, dbStatsSql, dbSubqueryAt, dbTemplateSql, dbWithExplain, renderDbBar };
