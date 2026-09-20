// @vitest-environment happy-dom
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

/* The chrome repaint and the 文/A flip guard (docs/38 §2.5): paintChrome rewrites the
   static index.html strings through tr() (English writes the same text back), and a
   canLeave veto aborts the flip BEFORE the preference is written — the guard must not cost
   the user their language. page-registry is mocked because toggleLang consults it at click
   time and the veto case needs canLeave=false without mounting a real page. */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { LANG_KEY, install, paintChrome, toggleLang } from "../src/i18n.js";
import zh from "../src/locales/zh.js";

vi.mock("../src/page-registry.js", () => ({
  pageHasPendingChanges: () => true,
  navigatePage: async () => {},
}));

/* The ids paintChrome touches, carrying the English attributes index.html ships. */
function chromeSkeleton(): string {
  return [
    '<aside class="rail" aria-label="Plugins"><nav id="railNav" aria-label="Plugins"></nav></aside>',
    '<input id="filter" type="search" placeholder="Search" aria-label="Filter MCPs">',
    '<button id="addBtn" title="New group" aria-label="New group"></button>',
    '<button id="themeBtn" title="Appearance" aria-label="Appearance"></button>',
    '<button id="langBtn" title="切换到中文" aria-label="Language"></button>',
    '<button id="expandBtn" title="Focus mode — hide app navigation (Esc exits)" aria-label="Focus mode"></button>',
    '<div id="sideCap">MCPs</div>',
    '<span id="memChip" title="swiss resident set">mem …</span>',
    '<nav id="list" aria-label="Hosted MCPs"></nav>',
  ].join("");
}

describe("paintChrome and the language flip (docs/38 §2.5)", () => {
  beforeEach(() => {
    document.body.innerHTML = chromeSkeleton();
    document.documentElement.lang = "en";
    localStorage.removeItem(LANG_KEY);
    install("en", null);
  });
  afterEach(() => { install("en", null); });

  it("an English paint writes the same strings back", () => {
    paintChrome();
    const filter = document.getElementById("filter") as HTMLInputElement;
    expect(filter.placeholder).toBe("Search");
    expect(filter.getAttribute("aria-label")).toBe("Filter MCPs");
    expect(document.getElementById("sideCap")!.textContent).toBe("MCPs");
  });

  it("an installed Chinese repaints the chrome", () => {
    install("zh-CN", zh);
    paintChrome();
    const filter = document.getElementById("filter") as HTMLInputElement;
    expect(filter.placeholder).toBe("搜索");
    expect(document.getElementById("sideCap")!.textContent).toBe("MCP");
    expect(document.getElementById("langBtn")!.getAttribute("aria-label")).toBe("语言");
    expect(document.getElementById("expandBtn")!.title).toContain("专注模式");
    expect(document.querySelector("aside.rail")!.getAttribute("aria-label")).toBe("插件");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("a canLeave veto stops the flip before the preference is written", async () => {
    await toggleLang();
    expect(localStorage.getItem(LANG_KEY)).toBe(null);
    expect(document.documentElement.lang).toBe("en");
  });
});
