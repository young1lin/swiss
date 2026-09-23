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

/* The MCP sidebar rebuilds its group bands only when its structural key changes (menu.ts
   patchSidebar) - the poll must not flash the list every six seconds. The bands' words (the
   empty line, the + and ⋯ labels) are built at rebuild time, so the language has to be part
   of that key: found live on 19998 in the docs/46 P1b second-language pass, where a 文/A flip
   left "No items - drop here or press +" on an otherwise Chinese screen until the membership
   next changed. */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { install } from "../src/i18n.js";
import zh from "../src/locales/zh.js";

function shellSkeleton(): string {
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

let patchSidebar: () => void;

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  const state = await import("../src/mcp-state.js");
  state.setMcpGroups(["default", "learn"]);
  state.setMcpRows([]);
  patchSidebar = (await import("../src/menu.js")).patchSidebar;
});

afterAll(() => { install("en", null); });

function band(name: string): HTMLElement {
  const g = document.querySelector<HTMLElement>('#list .grp[data-group="' + name + '"]');
  if (!g) throw new Error("no band " + name);
  return g;
}

describe("patchSidebar - the language is part of the rebuild key", () => {
  it("paints the bands in English, and a repeat paint keeps the same nodes", () => {
    install("en", null);
    patchSidebar();
    const first = band("learn");
    expect(first.querySelector(".grp-empty")?.textContent).toBe("No items — drop here or press +");
    expect(first.querySelector(".grp-more")?.getAttribute("aria-label")).toBe("Move, rename or delete this group");
    patchSidebar();
    expect(band("learn")).toBe(first); // nothing structural changed: no rebuild, no flash
  });

  it("a language flip rebuilds them in the new language, and flipping back rebuilds again", () => {
    install("en", null);
    patchSidebar();
    const english = band("learn");
    install("zh-CN", zh);
    patchSidebar();
    const chinese = band("learn");
    expect(chinese).not.toBe(english);
    expect(chinese.querySelector(".grp-empty")?.textContent).toBe(zh["groupLogic.itemsDropHerePress"]);
    expect(chinese.querySelector(".grp-more")?.getAttribute("aria-label")).toBe(zh["groups.moveRenameDeleteGroup"]);
    install("en", null);
    patchSidebar();
    expect(band("learn").querySelector(".grp-empty")?.textContent).toBe("No items — drop here or press +");
  });
});
