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
// @vitest-environment happy-dom

import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbConn, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";
import { tr } from "../src/i18n.js";
import type { ApiDbRedisValue, ApiDbStreamWindow } from "../src/types/api.js";

/* SPEC §data.tabs: the typed value table had NO right-click at all — a zset member that is
 *  a long JSON blob could only be glimpsed through the title hover, and stream values (a
 *  read-only pre) had nothing. The menu is the SPEC §data.grid vocabulary on the redis side:
 *  copy the cell, or open the same read-only viewer the SQL grid uses.
 *  SPEC §panel.pages: on happy-dom (it was a hand-rolled micro-DOM), so the menu is read the way a
 *  person meets it: a real contextmenu event on the cell, the #menu in the document, a click. */

const here = dirname(fileURLToPath(import.meta.url));
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

document.body.innerHTML = shellSkeleton();
const mod = await import("../src/data-browsers.js");

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  document.getElementById("menu")?.remove();
});

function mountValue(v: ApiDbRedisValue | ApiDbStreamWindow): HTMLElement {
  // The Data view's skeleton builds #dbGridWrap (index.html does not have it); the value
  // renderer only needs the box.
  const wrap = document.body.appendChild(Object.assign(document.createElement("div"), { id: "dbGridWrap" }));
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), { conn: "r", conns: [{ name: "r", dialect: "redis" }] });
  const k = freshTab("key");
  dbTabs()[0] = k;
  k.redisKey = "z";
  k.redisValue = v;
  mod.dbRenderRedisValue(wrap);
  return wrap;
}
function mountZset(value: unknown): HTMLElement {
  return mountValue({ key: "z", type: "zset", value: value, length: 1, ttl: -1 });
}
const cellWith = (wrap: HTMLElement, text: string): HTMLElement | undefined =>
  Array.from(wrap.querySelectorAll<HTMLElement>("td")).find((td) => td.textContent === text);
function rightClick(cell: HTMLElement): HTMLElement | null {
  cell.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  return document.getElementById("menu");
}
const labels = (menu: HTMLElement): string[] => Array.from(menu.querySelectorAll("button")).map((b) => b.textContent ?? "");

describe("the typed value table's right-click (SPEC §data.tabs)", () => {
  it("a zset member cell opens a menu offering copy and the read-only viewer", () => {
    const member = cellWith(mountZset({ "{\"id\":1}": "1767225600" }), "{\"id\":1}");
    expect(member, "the member cell exists").toBeTruthy();
    const menu = rightClick(member!);
    expect(menu, "the menu opened").not.toBeNull();
    expect(menu!.getAttribute("role")).toBe("menu");
    expect(labels(menu!)).toEqual([tr("logs.copyValue"), tr("dataCsv.viewValue")]);
  });

  it("View opens the value sheet carrying the whole member - the truncated cell, whole", () => {
    // A plain-text member: the viewer's pre guarantees the WHOLE string is in the sheet, which
    // is the complaint being fixed (the cell truncates, the title hover was the only peek).
    const long = "a-very-long-member-value-the-cell-truncates";
    const menu = rightClick(cellWith(mountZset({ [long]: "1767225600" }), long)!);
    Array.from(menu!.querySelectorAll("button")).find((b) => b.textContent === tr("dataCsv.viewValue"))!.click();
    const sheet = document.getElementById("sheet")!;
    expect(sheet.hidden, "the viewer sheet opened").toBe(false);
    expect(document.getElementById("menu"), "the click closed the menu").toBeNull();
    expect(sheet.querySelector(".sheet-body")!.textContent).toContain(long);
  });

  it("the member's own column is named in the viewer's caption, not a wire word", () => {
    const menu = rightClick(cellWith(mountZset({ "{\"id\":1}": "1767225600" }), "{\"id\":1}")!);
    Array.from(menu!.querySelectorAll("button")).find((b) => b.textContent === tr("dataCsv.viewValue"))!.click();
    const head = document.querySelector("#sheet .sheet-head")!;
    // The column header's own word (dataBrowsers.colMember), not the wire key or a raw
    // dictionary key; the sub names the key and its type.
    expect(head.querySelector("h2")!.textContent).toBe(tr("dataBrowsers.colMember"));
    expect(head.querySelector(".sheet-sub")!.textContent).toBe("z · zset");
  });

  it("a stream value's window cells carry the same right-click (SPEC §data.streams)", () => {
    // The stream view moved off the read-only pre onto its own window table; the SPEC §data.grid
    // cell menu travels with the cells that actually show data now.
    const wrap = mountValue({
      key: "z", type: "stream", ttl: -1, length: 1,
      entries: [{ id: "1690000000000-0", ts: "2023-07-22T04:00:00.000Z", fields: { a: "1" } }],
      columns: ["a"], more: false, firstId: "1690000000000-0", lastId: "1690000000000-0",
    });
    const cell = cellWith(wrap, "1");
    expect(cell, "the stream field cell exists").toBeTruthy();
    const menu = rightClick(cell!);
    expect(menu, "the menu opened on the stream cell too").not.toBeNull();
    expect(labels(menu!)).toEqual([tr("logs.copyValue"), tr("dataCsv.viewValue")]);
  });
});
