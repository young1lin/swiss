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

/* The loader's English floor (docs/38 §1.1): a Chinese dictionary import that rejects
   must not reject loadLocale itself. main.ts awaits loadLocale at the TOP LEVEL, so a
   rejection there kills the whole module graph and the panel boots dead; a 文/A click
   would instead leave an unhandled rejection with the preference already written. The
   zh module is mocked to fail at factory time, which surfaces as a rejected import. */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

vi.mock("../src/locales/zh.js", () => {
  throw new Error("stale asset tree - zh.js is missing");
});
vi.mock("../src/page-registry.js", () => ({
  pageHasPendingChanges: () => false,
  navigatePage: async () => {},
}));

import { LANG_KEY, loadLocale, locale, nextLang, setLang, toggleLang, tr } from "../src/i18n.js";

/* The ids paintChrome touches, so a full toggleLang does not die on a missing node
   before its assertions can run. */
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

describe("the zh dictionary failing to load (docs/38 §1.1 fallback floor)", () => {
  beforeEach(() => {
    document.body.innerHTML = chromeSkeleton();
    document.documentElement.lang = "en";
    localStorage.removeItem(LANG_KEY);
    setLang("en");
  });
  afterEach(() => { setLang("en"); });

  it("loadLocale resolves on the English floor instead of rejecting", async () => {
    localStorage.setItem(LANG_KEY, "zh-CN");
    setLang("zh-CN");
    await expect(loadLocale()).resolves.toBeUndefined();
    expect(locale()).toBe("en");
    expect(tr("i18n.search")).toBe("Search");
  });

  it("a failed flip reverts the preference so it stops disagreeing with the screen", async () => {
    await toggleLang();
    expect(locale()).toBe("en");
    expect(localStorage.getItem(LANG_KEY)).toBe(null);
    expect(document.documentElement.lang).toBe("en");
  });

  it("nextLang targets from the choice, so a second click flips back", async () => {
    expect(nextLang()).toBe("zh-CN");
    setLang("zh-CN");
    expect(nextLang()).toBe("en");
  });
});
