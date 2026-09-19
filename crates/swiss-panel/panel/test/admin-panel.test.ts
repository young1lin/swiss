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

import type { ApiMcpRow } from "../src/types/api.js";
import { describe, it, expect, beforeAll, afterAll, beforeEach, vi } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { setMemoryInfo } from "../src/mcp-state.js";

// The memory-chip click and the r key share polling.js but must not share side effects:
// refreshing the reading must never reload the active view under the pointer. The spy is
// what lets the suite tell "re-read memory" apart from "rebuild the view". Partial mock:
// everything else in page-registry stays real so the module graph still links.
vi.mock("../src/page-registry.js", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  return { ...actual, refreshPage: vi.fn() };
});

// The panel is a tree of ES modules served straight from disk — no bundler, so nothing but
// these checks stands between a typo'd import and a blank dashboard. The graph walk below does
// what a browser would: start at the entry the shell references, follow every static import,
// and verify each resolves to a file that exports the name. Filesystem, not HTTP — the serving
// side (content types, no-store, the path guard, the version stamp) is the Rust crate's own
// suite in crates/swiss-panel/src/admin.rs.
describe("admin panel assets", () => {
  const panelDir = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets");
  const jsDir = join(panelDir, "js");

  it("the shell references its styles and its module entry, with the theme boot inline", () => {
    const shell = readFileSync(join(panelDir, "index.html"), "utf8");
    expect(shell).toContain('src="/admin/js/main.js"');
    expect(shell).toContain('href="/admin/styles/base.css"');
    expect(shell).toContain('href="/admin/styles/views.css"');
    // The pre-paint theme resolver must stay inline in the shell — a linked script would flash.
    // (The localStorage key was renamed swiss_theme by the rebrand; this assertion lagged it.)
    expect(shell).toContain("swiss_theme");
    // And the 6k-line monolith it replaces is really gone. The bound rose twice with the icon
    // sprite — docs/18 V2 (~26 lines of hand-drawn symbols), then docs/29 (13 more: the
    // launch-tag glyphs) — still small, still no markup. The Apache-2.0 banner is stripped
    // before counting: it is a fixed licence cost, not shell content, and it must never buy
    // anyone headroom.
    const shellBody = shell.replace(/^<!--[\s\S]*?-->\s*/, "");
    expect(shellBody.split("\n").length).toBeLessThan(140);
  });

  it("links the whole module graph: every import resolves to a file that exports the name", () => {
    /* Why this walk is the strict link check, not the boot test below: vitest evaluates modules
       through its SSR transform, where a missing export resolves to undefined with at most a
       warning — the browser's native linker rejects the whole MODULE. The 2026-09 shell
       refactor shipped `export { paintImmersive as paint }` with no `paintImmersive` declared
       and every Node-side gate stayed green; only the browser went blank. So this test owns
       link-correctness and must be stricter than Node: it walks EVERY module on disk (page
       entries load lazily via dynamic import and would otherwise never be visited), and it
       reads export specs with real alias semantics — in `export { a as b }` the name importers
       must ask for is b. */
    const bodies = new Map<string, string>(); // path -> body
    const queue: string[] = [];
    const walkDir = (rel: string) => {
      for (const f of readdirSync(join(jsDir, rel), { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
        const p = rel ? rel + "/" + f.name : f.name;
        if (f.isDirectory()) walkDir(p);
        else if (f.name.endsWith(".js")) queue.push(p);
      }
    };
    walkDir("");
    while (queue.length) {
      const path = queue.shift()!;
      if (bodies.has(path)) continue;
      bodies.set(path, readFileSync(join(jsDir, path), "utf8")); // a missing file throws here
      for (const m of bodies.get(path)!.matchAll(/import\s*\{([^}]*)\}\s*from\s*"((?:\.{1,2})(?:\/[\w.-]+)+)"/g)) {
        // Resolve the specifier like the browser would: "." stays in the importing
        // module's directory, ".." climbs one level, and intermediate segments are KEPT
        // ("./views/tokens.js" is views/tokens.js, not tokens.js).
        const parts = path.split("/").slice(0, -1); // the importing module's directory
        for (const seg of m[2].split("/").slice(0, -1)) {
          if (seg === "..") parts.pop();
          else if (seg !== ".") parts.push(seg);
        }
        const target = [...parts, m[2].split("/").pop()!].join("/");
        queue.push(target);
        // The imported names must appear in the target's export list — the check that catches a
        // rename applied on one side of an import and forgotten on the other. Vendored files
        // are exempt from the NAME check (they ship verbatim in whatever export dialect they
        // came in — cronstrue is not ours to restyle), but not from the existence check above.
        const tbody = readFileSync(join(jsDir, target), "utf8");
        const exported = new Set<string>();
        if (!target.startsWith("vendor/")) {
          const exportMatch = tbody.match(/export\s*\{([^}]*)\}/);
          for (const spec of (exportMatch?.[1] ?? "").split(",").map((s) => s.trim()).filter(Boolean)) {
            // `export { a as b }`: the importable name is the alias, b.
            exported.add(spec.split(/\s+as\s+/).pop()!.trim());
          }
          // Inline declarations are the other export dialect the tree actually uses
          // (terminal-core.js ships `export function configPutBody(...)` with no clause).
          for (const d of tbody.matchAll(/export\s+(?:async\s+)?(?:function\*?|const|let|var|class)\s+([\w$]+)/g)) {
            exported.add(d[1]);
          }
        }
        if (target.startsWith("vendor/")) continue; // vendored: presence checked, names not ours
        for (let sym of m[1].split(",")) {
          sym = sym.trim();
          if (!sym) continue;
          // `groupOf as makeGroupOf` binds the ORIGINAL name; the alias is local. Checking
          // the whole spec string made every renamed import look missing — a false alarm
          // that first fired on sidebar.js's `groupOf as makeGroupOf`.
          const original = sym.split(/\s+as\s+/)[0].trim();
          expect(exported.has(original), `${path} imports ${original} from ${target}, which does not export it`).toBe(true);
        }
      }
    }
    // The graph is not one file wearing a directory costume.
    expect(bodies.size).toBeGreaterThan(15);
    // Every entry module is reachable from main.js — an orphan module is dead code at best.
    expect(bodies.has("main.js")).toBe(true);
  });

  // The regression: dropdown.js appends its menu to <body> as .menu.float.dd-menu, which must
  // paint ABOVE the sheet backdrop (40) — every select inside a sheet (the rule editor's
  // SSH-connection picker among them) opens its list over that full-screen overlay, where no click
  // can reach it: the backdrop eats the hit and closes the sheet instead. The first fix stated
  // z-index: 45 on .dd-menu in base.css and still shipped the bug — views.css loads after base.css,
  // and its .menu { z-index: 30 } won the same-specificity tie. Only the WINNING declaration after
  // the whole cascade matters, so this test resolves it like a browser would: collect every
  // pure-class rule in both sheets that matches the menu's class set, rank by specificity then
  // sheet order then position, and demand the winner beats the backdrop.
  it("layers the dropdown menu above the sheet backdrop it can open over", () => {
    const styles = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles");
    // In link order — the shell references base.css before views.css, which is the whole trap.
    // Comments are stripped first, the way the browser sees the sheet — otherwise a comment
    // glued to the front of a selector ("/* … */ .menu {") defeats the pure-class check below.
    const sheets = ["base.css", "views.css"].map((f, fi) => ({ fi, css: readFileSync(join(styles, f), "utf8").replace(/\/\*[\s\S]*?\*\//g, "") }));
    const rules: Array<{ classes: string[]; z: number; rank: number[] }> = [];
    for (const s of sheets) {
      // Leaf rules only: a media query's wrapper cannot match (it is not a class list), while its
      // inner rules are still walked — [^{}] stops at the wrapper's own braces.
      for (const m of s.css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        const sel = m[1].trim();
        if (!/^\.[\w-]+(\.[\w-]+)*$/.test(sel)) continue; // pure class-set selectors only
        const z = m[2].match(/z-index:\s*(\d+)/);
        if (!z) continue;
        rules.push({ classes: sel.slice(1).split("."), z: Number(z[1]), rank: [s.fi, m.index ?? 0] });
      }
    }
    const menuClasses = ["menu", "float", "dd-menu"];
    const applies = rules.filter((r) => r.classes.every((c) => menuClasses.includes(c)));
    expect(applies.length, "no z-index rule reaches the dropdown menu at all").toBeGreaterThan(0);
    // Highest specificity wins; on a tie, the later rule (sheet order, then position) wins.
    applies.sort((a, b) => (b.classes.length - a.classes.length) || (b.rank[0] - a.rank[0]) || (b.rank[1] - a.rank[1]));
    const winner = applies[0];
    const backdrop = rules.find((r) => r.classes.join(".") === "backdrop");
    expect(backdrop, "the layer a sheet's dropdown must beat — update this test if the idiom changes").toBeDefined();
    expect(winner.z, "the dropdown's winning layer is " + winner.z + " (." + winner.classes.join(".") + "), under the backdrop's " + backdrop!.z).toBeGreaterThan(backdrop!.z);
  });

  // The regression this test exists for: a refactor once shipped modules whose cut boundaries
  // broke mid-statement — every static check passed, and the dashboard rendered blank. The only
  // check that catches that class is EXECUTION. Each module is imported under Node with DOM stubs;
  // a syntax error, a missing export used at load, or an evaluation-time throw fails here.
  it("boots: every module parses and evaluates (DOM-stubbed, no network)", async () => {
    const el = (): Record<string, unknown> => {
      const node: Record<string, unknown> = {
        style: {}, dataset: {}, hidden: false, disabled: false, checked: false, value: "",
        textContent: "", innerHTML: "",
        classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
        setAttribute: () => {}, getAttribute: () => "", removeAttribute: () => {},
        addEventListener: () => {}, removeEventListener: () => {},
        appendChild: (c: unknown) => c, removeChild: (c: unknown) => c, remove: () => {},
        querySelector: () => null, querySelectorAll: () => [],
        focus: () => {}, blur: () => {}, click: () => {},
        getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
        contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
      };
      node.cloneNode = () => el();
      return node;
    };
    const doc = {
      documentElement: el(), body: el(), head: el(),
      hidden: false, visibilityState: "visible", activeElement: null,
      getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
      querySelector: () => null, querySelectorAll: () => [],
      addEventListener: () => {}, removeEventListener: () => {},
    };
    const prevWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
    Object.assign(globalThis, {
      document: doc, window: globalThis,
      localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
      location: { reload: () => {} },
      matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
      confirm: () => true, alert: () => {},
      Blob: class {}, setInterval: () => 0, clearInterval: () => {},
      addEventListener: () => {}, removeEventListener: () => {},
    });
    try {
      const dir = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js");
      const files = readdirSync(dir).filter((f) => f.endsWith(".js")).sort();
      expect(files.length).toBeGreaterThan(15);
      for (const f of files) {
        await import(pathToFileURL(join(dir, f)).href); // a broken module rejects here
      }
    } finally {
      // Leave the worker as we found it, as far as we can.
      if (prevWindow) Object.defineProperty(globalThis, "window", prevWindow);
    }
  });
});

/* Visual refresh V1 (docs/18): the token contract. Regression anchors for the palette swap —
   hairline surfaces, a warm near-black dark side, tabular numerals — not proofs of the design;
   the design itself is verified on the 19998 instance with screenshots. */
describe("visual refresh V1 — token contract", () => {
  const base = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "base.css"), "utf8");

  it("cards and knobs are hairline rings, not drop shadows", () => {
    expect(base).not.toContain("0 1px 2px");
    expect(base).toContain("--shadow-card: 0 0 0 1px var(--sep)");
  });

  it("the dark side is a warm near-black, not the old blue-grey", () => {
    expect(base).not.toContain("#17181c");
    expect(base).toContain("--bg: #0f1012");
  });

  it("numbers are tabular panel-wide, on the body element", () => {
    expect(base).toMatch(/body\s*\{[^}]*font-variant-numeric:\s*tabular-nums/);
  });
});

/* Visual refresh V6 (docs/18): state and controls. Idle is a hollow ring — "not running" is the
   absence of a claim, not a blue claim. Launch tags are monochrome (mysql and redis were both
   red, pg and http both blue — the hues carried nothing a scan could use). The sidebar's
   selected row is an accent bar over a tint, not a white card floating out of the list. The
   segmented controls drop their drop-shadows for hairline rings. */
describe("visual refresh V6 — state and controls", () => {
  const base = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "base.css"), "utf8");

  it("launch tags are one colour: the per-tag hue table is gone", () => {
    expect(base).not.toContain("data-tag");
  });

  it("the idle dot is a hollow ring", () => {
    expect(base).toContain("inset 0 0 0 1.5px");
  });

  it("segmented controls carry hairline rings, not drop shadows", () => {
    const views = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "views.css"), "utf8");
    expect(views).not.toContain("0 1px 2px");
  });

  it("dotTitle speaks for every state the dots paint", async () => {
    // util.js has no module-top DOM access; the guard below only keeps this test independent
    // of whether an earlier suite already imported the graph.
    const anyG = globalThis as unknown as Record<string, unknown>;
    const prev = anyG.document;
    if (!anyG.document) {
      const elem = () => ({ style: {}, dataset: {}, innerHTML: "", textContent: "", classList: { add() {}, remove() {} }, setAttribute() {}, appendChild() {}, addEventListener() {} });
      anyG.document = { getElementById: () => elem(), createElement: () => elem(), querySelector: () => null, querySelectorAll: () => [], addEventListener() {}, documentElement: elem(), body: elem() };
    }
    try {
      const { dotTitle } = await import("../src/util.js");
      expect(dotTitle("up", 203)).toBe("up · 203 ms");
      expect(dotTitle("up")).toBe("up");
      expect(dotTitle("idle")).toBe("idle — starts on first request");
      expect(dotTitle("error", null, "SSH refused")).toBe("error: SSH refused");
      expect(dotTitle("error")).toBe("error");
      // The transient words and plain down say themselves — no synonyms of our own.
      expect(dotTitle("starting")).toBe("starting");
      expect(dotTitle("stopping")).toBe("stopping");
      expect(dotTitle("reconnecting")).toBe("reconnecting");
      expect(dotTitle("down")).toBe("down");
    } finally {
      if (prev === undefined) delete anyG.document;
    }
  });

  it("tooltipOf explains what idle means on a lazy proc", async () => {
    // menu.js's import graph touches the DOM at module top level (add-sheet.js assigns to
    // $("addBtn")), so a permissive stub comes first and the import is dynamic — the same
    // pattern as the jobs-v2 suite.
    const anyG = globalThis as unknown as Record<string, unknown>;
    const prev = anyG.document;
    if (!anyG.document) {
      const elem = () => ({ style: {}, dataset: {}, innerHTML: "", textContent: "", classList: { add() {}, remove() {} }, setAttribute() {}, appendChild() {}, addEventListener() {} });
      anyG.document = { getElementById: () => elem(), createElement: () => elem(), querySelector: () => null, querySelectorAll: () => [], addEventListener() {}, documentElement: elem(), body: elem() };
    }
    try {
      const { tooltipOf } = await import("../src/menu.js");
      const tip = tooltipOf({ name: "mysql", type: "proc", source: "managed", state: "idle" } as ApiMcpRow);
      expect(tip).toContain("idle");
      expect(tip).toContain("lazy");
      expect(tooltipOf({ name: "pg", type: "http", source: "managed", state: "up" } as ApiMcpRow)).not.toContain("lazy");
    } finally {
      if (prev === undefined) delete anyG.document;
    }
  });
});

/* The toolbar's refresh button is gone: every view already reloads on the 6s poll, so the
   button's only unique job was force-reloading Data (its poll deliberately leaves paged-in
   lists alone) - that job lives on the r key now. The chip itself is a reading, not a
   reload button: its click re-reads memory only, so renderMemory must paint it as a CONTROL
   (class, hover title, click) on every poll, not only in the old childrenPending retry
   case, or the affordance and the click die with the first re-render. */
describe("the memory chip is a memory-only control", () => {
  interface FakeChip { textContent: string; title: string; className: string; onclick: unknown; }
  const els = new Map<string, FakeChip>();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let polling: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let state: any;
  let refreshPageMock: ReturnType<typeof vi.fn>;

  beforeAll(async () => {
    const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
    Object.assign(globalThis, {
      document: {
        getElementById: (id: string) => {
          let e = els.get(id);
          if (!e) { e = { textContent: "", title: "", className: "", onclick: null }; els.set(id, e); }
          return e;
        },
        createElement: () => ({ textContent: "", title: "", className: "", innerHTML: "", style: {}, dataset: {}, classList: { add() {}, remove() {} }, setAttribute() {}, appendChild() {}, addEventListener() {} }),
        querySelector: () => null,
        querySelectorAll: () => [],
        addEventListener: () => {},
        documentElement: {},
        body: {},
      },
    });
    afterAll(() => {
      if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
      else delete (globalThis as Record<string, unknown>).document;
    });
    polling = await import("../src/polling.js");
    refreshPageMock = (await import("../src/page-registry.js")).refreshPage as ReturnType<typeof vi.fn>;
  });

  it("renderMemory paints the chip as a control on EVERY paint", () => {
    setMemoryInfo({ gatewayMb: 12.3, heapUsedMb: 1, heapTotalMb: 2, externalMb: 3, processCount: 1, childrenPending: false, at: "12:00:00" });
    polling.renderMemory();
    const chip = els.get("memChip")!;
    expect(chip.textContent).toBe("12.3 MB");
    expect(chip.onclick).toBe(polling.refreshMemoryNow); // the click re-reads memory ONLY
    expect(chip.className).toContain("button"); // and it must look clickable
    expect(chip.title).toContain("refresh"); // the hover says what it does
  });

  it("the old retry-only case stays a memory-only control", () => {
    setMemoryInfo({ gatewayMb: 12.3, heapUsedMb: 1, heapTotalMb: 2, externalMb: 3, processCount: 1, childrenPending: true, at: "12:00:00" });
    polling.renderMemory();
    expect(els.get("memChip")!.onclick).toBe(polling.refreshMemoryNow);
  });

  // The regression: the chip's click used to land in refreshNow, whose refreshPage()
  // rebuilt the active view - Data's paged-in list, scroll, focus - under the pointer. A
  // click on a number asks for the number, nothing else.
  it("the chip's click re-reads memory WITHOUT reloading the active view", async () => {
    const prevFetch = globalThis.fetch;
    const calls: string[] = [];
    globalThis.fetch = (async (path: string) => {
      calls.push(String(path));
      return { ok: true, json: async () => ({ gatewayMb: 42, heapUsedMb: 1, heapTotalMb: 2, externalMb: 3, processCount: 1 }) };
    }) as unknown as typeof fetch;
    try {
      setMemoryInfo({ gatewayMb: 12.3, heapUsedMb: 1, heapTotalMb: 2, externalMb: 3, processCount: 1, childrenPending: false, at: "12:00:00" });
      polling.renderMemory();
      (els.get("memChip")!.onclick as () => void)();
      await new Promise((resolve) => setTimeout(resolve, 0)); // let loadMemory settle
      expect(calls).toEqual(["/api/memory?tree=1"]); // the child walk, and NOTHING else
      expect(refreshPageMock).not.toHaveBeenCalled(); // the view was not reloaded
    } finally {
      globalThis.fetch = prevFetch;
    }
  });

  it("the r key keeps the full refresh: memory AND the active view", async () => {
    const prevFetch = globalThis.fetch;
    const calls: string[] = [];
    globalThis.fetch = (async (path: string) => {
      calls.push(String(path));
      return { ok: true, json: async () => ({ gatewayMb: 42, heapUsedMb: 1, heapTotalMb: 2, externalMb: 3, processCount: 1 }) };
    }) as unknown as typeof fetch;
    try {
      refreshPageMock.mockClear();
      polling.refreshNow();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(calls).toEqual(["/api/memory?tree=1"]); // the child walk rides on r too
      expect(refreshPageMock).toHaveBeenCalledTimes(1);
    } finally {
      globalThis.fetch = prevFetch;
    }
  });
});

/* Focus mode gives the current page the rail's width while the minimal shell bar stays
   above it (body.immersive + base.css), then dispatches resize so every view fits itself.
   The browser's own fullscreen is deliberately NEVER requested: F11 is the user's keypress,
   the app layout is ours — the stub below pins that it stays uncalled. */
describe("focus mode - the page gains navigation space", () => {
  interface FakeBtn { title: string; innerHTML: string; onclick: unknown; setAttribute: (k: string, v: string) => void; }
  const els = new Map<string, FakeBtn>();
  let classes: Set<string>;
  let fullscreenCalls: string[];
  let resizeCount: number;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let immersive: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let fakeDocument: any;

  beforeAll(async () => {
    const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
    const prevWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
    classes = new Set();
    fullscreenCalls = [];
    resizeCount = 0;
    fakeDocument = {
      getElementById: (id: string) => {
        let e = els.get(id);
        if (!e) { e = { title: "", innerHTML: "", onclick: null, setAttribute() {} }; els.set(id, e); }
        return e;
      },
      querySelector: () => null,
      querySelectorAll: () => [],
      addEventListener: () => {},
      removeEventListener: () => {},
      documentElement: {
        // Counting on purpose: the module must NEVER call it — F11 is the user's, not ours.
        requestFullscreen: () => { fullscreenCalls.push("enter"); return Promise.resolve(); },
      },
      body: {
        classList: {
          contains: (c: string) => classes.has(c),
          add: (c: string) => void classes.add(c),
          remove: (c: string) => void classes.delete(c),
          toggle: (c: string, on?: boolean) => { if (on === undefined) { if (classes.has(c)) classes.delete(c); else classes.add(c); } else if (on) classes.add(c); else classes.delete(c); },
        },
      },
    };
    Object.assign(globalThis, {
      document: fakeDocument,
      window: { addEventListener: () => {}, removeEventListener: () => {}, dispatchEvent: () => { resizeCount++; return true; } },
    });
    afterAll(() => {
      if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
      else delete (globalThis as Record<string, unknown>).document;
      if (prevWindow) Object.defineProperty(globalThis, "window", prevWindow);
      else delete (globalThis as Record<string, unknown>).window;
    });
    immersive = await import("../src/immersive.js");
  });

  // Each test is self-contained: the mode is shell state, so every case resets it.
  beforeEach(() => {
    classes.clear();
    fullscreenCalls.length = 0;
    resizeCount = 0;
  });

  it("init wires the control and paints the resting state", () => {
    immersive.initImmersive();
    const btn = els.get("expandBtn")!;
    expect(typeof btn.onclick).toBe("function");
    expect(btn.innerHTML).toContain("#i-expand");
    expect(btn.title).toContain("Focus mode");
  });

  it("toggle: the class flips, the icon flips, views get one resize - and no document fullscreen is asked", () => {
    immersive.toggleImmersive();
    expect(immersive.immersiveOn()).toBe(true);
    expect(resizeCount).toBe(1);
    const btn = els.get("expandBtn")!;
    expect(btn.innerHTML).toContain("#i-collapse");
    expect(btn.title).toContain("Exit");
    expect(fullscreenCalls).toEqual([]); // F11 is the user's keypress; the page is ours
  });

  it("toggle back: the class returns and the views resize again", () => {
    immersive.toggleImmersive();
    immersive.toggleImmersive();
    expect(immersive.immersiveOn()).toBe(false);
    expect(resizeCount).toBe(2);
    expect(els.get("expandBtn")!.innerHTML).toContain("#i-expand");
    expect(fullscreenCalls).toEqual([]); // leaving is as quiet as entering
  });

  it("exitImmersive is a no-op when the panel is not immersive", () => {
    immersive.exitImmersive();
    expect(immersive.immersiveOn()).toBe(false);
    expect(resizeCount).toBe(0);
  });
});

/* Focus mode folds the plugin rail, but the shell's 40px app bar remains in flow. The
   page body must never share pixels with shell controls: the old fixed corner escape covered
   page-owned actions at the 900px support floor. Location and readouts fold inside the bar;
   theme and exit keep their normal far-right slots. */
describe("fullscreen - the CSS contract (e2e over the shipped sheet)", () => {
  const base = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "styles", "base.css"), "utf8");

  it("ordinary pages keep an in-flow bar, while a docked workspace gives it no empty row", () => {
    expect(base).toMatch(/body.immersive .rail {[^}]*visibility: hidden/);
    expect(base).not.toMatch(/body.immersive .rail {[^}]*display: none/);
    expect(base).toMatch(/body.immersive .ctxbar {[^}]*height: 40px/);
    expect(base).not.toMatch(/body.immersive .ctxbar {[^}]*(?:visibility: hidden|height: 0|position: fixed)/);
    expect(base).toMatch(/body.immersive.immersive-docked .ctxbar {[^}]*height: 0/);
  });

  it("the minimal bar folds page context and passive readouts", () => {
    expect(base).toMatch(/body.immersive #pageBtn,/);
    expect(base).toMatch(/body.immersive #pageLoc,/);
    expect(base).toMatch(/body.immersive #countChip,/);
    expect(base).toMatch(/body.immersive #memChip {[^}]*display: none/);
  });

  it("the page's own surfaces never fold: no immersive rule hides the sidebar", () => {
    expect(base).not.toMatch(/body.immersive[^{]*.sidebar[^{]*{[^}]*display: none/);
    expect(base).not.toMatch(/body.immersive[^{]*.sidebar[^{]*{[^}]*visibility: hidden/);
  });

  it("theme and exit stay in layout flow instead of floating over page actions", () => {
    expect(base).not.toMatch(/body.immersive #(?:expandBtn|themeBtn) {[^}]*position: fixed/);
    expect(base).not.toMatch(/body.immersive #(?:expandBtn|themeBtn) {[^}]*(?:top:|right:|z-index:)/);
  });

  it("Terminal exposes one shell dock and immersive moves the app zone there", () => {
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
    const terminal = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "views", "terminal.ts"), "utf8");
    const immersive = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "immersive.ts"), "utf8");
    expect(shell).toContain('id="appZone"');
    expect(terminal).toContain('data-shell-focus-slot');
    expect(immersive).toContain('"immersive-docked"');
    expect(immersive).toContain("MutationObserver");
  });
});

/* Focus mode is ONE shell-owned control (docs/13 D5, as revised). The entry lives at the
   context bar's far right in normal mode and remains there when the bar becomes minimal;
   no page mounts a fullscreen control of its own — the terminal's old .imm-toggle was
   exactly that debt. Pinned here as source contracts over the shipped shell and modules,
   next to the behavioral suite above. */
describe("focus mode - one shell-owned control", () => {
  // Source contracts read the TypeScript sources (docs/36 D2); panel-emit.test.ts proves the
  // emitted tree matches them, so what is asserted here is what ships. The shell stays an
  // admin_assets read - index.html is still edited in place, never emitted.
  const read = (rel: string) => readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", rel), "utf8");

  it("the app trio owns the bar's far right: count chip left, then mem, theme, focus", () => {
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
    const rail = shell.slice(shell.indexOf('class="rail"'), shell.indexOf('class="workbench"'));
    const ctx = shell.slice(shell.indexOf('class="ctxbar"'), shell.indexOf('class="shell"'));
    // The rail is pure navigation now — no app controls, no rail foot.
    expect(rail).not.toContain('id="expandBtn"');
    expect(rail).not.toContain('id="themeBtn"');
    expect(rail).not.toContain("rail-foot");
    // The reserved app zone is a fixed trio in a fixed order, right of every page chip.
    expect(ctx).toContain('id="expandBtn"');
    expect(ctx).toContain('id="themeBtn"');
    expect(ctx.indexOf('id="countChip"')).toBeLessThan(ctx.indexOf('id="memChip"'));
    expect(ctx.indexOf('id="memChip"')).toBeLessThan(ctx.indexOf('id="themeBtn"'));
    expect(ctx.indexOf('id="themeBtn"')).toBeLessThan(ctx.indexOf('id="expandBtn"'));
  });

  it("the terminal has no page-local fullscreen control anymore", () => {
    const terminal = read("views/terminal.ts");
    expect(terminal).not.toContain("imm-toggle");
    expect(terminal).not.toContain("term-full");
    expect(terminal).not.toContain("toggleImmersive");
  });

  it("immersive.js paints only the shell-owned control - no page-button repaint loop", () => {
    const immersive = read("immersive.ts");
    expect(immersive).not.toContain(".imm-toggle");
    expect(immersive).not.toContain("querySelectorAll");
  });

  it("no document Fullscreen API is requested anywhere in the panel tree", () => {
    for (const rel of ["immersive.ts", "main.ts", "views/terminal.ts"]) {
      expect(read(rel), rel).not.toContain("requestFullscreen");
    }
  });

  it("the bar is never hidden while immersive - its in-flow exit must survive every paint", () => {
    // paintPluginContext owns #ctxBar's hidden flag; #expandBtn lives inside the minimal bar.
    // A hidden parent would strand the exit with Esc as the only way out, so the guard stays.
    const registry = read("page-registry.ts");
    expect(registry).toContain('contains("immersive")');
    expect(registry).toMatch(/bar.hidden = !show && !immersive/);
  });
});

/* Body-chrome dedup (docs/13 D5, as revised): the context bar says where the user is, so
   single-page plugin bodies stop repeating the location as an h1. The workflow description
   stays. Pinned as source contracts over the shipped modules (Tokens is pinned
   behaviorally in admin-tokens-view.test.ts; the tunnels body in admin-tunnels-pages). */
describe("body chrome dedup - no repeated location titles", () => {
  const read = (rel: string) => readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", rel), "utf8");

  it("Jobs, Plugins and Secrets describe the workflow, not the location", () => {
    for (const rel of ["jobs.ts", "views/plugins.ts", "views/secrets.ts"]) {
      const src = read(rel);
      expect(src, rel).not.toContain("pane-title");
      expect(src, rel).toContain("pane-desc");
    }
  });
});
