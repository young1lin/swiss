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

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import vm from "node:vm";
const { unwrapGlobal } = await import(
  new URL("../../src/admin_assets/js/vendor/xterm/load-classic.js", import.meta.url).href
) as {
  unwrapGlobal: (ns: unknown, name: string) => unknown;
};

/** The vendored xterm bundles run as CLASSIC scripts: the browser hands them a global and
 * they assign their exports onto it. The first real browser run caught the shims assuming
 * window.FitAddon was the class itself - the addon UMDs assign a NAMESPACE object with the
 * class one level down, so every Open died on "window.FitAddon is missing".
 *
 * This file executes the actual vendored bytes under classic-script semantics - a vm
 * context where top-level this / self / globalThis all resolve to the sandbox global,
 * which is exactly what a <script> tag provides - and pins the global shape each shim
 * must unwrap. No browser involved; the contract is checkable right here. */

function runClassic(rel: string): Record<string, unknown> {
  const path = fileURLToPath(new URL("../../src/admin_assets/js/vendor/xterm/" + rel, import.meta.url));
  const source = readFileSync(path, "utf8");
  const sandbox: Record<string, unknown> = {};
  sandbox.self = sandbox; // the addon bundles pass "self" as their global
  // Feature detection runs at EVAL time in these bundles (xterm core and webgl both read
  // navigator); a classic script in a browser always has one, so the vm sandbox does too.
  sandbox.navigator = { userAgent: "node-vendor-shape-test", platform: process.platform, language: "en" };
  // addon-search reaches for queueMicrotask at EVAL time (its async search machinery is
  // built during module evaluation); a classic script in a browser always has one.
  sandbox.queueMicrotask = queueMicrotask;
  sandbox.window = sandbox; // eval-time feature detection reaches for window the same way
  vm.createContext(sandbox); // top-level "this" becomes the context global (unicode11 binds that)
  vm.runInContext(source, sandbox);
  return sandbox;
}

describe("unwrapGlobal", () => {
  it("accepts the namespace shape, the bare-class shape, and neither", () => {
    const cls = function Ctor() { /* classes are functions */ };
    expect(unwrapGlobal({ FitAddon: cls }, "FitAddon")).toBe(cls);
    expect(unwrapGlobal(cls, "FitAddon")).toBe(cls);
    expect(unwrapGlobal({}, "FitAddon")).toBeNull();
    expect(unwrapGlobal(undefined, "FitAddon")).toBeNull();
    expect(unwrapGlobal({ FitAddon: {} }, "FitAddon")).toBeNull();
  });
});

describe("the vendored bundles' real global shapes", () => {
  it("xterm core copies its exports onto the global: Terminal IS a class", () => {
    const g = runClassic("xterm-5.5.0/xterm.js");
    expect(typeof g.Terminal).toBe("function");
    expect(unwrapGlobal(g.Terminal, "Terminal")).toBeTypeOf("function");
  });

  const addons = [
    { dir: "addon-fit-0.10.0/addon-fit.js", global: "FitAddon", cls: "FitAddon" },
    { dir: "addon-unicode11-0.8.0/addon-unicode11.js", global: "Unicode11Addon", cls: "Unicode11Addon" },
    { dir: "addon-web-links-0.11.0/addon-web-links.js", global: "WebLinksAddon", cls: "WebLinksAddon" },
    { dir: "addon-webgl-0.18.0/addon-webgl.js", global: "WebglAddon", cls: "WebglAddon" },
    { dir: "addon-search-0.16.0/addon-search.js", global: "SearchAddon", cls: "SearchAddon" },
  ];
  for (const a of addons) {
    it(a.dir + " assigns a namespace whose " + a.cls + " property unwrapGlobal finds", () => {
      const g = runClassic(a.dir);
      // The namespace object is what lands on the global - an object, NOT the class.
      expect(typeof g[a.global]).toBe("object");
      const unwrapped = unwrapGlobal(g[a.global], a.cls);
      expect(unwrapped).toBeTypeOf("function");
    });
  }

  it("every shim index.js looks up the global its bundle assigns", () => {
    // the loop above pins what each UMD assigns; this pins that each hand-written
    // index.js reads that same name off the window - a typo there fails only in a browser
    for (const a of addons) {
      const dir = a.dir.split("/")[0];
      const shim = readFileSync(
        fileURLToPath(new URL("../../src/admin_assets/js/vendor/xterm/" + dir + "/index.js", import.meta.url)), "utf8",
      );
      expect(shim, dir + "/index.js must reference " + a.global).toContain(a.global);
    }
  });
});

describe("vendored addon-search provenance", () => {
  /** "Byte-identical to npm" was a commit-message claim until the fresh-eyes audit
   *  asked what in-repo evidence enforces it. This pin is that evidence: the SHA-256
   *  of the stock @xterm/addon-search@0.16.0 browser build (verified against
   *  unpkg.com at vendoring time). A different hash means either an upstream
   *  re-release under the same version or an accidental local edit - both need eyes. */
  it("is the stock npm 0.16.0 build, byte for byte", () => {
    const bytes = readFileSync(
      fileURLToPath(new URL("../../src/admin_assets/js/vendor/xterm/addon-search-0.16.0/addon-search.js", import.meta.url)),
    );
    const sha = createHash("sha256").update(bytes).digest("hex").toUpperCase();
    expect(sha).toBe("7BC1B8C7B3549411F6F6F779524C4DED6CA621FD80D64B40A70AE7C78AEFBF55");
  });
});
