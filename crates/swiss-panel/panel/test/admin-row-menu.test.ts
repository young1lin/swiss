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

import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* Integration: the row overflow menus (SPEC §panel.design) against the document-level click closer in
   connect.js. Regression (2026-09-12): the Jobs/Tunnels row ellipsis wired popupMenu WITHOUT
   stopping propagation, so the very click that opened the menu bubbled on to document, where
   connect.js's closer saw the menu latch and tore the menu down — a click, no menu, no error.
   The pane's own overflow button and the tunnel group-head menus never had this because they
   call ev.stopPropagation() first.

   The suite drives the real modules (wireJobs / wireTunnels -> ui/menu popupMenu -> closeMenu
   -> connect.js's document closer). The bug only exists where clicks bubble, so the DOM must
   bubble them: happy-dom does (SPEC §panel.pages; this suite used to carry its own micro-DOM with a
   hand-written bubbling loop, from before the repo had a DOM library). */

const here = dirname(fileURLToPath(import.meta.url));
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mods: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let tunState: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let jobState: any;

const JOB_RUNS_URL = "/api/jobs/env-check/runs";

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  Object.assign(globalThis, {
    confirm: () => true,
    fetch: async (url: string) => ({
      ok: true,
      status: 200,
      json: async () => {
        if (String(url).includes(JOB_RUNS_URL)) return { runs: [], nextBefore: null };
        if (String(url).startsWith("/api/jobs")) return { jobs: [] };
        return {};
      },
    }),
  });
  // connect.js installs the document-level click closer — the code the bug lived under.
  await import("../src/connect.js");
  mods = {
    jobs: await import("../src/jobs.js"),
    tunnels: await import("../src/tunnels.js"),
  };
  tunState = await import("../src/tunnel-state.js");
  jobState = await import("../src/job-state.js");
});

beforeEach(() => {
  document.getElementById("menu")?.remove();
  const sheet = document.getElementById("sheet")!;
  sheet.hidden = true;
  sheet.textContent = "";
});

describe("row overflow menus vs the document click closer (SPEC §panel.design)", () => {
  function row(attr: string, id: string, act: string): HTMLElement {
    const r = document.createElement("div");
    r.setAttribute(attr, id);
    for (const a of [act, "data-more"]) {
      const b = document.createElement("button");
      b.setAttribute(a, "");
      b.className = "btn ghost icon";
      r.appendChild(b);
    }
    return r;
  }
  function pane(...rows: HTMLElement[]): void {
    document.getElementById("pane")!.replaceChildren(...rows);
  }
  /** A real click on the ⋯: it bubbles through the row, #pane and on to document. */
  function openMenuOf(r: HTMLElement): HTMLElement {
    r.querySelector<HTMLButtonElement>("[data-more]")!.click();
    const menu = document.getElementById("menu");
    expect(menu, "the menu survived its own click").not.toBeNull();
    return menu!;
  }
  const labelsOf = (menu: HTMLElement): string[] => Array.from(menu.querySelectorAll("button")).map((b) => b.textContent ?? "");

  /** One Jobs row against the real row contract, wired by the real wireJobs. */
  function wiredJobRow(): HTMLElement {
    const r = row("data-job", "env-check", "data-run");
    pane(r);
    // jobByName() reads the jobs domain rows — the payload loadJobs stores.
    jobState.setJobRows([{ name: "env-check", cron: "5 * * * *", enabled: true, editableInV1: true }]);
    jobState.clearJobBusy("env-check");
    mods.jobs.wireJobs();
    return r;
  }

  it("a Jobs row ellipsis opens its menu even though the click bubbles to document", () => {
    // THE regression: without stopPropagation the same click that opens the menu reaches
    // connect.js's closer, which removes it before the browser even paints.
    const menu = openMenuOf(wiredJobRow());
    expect(labelsOf(menu)).toEqual(["Edit", "History", "Delete"]);
    expect(Array.from(menu.querySelectorAll("button.danger")).map((b) => b.textContent)).toEqual(["Delete"]);
  });

  it("an outside click still closes an open menu (the closer itself must keep working)", () => {
    openMenuOf(wiredJobRow());
    document.body.click();
    expect(document.getElementById("menu")).toBeNull();
  });

  it("History from the menu opens the runs sheet for that job", async () => {
    const menu = openMenuOf(wiredJobRow());
    Array.from(menu.querySelectorAll("button")).find((b) => b.textContent === "History")!.click();
    await new Promise((r) => { setTimeout(r, 0); });
    const sheet = document.getElementById("sheet")!;
    expect(sheet.hidden).toBe(false);
    expect(sheet.textContent).toContain("Runs — env-check");
    // The menu closed behind the action.
    expect(document.getElementById("menu")).toBeNull();
  });

  it("a Tunnels rule ellipsis opens its menu; Force free appears only while a port is held", () => {
    const held = row("data-rule", "r1", "data-act");
    const free = row("data-rule", "r2", "data-act");
    pane(held, free);
    // tunData() reads the tunnel domain payload — the payload loadTunnels stores.
    tunState.setMountedTunScope("rules");
    tunState.setTunResponse({
      connections: [],
      rules: [
        { id: "r1", localPort: 50383, portOwner: { pid: 12188 }, host: "a", state: "down" },
        { id: "r2", localPort: 50400, host: "b", state: "down" },
      ],
      ruleGroups: [],
      connGroups: [],
      mcps: [],
    });
    mods.tunnels.wireTunnels();

    expect(labelsOf(openMenuOf(held))).toEqual(["Edit", "Copy local port", "Force free 50383", "Delete"]);
    document.body.click(); // close before opening the next
    expect(labelsOf(openMenuOf(free))).toEqual(["Edit", "Copy local port", "Delete"]);
  });

  it("a Tunnels connection ellipsis opens its menu", () => {
    const conn = row("data-conn", "c1", "data-test");
    pane(conn);
    tunState.setMountedTunScope("conns");
    tunState.setTunResponse({
      connections: [{ id: "c1", host: "192.168.1.24", port: 22 }],
      rules: [],
      ruleGroups: [],
      connGroups: [],
      mcps: [],
    });
    mods.tunnels.wireTunnels();
    expect(labelsOf(openMenuOf(conn))).toEqual(["Edit", "Copy host", "Delete"]);
  });
});
