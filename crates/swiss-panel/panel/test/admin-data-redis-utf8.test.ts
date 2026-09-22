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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dbConn, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";

/* docs/43 M2 fixup: upstream Java services store their strings JSON-encoded, with astral
 *  characters escaped as literal surrogate-pair text. The wire hands the panel
 *  '"\uD83D\uDCC8⏫\uD83D\uDC46{0} rose {2} within {1} hr"' (verified live on the
 *  i18n_strings hash) and the value views used to paint those escapes verbatim.
 *  dbRedisDisplayText decodes DISPLAY; edit seeds keep the original bytes.
 *
 *  Escape-layer note: in THIS file a wire backslash is written as \\uXXXX in source (one
 *  real backslash after TS unescaping); expected emoji come from String.fromCodePoint so the
 *  assertions never depend on this file's own encoding. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) {
      if (c.tag === "#document-fragment") { c.children.forEach((k: Stub) => { k.parentNode = n; n.children.push(k); }); c.children = []; return c; }
      c.parentNode = n; n.children.push(c); return c;
    },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() { const p = n.parentNode as Stub | undefined; if (p) p.children = p.children.filter((x: Stub) => x !== n); },
    contains: () => false, closest: () => null,
    setAttribute(k: string, v: string) { if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => {} });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
    createDocumentFragment: () => el("#document-fragment"),
    createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
    getElementById: (id: string) => byId[id] || (byId[id] = el()),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener: () => {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const mod = await import(pathToFileURL(join(here, "data-browsers.ts")).href) as {
  dbRedisDisplayText: (raw: string) => string;
  dbRenderRedisValue: (wrap: Stub) => void;
};
function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}

const Q = String.fromCharCode(34);
const CHART = String.fromCodePoint(0x1f4c8); // the emoji the title names
const POINT = String.fromCodePoint(0x1f446);
const UP = String.fromCharCode(0x23eb);
// Exactly what the live hash serves: JSON literal, single backslashes, astral chars as
// surrogate pairs, the BMP arrow already a real character in the stored bytes.
const WIRE_TITLE = Q + "\\uD83D\\uDCC8" + UP + "\\uD83D\\uDC46{0} rose {2} within {1} hr" + Q;
const DECODED_TITLE = CHART + UP + POINT + "{0} rose {2} within {1} hr";

function mountHash(value: Record<string, unknown>): Stub {
  const wrap = el();
  byId.dbGridWrap = wrap;
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), { conn: "r", conns: [{ name: "r", dialect: "redis" }] });
  const k = freshTab("key");
  dbTabs()[0] = k;
  k.redisKey = "lang";
  k.redisValue = { key: "lang", type: "hash", value: value, length: 1, ttl: -1 };
  mod.dbRenderRedisValue(wrap);
  return wrap;
}

describe("redis values show the characters they name (docs/43 M2 fixup)", () => {
  it("the live wire title decodes: quotes gone, emoji and arrow whole, template body intact", () => {
    expect(mod.dbRedisDisplayText(WIRE_TITLE)).toBe(DECODED_TITLE);
  });

  it("the hash table's value cell paints the decoded text and stays editable", () => {
    const wrap = mountHash({ TITLE: WIRE_TITLE });
    const cell = find(wrap, (n) => n.tag === "td" && n.textContent.includes("rose"))[0];
    expect(cell, "the value cell exists").toBeTruthy();
    expect(cell.textContent.includes(Q), "the wrapping quotes are gone").toBe(false);
    expect(cell.textContent.includes("\\uD83D"), "no literal surrogate text left").toBe(false);
    expect(cell.textContent).toContain(CHART);
    expect(cell.textContent.includes("rose {2} within {1} hr"), "the template body survived").toBe(true);
    expect(typeof cell.ondblclick, "the value cell stays editable").toBe("function");
  });

  it("a string value's textarea opens decoded, so open-save is a no-op", () => {
    const wrap = el();
    byId.dbGridWrap = wrap;
    unmountDbView();
    mountDbView();
    Object.assign(dbConn(), { conn: "r", conns: [{ name: "r", dialect: "redis" }] });
    const k = freshTab("key");
    dbTabs()[0] = k;
    k.redisKey = "s";
    k.redisValue = { key: "s", type: "string", value: WIRE_TITLE, ttl: -1 };
    mod.dbRenderRedisValue(wrap);
    const ta = find(wrap, (n) => n.tag === "textarea")[0];
    expect(ta.value.includes("\\uD83D"), "no literal surrogate text in the editor").toBe(false);
    expect(ta.value).toContain(CHART);
    expect(ta.dataset.orig, "orig matches the decoded display").toBe(ta.value);
  });

  it("values without the pattern pass through byte-identical - no eager unescaping", () => {
    const plain = "c:\\users\\jdoe \\u0041nd a lone \\uD83D half-pair";
    expect(mod.dbRedisDisplayText(plain)).toBe(plain);
    const obj = "{\"a\": 1}"; // not a string literal - shown as stored
    expect(mod.dbRedisDisplayText(obj)).toBe(obj);
  });

  it("an unquoted stray surrogate pair folds too (double-escaped upstream payloads)", () => {
    const stray = "chart \\uD83D\\uDCC8! and box \\uD83E\\uDDEA done";
    const out = mod.dbRedisDisplayText(stray);
    expect(out.includes("\\uD83D")).toBe(false);
    expect(out).toContain(CHART);
    expect(out).toContain(String.fromCodePoint(0x1f9ea));
  });
});
