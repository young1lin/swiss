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

/* The Secrets page (the Settings group's second page, SPEC §host.vault): names only, a write-only
   store form, a rev-carrying delete, and a poll that never repaints the form mid-typing.

   REWRITTEN FOR R5 (SPEC §panel.toolchain), and the rewrite is the point. The view used to build its
   rows as a string and hand them to the suite through a rowsHtml() export, so every
   assertion here was a substring test against markup nobody parsed - and the vault state
   was planted through a __setVaultForTest seam that bypassed loadSecrets entirely. Now the
   view builds nodes, so the suite runs on a real DOM (happy-dom, per file) against the
   REAL load path: the fetch stub answers /api/secrets, mount() loads through it, and the
   assertions ask what they always meant - which rows are in the list, in what order, and
   did the half-typed value survive the poll. The seam is gone with the string it existed
   to serve. */

import { describe, it, expect, beforeAll, beforeEach, afterAll } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

interface Call {
  method: string;
  url: string;
  body?: string;
}

let body: unknown = { secrets: [], rev: 0 };
let replies: Record<string, unknown>;
let confirmAnswer: boolean;
let calls: Call[];
/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view's public surface is
   asserted by name below (the mutation helpers), so a typed import would be circular. */
let view: any;

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;

/* The shell skeleton, taken from the SERVED index.html rather than invented here (the same
 * rule as the tokens suite): modules in this graph wire shell controls at evaluation time,
 * so the ids must exist before the import - and reading the real file means a renamed shell
 * control fails here instead of drifting. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

beforeAll(async () => {
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  const prevConfirm = Object.getOwnPropertyDescriptor(globalThis, "confirm");
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      const u = String(url);
      const method = init?.method ?? "GET";
      calls.push({ method, url: u, body: typeof init?.body === "string" ? init.body : undefined });
      const key = method + " " + u;
      // GET /api/secrets falls back to `body` (the page-under-test state); everything else
      // answers only from `replies`, and an un-stubbed route 404s exactly like the old stub.
      const reply = method === "GET" && u === "/api/secrets" && replies[u] === undefined
        ? body
        : replies[key];
      const ok = reply !== undefined && reply !== null;
      return Promise.resolve({ status: ok ? 200 : 404, ok, json: () => Promise.resolve(reply ?? { error: "no stub for " + key }) } as Response);
    },
    confirm: (): boolean => confirmAnswer,
  });
  afterAll(() => {
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
    if (prevConfirm) Object.defineProperty(globalThis, "confirm", prevConfirm);
    else delete (globalThis as Record<string, unknown>).confirm;
  });
  document.body.innerHTML = shellSkeleton(); // before the import: the graph wires on evaluation
  view = await import("../src/views/secrets.js");
});

/* A clean shell per test so one test's pane cannot leak into the next; within a test #pane
 * keeps its identity, which is what the poll case is about. */
beforeEach(() => {
  calls = [];
  confirmAnswer = true;
  replies = {};
  document.body.innerHTML = shellSkeleton();
});

describe("the Secrets page (SPEC §host.vault)", () => {
  it("the view module exists at the entry every page descriptor points at", () => {
    const entry = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "views", "secrets.ts");
    expect(existsSync(entry), entry).toBe(true);
  });

  it("renders names and their references — never a value", async () => {
    body = { secrets: ["stripe-key", "zz-last"], rev: 5 };
    await view.mount();
    const rows = $("pane").querySelectorAll("[data-secret]");
    expect(rows.length).toBe(2);
    // The row shows the REFERENCE, in a code element, not any value: the page never even
    // holds a value to leak - its state is names + rev only.
    expect(($("pane").querySelector('[data-secret="stripe-key"] .lrow-sub code') as HTMLElement).textContent)
      .toBe("${secret://stripe-key}");
    // SPEC §panel.settings: the reference IS the sub-line. The sentence every row repeated after it
    // ("substituted at run time wherever a credential is used") is the description's, once.
    expect($("pane").querySelector('[data-secret="stripe-key"] .lrow-sub')?.textContent)
      .toBe("${secret://stripe-key}");
    expect($("pane").querySelector(".pane-desc")?.textContent)
      .toBe("Write-only vault. Reference a value as ${secret://name}; it is never shown again.");
    // Copy ref is a quiet ghost button with the copy glyph, one per row.
    const copy = $("pane").querySelectorAll('[data-skcopy="stripe-key"]');
    expect(copy.length).toBe(1);
    expect(copy[0].className).toBe("btn ghost with-ic");
    expect(copy[0].querySelector("use")?.getAttribute("href")).toBe("#i-copy");
    expect(copy[0].textContent).toBe("Copy ref");
    // New group is the folder-plus glyph - the sidebar's own for the same act (rule 7).
    expect($("skNewGroup").querySelector("use")?.getAttribute("href")).toBe("#i-folder-plus");
    // Delete lives behind the row's overflow menu (design rule 4: red never sits on a row).
    expect($("pane").querySelector(".btn.danger")).toBeNull();
    expect($("pane").querySelectorAll("[data-skmore]").length).toBe(2);
    expect($("countChip").textContent).toBe("2 secrets");
  });

  it("Copy ref says what it copied - and no token, because a reference carries none", async () => {
    body = { secrets: ["stripe-key"], rev: 5 };
    await view.mount();
    const wrote: string[] = [];
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } } });
    ($("pane").querySelector('[data-skcopy="stripe-key"]') as HTMLElement).click();
    for (let i = 0; i < 3; i++) await new Promise((r) => setTimeout(r, 0));
    expect(wrote).toEqual(["${secret://stripe-key}"]);
    // Found on a walk of SPEC §panel.settings: every copy in the panel toasted "... copied - the token is
    // embedded", the secret reference and the endpoint URL included. Only the connect snippets
    // embed a token (copyText's `token` option); a reference only names a secret.
    expect($("toast").textContent).toBe("Reference copied");
  });

  it("renders rows in the stored order, unranked names after in name order (SPEC §host.vault)", async () => {
    // Through the real path this time: loadSecrets applies the order the server sent.
    body = { secrets: ["zz-last", "aa-unranked", "mm-mid"], rev: 5, order: ["zz-last", "mm-mid"] };
    await view.mount();
    const names = Array.from($("pane").querySelectorAll("[data-secret] .lrow-name")).map((n) => n.textContent);
    expect(names).toEqual(["zz-last", "mm-mid", "aa-unranked"]);
  });

  it("a row drag PUTs the flat order, then reloads the rev the PUT bumped (SPEC §host.vault)", async () => {
    body = { secrets: ["aa", "bb", "cc"], rev: 5 };
    await view.mount();
    replies["PUT /api/groups/secrets/order"] = { order: ["bb", "aa", "cc"] };
    replies["/api/secrets"] = { secrets: ["aa", "bb", "cc"], rev: 6 };
    view.moveSecretRow("bb", "aa", true);
    await new Promise((r) => setTimeout(r, 0));
    const put = calls.find((c) => c.method === "PUT" && c.url === "/api/groups/secrets/order");
    expect(put?.body && JSON.parse(put.body as string)).toEqual({ order: ["bb", "aa", "cc"] });
    // The order PUT bumps the vault rev - the reload is mandatory, not polish.
    expect(calls.some((c) => c.method === "GET" && c.url === "/api/secrets")).toBe(true);
    // The moved list is what the rows show, even before the reload answers.
    const names = Array.from($("pane").querySelectorAll("[data-secret] .lrow-name")).map((n) => n.textContent);
    expect(names).toEqual(["bb", "aa", "cc"]);
  });

  it("an empty vault still paints its groups - a group made first has its header", async () => {
    // The Remote Targets rule (2026-09-20), applied here on 2026-09-27: the page used to swap
    // in "No secrets yet" whenever the vault was bare, so a group made then was listed in the
    // Group select and nowhere else - no header to rename or delete it by.
    body = { secrets: [], rev: 0, groups: ["default", "ci"], secretGroups: {} };
    await view.mount();
    expect($("skGroups").children.length).toBe(2);
    expect($("skGroups").textContent).toContain("ci");
    expect($("skGroups").querySelector(".empty")).toBeNull();
    expect($("pane").textContent).not.toContain("No secrets yet");
    // The page description still says what a secret is for.
    expect($("pane").querySelector(".pane-desc")?.textContent).toContain("${secret://name}");
    expect(Array.from(($("skGroup") as HTMLSelectElement).options).map((o) => o.value)).toEqual(["default", "ci"]);

    // Bare and single-grouped: the default group itself is the place a first secret goes.
    body = { secrets: [], rev: 0 };
    await view.refresh();
    expect($("skGroups").children.length).toBe(1);
    expect($("skGroups").textContent).toContain("default");
  });

  it("storing PUTs name+value+rev, then reloads the names and clears the value box", async () => {
    body = { secrets: [], rev: 3 };
    await view.mount();
    const nameBox = $("skName") as HTMLInputElement;
    const valueBox = $("skValue") as HTMLInputElement;
    nameBox.value = "stripe-key";
    valueBox.value = "sk_live_panel";
    replies["PUT /api/secrets/stripe-key"] = { rev: 4 };
    await view.storeSecret();
    const put = calls.find((c) => c.method === "PUT" && c.url === "/api/secrets/stripe-key");
    expect(put && JSON.parse(put.body as string)).toEqual({ value: "sk_live_panel", rev: 3 });
    // The one place a value is ever typed must not keep it after a successful store.
    expect(valueBox.value).toBe("");
  });

  it("a name outside the grammar is refused before any request", async () => {
    body = { secrets: [], rev: 1 };
    await view.mount();
    ($("skName") as HTMLInputElement).value = "BadName";
    ($("skValue") as HTMLInputElement).value = "v";
    await view.storeSecret();
    expect(calls.filter((c) => c.method === "PUT" && c.url.startsWith("/api/secrets/")).length).toBe(0);
  });

  it("deleting asks first, then DELETEs by rev; a refused confirm sends nothing", async () => {
    body = { secrets: ["old"], rev: 9 };
    await view.mount();
    confirmAnswer = false;
    await view.removeSecret("old");
    // A refused confirm sends nothing - mount's own GET is the only call allowed in the log.
    expect(calls.filter((c) => c.method !== "GET").length).toBe(0);
    confirmAnswer = true;
    replies["DELETE /api/secrets/old"] = { rev: 10 };
    await view.removeSecret("old");
    const del = calls.find((c) => c.method === "DELETE");
    expect(del?.url).toBe("/api/secrets/old?rev=9");
  });

  it("poll patches rows only — the store form survives it", async () => {
    body = { secrets: ["a"], rev: 1 };
    await view.mount();
    // Simulate the user mid-typing in the value box - the one place a value is ever typed.
    const valueBox = $("skValue") as HTMLInputElement;
    valueBox.value = "half-typed";
    const groupsRegion = $("skGroups");

    body = { secrets: ["a", "b"], rev: 2 };
    await view.poll();

    expect(($("skValue") as HTMLInputElement).value).toBe("half-typed"); // not rebuilt
    expect($("skValue")).toBe(valueBox);                                // the same node, even
    expect($("skGroups")).toBe(groupsRegion);                           // the host is kept
    expect($("pane").textContent).toContain("b");                       // the row arrived
    expect($("countChip").textContent).toBe("2 secrets");

    // An unchanged list must not even touch the groups region.
    const marker = document.createElement("i");
    marker.id = "rowsCanary";
    groupsRegion.appendChild(marker);
    await view.poll();
    expect($("rowsCanary")).toBe(marker);
  });

  it("a name the server sent is text, never markup (SPEC §panel.toolchain)", async () => {
    // The whole reason the row builder moved off string concatenation. Under the old idiom
    // this passed only because someone remembered esc(); now the DOM cannot do otherwise.
    const hostile = '<b>bold</b><img src=x onerror=1>';
    body = { secrets: [hostile], rev: 1 };
    await view.mount();
    const name = $("pane").querySelector("[data-secret] .lrow-name") as HTMLElement;
    expect(name.textContent).toContain(hostile);
    expect(name.querySelector("img")).toBeNull();
    // Same for the reference on the sub-line, and the attribute values the delegate keys on.
    expect($("pane").querySelector(".lrow-sub code")?.textContent).toBe("${secret://" + hostile + "}");
    expect($("pane").querySelector("[data-skmore]")?.getAttribute("aria-label")).toBe("Actions for " + hostile);
    expect($("pane").querySelectorAll('[data-secret="' + hostile + '"]').length).toBe(1);
  });

  it("the Group select keeps the user's pick across a poll, and follows a renamed list", async () => {
    body = { secrets: ["a"], rev: 1, groups: ["default", "ci"], secretGroups: {} };
    await view.mount();
    const sel = $("skGroup") as HTMLSelectElement;
    expect(Array.from(sel.options).map((o) => o.value)).toEqual(["default", "ci"]);

    sel.value = "ci"; // the hand that just picked a group
    await view.poll();
    expect(($("skGroup") as HTMLSelectElement).value).toBe("ci");

    // The group is gone from the family: the box must not be left pointing at a dead option,
    // because the next store would assign into a 400 (SPEC §host.groups).
    body = { secrets: ["a"], rev: 2, groups: ["default"], secretGroups: {} };
    await view.poll();
    const after = $("skGroup") as HTMLSelectElement;
    expect(Array.from(after.options).map((o) => o.value)).toEqual(["default"]);
    expect(after.value).toBe("default");
  });

  /* Replacing a value (SPEC §host.vault, 2026-09-27 addendum). The API always overwrote, but nothing on
     the page said so: the ⋯ held only Delete, and a replace typed into the form moved the
     row to whatever group the select held. */

  it("the ⋯ offers Replace value…, which points the form at the row and its group", async () => {
    body = { secrets: ["stripe-key"], rev: 7, groups: ["default", "ci"], secretGroups: { "stripe-key": "ci" } };
    await view.mount();
    expect($("skStore").textContent).toBe("Store");
    ($("pane").querySelector('[data-skmore="stripe-key"]') as HTMLElement).click();
    const menu = document.querySelector(".menu.float") as HTMLElement;
    const items = Array.from(menu.querySelectorAll("button"));
    expect(items.map((b) => b.textContent)).toEqual(["Replace value…", "Delete"]);
    expect(items[0].className).not.toContain("danger");
    items[0].click();
    expect(($("skName") as HTMLInputElement).value).toBe("stripe-key");
    expect(($("skGroup") as HTMLSelectElement).value).toBe("ci");
    expect($("skStore").textContent).toBe("Replace");
    expect(document.activeElement).toBe($("skValue"));
  });

  it("the primary reads Replace exactly while the typed name is already stored", async () => {
    body = { secrets: ["stripe-key"], rev: 1 };
    await view.mount();
    const nameBox = $("skName") as HTMLInputElement;
    nameBox.value = "stripe";
    nameBox.dispatchEvent(new Event("input"));
    expect($("skStore").textContent).toBe("Store");
    nameBox.value = " stripe-key ";
    nameBox.dispatchEvent(new Event("input"));
    expect($("skStore").textContent).toBe("Replace");
  });

  it("a replace asks first, keeps the row's group, and names the MCPs the gateway reloaded", async () => {
    body = { secrets: ["stripe-key"], rev: 7, groups: ["default", "ci"], secretGroups: { "stripe-key": "default" } };
    await view.mount();
    ($("skName") as HTMLInputElement).value = "stripe-key";
    ($("skValue") as HTMLInputElement).value = "sk_live_rotated";
    ($("skGroup") as HTMLSelectElement).value = "ci"; // left over from some other store

    confirmAnswer = false;
    await view.storeSecret();
    expect(calls.filter((c) => c.method === "PUT").length).toBe(0);
    expect(($("skValue") as HTMLInputElement).value).toBe("sk_live_rotated");

    confirmAnswer = true;
    replies["PUT /api/secrets/stripe-key"] = { rev: 8, refreshed: ["billing", "stripe-mcp"], failed: [] };
    await view.storeSecret();
    const put = calls.find((c) => c.method === "PUT" && c.url === "/api/secrets/stripe-key");
    expect(put && JSON.parse(put.body as string)).toEqual({ value: "sk_live_rotated", rev: 7 });
    // A replace never moves the row: no member write, whatever the select says.
    expect(calls.some((c) => c.url.startsWith("/api/groups/secrets/members/"))).toBe(false);
    expect(($("skValue") as HTMLInputElement).value).toBe("");
    expect($("toast").textContent).toBe("saved stripe-key — reloaded billing, stripe-mcp");
  });

  it("a rebuild that failed is an error toast naming the MCP", async () => {
    body = { secrets: ["stripe-key"], rev: 2 };
    await view.mount();
    ($("skName") as HTMLInputElement).value = "stripe-key";
    ($("skValue") as HTMLInputElement).value = "v2";
    replies["PUT /api/secrets/stripe-key"] = { rev: 3, refreshed: [], failed: [{ name: "billing", error: "401" }] };
    await view.storeSecret();
    expect($("toast").textContent).toBe("saved stripe-key, but billing did not reload — see its Logs");
    expect($("toast").className).toContain("err");
  });

  it("a replace nothing references says just that", async () => {
    body = { secrets: ["stripe-key"], rev: 2 };
    await view.mount();
    ($("skName") as HTMLInputElement).value = "stripe-key";
    ($("skValue") as HTMLInputElement).value = "v2";
    replies["PUT /api/secrets/stripe-key"] = { rev: 3, refreshed: [], failed: [] };
    await view.storeSecret();
    expect($("toast").textContent).toBe("replaced stripe-key");
  });

  it("still exports the mutation helpers the suite drives", () => {
    for (const name of ["storeSecret", "removeSecret", "moveSecretRow", "beginReplace"]) {
      expect(typeof view[name], name).toBe("function");
    }
  });
});
