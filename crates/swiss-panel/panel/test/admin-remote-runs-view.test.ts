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

/* The Remote Runs page (#remote-runs, the remote plugin's second page): the run record
   (crates/swiss-remote/src/history.rs) as the event list Logs and Traffic are (SPEC §panel.pages) -
   live runs first, the output fetched when a row opens, a run that did not succeed a red tag.
   A real DOM (happy-dom) since P6-2: the timeline, the menu and the pager are the library's
   nodes, read the way a user sees them. */

import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view is driven by its
   page-registry surface (mount / refresh / poll / unmount), so a typed import would add nothing. */
let view: any;
let replies: Record<string, unknown> = {};
let requests: { path: string; method: string }[] = [];

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;
const settle = async (): Promise<void> => { for (let i = 0; i < 3; i++) await new Promise((r) => setTimeout(r, 0)); };

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      const u = String(url);
      const method = init?.method || "GET";
      requests.push({ path: u, method });
      const reply = replies[(method !== "GET" ? method + " " : "") + u] ?? replies[u];
      const ok = reply !== undefined;
      return Promise.resolve({ status: ok ? 200 : 404, ok, json: () => Promise.resolve(reply ?? { error: "no stub for " + u }) } as Response);
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
const finished = {
  runId: 17, owner: "remote", label: "exec build", action: "remote.exec", state: "failed",
  queuedAt: now, startedAt: now, endedAt: now,
  ms: 12345, exitCode: 2, chars: 24, meta: { target: "build", endpoint: "conn-1" },
  input: { target: "build", argv: ["make", "-j8"], cwd: "src", envKeys: ["CC"] }, outputBytes: 24,
};
const running = {
  runId: 18, owner: "remote", label: "exec build", action: "remote.exec", state: "running",
  queuedAt: now, startedAt: now,
  input: { target: "build", argv: ["./test.sh"] },
};
const LIMITS = { maxAgeMs: 30 * 86400000, maxTotalBytes: 500 * 1024 * 1024, maxRuns: 5000, maxOutputBytes: 16 * 1024 * 1024 };

function serve(runs: unknown[], active: unknown[], extra: Record<string, unknown> = {}) {
  replies = {
    "/api/remote/targets": { targets: [{ id: "build", endpoint: "conn-1" }, { id: "dev", endpoint: "conn-1" }], groups: ["default"] },
    "/api/remote/runs?limit=20": { runs, active, limits: LIMITS, usage: { bytes: 1536, runs: runs.length }, ...extra },
  };
}

function item(id: number): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>("#pane .tl-item")).find((n) => n.dataset.rrun === String(id)) as HTMLElement;
}
/** An open row's value block, by its caption. */
function block(id: number, cap: string): HTMLElement | undefined {
  return Array.from(item(id).querySelectorAll<HTMLElement>(".vblock"))
    .find((b) => b.querySelector(".vblock-cap")?.textContent === cap);
}
function open(id: number): void {
  (item(id).querySelector(".tl-sum") as HTMLElement).click();
}

describe("the Remote Runs page (remote plugin, the run record)", () => {
  it("the view module exists at the entry the page descriptor points at", () => {
    expect(existsSync(join(here, "..", "src", "views", "remote-runs.ts"))).toBe(true);
  });

  it("the head is one sentence, what the record keeps, the target filter and a ⋯ - no caption, no standing Clear", async () => {
    // One list is the whole page, so its filter and its ⋯ are the head's (the Jobs and Tunnels
    // head ⋯ hold page-wide verbs too). In a captionless section head they stood on a row of
    // their own over the note (found on a walk of SPEC §panel.pages).
    serve([finished], [running]);
    await view.mount();
    const pane = $("pane");
    expect(pane.querySelector(".pane-desc")?.textContent).toBe("Every command, sync and pull run on a remote target, with its output.");
    expect(pane.querySelector(".pane-sub")?.textContent).toBe("1 run recorded · 1.5 KB of 500.0 MB · kept 30 days");
    const acts = Array.from(pane.querySelectorAll(".pane-actions > select, .pane-actions > button")).map((n) => n.id);
    expect(acts).toEqual(["rrTarget", "rrMore"]);
    expect(pane.querySelector(".sec-head, .sec-cap, .sec-note")).toBeNull();
    expect(Array.from(($("rrTarget") as HTMLSelectElement).options).map((o) => o.value)).toEqual(["", "build", "dev"]);
    expect($("rrClear")).toBeNull();
    expect($("countChip").textContent).toBe("1 run");
    expect(view.countText()).toBe("1 run");
  });

  it("a run is a timeline row: live first with its pulse, then the record - the kind, the command, a failure's red tag, the duration", async () => {
    serve([finished], [running]);
    await view.mount();
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#pane .tl-item")).map((n) => n.dataset.rrun);
    expect(rows).toEqual(["18", "17"]);
    const live = item(18);
    expect(live.querySelector(".tl-title")?.textContent).toBe("exec");
    expect(live.querySelector(".tl-arg")?.textContent).toBe("./test.sh");
    expect(live.querySelector(".tl-live")?.textContent).toBe("running");
    expect(live.querySelector(".tl-live .dot")?.className).toBe("dot starting");
    expect(live.querySelector(".tl-ms")).toBeNull();
    const done = item(17);
    expect(done.querySelector(".tl-arg")?.textContent).toBe("make -j8");
    expect(done.querySelector(".tag.bad")?.textContent).toBe("exit 2");
    expect(done.querySelector(".tl-ms")?.textContent).toBe("12 s");
    // One target, no actor anywhere: the who column is left out (it would say the same on every row).
    expect(done.querySelector(".tl-who")).toBeNull();
  });

  it("who is the target and the SURFACE - never the machine's login name (2026-09-28)", async () => {
    /* The actor rode into every row as `cli:<user>@<host>`, and on a personal machine that
       login name is the operator's own ("页面上把我名称都暴露出来了"). The CLI records the bare
       surface now; a record written before that still carries the identity in the file, and the
       page is where it stops. The fixture is a placeholder identity on purpose - this file is
       tracked, and a test about not printing a login name is no reason to commit a real one. */
    const byCli = { ...finished, runId: 19, actor: "cli:小明@build-box" };
    const byMcp = { ...running, runId: 20, actor: "mcp:claude-code" };
    // A file action has no exit code: success is no tag, its kind and shape say what ran.
    const synced = { ...finished, runId: 21, actor: "panel", action: "remote.sync", state: "succeeded", exitCode: undefined, input: { target: "dev", source: ".", to: "/srv" }, meta: { target: "dev" } };
    serve([byCli, finished, synced], [byMcp]);
    await view.mount();
    const who19 = item(19).querySelector(".tl-who")?.textContent || "";
    expect(who19).toBe("build · cli");
    expect(who19, "no login name, no hostname").not.toMatch(/@|小/);
    expect(item(20).querySelector(".tl-who")?.textContent).toBe("build · mcp");
    expect(item(21).querySelector(".tl-who")?.textContent).toBe("dev · panel");
    // And the open row's meta line says the same, not the record's raw string.
    open(19);
    await settle();
    const meta = item(19).querySelector(".tl-meta")?.textContent || "";
    expect(meta).toContain("cli");
    expect(meta, "the identity is not hiding in the body either").not.toContain("@build-box");
  });

  it("the actor label keeps the surface and drops whatever follows it", () => {
    const label = view.actorLabel as (a: string | null | undefined) => string;
    expect(label("cli:jdoe@box")).toBe("cli");
    expect(label("cli")).toBe("cli");
    expect(label("panel")).toBe("panel");
    expect(label("mcp:claude-code")).toBe("mcp");
    expect(label("mcp:a:b:c")).toBe("mcp");
    // Nothing to keep: an empty or absent actor stays empty, and a row then shows the target alone.
    expect(label("")).toBe("");
    expect(label(null)).toBe("");
    expect(label(undefined)).toBe("");
    // A leading colon has no surface before it - the string stands as it is rather than vanishing.
    expect(label(":odd")).toBe(":odd");
  });

  it("who is the target and the actor, verbatim (SPEC §remote.history) - a column once the rows disagree", async () => {
    const byCli = { ...finished, runId: 19, actor: "cli:jdoe@box" };
    const byMcp = { ...running, runId: 20, actor: "mcp:claude-code" };
    // A file action has no exit code: success is no tag, its kind and shape say what ran.
    const synced = { ...finished, runId: 21, actor: "panel", action: "remote.sync", state: "succeeded", exitCode: undefined, input: { target: "dev", source: ".", to: "/srv" }, meta: { target: "dev" } };
    serve([byCli, finished, synced], [byMcp]);
    await view.mount();
    expect(item(19).querySelector(".tl-who")?.textContent).toBe("build · cli");
    expect(item(20).querySelector(".tl-who")?.textContent).toBe("build · mcp");
    // A row an older gateway recorded has no actor: the target alone, no empty seat.
    expect(item(17).querySelector(".tl-who")?.textContent).toBe("build");
    expect(item(21).querySelector(".tl-title")?.textContent).toBe("sync");
    expect(item(21).querySelector(".tl-arg")?.textContent).toBe(". → /srv");
    expect(item(21).querySelector(".tag")).toBeNull();
  });

  it("every run that did not succeed is red and says how: exit code, timed out, canceled, failed", async () => {
    const t = (runId: number, o: Record<string, unknown>) => ({ ...finished, runId, exitCode: undefined, ...o });
    serve([
      t(31, { state: "timeout", timedOut: true }),
      t(32, { state: "canceled", canceled: true }),
      t(33, { state: "failed", action: "remote.pull", input: { target: "build", remote: "/var/log/x" } }),
      t(34, { state: "succeeded", exitCode: 0 }),
    ], []);
    await view.mount();
    const tagOf = (id: number) => item(id).querySelector(".tag.bad")?.textContent ?? null;
    expect([tagOf(31), tagOf(32), tagOf(33), tagOf(34)]).toEqual(["timed out", "canceled", "failed", null]);
  });

  it("identical consecutive runs fold into ×N - a run in flight never does", async () => {
    const same = (runId: number) => ({ ...finished, runId, state: "succeeded", exitCode: 0 });
    serve([same(41), same(40), same(39)], [running, { ...running, runId: 19 }]);
    await view.mount();
    expect(Array.from(document.querySelectorAll<HTMLElement>("#pane .tl-item")).map((n) => n.dataset.rrun)).toEqual(["18", "19", "41"]);
    expect(item(41).querySelector(".tl-n")?.textContent).toBe("×3");
  });

  it("an empty record draws the empty state, and the ⋯'s Clear is off", async () => {
    serve([], []);
    await view.mount();
    expect($("rrList").querySelector(".empty")?.textContent).toContain("No runs yet");
    $("rrMore").click();
    const clear = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === "Clear")!;
    expect(clear.disabled).toBe(true);
    expect($("countChip").textContent).toBe("");
  });

  it("opening a recorded row fetches its output and paints the meta and the Output block", async () => {
    serve([finished], []);
    replies["/api/remote/runs/17/output?after=0&max=131072"] = {
      runId: 17, cursor: 0, nextCursor: 24, output: "compiling...\nok\nwarn: x\n", total: 24, truncated: false, terminal: true,
    };
    await view.mount();
    open(17);
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs/17/output?after=0&max=131072")).toBe(true);
    const it17 = item(17);
    expect(it17.classList.contains("open")).toBe(true);
    expect(it17.querySelector(".tl-meta")?.textContent).toBe("#17 · build · src · exit 2");
    expect(block(17, "Command")?.querySelector("pre")?.textContent).toBe("make -j8");
    expect(block(17, "Output")?.querySelector(".vblock-note")?.textContent).toBe("24 B");
    expect(block(17, "Output")?.querySelector("pre")?.textContent).toBe("compiling...\nok\nwarn: x\n");
    expect(it17.querySelector("[data-rmore]")).toBeNull();
    // A click inside the open body is someone reading it: the row stays open.
    (block(17, "Output")!.querySelector("pre") as HTMLElement).click();
    expect(it17.classList.contains("open")).toBe(true);
    open(17);
    expect(it17.classList.contains("open")).toBe(false);
    expect(it17.querySelector(".tl-body")).toBeNull();
  });

  it("a long output pages through Load more, and a capped one shows its tail", async () => {
    const capped = { ...finished, runId: 21, outputBytes: 131072 * 2, outputCapped: true, tail: "the last lines\n" };
    serve([capped], []);
    replies["/api/remote/runs/21/output?after=0&max=131072"] = { runId: 21, cursor: 0, nextCursor: 131072, output: "head", total: 262144, terminal: true };
    replies["/api/remote/runs/21/output?after=131072&max=131072"] = { runId: 21, cursor: 131072, nextCursor: 262144, output: "+rest", total: 262144, terminal: true };
    await view.mount();
    open(21);
    await settle();
    let body = item(21).querySelector(".tl-body") as HTMLElement;
    expect(block(21, "Output")?.querySelector(".vblock-note")?.textContent).toBe("128.0 KB of 256.0 KB");
    expect(body.querySelector("[data-rmore]")?.textContent).toBe("Load more");
    const tail = block(21, "Tail")!;
    expect(tail.querySelector(".vblock-note")?.textContent).toBe("output capped at 16.0 MB; its last 15 B");
    expect(tail.querySelector("pre")?.textContent).toBe("the last lines\n");
    (body.querySelector("[data-rmore]") as HTMLElement).click();
    await settle();
    body = item(21).querySelector(".tl-body") as HTMLElement;
    expect(block(21, "Output")?.querySelector("pre")?.textContent).toBe("head+rest");
    expect(body.querySelector("[data-rmore]")).toBeNull();
  });

  it("an evicted output says so in place of the stream, keeps a capped run's tail, and the note names the window (SPEC §remote.history)", async () => {
    const evicted = { ...finished, runId: 22, outputBytes: 3072, outputEvicted: true, outputCapped: true, tail: "the last lines\n" };
    serve([evicted], [], { limits: { ...LIMITS, auditWindowMs: 7 * 86400000 } });
    replies["/api/remote/runs/22/output?after=0&max=131072"] = { runId: 22, cursor: 0, nextCursor: 0, output: "", total: 0, truncated: false, terminal: true };
    await view.mount();
    expect($("pane").querySelector(".pane-sub")?.textContent).toContain("kept 30 days · the last 7 days always traceable");
    open(22);
    await settle();
    const body = item(22).querySelector(".tl-body") as HTMLElement;
    expect(body.textContent).toContain("The output (3.0 KB) was evicted by the size budget; the record itself stays for 30 days.");
    expect(body.textContent).not.toContain("No output was produced.");
    expect(body.textContent).toContain("the last lines");
  });

  /* The owner's report (2026-09-28): a write's row showed "Output 34 B" and nothing of what
     was written. The body is kept sealed beside the record (SPEC §remote.history) and unsealed only
     when asked - a screenshot of an opened row shares no file until Show is pressed. */
  it("a write keeps what it wrote: Show unseals and paints it, Hide puts it away", async () => {
    const wrote = {
      ...finished, runId: 31, action: "remote.write", label: "write stream-demo.py", state: "succeeded", exitCode: 0,
      input: { target: "build", remote: "stream-demo.py", contentBytes: 12 }, outputBytes: 42, contentStored: true,
    };
    serve([wrote], []);
    replies["/api/remote/runs/31/output?after=0&max=131072"] = { runId: 31, cursor: 0, nextCursor: 42, output: "wrote /tmp/swiss/stream-demo.py (12 bytes)", total: 42, terminal: true };
    replies["/api/remote/runs/31/content"] = { runId: 31, content: "print('hi')\n", truncated: false };
    await view.mount();
    open(31);
    await settle();
    const block = (): HTMLElement | undefined => Array.from(item(31).querySelectorAll<HTMLElement>(".vblock"))
      .find((b) => b.querySelector(".vblock-cap")?.textContent === "Content written");
    expect(block(), "the block is there").toBeTruthy();
    expect(block()!.querySelector(".vblock-note")?.textContent).toBe("12 B");
    expect(block()!.querySelector("pre")).toBeNull();
    expect(requests.some((r) => r.path === "/api/remote/runs/31/content"), "nothing is unsealed until asked").toBe(false);
    (block()!.querySelector("[data-rcontent]") as HTMLElement).click();
    await settle();
    expect(block()!.querySelector("pre")?.textContent).toBe("print('hi')\n");
    (block()!.querySelector("[data-rcontenthide]") as HTMLElement).click();
    expect(block()!.querySelector("pre")).toBeNull();
    expect(block()!.querySelector("[data-rcontent]")).toBeTruthy();
  });

  it("a write whose body the size budget took says so", async () => {
    const wrote = {
      ...finished, runId: 32, action: "remote.write", state: "succeeded", exitCode: 0,
      input: { target: "build", remote: ".env", contentBytes: 2048 }, outputBytes: 0, contentEvicted: true,
    };
    serve([wrote], []);
    replies["/api/remote/runs/32/output?after=0&max=131072"] = { runId: 32, cursor: 0, nextCursor: 0, output: "", total: 0, terminal: true };
    await view.mount();
    open(32);
    await settle();
    expect(item(32).querySelector(".tl-body")?.textContent).toContain("The written content (2.0 KB) was evicted by the size budget.");
    expect(item(32).querySelector("[data-rcontent]")).toBeNull();
  });

  /* Between 9a6aebe (the clear body stopped being recorded) and R12 (it started being sealed)
     a write kept its size and nothing else. Those rows drew NO content block at all, so they
     read exactly like a row whose Show had not been pressed. They say what happened instead. */
  it("a write recorded while no body was kept says that, rather than drawing nothing", async () => {
    const wrote = {
      ...finished, runId: 35, action: "remote.write", state: "succeeded", exitCode: 0,
      input: { target: "build", remote: "seed.sql", contentBytes: 5587 }, outputBytes: 0,
    };
    serve([wrote], []);
    replies["/api/remote/runs/35/output?after=0&max=131072"] = { runId: 35, cursor: 0, nextCursor: 0, output: "", total: 0, terminal: true };
    await view.mount();
    open(35);
    await settle();
    expect(item(35).querySelector(".tl-body")?.textContent)
      .toContain("This write (5.5 KB) was recorded before written content was kept.");
    expect(item(35).querySelector("[data-rcontent]"), "nothing to unseal").toBeNull();
  });

  it("a run that wrote no file keeps its body free of content notes", async () => {
    serve([{ ...finished, runId: 36 }], []);
    replies["/api/remote/runs/36/output?after=0&max=131072"] = { runId: 36, cursor: 0, nextCursor: 0, output: "", total: 0, terminal: true };
    await view.mount();
    open(36);
    await settle();
    expect(item(36).querySelector(".tl-body")?.textContent).not.toContain("written content");
  });

  /* The owner's report (2026-09-28): a long command's row is cut with an ellipsis, and an open
     row showed its output but never the command whole - it could be neither read nor copied. */
  it("an open row states its whole command, quoted the way the gateway sent it, with a Copy", async () => {
    const script = {
      ...finished, runId: 33,
      input: { target: "build", argv: ["sh", "-c", "for i in $(seq 1 3); do echo \"$i\"; done", "it's", ""] },
    };
    serve([script], []);
    replies["/api/remote/runs/33/output?after=0&max=131072"] = { runId: 33, cursor: 0, nextCursor: 0, output: "", total: 0, terminal: true };
    await view.mount();
    open(33);
    await settle();
    const line = "sh -c 'for i in $(seq 1 3); do echo \"$i\"; done' 'it'\\''s' ''";
    expect(block(33, "Command")?.querySelector("pre")?.textContent).toBe(line);
    const wrote: string[] = [];
    vi.stubGlobal("navigator", { clipboard: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } } });
    try {
      (block(33, "Command")!.querySelector("[data-rcopy]") as HTMLElement).click();
      await settle();
      expect(wrote).toEqual([line]);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("a file action's Command block names the action and its path", async () => {
    const wrote = {
      ...finished, runId: 34, action: "remote.write", state: "failed", exitCode: undefined,
      input: { target: "build", remote: "C:/Users/me/seed.sql", contentBytes: 10 }, outputBytes: 0,
    };
    serve([wrote], []);
    replies["/api/remote/runs/34/output?after=0&max=131072"] = { runId: 34, cursor: 0, nextCursor: 0, output: "", total: 0, terminal: true };
    await view.mount();
    open(34);
    await settle();
    expect(block(34, "Command")?.querySelector("pre")?.textContent).toBe("write C:/Users/me/seed.sql");
  });

  it("a run with no output says so in its Output block", async () => {
    serve([{ ...finished, state: "succeeded", exitCode: 0, outputBytes: 0 }], []);
    replies["/api/remote/runs/17/output?after=0&max=131072"] = { runId: 17, cursor: 0, nextCursor: 0, output: "", total: 0, terminal: true };
    await view.mount();
    open(17);
    await settle();
    expect(item(17).querySelector(".vblock-text")?.textContent).toBe("No output was produced.");
  });

  it("opening a live row follows /api/runs output and offers Cancel, which posts the cancel", async () => {
    serve([], [running]);
    replies["/api/runs/18/output?after=0&max=131072"] = { runId: 18, state: "running", cursor: 0, nextCursor: 5, output: "tick\n", truncated: false, terminal: false };
    replies["POST /api/runs/18/cancel"] = { runId: 18, state: "canceled" };
    await view.mount();
    open(18);
    // Before the first bytes: the waiting line, and Cancel in the block's tools.
    expect(block(18, "Live output"), "the live block").toBeTruthy();
    expect(block(18, "Command")?.querySelector("pre")?.textContent).toBe("./test.sh");
    await settle();
    expect(requests.some((r) => r.path === "/api/runs/18/output?after=0&max=131072")).toBe(true);
    const body = item(18).querySelector(".tl-body") as HTMLElement;
    expect(block(18, "Live output")?.querySelector("pre")?.textContent).toBe("tick\n");
    const cancel = body.querySelector("[data-rcancel]") as HTMLElement;
    expect(cancel.textContent).toBe("Cancel");
    cancel.click();
    await settle();
    expect(requests.some((r) => r.path === "/api/runs/18/cancel" && r.method === "POST")).toBe(true);
    view.unmount(); // stop the live timer
  });

  it("Older asks for the page before the cursor, Newer comes back, and the filter narrows by target", async () => {
    serve([finished], [], { nextBefore: 17 });
    replies["/api/remote/runs?limit=20&before=17"] = { runs: [{ ...finished, runId: 9 }], active: [], usage: { bytes: 1536, runs: 2 } };
    replies["/api/remote/runs?limit=20&target=dev"] = { runs: [], active: [], usage: { bytes: 1536, runs: 2 } };
    await view.mount();
    expect($("rrNext").textContent).toBe("Older");
    expect(($("rrPrev") as HTMLButtonElement).disabled).toBe(true);
    expect($("pane").querySelector(".pager")?.getAttribute("aria-label")).toBe("Run pages");
    $("rrNext").click();
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs?limit=20&before=17")).toBe(true);
    expect(item(9)).toBeTruthy();
    expect($("pane").querySelector(".pager-status")?.textContent).toBe("Page 2");
    $("rrPrev").click();
    await settle();
    expect(item(17)).toBeTruthy();
    const sel = $("rrTarget") as HTMLSelectElement;
    sel.value = "dev";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs?limit=20&target=dev")).toBe(true);
    expect($("rrList").textContent).toBe("No runs on this target yet.");
  });

  it("Clear lives behind the section's ⋯: it confirms, deletes the record and repaints", async () => {
    serve([finished], []);
    replies["DELETE /api/remote/runs"] = { ok: true };
    await view.mount();
    $("rrMore").click();
    const clear = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === "Clear")!;
    expect(clear.disabled).toBe(false);
    expect(clear.className).toContain("danger");
    clear.click();
    await settle();
    expect(requests.some((r) => r.path === "/api/remote/runs" && r.method === "DELETE")).toBe(true);
  });

  it("a poll that changes nothing leaves the rows alone - an open row stays open", async () => {
    serve([finished], []);
    replies["/api/remote/runs/17/output?after=0&max=131072"] = { runId: 17, cursor: 0, nextCursor: 2, output: "ok", total: 2, terminal: true };
    await view.mount();
    open(17);
    await settle();
    const it17 = item(17);
    await view.poll();
    expect(item(17)).toBe(it17);
    expect(it17.classList.contains("open")).toBe(true);
    // A new run arriving does repaint - and the open row comes back open.
    serve([{ ...finished, runId: 30, exitCode: 1 }, finished], []);
    replies["/api/remote/runs/17/output?after=0&max=131072"] = { runId: 17, cursor: 0, nextCursor: 2, output: "ok", total: 2, terminal: true };
    await view.poll();
    expect(item(30)).toBeTruthy();
    expect(item(17).classList.contains("open")).toBe(true);
    expect(block(17, "Output")?.querySelector("pre")?.textContent).toBe("ok");
  });
});

/* The owner (2026-09-29): "我在 Runs 里面点取消，会有BUG，1. 点了后，没有 loading 状态内容，2. 点了两下
   后，确实取消了，但是还得转圈，我刷新才有". The gateway's cancel waits for the run to STOP, so the POST
   takes seconds; these park it to look at every moment of that wait - and at what happens when
   the operator has walked away from the page before it answers. */
describe("Cancel, while the run is stopping and after (2026-09-29)", () => {
  let release: ((ok: boolean) => void) | null = null;
  // The suite's own stub, read when a test runs: beforeAll installs it after this block is defined.
  let stub: typeof fetch = globalThis.fetch;
  const parkCancel = (): void => {
    stub = globalThis.fetch;
    Object.assign(globalThis, {
      fetch: (url: string, init?: RequestInit): Promise<Response> => {
        const u = String(url);
        if (u === "/api/runs/18/cancel") {
          requests.push({ path: u, method: init?.method || "GET" });
          return new Promise((res) => {
            release = (ok: boolean) => res({ ok, status: ok ? 200 : 409, json: () => Promise.resolve(ok ? { runId: 18, state: "canceled" } : { error: "run 18 is not running" }) } as Response);
          });
        }
        return stub(url, init);
      },
    });
  };
  const cancelBtn = (): HTMLButtonElement => item(18).querySelector("[data-rcancel]") as HTMLButtonElement;
  const liveOut = { runId: 18, state: "running", cursor: 0, nextCursor: 5, output: "tick\n", truncated: false, terminal: false };

  it("says Canceling… and is disabled while the stop is on its way; a second click sends nothing", async () => {
    serve([], [running]);
    replies["/api/runs/18/output?after=0&max=131072"] = liveOut;
    parkCancel();
    try {
      await view.mount();
      open(18);
      await settle();
      cancelBtn().click();
      await settle();
      expect(cancelBtn().textContent).toBe("Canceling…");
      expect(cancelBtn().disabled).toBe(true);
      cancelBtn().click();
      await settle();
      expect(requests.filter((r) => r.path === "/api/runs/18/cancel"), "one POST").toHaveLength(1);
      release!(true);
      await settle();
    } finally {
      Object.assign(globalThis, { fetch: stub });
      view.unmount();
    }
  });

  it("a refused cancel gives the button back", async () => {
    serve([], [running]);
    replies["/api/runs/18/output?after=0&max=131072"] = liveOut;
    parkCancel();
    try {
      await view.mount();
      open(18);
      await settle();
      cancelBtn().click();
      await settle();
      release!(false);
      await settle();
      expect(cancelBtn().textContent).toBe("Cancel");
      expect(cancelBtn().disabled).toBe(false);
    } finally {
      Object.assign(globalThis, { fetch: stub });
      view.unmount();
    }
  });

  it("once stopped, the open row reads the RECORDED output by itself - no spinner, no reload", async () => {
    serve([], [running]);
    replies["/api/runs/18/output?after=0&max=131072"] = liveOut;
    parkCancel();
    try {
      await view.mount();
      open(18);
      await settle();
      cancelBtn().click();
      await settle();
      // The run leaves the active set and lands in the record.
      serve([{ ...running, state: "canceled", canceled: true, endedAt: now, ms: 900 }], []);
      replies["/api/remote/runs/18/output?after=0&max=131072"] = { runId: 18, cursor: 0, nextCursor: 5, output: "tick\n", total: 5, terminal: true };
      release!(true);
      await settle();
      await settle();
      expect(requests.some((r) => r.path === "/api/remote/runs/18/output?after=0&max=131072"), "the record was asked").toBe(true);
      expect(block(18, "Live output"), "the live block is gone with the live run").toBeUndefined();
      expect(block(18, "Output")?.querySelector("pre")?.textContent, "the recorded output, not a spinner").toBe("tick\n");
    } finally {
      Object.assign(globalThis, { fetch: stub });
      view.unmount();
    }
  });

  it("walked away before the stop came back: the answer paints nothing over the page now on screen", async () => {
    serve([], [running]);
    replies["/api/runs/18/output?after=0&max=131072"] = liveOut;
    parkCancel();
    try {
      await view.mount();
      open(18);
      await settle();
      cancelBtn().click();
      await settle();
      view.unmount();
      $("pane").innerHTML = '<div id="someOtherPage">Logs</div>';
      const before = requests.length;
      release!(true);
      await settle();
      await settle();
      expect($("pane").innerHTML, "the other page is untouched").toBe('<div id="someOtherPage">Logs</div>');
      expect(requests.slice(before).some((r) => r.path.startsWith("/api/remote/runs?")), "no reload of a page that is gone").toBe(false);
      expect($("toast").textContent, "the panel still says the cancel went through").toBe("Cancel requested for run #18");
    } finally {
      Object.assign(globalThis, { fetch: stub });
      view.unmount();
    }
  });
});
