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

/* The Remote Runs search (SPEC §remote.panel, §host.history), 2026-10-12. The owner, of the page
   it replaced: "怎么搜索是这么设计的，哪个搜索放到最下面的。而且不要隐藏什么搜索框" - the search was a
   toggle in the head that opened a second box UNDER the list and the pager, and answered with
   "Run #42 · byte 70000" links. Now the box leads the head and is always there; a needle
   searches what the recorded runs printed, and the results take the list's place as the list's
   own rows, newest first, each with the line that matched. The literal scan's honest states
   stay: an unfinished scan that found nothing is never "no matches". */

import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view is driven by its
   page-registry surface (mount / refresh / poll / unmount), so a typed import would add nothing. */
let view: any;
type Reply = unknown | ((body: string) => unknown);
let replies: Record<string, Reply> = {};
let requests: { path: string; method: string; body: string }[] = [];

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;
const settle = async (): Promise<void> => { for (let i = 0; i < 3; i++) await new Promise((r) => setTimeout(r, 0)); };
/** Turn the event loop until `ok` holds - a scan round is a chain of awaited requests. */
async function until(ok: () => boolean, turns = 400): Promise<void> {
  for (let i = 0; i < turns && !ok(); i++) await new Promise((r) => setTimeout(r, 0));
}

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/** A reply by exact path, then by a key ending in * that the path starts with (the body route's
 *  byte window is the view's arithmetic, not this suite's). */
function replyFor(method: string, path: string): Reply {
  const exact = replies[(method !== "GET" ? method + " " : "") + path] ?? replies[path];
  if (exact !== undefined) return exact;
  const key = Object.keys(replies).find((k) => k.endsWith("*") && path.startsWith(k.slice(0, -1)));
  return key ? replies[key] : undefined;
}

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      const path = String(url);
      const method = init?.method || "GET";
      const body = typeof init?.body === "string" ? init.body : "";
      requests.push({ path, method, body });
      const r = replyFor(method, path);
      const reply = typeof r === "function" ? (r as (b: string) => unknown)(body) : r;
      const ok = reply !== undefined;
      return Promise.resolve({ status: ok ? 200 : 404, ok, json: () => Promise.resolve(reply ?? { error: "no stub for " + path }) } as Response);
    },
    confirm: () => true,
  });
  document.body.innerHTML = shellSkeleton();
  view = await import("../src/views/remote-runs.js");
});

beforeEach(() => {
  requests = [];
  view.unmount();
  document.body.innerHTML = shellSkeleton();
});

const now = new Date().toISOString();
const LIMITS = { maxAgeMs: 30 * 86400000, maxTotalBytes: 500 * 1024 * 1024, maxRuns: 5000, maxOutputBytes: 16 * 1024 * 1024 };
const run = (runId: number, target = "build") => ({
  runId, owner: "remote", label: "exec", action: "remote.exec", state: "succeeded",
  queuedAt: now, startedAt: now, endedAt: now, ms: 900, exitCode: 0,
  meta: { target }, input: { target, argv: ["bash", "-s"], stdinBytes: 40 }, stdinStored: true, outputBytes: 400,
});
const OUTPUT = "● caddy.service - Caddy\n     Loaded: loaded\n     Active: active (running) since Sat 2026-10-10\n";
const AT = OUTPUT.indexOf("running");

/** The page holds run 17; run 12 is older, off the page - the search reads its record back. */
function serve(extra: Record<string, unknown> = {}): void {
  replies = {
    "/api/remote/targets": { targets: [{ id: "build" }, { id: "dev" }], groups: ["default"] },
    "/api/remote/runs?limit=20": { runs: [run(17)], active: [], limits: LIMITS, usage: { bytes: 2048, runs: 40 }, nextBefore: 17, ...extra },
    "/api/remote/runs/12": run(12, "dev"),
    "/api/history/remote/records/12/body*": { text: OUTPUT, next: null, gone: false },
    "/api/history/remote/records/17/body*": { text: OUTPUT, next: null, gone: false },
  };
}

function hits(...ids: number[]): { module: string; runId: number; byteOfs: number }[] {
  return ids.map((runId) => ({ module: "remote", runId, byteOfs: AT }));
}

const box = (): HTMLInputElement => $("rrQ") as HTMLInputElement;
function press(key: string): void {
  box().dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
}
function search(q: string): void {
  box().value = q;
  press("Enter");
}
const scans = (): { path: string; method: string; body: string }[] => requests.filter((r) => r.path === "/api/history/query");
const rowIds = (): string[] => Array.from(document.querySelectorAll<HTMLElement>("#pane .tl-item")).map((n) => n.dataset.rrun || "");
const status = (): string => $("rrSearchStatus")?.textContent || "";
function item(id: number): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>("#pane .tl-item")).find((n) => n.dataset.rrun === String(id)) as HTMLElement;
}

describe("the Remote Runs search box (SPEC §remote.panel)", () => {
  it("Enter searches the recorded output; the results take the list's place - newest first, each with the line that matched", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: hits(12, 17), complete: true, next: {}, skipped: [], errors: [] };
    await view.mount();
    expect($("rrNext"), "the list's pager, before").toBeTruthy();
    search("RUNNING");
    await until(() => status().includes("contains"));
    const sent = JSON.parse(scans()[0].body);
    expect(sent.q).toBe("RUNNING");
    expect(sent.modules).toEqual(["remote"]);
    expect(sent.cursors, "a first batch starts at the beginning").toBeUndefined();
    expect(rowIds()).toEqual(["17", "12"]);
    // The line the match is on, the match as the output wrote it (ASCII case folds).
    const hit = item(12).querySelector(".tl-hit") as HTMLElement;
    expect(hit.textContent).toBe("Active: active (running) since Sat 2026-10-10");
    expect(hit.querySelector("mark")?.textContent).toBe("running");
    expect(status()).toBe("2 runs whose output contains “RUNNING”");
    expect($("rrNext"), "a search is not a page of the record").toBeNull();
    // Run 17 is on the page already; only run 12's record is read back.
    expect(requests.some((r) => r.path === "/api/remote/runs/12")).toBe(true);
    expect(requests.some((r) => r.path === "/api/remote/runs/17")).toBe(false);
  });

  it("a result opens as the list's row does: the meta with its match count, the command, the output", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: [...hits(12), { module: "remote", runId: 12, byteOfs: AT + 40 }], complete: true, next: {} };
    replies["/api/remote/runs/12/output?after=0&max=131072"] = { runId: 12, cursor: 0, nextCursor: OUTPUT.length, output: OUTPUT, total: OUTPUT.length, terminal: true };
    await view.mount();
    search("running");
    await until(() => rowIds().length === 1);
    (item(12).querySelector(".tl-hit") as HTMLElement).click(); // the reason is part of the summary
    await settle();
    expect(item(12).classList.contains("open")).toBe(true);
    expect(item(12).querySelector(".tl-meta")?.textContent).toContain("2 matches");
    expect(item(12).querySelector(".tl-body pre")?.textContent).toBe("bash -s");
    expect(requests.some((r) => r.path === "/api/remote/runs/12/output?after=0&max=131072")).toBe(true);
  });

  it("an unfinished scan that found nothing is never 'no matches': a round stops at its batch budget, Keep searching goes on from the cursor", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: [], complete: false, next: { remote: "cursor-1" }, scannedChunks: 64 };
    await view.mount();
    search("needle");
    await until(() => !!$("rrSearchMore"));
    expect(scans().length, "one round is 32 batches").toBe(32);
    expect(JSON.parse(scans()[1].body).cursors).toEqual({ remote: "cursor-1" });
    expect(status()).toContain("Nothing found yet");
    expect(status()).not.toContain("No recorded output");
    $("rrSearchMore").click();
    await until(() => scans().length === 64 && !!$("rrSearchMore"));
    expect(scans().length).toBe(64);
  });

  it("a complete scan that found nothing says so, and offers nothing to continue", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: [], complete: true, next: {} };
    await view.mount();
    search("zzz");
    await until(() => status().includes("No recorded"));
    expect(status()).toBe("No recorded output contains “zzz”.");
    expect($("rrSearchMore")).toBeNull();
    expect(rowIds()).toEqual([]);
  });

  it("Escape empties the box and the list comes back, pager and all", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: hits(12), complete: true, next: {} };
    await view.mount();
    search("running");
    await until(() => rowIds().length === 1);
    expect(rowIds()).toEqual(["12"]);
    press("Escape");
    expect(box().value).toBe("");
    expect(rowIds()).toEqual(["17"]);
    expect($("rrNext")).toBeTruthy();
    expect($("rrSearchStatus")).toBeNull();
  });

  it("typing searches once the hand stops, not on every key", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: [], complete: true, next: {} };
    await view.mount();
    for (const q of ["c", "ca", "cad"]) {
      box().value = q;
      box().dispatchEvent(new Event("input", { bubbles: true }));
    }
    expect(scans().length).toBe(0);
    await new Promise((r) => setTimeout(r, 360));
    await settle();
    expect(scans().map((r) => JSON.parse(r.body).q)).toEqual(["cad"]);
  });

  it("the target filter narrows the results on the spot", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: hits(12, 17), complete: true, next: {} };
    replies["/api/remote/runs?limit=20&target=dev"] = { runs: [], active: [], limits: LIMITS, usage: { bytes: 2048, runs: 40 } };
    await view.mount();
    search("running");
    await until(() => rowIds().length === 2);
    const sel = $("rrTarget") as HTMLSelectElement;
    sel.value = "dev";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
    expect(rowIds()).toEqual(["12"]);
    expect(status()).toBe("1 run whose output contains “running”");
  });

  it("a found run whose record is gone drops out of the results", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: hits(12, 99), complete: true, next: {} };
    await view.mount();
    search("running");
    await until(() => status().includes("contains"));
    expect(requests.some((r) => r.path === "/api/remote/runs/99")).toBe(true);
    expect(rowIds()).toEqual(["12"]);
    expect($("toast").textContent, "a missing record is not shouted about").toBe("");
  });

  it("a re-read repaints neither the results nor the box someone is typing in", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: hits(12), complete: true, next: {} };
    await view.mount();
    const input = box();
    search("running");
    await until(() => rowIds().length === 1);
    replies["/api/remote/runs?limit=20"] = { runs: [run(30), run(17)], active: [], limits: LIMITS, usage: { bytes: 4096, runs: 41 } };
    // The re-read a notice now drives is the same refresh() path the poll once called
    // (SPEC §remote.panel): the results are the search's, never the record page's.
    await view.refresh();
    expect(rowIds(), "the results are what the search found").toEqual(["12"]);
    expect(box(), "the same input node - focus and caret survive").toBe(input);
    expect($("countChip").textContent).toBe("41 runs");
    expect($("pane").querySelector(".pane-sub")?.textContent).toContain("41 runs recorded");
    // Back to the list: the re-read's newer page is what it shows.
    press("Escape");
    expect(rowIds()).toEqual(["30", "17"]);
  });

  it("a stopped plugin's record is named, not searched", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: [], complete: true, next: {}, skipped: ["remote"] };
    await view.mount();
    search("running");
    await until(() => status().includes("not open"));
    expect(status()).toBe("The run record is not open, so there is nothing to search - start the remote plugin.");
    expect(status()).not.toContain("No recorded output");
    expect($("rrSearchMore")).toBeNull();
  });

  it("a refused scan says why and offers Search again, which starts over without the cursor", async () => {
    serve();
    let n = 0;
    replies["POST /api/history/query"] = () => {
      n++;
      return n === 1 ? { hits: hits(12), complete: false, next: { remote: "c1" } }
        : n === 2 ? { hits: [], complete: false, next: {}, errors: [{ module: "remote", reason: "cursor-expired" }] }
        : { hits: hits(12), complete: true, next: {} };
    };
    await view.mount();
    search("running");
    await until(() => !!$("rrSearchAgain"));
    expect(status()).toBe("The search stopped: cursor-expired");
    expect(rowIds(), "what was found stays").toEqual(["12"]);
    $("rrSearchAgain").click();
    await until(() => status().includes("contains"));
    expect(JSON.parse(scans()[2].body).cursors).toBeUndefined();
    expect(status()).toBe("1 run whose output contains “running”");
  });

  it("leaving the page drops the needle: a return visit opens on the list", async () => {
    serve();
    replies["POST /api/history/query"] = { hits: hits(12), complete: true, next: {} };
    await view.mount();
    search("running");
    await until(() => rowIds().length === 1);
    view.unmount();
    document.body.innerHTML = shellSkeleton();
    await view.mount();
    expect(box().value).toBe("");
    expect(rowIds()).toEqual(["17"]);
  });
});

describe("hitIn - the line a match is on", () => {
  it("the rest of the match's line on either side, whitespace runs as one space", () => {
    expect(view.hitIn("one\n   Active:\tactive (running)  since\nthree", "RUNNING", false))
      .toEqual({ before: "Active: active (", match: "running", after: ") since" });
  });

  it("folds ASCII letters only, as the scan does", () => {
    expect(view.hitIn("Ärger", "är", false)).toBeNull();
    expect(view.hitIn("Ärger", "Är", false)?.match).toBe("Är");
    expect(view.hitIn("nothing here", "zzz", false)).toBeNull();
    expect(view.hitIn("anything", "", false)).toBeNull();
  });

  it("a long front is cut to fit one row, and so is a line the read started inside", () => {
    const long = view.hitIn("x".repeat(100) + "needle tail", "needle", false);
    expect(long.before).toBe("…" + "x".repeat(48));
    expect(long.after).toBe(" tail");
    expect(view.hitIn("abc needle", "needle", true).before).toBe("…abc ");
    expect(view.hitIn("abc needle", "needle", false).before).toBe("abc ");
    expect(view.hitIn("needle " + "y".repeat(300), "needle", false).after).toBe(" " + "y".repeat(159) + "…");
  });
});
