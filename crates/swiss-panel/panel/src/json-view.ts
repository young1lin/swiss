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

import { h } from "./h.js";
import { tr } from "./i18n.js";

/* --- docs/33 C3: the Logs JSON view ------------------------------------------------------------
   A tool call's arguments and reply are shown as ONE formatted code block: standard JSON, two-space
   indent, everything visible, coloured by token. It replaced the C2 folding tree after the operator
   found that reading a reply took a click per level, and that the tree's own layout (sans-serif
   keys, quoted mono values, guide lines) was uncomfortable to read. It is the same view for every
   MCP — nothing here knows about SQL, redis or search. What it adds over JSON.stringify(v, null, 2):

     - A STRING THAT HOLDS JSON IS SHOWN AS JSON. Several MCPs reply with JSON inside a string:
       web_search_prime's whole reply is "[{\"title\":…}]", webReader's is "{\"title\":…}", and a
       redis hash's values are JSON text, sometimes encoded twice. Such a string is parsed (as many
       layers as it has) and printed as the structure it holds, behind a "decoded" marker, so the
       display never claims a shape the wire did not have. Copy raw hands back the wire text.
     - A REPLY THAT IS JSON FOLLOWED BY PROSE (figma's get_screenshot, the gateway's own
       "[showing the first N of M …]" note) keeps the JSON formatted and the prose as text below.
     - A string's line breaks are real line breaks, so a SQL statement or a message keeps its
       shape. Everything else in a string stays JSON-escaped (quotes, backslashes, tabs), so the
       only departure from strict JSON text is those line breaks.

   The block is a single <pre> of text spans: a drag-selection copies exactly what is shown,
   indentation included, and the marker is a ::before pseudo-element that no selection picks up. */

/** A string that held JSON, parsed for display. `layers` counts the encodings peeled off. Plain
 *  fields rather than parameter properties: the panel's emit only blanks erasable syntax. */
class DecodedString {
  readonly value: unknown;
  readonly layers: number;
  constructor(value: unknown, layers: number) {
    this.value = value;
    this.layers = layers;
  }
}

/** Lines a block shows before "Show all". Enough for any ordinary reply, and a bound on the DOM a
 *  1 MB stored body (calls.rs BODY_MAX) would otherwise build the moment its call opens. */
const JV_LINES = 200;

/** Parse text as JSON only when it can be an object, an array or a JSON string literal — never a
 *  bare number or word, so a value "50" stays the string it is and "true" stays text. */
function parseJsonText(s: string): unknown {
  const t = s.trim();
  const c = t.charAt(0);
  if (c !== "{" && c !== "[" && c !== "\"") return undefined;
  try { return JSON.parse(t); } catch (e) { return undefined; }
}

/** Where the first top-level object/array in `s` ends (exclusive), or -1 when it never closes.
 *  Strings are skipped with their escapes, so a brace inside a value closes nothing. */
function jsonEnd(s: string): number {
  let depth = 0;
  let inStr = false;
  let esc = false;
  for (let i = 0; i < s.length; i++) {
    const c = s.charAt(i);
    if (inStr) {
      if (esc) esc = false;
      else if (c === "\\") esc = true;
      else if (c === "\"") inStr = false;
      continue;
    }
    if (c === "\"") inStr = true;
    else if (c === "{" || c === "[") depth++;
    else if (c === "}" || c === "]") {
      depth--;
      if (depth === 0) return i + 1;
    }
  }
  return -1;
}

/** A block's text as JSON: the whole text as one value, or a leading object/array followed by
 *  prose (`tail`). Null when the text is not JSON at all — plain text, markdown, an error message,
 *  or a preview clipped mid-value — and the caller shows it exactly as it arrived. */
function splitJsonBlock(text: string): { value: unknown; tail: string } | null {
  const t = text.trim();
  if (!t) return null;
  try { return { value: JSON.parse(t), tail: "" }; } catch (e) { /* not one JSON value */ }
  const c = t.charAt(0);
  if (c !== "{" && c !== "[") return null;
  const end = jsonEnd(t);
  if (end < 0) return null; // unbalanced: a preview cut off mid-value
  try { return { value: JSON.parse(t.slice(0, end)), tail: t.slice(end).trim() }; } catch (e) { return null; }
}

/** Replace every string that is itself JSON with a DecodedString of what it holds, peeling as
 *  many layers as the wire stacked (a redis value can be a JSON string of a JSON string). */
function decodeStrings(v: unknown): unknown {
  if (typeof v === "string") {
    let cur: unknown = v;
    let layers = 0;
    while (typeof cur === "string") {
      const next = parseJsonText(cur);
      if (next === undefined) break;
      cur = next;
      layers++;
    }
    return layers ? new DecodedString(decodeStrings(cur), layers) : v;
  }
  if (Array.isArray(v)) return v.map((x) => { return decodeStrings(x); });
  if (v !== null && typeof v === "object") {
    const src = v as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    Object.keys(src).forEach((k) => { out[k] = decodeStrings(src[k]); });
    return out;
  }
  return v;
}

/** The decoded structure as plain JSON values — what Copy serialises. */
function plainValue(v: unknown): unknown {
  if (v instanceof DecodedString) return plainValue(v.value);
  if (Array.isArray(v)) return v.map((x) => { return plainValue(x); });
  if (v !== null && typeof v === "object") {
    const src = v as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    Object.keys(src).forEach((k) => { out[k] = plainValue(src[k]); });
    return out;
  }
  return v;
}

function hasDecoded(v: unknown): boolean {
  if (v instanceof DecodedString) return true;
  if (Array.isArray(v)) return v.some(hasDecoded);
  if (v !== null && typeof v === "object") {
    const src = v as Record<string, unknown>;
    return Object.keys(src).some((k) => { return hasDecoded(src[k]); });
  }
  return false;
}

/** A string literal as the block prints it: JSON-escaped, except that its line breaks (LF or
 *  CRLF) are real ones. Escaping each line separately keeps a literal backslash-n in the value
 *  ("C:\\new") an escape and never mistakes it for a break. */
function stringLiteral(s: string): string {
  return "\"" + s.split(/\r?\n/).map((part) => { return JSON.stringify(part).slice(1, -1); }).join("\n") + "\"";
}

/** A painted body and the number of lines its FULL text has (the Show all count). */
interface Painted { node: HTMLElement; lines: number }

/** The formatted code block for a (decoded) JSON value. Past JV_LINES the text is still walked —
 *  the Show all button needs the total — but no more nodes are built unless `all`. */
function jsonCodeNode(value: unknown, all: boolean): Painted {
  const pre = h("pre", { class: "jv" });
  let line = 1;
  const shown = (): boolean => { return all || line <= JV_LINES; };
  const put = (cls: string, text: string): void => {
    if (shown()) pre.appendChild(cls ? h("span", { class: cls }, text) : document.createTextNode(text));
    for (let i = text.indexOf("\n"); i >= 0; i = text.indexOf("\n", i + 1)) line++;
  };
  const marker = (layers: number): void => {
    if (!shown()) return;
    const label = layers > 1 ? tr("logs.decodedN", { n: layers }) : tr("logs.decoded");
    pre.appendChild(h("span", { class: "jv-dec", title: tr("logs.decodedTitle"), data: { label } }));
  };
  const emit = (v: unknown, indent: string): void => {
    if (v instanceof DecodedString) { marker(v.layers); emit(v.value, indent); return; }
    if (v === null || typeof v === "boolean") { put("jv-l", String(v)); return; }
    if (typeof v === "number") { put("jv-n", String(v)); return; }
    if (typeof v === "string") { put("jv-s", stringLiteral(v)); return; }
    const inner = indent + "  ";
    if (Array.isArray(v)) {
      if (!v.length) { put("jv-p", "[]"); return; }
      put("jv-p", "[");
      v.forEach((item, i) => {
        put("", "\n" + inner);
        emit(item, inner);
        if (i < v.length - 1) put("jv-p", ",");
      });
      put("", "\n" + indent);
      put("jv-p", "]");
      return;
    }
    const obj = v as Record<string, unknown>;
    const keys = Object.keys(obj);
    if (!keys.length) { put("jv-p", "{}"); return; }
    put("jv-p", "{");
    keys.forEach((k, i) => {
      put("", "\n" + inner);
      put("jv-k", JSON.stringify(k));
      put("jv-p", ": ");
      emit(obj[k], inner);
      if (i < keys.length - 1) put("jv-p", ",");
    });
    put("", "\n" + indent);
    put("jv-p", "}");
  };
  emit(value, "");
  return { node: pre, lines: line };
}

/** Text that is not JSON, exactly as it arrived, cut at JV_LINES unless `all`. */
function textNode(text: string, all: boolean, cls: string): Painted {
  const lines = text.split("\n");
  const body = all || lines.length <= JV_LINES ? text : lines.slice(0, JV_LINES).join("\n");
  return { node: h("pre", { class: cls }, body), lines: lines.length };
}

/** What Copy puts on the clipboard: the decoded structure as valid, indented JSON (with any prose
 *  that followed it), or the text unchanged when it is not JSON. Copy raw is the text itself. */
function formattedCopyText(text: string): string {
  const block = splitJsonBlock(text);
  if (!block) return text;
  const json = JSON.stringify(plainValue(decodeStrings(block.value)), null, 2);
  return block.tail ? json + "\n\n" + block.tail : json;
}

export { DecodedString, JV_LINES, decodeStrings, formattedCopyText, hasDecoded, jsonCodeNode, plainValue, splitJsonBlock, stringLiteral, textNode };
export type { Painted };
