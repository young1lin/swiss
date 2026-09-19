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

                                                     
import { $ } from "./util.js";
import { fill, h } from "./h.js";
                                     
import { dbIsRedis, dbLoadKeys } from "./data-browsers.js";
import { dbLoadData } from "./data-grid.js";
import { dbDropEdits, dbOkToDrop } from "./data-view.js";
import { dbView } from "./db-state.js";

/* --- SQL syntax highlighting -------------------------------------------------------------------- */
/* A tiny tokenizer, not a parser: keywords, strings, numbers, comments, functions, identifiers.
   The token stream is classified pure (dbSqlTokens) and painted two ways: the node twin
   dbHighlightNodes builds the highlight layer the console mirrors, where every token is a TEXT
   NODE or a span wrapping one — a query full of <script> tags stays inert text without a single
   esc() call (docs/37 R5). */
const SQL_KEYWORDS = new Set((
  "select from where group by order having limit offset fetch insert into values update set delete " +
  "create table drop alter rename add column constraint primary key foreign references index unique " +
  "not null default as on join left right inner outer full cross natural using with recursive union " +
  "all distinct case when then else end exists between like ilike in is and or asc desc begin commit " +
  "rollback transaction explain analyze show describe desc vacuum truncate returning conflict do " +
  "nothing check cascade engine charset collate auto_increment unsigned comment partition over " +
  "window rows range cast coalesce true false null current_date current_timestamp database"
).split(" "));

const SQL_TOKEN_RE = /(--[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)|('(?:[^'\\]|\\.|'')*'|"(?:[^"\\]|\\.|"")*"|`[^`]*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][A-Za-z0-9_$]*)|(\s+)|([^\sA-Za-z0-9_$]+)/g;

/** The classified token stream: one {text, cls} per token, cls null for unstyled runs. Pure. */
function dbSqlTokens(sql        )                                         {
  const out                                         = [];
  let m                        ;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(sql)) !== null) {
    const tok = m[0];
    if (m[1]) out.push({ text: tok, cls: "c" });
    else if (m[2]) out.push({ text: tok, cls: "s" });
    else if (m[3]) out.push({ text: tok, cls: "n" });
    else if (m[4]) {
      const lower = tok.toLowerCase();
      if (SQL_KEYWORDS.has(lower)) out.push({ text: tok, cls: "k" });
      else if (sql[SQL_TOKEN_RE.lastIndex] === "(") out.push({ text: tok, cls: "f" });
      else if (/^_?[A-Z][A-Za-z0-9_]*$/.test(tok)) out.push({ text: tok, cls: "i" });
      else out.push({ text: tok, cls: null });
    }
    else out.push({ text: tok, cls: null }); // whitespace and punctuation, untouched
  }
  return out;
}

/** The token stream as highlight nodes — spans for styled tokens, bare strings otherwise. */
function dbHighlightNodes(sql        )           {
  return dbSqlTokens(sql).map((t)         => { return t.cls ? h("span", { class: t.cls }, t.text) : t.text; });
}

/** Keep the highlighted layer under the textarea: same text, same scroll. */
function dbSqlPaint()       {
  const ta = $                     ("dbSql"), hl = $("dbSqlHl");
  if (!ta || !hl) return;
  fill(hl, dbHighlightNodes(ta.value), "\n");
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
  { op: "in", label: "in list" }, { op: "notIn", label: "not in list" },
  { op: "between", label: "between" },
  { op: "like", label: "contains" }, { op: "notLike", label: "excludes" },
  { op: "isNull", label: "is NULL" }, { op: "isNotNull", label: "not NULL" },
];

function dbValueless(op        )          { return op === "isNull" || op === "isNotNull"; }

/** Apply the current filter set: back to page one (the filtered set is a different set) and
 *  reload. Buffered edits never survive a reload — the baseline rows change under them.
 *  Returns whether it ran: a REFUSED discard gate leaves everything untouched, and the row
 *  editors below restore their select from that answer (docs/22 closeout audit). */
function dbApplyFilters()          {
  const d = dbView();
  if (!dbOkToDrop()) { renderDbFilters(); return false; }
  d.offset = 0;
  dbDropEdits();
  void dbLoadData(true);
  return true;
}

/* The redis pattern box's debounce (docs/37 R5): one module-level timer, restarted per
   keystroke by #pane's delegated input listener. */
let dbKeyPatternTimer                                       = null;

function renderDbFilters()       {
  const box = $("dbFilters");
  if (!box) return;
  fill(box, dbFiltersNodes());
}

/** The filter rows as nodes. Every control carries a data-fi/data-fk (or data-frm/data-fadd)
 *  address instead of a per-render handler — #pane's delegated listeners answer them, and the
 *  filter object behind a row is resolved from live state at event time. */
function dbFiltersNodes()           {
  const d = dbView();
  if (dbIsRedis()) {
    // The redis filter is a glob PATTERN fed to SCAN's MATCH — server-side, cursor-safe.
    return [h("div", { class: "db-filter" },
      h("input", {
        type: "search", placeholder: "Key pattern, e.g. session:*", value: d.grep || "",
        style: "width:220px", title: "SCAN MATCH pattern — applies on Enter", data: { fkey: "" },
      }),
      // SCAN TYPE narrows the same cursor walk to one Redis type; the backend already speaks
      // it, and "" keeps the request byte-identical to the unfiltered one.
      h("select", { title: "Key type", data: { frtype: "" } },
        [""].concat(["string", "hash", "list", "set", "zset", "stream"]).map((t        )         => {
          return h("option", { value: t, selected: (d.redisType || "") === t }, t || "All types");
        })),
      d.redis && d.redis.total != null
        ? h("span", { class: "db-filter-hint" },
            (d.redis?.keys ? d.redis?.keys.length.toLocaleString() : "0") + " shown · " +
            Number(d.redis?.total).toLocaleString() + " in keyspace")
        : null)];
  }
  if (!d.data || d.tab !== "data") return []; // filters belong to the row grid only
  const cols = d.data?.columns.map((c                  )         => { return c.name; });
  const rows           = d.filters.map((f              , i        )         => {
    return h("div", { class: "db-filter" },
      h("select", { title: "Column", data: { fi: String(i), fk: "col" } },
        cols.map((c        )         => { return h("option", { value: c, selected: c === f.column }, c); })),
      h("select", { title: "Operator", data: { fi: String(i), fk: "op" } },
        DB_FILTER_OPS.map((op                               )         => {
          return h("option", { value: op.op, selected: op.op === f.op }, op.label);
        })),
      !dbValueless(f.op)
        ? h("input", {
            type: "text", title: "Enter applies", value: f.value           || "",
            // The list operators say what they want right in the box (docs/22 W1.2).
            placeholder: f.op === "in" || f.op === "notIn" ? "1,2,3" : (f.op === "between" ? "lo,hi" : "value"),
            data: { fi: String(i), fk: "val" },
          })
        : null,
      h("button", { class: "db-act", title: "Remove this filter", data: { frm: String(i) } }, "✕"));
  });
  rows.push(h("button", {
    class: "btn db-filter-add", title: "Filter rows by a column value (server-side)", data: { fadd: "" },
  }, "+ Filter"));
  if (d.filters.length) {
    rows.push(h("span", { class: "db-filter-hint" }, "Enter applies · terms stack with AND · filtered total shown above"));
  }
  return rows;
}

/* --- #pane's delegated listeners for the filter rows (docs/37 R5) --------------------------------
   Behavior note (docs/37 §10.1): the refused-discard contract is unchanged but its timing
   moved — the column and operator handlers used to close over the filter object at render
   time; the delegated handlers resolve d.filters[i] from LIVE state at event time, so a
   re-render between paint and click can never restore into a stale object. */

function dbFiltersClick(t         )          {
  const d = dbView();
  const rm = t.closest             ("[data-frm]");
  if (rm) {
    const i = Number(rm.dataset.frm);
    const f = d.filters[i];
    if (!f) return true; // a stale address (the row set changed under the click) — nothing to do
    // docs/22 closeout B6: remove goes through the same gate as every other row change —
    // a REFUSED discard must leave the row on screen, not silently swallow it.
    d.filters.splice(i, 1);
    if (!dbApplyFilters()) { d.filters.splice(i, 0, f); renderDbFilters(); }
    return true;
  }
  if (t.closest("[data-fadd]")) {
    const d2 = dbView();
    const cols = (d2.data && d2.data.columns || []).map((c                  )         => { return c.name; });
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

function dbFiltersChange(t         )          {
  const sel = t.closest             ("[data-fk],[data-frtype]");
  if (!sel) return false;
  const d = dbView();
  if (typeof sel.dataset.frtype !== "undefined") {
    d.redisType = (sel                     ).value;
    void dbLoadKeys(true);
    return true;
  }
  const i = Number(sel.dataset.fi);
  const f = d.filters[i];
  if (!f) return false;
  const value = (sel                     ).value;
  if (sel.dataset.fk === "col") {
    // docs/22 closeout audit: a refused discard must leave the row as it was — assign, ask,
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

function dbFiltersInput(t         )          {
  const inp = t.closest                  ("[data-fkey]");
  if (!inp) return false;
  const v = inp.value;
  if (dbKeyPatternTimer !== null) clearTimeout(dbKeyPatternTimer);
  dbKeyPatternTimer = setTimeout(()       => {
    dbKeyPatternTimer = null;
    const d = dbView();
    d.grep = v;
    void dbLoadKeys(true);
  }, 400);
  return true;
}

function dbFiltersKeydown(ev               , t         )          {
  if (ev.key !== "Enter") return false;
  const pat = t.closest                  ("[data-fkey]");
  if (pat) {
    // Enter applies the pattern at once — the debounce's pending write is superseded.
    ev.preventDefault();
    ev.stopPropagation();
    if (dbKeyPatternTimer !== null) clearTimeout(dbKeyPatternTimer);
    dbKeyPatternTimer = null;
    dbView().grep = pat.value;
    void dbLoadKeys(true);
    return true;
  }
  const vi = t.closest                  ('[data-fk="val"]');
  if (vi) {
    ev.preventDefault();
    ev.stopPropagation();
    const f = dbView().filters[Number(vi.dataset.fi)];
    if (f) f.value = vi.value;
    dbApplyFilters();
    return true;
  }
  return false;
}

export {
  DB_FILTER_OPS, SQL_KEYWORDS, SQL_TOKEN_RE, dbApplyFilters, dbFiltersChange, dbFiltersClick,
  dbFiltersInput, dbFiltersKeydown, dbHighlightNodes, dbSqlPaint, dbSqlTokens, dbValueless,
  renderDbFilters,
};
