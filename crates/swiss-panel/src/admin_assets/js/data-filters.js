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

import { $, el, esc, state } from "./util.js";
import { dbIsRedis, dbLoadKeys } from "./data-browsers.js";
import { dbLoadData } from "./data-grid.js";
import { dbDropEdits, dbOkToDrop } from "./data-view.js";

/* --- SQL syntax highlighting -------------------------------------------------------------------- */
/* A tiny tokenizer, not a parser: keywords, strings, numbers, comments, functions, identifiers.
   Everything is escaped on the way out, so a query full of <script> tags stays inert text. */
var SQL_KEYWORDS = new Set((
  "select from where group by order having limit offset fetch insert into values update set delete " +
  "create table drop alter rename add column constraint primary key foreign references index unique " +
  "not null default as on join left right inner outer full cross natural using with recursive union " +
  "all distinct case when then else end exists between like ilike in is and or asc desc begin commit " +
  "rollback transaction explain analyze show describe desc vacuum truncate returning conflict do " +
  "nothing check cascade engine charset collate auto_increment unsigned comment partition over " +
  "window rows range cast coalesce true false null current_date current_timestamp database"
).split(" "));

var SQL_TOKEN_RE = /(--[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)|('(?:[^'\\]|\\.|'')*'|"(?:[^"\\]|\\.|"")*"|`[^`]*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][A-Za-z0-9_$]*)|(\s+)|([^\sA-Za-z0-9_$]+)/g;

function dbHighlightSql(sql        )         {
  var out = "";
  var m                        ;
  SQL_TOKEN_RE.lastIndex = 0;
  while ((m = SQL_TOKEN_RE.exec(sql)) !== null) {
    var tok = m[0];
    if (m[1]) out += '<span class="c">' + esc(tok) + "</span>";
    else if (m[2]) out += '<span class="s">' + esc(tok) + "</span>";
    else if (m[3]) out += '<span class="n">' + esc(tok) + "</span>";
    else if (m[4]) {
      var lower = tok.toLowerCase();
      if (SQL_KEYWORDS.has(lower)) out += '<span class="k">' + esc(tok) + "</span>";
      else if (sql[SQL_TOKEN_RE.lastIndex] === "(") out += '<span class="f">' + esc(tok) + "</span>";
      else if (/^_?[A-Z][A-Za-z0-9_]*$/.test(tok)) out += '<span class="i">' + esc(tok) + "</span>";
      else out += esc(tok);
    }
    else out += esc(tok); // whitespace and punctuation, untouched
  }
  return out;
}

/** Keep the highlighted layer under the textarea: same text, same scroll. */
function dbSqlPaint()       {
  var ta = $                     ("dbSql"), hl = $("dbSqlHl");
  if (!ta || !hl) return;
  hl.innerHTML = dbHighlightSql(ta.value) + "\n";
  hl.scrollTop = ta.scrollTop;
  hl.scrollLeft = ta.scrollLeft;
}
/* --- field-level filters ------------------------------------------------------------------------ */
/* The ops mirror the server's whitelist (BROWSE_FILTER_OPS in dbbrowser.ts): every value is a
   bound parameter server-side, so a filter input is just text — never SQL. */
var DB_FILTER_OPS = [
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
 * Returns whether it ran: a REFUSED discard gate leaves everything untouched, and the row
 * editors below restore their select from that answer (docs/22 closeout audit). */
function dbApplyFilters()          {
  var d = state.db;
  if (!dbOkToDrop()) { renderDbFilters(); return false; }
  d .offset = 0;
  dbDropEdits();
  dbLoadData(true);
  return true;
}

function renderDbFilters()       {
  var d = state.db;
  var box = $("dbFilters");
  if (!box) return;
  box.innerHTML = "";
  if (dbIsRedis()) {
    // The redis filter is a glob PATTERN fed to SCAN's MATCH — server-side, cursor-safe.
    var rf = el("div", "db-filter");
    var ri = el("input")               ;
    ri.type = "search";
    ri.placeholder = "Key pattern, e.g. session:*";
    ri.value = d .grep || "";
    ri.style.width = "220px";
    ri.title = "SCAN MATCH pattern — applies on Enter";
    var t2                                      ;
    ri.oninput = function () {
      var v = this.value;
      clearTimeout(t2 );
      t2 = setTimeout(function ()       { d .grep = v; dbLoadKeys(true); }, 400);
    };
    ri.onkeydown = function (e) {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); clearTimeout(t2 ); d .grep = this.value; dbLoadKeys(true); }
    };
    rf.appendChild(ri);
    // SCAN TYPE narrows the same cursor walk to one Redis type; the backend already speaks
    // it, and "" keeps the request byte-identical to the unfiltered one.
    var rt = el("select")                ;
    rt.title = "Key type";
    [""].concat(["string", "hash", "list", "set", "zset", "stream"]).forEach(function (t        )       {
      var o = el("option", "", t || "All types")                     ;
      o.value = t;
      o.selected = (d .redisType || "") === t;
      rt.appendChild(o);
    });
    rt.onchange = function ()       { d .redisType = this.value; dbLoadKeys(true); };
    rf.appendChild(rt);
    if (d .redis && d .redis.total != null) {
      rf.appendChild(el("span", "db-filter-hint",
        (d .redis .keys ? d .redis .keys.length.toLocaleString() : "0") + " shown · " +
        Number(d .redis .total).toLocaleString() + " in keyspace"));
    }
    box.appendChild(rf);
    return;
  }
  if (!d .data || d .tab !== "data") return; // filters belong to the row grid only
  var cols = d .data .columns.map(function (c                  )         { return c.name; });
  d .filters.forEach(function (f              , i        )       {
    var row = el("div", "db-filter");
    var cs = el("select")                ;
    cs.title = "Column";
    cols.forEach(function (c        )       {
      var o = el("option", "", c)                     ;
      o.value = c;
      o.selected = c === f.column;
      cs.appendChild(o);
    });
    // docs/22 closeout audit: a refused discard must leave the row as it was — assign, ask,
    // and restore (plus one re-render, because the refused ask already repainted the mutated
    // row) instead of keeping a column change the user just said no to.
    cs.onchange = function () {
      var from = f.column;
      f.column = this.value;
      if (!dbApplyFilters()) { f.column = from; renderDbFilters(); }
    };
    var os = el("select")                ;
    os.title = "Operator";
    DB_FILTER_OPS.forEach(function (op                               )       {
      var o = el("option", "", op.label)                     ;
      o.value = op.op;
      o.selected = op.op === f.op;
      os.appendChild(o);
    });
    os.onchange = function () {
      var from = f.op;
      f.op = this.value;
      if (dbValueless(f.op)) {
        // nothing to type — apply at once, through the same restore-on-refusal gate
        if (!dbApplyFilters()) { f.op = from; renderDbFilters(); }
      } else renderDbFilters(); // re-render so the value input appears
    };
    row.appendChild(cs);
    row.appendChild(os);
    if (!dbValueless(f.op)) {
      var vi = el("input")               ;
      vi.type = "text";
      // The list operators say what they want right in the box (docs/22 W1.2).
      vi.placeholder = f.op === "in" || f.op === "notIn" ? "1,2,3" : (f.op === "between" ? "lo,hi" : "value");
      vi.title = "Enter applies";
      vi.value = f.value           || "";
      vi.onkeydown = function (e) {
        e.stopPropagation();
        if (e.key === "Enter") { e.preventDefault(); f.value = this.value; dbApplyFilters(); }
      };
      vi.onchange = function () { f.value = this.value; };
      row.appendChild(vi);
    }
    var rm = el("button", "db-act", "✕")                     ;
    rm.title = "Remove this filter";
    rm.onclick = function () {
      // docs/22 closeout B6: remove goes through the same gate as every other row change —
      // a REFUSED discard must leave the row on screen, not silently swallow it.
      d .filters.splice(i, 1);
      if (!dbApplyFilters()) { d .filters.splice(i, 0, f); renderDbFilters(); }
    };
    row.appendChild(rm);
    box.appendChild(row);
  });
  var add = el("button", "btn db-filter-add", "+ Filter");
  add.title = "Filter rows by a column value (server-side)";
  add.onclick = function () {
    var d2 = state.db;
    d2 .filters.push({ column: cols[0] || "", op: "eq", value: "" });
    renderDbFilters();
    var inputs = box.querySelectorAll                  ("input");
    if (inputs.length) inputs[inputs.length - 1].focus();
  };
  box.appendChild(add);
  if (d .filters.length) {
    box.appendChild(el("span", "db-filter-hint", "Enter applies · terms stack with AND · filtered total shown above"));
  }
}

export { DB_FILTER_OPS, SQL_KEYWORDS, SQL_TOKEN_RE, dbApplyFilters, dbHighlightSql, dbSqlPaint, dbValueless, renderDbFilters };
