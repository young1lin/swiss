// @vitest-environment happy-dom
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

/* The chrome repaint and the 文/A flip guard (SPEC §panel.i18n): paintChrome rewrites the
   static index.html strings through tr() (English writes the same text back), and a
   canLeave veto aborts the flip BEFORE the preference is written — the guard must not cost
   the user their language. page-registry is mocked because toggleLang consults it at click
   time and the veto case needs canLeave=false without mounting a real page. */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { LANG_KEY, install, locale, paintChrome, toggleLang } from "../src/i18n.js";
import zh from "../src/locales/zh.js";
import { trackPaneScroll } from "../src/pane-scroll.js";

const flipGate = vi.hoisted(() => ({ veto: true }));
vi.mock("../src/page-registry.js", () => ({
  pageHasPendingChanges: () => flipGate.veto,
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
    // The refused flip speaks through the toast, so the skeleton carries the shell's own.
    '<div id="toast" class="toast" hidden></div>',
  ].join("");
}

describe("paintChrome and the language flip (SPEC §panel.i18n)", () => {
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

  it("the back-to-top button the shell built at boot follows the flip (SPEC §panel.ui)", () => {
    const pane = document.createElement("main");
    document.body.appendChild(pane);
    trackPaneScroll(pane);
    const top = document.getElementById("toTop")!;
    expect(top.getAttribute("aria-label")).toBe("Back to top");
    install("zh-CN", zh);
    paintChrome();
    expect(top.getAttribute("aria-label")).toBe("回到顶部");
    expect(top.title).toBe("回到顶部");
  });

  it("a canLeave veto stops the flip before the preference is written, and says so", async () => {
    await toggleLang();
    expect(localStorage.getItem(LANG_KEY)).toBe(null);
    expect(document.documentElement.lang).toBe("en");
    // A button that answers a click with nothing reads as broken (found live, SPEC §data.tabs).
    const t = document.getElementById("toast")!;
    expect(t.hidden).toBe(false);
    expect(t.textContent).toContain("Unsaved changes");
  });

  it("a blocked localStorage still flips for the page load (L4's in-memory choice)", async () => {
    vi.stubGlobal("localStorage", {
      getItem: () => { throw new Error("blocked"); },
      setItem: () => { throw new Error("blocked"); },
      removeItem: () => { throw new Error("blocked"); },
    });
    flipGate.veto = false;
    try {
      await toggleLang();
      expect(document.documentElement.lang).toBe("zh-CN");
      expect(locale()).toBe("zh-CN");
    } finally {
      flipGate.veto = true;
      vi.unstubAllGlobals();
    }
  });
});
