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

/* SPEC §panel.scale - the interface size. One factor, --ui-scale, multiplies every length the
   stylesheets write from 3px up; the step is per browser and reaches <html data-scale> before the
   first paint. The step list lives in three places (ui/ui-scale.ts, base.css's blocks, the two
   pages' pre-paint scripts), so the copies are checked against each other here, and the sheets
   are held to the rule so a new hard-coded length cannot stay the one thing that does not grow. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { SCALE_KEY, UI_SCALES, applyScale, scalePref, scaleStep, scaled, scaledCss, setScale, uiScale } from "../src/ui/ui-scale.js";
import { parseCss } from "./css-rules.js";
import { assetsDir, sheet } from "./styles.js";
import type { SheetName } from "./styles.js";

const SHEETS: SheetName[] = ["base.css", "ui.css", "views.css"];

describe("SPEC §panel.scale - the step and the factor", () => {
  afterEach(() => { localStorage.clear(); document.documentElement.removeAttribute("data-scale"); });

  it("a stored or received value is one of the steps, anything else 100%", () => {
    expect(UI_SCALES).toEqual(["90", "100", "110", "125", "150", "175"]);
    for (const s of UI_SCALES) expect(scaleStep(s)).toBe(s);
    for (const bad of [null, undefined, "", "0", "200", "1.25", "125%", " 125"]) expect(scaleStep(bad), String(bad)).toBe("100");
  });

  it("the factor is the attribute on <html>; a length at 100% meets a measurement at the size in force", () => {
    expect(uiScale(), "no attribute is 100%").toBe(1);
    expect(scaled(280)).toBe(280);
    document.documentElement.setAttribute("data-scale", "125");
    expect(uiScale()).toBe(1.25);
    expect(scaled(280)).toBe(350);
    document.documentElement.setAttribute("data-scale", "bogus");
    expect(uiScale(), "an attribute nobody set by the picker is 100%").toBe(1);
    expect(scaledCss(160)).toBe("calc(160px * var(--ui-scale))");
  });

  it("a pick is stored and applied, 100% clears both, and the pages that measure are told", () => {
    let resizes = 0;
    const onResize = (): void => { resizes++; };
    window.addEventListener("resize", onResize);
    try {
      setScale("150");
      expect(localStorage.getItem(SCALE_KEY)).toBe("150");
      expect(document.documentElement.getAttribute("data-scale")).toBe("150");
      expect(scalePref()).toBe("150");
      expect(resizes).toBe(1);
      setScale("100");
      expect(localStorage.getItem(SCALE_KEY), "the default is not stored").toBeNull();
      expect(document.documentElement.hasAttribute("data-scale")).toBe(false);
      expect(scalePref()).toBe("100");
      setScale("bogus");
      expect(localStorage.getItem(SCALE_KEY)).toBeNull();
      expect(document.documentElement.hasAttribute("data-scale")).toBe(false);
      applyScale("90");
      expect(document.documentElement.getAttribute("data-scale")).toBe("90");
      expect(localStorage.getItem(SCALE_KEY), "applying a received step is the caller's to store").toBeNull();
      expect(resizes).toBe(4);
    } finally { window.removeEventListener("resize", onResize); }
  });
});

describe("SPEC §panel.scale - the copies agree", () => {
  /** Run a page's inline pre-paint script against stored preferences; return what it put on <html>. */
  function prePaint(page: string, stored: Record<string, string>, search = ""): Record<string, string> {
    const html = readFileSync(join(assetsDir, page), "utf8");
    const script = /<script>([\s\S]*?)<\/script>/.exec(html)![1];
    const attrs: Record<string, string> = {};
    const doc = { documentElement: { setAttribute: (k: string, v: string) => { attrs[k] = v; }, lang: "en" } };
    const storage = { getItem: (k: string) => stored[k] ?? null };
    const mm = () => ({ matches: false });
    new Function("document", "localStorage", "location", "window", "matchMedia", "URLSearchParams", script)(
      doc, storage, { search }, { matchMedia: mm }, mm, URLSearchParams);
    return attrs;
  }

  it("both pre-paint scripts put every step but 100 on <html>, and nothing for 100 or a stray value", () => {
    for (const page of ["index.html", "ui.html"]) {
      for (const pref of [...UI_SCALES, "200", "bogus", ""]) {
        const want = scaleStep(pref);
        expect(prePaint(page, { swiss_scale: pref })["data-scale"], page + " " + pref).toBe(want === "100" ? undefined : want);
      }
      expect(prePaint(page, {})["data-scale"], page + " nothing stored").toBeUndefined();
    }
    // The gallery's ?scale= wins over the panel's own preference, as its ?code= does.
    expect(prePaint("ui.html", { swiss_scale: "110" }, "?scale=150")["data-scale"]).toBe("150");
    expect(prePaint("ui.html", { swiss_scale: "110" }, "?scale=bogus")["data-scale"], "a stray ?scale= reads as 100%").toBeUndefined();
  });

  it("base.css sets the factor for every step but 100, and 1 at :root", () => {
    const rules = parseCss(sheet("base.css"));
    const factor = (sel: string): string | undefined => rules.find((r) => r.selectors.includes(sel))?.decls.find((d) => d.prop === "--ui-scale")?.value;
    expect(factor(":root")).toBe("1");
    for (const s of UI_SCALES.filter((x) => x !== "100")) {
      expect(Number(factor(':root[data-scale="' + s + '"]')), s).toBe(Number(s) / 100);
    }
    const root = rules.find((r) => r.selectors.includes(":root") && r.at === "")!;
    expect(root.decls.find((d) => d.prop === "font-size")?.value, "rem follows the factor").toBe("calc(16px * var(--ui-scale))");
  });
});

describe("SPEC §panel.scale - the sheets grow as one", () => {
  // Lengths that are not sizes to grow: a pill's radius is "rounder than anything".
  const FIXED = new Set(["--r-pill"]);
  const PX = /(?<![\w.#-])(-?\d*\.?\d+)px\b/g;
  const SCALED = /calc\(-?\d*\.?\d+px \* var\(--ui-scale\)\)/g;

  it("every length from 3px up in a declaration is written calc(Npx * var(--ui-scale)); hairlines stay", () => {
    const loose: string[] = [];
    for (const name of SHEETS) {
      for (const r of parseCss(sheet(name))) {
        for (const d of r.decls) {
          if (FIXED.has(d.prop)) continue;
          const bare = d.value.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, "").replace(SCALED, "");
          for (const m of bare.matchAll(PX)) {
            if (Math.abs(parseFloat(m[1])) >= 3) loose.push(name + " " + r.selectors.join(", ") + " { " + d.prop + ": " + d.value + " }");
          }
        }
      }
    }
    expect(loose).toEqual([]);
  });

  it("no width rule is a px media query: a container query in rem moves with the factor", () => {
    for (const name of SHEETS) {
      const preludes = Array.from(sheet(name).matchAll(/@(media|container)[^{]*/g)).map((m) => m[0]);
      for (const p of preludes) expect(p, name).not.toMatch(/(min|max)-(width|height)\s*:\s*\d+px/);
    }
  });
});
