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

/* The Remote Targets page (docs/34 R6 + R8, on the library since docs/46 P6-2): a grouped list
   of alias rows plus an Add/Edit sheet. The page carries the docs/20 family wiring - the group
   names from the same response become headers, the sheet's Group select rides the POST body,
   the New-group glyph exists - and the docs/46 row: the endpoint's state only when it is not a
   resting one, where the target runs as the sub-line, and when it last ran from the run record.

   A real DOM (happy-dom) since P6-2: the library's rows, sheet and menu are real nodes, so the
   suite reads them the way a user sees them instead of a hand-rolled stub's serialisation. */

import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view is driven by its
   page-registry surface (mount / refresh / countText), so a typed import would add nothing. */
let view: any;
let replies: Record<string, unknown> = {};
let requests: { path: string; method: string; body?: string }[] = [];

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;
const MIN = 60 * 1000;

/* The shell skeleton from the SERVED index.html (the house idiom): modules in this graph wire
 * shell controls at evaluation time, so the ids exist before the import. */
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
      requests.push({ path: u, method, body: init?.body as string | undefined });
      const reply = replies[(method !== "GET" ? method + " " : "") + u] ?? replies[u];
      const ok = reply !== undefined;
      return Promise.resolve({ status: ok ? 200 : 500, ok, json: () => Promise.resolve(reply ?? { error: "no stub for " + u }) } as Response);
    },
    confirm: () => true,
  });
  document.body.innerHTML = shellSkeleton();
  view = await import("../src/views/remote.js");
});

beforeEach(() => {
  requests = [];
  document.body.innerHTML = shellSkeleton();
  localStorage.clear();
});

const BUILD = { id: "build", label: "Build", endpoint: "conn-1", workspaceRoot: "/data/ws/proj", capabilities: ["exec", "sync"] };

function serve(targets: unknown[], groups?: string[], o: { state?: string; runs?: unknown[]; active?: unknown[]; nextBefore?: number; noRuns?: boolean } = {}) {
  replies = {
    "/api/remote/endpoints": { presence: "serving", endpoints: [{ id: "conn-1", label: "Build box", state: o.state || "idle" }] },
    "/api/remote/targets": groups ? { targets, groups } : { targets },
  };
  if (!o.noRuns) {
    replies["/api/remote/runs?limit=100"] = {
      runs: o.runs || [], active: o.active || [], usage: { bytes: 0, runs: (o.runs || []).length },
      limits: { maxAgeMs: 30 * 86400000, maxTotalBytes: 1, maxRuns: 1, maxOutputBytes: 1 },
      ...(o.nextBefore ? { nextBefore: o.nextBefore } : {}),
    };
  }
}

function rowOf(id: string): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>("#rmGroups [data-rmrow]")).find((n) => n.dataset.rmrow === id) as HTMLElement;
}

function menuItem(label: string): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>("#menu button")).find((b) => b.textContent === label) as HTMLElement;
}

function ago(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

describe("the Remote Targets page (remote plugin, R6 + R8)", () => {
  it("the view module exists at the entry the page descriptor points at", () => {
    expect(existsSync(join(here, "..", "src", "views", "remote.ts"))).toBe(true);
  });

  it("the head is one sentence, the transport line, the folder-plus glyph and the one primary", async () => {
    serve([BUILD]);
    await view.mount();
    const pane = $("pane");
    expect(pane.querySelector(".pane-desc")?.textContent).toBe("Machines the gateway runs commands on, over Tunnels SSH connections.");
    expect(pane.querySelector(".pane-sub")?.textContent).toBe("1 endpoint served by tunnels");
    const acts = Array.from(pane.querySelectorAll(".pane-actions > *"));
    expect(acts.map((b) => b.id)).toEqual(["rmNewGroup", "rmAdd"]);
    expect($("rmNewGroup").querySelector("use")?.getAttribute("href")).toBe("#i-folder-plus");
    expect($("rmNewGroup").getAttribute("aria-label")).toBe("New group");
    expect($("rmAdd").className).toBe("btn primary");
    expect($("countChip").textContent).toBe("1 target");
  });

  it("a row is the alias, where it runs as the sub-line, and one ⋯ - no lead dot, no chips", async () => {
    serve([BUILD]);
    await view.mount();
    const r = rowOf("build");
    expect(r.classList.contains("lrow")).toBe(true);
    expect(r.getAttribute("draggable")).toBe("true");
    expect(r.querySelector(".lrow-lead")).toBeNull();
    expect(r.querySelector(".lrow-name")?.textContent).toBe("build");
    // The label leads the sub-line when it says more than the alias; the root is mono.
    expect(r.querySelector(".lrow-sub")?.textContent).toBe("Build · Build box · /data/ws/proj · exec, sync");
    expect(r.querySelector(".lrow-sub code")?.textContent).toBe("/data/ws/proj");
    expect(r.querySelector(".lrow-acts [data-rmmore]")?.getAttribute("aria-label")).toBe("Actions for build");
    // A label the sheet defaulted to the alias is not said twice.
    serve([{ ...BUILD, label: "build" }]);
    await view.mount();
    expect(rowOf("build").querySelector(".lrow-sub")?.textContent).toBe("Build box · /data/ws/proj · exec, sync");
  });

  it("the endpoint's state shows only when it is not a resting one: a dial is amber, an error red", async () => {
    // Connected and idle both run a command (idle dials on demand): no dot. The old row gave
    // an `error` the amber of a transition.
    for (const [state, cls] of [["connected", null], ["idle", null], ["connecting", "dot starting"], ["error", "dot down"]] as const) {
      serve([BUILD], undefined, { state });
      await view.mount();
      const d = rowOf("build").querySelector(".lrow-name .dot");
      expect(d ? d.className : null, state).toBe(cls);
      if (d) expect(d.getAttribute("title")).toBe("endpoint " + state);
    }
    // A target whose endpoint is gone (the Tunnels connection was deleted) cannot run either.
    serve([{ ...BUILD, endpoint: "conn-gone" }]);
    await view.mount();
    expect(rowOf("build").querySelector(".lrow-name .dot")?.className).toBe("dot down");
  });

  it("the last-run column: the newest run per target from one read, a failure a red tag, a run in flight its pulse", async () => {
    const other = { ...BUILD, id: "dev", label: "dev" };
    const idle = { ...BUILD, id: "lab", label: "lab" };
    serve([BUILD, other, idle], undefined, {
      active: [{ runId: 9, action: "remote.exec", state: "running", queuedAt: ago(MIN), input: { target: "dev", argv: ["./test.sh"] } }],
      runs: [
        { runId: 8, action: "remote.exec", state: "failed", exitCode: 2, queuedAt: ago(4 * MIN), endedAt: ago(3 * MIN), meta: { target: "build" }, input: { target: "build", argv: ["make", "-j8"] } },
        { runId: 7, action: "remote.exec", state: "succeeded", exitCode: 0, queuedAt: ago(90 * MIN), endedAt: ago(90 * MIN), meta: { target: "build" }, input: { target: "build", argv: ["make"] } },
        { runId: 6, action: "remote.sync", state: "succeeded", queuedAt: ago(2 * 60 * MIN), endedAt: ago(2 * 60 * MIN), meta: { target: "dev" }, input: { target: "dev", source: "." } },
      ],
    });
    await view.mount();
    expect(requests.filter((r) => r.path.startsWith("/api/remote/runs")).map((r) => r.path)).toEqual(["/api/remote/runs?limit=100"]);
    const col = (id: string): HTMLElement => rowOf(id).querySelector(".lrow-col") as HTMLElement;
    // build: its newest run (8, not 7) failed - the red tag the Runs page gives it, then when.
    expect(col("build").className).toBe("lrow-col w-l");
    expect(col("build").querySelector(".tag.bad")?.textContent).toBe("exit 2");
    expect(col("build").textContent).toBe("exit 2 3 min. ago");
    expect(col("build").title).toMatch(/^Last run: .+ · make -j8$/);
    // dev: the in-flight run is newer than anything recorded.
    expect(col("dev").querySelector(".dot")?.className).toBe("dot starting");
    expect(col("dev").textContent).toBe(" running");
    expect(col("dev").title).toBe("./test.sh");
    // lab: nothing on record, and the newest page was the whole record (no nextBefore).
    expect(col("lab").textContent).toBe("No runs");
  });

  it("a target the newest page missed is asked on its own - once per visit, not every poll", async () => {
    serve([BUILD], undefined, {
      nextBefore: 50,
      runs: [{ runId: 60, action: "remote.exec", state: "succeeded", exitCode: 0, queuedAt: ago(MIN), endedAt: ago(MIN), meta: { target: "dev" }, input: { target: "dev", argv: ["ls"] } }],
    });
    replies["/api/remote/runs?limit=1&target=build"] = {
      runs: [{ runId: 3, action: "remote.exec", state: "succeeded", exitCode: 0, queuedAt: ago(3 * 86400000), endedAt: ago(3 * 86400000), meta: { target: "build" }, input: { target: "build", argv: ["make"] } }],
      active: [], usage: { bytes: 0, runs: 1 },
    };
    await view.mount();
    expect(rowOf("build").querySelector(".lrow-col")?.textContent).toBe("3 days ago");
    await view.refresh();
    await view.refresh();
    expect(requests.filter((r) => r.path === "/api/remote/runs?limit=1&target=build").length).toBe(1);
  });

  it("when the run record does not answer, the column is not drawn - and nothing is toasted", async () => {
    serve([BUILD], undefined, { noRuns: true });
    await view.mount();
    expect(rowOf("build").querySelector(".lrow-col")).toBeNull();
    expect($("toast").textContent).toBe("");
  });

  it("a poll patches the column in place: the row and its ⋯ stay the same nodes", async () => {
    serve([BUILD], undefined, { runs: [] });
    await view.mount();
    const r = rowOf("build");
    const more = r.querySelector("[data-rmmore]");
    expect(r.querySelector(".lrow-col")?.textContent).toBe("No runs");
    serve([BUILD], undefined, {
      runs: [{ runId: 5, action: "remote.exec", state: "succeeded", exitCode: 0, queuedAt: ago(2 * MIN), endedAt: ago(2 * MIN), meta: { target: "build" }, input: { target: "build", argv: ["ls"] } }],
    });
    await view.refresh();
    expect(rowOf("build")).toBe(r);
    expect(r.querySelector("[data-rmmore]")).toBe(more);
    expect(r.querySelector(".lrow-col")?.textContent).toBe("2 min. ago");
  });

  /* The owner (2026-09-28): "the Remote page should let me run write and exec straight from the
     web, and Runs should show what is running so I can cancel it." Runs already streams an open
     run and offers Cancel, so this half is only the door: a recorded run submitted through the
     same POST /api/runs the CLI uses, then the Runs page. The typed line goes to the target's
     own shell rather than through a shell parser written here - pipes, quotes and redirects
     then mean what the operator expects, and the record shows exactly what was sent. */
  const menu = (): string[] =>
    Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent || "");
  const openMore = async (id: string): Promise<void> => {
    await view.mount();
    (rowOf(id).querySelector("[data-rmmore]") as HTMLElement).click();
  };
  const pick = (label: string): void => {
    (Array.from(document.querySelectorAll<HTMLElement>("#menu button"))
      .find((b) => b.textContent === label) as HTMLElement).click();
  };
  const submitted = (): { path: string; method: string; body?: string } | undefined =>
    requests.find((r) => r.method === "POST" && r.path === "/api/runs");

  it("a target's ⋯ opens the run and the write it is allowed, and only those", async () => {
    serve([BUILD]); // exec, sync - no files
    await openMore("build");
    expect(menu()).toEqual(["Run a command…", "Edit", "Delete"]);

    serve([{ ...BUILD, capabilities: ["files"] }]);
    await openMore("build");
    expect(menu(), "no exec, no run door").toEqual(["Write a file…", "Edit", "Delete"]);

    serve([{ ...BUILD, capabilities: ["sync"] }]);
    await openMore("build");
    expect(menu()).toEqual(["Edit", "Delete"]);
  });

  it("Run a command records the run through the target's own shell and goes to Runs", async () => {
    serve([BUILD]);
    replies["POST /api/runs"] = { runId: 77, state: "queued" };
    await openMore("build");
    pick("Run a command…");
    expect($("sheet").hidden).toBe(false);
    ($("rmx-cmd") as HTMLInputElement).value = "make -j8 | tee build.log";
    ($("rmx-cwd") as HTMLInputElement).value = "/data/ws/proj/sub";
    $("rmx-run").click();
    await new Promise((r) => setTimeout(r, 0));
    const post = submitted();
    expect(post, "one submit").toBeTruthy();
    expect(JSON.parse(post!.body as string)).toEqual({
      action: "remote.exec",
      input: { target: "build", argv: ["sh", "-c", "make -j8 | tee build.log"], cwd: "/data/ws/proj/sub" },
      timeoutMs: 15 * MIN,
      actor: "panel",
    });
    expect($("sheet").hidden, "the sheet closes on the run it started").toBe(true);
    expect(location.hash, "the run is watched where runs are watched").toBe("#remote-runs");
  });

  it("an empty command is refused before anything is sent, and the cwd is optional", async () => {
    serve([BUILD]);
    replies["POST /api/runs"] = { runId: 78, state: "queued" };
    await openMore("build");
    pick("Run a command…");
    $("rmx-run").click();
    await new Promise((r) => setTimeout(r, 0));
    expect(submitted(), "nothing sent for an empty line").toBeUndefined();
    expect($("sheet").hidden, "the sheet stays with what was typed").toBe(false);

    ($("rmx-cmd") as HTMLInputElement).value = "uptime";
    $("rmx-run").click();
    await new Promise((r) => setTimeout(r, 0));
    expect(JSON.parse(submitted()!.body as string).input).toEqual({
      target: "build", argv: ["sh", "-c", "uptime"],
    });
  });

  it("Write a file sends the path and the body as one recorded write", async () => {
    serve([{ ...BUILD, capabilities: ["exec", "files"] }]);
    replies["POST /api/runs"] = { runId: 79, state: "queued" };
    await openMore("build");
    pick("Write a file…");
    ($("rmw-path") as HTMLInputElement).value = "conf/app.toml";
    ($("rmw-body") as HTMLTextAreaElement).value = "port = 8080\n";
    $("rmw-save").click();
    await new Promise((r) => setTimeout(r, 0));
    expect(JSON.parse(submitted()!.body as string)).toEqual({
      action: "remote.write",
      input: { target: "build", remote: "conf/app.toml", content: "port = 8080\n" },
      timeoutMs: 15 * MIN,
      actor: "panel",
    });
    expect(location.hash).toBe("#remote-runs");
  });

  it("an empty table still draws the default group - a place with a +, under the standing header", async () => {
    serve([]);
    await view.mount();
    // The groups slot holds the default group as an empty container, never the "No targets
    // yet" empty state - that state used to take the slot whenever only the default group was
    // left, so a fresh page showed no group at all (2026-09-20).
    expect($("rmGroups").children.length).toBe(1);
    expect($("rmGroups").textContent).toContain("default");
    expect($("rmGroups").textContent).not.toContain("No targets yet");
    expect($("rmAdd")).not.toBeNull();
    expect($("countChip").textContent).toBe("");
  });

  it("deleting the last user-made group leaves the default group on the page", async () => {
    serve([], ["default", "test"]);
    await view.mount();
    expect($("rmGroups").children.length).toBe(2);
    serve([], ["default"]);
    await view.refresh();
    expect($("rmGroups").children.length).toBe(1);
    expect($("rmGroups").textContent).toContain("default");
    expect($("rmGroups").textContent).not.toContain("test");
  });

  it("the rows render through the groups component: one container per group, a stale group sinks to the first", async () => {
    serve(
      [
        { ...BUILD, group: "prod" },
        { ...BUILD, id: "flash", label: "flash", group: null },
        { ...BUILD, id: "old", label: "old", group: "deleted-group" },
      ],
      ["default", "prod"],
    );
    await view.mount();
    const [first, second] = Array.from($("rmGroups").children);
    expect(first.textContent).toContain("default");
    expect(first.querySelector('[data-rmrow="flash"]')).not.toBeNull();
    expect(first.querySelector('[data-rmrow="old"]')).not.toBeNull(); // the sink rule the family shares
    expect(second.textContent).toContain("prod");
    expect(second.querySelector('[data-rmrow="build"]')).not.toBeNull();
  });

  it("⋯ holds the run door, Edit and Delete; Edit reopens the sheet prefilled and posts to the row route with the group", async () => {
    serve([{ ...BUILD, capabilities: ["exec"], group: "prod" }], ["default", "prod"]);
    replies["POST /api/remote/targets/build"] = { ok: true };
    await view.mount();
    (rowOf("build").querySelector("[data-rmmore]") as HTMLElement).click();
    expect(Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent)).toEqual(["Run a command…", "Edit", "Delete"]);
    menuItem("Edit").click();
    const sheet = $("sheet");
    expect(sheet.hidden).toBe(false);
    expect(sheet.querySelector(".sheet[role=dialog] h2")?.textContent).toBe("Edit build");
    expect(($("rm-id") as HTMLInputElement).disabled).toBe(true);
    expect(($("rm-label") as HTMLInputElement).value).toBe("Build");
    expect(($("rm-root") as HTMLInputElement).value).toBe("/data/ws/proj");
    expect(($("g-sel") as HTMLSelectElement).value).toBe("prod");
    expect(($("rmcap-exec") as HTMLInputElement).checked).toBe(true);
    expect(($("rmcap-sync") as HTMLInputElement).checked).toBe(false);
    ($("rm-label") as HTMLInputElement).value = "Renamed";
    ($("g-sel") as HTMLSelectElement).value = "default";
    await ($("rm-save").onclick as () => Promise<void>)();
    const post = requests.find((r) => r.method === "POST")!;
    expect(post.path).toBe("/api/remote/targets/build");
    const sent = JSON.parse(String(post.body));
    // The row route still wants the id in the body - the store compares it with the path's to
    // refuse a rename (api.rs write_target). The panel left it out, so every Edit from this page
    // answered "target.id is required" (found on the docs/46 P6-2 walk; the old suite pinned
    // the missing id as the contract).
    expect(sent.id).toBe("build");
    expect(sent.label).toBe("Renamed");
    expect(sent.shell).toBe("posix");
    expect(sent.group).toBe("default");
    expect(sheet.hidden).toBe(true);
  });

  it("the sheet is the library's: alias with its hint, endpoint beside group, capabilities a group whose caption presses nothing", async () => {
    serve([], ["default", "prod"]);
    await view.mount();
    $("rmAdd").click();
    const dlg = $("sheet").querySelector(".sheet[role=dialog]") as HTMLElement;
    expect(dlg.querySelector("h2")?.textContent).toBe("Add a remote target");
    expect(dlg.querySelector(".sheet-body .hint")?.textContent).toBe("The name commands call: swiss remote exec <alias>");
    const two = dlg.querySelector(".sheet-body > .two") as HTMLElement;
    expect(two.querySelector("#rm-endpoint")).not.toBeNull();
    expect(two.querySelector("#g-sel")).not.toBeNull();
    const caps = dlg.querySelector('.field[role="group"]') as HTMLElement;
    expect(caps.querySelector(":scope > span")?.textContent).toBe("Capabilities");
    (caps.querySelector(":scope > span") as HTMLElement).click();
    expect(["exec", "sync", "files"].map((c) => ($("rmcap-" + c) as HTMLInputElement).checked)).toEqual([true, false, false]);
    expect(Array.from(dlg.querySelectorAll(".sheet-foot button")).map((b) => b.textContent)).toEqual(["Cancel", "Add"]);
    $("rm-cancel").click();
    expect($("sheet").hidden).toBe(true);
  });

  it("Add posts the typed row - group included - to the same route the CLI uses", async () => {
    serve([], ["default", "prod"]);
    replies["POST /api/remote/targets"] = { ok: true };
    await view.mount();
    $("rmAdd").click();
    ($("rm-id") as HTMLInputElement).value = "build";
    ($("rm-label") as HTMLInputElement).value = "Build";
    ($("rm-endpoint") as HTMLSelectElement).value = "conn-1";
    ($("g-sel") as HTMLSelectElement).value = "prod";
    ($("rm-root") as HTMLInputElement).value = "/data/ws/proj";
    ($("rmcap-sync") as HTMLInputElement).checked = true;
    await ($("rm-save").onclick as () => Promise<void>)();
    const post = requests.find((r) => r.method === "POST")!;
    expect(post.path).toBe("/api/remote/targets");
    const sent = JSON.parse(String(post.body));
    expect(sent).toMatchObject({ id: "build", endpoint: "conn-1", workspaceRoot: "/data/ws/proj", shell: "posix", capabilities: ["exec", "sync"], group: "prod" });
  });

  it("a blank label posts the alias - the store refuses an empty one and the field says optional", async () => {
    serve([], ["default"]);
    replies["POST /api/remote/targets"] = { ok: true };
    await view.mount();
    $("rmAdd").click();
    ($("rm-id") as HTMLInputElement).value = "flash";
    ($("rm-endpoint") as HTMLSelectElement).value = "conn-1";
    ($("rm-root") as HTMLInputElement).value = "/opt/flash";
    await ($("rm-save").onclick as () => Promise<void>)();
    const sent = JSON.parse(String(requests.find((r) => r.method === "POST")!.body));
    expect(sent.id).toBe("flash");
    expect(sent.label).toBe("flash");
  });

  it("Delete confirms and deletes through the row route", async () => {
    serve([BUILD]);
    replies["DELETE /api/remote/targets/build"] = { ok: true };
    await view.mount();
    (rowOf("build").querySelector("[data-rmmore]") as HTMLElement).click();
    menuItem("Delete").click();
    await new Promise((r) => setTimeout(r, 0));
    expect(requests.some((r) => r.method === "DELETE" && r.path === "/api/remote/targets/build")).toBe(true);
  });

  it("a poll that changes nothing repaints nothing - the sheet the user types into stays put", async () => {
    serve([BUILD]);
    await view.mount();
    const r = rowOf("build");
    $("rmAdd").click();
    const dlg = $("sheet").querySelector(".sheet");
    await view.refresh();
    expect(rowOf("build")).toBe(r);
    expect($("sheet").querySelector(".sheet")).toBe(dlg);
  });
});
