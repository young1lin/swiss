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

// @vitest-environment happy-dom

/* Settings / System: the destructive process action, deliberately NOT permanent chrome.
   The sheet is the whole point - quitting the toolbox must be an explicit, visible
   confirmation - and the late-response case is the regression this page owns: a slow
   POST /api/shutdown must not paint its stopping state over a page the user navigated
   to while the sheet was open.

   REWRITTEN FOR R5 (docs/37 §7): the view used to build its body and sheet as strings
   against a hand-rolled element stub with a string innerHTML, and every assertion was a
   substring test against markup nobody parsed. Now the suite runs on a real DOM
   (happy-dom, per file) and CLICKS the real buttons through the delegated pane listener
   - the wiring the old suite probed by reading .onclick off a stub is now exercised by
   the event path the browser actually takes. The late-response case drives the REAL
   ui-state view switch instead of a mock. */

import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { setCurrentView } from "../src/ui-state.js";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view's public surface
   is asserted by name below, so a typed import would be circular. */
let view: any;
let shutdownReply: unknown = null;
let shutdownCalls: { method: string; url: string }[] = [];

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;

/* The shell skeleton, taken from the SERVED index.html rather than invented here (the same
 * rule as the other view suites): #sheet and #pane must exist before the import. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

beforeEach(async () => {
  shutdownReply = null;
  shutdownCalls = [];
  document.body.innerHTML = shellSkeleton();
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      shutdownCalls.push({ method: init?.method ?? "GET", url: String(url) });
      const ok = shutdownReply != null;
      return Promise.resolve({ status: ok ? 200 : 500, ok, json: () => Promise.resolve(shutdownReply ?? { error: "no" }) } as Response);
    },
  });
  setCurrentView("system");
  view = await import("../src/views/system.js");
});

describe("Settings / System", () => {
  it("places the low-frequency destructive action in a dedicated Runtime section", async () => {
    await view.mount();
    expect($("pane").textContent).toContain("Control this running swiss process");
    const cap = $("pane").querySelector(".sec-cap");
    expect(cap?.textContent).toBe("Runtime");
    const quit = $("system-quit");
    expect(quit.className).toBe("btn danger");
    expect(quit.textContent).toBe("Quit swiss");
  });

  it("opens a visible, explicit confirmation sheet before stopping anything", async () => {
    await view.mount();
    $("system-quit").click(); // through the pane's delegated listener
    expect(shutdownCalls.length).toBe(0);
    const sheet = $("sheet");
    expect(sheet.hidden).toBe(false);
    const dialog = sheet.querySelector(".sheet");
    expect(dialog?.getAttribute("role")).toBe("dialog");
    expect(dialog?.getAttribute("aria-modal")).toBe("true");
    expect(sheet.textContent).toContain("disconnects every MCP client");
    expect(sheet.querySelector("#quit-cancel")).not.toBeNull();
    expect(sheet.querySelector("#quit-confirm")).not.toBeNull();
    $("quit-cancel").click();
    expect(sheet.hidden).toBe(true);
    expect(shutdownCalls.length).toBe(0);
  });

  it("the sheet's backdrop click closes without stopping anything", async () => {
    await view.mount();
    $("system-quit").click();
    const sheet = $("sheet");
    // A click that lands on the backdrop (target IS the host) closes; the delegated
    // listener reads target, so dispatch one whose target is the sheet host itself.
    sheet.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(sheet.hidden).toBe(true);
    expect(shutdownCalls.length).toBe(0);
  });

  it("restores the confirmation when the shutdown request is refused", async () => {
    await view.mount();
    $("system-quit").click();
    const before = $("pane").innerHTML;
    shutdownReply = null; // the POST fails; apiJson returns null after toasting
    await view.requestQuit();
    const button = $("quit-confirm") as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    expect(button.textContent).toBe("Quit swiss");
    expect($("sheet").hidden).toBe(false); // still open - the work is not done
    expect($("pane").innerHTML).toBe(before);
  });

  it("POSTs the existing graceful shutdown action and leaves a final page state", async () => {
    await view.mount();
    shutdownReply = { ok: true, stopping: true };
    await view.requestQuit();
    expect(shutdownCalls).toEqual([{ method: "POST", url: "/api/shutdown" }]);
    expect($("sheet").hidden).toBe(true);
    expect($("pane").querySelector(".empty h2")?.textContent).toBe("swiss is stopping");
    expect($("pane").textContent).toContain("close this tab");
  });

  it("does not overwrite a different page after a late shutdown response", async () => {
    await view.mount();
    const before = $("pane").innerHTML;
    setCurrentView("plugins"); // the user navigated while the sheet was open
    shutdownReply = { ok: true, stopping: true };
    await view.requestQuit();
    expect($("sheet").hidden).toBe(true); // the sheet closes either way
    expect($("pane").innerHTML).toBe(before); // but the pane is not ours to paint
  });
});
