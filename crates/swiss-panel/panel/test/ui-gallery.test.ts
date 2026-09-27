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

/* docs/46 G6 (U13, U17) - the gallery shows the whole library, zero tolerance.

   /admin/ui.html is where a component is judged and where a new design is proposed (a scene),
   so it is only worth anything if it is complete and honest:

   - complete: every value ui/index.ts exports is either CLAIMED - by a catalogue section's
     data-ui or a scene's `uses` - or named in HELPERS with the reason it draws nothing. A claim
     is checked against the DOM: the section (or scene) must contain the shape that function
     draws, so a ledger entry cannot outlive the drawing it stands for;
   - honest: the page links base.css and ui.css and nothing else, and every class it draws is
     one of theirs - a component that only looks right with views.css loaded is not in the
     library yet. The scenes carry no style and import nothing but the library, h and i18n;
   - alive: it boots on happy-dom with no error, and its switches and demos answer - the theme,
     the language, the width, a menu, a sheet, a field sheet's refusal, a call that opens. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import ts from "typescript";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { tr } from "../src/i18n.js";
import * as ui from "../src/ui/index.js";
import { boot } from "../src/ui-gallery.js";
import { SCENES } from "../src/ui-scenes.js";
import { assetsDir, libraryClasses } from "./styles.js";

const SRC = join(import.meta.dirname, "..", "src");

/** Exports that draw nothing a reader could look at - each with why it is not a section. */
const HELPERS: Record<string, string> = {
  dayLabel: "the timeline's day heading words - drawn by timeline()",
  timeLabel: "the timeline's clock words - drawn by timeline()",
  fmtMs: "the timeline's duration words - drawn by timeline()",
  relTime: "a relative moment's words - drawn inside the rows section's scheduled row",
  collapseRuns: "the timeline's ×N folding - drawn by timeline()",
  timelineToggle: "opens a timeline row in place - driven by the gallery's click (tested below)",
  clampMenuPos: "the menu's placement arithmetic - exercised by every popupMenu()",
  closeMenu: "closes popupMenu() - the demo's Escape and outside click",
  menuOpen: "popupMenu()'s open flag",
  setMenuOpen: "popupMenu()'s open flag, for the panel's own openers",
  closeSelect: "closes a styled select's list",
  selectOpen: "a styled select's open flag",
  initSelects: "styles every select on the page - the gallery boots it",
  closeSheet: "closes sheet() - the demo's Cancel and Escape",
  sheetOpen: "sheet()'s open flag",
  showSheet: "puts a sheet() on screen - the demo's opener",
  initSheet: "makes #sheet modal - the gallery boots it",
  decodeStrings: "parses JSON-in-a-string for jsonCodeNode() - fed to every code block here",
  splitJsonBlock: "Logs' body splitter - text in, parts out",
  formattedCopyText: "Logs' copy text - a string",
  hasDecoded: "a predicate over decodeStrings()'s output",
  plainValue: "undoes decodeStrings() - a value",
  stringLiteral: "JSON string quoting - a string",
  textNode: "jsonCodeNode()'s non-JSON fallback - drawn by it",
  DecodedString: "decodeStrings()'s marker class",
  JV_LINES: "jsonCodeNode()'s line cap - a number",
  JV_INLINE: "jsonCodeNode()'s one-line limit - a number",
  fitsOneLine: "the test jsonCodeNode({ oneLine }) applies - a predicate",
  isLocalAddress: "the loopback predicate redacted() applies - a predicate",
  stackSheet: "a second layer over the open sheet - its dialog is sheet(), its backdrop showSheet's; the tunnel key picker drives it",
};

/** The shape each drawing function leaves in the DOM: how a claim is checked. */
const SIG: Record<string, string> = {
  btn: ".btn:not(.icon)",
  iconBtn: ".btn.icon",
  moreBtn: ".btn.icon.ghost use[href=\"#i-ellipsis\"]",
  iconNode: "svg.ic > use",
  redacted: ".redact > code + button.redact-eye",
  dot: ".dot",
  heldDot: ".db-tab-dot",
  tag: ".tag",
  spinner: ".spin",
  note: ".note",
  failNote: ".fail-note[role=status] .fail-why",
  filterInput: "input.filter[type=search]",
  pager: ".pager[role=navigation] .pager-status[aria-live=polite]",
  valueBlock: ".vblock > .vblock-head .vblock-cap",
  timelineMeta: ".tl-meta",
  form: ".form .fld",
  field: ".fld > label.field > span + *",
  checkField: ".fld > label.check > input[type=checkbox]",
  pair: ".two > .fld + .fld",
  formActions: ".form-actions > .btn",
  hint: ".fld > .hint, .form > .hint",
  formCap: ".form-cap",
  formFold: "details.fold > summary + .fold-body .field-row > .btn",
  sw: ".sw[role=switch]",
  styleSelect: ".dd",
  pane: "main.pane",
  paneBody: ".wide",
  paneHead: ".pane-head",
  resHead: ".pane-head.res + .res-meta + .pane-nav .seg",
  pageFoot: ".page-foot",
  inlineForm: ".inline-form",
  section: "section.sec",
  card: ".group:not(.grp)",
  kvRow: ".kv",
  row: ".lrow",
  sideRow: ".side-row",
  groupNode: ".grp",
  seg: ".seg[role=tablist]",
  objTab: ".otab[role=tab] > button.otab-close",
  timeline: ".tl .tl-item",
  jsonCodeNode: "pre.jv",
  emptyNode: ".empty",
  popupMenu: "[data-demo=menu]",
  sheet: "[data-demo=sheet]",
  openFieldSheet: "[data-demo=field]",
  anchoredMenu: ".pane-actions [data-demo=anchored]",
  toTop: "[data-demo=totop]",
};

const settle = (): Promise<void> => new Promise((r) => { setTimeout(r, 0); });
const app = (): HTMLElement => document.getElementById("app")!;
const sheetHost = (): HTMLElement => document.getElementById("sheet")!;

/** A real click: dispatched on the element, bubbling - the gallery's handlers are delegated. */
function click(el: Element): void {
  el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
}
function key(el: Element, k: string): void {
  el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
}
async function go(hash: string): Promise<void> {
  history.replaceState(null, "", location.pathname + location.search + hash);
  window.dispatchEvent(new Event("hashchange"));
  await settle();
}
function exportedValues(): string[] {
  return Object.keys(ui).sort();
}
function catalogueClaims(): Map<string, Element> {
  const out = new Map<string, Element>();
  app().querySelectorAll<HTMLElement>("[data-ui]").forEach((sec) => {
    (sec.dataset.ui || "").split(/\s+/).filter(Boolean).forEach((u) => out.set(u, sec));
  });
  return out;
}

const errors: unknown[] = [];
const onError = (e: ErrorEvent): void => { errors.push(e.error ?? e.message); };
const consoleError = vi.spyOn(console, "error").mockImplementation((...a: unknown[]) => { errors.push(a); });
const indexHtml = readFileSync(join(assetsDir, "index.html"), "utf8");
const galleryHtml = readFileSync(join(assetsDir, "ui.html"), "utf8");

beforeAll(async () => {
  window.addEventListener("error", onError);
  // The sprite is read from index.html through DOMParser. A browser loads nothing for a parsed
  // document; happy-dom fetches its <link> sheets and logs the network failure - an artifact of
  // the test DOM, not of the page, so it is switched off rather than filtered out of `errors`.
  const settings = (window as unknown as { happyDOM: { settings: Record<string, boolean> } }).happyDOM.settings;
  settings.disableCSSFileLoading = true;
  settings.handleDisabledFileLoadingAsSuccess = true;
  vi.stubGlobal("fetch", async (url: string) => {
    if (url !== "/admin/index.html") throw new Error("the gallery fetched " + url);
    return { ok: true, status: 200, text: async () => indexHtml };
  });
  history.replaceState(null, "", "/admin/ui.html?theme=light&lang=en&w=1440");
  document.body.innerHTML = "<div id=\"app\"></div><div id=\"sheet\" class=\"backdrop\" hidden></div>";
  await boot();
  await settle();
});
afterAll(() => {
  window.removeEventListener("error", onError);
  consoleError.mockRestore();
  vi.unstubAllGlobals();
});

describe("docs/46 G6 - ui.html is the library and nothing else", () => {
  it("links base.css and ui.css only, and loads the gallery as a module", () => {
    const sheets = Array.from(galleryHtml.matchAll(/<link rel="stylesheet" href="([^"]+)">/g)).map((m) => m[1]);
    expect(sheets).toEqual(["/admin/styles/base.css", "/admin/styles/ui.css"]);
    expect(galleryHtml).toContain("<script type=\"module\" src=\"/admin/js/ui-gallery.js\"></script>");
    expect(galleryHtml).toContain("data-page=\"gallery\"");
    // The sprite comes from index.html at boot; a copy here would be a second list of icons.
    expect(galleryHtml).not.toContain("<symbol");
  });

  const IMPORTS: Record<string, string[]> = {
    "ui-gallery.ts": ["./h.js", "./i18n.js", "./ui-scenes.js", "./ui/index.js", "./locales/zh.js"],
    "ui-scenes.ts": ["./h.js", "./i18n.js", "./ui/index.js"],
  };
  for (const [file, allowed] of Object.entries(IMPORTS)) {
    it(file + " imports the library, h and i18n - no api, no state, no views", () => {
      const src = readFileSync(join(SRC, file), "utf8");
      const specs = Array.from(src.matchAll(/\bfrom\s*["']([^"']+)["']|\bimport\s*\(\s*["']([^"']+)["']\s*\)/g)).map((m) => m[1] || m[2]);
      expect(specs.length).toBeGreaterThan(0);
      expect(specs.filter((s) => !allowed.includes(s))).toEqual([]);
    });
  }

  it("the scenes write no style - not a prop, not a property", () => {
    const text = readFileSync(join(SRC, "ui-scenes.ts"), "utf8");
    const sf = ts.createSourceFile("ui-scenes.ts", text, ts.ScriptTarget.Latest, true);
    const hits: string[] = [];
    const visit = (n: ts.Node): void => {
      if (ts.isPropertyAssignment(n) && (ts.isIdentifier(n.name) || ts.isStringLiteral(n.name)) && n.name.text === "style") hits.push("style: prop");
      if (ts.isPropertyAccessExpression(n) && n.name.text === "style") hits.push(".style");
      if (ts.isStringLiteral(n) && /\bstyle\s*=/.test(n.text)) hits.push("style= in a string");
      ts.forEachChild(n, visit);
    };
    visit(sf);
    expect(hits).toEqual([]);
  });
});

describe("docs/46 G6 - every export is on the page", () => {
  it("the barrel is what the ledger is read against", () => {
    expect(exportedValues().length).toBeGreaterThan(30);
  });

  it("every export is claimed by a section or a scene, or is a named helper", () => {
    const claimed = new Set<string>(catalogueClaims().keys());
    SCENES.forEach((s) => s.uses.forEach((u) => claimed.add(u)));
    const missing = exportedValues().filter((n) => !claimed.has(n) && !(n in HELPERS));
    expect(missing, "draw it in a gallery section, or name it in HELPERS with why it draws nothing").toEqual([]);
  });

  it("every claim and every helper names a real export, and none is both", () => {
    const real = new Set(exportedValues());
    const claims = new Set<string>(catalogueClaims().keys());
    SCENES.forEach((s) => s.uses.forEach((u) => claims.add(u)));
    expect(Array.from(claims).filter((c) => !real.has(c))).toEqual([]);
    expect(Object.keys(HELPERS).filter((c) => !real.has(c))).toEqual([]);
    expect(Object.keys(HELPERS).filter((c) => claims.has(c))).toEqual([]);
  });

  it("every catalogue claim is drawn in its own section", () => {
    const wrong: string[] = [];
    for (const [u, sec] of catalogueClaims()) {
      const sig = SIG[u];
      if (!sig) wrong.push(u + ": no signature in SIG");
      else if (!sec.matches(sig) && !sec.querySelector(sig)) wrong.push(u + ": its section has no " + sig);
    }
    expect(wrong).toEqual([]);
  });

  it("the icons section has one row per symbol of the one sprite", () => {
    const symbols = Array.from(indexHtml.matchAll(/<symbol id="i-([\w-]+)"/g)).map((m) => m[1]);
    expect(symbols.length).toBeGreaterThan(20);
    expect(document.querySelectorAll("body > svg[hidden] symbol")).toHaveLength(symbols.length);
    const sec = catalogueClaims().get("iconNode")!;
    const shown = Array.from(sec.querySelectorAll(".kv use")).map((u) => u.getAttribute("href"));
    expect(shown).toEqual(symbols.map((s) => "#i-" + s));
  });

  it("the page's selects are the library's styled select", () => {
    const selects = app().querySelectorAll("select");
    expect(selects.length).toBeGreaterThan(0);
    selects.forEach((s) => { expect(s.classList.contains("dd-native")).toBe(true); });
  });
});

describe("docs/46 G6 - the scenes", () => {
  it("there are the four §2.6 names", () => {
    expect(SCENES.map((s) => s.id)).toEqual(["content", "resource", "event", "empty"]);
  });

  for (const s of SCENES) {
    it("#scene-" + s.id + " renders under its tab, drawing everything it claims", async () => {
      await go("#scene-" + s.id);
      const current = app().querySelector(".ctx-tab[aria-current=page]");
      expect(current && current.getAttribute("href")).toBe("#scene-" + s.id);
      expect(app().querySelector("[data-ui]"), "the catalogue is gone").toBeNull();
      const missing = s.uses.filter((u) => !SIG[u] || !app().querySelector(".shell " + SIG[u].split(", ").join(", .shell ")));
      expect(missing).toEqual([]);
    });
  }

  it("the resource scene: the source list beside the selected resource, one call open", async () => {
    await go("#scene-resource");
    const rows = app().querySelectorAll("aside.sidebar .side-row");
    expect(rows.length).toBe(4);
    expect(Array.from(rows).filter((r) => r.getAttribute("aria-selected") === "true").map((r) => r.querySelector(".side-name")!.textContent)).toEqual(["orders-db"]);
    expect(app().querySelector("main.pane .pane-title")!.textContent).toBe("orders-db");
    // The resource head's layers are the pane frame's own children, so each can stick in it.
    const frame = app().querySelector("main.pane > .wide")!;
    expect(Array.from(frame.children).slice(0, 3).map((n) => n.className)).toEqual(["pane-head res", "res-meta", "pane-nav"]);
    expect(frame.querySelector(":scope > .pane-nav > .seg [aria-selected=true]")!.textContent).toBe(tr("gallery.s.logs"));
    expect(app().querySelectorAll(".tl-item.open .tl-body pre.jv")).toHaveLength(2);
    expect(app().querySelector(".tl-n")!.textContent).toBe(tr("ui.timesN", { n: 3 }));
  });

  it("the event scene: three days, a folded run, a failure as a red tag, the who column", async () => {
    await go("#scene-event");
    expect(app().querySelectorAll(".tl-day")).toHaveLength(3);
    expect(app().querySelector(".tl-n")!.textContent).toBe(tr("ui.timesN", { n: 2 }));
    expect(app().querySelectorAll(".tl-item .tag.bad")).toHaveLength(1);
    expect(app().querySelectorAll(".tl-who").length).toBeGreaterThan(0);
  });

  it("the scenes' days do not move with the clock: just after midnight the event scene still has three", async () => {
    // Found at 00:14: data placed minutes before the REAL now put "30 minutes ago" on yesterday.
    vi.useFakeTimers({ toFake: ["Date"] });
    try {
      const late = new Date();
      late.setHours(0, 10, 0, 0);
      vi.setSystemTime(late);
      await go("#scene-content");
      await go("#scene-event");
      expect(app().querySelectorAll(".tl-day")).toHaveLength(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("the empty scene: the one empty shape with its action", async () => {
    await go("#scene-empty");
    const empty = app().querySelector(".pane .empty")!;
    expect(empty.querySelector("[data-empty-action]")!.textContent).toBe(tr("gallery.s.emptyAction"));
  });

  it("an unknown hash is the catalogue", async () => {
    await go("#nope");
    expect(app().querySelector(".ctx-tab[aria-current=page]")!.getAttribute("href")).toBe("#components");
    expect(app().querySelectorAll("[data-ui]").length).toBeGreaterThan(10);
  });
});

describe("docs/46 G6 - only the library's classes are drawn", () => {
  const known = libraryClasses();
  const drawn = (): string[] => {
    const out = new Set<string>();
    app().querySelectorAll("[class]").forEach((el) => {
      (el.getAttribute("class") || "").split(/\s+/).filter(Boolean).forEach((c) => out.add(c));
    });
    return Array.from(out).sort();
  };

  for (const hash of ["#components"].concat(SCENES.map((s) => "#scene-" + s.id))) {
    it(hash + ": every class is one base.css or ui.css styles", async () => {
      await go(hash);
      await settle();
      const cls = drawn();
      expect(cls.length).toBeGreaterThan(10);
      expect(cls.filter((c) => !known.has(c))).toEqual([]);
    });
  }
});

describe("docs/46 G6 - the switches and demos answer", () => {
  it("theme: a click flips data-theme and the query, and back", async () => {
    await go("#components");
    click(app().querySelector("[data-g=theme]")!);
    await vi.waitFor(() => { expect(document.documentElement.dataset.theme).toBe("dark"); });
    expect(new URLSearchParams(location.search).get("theme")).toBe("dark");
    expect(location.hash, "the view switch keeps the route").toBe("#components");
    click(app().querySelector("[data-g=theme]")!);
    await vi.waitFor(() => { expect(document.documentElement.dataset.theme).toBe("light"); });
  });

  it("language: a click reads Chinese, and back to English", async () => {
    await go("#components");
    const enTitle = app().querySelector(".ctx-name")!.textContent;
    click(app().querySelector("[data-g=lang]")!);
    await vi.waitFor(() => { expect(new URLSearchParams(location.search).get("lang")).toBe("zh"); });
    await vi.waitFor(() => { expect(app().querySelector(".ctx-name")!.textContent).not.toBe(enTitle); });
    expect(app().querySelector(".ctx-name")!.textContent).toMatch(/[一-鿿]/);
    expect(document.title).toMatch(/[一-鿿]/);
    click(app().querySelector("[data-g=lang]")!);
    await vi.waitFor(() => { expect(app().querySelector(".ctx-name")!.textContent).toBe(enTitle); });
    expect(new URLSearchParams(location.search).get("lang")).toBe("en");
  });

  it("width: a click holds #app to 960, and back to 1440", async () => {
    await go("#components");
    click(app().querySelector("[data-g=width]")!);
    await vi.waitFor(() => { expect(app().style.width).toBe("960px"); });
    expect(new URLSearchParams(location.search).get("w")).toBe("960");
    expect(app().querySelector(".chip")!.textContent).toContain("960");
    click(app().querySelector("[data-g=width]")!);
    await vi.waitFor(() => { expect(app().style.width).toBe("1440px"); });
  });

  it("menu: the demo opens the one floating menu, Escape inside it closes it", async () => {
    await go("#components");
    click(app().querySelector("[data-demo=menu]")!);
    const menu = document.getElementById("menu")!;
    expect(ui.menuOpen()).toBe(true);
    expect(menu.querySelectorAll("button[role=menuitem]").length).toBeGreaterThan(3);
    expect(menu.querySelector("button.danger")!.textContent).toBe(tr("gallery.d.delete"));
    key(document.activeElement!, "Escape");
    expect(ui.menuOpen()).toBe(false);
  });

  it("menu: a click outside closes it", async () => {
    await go("#components");
    click(app().querySelector("[data-demo=menu]")!);
    expect(ui.menuOpen()).toBe(true);
    click(app().querySelector(".pane-head")!);
    expect(ui.menuOpen()).toBe(false);
  });

  it("sheet: the demo shows it, Cancel closes it; Escape closes a reopened one", async () => {
    await go("#components");
    click(app().querySelector("[data-demo=sheet]")!);
    expect(sheetHost().hidden).toBe(false);
    expect(sheetHost().querySelector(".sheet h2")!.textContent).toBe(tr("gallery.d.sheetTitle"));
    const cancel = Array.from(sheetHost().querySelectorAll(".sheet-foot .btn")).find((b) => b.textContent === tr("gallery.d.cancel"))!;
    click(cancel);
    expect(sheetHost().hidden).toBe(true);
    click(app().querySelector("[data-demo=sheet]")!);
    expect(sheetHost().hidden).toBe(false);
    key(sheetHost().querySelector(".sheet")!, "Escape");
    expect(sheetHost().hidden).toBe(true);
  });

  it("field sheet: a taken name paints the inline error and stays open; a fresh one closes it", async () => {
    await go("#components");
    click(app().querySelector("[data-demo=field]")!);
    expect(sheetHost().hidden).toBe(false);
    const input = sheetHost().querySelector<HTMLInputElement>(".sheet input")!;
    input.value = "taken";
    click(sheetHost().querySelector("#g-save")!);
    await vi.waitFor(() => { expect(sheetHost().querySelector<HTMLElement>("#g-err")!.hidden).toBe(false); });
    expect(sheetHost().querySelector("#g-err")!.textContent).toBe(tr("gallery.d.nameTaken"));
    expect(sheetHost().hidden).toBe(false);
    input.value = "fresh";
    click(sheetHost().querySelector("#g-save")!);
    await vi.waitFor(() => { expect(sheetHost().hidden).toBe(true); });
  });

  it("timeline: a click on a closed call opens it with its body, a second closes it", async () => {
    await go("#components");
    const item = app().querySelector<HTMLElement>(".tl-item:not(.open)")!;
    click(item.querySelector(".tl-sum")!);
    expect(item.classList.contains("open")).toBe(true);
    expect(item.querySelector(".tl-sum")!.getAttribute("aria-expanded")).toBe("true");
    expect(item.querySelector(".tl-body pre.jv")).not.toBeNull();
    click(item.querySelector(".tl-sum")!);
    expect(item.classList.contains("open")).toBe(false);
    expect(item.querySelector(".tl-body")).toBeNull();
  });

  it("anchored menu: a resource head's ⋯ hangs it inside the head's actions; Escape closes it", async () => {
    await go("#components");
    const more = app().querySelector<HTMLElement>("[data-demo=anchored]")!;
    click(more);
    const menu = document.getElementById("menu")!;
    expect(ui.menuOpen()).toBe(true);
    expect(menu.parentElement).toBe(more.closest(".pane-actions"));
    expect(menu.classList.contains("float")).toBe(false);
    expect(menu.querySelector(".menu-head")!.textContent).toBe(tr("gallery.d.menuHeading"));
    expect(document.activeElement).toBe(menu.querySelector("button"));
    key(document.activeElement!, "Escape");
    expect(ui.menuOpen()).toBe(false);
    expect(document.getElementById("menu")).toBeNull();
  });

  it("back to top: one per render, hidden at the top, shown a screen down, a click goes home", async () => {
    await go("#components");
    const buttons = document.querySelectorAll<HTMLButtonElement>("body > .btn.to-top");
    expect(buttons).toHaveLength(1);
    const top = buttons[0];
    expect(top.getAttribute("aria-label")).toBe(tr("ui.toTop"));
    const scroller = app().querySelector<HTMLElement>("main.pane")!;
    expect(top.classList.contains("on")).toBe(false);
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 600 });
    scroller.scrollTop = 500;
    scroller.dispatchEvent(new Event("scroll"));
    expect(top.classList.contains("on")).toBe(false);
    scroller.scrollTop = 1400;
    scroller.dispatchEvent(new Event("scroll"));
    expect(top.classList.contains("on")).toBe(true);
    const to = vi.spyOn(scroller, "scrollTo");
    click(top);
    // Smooth, since the test DOM reports no reduced-motion preference.
    expect(to).toHaveBeenCalledWith({ top: 0, behavior: "smooth" });
    await vi.waitFor(() => { expect(scroller.scrollTop).toBe(0); });
    scroller.dispatchEvent(new Event("scroll"));
    expect(top.classList.contains("on")).toBe(false);
    // A scene is a new pane: the old button goes with the old pane.
    await go("#scene-event");
    expect(document.querySelectorAll("body > .btn.to-top")).toHaveLength(1);
    expect(document.querySelector("body > .btn.to-top")).not.toBe(top);
  });

  it("nothing threw or logged an error along the way", () => {
    expect(errors).toEqual([]);
  });
});
