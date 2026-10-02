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

import type { DbFilterTerm } from "./types/state.js";
import { $, iconNode } from "./util.js";
import { fill, h } from "./h.js";
import type { HChild } from "./h.js";
import { dbIsRedis, dbLoadKeys } from "./data-browsers.js";
import { dbIsMongo, mongoFilterNodes } from "./data-mongo.js";
import { dbLoadData } from "./data-grid.js";
import { dbDropEdits, dbIsPg, dbOkToDrop } from "./data-view.js";
// The strip reads this module's filter rows for the per-card count; the call crosses inside a
// function, never at module scope, which is the accepted shape for every data-* edge.
import { renderDbTabs } from "./data-tabs.js";
import { dbConn, dbTab } from "./db-state.js";
import { locale, tk, tr } from "./i18n.js";
import { btn } from "./ui/button.js";
import { tokenSpans } from "./ui/json-view.js";
import type { CodeToken } from "./ui/json-view.js";

/* --- SQL syntax highlighting -------------------------------------------------------------------- */
/* A tiny tokenizer, not a parser (SPEC §panel.code): the token stream is classed with the code
   family the library paints (ui/json-view.ts tokenSpans) - w keyword or type, f function, s
   string, n number, c comment, l literal, k field - in whatever scheme <html data-code> names.
   The roles copy what DataGrip shows for SQL it has not resolved against a schema: keywords
   and types in one hue, functions in another, a column where the text itself says it is one
   (a column list after a table name, the column definitions of a CREATE TABLE, the half after a
   qualifier's dot, the word after COLUMN), every other identifier in the plain text colour.
   Every token is a TEXT NODE or a span wrapping one - a query full of <script> tags stays inert
   text without a single esc() call (SPEC §panel.toolchain). */
const SQL_KEYWORDS = new Set((
  "select from where group by order having limit offset fetch insert into values update set delete " +
  "create table drop alter rename add column constraint primary key foreign references index unique " +
  "not default as on join left right inner outer full cross natural using with recursive union " +
  "all distinct case when then else end exists between like ilike in is and or asc desc begin commit " +
  "rollback transaction explain analyze show describe vacuum truncate returning conflict do " +
  "nothing check cascade restrict engine charset collate auto_increment unsigned zerofill comment " +
  "partition over window rows range cast coalesce current_date current_timestamp current_time " +
  "database schema if replace ignore view temporary temp sequence trigger procedure function returns " +
  "language declare grant revoke to lock unlock tables generated always stored virtual identity " +
  "zone without row_format storage owner extension materialized intersect except lateral filter " +
  "within least greatest nulls first last only to each for share no action deferrable initially"
).split(" "));
/* Types take the keyword hue too, as they do in DataGrip. */
const SQL_TYPES = new Set((
  "int integer bigint smallint tinyint mediumint serial bigserial smallserial decimal numeric float " +
  "double real precision bit bool boolean char character varchar varying nchar nvarchar text tinytext " +
  "mediumtext longtext blob tinyblob mediumblob longblob binary varbinary bytea date time datetime " +
  "timestamp timestamptz year interval json jsonb uuid xml enum money inet cidr macaddr point geometry citext"
).split(" "));
const SQL_LITERALS = new Set(["true", "false", "null", "unknown"]);
/* A word after one of these stands where a table name goes: its own "(" opens a column list,
   and a dot after it qualifies the table, not a column. */
const SQL_TABLE_LEADS = new Set(["from", "join", "into", "update", "table", "references", "exists", "view"]);

/* Punctuation (group 6) never takes a quote: "(`id`)" is "(", the quoted name, ")" - a run
   that swallowed the opening quote would read the rest of the line as inside out (and a ";"
   in "('a;b')" as the end of a statement, data-sql.ts dbSplitStatements). A quote no string
   closes is punctuation on its own. */
const SQL_TOKEN_RE = /(--[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)|('(?:[^'\\]|\\.|'')*'|"(?:[^"\\]|\\.|"")*"|`[^`]*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][A-Za-z0-9_$]*)|(\s+)|([^\sA-Za-z0-9_$'"`]+|['"`])/g;

/** The classed token stream ([class, text], "" for plain runs). `dquoteIdent`: a "double
 *  quoted" run is an identifier (PostgreSQL), not a string (MySQL's default). Pure. */
function dbSqlTokens(sql: string, o: { dquoteIdent?: boolean } = {}): CodeToken[] {
  const out: CodeToken[] = [];
  let depth = 0;
  let listDepth = 0;   // > 0: inside the column list a "(" after a table name opened, at this depth
  let prev = "";       // the previous significant token: a word lowercased, a punctuation run's last char
  let tablePos = false; // the previous identifier stood where a table name goes
  let qualTable = false; // ...and a dot followed it: the next identifier is the table's second half
  const ident = (text: string): string => {
    let cls = "";
    if (prev === ".") { cls = qualTable ? "" : "jv-k"; tablePos = qualTable; qualTable = false; }
    else if (SQL_TABLE_LEADS.has(prev)) tablePos = true;
    else {
      tablePos = false;
      if (listDepth && ((depth === listDepth && (prev === "(" || prev === ",")) || depth > listDepth)) cls = "jv-k";
      else if (prev === "column") cls = "jv-k";
    }
    prev = text.toLowerCase();
    return cls;
  };
  let m: RegExpExecArray | null;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(sql)) !== null) {
    const tok = m[0];
    if (m[1]) { out.push(["jv-c", tok]); continue; }
    if (m[5]) { out.push(["", tok]); continue; }
    if (m[2]) {
      if (tok[0] === "`" || (tok[0] === "\"" && o.dquoteIdent)) { out.push([ident(tok), tok]); continue; }
      out.push(["jv-s", tok]);
      tablePos = false; prev = "'";
      continue;
    }
    if (m[3]) { out.push(["jv-n", tok]); tablePos = false; prev = "0"; continue; }
    if (m[4]) {
      const lower = tok.toLowerCase();
      const call = sql[SQL_TOKEN_RE.lastIndex] === "(";
      if (prev === ".") { out.push([ident(tok), tok]); continue; }
      if (SQL_LITERALS.has(lower)) { out.push(["jv-l", tok]); tablePos = false; prev = lower; continue; }
      if (SQL_KEYWORDS.has(lower) || SQL_TYPES.has(lower)) { out.push(["jv-w", tok]); tablePos = false; prev = lower; continue; }
      // CREATE INDEX i ON users(city): the word before "(" is the table, not a call.
      if (call && prev === "on") { out.push(["", tok]); tablePos = true; prev = lower; continue; }
      if (call && !SQL_TABLE_LEADS.has(prev)) { out.push(["jv-f", tok]); tablePos = false; prev = lower; continue; }
      out.push([ident(tok), tok]);
      continue;
    }
    // A punctuation run: parens move the depth, a dot after a table name keeps the chain.
    for (const ch of tok) {
      if (ch === "(") { depth++; if (tablePos && !listDepth) listDepth = depth; }
      else if (ch === ")") { if (depth === listDepth) listDepth = 0; depth = Math.max(0, depth - 1); }
    }
    if (tok === "." && tablePos) qualTable = true;
    else if (tok !== ".") tablePos = false;
    prev = tok[tok.length - 1];
    out.push(["", tok]);
  }
  return out;
}

/* The mongo console's text (SPEC §data.mongo-panel): the shell's own syntax, classed the way the
   document cards print it - a key before ":", a constructor before "(", true / false / null. */
const SHELL_TOKEN_RE = /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')|(\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b)|([A-Za-z_$][A-Za-z0-9_$]*)|(\s+)|([^\sA-Za-z0-9_$"']+|["'])/g;

/** The mongo console's classed token stream. Pure. */
function dbShellTokens(text: string): CodeToken[] {
  const out: CodeToken[] = [];
  const keyNext = (at: number): boolean => { return /^\s*:/.test(text.slice(at, at + 64)); };
  let m: RegExpExecArray | null;
  SHELL_TOKEN_RE.lastIndex = 0;
  while ((m = SHELL_TOKEN_RE.exec(text)) !== null) {
    const tok = m[0];
    const at = SHELL_TOKEN_RE.lastIndex;
    if (m[1]) out.push(["jv-c", tok]);
    else if (m[2]) out.push([keyNext(at) ? "jv-k" : "jv-s", tok]);
    else if (m[3]) out.push(["jv-n", tok]);
    else if (m[4]) {
      if (tok === "true" || tok === "false" || tok === "null") out.push(["jv-l", tok]);
      else if (tok === "new") out.push(["jv-w", tok]);
      else if (keyNext(at)) out.push(["jv-k", tok]);
      else if (text[at] === "(") out.push(["jv-f", tok]);
      else out.push(["", tok]);
    }
    else out.push(["", tok]);
  }
  return out;
}

/** The token stream as highlight nodes - the library's spans, in the active scheme, for the
 *  connection on screen (a PostgreSQL one quotes identifiers with "). `lined` numbers the
 *  lines, as the DDL pane and the statement previews do. */
function dbHighlightNodes(sql: string, o: { lined?: boolean } = {}): Node[] {
  return tokenSpans(dbSqlTokens(sql, { dquoteIdent: dbIsPg() }), { lined: o.lined }).nodes;
}

/** Keep the highlighted layer under the textarea: same text, same scroll. A mongo console is
 *  the shell's syntax, a PostgreSQL one quotes identifiers with ". */
function dbSqlPaint(): void {
  const ta = $<HTMLTextAreaElement>("dbSql"), hl = $("dbSqlHl");
  if (!ta || !hl) return;
  const tokens = dbIsMongo() ? dbShellTokens(ta.value) : dbSqlTokens(ta.value, { dquoteIdent: dbIsPg() });
  fill(hl, tokenSpans(tokens).nodes, "\n");
  hl.scrollTop = ta.scrollTop;
  hl.scrollLeft = ta.scrollLeft;
}
/* --- field-level filters ------------------------------------------------------------------------ */
/* The ops mirror the server's whitelist (BROWSE_FILTER_OPS in dbbrowser.ts): every value is a
   bound parameter server-side, so a filter input is just text — never SQL. */
const DB_FILTER_OPS = [
  { op: "eq", label: "=" }, { op: "ne", label: "≠" },
  { op: "gt", label: ">" }, { op: "gte", label: "≥" },
  { op: "lt", label: "<" }, { op: "lte", label: "≤" },
  { op: "in", label: tk("dataFilters.list") }, { op: "notIn", label: tk("dataFilters.list2") },
  { op: "between", label: tk("dataFilters.between") },
  { op: "like", label: tk("dataFilters.contains") }, { op: "notLike", label: tk("dataFilters.excludes") },
  { op: "isNull", label: tk("dataFilters.null") }, { op: "isNotNull", label: tk("dataFilters.null2") },
];

function dbValueless(op: string): boolean { return op === "isNull" || op === "isNotNull"; }

/** Apply the current filter set: back to page one (the filtered set is a different set) and
 *  reload. Buffered edits never survive a reload — the baseline rows change under them.
 *  Returns whether it ran: a REFUSED discard gate leaves everything untouched, and the row
 *  editors below restore their select from that answer (SPEC §data). */
function dbApplyFilters(): boolean {
  const d = dbTab();
  if (!dbOkToDrop()) { renderDbFilters(); return false; }
  if (d.kind !== "table") return true;
  d.offset = 0;
  dbDropEdits();
  void dbLoadData(true);
  return true;
}

/* The redis pattern box's debounce (SPEC §panel.toolchain): one module-level timer, restarted per
   keystroke by #pane's delegated input listener. */
let dbKeyPatternTimer: ReturnType<typeof setTimeout> | null = null;

function renderDbFilters(): void {
  const box = $("dbFilters");
  if (!box) return;
  fill(box, dbFiltersNodes());
  // The card carries this tab's filter COUNT (SPEC §data.tabs), so the strip is repainted with the
  // row it counts. Pairing it here is what keeps the two from drifting: every path that adds,
  // edits or drops a term already ends in this render.
  renderDbTabs();
}

/** The filter rows as nodes. Every control carries a data-fi/data-fk (or data-frm/data-fadd)
 *  address instead of a per-render handler — #pane's delegated listeners answer them, and the
 *  filter object behind a row is resolved from live state at event time. */
function dbFiltersNodes(): HChild[] {
  const c = dbConn();
  const d = dbTab();
  if (dbIsRedis()) {
    // The redis filter is a glob PATTERN fed to SCAN's MATCH — server-side, cursor-safe.
    return [h("div", { class: "db-filter" },
      h("input", {
        type: "search", placeholder: tr("dataFilters.keyPatternEG"), value: c.grep || "",
        style: "width:220px", title: tr("dataFilters.scanMatchPatternApplies"), data: { fkey: "" },
      }),
      // SCAN TYPE narrows the same cursor walk to one Redis type; the backend already speaks
      // it, and "" keeps the request byte-identical to the unfiltered one.
      h("select", { class: "db-fsel", title: tr("dataFilters.keyType"), data: { frtype: "" } },
        [""].concat(["string", "hash", "list", "set", "zset", "stream"]).map((t: string): HChild => {
          return h("option", { value: t, selected: (c.redisType || "") === t }, t || tr("dataFilters.allTypes"));
        })),
      c.redis && c.redis.total != null
        ? h("span", { class: "db-filter-hint" },
            tr("dataFilters.shownShownTotalKeyspace", { shown: (c.redis?.keys ? c.redis?.keys.length.toLocaleString(locale()) : "0"), total: Number(c.redis?.total).toLocaleString(locale()) }))
        : null)];
  }
  // A collection's query bar: filter, projection, sort, skip (SPEC §data.mongo-panel).
  if (d.kind === "coll") return mongoFilterNodes(d);
  if (d.kind !== "table" || !d.data || d.pane !== "data") return []; // filters belong to the row grid only
  const cols = d.data?.columns.map((c: { name: string }): string => { return c.name; });
  const rows: HChild[] = d.filters.map((f: DbFilterTerm, i: number): HChild => {
    // The list operators say what they want right in the box (SPEC §data.browse).
    const ph = f.op === "in" || f.op === "notIn" ? tr("dataFilters.phList")
      : f.op === "between" ? tr("dataFilters.phBetween") : tr("dataFilters.phValue");
    return h("div", { class: "db-filter" },
      h("select", { class: "db-fsel", title: tr("dataFilters.column"), data: { fi: String(i), fk: "col" } },
        cols.map((c: string): HChild => { return h("option", { value: c, selected: c === f.column }, c); })),
      h("select", { class: "db-fsel", title: tr("dataFilters.operator"), data: { fi: String(i), fk: "op" } },
        DB_FILTER_OPS.map((op: { op: string; label: string }): HChild => {
          return h("option", { value: op.op, selected: op.op === f.op }, tr(op.label));
        })),
      !dbValueless(f.op)
        ? h("input", {
            type: "text", title: tr("dataFilters.enterApplies"), value: f.value as string || "",
            placeholder: ph,
            data: { fi: String(i), fk: "val" },
          })
        : null,
      h("button", { class: "db-act", title: tr("dataFilters.removeFilter"), data: { frm: String(i) } }, iconNode("x")));
  });
  rows.push(btn(tr("dataFilters.filter"), { title: tr("dataFilters.filterRowsColumnValue"), data: { fadd: "" } }));
  if (d.filters.length) {
    rows.push(h("span", { class: "db-filter-hint" }, tr("dataFilters.enterAppliesTermsStack")));
  }
  return rows;
}

/* --- #pane's delegated listeners for the filter rows (SPEC §panel.toolchain) --------------------------------
   Behavior note (SPEC §panel.toolchain): the refused-discard contract is unchanged but its timing
   moved — the column and operator handlers used to close over the filter object at render
   time; the delegated handlers resolve d.filters[i] from LIVE state at event time, so a
   re-render between paint and click can never restore into a stale object. */

function dbFiltersClick(t: Element): boolean {
  const d = dbTab();
  const rm = t.closest<HTMLElement>("[data-frm]");
  if (rm && d.kind === "table") {
    const i = Number(rm.dataset.frm);
    const f = d.filters[i];
    if (!f) return true; // a stale address (the row set changed under the click) — nothing to do
    // SPEC §data: remove goes through the same gate as every other row change —
    // a REFUSED discard must leave the row on screen, not silently swallow it.
    d.filters.splice(i, 1);
    if (!dbApplyFilters()) { d.filters.splice(i, 0, f); renderDbFilters(); }
    return true;
  }
  if (t.closest("[data-fadd]")) {
    const d2 = dbTab();
    if (d2.kind !== "table") return true;
    const cols = (d2.data && d2.data.columns || []).map((c: { name: string }): string => { return c.name; });
    d2.filters.push({ column: cols[0] || "", op: "eq", value: "" });
    renderDbFilters();
    const box = $("dbFilters");
    if (box) {
      const inputs = box.querySelectorAll("input");
      if (inputs.length) inputs[inputs.length - 1].focus();
    }
    return true;
  }
  return false;
}

function dbFiltersChange(t: Element): boolean {
  const sel = t.closest<HTMLElement>("[data-fk],[data-frtype]");
  if (!sel) return false;
  const c = dbConn();
  const d = dbTab();
  if (typeof sel.dataset.frtype !== "undefined") {
    c.redisType = (sel as HTMLSelectElement).value;
    void dbLoadKeys(true);
    return true;
  }
  if (d.kind !== "table") return false;
  const i = Number(sel.dataset.fi);
  const f = d.filters[i];
  if (!f) return false;
  const value = (sel as HTMLSelectElement).value;
  if (sel.dataset.fk === "col") {
    // SPEC §data: a refused discard must leave the row as it was — assign, ask,
    // and restore (plus one re-render, because the refused ask already repainted the mutated
    // row) instead of keeping a column change the user just said no to.
    const from = f.column;
    f.column = value;
    if (!dbApplyFilters()) { f.column = from; renderDbFilters(); }
    return true;
  }
  if (sel.dataset.fk === "op") {
    const from = f.op;
    f.op = value;
    if (dbValueless(f.op)) {
      // nothing to type — apply at once, through the same restore-on-refusal gate
      if (!dbApplyFilters()) { f.op = from; renderDbFilters(); }
    } else renderDbFilters(); // re-render so the value input appears
    return true;
  }
  if (sel.dataset.fk === "val") f.value = value;
  return true;
}

function dbFiltersInput(t: Element): boolean {
  const inp = t.closest<HTMLInputElement>("[data-fkey]");
  if (!inp) return false;
  const v = inp.value;
  if (dbKeyPatternTimer !== null) clearTimeout(dbKeyPatternTimer);
  dbKeyPatternTimer = setTimeout((): void => {
    dbKeyPatternTimer = null;
    const d = dbConn();
    d.grep = v;
    void dbLoadKeys(true);
  }, 400);
  return true;
}

function dbFiltersKeydown(ev: KeyboardEvent, t: Element): boolean {
  if (ev.key !== "Enter") return false;
  const pat = t.closest<HTMLInputElement>("[data-fkey]");
  if (pat) {
    // Enter applies the pattern at once — the debounce's pending write is superseded.
    ev.preventDefault();
    ev.stopPropagation();
    if (dbKeyPatternTimer !== null) clearTimeout(dbKeyPatternTimer);
    dbKeyPatternTimer = null;
    dbConn().grep = pat.value;
    void dbLoadKeys(true);
    return true;
  }
  const vi = t.closest<HTMLInputElement>('[data-fk="val"]');
  if (vi) {
    ev.preventDefault();
    ev.stopPropagation();
    const t2 = dbTab();
    const f = t2.kind === "table" ? t2.filters[Number(vi.dataset.fi)] : null;
    if (f) f.value = vi.value;
    dbApplyFilters();
    return true;
  }
  return false;
}

export {
  DB_FILTER_OPS, SQL_KEYWORDS, SQL_TOKEN_RE, dbApplyFilters, dbFiltersChange, dbFiltersClick,
  dbFiltersInput, dbFiltersKeydown, dbHighlightNodes, dbShellTokens, dbSqlPaint, dbSqlTokens, dbValueless,
  renderDbFilters,
};
