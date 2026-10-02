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

/* ================================================================================================
   MongoDB values in the panel (SPEC §data.mongo-panel) - the pure half.

   The gateway hands documents over as CANONICAL Extended JSON ({"$oid": …}, {"$numberLong": "…"},
   {"$date": {"$numberLong": "…"}}) so nothing about a value is lost on the way to the page and
   back: an Int64 past 2^53 is a string, a Double stays a Double, a Decimal128 keeps every digit.
   Nobody wants to READ or TYPE that, so this module speaks the mongo shell's own syntax on the
   operator's side of the seam and canonical EJSON on the wire's side:

     - shellTokens / shellText print a value the way mongosh does - ObjectId("…"),
       ISODate("…"), NumberLong("…"), NumberDecimal("…"), UUID("…"), /re/i - and print a Double
       with its decimal point (1.0), so what is printed parses back to the SAME type;
     - mongoParse reads that syntax back: unquoted keys, single or double quotes, comments,
       trailing commas, the shell's constructors and regex literals, strict JSON included. An
       integer that fits Int32 stays a plain JSON number (the server reads it as Int32), a
       longer one becomes $numberLong from its digits (never through a lossy JS number), and a
       number written with a point or an exponent becomes $numberDouble.

   The query bar, the document editor, the pipeline stages and the list view all go through
   these two functions, so a value survives read → edit → write without a type changing under
   it - the property the top-tier tools' editors are built around.

   A leaf module (no DOM, no imports): the vitest suite pins the grammar without a page.
   ================================================================================================ */

/** A JSON value as the wire and the parser carry it. */
                                                                                        

/** One printed token: the jv-* class the code block colours it with ("" = layout), and its text. */
                                          

const INT32_MIN = -2147483648;
const INT32_MAX = 2147483647;
const INT64_MIN = BigInt("-9223372036854775808");
const INT64_MAX = BigInt("9223372036854775807");

function isObj(v         )                             {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

/** `o[k] = v` that keeps every field an own field. `__proto__` is a legal BSON key, but plain
 *  assignment runs the prototype setter instead, and the field vanishes from keys and JSON. */
function setField(o                       , k        , v       )       {
  if (k === "__proto__") Object.defineProperty(o, k, { value: v, enumerable: true, writable: true, configurable: true });
  else o[k] = v;
}

function oneKey(m                       )                {
  const keys = Object.keys(m);
  return keys.length ? keys[0] : null;
}

/* --- types -------------------------------------------------------------------------------------- */

/** The BSON type of an Extended JSON wrapper document, or null when the document is data. The
 *  same reading as the gateway's bson_type (mongobrowser.rs), so the schema view and the list
 *  view never disagree about what a value is. */
export function wrapperType(m                       )                {
  const keys = Object.keys(m);
  const first = keys[0];
  if (!first || first.charAt(0) !== "$") return null;
  const n = keys.length;
  if (n === 1) {
    switch (first) {
      case "$oid": return "ObjectId";
      case "$numberInt": return "Int32";
      case "$numberLong": return "Int64";
      case "$numberDouble": return "Double";
      case "$numberDecimal": return "Decimal128";
      case "$date": return "Date";
      case "$timestamp": return "Timestamp";
      case "$minKey": return "MinKey";
      case "$maxKey": return "MaxKey";
      case "$symbol": return "Symbol";
      case "$code": return "Code";
      case "$regularExpression": return "RegExp";
      case "$dbPointer": return "DBPointer";
      case "$undefined": return "Undefined";
      case "$uuid": return "UUID";
      case "$binary": {
        const b = m.$binary;
        return isObj(b) && b.subType === "04" ? "UUID" : "Binary";
      }
      default: return null;
    }
  }
  if (n === 2 && first === "$code" && "$scope" in m) return "CodeWithScope";
  return null;
}

/** The BSON type name of one value, in the vocabulary Compass and the shell use. A bare JSON
 *  number only arrives from relaxed input: whole numbers read as Int32 when they fit, Int64
 *  when they do not, the rest as Double. Pure. */
export function mongoType(v         )         {
  if (v === null || v === undefined) return "Null";
  if (typeof v === "boolean") return "Boolean";
  if (typeof v === "string") return "String";
  if (typeof v === "number") {
    if (Number.isInteger(v)) return v >= INT32_MIN && v <= INT32_MAX ? "Int32" : "Int64";
    return "Double";
  }
  if (Array.isArray(v)) return "Array";
  if (isObj(v)) return wrapperType(v) || "Document";
  return "Null";
}

/** A Date's epoch milliseconds out of canonical ({"$numberLong"}) or relaxed (ISO string or a
 *  number) form, or null. */
export function dateMs(v         )                {
  if (!isObj(v)) return null;
  const d = v.$date;
  if (typeof d === "number") return d;
  if (typeof d === "string") { const t = Date.parse(d); return Number.isNaN(t) ? null : t; }
  if (isObj(d) && typeof d.$numberLong === "string") { const n = Number(d.$numberLong); return Number.isFinite(n) ? n : null; }
  return null;
}

/** A number out of any numeric form, or null — for sorting and stats, never for writing back. */
export function numberOf(v         )                {
  if (typeof v === "number") return v;
  if (!isObj(v)) return null;
  const k = oneKey(v);
  if (k === "$numberInt" || k === "$numberLong" || k === "$numberDouble" || k === "$numberDecimal") {
    const s = v[k];
    if (typeof s !== "string") return null;
    const n = Number(s);
    return Number.isNaN(n) && s !== "NaN" ? null : n;
  }
  return null;
}

/* --- base64 / hex (UUID and BinData) ------------------------------------------------------------ */

function hexToBase64(hex        )         {
  let bin = "";
  for (let i = 0; i + 1 < hex.length; i += 2) bin += String.fromCharCode(parseInt(hex.slice(i, i + 2), 16));
  return btoa(bin);
}

function base64ToHex(b64        )                {
  let bin        ;
  try { bin = atob(b64); } catch (e) { return null; }
  let hex = "";
  for (let i = 0; i < bin.length; i++) hex += bin.charCodeAt(i).toString(16).padStart(2, "0");
  return hex;
}

function uuidText(hex        )         {
  return hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-" + hex.slice(12, 16) + "-" + hex.slice(16, 20) + "-" + hex.slice(20);
}

/* --- printing ----------------------------------------------------------------------------------- */

const IDENT = /^[A-Za-z_$][\w$]*$/;
/** Scalar arrays and documents print on one line while their compact text stays this short. */
const INLINE_MAX = 72;

/** A key as the shell writes it: bare when it is an identifier, quoted otherwise. */
function keyText(k        )         {
  return IDENT.test(k) ? k : JSON.stringify(k);
}

/** A Double's text with the decimal point that keeps it a Double when read back. */
function doubleText(s        )         {
  if (s === "Infinity" || s === "-Infinity" || s === "NaN") return s;
  if (/[.eEn]/.test(s)) return s;
  return s + ".0";
}

function isoOf(ms        )                {
  if (!Number.isFinite(ms) || Math.abs(ms) > 8.64e15) return null;
  return new Date(ms).toISOString();
}

/** The shell spelling of one wrapper value as tokens, or null when `m` is plain data. */
function wrapperTokens(m                       )                      {
  const t = wrapperType(m);
  if (!t) return null;
  const call = (name        , arg              )               => [["jv-l", name], ["jv-p", "("], ...arg, ["jv-p", ")"]];
  const str = (s        )               => [["jv-s", JSON.stringify(s)]];
  switch (t) {
    case "ObjectId": return call("ObjectId", str(String(m.$oid)));
    case "Int32": return [["jv-n", String(m.$numberInt)]];
    case "Int64": return call("NumberLong", str(String(m.$numberLong)));
    case "Double": return [["jv-n", doubleText(String(m.$numberDouble))]];
    case "Decimal128": return call("NumberDecimal", str(String(m.$numberDecimal)));
    case "Date": {
      const ms = dateMs(m);
      const iso = ms === null ? null : isoOf(ms);
      if (iso) return call("ISODate", str(iso));
      // Outside what a JS Date can hold: the milliseconds, as the digits the wire carried.
      const d = m.$date;
      const raw = isObj(d) && typeof d.$numberLong === "string" ? d.$numberLong : String(ms);
      return [["jv-l", "new Date"], ["jv-p", "("], ["jv-n", raw], ["jv-p", ")"]];
    }
    case "UUID": {
      if (typeof m.$uuid === "string") return call("UUID", str(m.$uuid));
      const b = m.$binary;
      const hex = isObj(b) && typeof b.base64 === "string" ? base64ToHex(b.base64) : null;
      if (hex && hex.length === 32) return call("UUID", str(uuidText(hex)));
      break;
    }
    case "Binary": {
      const b = m.$binary;
      if (isObj(b) && typeof b.base64 === "string" && typeof b.subType === "string") {
        return [["jv-l", "BinData"], ["jv-p", "("], ["jv-n", String(parseInt(b.subType, 16))], ["jv-p", ", "], ["jv-s", JSON.stringify(b.base64)], ["jv-p", ")"]];
      }
      break;
    }
    case "RegExp": {
      const r = m.$regularExpression;
      if (isObj(r) && typeof r.pattern === "string") {
        const opts = typeof r.options === "string" ? r.options : "";
        // A literal cannot hold an unescaped "/" or a line break; the constructor form can.
        if (r.pattern.length && !/[\n\r]/.test(r.pattern) && !/(^|[^\\])(\\\\)*\//.test(r.pattern)) {
          return [["jv-s", "/" + r.pattern + "/" + opts]];
        }
        return [["jv-l", "RegExp"], ["jv-p", "("], ["jv-s", JSON.stringify(r.pattern)], ["jv-p", ", "], ["jv-s", JSON.stringify(opts)], ["jv-p", ")"]];
      }
      break;
    }
    case "Timestamp": {
      const ts = m.$timestamp;
      if (isObj(ts)) {
        return [["jv-l", "Timestamp"], ["jv-p", "({ "], ["jv-k", "t"], ["jv-p", ": "], ["jv-n", String(ts.t)], ["jv-p", ", "],
          ["jv-k", "i"], ["jv-p", ": "], ["jv-n", String(ts.i)], ["jv-p", " })"]];
      }
      break;
    }
    case "MinKey": return [["jv-l", "MinKey()"]];
    case "MaxKey": return [["jv-l", "MaxKey()"]];
    case "Code": return call("Code", str(String(m.$code)));
    default: break;
  }
  return null;
}

/** Whether a value prints on one line inside its parent (a short scalar array, a small flat
 *  document). */
function compact(v       , budget        )         {
  if (v === null || typeof v !== "object") return JSON.stringify(v).length;
  if (isObj(v) && wrapperType(v)) {
    const toks = wrapperTokens(v);
    return toks ? toks.reduce((n        , t            )         => n + t[1].length, 0) : budget + 1;
  }
  let n = 2;
  if (Array.isArray(v)) {
    for (const x of v) {
      if (x !== null && typeof x === "object" && !(isObj(x) && wrapperType(x))) return budget + 1;
      n += compact(x, budget) + 2;
      if (n > budget) return n;
    }
    return n;
  }
  for (const k of Object.keys(v)) {
    const x = v[k];
    if (x !== null && typeof x === "object" && !(isObj(x) && wrapperType(x))) return budget + 1;
    n += keyText(k).length + 2 + compact(x, budget) + 2;
    if (n > budget) return n;
  }
  return n;
}

/** A value in the shell's syntax as coloured tokens: two-space indent, one member per line, a
 *  short flat array or document on one line. `oneLine` prints everything on one line - a table
 *  cell, a caption. Pure. */
export function shellTokens(v         , o                        = {})               {
  const out               = [];
  const emit = (x       , indent        , flat         )       => {
    if (x === null) { out.push(["jv-l", "null"]); return; }
    if (typeof x === "boolean") { out.push(["jv-l", String(x)]); return; }
    if (typeof x === "number") { out.push(["jv-n", String(x)]); return; }
    if (typeof x === "string") { out.push(["jv-s", JSON.stringify(x)]); return; }
    if (isObj(x)) {
      const w = wrapperTokens(x);
      if (w) { out.push(...w); return; }
    }
    const inner = indent + "  ";
    const one = flat || compact(x, INLINE_MAX) <= INLINE_MAX;
    if (Array.isArray(x)) {
      if (!x.length) { out.push(["jv-p", "[]"]); return; }
      out.push(["jv-p", "["]);
      x.forEach((item       , i        )       => {
        out.push(["", one ? (i ? " " : "") : "\n" + inner]);
        emit(item, inner, one);
        if (i < x.length - 1) out.push(["jv-p", ","]);
      });
      out.push(["", one ? "" : "\n" + indent]);
      out.push(["jv-p", "]"]);
      return;
    }
    const keys = Object.keys(x);
    if (!keys.length) { out.push(["jv-p", "{}"]); return; }
    out.push(["jv-p", "{"]);
    keys.forEach((k        , i        )       => {
      out.push(["", one ? " " : "\n" + inner]);
      out.push(["jv-k", keyText(k)]);
      out.push(["jv-p", ": "]);
      emit(x[k], inner, one);
      if (i < keys.length - 1) out.push(["jv-p", ","]);
    });
    out.push(["", one ? " " : "\n" + indent]);
    out.push(["jv-p", "}"]);
  };
  emit(v         , "", !!o.oneLine);
  return out;
}

/** shellTokens as plain text - what the editor shows and Copy puts on the clipboard. Pure. */
export function shellText(v         , o                        = {})         {
  return shellTokens(v, o).map((t            )         => t[1]).join("");
}

/** A short one-line rendering for a table cell or a caption: scalars and wrappers in shell
 *  form, a document as its field count, an array as its length. Pure. */
export function cellText(v         , max = 120)         {
  if (Array.isArray(v)) return "[" + v.length + "]";
  if (isObj(v) && !wrapperType(v)) return "{" + Object.keys(v).length + "}";
  const s = shellText(v, { oneLine: true });
  return s.length > max ? s.slice(0, max - 1) + "…" : s;
}

/* --- parsing ------------------------------------------------------------------------------------ */

/** A parse failure with the 1-based line and column it happened at. */
export class MongoSyntaxError extends Error {
           line        ;
           col        ;
  constructor(message        , line        , col        ) {
    super(message);
    this.line = line;
    this.col = col;
  }
}

;                                                                        
;                                                                        

class Lexer {
           src        ;
  pos = 0;
  constructor(src        ) { this.src = src; }

  fail(message        , at        )        {
    let line = 1;
    let col = 1;
    for (let i = 0; i < at && i < this.src.length; i++) {
      if (this.src.charAt(i) === "\n") { line++; col = 1; } else col++;
    }
    throw new MongoSyntaxError(message, line, col);
  }

  skip()       {
    const s = this.src;
    for (;;) {
      const c = s.charAt(this.pos);
      if (c === " " || c === "\t" || c === "\n" || c === "\r" || c === " ") { this.pos++; continue; }
      if (c === "/" && s.charAt(this.pos + 1) === "/") {
        while (this.pos < s.length && s.charAt(this.pos) !== "\n") this.pos++;
        continue;
      }
      if (c === "/" && s.charAt(this.pos + 1) === "*") {
        const end = s.indexOf("*/", this.pos + 2);
        if (end < 0) this.fail("unterminated comment", this.pos);
        this.pos = end + 2;
        continue;
      }
      return;
    }
  }

  /** The next token. `valueAt` says a value may start here, which is the one place a "/" opens
   *  a regex literal instead of being a stray character. */
  next(valueAt         )      {
    this.skip();
    const s = this.src;
    const at = this.pos;
    if (at >= s.length) return { kind: "eof", text: "", at };
    const c = s.charAt(at);
    if ("{}[]():,".includes(c)) { this.pos++; return { kind: "punct", text: c, at }; }
    if (c === "\"" || c === "'") return this.string(c, at);
    if (c === "/" && valueAt) return this.regex(at);
    if (/[0-9.\-+]/.test(c)) {
      const m = /^[-+]?(?:Infinity|0[xX][0-9a-fA-F]+|(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?)/.exec(s.slice(at));
      if (m && m[0] !== "-" && m[0] !== "+" && m[0] !== ".") {
        this.pos += m[0].length;
        return { kind: "number", text: m[0], at };
      }
    }
    if (/[A-Za-z_$]/.test(c)) {
      const m = /^[A-Za-z_$][\w$]*/.exec(s.slice(at));
      const text = m ? m[0] : c;
      this.pos += text.length;
      return { kind: "ident", text, at };
    }
    return this.fail("unexpected character " + JSON.stringify(c), at);
  }

  string(q        , at        )      {
    const s = this.src;
    let i = at + 1;
    let out = "";
    while (i < s.length) {
      const c = s.charAt(i);
      if (c === q) { this.pos = i + 1; return { kind: "string", text: out, at }; }
      if (c === "\n") break;
      if (c === "\\") {
        const e = s.charAt(i + 1);
        const simple                         = { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f", v: "\v", "0": "\0" };
        if (e === "u") {
          const hex = s.slice(i + 2, i + 6);
          if (!/^[0-9a-fA-F]{4}$/.test(hex)) this.fail("bad \\u escape", i);
          out += String.fromCharCode(parseInt(hex, 16));
          i += 6;
          continue;
        }
        if (e === "x") {
          const hex = s.slice(i + 2, i + 4);
          if (!/^[0-9a-fA-F]{2}$/.test(hex)) this.fail("bad \\x escape", i);
          out += String.fromCharCode(parseInt(hex, 16));
          i += 4;
          continue;
        }
        if (e === "\n") { i += 2; continue; }
        out += simple[e] ?? e;
        i += 2;
        continue;
      }
      out += c;
      i++;
    }
    return this.fail("unterminated string", at);
  }

  regex(at        )      {
    const s = this.src;
    let i = at + 1;
    let inClass = false;
    while (i < s.length) {
      const c = s.charAt(i);
      if (c === "\n") break;
      if (c === "\\") { i += 2; continue; }
      if (c === "[") inClass = true;
      else if (c === "]") inClass = false;
      else if (c === "/" && !inClass) {
        const pattern = s.slice(at + 1, i);
        const m = /^[a-z]*/.exec(s.slice(i + 1));
        const flags = m ? m[0] : "";
        this.pos = i + 1 + flags.length;
        if (!pattern) this.fail("empty regular expression", at);
        return { kind: "regex", text: pattern, at, flags };
      }
      i++;
    }
    return this.fail("unterminated regular expression", at);
  }
}

/** An integer literal as the shell means it: Int32 when it fits (a plain JSON number, which
 *  the server reads as Int32), Int64 from its digits when it does not, Double past Int64. */
function integerValue(text        )        {
  const neg = text.charAt(0) === "-";
  const digits = text.replace(/^[-+]/, "").replace(/^0+(?=\d)/, "");
  const big = BigInt((neg ? "-" : "") + digits);
  if (big >= BigInt(INT32_MIN) && big <= BigInt(INT32_MAX)) return Number(big);
  if (big >= INT64_MIN && big <= INT64_MAX) return { $numberLong: big.toString() };
  return { $numberDouble: String(Number(big)) };
}

function numberValue(text        )        {
  if (/Infinity$/.test(text)) return { $numberDouble: text.charAt(0) === "-" ? "-Infinity" : "Infinity" };
  if (/^[-+]?0[xX]/.test(text)) {
    const neg = text.charAt(0) === "-";
    return integerValue((neg ? "-" : "") + BigInt(text.replace(/^[-+]/, "")).toString());
  }
  if (/^[-+]?\d+$/.test(text)) return integerValue(text);
  const n = Number(text);
  // The digits as typed when they are a plain decimal (keeps 0.10 as written); else the value.
  const typed = text.replace(/^\+/, "");
  return { $numberDouble: /^-?\d+\.\d+(e[-+]?\d+)?$/i.test(typed) ? typed : String(n) };
}

/** A random ObjectId (timestamp + random), for an insert that writes `ObjectId()`. */
export function newObjectId(now         = Date.now())         {
  const ts = Math.floor(now / 1000).toString(16).padStart(8, "0").slice(-8);
  let rest = "";
  const bytes = new Uint8Array(8);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") crypto.getRandomValues(bytes);
  else for (let i = 0; i < 8; i++) bytes[i] = Math.floor(Math.random() * 256);
  bytes.forEach((b        )       => { rest += b.toString(16).padStart(2, "0"); });
  return ts + rest;
}

function randomUuid()         {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const hex = newObjectId() + newObjectId().slice(0, 8);
  return uuidText(hex.slice(0, 12) + "4" + hex.slice(13, 16) + "8" + hex.slice(17, 32));
}

class Parser {
           lx       ;
  tok     ;
  constructor(src        ) {
    this.lx = new Lexer(src);
    this.tok = this.lx.next(true);
  }

  advance(valueAt         )       { this.tok = this.lx.next(valueAt); }

  expect(text        , valueAt         )       {
    if (this.tok.kind !== "punct" || this.tok.text !== text) {
      this.lx.fail("expected " + JSON.stringify(text) + (this.tok.kind === "eof" ? " before the end" : " but found " + JSON.stringify(this.tok.text || this.tok.kind)), this.tok.at);
    }
    this.advance(valueAt);
  }

  value()        {
    const t = this.tok;
    if (t.kind === "punct" && t.text === "{") return this.object();
    if (t.kind === "punct" && t.text === "[") return this.array();
    if (t.kind === "string") { this.advance(false); return t.text; }
    if (t.kind === "number") { this.advance(false); return numberValue(t.text); }
    if (t.kind === "regex") {
      this.advance(false);
      return { $regularExpression: { pattern: t.text, options: (t.flags || "").split("").sort().join("") } };
    }
    if (t.kind === "ident") return this.ident();
    if (t.kind === "eof") return this.lx.fail("expected a value before the end", t.at);
    return this.lx.fail("unexpected " + JSON.stringify(t.text), t.at);
  }

  object()        {
    this.advance(false);
    const out                        = {};
    while (!(this.tok.kind === "punct" && this.tok.text === "}")) {
      const k = this.tok;
      let key        ;
      if (k.kind === "string" || k.kind === "ident") key = k.text;
      else if (k.kind === "number") key = k.text;
      else return this.lx.fail(k.kind === "eof" ? "unclosed {" : "expected a field name but found " + JSON.stringify(k.text), k.at);
      this.advance(false);
      this.expect(":", true);
      setField(out, key, this.value());
      if (this.tok.kind === "punct" && this.tok.text === ",") { this.advance(false); continue; }
      if (!(this.tok.kind === "punct" && this.tok.text === "}")) {
        this.lx.fail(this.tok.kind === "eof" ? "unclosed {" : "expected \",\" or \"}\" but found " + JSON.stringify(this.tok.text), this.tok.at);
      }
    }
    this.advance(false);
    return out;
  }

  array()        {
    this.advance(true);
    const out          = [];
    while (!(this.tok.kind === "punct" && this.tok.text === "]")) {
      out.push(this.value());
      if (this.tok.kind === "punct" && this.tok.text === ",") { this.advance(true); continue; }
      if (!(this.tok.kind === "punct" && this.tok.text === "]")) {
        this.lx.fail(this.tok.kind === "eof" ? "unclosed [" : "expected \",\" or \"]\" but found " + JSON.stringify(this.tok.text), this.tok.at);
      }
    }
    this.advance(false);
    return out;
  }

  /** The arguments of a constructor call, as raw values (the constructor decides what they mean). */
  args()          {
    this.expect("(", true);
    const out          = [];
    while (!(this.tok.kind === "punct" && this.tok.text === ")")) {
      out.push(this.value());
      if (this.tok.kind === "punct" && this.tok.text === ",") { this.advance(true); continue; }
      if (!(this.tok.kind === "punct" && this.tok.text === ")")) this.lx.fail("expected \",\" or \")\"", this.tok.at);
    }
    this.advance(false);
    return out;
  }

  ident()        {
    const t = this.tok;
    const word = t.text;
    this.advance(false);
    if (word === "true") return true;
    if (word === "false") return false;
    if (word === "null" || word === "undefined") return null;
    if (word === "NaN") return { $numberDouble: "NaN" };
    if (word === "Infinity") return { $numberDouble: "Infinity" };
    if (word === "MinKey" || word === "MaxKey") {
      if (this.tok.kind === "punct" && this.tok.text === "(") this.args();
      return word === "MinKey" ? { $minKey: 1 } : { $maxKey: 1 };
    }
    let name = word;
    if (word === "new") {
      if (this.tok.kind !== "ident") return this.lx.fail("expected a constructor after new", this.tok.at);
      name = this.tok.text;
      this.advance(false);
    }
    if (!(this.tok.kind === "punct" && this.tok.text === "(")) {
      return this.lx.fail("unknown word " + JSON.stringify(word) + " - quote it if it is a string", t.at);
    }
    const a = this.args();
    return this.construct(name, a, t.at);
  }

  construct(name        , a         , at        )        {
    const text = (v                   )                => {
      if (typeof v === "string") return v;
      if (typeof v === "number") return String(v);
      if (isObj(v)) {
        const k = oneKey(v);
        if (k && (k === "$numberLong" || k === "$numberInt" || k === "$numberDouble") && typeof v[k] === "string") return v[k]          ;
      }
      return null;
    };
    switch (name) {
      case "ObjectId": {
        if (!a.length) return { $oid: newObjectId() };
        const h = text(a[0]);
        if (!h || !/^[0-9a-fA-F]{24}$/.test(h)) return this.lx.fail("ObjectId needs 24 hex characters", at);
        return { $oid: h.toLowerCase() };
      }
      case "ISODate": case "Date": {
        if (!a.length) return { $date: { $numberLong: String(Date.now()) } };
        const v = a[0];
        if (typeof v === "string") {
          const ms = Date.parse(v);
          if (Number.isNaN(ms)) return this.lx.fail("not a date: " + JSON.stringify(v), at);
          return { $date: { $numberLong: String(ms) } };
        }
        const digits = text(v);
        if (digits !== null && /^-?\d+$/.test(digits)) return { $date: { $numberLong: digits } };
        return this.lx.fail(name + " needs an ISO date string or milliseconds", at);
      }
      case "NumberLong": case "Long": {
        const s = text(a[0]);
        if (s === null || !/^[-+]?\d+$/.test(s)) return this.lx.fail(name + " needs a whole number", at);
        const big = BigInt(s.replace(/^\+/, ""));
        if (big < INT64_MIN || big > INT64_MAX) return this.lx.fail(name + " is out of the Int64 range", at);
        return { $numberLong: big.toString() };
      }
      case "NumberInt": case "Int32": {
        const s = text(a[0]);
        const n = s === null ? NaN : Number(s);
        if (!Number.isInteger(n) || n < INT32_MIN || n > INT32_MAX) return this.lx.fail(name + " needs a whole number in the Int32 range", at);
        return { $numberInt: String(n) };
      }
      case "Double": {
        const s = text(a[0]);
        const n = s === null ? NaN : Number(s);
        if (Number.isNaN(n) && s !== "NaN") return this.lx.fail("Double needs a number", at);
        return { $numberDouble: s === null ? "NaN" : String(n) };
      }
      case "NumberDecimal": case "Decimal128": {
        const s = text(a[0]);
        if (s === null || !/^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$|^[-+]?(Infinity|NaN)$/.test(s)) return this.lx.fail(name + " needs a decimal number", at);
        return { $numberDecimal: s };
      }
      case "UUID": {
        const s = a.length ? text(a[0]) : randomUuid();
        const hex = s === null ? "" : s.replace(/-/g, "").toLowerCase();
        if (!/^[0-9a-f]{32}$/.test(hex)) return this.lx.fail("UUID needs 32 hex digits", at);
        return { $binary: { base64: hexToBase64(hex), subType: "04" } };
      }
      case "BinData": {
        const sub = typeof a[0] === "number" ? a[0] : NaN;
        const b64 = a[1];
        if (!Number.isInteger(sub) || sub < 0 || sub > 255 || typeof b64 !== "string") return this.lx.fail("BinData needs a subtype and base64 text", at);
        return { $binary: { base64: b64, subType: sub.toString(16).padStart(2, "0") } };
      }
      case "HexData": {
        const sub = typeof a[0] === "number" ? a[0] : NaN;
        const hex = a[1];
        if (!Number.isInteger(sub) || typeof hex !== "string" || !/^([0-9a-fA-F]{2})*$/.test(hex)) return this.lx.fail("HexData needs a subtype and hex text", at);
        return { $binary: { base64: hexToBase64(hex), subType: sub.toString(16).padStart(2, "0") } };
      }
      case "Timestamp": {
        let tv                    = a[0];
        let iv                    = a[1];
        if (isObj(a[0])) { tv = a[0].t; iv = a[0].i; }
        const t = Number(text(tv));
        const i = Number(text(iv));
        if (!Number.isInteger(t) || !Number.isInteger(i) || t < 0 || i < 0) return this.lx.fail("Timestamp needs whole t and i", at);
        return { $timestamp: { t, i } };
      }
      case "RegExp": {
        const p = a[0];
        const o = a.length > 1 ? a[1] : "";
        if (typeof p !== "string" || typeof o !== "string") return this.lx.fail("RegExp needs a pattern and options", at);
        return { $regularExpression: { pattern: p, options: o.split("").sort().join("") } };
      }
      case "Code": {
        if (typeof a[0] !== "string") return this.lx.fail("Code needs its source as a string", at);
        return a.length > 1 && isObj(a[1]) ? { $code: a[0], $scope: a[1] } : { $code: a[0] };
      }
      default:
        return this.lx.fail("unknown constructor " + name + "()", at);
    }
  }

  end()       {
    if (this.tok.kind !== "eof") this.lx.fail("unexpected " + JSON.stringify(this.tok.text) + " after the value", this.tok.at);
  }
}

/** Read one value in the shell's syntax (strict JSON included) into canonical-safe Extended
 *  JSON. Throws MongoSyntaxError with the line and column. Pure. */
export function mongoParse(text        )        {
  const p = new Parser(text);
  const v = p.value();
  p.end();
  return v;
}

/** mongoParse for a document-valued input (a filter, a projection, a sort, a document): empty
 *  text is `empty` (the caller's "not sent"), anything that is not a document is an error that
 *  says so. Returns a message instead of throwing, for an input that should paint its error.
 *  Pure. */
export function parseDocument(text        , empty                               = {})                                                                {
  if (!text.trim()) return { value: empty, error: null };
  try {
    const v = mongoParse(text);
    if (!isObj(v)) return { value: null, error: "not a document - wrap it in { }" };
    return { value: v, error: null };
  } catch (e) {
    if (e instanceof MongoSyntaxError) return { value: null, error: e.message + " (line " + e.line + ", column " + e.col + ")" };
    return { value: null, error: String(e) };
  }
}

/* --- comparing and normalising ------------------------------------------------------------------ */

/** The canonical form of a value, every number and date wrapped: two values that would be the
 *  same BSON print the same text here, so an editor can tell "unchanged" from "changed" even
 *  when one side came from the wire and the other from the parser. Pure. */
export function canon(v         )        {
  if (v === null || v === undefined) return null;
  if (typeof v === "boolean" || typeof v === "string") return v;
  if (typeof v === "number") {
    if (Number.isInteger(v) && v >= INT32_MIN && v <= INT32_MAX) return { $numberInt: String(v) };
    if (Number.isInteger(v)) return { $numberLong: String(v) };
    return { $numberDouble: String(v) };
  }
  if (Array.isArray(v)) return v.map(canon);
  if (isObj(v)) {
    const ms = wrapperType(v) === "Date" ? dateMs(v) : null;
    if (ms !== null) return { $date: { $numberLong: String(ms) } };
    if (wrapperType(v)) return v;
    const out                        = {};
    Object.keys(v).forEach((k        )       => { setField(out, k, canon(v[k])); });
    return out;
  }
  return null;
}

/** Same BSON value? Field order counts, exactly as BSON comparison counts it. Pure. */
export function sameValue(a         , b         )          {
  return JSON.stringify(canon(a)) === JSON.stringify(canon(b));
}

/** The key one document goes by in the page — its `_id` in canonical form. Pure. */
export function idKey(doc                         )         {
  return JSON.stringify(canon(doc._id));
}

/** The top-level fields of a page of documents, in first-seen order, `_id` first — the table
 *  view's columns. Pure. */
export function columnsOf(docs                           )           {
  const out           = [];
  docs.forEach((d                         )       => {
    Object.keys(d).forEach((k        )       => { if (!out.includes(k)) out.push(k); });
  });
  out.sort((a        , b        )         => (b === "_id" ? 1 : 0) - (a === "_id" ? 1 : 0));
  return out;
}

/** A document without its `_id` - what "Clone" opens the insert sheet with. Pure. */
export function withoutId(doc                       )                        {
  const out                        = {};
  Object.keys(doc).forEach((k        )       => { if (k !== "_id") setField(out, k, doc[k]); });
  return out;
}

/** The value at a dotted path, descending documents only, or undefined. Pure. */
export function valueAt(doc         , path        )          {
  let cur          = doc;
  for (const part of path.split(".")) {
    if (!isObj(cur) || wrapperType(cur)) return undefined;
    cur = cur[part];
  }
  return cur;
}
