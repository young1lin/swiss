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

/* Token management lives in the MCP group (the toolbar button is gone): the token exists
   FOR MCP clients, so the page that lists, creates, rotates and revokes them sits beside
   Servers and Traffic. This suite pins the view module's own contract: what mount paints,
   the count chip, and — the regression this page must never recommit — that the 6s poll
   patches the GROUPS REGION only, because rebuilding the pane would wipe the label input the
   user may be typing a new token's name into.

   REWRITTEN FOR R5 (docs/37 §7), and the rewrite is the point. The view used to build its
   markup as a string, so this file hand-rolled a fake element whose innerHTML was a string
   too, and every assertion was `expect(paneEl.innerHTML).toContain("…")` — a substring test
   against markup nobody parsed. Now the view builds nodes, so the test runs on a real DOM
   (happy-dom, per-file) and asks the questions it always meant: is the input still there, is
   the option list this long, did the count chip change. The poll canary is the strongest
   example — it used to overwrite innerHTML with the string "CANARY" and check it came back;
   it now types into the actual label input and checks the value survived, which is the
   behaviour the regression was about. */

import { describe, it, expect, beforeAll, beforeEach, afterAll } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

let body: unknown = { tokens: [] };
/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view's public surface is
   asserted by name below (the connect-flow helpers), so a typed import would be circular. */
let view: any;

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;

/* The shell skeleton, taken from the SERVED index.html rather than invented here: several
 * modules in this view's import graph wire a shell control at evaluation time (add-sheet does
 * $("addBtn").onclick = newGroup on its top level), so the ids have to exist before the import
 * or the graph throws. Reading the real file means a shell control that is renamed or dropped
 * shows up as a failure here instead of as a stub that quietly disagrees with the page. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

beforeAll(async () => {
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    fetch: () => Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve(body) }),
    confirm: () => true,
  });
  afterAll(() => {
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  document.body.innerHTML = shellSkeleton(); // before the import: the graph wires on evaluation
  view = await import("../src/views/tokens.js");
});

/* A clean shell per test so one test's pane cannot leak into the next; within a test #pane
 * keeps its identity, which is what the poll case is about. */
beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
});

describe("the Token page (MCP group)", () => {
  it("the view module exists at the entry every page descriptor points at", () => {
    const entry = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "views", "tokens.ts");
    expect(existsSync(entry), entry).toBe(true);
  });

  it("mount paints the list, the create form and the count chip", async () => {
    body = { tokens: [
      { id: "t0", label: "default", createdAt: 1700000000000 },
      { id: "t1", label: "claude-code", createdAt: 1700000001000 },
    ] };
    await view.mount();
    // No location title: the context bar already says "MCP / Token"; the body opens with
    // its workflow description instead (docs/13 D5 rev., body-chrome dedup).
    expect($("pane").querySelector(".pane-desc")).not.toBeNull();
    expect($("pane").querySelector(".pane-title")).toBeNull();
    expect($("pane").textContent).toContain("claude-code");
    expect($("tkLabel")).not.toBeNull();
    // The create form carries the Group select (docs/20 G7) — one option on a bare gateway.
    expect($("tkGroup")).not.toBeNull();
    expect($("tkNewGroup")).not.toBeNull();
    expect($("countChip").textContent).toBe("2 tokens");
  });

  it("each row carries its token id and the actions the delegate dispatches on", async () => {
    body = { tokens: [
      { id: "t0", label: "default", createdAt: 1700000000000 },
      { id: "t1", label: "claude-code", createdAt: 1700000001000 },
    ] };
    await view.mount();
    const rows = $("pane").querySelectorAll("[data-token]");
    expect(rows.length).toBe(2);
    // The overflow menu is on every row; Use is absent on the one copies already use, which
    // with no remembered id is the token labeled `default`.
    expect($("pane").querySelectorAll("[data-tkmore]").length).toBe(2);
    const uses = Array.from($("pane").querySelectorAll("[data-tkuse]")).map((b) => b.getAttribute("data-tkuse"));
    expect(uses).toEqual(["t1"]);
    expect($("pane").querySelector('[data-token="t0"] .tag')?.textContent).toBe("copies use this");
  });

  it("a label the server sent is text, never markup (docs/37 R5)", async () => {
    // The whole reason the row builder moved off string concatenation. Under the old idiom
    // this passed only because someone remembered esc(); now the DOM cannot do otherwise.
    body = { tokens: [{ id: "t0", label: '<img src=x onerror="boom()">', createdAt: 1 }] };
    await view.mount();
    const name = $("pane").querySelector('[data-token="t0"] .name') as HTMLElement;
    expect(name.textContent).toContain('<img src=x onerror="boom()">');
    expect(name.querySelector("img")).toBeNull();
    // Same for the id, which rides in an attribute and in the aria-label.
    expect($("pane").querySelector('[data-tkmore="t0"]')?.getAttribute("aria-label"))
      .toBe('Actions for <img src=x onerror="boom()">');
  });

  it("the once-only secret box appears with its copy actions once a secret exists", async () => {
    body = { tokens: [{ id: "t0", label: "default" }] };
    await view.mount();
    view.setTokenViewSecret("s3cr3t-oneshot");
    await view.refresh(); // an explicit refresh must keep the box alive
    expect($("pane").textContent).toContain("shown only once");
    expect($("tkCopySecret")?.textContent).toBe("Copy secret");
    expect($("tkCopyConn")?.textContent).toBe("Copy connect commands (all MCPs)");
    // The secret is the input's live value, not a value= default that a reset would restore.
    expect(($("tkSecret") as HTMLInputElement).value).toBe("s3cr3t-oneshot");
    expect(($("tkSecret") as HTMLInputElement).readOnly).toBe(true);
    view.setTokenViewSecret(null);
  });

  it("countText pluralizes honestly", async () => {
    body = { tokens: [{ id: "t0", label: "default" }] };
    await view.mount();
    expect(view.countText()).toBe("1 token");
    body = { tokens: [] };
    await view.refresh();
    expect(view.countText()).toBe("0 tokens");
    expect($("pane").querySelector(".empty h2")?.textContent).toBe("No tokens");
  });

  it("poll patches the groups region only - a half-typed label survives it", async () => {
    body = { tokens: [{ id: "t0", label: "default" }] };
    await view.mount();
    // The canary is now the real control, not a string: somebody is mid-word in the label box.
    const label = $("tkLabel") as HTMLInputElement;
    label.value = "claude-co";
    const groupsRegion = $("tkGroups");

    body = { tokens: [{ id: "t0", label: "default" }, { id: "t1", label: "cursor" }] };
    await view.poll();

    expect(($("tkLabel") as HTMLInputElement).value).toBe("claude-co"); // not rebuilt
    expect($("tkLabel")).toBe(label);                                   // the same node, even
    expect($("tkGroups")).toBe(groupsRegion);                           // the host is kept
    expect($("pane").textContent).toContain("cursor");                  // rows moved on
    expect($("countChip").textContent).toBe("2 tokens");

    // An unchanged list must not even touch the groups region.
    const marker = document.createElement("i");
    marker.id = "rowsCanary";
    groupsRegion.appendChild(marker);
    await view.poll();
    expect($("rowsCanary")).toBe(marker);
  });

  it("the Group select keeps the user's pick across a poll, and follows a renamed list", async () => {
    body = { tokens: [{ id: "t0", label: "default" }], groups: ["default", "clients"], tokenGroups: {} };
    await view.mount();
    const sel = $("tkGroup") as HTMLSelectElement;
    expect(Array.from(sel.options).map((o) => o.value)).toEqual(["default", "clients"]);

    sel.value = "clients"; // the hand that just picked a group
    await view.poll();
    expect(($("tkGroup") as HTMLSelectElement).value).toBe("clients");

    // The group is gone from the family: the box must not be left pointing at a dead option,
    // because the next create would assign into a 400 (docs/20 G7).
    body = { tokens: [{ id: "t0", label: "default" }], groups: ["default"], tokenGroups: {} };
    await view.poll();
    const after = $("tkGroup") as HTMLSelectElement;
    expect(Array.from(after.options).map((o) => o.value)).toEqual(["default"]);
    expect(after.value).toBe("default");
  });

  it("still exports the connect-flow helpers connect.js depends on", () => {
    for (const name of ["pickCopyToken", "refreshTokens", "rememberedTokenId"]) {
      expect(typeof view[name], name).toBe("function");
    }
  });
});
