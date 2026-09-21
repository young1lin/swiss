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

/* The lookup's own contract (docs/38 §2.5): English passthrough, installed-dictionary
   lookups, placeholder handling with missing slots kept visible, plural selection in both
   languages, and the preference round-trip on a stubbed storage. Node env on purpose: the
   module under test touches localStorage and document only through guards, and those guards
   are part of what is being asserted. */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { LANG_KEY, install, langPref, loadLocale, locale, nextLang, setLang, tr, trn } from "../src/i18n.js";

/* A minimal storage: node has none, and a throwing one must be installable to cover the
 * catch paths. */
function makeStorage(initial?: Record<string, string>): Storage {
  const store = new Map<string, string>(Object.entries(initial ?? {}));
  return {
    get length() { return store.size; },
    clear: () => { store.clear(); },
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    removeItem: (k: string) => { store.delete(k); },
    setItem: (k: string, v: string) => { store.set(k, v); },
  };
}
const throwingStorage = {
  getItem: (): string => { throw new Error("blocked"); },
  setItem: (): void => { throw new Error("blocked"); },
  removeItem: (): void => { throw new Error("blocked"); },
} as unknown as Storage;

describe("i18n lookup and preference (docs/38 §2.5)", () => {
  beforeEach(() => {
    install("en", null);
    vi.stubGlobal("localStorage", makeStorage());
    vi.stubGlobal("document", { documentElement: { lang: "en" } });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    install("en", null);
  });

  it("tr passes the key through in English and looks it up once installed", () => {
    expect(tr("remoteRuns.runsTarget")).toBe("No runs on this target yet.");
    install("zh-CN", { "remoteRuns.runsTarget": "该目标还没有运行记录。" });
    expect(tr("remoteRuns.runsTarget")).toBe("该目标还没有运行记录。");
    expect(locale()).toBe("zh-CN");
    expect(tr("no.suchKeyAnywhere")).toBe("no.suchKeyAnywhere"); // unknown everywhere: the raw key, visibly, never undefined
  });

  it("install(en, null) returns to English", () => {
    install("zh-CN", { "i18n.search": "搜索" });
    expect(tr("i18n.search")).toBe("搜索");
    install("en", null);
    expect(tr("i18n.search")).toBe("Search");
    expect(locale()).toBe("en");
  });

  it("tr substitutes provided vars and keeps missing placeholders literally visible", () => {
    expect(tr("addSheet.importedNSkippedS", { n: 3, s: 1 })).toBe("Imported 3, skipped 1");
    expect(tr("addSheet.importedNSkippedS", { n: 3 })).toBe("Imported 3, skipped {s}");
  });

  it("trn selects one/other in English and only other in Chinese", () => {
    expect(trn(1, "dataGrid.nRows.one", "dataGrid.nRows.other")).toBe("1 row");
    expect(trn(2, "dataGrid.nRows.one", "dataGrid.nRows.other")).toBe("2 rows");
    expect(trn(0, "dataGrid.nRows.one", "dataGrid.nRows.other")).toBe("0 rows");
    install("zh-CN", { "dataGrid.nRows.other": "{n} 行" });
    expect(trn(1, "dataGrid.nRows.one", "dataGrid.nRows.other")).toBe("1 行");
    expect(trn(5, "dataGrid.nRows.one", "dataGrid.nRows.other")).toBe("5 行");
  });

  it("trn lets caller vars override the injected {n}", () => {
    expect(trn(2, "dataGrid.nRows.one", "dataGrid.nRows.other", { n: "two" })).toBe("two rows");
  });

  it("langPref defaults to en and only accepts zh-CN", () => {
    expect(langPref()).toBe("en");
    localStorage.setItem(LANG_KEY, "zh-CN");
    expect(langPref()).toBe("zh-CN");
    localStorage.setItem(LANG_KEY, "garbage");
    expect(langPref()).toBe("en");
    vi.stubGlobal("localStorage", throwingStorage);
    expect(langPref()).toBe("en");
  });

  it("setLang writes/erases the key and keeps <html lang> in step; nextLang flips", () => {
    expect(nextLang()).toBe("zh-CN");
    setLang("zh-CN");
    expect(localStorage.getItem(LANG_KEY)).toBe("zh-CN");
    expect(document.documentElement.lang).toBe("zh-CN");
    install("zh-CN", null);
    expect(nextLang()).toBe("en");
    setLang("en");
    expect(localStorage.getItem(LANG_KEY)).toBe(null);
    expect(document.documentElement.lang).toBe("en");
  });

  it("setLang survives a blocked storage (the choice still applies to the page)", () => {
    vi.stubGlobal("localStorage", throwingStorage);
    setLang("zh-CN");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("loadLocale installs the real dictionary only for a Chinese preference", async () => {
    await loadLocale();
    expect(tr("i18n.search")).toBe("Search");
    localStorage.setItem(LANG_KEY, "zh-CN");
    await loadLocale();
    expect(tr("i18n.search")).toBe("搜索");
    expect(locale()).toBe("zh-CN");
  });
});
