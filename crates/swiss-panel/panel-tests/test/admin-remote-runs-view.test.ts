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

import { describe, it, expect, beforeAll, afterAll, beforeEach } from "vitest";
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* The Remote Runs page (#remote-runs, the remote plugin's second page): the run record
   (crates/swiss-remote/src/history.rs) as a Traffic-shaped card of .call rows, live runs
   first, with the output fetched when a row opens. The suite pins the view contract under
   the same hand-rolled DOM the Remote Targets suite uses; document.querySelector answers
   the two selectors the view paints into (a row's body, a live row's <pre>). */

interface FakeEl {
  innerHTML: string;
  textContent: string;
  className: string;
  value: string;
  disabled: boolean;
  onclick: unknown;
  onchange: unknown;
  dataset: Record<string, string>;
  closest: (sel: string) => FakeEl | null;
}

function fakeEl(): FakeEl {
  return { innerHTML: "", textContent: "", className: "", value: "", disabled: false, onclick: null, onchange: null, dataset: {}, closest: () => null };
}

const els = new Map<string, FakeEl>();
const byId = (id: string): FakeEl => {
  let e = els.get(id);
  if (!e) { e = fakeEl(); els.set(id, e); }
  return e;
};
// The nodes the view paints into after mount: keyed by the selector it uses.
const painted = new Map<string, FakeEl>();
const node = (sel: string): FakeEl => {
  let e = painted.get(sel);
  if (!e) { e = fakeEl(); painted.set(sel, e); }
  return e;
};

let bodyByPath: Record<string, unknown> = {};
const requests: { path: string; method: string }[] = [];
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let view: any;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    document: {
      getElementById: byId,
      createElement: () => fakeEl(),
      querySelector: (sel: string) => node(sel),
      querySelectorAll: () => [],
      addEventListener() {},
      removeEventListener() {},
      body: fakeEl(),
      head: fakeEl(),
      visibilityState: "visible",
      activeElement: null,
    },
    fetch: (path: unknown, init: unknown) => {
      const p = String(path);
      requests.push({ path: p, method: (init && (init as { method?: string }).method) || "GET" });
      const body = bodyByPath[p];
      return Promise.resolve({ status: body === undefined ? 404 : 200, ok: body !== undefined, json: async () => body ?? { error: "no such route in this test: " + p } });
    },
    window: { innerWidth: 1440, innerHeight: 900, addEventListener() {} },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    confirm: () => true,
    location: { origin: "http://127.0.0.1:19998" },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  view = await import("../../src/admin_assets/js/views/remote-runs.js");
});

beforeEach(() => {
  requests.length = 0;
  painted.clear();
  view.unmount();
});

/* Fire the pane's delegated click at a stub the selector would have caught. */
function click(dataset: Record<string, string>, id?: string) {
  const stub = fakeEl();
  stub.dataset = dataset;
  stub.closest = (sel: string) => {
    if (sel.startsWith("#")) return id === sel.slice(1) ? stub : null;
    const key = sel.slice(1, -1); // "[data-rtog]" -> "data-rtog"
    const prop = key.replace(/^data-/, "");
    return prop in dataset ? stub : null;
  };
  (byId("pane").onclick as (e: { target: FakeEl }) => void)({ target: stub });
}

const finished = {
  runId: 17, owner: "remote", label: "exec build", action: "remote.exec", state: "failed",
  queuedAt: new Date().toISOString(), startedAt: new Date().toISOString(), endedAt: new Date().toISOString(),
  ms: 12345, exitCode: 2, chars: 24, meta: { target: "build", endpoint: "conn-1" },
  input: { target: "build", argv: ["make", "-j8"], cwd: "src", envKeys: ["CC"] }, outputBytes: 24,
};
const running = {
  runId: 18, owner: "remote", label: "exec build", action: "remote.exec", state: "running",
  queuedAt: new Date().toISOString(), startedAt: new Date().toISOString(),
  input: { target: "build", argv: ["./test.sh"] },
};

function serve(runs: unknown[], active: unknown[], extra: Record<string, unknown> = {}) {
  bodyByPath = {
    "/api/remote/targets": { targets: [{ id: "build", endpoint: "conn-1" }, { id: "dev", endpoint: "conn-1" }], groups: ["default"] },
    "/api/remote/runs?limit=20": {
      runs, active,
      limits: { maxAgeMs: 30 * 86400000, maxTotalBytes: 500 * 1024 * 1024, maxRuns: 5000, maxOutputBytes: 16 * 1024 * 1024 },
      usage: { bytes: 1536, runs: runs.length },
      ...extra,
    },
  };
}

async function settle() {
  // Let the fetch -> json -> repaint chain drain: a macrotask turn, twice, is past every await.
  for (let i = 0; i < 2; i++) await new Promise((r) => setTimeout(r, 0));
}

describe("the Remote Runs page (remote plugin, the run record)", () => {
  it("the view module exists at the entry the page descriptor points at", () => {
    const entry = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "views", "remote-runs.js");
    expect(existsSync(entry), entry).toBe(true);
  });

  it("mount paints the budget line, the live row first, the recorded row with its command and exit, and the chip", async () => {
    serve([finished], [running]);
    await view.mount();
    const pane = byId("pane").innerHTML;
    expect(pane).toContain("1 run recorded");
    expect(pane).toContain("1.5 KB of 500.0 MB");
    expect(pane).toContain("kept 30 days");
    const list = byId("rrList").innerHTML;
    // Live first: the running row precedes the recorded one.
    expect(list.indexOf('data-rrun="18"')).toBeLessThan(list.indexOf('data-rrun="17"'));
    expect(list).toContain("./test.sh");
    expect(list).toContain("#18 · running");
    // The recorded row: argv as a command line, target and cwd, exit code and duration.
    expect(list).toContain("make -j8");
    expect(list).toContain("build · src");
    expect(list).toContain("#17 · exit 2 · 12.3s");
    expect(list).toContain('class="dot down" title="failed"');
    expect(list).toContain('class="dot starting" title="running"');
    // The filter lists the targets table; Clear is enabled once something is recorded.
    expect(pane).toContain('<option value="build">build</option>');
    expect(pane).toContain('id="rrClear">Clear');
    expect(byId("countChip").textContent).toBe("1 run");
    expect(view.countText()).toBe("1 runs");
  });

  it("an empty record draws the empty state and a disabled Clear", async () => {
    serve([], []);
    await view.mount();
    expect(byId("rrList").innerHTML).toContain("No runs yet");
    expect(byId("pane").innerHTML).toContain('id="rrClear" disabled');
    expect(byId("countChip").textContent).toBe("");
  });

  it("opening a recorded row fetches its output from the record and paints it into the body", async () => {
    serve([finished], []);
    bodyByPath["/api/remote/runs/17/output?after=0&max=131072"] = {
      runId: 17, cursor: 0, nextCursor: 24, output: "compiling...\nok\nwarn: x\n", total: 24, truncated: false, terminal: true,
    };
    await view.mount();
    click({ rtog: "17" });
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs/17/output?after=0&max=131072")).toBe(true);
    const body = node('#pane .call[data-rrun="17"] .call-body').innerHTML;
    expect(body).toContain("Output · 24 B");
    expect(body).toContain("compiling...\nok\nwarn: x\n");
    expect(body).not.toContain("Load more");
    expect(node('#pane .call[data-rrun="17"]').className).toBe("call open");
  });

  it("a long output pages through Load more and a capped one shows its tail", async () => {
    const capped = { ...finished, runId: 21, outputBytes: 131072 * 2, outputCapped: true, tail: "the last lines\n" };
    serve([capped], []);
    bodyByPath["/api/remote/runs/21/output?after=0&max=131072"] = { runId: 21, cursor: 0, nextCursor: 131072, output: "head", total: 262144, terminal: true };
    bodyByPath["/api/remote/runs/21/output?after=131072&max=131072"] = { runId: 21, cursor: 131072, nextCursor: 262144, output: "+rest", total: 262144, terminal: true };
    await view.mount();
    click({ rtog: "21" });
    await settle();
    let body = node('#pane .call[data-rrun="21"] .call-body').innerHTML;
    expect(body).toContain('data-rmore="21">Load more');
    expect(body).toContain("128.0 KB of 256.0 KB");
    expect(body).toContain("Output capped at 16.0 MB");
    expect(body).toContain("the last lines");
    click({ rmore: "21" });
    await settle();
    body = node('#pane .call[data-rrun="21"] .call-body').innerHTML;
    expect(body).toContain("head+rest");
    expect(body).not.toContain("Load more");
  });

  it("opening a live row follows /api/runs output and offers Cancel, which posts the cancel", async () => {
    serve([], [running]);
    bodyByPath["/api/runs/18/output?after=0&max=131072"] = { runId: 18, state: "running", cursor: 0, nextCursor: 5, output: "tick\n", truncated: false, terminal: false };
    bodyByPath["/api/runs/18/cancel"] = { runId: 18, state: "canceled" };
    await view.mount();
    click({ rtog: "18" });
    await settle();
    expect(requests.some((r) => r.path === "/api/runs/18/output?after=0&max=131072")).toBe(true);
    expect(node('#pane .call[data-rrun="18"] .call-body').innerHTML).toContain('data-rcancel="18">Cancel');
    expect(node('#pane pre[data-rlivepre="18"]').textContent).toBe("tick\n");
    click({ rcancel: "18" });
    await settle();
    expect(requests.some((r) => r.path === "/api/runs/18/cancel" && r.method === "POST")).toBe(true);
  });

  it("Older asks for the page before the cursor, Newer comes back, and the filter narrows by target", async () => {
    serve([finished], [], { nextBefore: 17 });
    bodyByPath["/api/remote/runs?limit=20&before=17"] = { runs: [{ ...finished, runId: 9 }], active: [], usage: { bytes: 1536, runs: 2 } };
    bodyByPath["/api/remote/runs?limit=20&target=dev"] = { runs: [], active: [], usage: { bytes: 1536, runs: 2 } };
    await view.mount();
    expect(byId("rrList").innerHTML).toContain('id="rrNext">Older');
    click({}, "rrNext");
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs?limit=20&before=17")).toBe(true);
    expect(byId("rrList").innerHTML).toContain('data-rrun="9"');
    expect(byId("rrList").innerHTML).toContain("Page 2");
    click({}, "rrPrev");
    await settle();
    expect(byId("rrList").innerHTML).toContain('data-rrun="17"');
    const sel = byId("rrTarget");
    sel.value = "dev";
    (sel.onchange as () => void)();
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs?limit=20&target=dev")).toBe(true);
    expect(byId("rrList").innerHTML).toContain("No runs on this target yet");
  });

  it("Clear confirms, deletes the record and repaints", async () => {
    serve([finished], []);
    bodyByPath["/api/remote/runs"] = { ok: true };
    await view.mount();
    click({}, "rrClear");
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs" && r.method === "DELETE")).toBe(true);
  });

  it("a poll that changes nothing leaves the pane's nodes alone", async () => {
    serve([finished], []);
    await view.mount();
    byId("pane").innerHTML = "untouched";
    await view.poll();
    expect(byId("pane").innerHTML).toBe("untouched");
    // A new run arriving does repaint.
    serve([{ ...finished, runId: 30 }, finished], []);
    await view.poll();
    expect(byId("pane").innerHTML).not.toBe("untouched");
    expect(byId("rrList").innerHTML).toContain('data-rrun="30"');
  });
});
