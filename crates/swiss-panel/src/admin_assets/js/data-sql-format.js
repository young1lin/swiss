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

/* The SQL formatter (SPEC §data.console): a query laid out the way a JetBrains IDE's SQL
 * formatter lays it out with its defaults (DataGrip's code style), so the console reads like
 * the client the operator already knows:
 *
 *   select u.id,
 *          u.name
 *   from users u
 *            left join orders o on u.id = o.user_id
 *   where u.age > 21
 *     and o.total > 100
 *   order by u.id desc
 *
 * - every clause starts its line at the query's column; a select list puts each element on a
 *   line of its own, aligned after the keyword; GROUP BY and ORDER BY stay on one line while
 *   they fit 120 columns; SET, VALUES and WITH put each assignment, row or CTE on its own line;
 * - a join line is indented four past the table; AND/OR start their lines right-aligned under
 *   WHERE/HAVING (the AND of a BETWEEN stays);
 * - a subquery is laid out the same way at the column after its "("; a CASE with more than
 *   one WHEN puts each branch on its own line, END under the WHENs; a comma list that runs
 *   past 120 columns wraps under its "(";
 * - spacing: none inside ( ), before , ; or around . and ::, none between a function and its
 *   "(", a space after a keyword or a table name before "(", one around a binary operator, a
 *   unary minus kept on its operand.
 *
 * It changes nothing but whitespace. Case, words, strings, comments and every operator ride
 * byte-identical, and the result is checked: lexed again it must give the same tokens, with a
 * blank line before the same ones (a blank line is where the console's Run splits blocks) - a
 * text that fails the check is handed back as it came. Statements of a kind it does not lay
 * out (DDL, SET, a procedure) keep the lines they were written on; a one-line one has its
 * spacing tidied. The lexer knows the dialect: MySQL's backslash escapes, # comments and @
 * variables; PostgreSQL's $$ strings, $1 parameters, E'' strings and operators like #>> and ?|. */

                                
                                                    
                   
                                                                                             
                  
 

                                                                                                  

               
                                       
          
                                                     
                                                         
                                             
                                           
              
              
 

/** A bracketed run: ( ), [ ], or CASE ... END. `m` memoizes its one-line measure. */
                                                                                                         
                      
                                                   

const WIDTH = 120;

/* --- the lexer ---------------------------------------------------------------------------------------- */

const WORD_RE = /[\p{L}\p{N}_$]+/uy;
const NUM_RE = /(?:0[xX][0-9a-fA-F]+|0[bB][01]+|\d+(?:\.\d*)?(?:[eE][+-]?\d+)?|\.\d+(?:[eE][+-]?\d+)?)(?![\p{L}\p{N}_$])/uy;
const WS_RE = /\s+/y;
const DOLLAR_RE = /\$(?:[\p{L}_][\p{L}\p{N}_]*)?\$/uy;
const WORD_START = /[\p{L}_]/u;
const OP_PG = "+-*/<>=~!@#%^&|?";
const OP_MY = "+-*/<>=~!%^&|";
const MY_OPS = ["<=>", "->>", "<=", ">=", "<>", "!=", "<<", ">>", "&&", "||", "->"];

function lex(text        , pg         )        {
  const out        = [];
  const n = text.length;
  let i = 0, nl = 0, sp = false, blank = false;
  const push = (k      , end        )       => {
    const t = text.slice(i, end);
    out.push({ t, k, low: k === "word" ? t.toLowerCase() : t, nl, sp, blank, pos: i, end });
    i = end; nl = 0; sp = false; blank = false;
  };
  // A quoted run from `at` (the opening quote): a doubled quote stays inside, and so does a
  // backslash's next character where the dialect escapes; an unclosed one runs to the end.
  const quoted = (at        , q        , backslash         )         => {
    for (let j = at + 1; j < n; j++) {
      const ch = text.charAt(j);
      if (backslash && ch === "\\") j++;
      else if (ch === q) {
        if (text.charAt(j + 1) === q) j++;
        else return j + 1;
      }
    }
    return n;
  };
  // A line comment stops before the line's end and its own trailing blanks, so a trimmed line
  // never trims a token.
  const lineEnd = (at        )         => {
    let e = text.indexOf("\n", at);
    if (e < 0) e = n;
    while (e > at && /[ \t\r]/.test(text.charAt(e - 1))) e--;
    return e;
  };
  const wordEnd = (at        )         => {
    WORD_RE.lastIndex = at;
    const m = WORD_RE.exec(text);
    return m ? at + m[0].length : at;
  };
  while (i < n) {
    const c = text.charAt(i), c2 = text.charAt(i + 1);
    if (/\s/.test(c)) {
      WS_RE.lastIndex = i;
      const ws = (WS_RE.exec(text) || [c])[0];
      nl += ws.split("\n").length - 1;
      sp = true;
      if (/\n[ \t]*\n/.test(ws)) blank = true;
      i += ws.length;
      continue;
    }
    if (c === "-" && c2 === "-") { push("lcmt", lineEnd(i)); continue; }
    if (c === "#" && !pg) { push("lcmt", lineEnd(i)); continue; }
    if (c === "/" && c2 === "*") {
      const e = text.indexOf("*/", i + 2);
      push("bcmt", e < 0 ? n : e + 2);
      continue;
    }
    if (c === "'") { push("str", quoted(i, "'", !pg)); continue; }
    if (c === "\"") { push(pg ? "qid" : "str", quoted(i, "\"", !pg)); continue; }
    if (c === "`") { push("qid", quoted(i, "`", false)); continue; }
    if (WORD_START.test(c)) {
      const e = wordEnd(i);
      const w = text.slice(i, e);
      // A string's prefix is part of it: E'..', N'..', X'..', B'..', MySQL's _utf8mb4'..'.
      if (text.charAt(e) === "'" && (/^[EeNnBbXxRr]$/.test(w) || /^_[A-Za-z0-9]+$/.test(w))) {
        push("str", quoted(e, "'", !pg || w === "E" || w === "e"));
        continue;
      }
      if ((w === "U" || w === "u") && c2 === "&" && (text.charAt(i + 2) === "'" || text.charAt(i + 2) === "\"")) {
        push(text.charAt(i + 2) === "'" ? "str" : "qid", quoted(i + 2, text.charAt(i + 2), false));
        continue;
      }
      push("word", e);
      continue;
    }
    const prev = out.length ? out[out.length - 1] : null;
    const qualifies = !sp && prev !== null && (prev.k === "word" || prev.k === "qid" || prev.t === ")");
    if (/\d/.test(c) || (c === "." && /\d/.test(c2) && !qualifies)) {
      NUM_RE.lastIndex = i;
      const m = NUM_RE.exec(text);
      if (m) { push("num", i + m[0].length); continue; }
      // Digits glued to letters: a name MySQL allows (1abc).
      if (/\d/.test(c)) { push("word", wordEnd(i)); continue; }
    }
    if (c === "$") {
      if (/\d/.test(c2)) {
        let j = i + 1;
        while (j < n && /\d/.test(text.charAt(j))) j++;
        push("param", j);
        continue;
      }
      if (pg) {
        DOLLAR_RE.lastIndex = i;
        const m = DOLLAR_RE.exec(text);
        if (m) {
          const e = text.indexOf(m[0], i + m[0].length);
          push("str", e < 0 ? n : e + m[0].length);
          continue;
        }
      }
      push("word", wordEnd(i));
      continue;
    }
    if (c === "@" && !pg) {
      let j = i + 1;
      if (text.charAt(j) === "@") j++;
      const q = text.charAt(j);
      push("param", q === "'" || q === "\"" || q === "`" ? quoted(j, q, q !== "`") : wordEnd(j));
      continue;
    }
    if (c === "?" && !pg) { push("param", i + 1); continue; }
    if (c === ":") {
      if (c2 === ":") { push("punct", i + 2); continue; }
      if (c2 === "=") { push("op", i + 2); continue; }
      if (WORD_START.test(c2)) { push("param", wordEnd(i + 1)); continue; }
      push("punct", i + 1);
      continue;
    }
    if ("()[],;.".includes(c)) { push("punct", i + 1); continue; }
    if (pg && OP_PG.includes(c)) {
      let j = i;
      while (j < n && OP_PG.includes(text.charAt(j)) && !(text.startsWith("--", j) || text.startsWith("/*", j))) j++;
      // PostgreSQL's rule: a run ends in + or - only when it holds one of ~ ! @ # % ^ & | ` ?
      while (j - i > 1 && /[+-]$/.test(text.slice(i, j)) && !/[~!@#%^&|`?]/.test(text.slice(i, j))) j--;
      push("op", j);
      continue;
    }
    if (!pg && OP_MY.includes(c)) {
      const m = MY_OPS.find((o        )          => { return text.startsWith(o, i); });
      push("op", i + (m ? m.length : 1));
      continue;
    }
    const cp = text.codePointAt(i) || 0;
    push("other", i + (cp > 0xffff ? 2 : 1));
  }
  return out;
}

/* --- the tree: brackets and CASE ... END nest ---------------------------------------------------------- */

const isGrp = (n                  )           => { return n !== undefined && "g" in n; };
const isCmt = (n                  )           => { return n !== undefined && !isGrp(n) && (n.k === "lcmt" || n.k === "bcmt"); };
const isComma = (n                  )           => { return n !== undefined && !isGrp(n) && n.k === "punct" && n.t === ","; };
/** The node's word, lowercased; "" for anything else. */
const kw = (n                  )         => { return n !== undefined && !isGrp(n) && n.k === "word" ? n.low : ""; };

function tree(toks       )         {
  const root         = [];
  const stack        = [];
  const top = ()         => { return stack.length ? stack[stack.length - 1].items : root; };
  for (const t of toks) {
    if (t.k === "punct" && (t.t === "(" || t.t === "[")) {
      const g      = { g: t.t === "(" ? "paren" : "brack", open: t, items: [], close: null };
      top().push(g);
      stack.push(g);
    } else if (t.k === "word" && t.low === "case") {
      const g      = { g: "case", open: t, items: [], close: null };
      top().push(g);
      stack.push(g);
    } else if (t.k === "punct" && (t.t === ")" || t.t === "]")) {
      const want = t.t === ")" ? "paren" : "brack";
      let at = stack.length - 1;
      while (at >= 0 && stack[at].g !== want) at--;
      if (at < 0) top().push(t);
      else { stack[at].close = t; stack.length = at; }
    } else if (t.k === "word" && t.low === "end" && stack.length && stack[stack.length - 1].g === "case") {
      const g = stack.pop();
      if (g) g.close = t;
    } else top().push(t);
  }
  return root;
}

/* --- the writer ---------------------------------------------------------------------------------------- */

/* A word before "(" that takes a space: keywords, not functions or types. */
const KW_PAREN = new Set(("select from where and or not in exists as on using values when then else case over filter " +
  "join lateral any some all union intersect except between like ilike is set having by distinct return returning " +
  "with within group key index unique check references primary foreign table into do conflict default against").split(" "));
/* A unary operator follows these (or an operator, an opening bracket, a comma, nothing). */
const UNARY_AFTER = new Set(("select where and or not when then else case in is by on set values return returning " +
  "having limit offset between like ilike as distinct all any some exists interval array if").split(" "));
/* A name after one of these is a table's (or an index's): its "(" opens a column list. */
const TABLE_LEADS = new Set(["into", "table", "references", "view", "exists", "index", "key"]);
const UNARY_OPS = new Set(["-", "+", "~", "!"]);

class Out {
  done           = [];
  line        ;
  last             = null; // the token before, on this line
  sig             = null;  // the last token not a comment, on any line
  unary = false;           // `last` is a unary operator: the next token sits on it
  tl = 0;                  // after a table lead: 1 the name is next, 2 the name was last
  ddl = false;             // a CREATE/ALTER: ON leads a table too (CREATE INDEX i ON t (a))
  pend = false;            // a line comment ended the line: the next token starts a new one
  cont = 0;                // the column a forced break continues at
  measuring         ;
  multi = false;
  constructor(indent        , measuring         ) { this.line = " ".repeat(indent); this.measuring = measuring; }
  col()         { return this.line.length - this.line.lastIndexOf("\n") - 1; }
  /** A new line at `at`. Never a blank one: an empty line takes the new indent instead. */
  nl(at        , blank = false)       {
    this.pend = false;
    if (this.line.trim() !== "") {
      this.done.push(this.line.replace(/[ \t]+$/, ""));
      if (blank) this.done.push("");
    }
    this.line = " ".repeat(at);
    this.last = null;
  }
  tok(t     )       {
    if (this.pend) this.nl(this.cont);
    else if ((t.k === "lcmt" || t.k === "bcmt") && t.nl > 0 && this.last) this.nl(this.cont);
    if (this.last) this.line += glue(this, t);
    this.line += t.t;
    const s = this.sig;
    this.unary = t.k === "op" && UNARY_OPS.has(t.t) && (s === null || s.k === "op" ||
      (s.k === "punct" && s.t !== ")" && s.t !== "]") || (s.k === "word" && UNARY_AFTER.has(s.low)));
    if (t.k === "word" && (TABLE_LEADS.has(t.low) || (this.ddl && t.low === "on"))) this.tl = 1;
    else if (this.tl === 1 && (t.k === "word" || t.k === "qid")) this.tl = 2;
    else if (this.tl === 2 && t.t === ".") this.tl = 1;
    else this.tl = 0;
    this.last = t;
    if (t.k !== "lcmt" && t.k !== "bcmt") this.sig = t;
    // A comment that had a line of its own keeps it; a line comment always ends its line.
    if (t.k === "lcmt" || (t.k === "bcmt" && t.nl > 0)) this.pend = true;
  }
  /** Text as written, for a statement kept the way it came. */
  raw(s        , lastTok     )       {
    this.line += s;
    this.last = lastTok;
    this.sig = lastTok;
    this.unary = false;
    this.tl = 0;
  }
  text()         { return this.done.concat(this.line).join("\n"); }
}

/** The space between the token before and `t`: "" or " ". */
function glue(w     , t     )         {
  const p = w.last;
  if (!p) return "";
  if (p.k === "other" || t.k === "other") return t.sp ? " " : "";
  if (t.k === "lcmt" || t.k === "bcmt" || p.k === "bcmt") return " ";
  // Two operators never touch: "- -1" must not become a comment, nor "=-" one operator.
  if (w.unary) return t.k === "op" ? " " : "";
  const c = t.t, q = p.t;
  if (t.k === "punct" && (c === "," || c === ";" || c === ")" || c === "]" || c === "." || c === "::" || c === ":")) return "";
  if (p.k === "punct" && (q === "(" || q === "[" || q === "." || q === "::" || q === ":")) return "";
  if (t.k === "punct" && c === "(") {
    const name = p.k === "qid" || (p.k === "word" && !KW_PAREN.has(p.low));
    return name && w.tl !== 2 ? "" : " ";
  }
  if (t.k === "punct" && c === "[") {
    return (p.k === "word" && !KW_PAREN.has(p.low)) || p.k === "qid" || p.k === "param" || q === ")" || q === "]" ? "" : " ";
  }
  return " ";
}

/* --- layout -------------------------------------------------------------------------------------------- */

function measure(fn                  )          {
  const o = new Out(0, true);
  fn(o);
  const lines = o.text().split("\n");
  return { width: Math.max(...lines.map((l        )         => { return l.length; })), multi: lines.length > 1 || o.pend || o.multi };
}

function measureSeq(items        )          {
  return measure((o     )       => { for (const n of items) node(o, n); });
}

function fits(w     , m         )          {
  return !m.multi && w.col() + 1 + m.width <= WIDTH;
}

function node(w     , n      )       {
  if (!isGrp(n)) { w.tok(n); return; }
  if (w.measuring) {
    // Measured once, at column 0: the layout inside is decided where it is really written.
    const m = n.m || (n.m = measure((o     )       => { grp(o, n); }));
    w.tok(n.open);
    w.line += "x".repeat(Math.max(0, m.width - n.open.t.length - (n.close ? n.close.t.length : 0)));
    if (n.close) w.tok(n.close);
    if (m.multi) w.multi = true;
    return;
  }
  grp(w, n);
}

function seq(w     , items        , cont        )       {
  const saved = w.cont;
  w.cont = cont;
  for (const n of items) node(w, n);
  w.cont = saved;
}

function isQuery(items        )          {
  const first = items.find((n      )          => { return !isCmt(n); });
  const k = kw(first);
  return k === "select" || k === "with" || k === "values";
}

function grp(w     , g     )       {
  if (g.g === "case") { caseNode(w, g); return; }
  w.tok(g.open);
  const col = w.col();
  if (g.g === "paren" && isQuery(g.items)) query(w, g.items, col);
  else if (g.items.some(isComma)) list(w, g.items, col, "wrap");
  else seq(w, g.items, col);
  if (g.close) w.tok(g.close);
}

/** CASE: on one line while it has one WHEN and fits; else each branch on its own line, four
 *  past CASE, and END with them. */
function caseNode(w     , g     )       {
  w.tok(g.open);
  const at = w.col() - g.open.t.length;
  const branches           = [[]];
  for (const n of g.items) {
    const k = kw(n);
    if ((k === "when" || k === "else") && branches[branches.length - 1].some((x      )          => { return !isCmt(x); })) branches.push([]);
    else if ((k === "when" || k === "else") && branches.length === 1 && kw(branches[0][0]) !== "when") branches.push([]);
    branches[branches.length - 1].push(n);
  }
  const whens = g.items.filter((n      )          => { return kw(n) === "when"; }).length;
  if (whens <= 1 && fits(w, measureSeq(g.items.concat(g.close ? [g.close] : [])))) {
    seq(w, g.items, at + 4);
    if (g.close) w.tok(g.close);
    return;
  }
  // The operand (CASE x WHEN ...) stays on CASE's line.
  let k = 0;
  if (kw(branches[0][0]) !== "when" && kw(branches[0][0]) !== "else") { seq(w, branches[0], at + 4); k = 1; }
  for (; k < branches.length; k++) {
    if (!branches[k].length) continue;
    w.nl(at + 4);
    seq(w, branches[k], at + 8);
  }
  if (g.close) { w.nl(at + 4); w.tok(g.close); }
}

;                                                              

/** Split at the top-level commas; a comment on the comma's line rides after the comma. */
function splitList(items        )         {
  const parts         = [{ items: [], sep: null, trail: [] }];
  for (let k = 0; k < items.length; k++) {
    const n = items[k];
    const cur = parts[parts.length - 1];
    if (isComma(n)) {
      cur.sep = n;
      while (isCmt(items[k + 1]) && (items[k + 1]       ).nl === 0) cur.trail.push(items[++k]       );
      parts.push({ items: [], sep: null, trail: [] });
    } else cur.items.push(n);
  }
  if (parts.length > 1 && !parts[parts.length - 1].items.length) parts.pop();
  return parts;
}

/** A comma list from the current column, its elements at `col`: "chop" one a line, "fit" on
 *  one line while it fits and else chopped, "wrap" filled up to the width. */
function list(w     , items        , col        , mode                         )       {
  const parts = splitList(items);
  const saved = w.cont;
  w.cont = col;
  if (parts.length === 1 || (mode === "fit" && fits(w, measureSeq(items)))) {
    for (const n of items) node(w, n);
    w.cont = saved;
    return;
  }
  parts.forEach((p      , k        )       => {
    if (k > 0 && mode === "wrap" && w.last && !fits(w, measureSeq(p.items.concat(p.sep ? [p.sep] : [])))) w.nl(col);
    for (const n of p.items) node(w, n);
    if (!p.sep) return;
    // A comma after a line comment opens the next line itself: ", b".
    const forced = w.pend;
    w.tok(p.sep);
    for (const c of p.trail) w.tok(c);
    if (mode !== "wrap" && k < parts.length - 1 && !forced && !w.pend) w.nl(col);
  });
  w.cont = saved;
}

/* FROM's tables: a comma puts the next under the first, a join starts a line four past them. */
const JOIN_LEADS = new Set(["left", "right", "full", "inner", "outer", "cross", "natural"]);

function joinAt(items        , k        )          {
  const w = kw(items[k]);
  if (w === "join" || w === "straight_join") return !JOIN_LEADS.has(kw(items[k - 1]));
  if (!JOIN_LEADS.has(w) || JOIN_LEADS.has(kw(items[k - 1]))) return false;
  let j = k;
  while (JOIN_LEADS.has(kw(items[j]))) j++;
  return kw(items[j]) === "join";
}

function from(w     , items        , col        )       {
  const saved = w.cont;
  w.cont = col;
  for (let k = 0; k < items.length; k++) {
    const n = items[k];
    if (k > 0 && joinAt(items, k)) { w.nl(col + 4); w.cont = col + 4; }
    if (isComma(n)) {
      w.tok(n);
      while (isCmt(items[k + 1]) && (items[k + 1]       ).nl === 0) w.tok(items[++k]       );
      if (k < items.length - 1 && !w.pend) w.nl(col);
      w.cont = col;
      continue;
    }
    node(w, n);
  }
  w.cont = saved;
}

/** WHERE/HAVING: a condition a line, AND/OR right-aligned under the keyword. */
function cond(w     , items        , base        , kwWidth        )       {
  const col = base + kwWidth + 1;
  const parts                                      = [{ op: null, items: [] }];
  let between = false;
  for (const n of items) {
    const k = kw(n);
    const cur = parts[parts.length - 1];
    if (k === "between") between = true;
    if ((k === "and" || k === "or") && cur.items.some((x      )          => { return !isCmt(x); })) {
      if (k === "and" && between) between = false;
      else { parts.push({ op: n       , items: [] }); continue; }
    }
    cur.items.push(n);
  }
  for (const p of parts) {
    if (p.op) { w.nl(base + Math.max(0, kwWidth - p.op.t.length)); w.tok(p.op); }
    seq(w, p.items, col);
  }
}

;                                                                       

/** The clause a keyword at items[k] starts, and how many words it takes. */
function clauseAt(items        , k        , cur        , stmt        , prev             )                                     {
  const w = kw(items[k]);
  if (!w) return null;
  const w1 = kw(items[k + 1]), w2 = kw(items[k + 2]), w3 = kw(items[k + 3]);
  const start = k === 0;
  switch (w) {
    case "with": return start ? { kind: "with", n: w1 === "recursive" ? 2 : 1 } : null;
    case "select": return { kind: "select", n: 1 };
    case "insert": case "replace": return (start || cur === "with") && !isGrp(items[k + 1]) ? { kind: "insert", n: 1 } : null;
    case "update": return start || cur === "with" ? { kind: "update", n: 1 } : null;
    case "delete": return start || cur === "with" ? { kind: "delete", n: 1 } : null;
    case "values": case "value": return start || cur === "insert" ? { kind: "values", n: 1 } : null;
    case "set": return cur === "update" || (cur === "insert" && stmt === "insert") ? { kind: "set", n: 1 } : null;
    // IS [NOT] DISTINCT FROM is a comparison, not a clause.
    case "from": return kw(prev ?? undefined) === "distinct" ? null : { kind: "from", n: 1 };
    case "where": case "having": case "qualify": return { kind: "where", n: 1 };
    case "returning": return { kind: "select", n: 1 };
    case "group": case "order": return w1 === "by" ? { kind: "by", n: 2 } : null;
    case "window": return stmt === "select" ? { kind: "tail", n: 1 } : null;
    case "limit": return { kind: "limit", n: 1 };
    case "offset": return cur === "limit" ? null : { kind: "tail", n: 1 };
    case "fetch": return w1 === "first" || w1 === "next" ? { kind: "tail", n: 1 } : null;
    case "for": return w1 === "update" || w1 === "share" || w1 === "no" || w1 === "key" ? { kind: "tail", n: 1 } : null;
    case "lock": return w1 === "in" ? { kind: "tail", n: 1 } : null;
    case "into": return stmt === "select" ? { kind: "tail", n: 1 } : null;
    case "union": case "intersect": case "except": return { kind: "setop", n: w1 === "all" || w1 === "distinct" ? 2 : 1 };
    case "on":
      if (w1 === "duplicate" && w2 === "key" && w3 === "update") return { kind: "set", n: 4 };
      return w1 === "conflict" ? { kind: "tail", n: 2 } : null;
  }
  return null;
}

function splitClauses(items        )           {
  const out           = [];
  let cur                = null;
  let stmt = "";
  let prev              = null;
  for (let k = 0; k < items.length;) {
    const hit = clauseAt(items, k, cur ? cur.kind : "", stmt, prev);
    if (hit) {
      const c         = { kind: hit.kind, kw: items.slice(k, k + hit.n), body: [], lead: [] };
      // Comments on their own lines just above a clause belong to it, not to the clause before.
      while (cur && isCmt(cur.body[cur.body.length - 1]) && (cur.body[cur.body.length - 1]       ).nl > 0) {
        c.lead.unshift(cur.body.pop()       );
      }
      if (!stmt && hit.kind !== "with") stmt = hit.kind;
      out.push(c);
      cur = c;
      prev = items[k + hit.n - 1];
      k += hit.n;
      continue;
    }
    if (!cur) { cur = { kind: "expr", kw: [], body: [], lead: [] }; out.push(cur); }
    cur.body.push(items[k]);
    if (!isCmt(items[k])) prev = items[k];
    k++;
  }
  return out;
}

/* SELECT's modifiers stay on its line: the list aligns after them. */
const SELECT_MODS = new Set(("distinct all distinctrow high_priority straight_join sql_small_result sql_big_result " +
  "sql_buffer_result sql_no_cache sql_cache sql_calc_found_rows").split(" "));

function clause(w     , c        , base        )       {
  for (const n of c.kw) node(w, n);
  const kwWidth = w.col() - base;
  const body = c.body;
  switch (c.kind) {
    case "select": {
      let k = 0;
      while (k < body.length && (SELECT_MODS.has(kw(body[k])) || (isCmt(body[k]) && (body[k]       ).nl === 0) ||
        (kw(body[k]) === "on" && kw(body[k - 1]) === "distinct") || (isGrp(body[k]) && kw(body[k - 1]) === "on"))) {
        node(w, body[k]);
        k++;
      }
      list(w, body.slice(k), w.col() + 1, "chop");
      return;
    }
    case "by": list(w, body, w.col() + 1, "fit"); return;
    case "set": case "values": case "with": list(w, body, w.col() + 1, "chop"); return;
    case "from": case "update": from(w, body, w.col() + 1); return;
    case "where": cond(w, body, base, kwWidth); return;
    case "setop":
      if (body.length) { w.nl(base); seq(w, body, base); }
      return;
    default: seq(w, body, base + 4);
  }
}

/** A query at `base`: its first clause goes on from where the writer stands. */
function query(w     , items        , base        )       {
  const saved = w.cont;
  w.cont = base;
  let i = 0;
  while (isCmt(items[i])) node(w, items[i++]);
  const clauses = splitClauses(items.slice(i));
  // Comments on their own lines after the last clause end the query at its column.
  const tail        = [];
  const lastBody = clauses.length ? clauses[clauses.length - 1].body : [];
  while (isCmt(lastBody[lastBody.length - 1]) && (lastBody[lastBody.length - 1]       ).nl > 0) tail.unshift(lastBody.pop()       );
  clauses.forEach((c        , k        )       => {
    if (k > 0) {
      for (const lc of c.lead) { w.nl(base); w.tok(lc); }
      w.nl(base);
    }
    clause(w, c, base);
  });
  for (const t of tail) { w.nl(base); w.tok(t); }
  w.cont = saved;
}

/* --- statements ---------------------------------------------------------------------------------------- */

const QUERY_HEADS = new Set(["select", "with", "insert", "replace", "update", "delete", "values", "value",
  "from", "where", "group", "order", "having", "limit"]);
const QUERY_START = new Set(["select", "with", "insert", "replace", "update", "delete", "values"]);
const PREFIX_HEADS = new Set(["explain", "describe", "desc", "create", "prepare", "declare"]);

/** CREATE TABLE t (...): the columns one a line between the brackets on lines of their own. */
function createTable(w     , items        , base        )          {
  let at = -1;
  for (let k = 1; k < items.length && at < 0; k++) {
    const p = items[k - 1];
    if (isGrp(items[k]) && (items[k]       ).g === "paren" && !isGrp(p) && (p.k === "word" || p.k === "qid") &&
      items.slice(0, k).some((n      )          => { return kw(n) === "table"; })) at = k;
  }
  if (at < 0) return false;
  const g = items[at]       ;
  seq(w, items.slice(0, at), base + 4);
  w.nl(base);
  w.tok(g.open);
  for (const p of splitList(g.items)) {
    w.nl(base + 4);
    seq(w, p.items, base + 4);
    if (p.sep) w.tok(p.sep);
    for (const c of p.trail) w.tok(c);
  }
  if (g.close) {
    w.nl(base);
    w.tok(g.close);
  }
  seq(w, items.slice(at + 1), base + 4);
  return true;
}

function statement(w     , toks       , src        , base        )       {
  // The ";" and the comments on its line close the statement.
  let semi = -1;
  for (let k = toks.length - 1; k >= 0; k--) {
    if (toks[k].k === "punct" && toks[k].t === ";") { semi = k; break; }
    if (toks[k].k !== "lcmt" && toks[k].k !== "bcmt") break;
  }
  const body = semi >= 0 ? toks.slice(0, semi) : toks;
  const tail = semi >= 0 ? toks.slice(semi) : [];
  const first = body.find((t     )          => { return t.k !== "lcmt" && t.k !== "bcmt"; });
  const head = !first ? "" : first.k === "word" ? first.low : first.t;
  const items = tree(body);
  const saved = w.cont;
  w.cont = base;
  w.ddl = head === "create" || head === "alter";
  if (!body.length) { /* a lone ";" */ }
  else if (QUERY_HEADS.has(head) || head === "(") query(w, items, base);
  else if (PREFIX_HEADS.has(head) && prefixed(w, items, base, head)) { /* laid out */ }
  else if (head === "create" && createTable(w, items, base)) { /* laid out */ }
  else if (body.some((t     , k        )          => { return k > 0 && t.nl > 0; })) {
    // A statement of a kind not laid out here keeps the lines it was written on.
    if (w.pend) w.nl(base);
    w.raw(src.slice(body[0].pos, body[body.length - 1].end), body[body.length - 1]);
    if (body[body.length - 1].k === "lcmt") w.pend = true;
  } else seq(w, items, base + 4);
  for (const t of tail) w.tok(t);
  w.cont = saved;
}

/** EXPLAIN ..., CREATE VIEW v AS ..., DECLARE c CURSOR FOR ...: the head on its line, the
 *  query under it. */
function prefixed(w     , items        , base        , head        )          {
  const explain = head === "explain" || head === "describe" || head === "desc";
  let at = -1;
  for (let k = 1; k < items.length && at < 0; k++) {
    if (QUERY_START.has(kw(items[k])) && (explain || kw(items[k - 1]) === "as" || kw(items[k - 1]) === "for")) at = k;
  }
  if (at < 0) return false;
  seq(w, items.slice(0, at), base + 4);
  w.nl(base);
  query(w, items.slice(at), base);
  return true;
}

/** Blocks (a blank line apart, the console's Run unit) and in them statements (";" apart). */
function statements(toks       )                                    {
  const out                                    = [];
  let cur        = [];
  let curBlank = false, pendBlank = false, depth = 0;
  const close = ()       => { if (cur.length) out.push({ toks: cur, blank: curBlank }); cur = []; depth = 0; };
  for (let k = 0; k < toks.length; k++) {
    const t = toks[k];
    if (t.blank) { close(); pendBlank = true; }
    if (!cur.length) { curBlank = pendBlank && out.length > 0; pendBlank = false; }
    cur.push(t);
    if (t.k !== "punct") continue;
    if (t.t === "(") depth++;
    else if (t.t === ")") depth = Math.max(0, depth - 1);
    else if (t.t === ";" && depth === 0) {
      while (k + 1 < toks.length && (toks[k + 1].k === "lcmt" || toks[k + 1].k === "bcmt") && toks[k + 1].nl === 0) cur.push(toks[++k]);
      close();
    }
  }
  close();
  return out;
}

/** SPEC §data.console: the console's SQL laid out like a JetBrains IDE's SQL formatter.
 *  Whitespace only - a text whose tokens or blocks would not survive comes back unchanged. */
export function dbFormatSql(text        , o                = {})         {
  const src = text;
  const pg = o.dialect === "pg";
  const toks = lex(src, pg);
  if (!toks.length) return src.trim();
  const indent = Math.max(0, o.indent || 0);
  const w = new Out(indent, false);
  statements(toks).forEach((s                                 , k        )       => {
    if (k > 0) w.nl(indent, s.blank);
    statement(w, s.toks, src, indent);
  });
  const out = w.text().replace(/^[ \t]+/, "").trimEnd();
  // The promise, checked: the same tokens, a blank line before the same ones.
  const again = lex(out, pg);
  const same = again.length === toks.length && again.every((t     , k        )          => {
    return t.t === toks[k].t && (k === 0 || t.blank === toks[k].blank);
  });
  return same ? out : src;
}
