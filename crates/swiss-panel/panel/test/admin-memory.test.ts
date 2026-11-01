/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// @vitest-environment happy-dom

/* The memory plugin page (#memory, SPEC §memory 11.2) against a canned /api/knowledge. The page
   is a workspace: the project rail (Global first, counts, the review badge, an unreachable
   project said aloud) and its sheets (create, a directory another project owns, add and remove a
   directory, rename, delete by typed id); export (a domain's file) and import (Check and its
   report - the source, the counts, the conflicts - then Import); the MCP conflict banner; Notes - the list beside the
   note picked from it: keyword search with its score, association with its 0-100 score and
   reasons, a new note that meets a similar one (supersede it or save anyway) or a secret; the
   detail - what a note is and says, its links by kind (walked, removed, added by search or from
   an association), a delete that needs the id typed; the Graph tab - what graphView hands the
   engine (a stand-in, graph-fake.ts), a tap that selects in place and fills the inspector,
   Center here, a double tap that opens; the Agent view's brief as text; Review's verdicts, each
   moving on to the next note waiting; semantic search - the foot saying whether meaning was
   matched, and the rail's settings sheet (the index state, the block saved into the plugin row,
   the privacy line, Test against the saved provider, Re-embed); auto-update - the head's button
   saying the daily review, its sheet (the review's switch, time, runs and workers, what waits, the
   next and last review, Review now, the shared auto sync), the Review bar's word on it. */

import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type * as Core from "../src/views/memory-core.js";
import type * as View from "../src/views/memory.js";
import * as engineModule from "../src/graph-engine.js";
import { loadLocale, setLang } from "../src/i18n.js";
import type { FakeCore } from "./graph-fake.js";
import { drawn } from "./graph-fake.js";

/* The vendored engine needs a canvas happy-dom has not got: the page draws on the stand-in,
 * and the tests read what it was told. */
vi.mock("../src/graph-engine.js", async () => {
  const { fakeEngine } = await import("./graph-fake.js");
  const fake = fakeEngine();
  return { graphEngine: fake.engine, fakeCores: fake.cores };
});
const fakeCores = (engineModule as unknown as { fakeCores: FakeCore[] }).fakeCores;

interface FakeResponse {
  status: number; ok: boolean; json: () => Promise<unknown>; blob: () => Promise<Blob>; headers: { get: (name: string) => string | null };
}
type Body = Record<string, unknown>;
interface Call { method: string; path: string; query: URLSearchParams; body: Body | null }
/** Status, body, and the response headers a download reads (lowercase names). */
type Answer = [number, unknown, Record<string, string>?];

const here = dirname(fileURLToPath(import.meta.url));

/* The shell skeleton, taken from the SERVED index.html rather than invented here (the same
 * rule as the tokens suite: the import graph wires shell controls at evaluation time). */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* ------------------------------------------------------------------ the canned store */

const NOW = Date.UTC(2026, 9, 5, 9, 0, 0);
const counts = (live: number, needsReview = 0, unconfirmed = 0, links = 0): Body => ({ live, needsReview, unconfirmed, history: 0, links });

function projects(): Body[] {
  return [
    { id: "global", name: "Global", global: true, createdMs: NOW, roots: [], counts: counts(1) },
    {
      id: "p-1", name: "swiss", global: false, createdMs: NOW, counts: counts(3, 1, 1, 2),
      roots: [{ identity: "git:4f2a", kind: "git", path: "C:/src/swiss", addedMs: NOW, reachable: true }],
    },
    { id: "p-2", name: "orders", global: false, createdMs: NOW, roots: [], counts: counts(0) },
  ];
}

function summary(id: string, title: string, o: Body = {}): Body {
  return {
    id, scope: "p-1", kind: "decision", title, status: "active", origin: "user", actor: "panel", producer: "human", transport: "panel",
    confirmed: true, sourceVerified: false, revision: 1, updatedMs: NOW - 3600_000, ageDays: 0, markers: [], linkCount: 0,
    excerpt: "why: " + title, ...o,
  };
}

function record(id: string, title: string): Body {
  const s = summary(id, title);
  delete s.excerpt;
  return {
    ...s, revision: 2, content: "Two processes share one home, so the store is SQLite.", refs: ["ADR-031"], createdMs: NOW - 86_400_000,
    sourceVerified: true,
    evidence: [
      { id: "e-0000000001", revision: 1, sourceType: "session", relation: "derived_from", sessionId: "s-1", actor: "cli", createdMs: NOW - 86_400_000, current: false },
      { id: "e-0000000002", revision: 2, sourceType: "code", relation: "verified_by", path: "crates/swiss-memory/src/store.rs", symbol: "Store::open", actor: "panel", createdMs: NOW - 3600_000, current: true },
    ],
    anchors: [{ path: "crates/swiss-memory/src/store.rs", glob: false, hash: "ab12", state: "ok" }],
    links: [
      { dir: "out", kind: "supports", id: "m-2", title: "Retry with the same key", status: "active" },
      { dir: "out", kind: "supersedes", id: "m-0", title: "redb as the store", status: "superseded" },
    ],
  };
}

const REVISIONS = [
  { revision: 1, op: "create", actor: "cli", atMs: NOW - 86_400_000, snapshot: { kind: "decision", title: "SQLite", content: "SQLite.", status: "active", confirmed: true, anchors: [], refs: [] } },
  { revision: 2, op: "edit", actor: "panel", reason: "sharpened the why", atMs: NOW - 3600_000, snapshot: { kind: "decision", title: "SQLite over redb", content: "Two processes share one home, so the store is SQLite.", status: "active", confirmed: true, anchors: ["crates/swiss-memory/src/store.rs"], refs: ["ADR-031"] } },
];

/** A note's neighbourhood (the detail's own small graph asks for one). */
function egoGraph(): Body {
  return {
    nodes: [
      { id: "m-1", kind: "decision", title: "SQLite over redb", status: "active", confirmed: true, origin: "user", degree: 2, hop: 0 },
      { id: "m-2", kind: "pitfall", title: "Retry with the same key", status: "active", confirmed: true, origin: "agent", degree: 1, hop: 1 },
      { id: "m-0", kind: "decision", title: "redb as the store", status: "superseded", confirmed: true, origin: "user", degree: 1, hop: 1 },
    ],
    edges: [
      { from: "m-1", to: "m-2", kind: "supports", directed: true },
      { from: "m-1", to: "m-0", kind: "supersedes", directed: true },
    ],
    truncated: false,
  };
}

/** A project's map: Payments with Refunds under it (drawn by an agent, not confirmed yet), and
 *  Store. */
function domain(id: string, path: string, title: string, o: Body = {}): Body {
  const depth = path.split("/").length;
  return {
    id, scope: "p-1", path, title, summary: "", owns: [], origin: "user", actor: "panel", confirmed: true, revision: 1,
    createdMs: NOW - 86_400_000, updatedMs: NOW - 3600_000, verifiedMs: NOW - 86_400_000, depth,
    parent: depth > 1 ? path.slice(0, path.lastIndexOf("/")) : undefined, subDomains: 0,
    counts: { live: 0, needsReview: 0, unconfirmed: 0 }, ...o,
  };
}

function domains(): Body[] {
  return [
    domain("d-1", "pay", "Payments", { summary: "Charging and refunds", owns: ["src/pay"], subDomains: 1, counts: { live: 2, needsReview: 1, unconfirmed: 0 } }),
    domain("d-2", "pay/refunds", "Refunds", { owns: ["src/pay/refund"], origin: "agent", actor: "codex", confirmed: false, counts: { live: 1, needsReview: 0, unconfirmed: 0 } }),
    domain("d-3", "store", "Store", { owns: ["crates/swiss-memory"] }),
  ];
}

const LATEST = { hash: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", atMs: NOW - 600_000, subject: "pay: retry the charge" };

function coverage(): Body {
  return {
    git: true,
    domains: [
      { id: "d-1", path: "pay", counts: { live: 2, needsReview: 1, unconfirmed: 0 }, files: 12, drift: 2, latest: LATEST },
      { id: "d-2", path: "pay/refunds", counts: { live: 1, needsReview: 0, unconfirmed: 0 }, files: 3, drift: 0 },
      { id: "d-3", path: "store", counts: { live: 0, needsReview: 0, unconfirmed: 0 }, files: 5, drift: 0 },
    ],
    unfiled: { live: 1, needsReview: 0, unconfirmed: 0 },
    uncovered: [{ path: "src/web", files: 4 }],
    filesTruncated: false,
  };
}

function domainDetail(id: string): Body {
  const all = domains();
  const d = all.find((x) => x.id === id) || all[0];
  const path = String(d.path);
  return {
    ...d,
    body: "## Entry points\n- `PayController`",
    ancestors: path.includes("/") ? [{ id: "d-1", path: "pay", title: "Payments" }] : [],
    children: all.filter((x) => x.parent === path),
    notes: path === "pay" ? [summary("m-1", "SQLite over redb", { domain: "pay" })] : [],
    history: [
      { revision: 1, op: "create", actor: "codex", atMs: NOW - 86_400_000, snapshot: { path, title: d.title, summary: "", body: "", owns: [], confirmed: false } },
      { revision: 2, op: "move", actor: "panel", reason: "moved with payments", atMs: NOW - 3600_000, snapshot: { path, title: d.title, summary: d.summary, body: "## Entry points\n- `PayController`", owns: d.owns, confirmed: true } },
    ],
  };
}

/** The domain routes' canned answers. */
function domainAnswer(c: Call, p: string): Answer | undefined {
  const b = c.body || {};
  const route = c.method + " " + p;
  if (route === "GET /domains") return [200, { project: c.query.get("project"), items: c.query.get("project") === "p-1" ? domains() : [], unfiled: { live: 1, needsReview: 0, unconfirmed: 0 } }];
  if (route === "GET /domains/coverage") return [200, coverage()];
  if (route === "POST /domains") return [201, { domain: domain("d-9", String(b.path), String(b.title)) }];
  if (route === "POST /domains/autofile") return [200, { filed: 2 }];
  if (route === "POST /records/file") return [200, { moved: 1, domain: b.domain ? { id: "d-1", path: b.domain, title: "Payments" } : undefined }];
  const one = /^\/domains\/([^/]+)$/.exec(p);
  if (one && c.method === "GET") return [200, { domain: domainDetail(decodeURIComponent(one[1])) }];
  if (one && c.method === "PUT") return [200, { domain: domain(decodeURIComponent(one[1]), String(b.path || "pay"), "Payments", { revision: 2 }) }];
  if (one && c.method === "DELETE") return [200, { path: c.query.get("confirm"), refiled: 1, to: "pay" }];
  if (/^\/domains\/[^/]+\/verify$/.test(p)) return [200, { domain: domain("d-1", "pay", "Payments") }];
  if (/^\/domains\/[^/]+\/confirm$/.test(p)) return [200, { domain: domain("d-1", "pay", "Payments"), domains: 1, notes: 0, held: 0 }];
  return undefined;
}

let mcpState = "serving";
let projectList: Body[] = projects();
/** The project's own settings GET /projects/{id}/settings answers (the head's Auto-update). */
const noSettings = (): Body => ({ agents: null, lead: null, embedding: null, autoReview: null, autoReviewRuns: null });
let projectSettings: Body = noSettings();
let calls: Call[] = [];
/** A test's own answers, tried before the defaults. */
/** A test's own answer; a promise holds the reply back until the test resolves it. */
let override: ((c: Call) => Answer | Promise<Answer> | undefined) | null = null;

function answer(c: Call): Answer | Promise<Answer> {
  const mine = override ? override(c) : undefined;
  if (mine) return mine;
  const p = c.path.replace(/^\/api\/knowledge/, "");
  const b = c.body || {};
  const route = c.method + " " + p;
  if (route === "GET /status") return [200, { dbPath: "C:/swiss/memory.db", schemaVersion: 2, mcp: { state: mcpState, path: "/mcp/memory" } }];
  if (route === "GET /projects") return [200, { projects: projectList }];
  if (route === "POST /projects") {
    const made = { id: "p-9", name: b.name, global: false, createdMs: NOW, roots: [], counts: counts(0) };
    projectList = [...projectList, made];
    return [201, { project: made }];
  }
  if (route === "GET /records") {
    return [200, { items: [summary("m-1", "SQLite over redb", { linkCount: 2 }), summary("m-2", "Retry with the same key", { kind: "pitfall" })], truncated: false }];
  }
  if (route === "POST /records") return [201, { record: record("m-7", String(b.title)), duplicate: false, similar: [] }];
  const one = /^\/records\/([^/]+)$/.exec(p);
  if (one && c.method === "GET") return [200, { record: record(decodeURIComponent(one[1]), "SQLite over redb"), revisions: REVISIONS }];
  if (one && c.method === "DELETE") return [200, { deleted: true }];
  if (/^\/records\/[^/]+\/(confirm|mark|restore)$/.test(p)) return [200, { record: record("m-1", "SQLite over redb") }];
  if (route === "POST /related") return [200, { items: [] }];
  if (route === "GET /search") return [200, { items: [] }];
  if (route === "POST /links") return [200, { link: { from: b.from, to: b.to, kind: b.kind, directed: true }, changed: true }];
  if (route === "DELETE /links") return [200, { changed: true }];
  if (route === "GET /review") return [200, { items: [] }];
  if (route === "GET /graph") return [200, egoGraph()];
  if (/^\/projects\/[^/]+$/.test(p) && c.method === "DELETE") return [200, { deleted: { records: 3, links: 2 } }];
  if (/^\/projects\/[^/]+$/.test(p) && c.method === "PATCH") return [200, { project: { ...projects()[1], name: b.name } }];
  if (/^\/projects\/[^/]+\/roots$/.test(p)) return [200, { project: projects()[1] }];
  if (/^\/projects\/[^/]+\/settings$/.test(p) && c.method === "GET") return [200, { settings: projectSettings }];
  const dom = domainAnswer(c, p);
  if (dom) return dom;
  return [404, { error: "memory_not_found", message: "no canned answer for " + route }];
}

function sent(method: string, path: string): Call[] {
  return calls.filter((c) => c.method === method && c.path === "/api/knowledge" + path);
}

/* ------------------------------------------------------------------ the DOM side */

let view: typeof View;
let core: typeof Core;

beforeAll(async () => {
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  const fake = (url: string, opts?: RequestInit): Promise<FakeResponse> => {
    const [path, query] = url.split("?");
    const raw = opts && typeof opts.body === "string" ? opts.body : null;
    const call: Call = { method: (opts && opts.method) || "GET", path, query: new URLSearchParams(query || ""), body: raw ? JSON.parse(raw) as Body : null };
    calls.push(call);
    return Promise.resolve(answer(call))
      .then(([status, json, headers]) => ({
        status, ok: status >= 200 && status < 300, json: () => Promise.resolve(json),
        blob: () => Promise.resolve(new Blob([typeof json === "string" ? json : JSON.stringify(json)])),
        headers: { get: (name: string) => (headers || {})[name.toLowerCase()] ?? null },
      }));
  };
  Object.assign(globalThis, { fetch: fake });
  afterAll(() => {
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  document.body.innerHTML = shellSkeleton();
  core = await import("../src/views/memory-core.js");
  view = await import("../src/views/memory.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  mcpState = "serving";
  projectList = projects();
  projectSettings = noSettings();
  calls = [];
  override = null;
  // The module's state outlives a mount (the page keeps its tab and filters across visits).
  Object.assign(core.S, {
    projects: [], project: "", status: null, tab: "notes", mode: "keyword", query: "", text: "", kind: "", statusFilter: "",
    unconfirmed: false, history: false, briefPaths: "", briefTask: "", briefGlobal: true,
    graph: { center: null, depth: 1, history: false, selected: null }, sel: null,
    domain: "", dom: null, domains: null, domainsOf: "", auto: null,
  });
});

afterEach(() => { view.unmount(); });

const settle = async (): Promise<void> => {
  for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
};

function q<T extends Element = HTMLElement>(sel: string, root: ParentNode = document): T {
  const el = root.querySelector<T>(sel);
  if (!el) throw new Error("nothing matches " + sel);
  return el;
}

function text(sel: string): string {
  const el = document.querySelector(sel);
  return el ? el.textContent || "" : "";
}

async function click(target: string | Element): Promise<void> {
  const el = typeof target === "string" ? q(target) : target;
  el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  await settle();
}

function type(sel: string, value: string): void {
  (q(sel) as HTMLInputElement).value = value;
}

async function menuPick(label: string): Promise<void> {
  const item = Array.from(document.querySelectorAll<HTMLButtonElement>(".menu.float button")).find((b) => b.textContent === label);
  if (!item) throw new Error("no menu item " + label);
  await click(item);
}

async function mounted(): Promise<void> {
  await view.mount();
  await settle();
}


/* ------------------------------------------------------------------ the rail and the projects */

describe("the Memory page - projects", () => {
  it("the page owns the pane while it is shown, and gives it back", async () => {
    await mounted();
    expect(q("#pane").classList.contains("full")).toBe(true);
    expect(q("#pane > .ws > aside#memRail + .grip.x + .ws-main > header#memHead + #memBody")).not.toBeNull();
    view.unmount();
    expect(q("#pane").classList.contains("full")).toBe(false);
  });

  it("the rail: Global first, live counts, the review badge, an unreachable project said aloud", async () => {
    await mounted();
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#memRail .side-row[data-mproject]"));
    expect(rows.map((r) => r.dataset.mproject)).toEqual(["global", "p-1", "p-2"]);
    expect(rows.map((r) => q(".side-name", r).textContent)).toEqual(["Global", "swiss", "orders"]);
    expect(rows.map((r) => q(".side-type", r).textContent)).toEqual(["1", "3", "0"]);
    // The first bound project is selected, never Global by accident.
    expect(rows[1].getAttribute("aria-selected")).toBe("true");
    expect(q(".tag.warn", rows[1]).textContent).toBe("2");
    expect(rows[0].querySelector(".tag")).toBeNull();
    expect(rows[2].querySelector(".dot.idle")).not.toBeNull();
    // The head: the project, its directory, its menu, and the tabs with their counts.
    expect(text("#memHead .ws-title")).toBe("swiss");
    expect(text("#memHead .ws-sub")).toBe("C:/src/swiss");
    expect(document.querySelector("#memHead [data-mpmenu]")).not.toBeNull();
    const tabs = Array.from(document.querySelectorAll<HTMLElement>("#memHead [data-mtab]"));
    expect(tabs.map((t) => t.dataset.mtab)).toEqual(["domains", "notes", "review", "graph", "agent"]);
    expect(tabs.map((t) => t.querySelector(".seg-n")?.textContent ?? null)).toEqual(["3", "3", "2", "2", null]);

    await click(rows[2]);
    expect(document.querySelector("#memHead [data-munreach]")?.textContent).toBe("Agents can't reach this project yet");
    // Global is the shared scope: no rename, no delete.
    await click("[data-mproject=global]");
    expect(document.querySelector("[data-mpmenu]")).toBeNull();
    expect(core.S.project).toBe("global");
  });

  it("a pick of a tab or a project reads the counts again: notes arrive from outside the panel", async () => {
    await mounted();
    const tail = (id: string): string => text(`[data-mproject=${id}] .side-type`);
    expect(tail("p-1")).toBe("3");
    // The CLI (or an agent, or a sync) writes behind the page's back.
    projectList = projects().map((p) => p.id === "p-1" ? { ...p, counts: counts(199, 0, 199, 4) } : p);
    await click("[data-mtab=review]");
    expect(tail("p-1")).toBe("199");
    const n = (tab: string): string | null => q(`#memHead [data-mtab=${tab}]`).querySelector(".seg-n")?.textContent ?? null;
    expect([n("notes"), n("review"), n("graph")]).toEqual(["199", "199", "4"]);
    projectList = projects().map((p) => p.id === "p-2" ? { ...p, counts: counts(7) } : p);
    await click("[data-mproject=p-2]");
    expect(tail("p-2")).toBe("7");
    expect(n("notes")).toBe("7");
  });

  it("New project sends the name, the directory and exact, then selects what it made", async () => {
    await mounted();
    await click("[data-mnewproject]");
    type("#memPName", "orders-api");
    type("#memPPath", "C:/src/orders/api");
    (q("#memPExact") as HTMLInputElement).checked = true;
    await click("#memPSave");
    const posts = sent("POST", "/projects");
    expect(posts.length).toBe(1);
    expect(posts[0].body).toEqual({ name: "orders-api", path: "C:/src/orders/api", exact: true });
    expect(core.S.project).toBe("p-9");
    expect(q("[data-mproject=p-9]").getAttribute("aria-selected")).toBe("true");
    expect(text("#toast")).toBe("Project orders-api created");
  });

  it("a directory another project owns offers the way to that project", async () => {
    await mounted();
    await click("[data-mproject=p-2]");
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/projects"
      ? [409, { error: "memory_root_bound", message: "C:/src/swiss already belongs to swiss.", projectId: "p-1" }]
      : undefined;
    await click("[data-mnewproject]");
    type("#memPName", "swiss again");
    type("#memPPath", "C:/src/swiss");
    await click("#memPSave");
    expect(text("#memPErr")).toBe("C:/src/swiss already belongs to swiss.");
    const go = q("#memPOffer [data-mswitch=p-1]");
    expect(go.textContent).toBe("Go to swiss");
    await click(go);
    expect(core.S.project).toBe("p-1");
    expect(q("[data-mproject=p-1]").getAttribute("aria-selected")).toBe("true");
  });

  it("the project menu: add and remove a directory, rename", async () => {
    await mounted();
    await click("[data-mpmenu]");
    await menuPick("Add directory…");
    type("#memPPath", "D:/work/swiss-web");
    await click("#memPSave");
    expect(sent("POST", "/projects/p-1/roots").map((c) => c.body)).toEqual([{ path: "D:/work/swiss-web" }]);

    await click("[data-mpmenu]");
    await menuPick("Remove directory…");
    await click("#sheet [data-mrootdel]");
    const del = sent("DELETE", "/projects/p-1/roots");
    expect(del.length).toBe(1);
    expect(del[0].query.get("identity")).toBe("git:4f2a");

    await click("[data-mpmenu]");
    await menuPick("Rename…");
    type("#g-name", "swiss-gateway");
    await click("#g-save");
    expect(sent("PATCH", "/projects/p-1").map((c) => c.body)).toEqual([{ name: "swiss-gateway" }]);
  });

  it("deleting a project needs its id typed, and then sends it as confirm", async () => {
    await mounted();
    await click("[data-mpmenu]");
    await menuPick("Delete project…");
    type("#g-name", "swiss");
    await click("#g-save");
    expect(text("#g-err")).toBe("That is not this project's id - nothing was deleted.");
    expect(sent("DELETE", "/projects/p-1").length).toBe(0);
    type("#g-name", "p-1");
    await click("#g-save");
    const del = sent("DELETE", "/projects/p-1");
    expect(del.length).toBe(1);
    expect(del[0].query.get("confirm")).toBe("p-1");
    expect(text("#toast")).toBe("swiss deleted: 3 notes, 2 links");
    expect(core.S.project).toBe("global");
  });

  it("the MCP conflict banner names the fix, and only when the endpoint is shadowed", async () => {
    await mounted();
    expect(document.getElementById("memMcpBanner")).toBeNull();
    view.unmount();
    mcpState = "conflicted";
    await mounted();
    const banner = q("#memMcpBanner");
    expect(banner.textContent).toContain("/mcp/memory");
    expect(banner.textContent).toContain("Rename your memory MCP, then restart the Memory plugin.");
  });
});

/* ------------------------------------------------------------------ export and import */

describe("the Memory page - export and import", () => {
  it("Export sends the domain picked and saves the file under the name the route gives", async () => {
    const saved: string[] = [];
    const prev = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
    Object.assign(URL, { createObjectURL: vi.fn(() => "blob:memory"), revokeObjectURL: vi.fn() });
    const anchor = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) { saved.push(this.download); });
    override = (c) => c.path === "/api/knowledge/export"
      ? [200, "{\"type\":\"header\"}\n", { "content-disposition": "attachment; filename=\"memory-p-1-pay-refunds.jsonl\"" }]
      : undefined;
    try {
      await mounted();
      await click("[data-mpmenu]");
      await menuPick("Export…");
      q<HTMLSelectElement>("#memXferWhat").value = "pay/refunds";
      await click("#memXferExport");
      expect(sent("GET", "/export").map((c) => [c.query.get("project"), c.query.get("domain")])).toEqual([["p-1", "pay/refunds"]]);
      expect(saved).toEqual(["memory-p-1-pay-refunds.jsonl"]);
      expect(text("#toast")).toBe("Download started: memory-p-1-pay-refunds.jsonl");
    } finally {
      anchor.mockRestore();
      Object.assign(URL, { createObjectURL: prev.create, revokeObjectURL: prev.revoke });
    }
  });

  it("Import checks first - where the file came from, what it adds, each conflict - and then imports", async () => {
    const side = (id: string, revision: number, excerpt: string): Body => ({ id, revision, title: "Refunds retry twice", status: "active", confirmed: false, updatedMs: NOW, excerpt });
    const report = (dryRun: boolean): Body => ({
      dryRun, into: "p-1", projectsCreated: [], domainsCreated: 1, domainsSkipped: [], recordsCreated: 1, recordsSkipped: ["m-5"], linksCreated: 2, linksSkipped: 1,
      source: { formatVersion: 4, exportedMs: NOW, exporter: "swiss-memory 0.9.0", scope: "domain", project: { id: "p-7", name: "shop" }, domain: "pay", visibility: "project", notes: 3, confirmed: 1, linksLeftOut: 1 },
      conflicts: [{ kind: "title", line: 4, incoming: side("m-6", 1, "Then a person looks."), stored: side("m-2", 3, "Three times, here.") }],
    });
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/import" ? [200, report(c.body?.dryRun === true)] : undefined;
    await mounted();
    await click("[data-mpmenu]");
    await menuPick("Import…");
    await click("#memXferCheck");
    expect(text("#memXferErr")).toBe("Pick a file first.");
    expect(sent("POST", "/import").length).toBe(0);

    const file = "{\"type\":\"header\",\"formatVersion\":4}\n";
    const input = q<HTMLInputElement>("#memXferFile");
    Object.defineProperty(input, "files", { value: [{ name: "pay.jsonl", text: () => Promise.resolve(file) }] });
    input.dispatchEvent(new Event("change"));
    await settle();
    await click("#memXferCheck");
    expect(sent("POST", "/import").map((c) => c.body)).toEqual([{ project: "p-1", text: file, dryRun: true }]);
    expect(text("#memXferSource")).toContain("From domain pay of shop · 3 notes, 1 confirmed by a person · 1 links left out");
    expect(text("#memXferCounts")).toBe("Would add 1 notes, 1 domains and 2 links; 1 notes are already here.");
    const conflicts = text("#memXferConflicts");
    expect(conflicts).toContain("Refunds retry twice");
    expect(conflicts).toContain("a note here (m-2) has this title with other text");
    expect(conflicts).toContain("In the file: Then a person looks.");
    expect(conflicts).toContain("Here: Three times, here.");
    expect(q("#memXferCheck").hidden).toBe(true);

    await click("#memXferImport");
    expect(sent("POST", "/import").map((c) => c.body?.dryRun)).toEqual([true, false]);
    expect(text("#toast")).toBe("Imported 1 notes; 1 conflicts kept out");
    expect(q("#sheet").hidden).toBe(true);
  });
});

/* ------------------------------------------------------------------ domains: the map */

describe("the Memory page - domains", () => {
  async function domainsTab(): Promise<void> {
    await mounted();
    await click("[data-mtab=domains]");
  }

  it("the map: a tree with each domain's notes, review, unconfirmed mark and drift; then Unfiled and Not covered", async () => {
    await domainsTab();
    expect(sent("GET", "/domains/coverage")[0].query.get("project")).toBe("p-1");
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#memDomList .pick-row[data-mdom]"));
    expect(rows.map((r) => [r.dataset.mdom, r.className])).toEqual([["d-1", "pick-row"], ["d-2", "pick-row d1"], ["d-3", "pick-row"]]);
    expect(rows.map((r) => q(".pick-meta", r).textContent)).toEqual(["2", "1", "0"]);
    expect(Array.from(rows[0].querySelectorAll(".tag.warn")).map((t) => t.textContent)).toEqual(["1 to review", "2 commits since verified"]);
    expect(q(".pick-sub", rows[0]).textContent).toContain("Charging and refunds");
    expect(q(".pick-sub", rows[1]).textContent).toContain("unconfirmed");
    const more = Array.from(document.querySelectorAll<HTMLElement>("#memDomMore .pick-row[data-mdom]"));
    expect(more.map((r) => [r.dataset.mdom, q(".pick-meta", r).textContent])).toEqual([[":unfiled", "1"], [":uncovered", "1"]]);
    expect(text("#memDomFoot")).toBe("3 domains");
    // The first domain opens beside the map; the arrows walk on into Unfiled and Not covered.
    expect(rows[0].getAttribute("aria-selected")).toBe("true");
    rows[2].dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await settle();
    expect(core.S.dom).toBe(":unfiled");
    expect(q('#memDomMore [data-mdom=":unfiled"]').getAttribute("aria-selected")).toBe("true");
  });

  it("a map goes five levels deep: each level a step further in, and the fifth takes no sub-domain", async () => {
    const chain = ["a", "a/b", "a/b/c", "a/b/c/d", "a/b/c/d/e"]
      .map((p, i) => domain("d-c" + String(i + 1), p, "Level " + String(i + 1), { subDomains: i < 4 ? 1 : 0 }));
    override = (c) => {
      if (c.method !== "GET") return undefined;
      if (c.path === "/api/knowledge/domains") return [200, { project: "p-1", items: chain, unfiled: { live: 0, needsReview: 0, unconfirmed: 0 } }];
      const d = chain.find((x) => c.path === "/api/knowledge/domains/" + String(x.id));
      return d ? [200, { domain: { ...d, body: "", ancestors: [], children: chain.filter((x) => x.parent === d.path), notes: [], history: [] } }] : undefined;
    };
    await domainsTab();
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#memDomList .pick-row[data-mdom]"));
    expect(rows.map((r) => r.className)).toEqual(["pick-row", "pick-row d1", "pick-row d2", "pick-row d3", "pick-row d4"]);
    // The fourth level still takes a sub-domain; the fifth, the deepest a map goes, does not.
    await click('#memDomList [data-mdom="d-c4"]');
    expect(q("#memDomDetail article[data-mdomdoc=d-c4] [data-mdact=newsub]").getAttribute("aria-label")).toBe("New sub-domain");
    expect(q("#memDomDetail article[data-mdomdoc=d-c4] [data-mdact=newsub] use").getAttribute("href")).toBe("#i-plus");
    await click('#memDomList [data-mdom="d-c5"]');
    expect(q("#memDomDetail article[data-mdomdoc=d-c5]").querySelector("[data-mdact=newsub]")).toBeNull();
  });

  it("the map and the pick paint before coverage answers; drift and Not covered fill in when it does", async () => {
    let land: (a: Answer) => void = () => undefined;
    override = (c) => c.path === "/api/knowledge/domains/coverage" ? new Promise<Answer>((r) => { land = r; }) : undefined;
    await domainsTab();
    const warns = (sel: string): string[] => Array.from(q(sel).querySelectorAll(".tag.warn")).map((t) => t.textContent || "");
    expect(Array.from(document.querySelectorAll<HTMLElement>("#memDomList .pick-row[data-mdom]")).map((r) => r.dataset.mdom))
      .toEqual(["d-1", "d-2", "d-3"]);
    expect(warns('#memDomList [data-mdom="d-1"]')).toEqual(["1 to review"]);
    expect(warns("#memDomDetail article[data-mdomdoc=d-1] .doc-head")).toEqual([]);
    expect(text('#memDomMore [data-mdom=":uncovered"] .pick-meta')).toBe("—");
    q('#memDomList [data-mdom="d-2"]').focus();
    land([200, coverage()]);
    await settle();
    expect(warns('#memDomList [data-mdom="d-1"]')).toEqual(["1 to review", "2 commits since verified"]);
    expect(warns("#memDomDetail article[data-mdomdoc=d-1] .doc-head")).toEqual(["2 commits since verified"]);
    expect(text('#memDomMore [data-mdom=":uncovered"] .pick-meta')).toBe("1");
    // The repaint keeps the row a keyboard user stood on.
    expect((document.activeElement as HTMLElement).dataset.mdom).toBe("d-2");
  });

  it("Not covered waits for coverage; an answer a newer read overtook paints nothing", async () => {
    const lands: Array<(a: Answer) => void> = [];
    override = (c) => c.path === "/api/knowledge/domains/coverage" ? new Promise<Answer>((r) => { lands.push(r); }) : undefined;
    await domainsTab();
    await click('#memDomMore [data-mdom=":uncovered"]');
    expect(text("#memDomDetail")).toContain("Reading the checkout and its history…");
    const mod = await import("../src/views/memory-domains.js");
    await mod.refreshDomains();
    expect(lands.length).toBe(2);
    lands[0]([200, { git: false, issue: "git_failed", reason: "stale", domains: [], unfiled: { live: 1, needsReview: 0, unconfirmed: 0 }, uncovered: [], filesTruncated: false }]);
    await settle();
    expect(text("#memDomFoot")).toBe("3 domains");
    expect(text("#memDomDetail")).toContain("Reading the checkout and its history…");
    lands[1]([200, coverage()]);
    await settle();
    expect(q("#memDomDetail button.ref[data-mpvfile='src/web']").textContent).toBe("src/web/");
    expect(text('#memDomMore [data-mdom=":uncovered"] .pick-meta')).toBe("1");
  });

  it("a domain reads: where it sits, its hub as Markdown, the code it owns, sub-domains, notes, history", async () => {
    await domainsTab();
    const doc = q("#memDomDetail article[data-mdomdoc=d-1]");
    expect(sent("GET", "/domains/d-1")[0].query.get("history")).toBe("true");
    expect(q(".doc-title", doc).textContent).toBe("Payments");
    // The summary is the head's lede, not a line between the head and the hub.
    expect(q(".doc-head > p.doc-lede", doc).textContent).toBe("Charging and refunds");
    expect(q(".md.note-body", doc).textContent).toContain("PayController");
    expect(q("[data-mddrift]", doc).textContent).toContain("2 commits touched this domain's code since the hub was verified.");
    expect(q("[data-mddrift] button.ref", doc).dataset.mpvcommit).toBe("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678");
    expect(q("button.ref[data-mpvfile='src/pay']", doc)).not.toBeNull();
    expect(doc.textContent).toContain("12 files");
    expect(q("button.ref[data-mdom=d-2]", doc).textContent).toBe("Refunds");
    expect(q("button.ref[data-mopen=m-1]", doc).textContent).toBe("SQLite over redb");
    expect(q("#memDomHistFold > summary", doc).textContent).toBe("2 revisions");
    expect(q("#memDomHistFold", doc).textContent).toContain("moved with payments");
    // A sub-domain is a step down, its crumbs a step back up.
    await click(q("button.ref[data-mdom=d-2]", doc));
    const sub = q("#memDomDetail article[data-mdomdoc=d-2]");
    expect(q("#memDomList [data-mdom=d-2]").getAttribute("aria-selected")).toBe("true");
    expect(q("[data-mdcrumbs]", sub).textContent).toBe("Payments / pay/refunds");
    expect(sub.querySelector(".doc-lede")).toBeNull();
    // An agent drew it: Confirm is offered; Verified says the hub is true to the code again.
    await click(q("[data-mdact=more]", sub));
    await menuPick("Confirm");
    expect(sent("POST", "/domains/d-2/confirm").map((c) => c.body)).toEqual([{ project: "p-1", all: true }]);
    expect(text("#toast")).toBe("Domain confirmed");
    await click("#memDomDetail [data-mdact=more]");
    await menuPick("Verified");
    expect(sent("POST", "/domains/d-2/verify").map((c) => c.body)).toEqual([{ project: "p-1" }]);
    expect(text("#toast")).toBe("Marked verified");
  });

  it("Confirm all takes the domain whole - the domains under it and their notes - and says what still waits", async () => {
    override = (c) => {
      if (c.method === "GET" && c.path === "/api/knowledge/domains") {
        const items = domains().map((d) => d.id === "d-2" ? { ...d, counts: { live: 4, needsReview: 1, unconfirmed: 4 } } : d);
        return [200, { project: "p-1", items, unfiled: { live: 0, needsReview: 0, unconfirmed: 0 } }];
      }
      if (c.method === "POST" && c.path === "/api/knowledge/domains/d-1/confirm") {
        return [200, { domain: domain("d-1", "pay", "Payments"), domains: 1, notes: 3, held: 1 }];
      }
      return undefined;
    };
    await domainsTab();
    await click("#memDomDetail article[data-mdomdoc=d-1] [data-mdact=more]");
    const all = [...document.querySelectorAll<HTMLButtonElement>(".menu.float button")].find((b) => b.textContent === "Confirm all");
    expect(all?.title).toBe("Confirm this domain, the domains under it and their 4 unconfirmed notes; a note that needs review waits for its verdict");
    await menuPick("Confirm all");
    expect(sent("POST", "/domains/d-1/confirm").map((c) => c.body)).toEqual([{ project: "p-1", all: true }]);
    expect(text("#toast")).toBe("Domain confirmed with 3 notes · 1 that needs review waits in Review");
    // Nothing left to confirm under a domain: no confirm at all.
    await click("#memDomList [data-mdom=d-3]");
    await click("#memDomDetail article[data-mdomdoc=d-3] [data-mdact=more]");
    expect([...document.querySelectorAll(".menu.float button")].some((b) => b.textContent === "Confirm" || b.textContent === "Confirm all")).toBe(false);
  });

  it("New domain sends the path, title, summary, owns and hub; a taken path is said in the sheet", async () => {
    await domainsTab();
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/domains" && c.body && c.body.path === "pay"
      ? [409, { error: "memory_domain_exists", message: "domain pay already exists" }]
      : undefined;
    await click("[data-mdnew]");
    type("#memDomPath", "pay");
    type("#memDomTitle", "Payments again");
    await click("#memDomSave");
    expect(text("#memDomEdErr")).toBe("domain pay already exists");
    type("#memDomPath", "web/");
    type("#memDomTitle", "Web");
    type("#memDomSummary", "The storefront");
    type("#memDomOwns", "src/web\n\n web/static ");
    type("#memDomBody", "## Overview\nThe shop.");
    await click("#memDomSave");
    expect(sent("POST", "/domains")[1].body).toEqual({
      project: "p-1", path: "web", title: "Web", owns: ["src/web", "web/static"], summary: "The storefront", body: "## Overview\nThe shop.",
    });
    expect(text("#toast")).toBe("Domain web created");
  });

  it("Edit sends only what changed, with the revision it read; a moved revision offers a reload", async () => {
    await domainsTab();
    await click("#memDomDetail [data-mdact=edit]");
    expect((q("#memDomPath") as HTMLInputElement).value).toBe("pay");
    expect((q("#memDomBody") as HTMLTextAreaElement).value).toBe("## Entry points\n- `PayController`");
    type("#memDomSummary", "Charging, refunds, payouts");
    await click("#memDomSave");
    expect(sent("PUT", "/domains/d-1").map((c) => c.body)).toEqual([{ project: "p-1", expectedRevision: 1, summary: "Charging, refunds, payouts" }]);

    override = (c) => c.method === "PUT" ? [409, { error: "memory_revision_conflict", message: "moved", current: 3 }] : undefined;
    await click("#memDomDetail [data-mdact=edit]");
    type("#memDomTitle", "Money");
    await click("#memDomSave");
    expect(text("#memDomEdConflict")).toContain("3");
    expect(q("#memDomEdConflict [data-mdreload]")).not.toBeNull();
  });

  it("Move sends the new path; delete needs the path typed and says where the notes went", async () => {
    await domainsTab();
    await click("#memDomDetail [data-mdact=more]");
    await menuPick("Move…");
    type("#g-name", "billing");
    await click("#g-save");
    expect(sent("PUT", "/domains/d-1").map((c) => c.body)).toEqual([{ project: "p-1", expectedRevision: 1, path: "billing" }]);
    expect(text("#toast")).toBe("Moved to billing");

    await click("#memDomList [data-mdom=d-2]");
    await click("#memDomDetail [data-mdact=more]");
    await menuPick("Delete…");
    type("#g-name", "pay");
    await click("#g-save");
    expect(sent("DELETE", "/domains/d-2").length).toBe(0);
    type("#g-name", "pay/refunds");
    await click("#g-save");
    expect(sent("DELETE", "/domains/d-2").map((c) => Object.fromEntries(c.query))).toEqual([{ project: "p-1", confirm: "pay/refunds" }]);
    expect(text("#toast")).toBe("Deleted pay/refunds; 1 note moved to pay");
  });

  it("Unfiled lists the notes no domain holds: File… files one, File by anchors the lot", async () => {
    override = (c) => c.path === "/api/knowledge/records" && c.query.get("domain") === "none"
      ? [200, { items: [summary("m-5", "Loose note")], truncated: false }]
      : undefined;
    await domainsTab();
    await click('#memDomMore [data-mdom=":unfiled"]');
    const doc = q('#memDomDetail article[data-mdomdoc=":unfiled"]');
    expect(q(".doc-head > .doc-lede", doc).textContent).toContain("let its anchors decide");
    expect(doc.textContent).toContain("Loose note");
    await click(q("[data-mdfile=m-5]", doc));
    const pick = q("#memFileTo") as HTMLSelectElement;
    expect(Array.from(pick.options).map((o) => o.value)).toEqual(["none", "pay", "pay/refunds", "store"]);
    // A sub-domain sits under its parent.
    expect(pick.options[2].textContent).toBe("\u00a0\u00a0Refunds · pay/refunds");
    pick.value = "pay/refunds";
    await click("#memFileSave");
    expect(sent("POST", "/records/file").map((c) => c.body)).toEqual([{ project: "p-1", ids: ["m-5"], domain: "pay/refunds" }]);
    expect(text("#toast")).toBe("Filed in pay/refunds");
    await click("#memDomDetail [data-mdauto]");
    expect(sent("POST", "/domains/autofile").map((c) => c.body)).toEqual([{ project: "p-1" }]);
    expect(text("#toast")).toBe("2 notes filed by their anchors");
  });

  it("Not covered: each directory one click from a new domain owning it", async () => {
    await domainsTab();
    await click('#memDomMore [data-mdom=":uncovered"]');
    const doc = q('#memDomDetail article[data-mdomdoc=":uncovered"]');
    expect(q(".doc-head > .doc-lede", doc).textContent).toContain("Code no domain owns");
    expect(q("button.ref[data-mpvfile='src/web']", doc).textContent).toBe("src/web/");
    expect(doc.textContent).toContain("4 files");
    await click(q("[data-mdown='src/web']", doc));
    expect((q("#memDomPath") as HTMLInputElement).value).toBe("web");
    expect((q("#memDomOwns") as HTMLTextAreaElement).value).toBe("src/web");
  });

  it("without Git, drift and Not covered say why in the reader's words and offer the cure", async () => {
    override = (c) => c.path === "/api/knowledge/domains/coverage"
      ? [200, { git: false, issue: "not_a_repository", reason: "the project's directory is not a git checkout", domains: [], unfiled: { live: 1, needsReview: 0, unconfirmed: 0 }, uncovered: [], filesTruncated: false }]
      : undefined;
    await domainsTab();
    expect(text("#memDomFoot")).toBe("3 domains · not a Git repository");
    expect(q("#memDomFoot span").title).toContain("Point the project at its Git checkout");
    expect(text('#memDomMore [data-mdom=":uncovered"] .pick-meta')).toBe("—");
    expect(text('#memDomMore [data-mdom=":uncovered"] .pick-sub')).toBe("not a Git repository");
    await click('#memDomMore [data-mdom=":uncovered"]');
    const why = text("#memDomDetail [data-mdnogit]");
    expect(why).toContain("The project's directory is not a Git repository.");
    expect(why).not.toContain("git checkout");
    await click("#memDomDetail [data-mdchangedir]");
    expect(text(".sheet-head h2")).toBe("Change a directory");
    expect((q("#memPRoot") as HTMLSelectElement).value).toBe("git:4f2a");
  });

  it("a git failure nothing better explains shows git's own line, and no cure to offer", async () => {
    override = (c) => c.path === "/api/knowledge/domains/coverage"
      ? [200, { git: false, issue: "git_failed", reason: "git took longer than 10 s", domains: [], unfiled: { live: 1, needsReview: 0, unconfirmed: 0 }, uncovered: [], filesTruncated: false }]
      : undefined;
    await domainsTab();
    expect(text("#memDomFoot")).toBe("3 domains · Git failed");
    await click('#memDomMore [data-mdom=":uncovered"]');
    expect(text("#memDomDetail [data-mdnogit]")).toBe("Git couldn't read the history: git took longer than 10 s");
    expect(document.querySelector("#memDomDetail [data-mdchangedir]")).toBeNull();
    const core = await import("../src/views/memory-core.js");
    expect(core.errMessage({ error: "memory_git_unavailable", message: "the project's directory is not a git checkout", issue: "not_a_repository" }))
      .toContain("not a Git repository");
    expect(core.errMessage({ error: "memory_not_found", message: "no such commit in this checkout" })).toBe("no such commit in this checkout");
  });

  it("a directory's last name is the new domain's path", async () => {
    const mod = await import("../src/views/memory-domains.js");
    expect(["src/web", "Src/My Web!/", "packages/_internal", ""].map(mod.pathSuggestion)).toEqual(["web", "my-web", "internal", ""]);
  });

  it("Notes: the domain filter asks for a domain's notes; a note's domain steps to the map; Move files it elsewhere", async () => {
    override = (c) => c.method === "GET" && c.path === "/api/knowledge/records/m-1"
      ? [200, { record: { ...record("m-1", "SQLite over redb"), domain: "pay/refunds" }, revisions: REVISIONS }]
      : undefined;
    await mounted();
    const sel = q("#memDomain") as HTMLSelectElement;
    expect(Array.from(sel.options).map((o) => o.value)).toEqual(["", "none", "pay", "pay/refunds", "store"]);
    // Kind and status share a line; the domain, a path, has one of its own.
    expect(sel.closest(".ws-bar-row")?.querySelectorAll("select").length).toBe(1);
    expect(q("#memKind").closest(".ws-bar-row")?.contains(q("#memStatus"))).toBe(true);
    sel.value = "pay";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
    await settle();
    const lists = sent("GET", "/records");
    expect(lists[lists.length - 1].query.get("domain")).toBe("pay");

    const doc = q("#memDetail article[data-mdoc=m-1]");
    expect(q("button.ref[data-mdomopen]", doc).textContent).toBe("pay/refunds");
    await click(q("[data-mact=more]", doc));
    await menuPick("Move to a domain…");
    const to = q("#memFileTo") as HTMLSelectElement;
    expect(to.value).toBe("pay/refunds");
    to.value = "none";
    await click("#memFileSave");
    expect(sent("POST", "/records/file").map((c) => c.body)).toEqual([{ project: "p-1", ids: ["m-1"], domain: null }]);
    expect(text("#toast")).toBe("Now unfiled");

    // The domain line walks to the map, that domain picked.
    await click("#memDetail button.ref[data-mdomopen]");
    expect(core.S.tab).toBe("domains");
    expect(q("#memDomList [data-mdom=d-2]").getAttribute("aria-selected")).toBe("true");
  });

  it("a new note is filed in the domain picked, else by its anchors", async () => {
    await mounted();
    await click("[data-mnewnote]");
    const pick = q("#memEdDomain") as HTMLSelectElement;
    expect([pick.value, pick.options[0].textContent]).toEqual(["", "By its anchors"]);
    type("#memEdTitle", "Refunds settle next day");
    type("#memEdContent", "Why: the bank batches.");
    pick.value = "pay/refunds";
    await click("#memEdSave");
    expect(sent("POST", "/records")[0].body).toMatchObject({ title: "Refunds settle next day", domain: "pay/refunds" });
    // From a domain's page, New note here starts in that domain.
    await click("[data-mtab=domains]");
    await click("#memDomDetail [data-mdact=newnote]");
    expect((q("#memEdDomain") as HTMLSelectElement).value).toBe("pay");
  });

  it("the global scope has no map: no Domains tab, no domain filter", async () => {
    await mounted();
    await click("[data-mproject=global]");
    expect(q("[data-mtab=domains]").hidden).toBe(true);
    expect(document.querySelector("#memDomain")).toBeNull();
    expect(sent("GET", "/domains").every((c) => c.query.get("project") !== "global")).toBe(true);
  });

  it("Change a directory sends the identity, the new path and exact; one another project owns offers it", async () => {
    await mounted();
    await click("[data-mpmenu]");
    await menuPick("Change a directory…");
    expect((q("#memPRoot") as HTMLSelectElement).value).toBe("git:4f2a");
    type("#memPPath", "D:/moved/swiss");
    await click("#memPSave");
    expect(sent("PUT", "/projects/p-1/roots").map((c) => c.body)).toEqual([{ identity: "git:4f2a", path: "D:/moved/swiss", exact: false }]);
    expect(text("#toast")).toBe("Directory changed");

    override = (c) => c.method === "PUT" ? [409, { error: "memory_root_bound", message: "D:/orders already belongs to orders.", projectId: "p-2" }] : undefined;
    await click("[data-mpmenu]");
    await menuPick("Change a directory…");
    type("#memPPath", "D:/orders");
    await click("#memPSave");
    expect(text("#memPErr")).toBe("D:/orders already belongs to orders.");
    expect(q("#memPOffer [data-mswitch=p-2]").textContent).toBe("Go to orders");
  });
});

/* ------------------------------------------------------------------ notes: the list column */

describe("the Memory page - notes", () => {
  it("the list asks for the project's notes; a row is the kind's swatch, the title, why it is here", async () => {
    await mounted();
    const list = sent("GET", "/records");
    expect(list.length).toBe(1);
    expect(list[0].query.get("project")).toBe("p-1");
    expect(list[0].query.get("limit")).toBe("200");
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#memList button.pick-row[data-mnote]"));
    expect(rows.map((r) => r.dataset.mnote)).toEqual(["m-1", "m-2"]);
    expect(q(".pick-name", rows[0]).textContent).toBe("SQLite over redb");
    // The kind is the swatch, its word the swatch's name.
    expect(q(".pick-lead .cat.bare", rows[0]).classList.contains("c1")).toBe(true);
    expect(q(".pick-lead .cat", rows[0]).getAttribute("aria-label")).toBe("decision");
    expect(q(".pick-lead .cat", rows[1]).classList.contains("c3")).toBe(true);
    expect(q(".pick-sub", rows[0]).textContent).toContain("why: SQLite over redb");
    expect(text("#memListFoot")).toBe("2 notes");
    // Nothing picked yet: the first note opens beside the list.
    expect(rows[0].getAttribute("aria-selected")).toBe("true");
    expect(q("#memDetail article").dataset.mdoc).toBe("m-1");
  });

  it("a row click, or the arrows, move the mark in place and the detail follows", async () => {
    await mounted();
    const first = q("#memList [data-mnote=m-1]");
    await click("#memList [data-mnote=m-2]");
    // The same rows: the mark moved, the list was not repainted.
    expect(q("#memList [data-mnote=m-1]")).toBe(first);
    expect(first.getAttribute("aria-selected")).toBe("false");
    expect(q("#memList [data-mnote=m-2]").getAttribute("aria-selected")).toBe("true");
    expect(q("#memDetail article").dataset.mdoc).toBe("m-2");

    q("#memList [data-mnote=m-2]").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }));
    await settle();
    expect(first.getAttribute("aria-selected")).toBe("true");
    expect(q("#memDetail article").dataset.mdoc).toBe("m-1");
  });

  it("keyword search goes to /search and shows each hit's score", async () => {
    override = (c) => c.path === "/api/knowledge/search"
      ? [200, { items: [{ ...summary("m-3", "ADR-031 rationale"), score: 12.5, tier: "title" }] }]
      : undefined;
    await mounted();
    type("#memQ", "ADR-031");
    q("#memQ").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    const s = sent("GET", "/search");
    expect(s.length).toBe(1);
    expect(s[0].query.get("q")).toBe("ADR-031");
    expect(s[0].query.get("project")).toBe("p-1");
    const row = q("#memList [data-mnote=m-3]");
    expect(q("[data-mscore]", row).textContent).toBe("12.50");
    expect(row.textContent).toContain("ADR-031 rationale");
  });

  it("a search says whether it matched on meaning too, and why not when it could not", async () => {
    let semantic: Body = { state: "on", model: "openai:bge-m3:native" };
    override = (c) => c.path === "/api/knowledge/search"
      ? [200, { items: [{ ...summary("m-3", "Retry storms"), score: 640, tier: "semantic", similarity: 0.812 }], semantic }]
      : undefined;
    await mounted();
    type("#memQ", "back-off");
    q("#memQ").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    const on = q("#memListFoot [data-msemantic=on]");
    expect(on.textContent).toBe("+ meaning");
    expect(on.title).toBe("Matched on words and on meaning (openai:bge-m3:native)");
    expect(text("#memListFoot")).toBe("1 note · + meaning");
    expect(q("#memList [data-mnote=m-3] .pick-meta").title).toBe("Search score - matched on meaning · similarity 0.81");

    semantic = { state: "unavailable", model: "openai:bge-m3:native", reason: "no answer within 5000 ms" };
    type("#memQ", "back off");
    q("#memQ").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    const tag = q("#memListFoot [data-msemantic=unavailable] .tag.warn");
    expect(tag.textContent).toBe("words only");
    expect(tag.title).toBe("Semantic search did not run: no answer within 5000 ms");

    // Off says nothing: the foot is the count alone.
    semantic = { state: "off" };
    type("#memQ", "backoff");
    q("#memQ").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    expect(text("#memListFoot")).toBe("1 note");
  });

  it("Associate scores a passage 0-100 and says why each note matched", async () => {
    override = (c) => c.path === "/api/knowledge/related" && c.body && "text" in c.body
      ? [200, { items: [{
          ...summary("m-2", "Retry with the same key", { kind: "pitfall" }), score: 87.4,
          reasons: [
            { type: "link", kind: "supports", hops: 1, via: "m-1" }, { type: "text", terms: ["retry", "idempotent"] },
            { type: "anchor", paths: ["src/orders.rs"] }, { type: "similar", similarity: 0.7134 },
          ],
        }] }]
      : undefined;
    await mounted();
    await click("[data-mmode=associate]");
    // No passage yet: the list says what to do, nothing is asked.
    expect(text("#memList")).toContain("Associate a passage");
    type("#memText", "make the order retry idempotent");
    await click("[data-massoc]");
    const rel = sent("POST", "/related").filter((c) => c.body && "text" in c.body);
    expect(rel.length).toBe(1);
    expect(rel[0].body).toEqual({ project: "p-1", text: "make the order retry idempotent", limit: 50 });
    const row = q("#memList [data-mnote=m-2]");
    expect(q("[data-mscore]", row).textContent).toBe("87/100");
    expect(q("[data-mreason=link]", row).textContent).toBe("supports link via m-1");
    expect(q("[data-mreason=text]", row).textContent).toBe("shares retry, idempotent");
    expect(q("[data-mreason=anchor]", row).textContent).toBe("same files src/orders.rs");
    expect(q("[data-mreason=similar]", row).textContent).toBe("close in meaning (0.71)");
  });

  it("a new note that meets a similar one: Save anyway, or Supersede it", async () => {
    override = (c) => {
      if (c.method !== "POST" || c.path !== "/api/knowledge/records") return undefined;
      if (c.body && (c.body.allowSimilar || c.body.replaces)) return undefined;
      return [409, { error: "memory_similar_exists", message: "a similar note exists", similar: { id: "m-1", revision: 2, title: "SQLite over redb" } }];
    };
    await mounted();
    await click("[data-mnewnote]");
    type("#memEdTitle", "Use SQLite");
    type("#memEdContent", "Both processes write; SQLite takes the lock.");
    (q("#memEdKind") as HTMLSelectElement).value = "constraint";
    await click("#memEdSave");
    expect(sent("POST", "/records")[0].body).toEqual({ project: "p-1", kind: "constraint", title: "Use SQLite", content: "Both processes write; SQLite takes the lock." });
    expect(text("#memEdConflict")).toContain("A similar note exists: SQLite over redb (m-1).");
    await click("#memEdConflict [data-medanyway]");
    expect(sent("POST", "/records")[1].body).toMatchObject({ title: "Use SQLite", allowSimilar: true });
    // Saved: the sheet closed and the new note is the one the detail shows.
    expect(document.querySelector("#sheet #memEdSave")).toBeNull();
    expect(q("#memDetail article").dataset.mdoc).toBe("m-7");

    await click("[data-mnewnote]");
    type("#memEdTitle", "Use SQLite");
    type("#memEdContent", "Both processes write.");
    await click("#memEdSave");
    await click("#memEdConflict [data-medsup]");
    const third = sent("POST", "/records")[3].body;
    expect(third).toMatchObject({ replaces: "m-1" });
    expect(third && third.allowSimilar).toBeUndefined();
  });

  it("a note that reads like documentation is saved, and the toast names what to fix", async () => {
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/records"
      ? [201, { record: record("m-7", "Retries"), duplicate: false, similar: [], formation: [
        { rule: "sections", message: "2 headings" }, { rule: "global", message: "\"every\" claims more than was checked" }] }]
      : undefined;
    await mounted();
    await click("[data-mnewnote]");
    type("#memEdTitle", "Retries");
    type("#memEdContent", "## What happens\nEvery retry doubles.\n## Why\nNo key.");
    await click("#memEdSave");
    expect(document.querySelector("#sheet #memEdSave")).toBeNull();
    expect(text("#toast")).toBe("Saved - it reads like documentation (headings, absolute words); a note is one claim and its why");
  });

  it("a note that retells its anchored files is saved, and the toast names the rule", async () => {
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/records"
      ? [201, { record: record("m-7", "Retry code"), duplicate: false, similar: [], formation: [
        { rule: "restates_code", message: "most of the note appears verbatim in its anchored files" }] }]
      : undefined;
    await mounted();
    await click("[data-mnewnote]");
    type("#memEdTitle", "Retry code");
    type("#memEdContent", "pub fn retry_key(order: u64) -> String {");
    await click("#memEdSave");
    expect(text("#toast")).toBe("Saved - it reads like documentation (code restated); a note is one claim and its why");
  });

  it("a save whose code check omits its empty findings still toasts its formation hints", async () => {
    // The wire skips `findings` when the check found nothing (Vec::is_empty); the toast must survive that.
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/records"
      ? [201, { record: record("m-7", "Retry code"), duplicate: false, similar: [], formation: [
        { rule: "restates_code", message: "most of the note appears verbatim in its anchored files" }],
        code: { index: "none", names: 4, held: false } }]
      : undefined;
    await mounted();
    await click("[data-mnewnote]");
    type("#memEdTitle", "Retry code");
    type("#memEdContent", "pub fn retry_key(order: u64) -> String {");
    await click("#memEdSave");
    expect(document.querySelector("#sheet #memEdSave")).toBeNull();
    expect(text("#toast")).toBe("Saved - it reads like documentation (code restated); a note is one claim and its why");
  });

  it("a note naming code the checkout lacks is saved, and the toast names what was not found", async () => {
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/records"
      ? [201, { record: record("m-7", "Retries"), duplicate: false, similar: [], formation: [{ rule: "long", message: "long" }],
        code: { index: "checked", names: 2, held: false, findings: [
          { rule: "unknown_name", name: "retry_budgett", message: "not in the checkout", files: [] },
          { rule: "missing_file", name: "src/gone.rs", message: "no such file", files: [] },
          { rule: "elsewhere", name: "fitRailLabels", message: "defined in panel/rail.ts", files: ["panel/rail.ts"] }] } }]
      : undefined;
    await mounted();
    await click("[data-mnewnote]");
    type("#memEdTitle", "Retries");
    type("#memEdContent", "retry_budgett caps one retry.");
    await click("#memEdSave");
    expect(text("#toast")).toBe("Saved - the checkout does not hold retry_budgett, src/gone.rs; check the names");
  });

  it("a secret in a note is refused with the rule that caught it", async () => {
    override = (c) => c.method === "POST" && c.path === "/api/knowledge/records"
      ? [400, { error: "memory_sensitive_content", message: "looks like a secret", rule: "aws-access-key" }]
      : undefined;
    await mounted();
    await click("[data-mnewnote]");
    type("#memEdTitle", "The deploy key");
    type("#memEdContent", "AKIA0000000000000000");
    await click("#memEdSave");
    const err = q("#memEdErr");
    expect(err.hidden).toBe(false);
    expect(err.textContent).toContain("aws-access-key");
  });
});

/* ------------------------------------------------------------------ notes: the detail */

describe("the Memory page - the note detail", () => {
  async function detailM1(): Promise<HTMLElement> {
    await mounted();
    const doc = q("#memDetail article[data-mdoc=m-1]");
    return doc;
  }

  it("reads top to bottom: what it is, what it says, its links by kind, anchors, refs, revisions", async () => {
    const doc = await detailM1();
    expect(sent("GET", "/records/m-1")[0].query.get("history")).toBe("true");
    expect(sent("POST", "/related")[0].body).toEqual({ project: "p-1", id: "m-1", limit: 8 });
    const ego = sent("GET", "/graph")[0].query;
    expect(Object.fromEntries(ego)).toEqual({ project: "p-1", center: "m-1", depth: "1", history: "true", limit: "60" });

    expect(q(".doc-title", doc).textContent).toBe("SQLite over redb");
    expect(q(".doc-tags .cat.c1", doc).textContent).toBe("decision");
    expect(q(".doc-meta", doc).textContent).toContain("m-1");
    expect(q(".doc-acts [data-mact=edit]", doc)).not.toBeNull();
    expect(q(".md.note-body > p", doc).textContent).toBe("Two processes share one home, so the store is SQLite.");
    // Links, a group per kind and direction, read from this note's side.
    const groups = Array.from(doc.querySelectorAll<HTMLElement>(".form-cap[data-mlgroup]"));
    expect(groups.map((g) => g.dataset.mlgroup)).toEqual(["supports:out", "supersedes:out"]);
    expect(groups[0].textContent).toBe("→ supports · 1");
    expect(q("button.ref[data-mopen=m-2]", doc).textContent).toBe("Retry with the same key");
    // A supersede's link is history: shown, never removable by hand.
    expect(doc.querySelectorAll("[data-mlinkdel]").length).toBe(1);
    expect(doc.textContent).toContain("redb as the store");
    expect(doc.textContent).toContain("crates/swiss-memory/src/store.rs");
    expect(doc.textContent).toContain("ADR-031");
    // Who wrote it and how it arrived, apart from who confirmed it; what verifies it (ADR-032).
    expect(q("[data-mprov]", doc).textContent).toBe("by a person via panel");
    expect(q(".doc-tags", doc).textContent).toContain("source-verified");
    const pieces = Array.from(doc.querySelectorAll<HTMLElement>("[data-mev]"));
    expect(pieces.map((p) => p.dataset.mev)).toEqual(["e-0000000001", "e-0000000002"]);
    expect(pieces[0].textContent).toContain("earlier text");
    expect(pieces[0].textContent).toContain("derived from");
    expect(q("[data-mpvfile]", pieces[1]).textContent).toBe("crates/swiss-memory/src/store.rs#Store::open");
    expect(pieces[1].textContent).toContain("verified by");
    expect(pieces[1].textContent).not.toContain("earlier text");
    expect(q("#memHistFold > summary", doc).textContent).toBe("2 revisions");
    expect(q("#memHistFold", doc).textContent).toContain("sharpened the why");
    // Its own small graph: it in the middle, its neighbours round it.
    expect(q("#memEgo", doc).classList.contains("compact")).toBe(true);
  });

  it("a link walks to the note at its other end, in the same pane", async () => {
    const doc = await detailM1();
    await click(q("button.ref[data-mopen=m-2]", doc));
    expect(q("#memDetail article").dataset.mdoc).toBe("m-2");
    expect(q("#memList [data-mnote=m-2]").getAttribute("aria-selected")).toBe("true");
  });

  it("removes a link, and adds one to a note found by search", async () => {
    override = (c) => c.path === "/api/knowledge/search"
      ? [200, { items: [{ ...summary("m-3", "Use UTC everywhere", { kind: "knowledge" }), score: 3, tier: "lexical" }] }]
      : undefined;
    const doc = await detailM1();
    await click(q("[data-mlinkdel=supports]", doc));
    const del = sent("DELETE", "/links");
    expect(del.length).toBe(1);
    expect(Object.fromEntries(del[0].query)).toEqual({ project: "p-1", from: "m-1", to: "m-2", kind: "supports" });
    expect(text("#toast")).toBe("Link removed");

    (q("#memLinkKind") as HTMLSelectElement).value = "depends_on";
    type("#memLinkNote", "timestamps compare");
    type("#memLinkQ", "utc");
    q("#memLinkQ").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    expect(sent("GET", "/search").at(-1)?.query.get("q")).toBe("utc");
    await click("#memLinkHits [data-mpick=m-3]");
    expect(sent("POST", "/links").map((c) => c.body)).toEqual([{ project: "p-1", from: "m-1", to: "m-3", kind: "depends_on", note: "timestamps compare" }]);
    expect(text("#toast")).toBe("Link added");
  });

  it("an associated note not linked yet is linked from the detail in one click", async () => {
    override = (c) => c.path === "/api/knowledge/related" && c.body && c.body.id === "m-1"
      ? [200, { items: [{ ...summary("m-6", "Lock the home dir", { kind: "constraint" }), score: 64, reasons: [{ type: "text", terms: ["sqlite"] }] }] }]
      : undefined;
    const doc = await detailM1();
    expect(q("[data-mreason=text]", doc).textContent).toBe("shares sqlite");
    await click(q("[data-mrellink=m-6]", doc));
    expect(sent("POST", "/links").map((c) => c.body)).toEqual([{ project: "p-1", from: "m-1", to: "m-6", kind: "related_to" }]);
  });

  it("shows the note before its associations, which fill their section when they arrive", async () => {
    let release: (a: Answer) => void = () => undefined;
    override = (c) => c.path === "/api/knowledge/related" && c.body && c.body.id === "m-1"
      ? new Promise<Answer>((done) => { release = done; })
      : undefined;
    const doc = await detailM1();
    expect(q(".doc-title", doc).textContent).toBe("SQLite over redb");
    expect(q("#memEgo", doc)).not.toBeNull();
    expect(q("#memRelated", doc).textContent).toContain("Finding related notes");
    release([200, { items: [{ ...summary("m-6", "Lock the home dir", { kind: "constraint" }), score: 64, reasons: [{ type: "text", terms: ["sqlite"] }] }] }]);
    await settle();
    expect(q("#memRelated [data-mreason=text]", doc).textContent).toBe("shares sqlite");
  });

  it("a link to a purged note says so: no button to open it, and it can still be removed", async () => {
    override = (c) => {
      if (c.method !== "GET" || c.path !== "/api/knowledge/records/m-1") return undefined;
      const r = record("m-1", "SQLite over redb");
      r.links = [{ dir: "in", kind: "depends_on", id: "m-5", title: "", status: null }];
      return [200, { record: r, revisions: REVISIONS }];
    };
    const doc = await detailM1();
    expect(doc.querySelector("[data-mopen=m-5]")).toBeNull();
    expect(doc.textContent || "").toContain("purged note");
    await click(q("[data-mlinkdel=depends_on]", doc));
    expect(Object.fromEntries(sent("DELETE", "/links")[0].query)).toEqual({ project: "p-1", from: "m-5", to: "m-1", kind: "depends_on" });
  });

  it("delete, under the detail's ⋯, needs the note's id typed; a wrong one deletes nothing", async () => {
    await detailM1();
    await click("#memDetail [data-mact=more]");
    await menuPick("Delete…");
    type("#g-name", "m-2");
    await click("#g-save");
    expect(text("#g-err")).toBe("That is not this note's id - nothing was deleted.");
    expect(sent("DELETE", "/records/m-1").length).toBe(0);
    type("#g-name", "m-1");
    await click("#g-save");
    const del = sent("DELETE", "/records/m-1");
    expect(del.length).toBe(1);
    expect(Object.fromEntries(del[0].query)).toEqual({ project: "p-1", confirm: "m-1" });
    expect(text("#toast")).toBe("m-1 deleted");
  });
});

/* ------------------------------------------------------------------ graph, agent view, review */

describe("the Memory page - graph, agent view, review", () => {
  const graph = (truncated: boolean): Body => ({
    nodes: [
      { id: "m-1", kind: "decision", title: "SQLite over redb", status: "active", confirmed: true, origin: "user", degree: 1, hop: 1 },
      { id: "m-2", kind: "pitfall", title: "Retry with the same key", status: "active", confirmed: false, origin: "agent", degree: 2, hop: 0 },
      { id: "m-3", kind: "knowledge", title: "Old routing rule", status: "superseded", confirmed: true, origin: "user", degree: 1, hop: 1 },
    ],
    edges: [
      { from: "m-1", to: "m-2", kind: "supports", directed: true },
      { from: "m-2", to: "m-3", kind: "related_to", directed: false, note: "same incident" },
    ],
    truncated,
  });
  /** The Graph tab's drawing (a note's own small graph is a core of its own). */
  const last = (): FakeCore => {
    const tab = document.getElementById("memGraph");
    const c = fakeCores.filter((x) => !x.destroyed && !!tab && tab.contains(x.opts.container)).at(-1);
    if (!c) throw new Error("no graph drawn");
    return c;
  };
  const group = (c: FakeCore, g: string): string[] => c.opts.elements.filter((e) => e.group === g).map((e) => String(e.data.id));

  it("hands the engine one node and one edge per answer row, each in its kind's look", async () => {
    override = (c) => c.path === "/api/knowledge/graph" ? [200, graph(c.query.has("center"))] : undefined;
    await mounted();
    calls = [];
    await click("[data-mtab=graph]");
    await drawn();
    const asked = sent("GET", "/graph");
    expect(asked.length).toBe(1);
    expect(Object.fromEntries(asked[0].query)).toEqual({ project: "p-1", limit: "300" });
    const core = last();
    // Most connected first: the layout seeds them in the middle.
    expect(group(core, "nodes")).toEqual(["m-2", "m-1", "m-3"]);
    expect(group(core, "edges")).toEqual(["m-1>m-2>supports", "m-2>m-3>related to"]);
    expect(core.cls("m-1")).toEqual(["t1"]);
    // History is dimmed, an unconfirmed note dashed (pending); the note with two links, a hub,
    // is named at any zoom.
    expect(core.cls("m-3")).toEqual(["dim", "t4"]);
    expect(core.cls("m-2")).toEqual(["hub", "pending", "t3"]);
    expect(core.cls("m-1>m-2>supports")).toEqual(["directed", "solid"]);
    expect(core.cls("m-2>m-3>related to")).toEqual(["dotted"]);
    expect(core.opts.layout.name).toBe("cose");
    // The legend names every kind and every link kind.
    expect(document.querySelectorAll("#memGraph .gv-legend .gv-key").length).toBe(10);
    expect(text("#memGraphCount")).toBe("3 notes · 2 links");
    expect(document.querySelector("[data-mgtrunc]")).toBeNull();
    expect(document.querySelector("[data-mgdepth]")).toBeNull();
    expect(text("#memGraphSel")).toBe("Click a node to see what it is.");
  });

  it("a tap selects in place; the inspector reads the note's links out and walks them", async () => {
    override = (c) => c.path === "/api/knowledge/graph" ? [200, graph(c.query.has("center"))] : undefined;
    await mounted();
    await click("[data-mtab=graph]");
    await drawn();
    calls = [];
    const core = last();
    core.fire("tap", "m-2");
    await settle();
    // In place: the same drawing, now marked - no second request, no new layout.
    expect(sent("GET", "/graph").length).toBe(0);
    expect(last()).toBe(core);
    expect(core.cls("m-2")).toContain("sel");
    expect(core.cls("m-1>m-2>supports")).toContain("hl");
    expect(text("#memGraphSel .doc-title")).toBe("Retry with the same key");
    expect(text("#memGraphSel .doc-tags")).toContain("unconfirmed");
    const lines = Array.from(document.querySelectorAll<HTMLElement>("#memGraphSel .lrow"));
    expect(lines.map((l) => q("button.ref", l).dataset.mgsel)).toEqual(["m-1", "m-3"]);
    expect(q(".lrow-sub", lines[0]).textContent).toBe("← supported by");
    expect(q(".lrow-sub", lines[1]).textContent).toBe("↔ related to · same incident");

    await click("#memGraphSel [data-mgsel=m-3]");
    expect(core.cls("m-3")).toContain("sel");
    expect(core.cls("m-2")).not.toContain("sel");
    expect(text("#memGraphSel .doc-title")).toBe("Old routing rule");

    // The background clears it.
    core.fire("tap", null);
    await settle();
    expect(core.cls("m-3")).not.toContain("sel");
    expect(text("#memGraphSel")).toBe("Click a node to see what it is.");
  });

  it("Center here re-asks around the note; a double tap opens it on Notes", async () => {
    override = (c) => c.path === "/api/knowledge/graph" ? [200, graph(c.query.has("center"))] : undefined;
    await mounted();
    await click("[data-mtab=graph]");
    await drawn();
    const whole = last();
    whole.fire("tap", "m-2");
    await settle();
    calls = [];
    await click("#memGraphSel [data-mgcenter=m-2]");
    await drawn();
    const centred = sent("GET", "/graph");
    expect(centred.length).toBe(1);
    expect(centred[0].query.get("center")).toBe("m-2");
    expect(centred[0].query.get("depth")).toBe("1");
    expect(whole.destroyed).toBe(true);
    const core = last();
    expect(core).not.toBe(whole);
    expect(core.opts.layout.name).toBe("concentric");
    expect(core.cls("m-2")).toContain("center");
    expect(core.cls("m-2")).toContain("sel");
    expect(document.querySelector("[data-mgtrunc]")).not.toBeNull();
    expect(document.querySelectorAll("[data-mgdepth]").length).toBe(3);
    // Centred already: no Center here for the centre itself.
    expect(document.querySelector("#memGraphSel [data-mgcenter]")).toBeNull();

    core.fire("dbltap", "m-1");
    await settle();
    expect(core.destroyed).toBe(true);
    expect(core.opts.container.isConnected).toBe(false);
    expect(q("[data-mtab=notes]").getAttribute("aria-selected")).toBe("true");
    expect(q("#memDetail article").dataset.mdoc).toBe("m-1");
  });

  it("a project with domains draws each as a box round its notes, a sub-domain inside its parent's", async () => {
    const filed = graph(false);
    const homes = ["pay", "pay/refunds", undefined];
    (filed.nodes as Body[]).forEach((n, i) => { if (homes[i]) n.domain = homes[i]; });
    override = (c) => c.path === "/api/knowledge/graph" ? [200, filed] : undefined;
    await mounted();
    calls = [];
    await click("[data-mtab=graph]");
    await drawn();
    const core = last();
    const boxes = core.opts.elements.filter((e) => (e.classes || "").split(" ").includes("group"));
    // "store" holds no note: no box. The note in no domain sits in Unfiled's.
    expect(boxes.map((b) => [b.data.id, b.data.label, b.data.parent, b.classes])).toEqual([
      ["group:pay", "Payments", undefined, "group"],
      ["group::unfiled", "Unfiled", undefined, "group"],
      ["group:pay/refunds", "Refunds", "group:pay", "group sub"],
    ]);
    expect(boxes[0].data.title).toBe("pay - Charging and refunds");
    const parent = (id: string): unknown => core.opts.elements.find((e) => e.data.id === id)?.data.parent;
    expect(["m-1", "m-2", "m-3"].map(parent)).toEqual(["group:pay", "group:pay/refunds", "group::unfiled"]);
    expect(core.opts.layout.name).toBe("preset");
    // A box is background: a tap on one clears the inspector.
    core.fire("tap", "m-2");
    await settle();
    expect(text("#memGraphSel .doc-title")).toBe("Retry with the same key");
    core.fire("tap", "group:pay");
    await settle();
    expect(text("#memGraphSel")).toBe("Click a node to see what it is.");
  });

  it("an empty project draws nothing and says so", async () => {
    override = (c) => c.path === "/api/knowledge/graph" && !c.query.has("center") ? [200, { nodes: [], edges: [], truncated: false }] : undefined;
    await mounted();
    await click("[data-mtab=graph]");
    await drawn();
    expect(document.getElementById("memGraph")).toBeNull();
    const body = q("#memBody");
    expect(fakeCores.some((c) => body.contains(c.opts.container))).toBe(false);
    expect(body.textContent).toContain("No notes to draw");
  });

  it("the Agent view draws the brief as the Markdown it is; its source is a toggle away and what Copy copies", async () => {
    const md = "# Memory: swiss\n\n## Rules\n\n- [decision] SQLite over redb · `m-1` <script>alert(1)</script>";
    override = (c) => c.path === "/api/knowledge/brief" ? [200, { markdown: md, chars: md.length, root: "C:/src/swiss" }] : undefined;
    await mounted();
    await click("[data-mtab=agent]");
    expect(sent("POST", "/brief")[0].body).toEqual({ project: "p-1", includeGlobal: true });
    // Drawn: headings, the list, the id as code - and the markup in a note stays text.
    expect(text("#memBrief .md.brief h1")).toBe("Memory: swiss");
    expect(text("#memBrief .md.brief h2")).toBe("Rules");
    expect(text("#memBrief .md.brief li code")).toBe("m-1");
    expect(text("#memBrief li")).toContain("<script>alert(1)</script>");
    expect(document.querySelector("#memBrief script")).toBeNull();
    expect(text("#memBrief")).toContain(String(md.length) + " chars");

    // The source: the text the agent reads, as it reads it; pressed again, drawn again.
    const toggle = (): HTMLElement => q("#memBrief [data-msource]");
    expect(toggle().getAttribute("aria-pressed")).toBe("false");
    await click("#memBrief [data-msource]");
    expect(text("#memBrief pre")).toBe(md);
    expect(document.querySelector("#memBrief .md")).toBeNull();
    expect(toggle().getAttribute("aria-pressed")).toBe("true");

    const wrote: string[] = [];
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } } });
    try {
      await click("#memBrief [data-mcopy]");
    } finally {
      Reflect.deleteProperty(navigator, "clipboard");
    }
    expect(wrote).toEqual([md]);

    await click("#memBrief [data-msource]");
    expect(document.querySelector("#memBrief .md.brief")).not.toBeNull();
    expect(toggle().getAttribute("aria-pressed")).toBe("false");

    type("#memBriefPaths", "src/orders.rs\n\n  src/retry.rs ");
    type("#memBriefTask", "add retries");
    await click("[data-mbrief]");
    expect(sent("POST", "/brief")[1].body).toEqual({ project: "p-1", includeGlobal: true, paths: ["src/orders.rs", "src/retry.rs"], task: "add retries" });
  });

  it("Review: a verdict in the detail, then the next note waiting takes its place", async () => {
    let inbox = ["m-4", "m-5"];
    const titles: Record<string, string> = { "m-4": "Agents run cargo fmt", "m-5": "Port 19999 is production" };
    override = (c) => {
      if (c.path === "/api/knowledge/review") {
        return [200, { items: inbox.map((id) => summary(id, titles[id], { origin: "agent", actor: "codex", confirmed: false, markers: ["unconfirmed"] })) }];
      }
      const one = /^\/api\/knowledge\/records\/(m-[45])$/.exec(c.path);
      if (one && c.method === "GET") {
        const r = record(one[1], titles[one[1]]);
        Object.assign(r, { origin: "agent", actor: "codex", confirmed: false, links: [] });
        return [200, { record: r, revisions: [] }];
      }
      const done = /^\/api\/knowledge\/records\/(m-[45])\/(confirm|mark)$/.exec(c.path);
      if (done) inbox = inbox.filter((x) => x !== done[1]);
      return undefined;
    };
    await mounted();
    await click("[data-mtab=review]");
    expect(sent("GET", "/review")[0].query.get("project")).toBe("p-1");
    expect(text("#memList [data-mnote=m-4] .pick-sub")).toContain("written by codex, not confirmed yet");
    expect(q("#memDetail article").dataset.mdoc).toBe("m-4");

    await click("#memDetail [data-mact=more]");
    await menuPick("Confirm");
    expect(sent("POST", "/records/m-4/confirm").map((c) => c.body)).toEqual([{ project: "p-1" }]);
    expect(text("#toast")).toBe("Confirmed");
    // Decided: it left the inbox, and the one after it is open.
    expect(document.querySelector("#memList [data-mnote=m-4]")).toBeNull();
    expect(q("#memList [data-mnote=m-5]").getAttribute("aria-selected")).toBe("true");
    expect(q("#memDetail article").dataset.mdoc).toBe("m-5");

    await click("#memDetail [data-mact=more]");
    await menuPick("Verified");
    expect(sent("POST", "/records/m-5/mark").map((c) => c.body)).toEqual([{ project: "p-1", verdict: "verified" }]);
    expect(text("#memList")).toContain("Nothing to review");
    expect(text("#memDetail")).toContain("No note selected");
  });
});

/* ------------------------------------------------------------------ semantic search settings */

describe("the Memory page - semantic search settings", () => {
  const OFF_STATUS = { state: "off", model: null, dims: null, indexed: 0, pending: 0, lastError: null, lastRunMs: null };

  it("the gear shows the index and the model in force, read-only; Edit on the LLM page goes there", async () => {
    let embedding: Body = { config: { provider: "off" }, remote: false, configError: null, status: OFF_STATUS };
    override = (c) => {
      if (c.path === "/api/knowledge/embedding") return [200, embedding];
      return undefined;
    };
    await mounted();
    await click("[data-membed]");
    expect(text("#sheet h2")).toBe("Semantic search");
    expect(text("#memEmbStatus")).toContain("Off - word search only");
    // Off: the sheet points at the llm plugin, and there is nothing to test or re-embed.
    expect(text("#memEmbConfig")).toBe("No embedding model is set - search matches words only. Set one on the LLM plugin's Embedding page.");
    expect(q<HTMLButtonElement>("#memEmbTest").disabled).toBe(true);
    expect(q<HTMLButtonElement>("#memEmbReindex").disabled).toBe(true);
    // The block is the llm plugin's: nothing on this sheet edits it.
    expect(document.querySelector("#sheet input, #sheet select, #sheet textarea")).toBe(null);
    await click("#memEmbManage");
    expect(location.hash).toBe("#llm-embedding");
    expect(q("#sheet").hidden).toBe(true);
    location.hash = "";

    // A model off this machine: the provider is named, and where the text goes is said.
    embedding = {
      config: { provider: "openai", model: "text-embedding-3-small", dimensions: 512, batch: 16, timeoutMs: 30000, queryPrefix: "", documentPrefix: "", remote: true },
      remote: true, configError: null,
      status: { state: "ready", model: "openai:text-embedding-3-small:512", dims: 512, indexed: 3, pending: 0, lastError: null, lastRunMs: NOW },
    };
    await click("[data-membed]");
    expect(text("#memEmbConfig")).toContain("openai");
    expect(text("#memEmbConfig")).toContain("text-embedding-3-small");
    expect(text("#memEmbConfig")).toContain("512");
    expect(text("#memEmbConfig .tag.warn")).toBe("leaves this machine");
    expect(text("#memEmbConfig")).toContain("Note text is sent off this machine to be embedded.");
    expect(q<HTMLButtonElement>("#memEmbTest").disabled).toBe(false);
  });

  it("Test asks the model in force, Re-embed starts over, and a provider's refusal is said", async () => {
    const config = {
      provider: "openai", model: "text-embedding-3-small",
      batch: 16, timeoutMs: 30000, queryPrefix: "", documentPrefix: "", remote: true,
    };
    const ready = { state: "ready", model: "openai:text-embedding-3-small:native", dims: 1536, indexed: 3, pending: 0, lastError: null, lastRunMs: NOW };
    let answerTest: Body = { ok: true, model: "openai:text-embedding-3-small:native", dims: 1536, ms: 120 };
    override = (c) => {
      if (c.path === "/api/knowledge/embedding") return [200, { config, remote: true, configError: null, status: ready }];
      if (c.path === "/api/knowledge/embedding/test") return [200, answerTest];
      if (c.path === "/api/knowledge/embedding/reindex") return [200, { dropped: 3, status: { ...ready, state: "indexing", indexed: 0, pending: 3 } }];
      return undefined;
    };
    await mounted();
    await click("[data-membed]");
    expect(text("#memEmbStatus")).toContain("3 of 3 notes · 1536 dimensions");

    await click("#memEmbTest");
    expect(sent("POST", "/embedding/test").map((c) => c.body)).toEqual([{}]);
    expect(text("#memEmbResult")).toBe("openai:text-embedding-3-small:native answered: 1536 dimensions in 120 ms");

    answerTest = { ok: false, model: "openai:text-embedding-3-small:native", error: "401 Unauthorized: invalid key", ms: 80 };
    await click("#memEmbTest");
    expect(text("#memEmbErr")).toBe("The provider failed: 401 Unauthorized: invalid key");
    expect(q("#memEmbResult").hidden).toBe(true);

    await click("#memEmbReindex");
    expect(sent("POST", "/embedding/reindex").map((c) => c.body)).toEqual([{ drop: true }]);
    expect(text("#memEmbResult")).toBe("Embedding every note again (3 vectors cleared)");
    expect(text("#memEmbStatus .tag.warn")).toBe("3 waiting");
  });

  it("a stale index says so: vectors of another width are counted apart, never as embedded", async () => {
    const config = {
      provider: "openai", baseUrl: "http://127.0.0.1:11434/v1", model: "bge-m3",
      batch: 16, timeoutMs: 30000, queryPrefix: "", documentPrefix: "",
    };
    const stale = {
      state: "stale", model: "openai:bge-m3:native:ep-1a2b3c4d", dims: 768, indexed: 0, pending: 3, incompatible: 3,
      lastError: null, lastRunMs: NOW,
    };
    override = (c) => {
      if (c.path === "/api/knowledge/embedding") return [200, { config, remote: false, configError: null, status: stale }];
      return undefined;
    };
    await mounted();
    await click("[data-membed]");
    expect(text("#memEmbStatus")).toContain("Stale - the provider answers another width now; embedding again");
    expect(text("#memEmbStatus")).toContain("0 of 3 notes · 768 dimensions");
    const warnings = [...document.querySelectorAll("#memEmbStatus .tag.warn")].map((t) => t.textContent);
    expect(warnings).toEqual(["3 waiting", "3 of another width"]);
  });
});

/* ------------------------------------------------------------------ sync settings */

describe("the Memory page - sync settings", () => {
  function change(sel: string, value: string, ev = "change"): void {
    const el = q<HTMLInputElement | HTMLSelectElement>(sel);
    el.value = value;
    el.dispatchEvent(new Event(ev, { bubbles: true }));
  }

  /** The llm plugin's agents, as GET /api/llm/status lists them. */
  const AGENTS = [
    { name: "distiller", runtime: "builtin", provider: "ollama", model: "qwen3.5:4b", timeoutMs: 1_800_000, report: true, reports: "swiss", maxTurns: 24, maxTokens: 8192 },
    { name: "cc", runtime: "claude", provider: null, model: null, timeoutMs: 1_800_000, report: true, reports: "child", command: "claude", program: "C:/Users/me/.local/bin/claude.exe" },
    // The Data page's own: never offered to a memory task.
    { name: "data", runtime: "db", provider: "glm", model: "glm-5.3-flash", timeoutMs: 1_800_000, report: true, reports: "swiss", maxTurns: 24 },
  ];

  /** The routes the sheet reads: the overview, the queue, the llm plugin's agents, the
   *  plugin row and the project's settings. */
  function syncAnswers(o: { config?: Body | null; agent?: Body | null; workers?: Body[]; lead?: Body | null; chosen?: Body; settings?: Body; llm?: Answer; runs?: Body[] } = {}): { puts: Body[]; starts: Body[]; patches: Body[]; runs: Body[] } {
    const puts: Body[] = [];
    const starts: Body[] = [];
    const patches: Body[] = [];
    const overview = {
      config: o.config ?? null, configError: null, agent: o.agent ?? null,
      workers: o.workers, lead: o.lead ?? null, chosen: o.chosen, settings: o.settings,
      state: { head: "a1b2c3d4e5f60718293a0b5c6d7e8f9012345678", lastFinishedMs: NOW - 600_000, pendingCommits: 2, pendingBytes: 4096 },
      runs: o.runs ?? [
        { id: "r-1", project: "p-1", trigger: "manual", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 600_000, summary: "1 note, 1 update", ops: [] },
        { id: "r-2", project: "p-1", trigger: "nudge", dryRun: true, state: "failed", queuedMs: NOW - 400_000, dueMs: NOW - 400_000, finishedMs: NOW - 300_000, summary: "", error: "the agent answered no JSON object" },
      ],
    };
    let saved: Body = o.settings ?? { agents: null, lead: null, embedding: null };
    override = (c) => {
      if (c.path === "/api/knowledge/sync" && c.method === "GET") return [200, overview];
      if (c.path === "/api/knowledge/sync" && c.method === "POST") {
        starts.push(c.body || {});
        return [202, { run: { id: "r-9", project: "p-1", state: "queued", queuedMs: NOW, dueMs: NOW }, merged: false }];
      }
      if (c.path === "/api/llm/status") return o.llm ?? [200, { serving: true, providers: [], embedding: null, agents: AGENTS }];
      if (c.path === "/api/plugins/memory/config" && c.method === "GET") return [200, { revision: 7, config: { enabled: true } }];
      if (c.path === "/api/plugins/memory/config" && c.method === "PUT") {
        puts.push(c.body || {});
        return [200, {}];
      }
      if (/^\/api\/knowledge\/projects\/[^/]+\/settings$/.test(c.path) && c.method === "PATCH") {
        patches.push(c.body || {});
        const had: Body = saved;
        saved = {
          agents: c.body && "agents" in c.body ? c.body.agents : had.agents,
          lead: c.body && "lead" in c.body ? c.body.lead : had.lead,
          embedding: c.body && "embedding" in c.body ? c.body.embedding : had.embedding,
          autoReview: c.body && "autoReview" in c.body ? c.body.autoReview : had.autoReview ?? null,
          autoReviewRuns: c.body && "autoReviewRuns" in c.body ? c.body.autoReviewRuns : had.autoReviewRuns ?? null,
        };
        overview.settings = saved;
        return [200, { settings: saved }];
      }
      return undefined;
    };
    return { puts, starts, patches, runs: overview.runs };
  }

  it("the sheet says where the project stands, and the queue buttons post their runs", async () => {
    const { starts } = syncAnswers({
      config: { agent: "distiller", transcripts: "claude-code", auto: true },
      agent: { name: "distiller", runtime: "builtin", model: "qwen3.5:4b" },
    });
    await mounted();
    await click("[data-msync]");
    expect(text("#sheet h2")).toBe("Sync - commits into memory");
    expect(text("#memSyncStatus")).toContain("distiller · built-in · qwen3.5:4b");
    expect(text("#memSyncStatus")).toContain("a1b2c3d");
    // The conversations waiting read as a size, never a raw byte count.
    expect(text("#memSyncStatus")).toContain("2 commit(s) · 4.0 KiB of conversations unread");
    expect(text("#memSyncRuns")).toContain("1 note, 1 update");
    expect(text("#memSyncRuns")).toContain("the agent answered no JSON object");
    expect(document.querySelectorAll("#memSyncRuns [data-mrun]").length).toBe(2);
    // An agent is named: the settings stay folded under the status.
    expect(q("#memSyncBlockFold").hasAttribute("open")).toBe(false);

    await click("#memSyncNow");
    await click("#memSyncDry");
    expect(starts).toEqual([
      { project: "p-1", dryRun: false },
      { project: "p-1", dryRun: true },
    ]);
    expect(text("#toast")).toContain("Queued run r-9");
  });

  it("the picker offers inherit, off and a pool of names; an untouched inherit saves back absent", async () => {
    const { puts } = syncAnswers();
    await mounted();
    await click("[data-msync]");
    expect(text("#memSyncStatus")).toContain("none");
    // Nothing named here yet: the one setting that matters starts unfolded, its extra fields hidden.
    expect(q("#memSyncBlockFold").hasAttribute("open")).toBe(true);
    const mode = q<HTMLSelectElement>("#memSyncAgentMode");
    expect([...mode.options].map((o) => [o.value, o.textContent])).toEqual([
      ["inherit", "inherit - the layers below decide"], ["off", "off - no runs"], ["named", "these agents…"],
    ]);
    expect(mode.value).toBe("inherit");
    expect(q("#memSyncAgentNames").hidden).toBe(true);
    // Inherit still carries the block's own settings; an untouched save writes agent back absent.
    expect(q("#memSyncTranscripts")).toBeTruthy();
    type("#memSyncInterval", "900");
    await click("#memSyncBlockSave");
    expect(puts).toEqual([{ revision: 7, config: { enabled: true, sync: { transcripts: "claude-code", auto: true, minIntervalSec: 900 } } }]);

    // A pool of names: one name saves a string, more save a list; none named is refused.
    change("#memSyncAgentMode", "named");
    expect(q("#memSyncAgentNames").hidden).toBe(false);
    await click('#memSyncAgentNames [data-magent="distiller"]');
    type("#memSyncInterval", "10m");
    await click("#memSyncBlockSave");
    expect(text("#memSyncErr")).toBe("Min interval (s): a whole number");
    expect(puts).toHaveLength(1);
    type("#memSyncInterval", "900");
    await click("#memSyncBlockSave");
    // The fresh revision, the rest of the row untouched: the agent's runtime, model and key are not memory's.
    expect(puts[1]).toEqual({
      revision: 7,
      config: { enabled: true, sync: { agent: "distiller", transcripts: "claude-code", auto: true, minIntervalSec: 900 } },
    });
    expect(text("#toast")).toBe("Sync block saved");
    await click('#memSyncAgentNames [data-magent="cc"]');
    await click("#memSyncBlockSave");
    expect(((puts[2].config as Body).sync as Body).agent).toEqual(["distiller", "cc"]);
    await click('#memSyncAgentNames [data-magent="distiller"]');
    await click('#memSyncAgentNames [data-magent="cc"]');
    await click("#memSyncBlockSave");
    expect(text("#memSyncErr")).toBe("Name at least one agent.");
    expect(puts).toHaveLength(3);

    change("#memSyncAgentMode", "off");
    expect(document.querySelector("#memSyncTranscripts")).toBe(null);
    await click("#memSyncBlockSave");
    expect(puts[3]).toEqual({ revision: 7, config: { enabled: true, sync: { agent: "off" } } });
  });

  it("the lead is inherit, off, or one name - off is written, inherit is left out", async () => {
    const { puts } = syncAnswers({ config: { agent: "distiller", lead: "cc", transcripts: "off", auto: true } });
    await mounted();
    await click("[data-msync]");
    const lead = q<HTMLSelectElement>("#memSyncLead");
    expect([...lead.options].map((o) => [o.value, o.textContent])).toEqual([
      ["inherit", "inherit - the project's settings, else none"], ["off", "none - the agent reviews alone"],
      ["distiller", "distiller · built-in"], ["cc", "cc · Claude Code"],
    ]);
    // The db agent the status also names serves the Data page alone: no task picks it.
    expect(document.querySelector('#memSyncAgentNames [data-magent="data"]')).toBe(null);
    expect(lead.value).toBe("cc");
    expect(text("#memSyncBlockFields")).toContain("wraps up each domain the run wrote in");
    await click("#memSyncBlockSave");
    expect(puts[0]).toEqual({ revision: 7, config: { enabled: true, sync: { agent: "distiller", lead: "cc", transcripts: "off", auto: true } } });

    change("#memSyncLead", "off");
    await click("#memSyncBlockSave");
    expect((puts[1].config as Body).sync).toEqual({ agent: "distiller", lead: "off", transcripts: "off", auto: true });
    change("#memSyncLead", "inherit");
    await click("#memSyncBlockSave");
    expect((puts[2].config as Body).sync).toEqual({ agent: "distiller", transcripts: "off", auto: true });
    // Off drops the block to its one word: no lead to pick under it.
    change("#memSyncAgentMode", "off");
    expect(document.querySelector("#memSyncLead")).toBe(null);
  });

  it("the overview names the workers, the lead and which layer decided each", async () => {
    syncAnswers({
      config: { agent: "distiller", transcripts: "off", auto: true },
      workers: [
        { name: "distiller", runtime: "builtin", model: "qwen3.5:4b" },
        { name: "cc", runtime: "claude", model: null },
      ],
      lead: { name: "cc", runtime: "claude", model: null },
      chosen: { agents: { names: ["distiller", "cc"], from: "project" }, lead: { names: [], from: null } },
    });
    await mounted();
    await click("[data-msync]");
    expect(text("#memSyncStatus")).toContain("distiller · built-in · qwen3.5:4b");
    expect(text("#memSyncStatus")).toContain("+");
    expect(text("#memSyncStatus")).toContain("cc · Claude Code");
    expect(text("#memSyncStatus")).toContain("the project's settings");
    // The lead row: the agent, and no layer named one when none did.
    expect(text("#memSyncStatus")).toContain("Lead");
    expect(text("#memSyncStatus").split("Lead")[1]).toContain("no layer named one");
    // A worker that cannot run says so under the card, naming the worker.
    syncAnswers({
      workers: [{ name: "distiller", runtime: "builtin", model: "m", error: "its provider is gone" }],
      chosen: { agents: { names: [], from: "project" }, lead: { names: [], from: null } },
    });
    await mounted();
    await click("[data-msync]");
    expect(text("#memSyncStatus")).toContain("distiller: its provider is gone");
    expect(text("#memSyncStatus")).toContain("none");
    expect(text("#memSyncStatus")).toContain("the project's settings");
  });

  it("a run that asked a pool says how each worker went", async () => {
    syncAnswers({
      runs: [
        { id: "r-1", project: "p-1", trigger: "full", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 600_000, summary: "done",
          runner: { runner: "distiller + cc", attempts: 9, workers: [
            { label: "distiller", model: "qwen3.5:4b", asked: 6, failed: 1 },
            { label: "cc", asked: 3, failed: 0 },
          ] } },
      ],
    });
    await mounted();
    await click("[data-msync]");
    const line = text('[data-mworkers="r-1"]');
    expect(line).toContain("distiller: 6 asked, 1 failed");
    expect(line).toContain("cc: 3 asked, 0 failed");
  });

  it("the project's own settings save through the settings API and repaint from the answer", async () => {
    const { patches } = syncAnswers({ settings: { agents: ["distiller"], lead: null, embedding: null } });
    await mounted();
    await click("[data-msync]");
    // The fold shows what the project said: one agent held, the rest inherited.
    expect(q<HTMLSelectElement>("#memSyncProjectAgentsMode").value).toBe("named");
    expect(q<HTMLInputElement>('#memSyncProjectAgentsNames [data-magent="distiller"]').checked).toBe(true);
    expect(q<HTMLInputElement>('#memSyncProjectAgentsNames [data-magent="cc"]').checked).toBe(false);
    expect(q<HTMLSelectElement>("#memSyncProjectLead").value).toBe("inherit");
    expect(q<HTMLSelectElement>("#memSyncProjectEmbedding").value).toBe("inherit");
    // The daily review is not set here: the fold says how it stands and points at Auto-update.
    expect(document.querySelector("#memSyncProjectAutoReview")).toBeNull();
    expect(text("#memSyncAutoState")).toBe("off");
    // A pool, a lead, embedding off: one PATCH, only the fold's own fields - a daily review set
    // in Auto-update is never sent back from here.
    await click('#memSyncProjectAgentsNames [data-magent="cc"]');
    change("#memSyncProjectLead", "cc");
    change("#memSyncProjectEmbedding", "off");
    await click("#memSyncSettingsSave");
    expect(patches).toEqual([{ agents: ["distiller", "cc"], lead: "cc", embedding: false }]);
    expect(text("#toast")).toBe("Project settings saved");
    // The fold repaints from the saved answer.
    expect(q<HTMLSelectElement>("#memSyncProjectLead").value).toBe("cc");
    expect(q<HTMLSelectElement>("#memSyncProjectEmbedding").value).toBe("off");
    // A refused word is said in the sheet; nothing was sent for it.
    change("#memSyncProjectAgentsMode", "named");
    await click('#memSyncProjectAgentsNames [data-magent="distiller"]');
    await click('#memSyncProjectAgentsNames [data-magent="cc"]');
    await click("#memSyncSettingsSave");
    expect(text("#memSyncErr")).toBe("Name at least one agent.");
    expect(patches).toHaveLength(1);
    // Inherit hands every field back: null all round.
    change("#memSyncProjectAgentsMode", "inherit");
    change("#memSyncProjectLead", "inherit");
    change("#memSyncProjectEmbedding", "inherit");
    await click("#memSyncSettingsSave");
    expect(patches[1]).toEqual({ agents: null, lead: null, embedding: null });
  });

  it("a project whose settings turn the lead off shows off, and an untouched save keeps it off", async () => {
    const { patches } = syncAnswers({ settings: { agents: null, lead: "off", embedding: null } });
    await mounted();
    await click("[data-msync]");
    expect(q<HTMLSelectElement>("#memSyncProjectLead").value).toBe("off");
    await click("#memSyncSettingsSave");
    expect(patches).toEqual([{ agents: null, lead: "off", embedding: null }]);
    expect(q<HTMLSelectElement>("#memSyncProjectLead").value).toBe("off");
  });

  it("the fold says the daily review's time and its button opens Auto-update in place of the sheet", async () => {
    syncAnswers({ settings: { agents: null, lead: null, embedding: null, autoReview: "21:30", autoReviewRuns: null } });
    await mounted();
    await click("[data-msync]");
    expect(text("#memSyncAutoState")).toBe("daily 21:30");
    await click("#memSyncAutoOpen");
    expect(text("#sheet h2")).toBe("Auto-update");
    expect(q<HTMLInputElement>("#memAutoReviewAt").value).toBe("21:30");
    // And back: the auto-update sheet's foot reopens the sync sheet.
    await click("#memAutoSyncOpen");
    expect(text("#sheet h2")).toBe("Sync - commits into memory");
  });

  it("a run's own pick rides the queued run: agents with any kind, a lead beside map and full only", async () => {
    const { starts } = syncAnswers();
    await mounted();
    await click("[data-msync]");
    // Left out, nothing rides: the pick is absent from the body.
    await click("#memSyncMapDraw");
    expect(starts[0]).toEqual({ project: "p-1", map: true, dryRun: false });
    // A pool for this run, and a lead; recheck takes the agents but never the lead.
    change("#memSyncRunAgentsMode", "named");
    await click('#memSyncRunAgentsNames [data-magent="distiller"]');
    await click('#memSyncRunAgentsNames [data-magent="cc"]');
    change("#memSyncRunLead", "off");
    await click("#memSyncMapDraw");
    expect(starts[1]).toEqual({ project: "p-1", map: true, dryRun: false, agents: ["distiller", "cc"], lead: "off" });
    await click("#memSyncRecheck");
    expect(starts[2]).toEqual({ project: "p-1", recheck: true, dryRun: false, agents: ["distiller", "cc"] });
    change("#memSyncRunAgentsMode", "off");
    await click("#memSyncInit");
    expect(starts[3]).toEqual({ project: "p-1", full: true, dryRun: false, agents: "off", lead: "off" });
  });

  it("the map preview lists the areas, and a covered map says so", async () => {
    const { starts } = syncAnswers();
    const base = override!;
    let plan: Body = {
      plan: [
        { path: "billing", title: "Billing", kind: "dir", owns: ["src/billing"], files: 3, kept: false },
        { path: "orders", title: "Orders", kind: "dir", owns: ["src/orders"], files: 3, kept: true },
      ],
      asks: 1, codeFiles: 6, ownedFiles: 6, mappedFiles: 3, truncated: false,
    };
    override = (c) => (c.path === "/api/knowledge/sync/map" ? [200, plan] : base(c));
    await mounted();
    await click("[data-msync]");
    // Preview lists the areas without asking a model; the ones on the map already are tagged kept.
    await click("#memSyncMapPreview");
    expect(sent("GET", "/sync/map")).toHaveLength(1);
    const listed = Array.from(document.querySelectorAll<HTMLElement>("#memSyncMapPlan [data-marea]"));
    expect(listed.map((r) => r.dataset.marea)).toEqual(["billing", "orders"]);
    expect(listed[1].querySelector(".tag")?.textContent).toBe("on the map");
    expect(text("#memSyncMapPlan")).toContain("1 area(s) to describe · 6 of 6 code file(s) owned");
    // Draw queues a map run: the language typed and the dry-run box ride along.
    type("#memSyncMapLang", "zh");
    q<HTMLInputElement>("#memSyncMapDry").checked = true;
    await click("#memSyncMapDraw");
    expect(starts.at(-1)).toEqual({ project: "p-1", map: true, dryRun: true, language: "zh" });
    expect(text("#toast")).toContain("Queued map run r-9");
    // A map that already owns the code: the empty preview says so, never that the index holds none.
    plan = { plan: [], asks: 0, codeFiles: 6, ownedFiles: 0, mappedFiles: 6, truncated: false };
    await click("#memSyncMapPreview");
    expect(text("#memSyncMapPlan")).toContain("the map already owns 6");
    expect(text("#memSyncMapPlan")).not.toContain("No areas");
    // A checkout the index holds no code for keeps the plain empty state.
    plan = { plan: [], asks: 0, codeFiles: 0, ownedFiles: 0, mappedFiles: 0, truncated: false };
    await click("#memSyncMapPreview");
    expect(text("#memSyncMapPlan")).toContain("No areas");
  });

  it("Initialize previews the slices and the index, refuses a bad cap, and queues a full run with redraw", async () => {
    const { starts } = syncAnswers();
    const base = override!;
    const full: Body = {
      plan: [{ path: "ordering", title: "Ordering", kind: "dir", owns: ["src/main/java/com/shop/order"], files: 4, kept: false }],
      asks: 1, review: true, removed: ["order"], codeFiles: 6, ownedFiles: 4, mappedFiles: 0, truncated: false,
      knowledge: { files: 6, bytes: 9000, slices: 2, asks: 1, unchanged: 0, more: 1, unread: 0 },
      embedding: { state: "off", indexed: 0, pending: 0 },
    };
    override = (c) => (c.path === "/api/knowledge/sync/full" ? [200, full] : c.path === "/api/knowledge/sync/map" ? [200, full] : base(c));
    await mounted();
    await click("[data-msync]");
    q<HTMLInputElement>("#memSyncMapRedraw").checked = true;
    // A cap that is no whole number from 1 is said under the form, and nothing is asked.
    type("#memSyncInitMax", "0");
    await click("#memSyncInitPreview");
    expect(text("#memSyncErr")).toBe("Max questions: a whole number from 1");
    expect(sent("GET", "/sync/full")).toEqual([]);
    type("#memSyncInitMax", "1");
    await click("#memSyncInitPreview");
    const asked = sent("GET", "/sync/full");
    expect(asked).toHaveLength(1);
    expect([asked[0].query.get("redraw"), asked[0].query.get("maxAsks")]).toEqual(["1", "1"]);
    // The preview counts the slices, says what waits, says the index is off, then the map.
    expect(text("#memSyncInitPlan")).toContain("6 file(s) in 2 slice(s) · 1 to ask");
    expect(text("#memSyncInitPlan")).toContain("0 unchanged since the last initialize · 1 for a later run");
    expect(text("#memSyncInitPlan")).toContain("embedding off - notes are found by their words only");
    expect(text("#memSyncInitPlan")).toContain("The redraw removes 1 domain(s) an earlier map drew: order");
    expect(text("#memSyncInitPlan")).toContain("grouped into business domains");
    // The map preview plans the redraw too.
    await click("#memSyncMapPreview");
    expect(sent("GET", "/sync/map")[0].query.get("redraw")).toBe("1");
    // Initialize queues a full run: the redraw box and the cap ride along.
    await click("#memSyncInit");
    expect(starts.at(-1)).toEqual({ project: "p-1", full: true, dryRun: false, redraw: true, maxAsks: 1 });
    expect(text("#toast")).toContain("Queued full run r-9");
  });

  it("a running full run shows its notes and the index, and the open sheet follows it to its end", async () => {
    const knowledge = { files: 6, bytes: 9000, slices: 2, asked: 1, unchanged: 0, more: 0, failed: 0, proposed: 2, saved: 2, held: 0, duplicates: 0, refused: 0 };
    const { runs } = syncAnswers({
      runs: [{ id: "r-5", project: "p-1", trigger: "full", dryRun: false, state: "running", queuedMs: NOW - 60_000, dueMs: NOW - 60_000, startedMs: NOW - 50_000,
        summary: "", scope: { redraw: true, asks: 2, filled: 2, knowledge } }],
    });
    await mounted();
    vi.useFakeTimers({ toFake: ["setTimeout"], shouldAdvanceTime: true });
    try {
      await click("[data-msync]");
      expect(text("#memSyncRuns")).toContain("full init");
      expect(text("#memSyncRuns")).toContain("redraw");
      expect(text("#memSyncRuns")).toContain("2/2 area(s) described");
      expect(text("#memSyncRuns [data-mknow]")).toContain("2 note(s) saved (0 held for review, 0 known) from 1/2 slice(s)");
      const reads = (): number => sent("GET", "/sync").length;
      const before = reads();
      // The run ends while the sheet is open: the next look shows it and the index.
      runs[0] = { ...runs[0], state: "ok", finishedMs: NOW, summary: "the map drew 2 domain(s); 3 note(s) saved",
        scope: { redraw: true, asks: 2, filled: 2, knowledge: { ...knowledge, asked: 2, proposed: 3, saved: 3, embedding: { state: "ready", model: "bge-m3", indexed: 3, pending: 0 } } } };
      await vi.advanceTimersByTimeAsync(3000);
      await settle();
      expect(reads()).toBe(before + 1);
      expect(text("#memSyncRuns [data-mknow]")).toContain("3 note(s) saved (0 held for review, 0 known) from 2/2 slice(s)");
      expect(text("#memSyncRuns [data-mknow]")).toContain("embedding ready (bge-m3) · 3 indexed, 0 pending");
      // An ended run is not followed any further.
      await vi.advanceTimersByTimeAsync(9000);
      await settle();
      expect(reads()).toBe(before + 1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a lead's wrap-up is said on its run: the phase it stopped in, the domains, hubs and notes it came to", async () => {
    const knowledge = { files: 6, bytes: 9000, slices: 2, asked: 2, unchanged: 0, more: 0, failed: 0, proposed: 3, saved: 3, held: 0, duplicates: 0, refused: 0 };
    syncAnswers({
      runs: [
        { id: "r-7", project: "p-1", trigger: "full", dryRun: false, state: "cancelled", queuedMs: NOW - 60_000, dueMs: NOW - 60_000, finishedMs: NOW - 10_000,
          summary: "", scope: { asks: 2, filled: 2, knowledge, phase: "wrapup", wrapup: { domains: 3, wrapped: 1, hubs: 1, saved: 2, held: 1, failed: 0 } } },
        { id: "r-6", project: "p-1", trigger: "full", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 600_000,
          summary: "", scope: { asks: 2, filled: 2, knowledge, wrapup: { domains: 2, wrapped: 2, hubs: 2, saved: 4, held: 0, failed: 1 } },
          runner: { runner: "builtin:distiller", attempts: 1, lead: "planner" } },
        { id: "r-5", project: "p-1", trigger: "full", dryRun: false, state: "ok", queuedMs: NOW - 990_000, dueMs: NOW - 990_000, finishedMs: NOW - 960_000,
          summary: "", scope: { asks: 2, filled: 2, knowledge } },
      ],
    });
    await mounted();
    await click("[data-msync]");
    expect(text("[data-mrun=r-7]")).toContain("wrapping up the domains");
    expect(text("[data-mrun=r-7] [data-mwrap]")).toBe("the lead wrapped up 1/3 domain(s): 1 hub(s) refreshed, 2 note(s) saved (1 held for review)");
    expect(text("[data-mrun=r-6] [data-mwrap]")).toBe("the lead wrapped up 2/2 domain(s): 2 hub(s) refreshed, 4 note(s) saved (0 held for review) · 1 failed");
    // A run without a lead wrapped nothing up and says nothing of it.
    expect(document.querySelector("[data-mrun=r-5] [data-mwrap]")).toBe(null);
  });

  it("Recheck notes queues a recheck with the cap but never the redraw, and a recheck's count is said on its run", async () => {
    const recheck = { domains: 2, notes: 27, questions: 3, asked: 2, failed: 1, more: 0, kept: 15, fixed: 4, dropped: 2, merged: 1, linked: 9, held: 1, refused: 0, left: 0 };
    const knowledge = { files: 6, bytes: 9000, slices: 2, asked: 2, unchanged: 0, more: 0, failed: 0, proposed: 3, saved: 3, held: 0, duplicates: 0, refused: 0 };
    const { starts } = syncAnswers({
      runs: [
        { id: "r-8", project: "p-1", trigger: "recheck", dryRun: false, state: "running", queuedMs: NOW - 60_000, dueMs: NOW - 60_000, startedMs: NOW - 50_000,
          summary: "", scope: { phase: "recheck", recheck } },
        { id: "r-7", project: "p-1", trigger: "full", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 600_000,
          summary: "", scope: { asks: 2, filled: 2, knowledge, recheck: { ...recheck, failed: 0, more: 4, held: 0, refused: 2, lifted: 1, rounds: 1, open: 1 } } },
        { id: "r-6", project: "p-1", trigger: "full", dryRun: false, state: "ok", queuedMs: NOW - 990_000, dueMs: NOW - 990_000, finishedMs: NOW - 960_000,
          summary: "", scope: { asks: 2, filled: 2, knowledge } },
        { id: "r-5", project: "p-1", trigger: "recheck", dryRun: true, state: "ok", queuedMs: NOW - 999_000, dueMs: NOW - 999_000, finishedMs: NOW - 998_000,
          summary: "would recheck 27 note(s) in 2 domain(s): 3 question(s), 0 for a later run", scope: { recheck: { ...recheck, asked: 0, failed: 0, kept: 0, fixed: 0, dropped: 0, merged: 0, linked: 0, held: 0 } } },
      ],
    });
    await mounted();
    await click("[data-msync]");
    // The row names the run, then its phase in the quiet line after the dot.
    expect(text("[data-mrun=r-8] .fl-title")).toBe("recheck");
    expect(text("[data-mrun=r-8] .fl-summary")).toBe("rechecking the notes");
    expect(text("[data-mrun=r-8] [data-mcheck]")).toBe(
      "rechecked 2/3 question(s) over 27 note(s): 15 kept, 4 fixed, 2 dropped, 1 merged, 9 link(s) added · 1 still held by the code check · 1 question(s) failed");
    // A full run closes with a recheck and says it the same way, the contradictions it settled too.
    expect(text("[data-mrun=r-7] [data-mcheck]")).toBe(
      "rechecked 2/3 question(s) over 27 note(s): 15 kept, 4 fixed, 2 dropped, 1 merged, 9 link(s) added · 1 contradiction(s) lifted"
      + " · 1 more round(s) for the contradictions the answers raised · 1 contradiction(s) between the model's notes still open"
      + " · 4 question(s) left for the next recheck · 2 change(s) refused");
    expect(document.querySelector("[data-mrun=r-6] [data-mcheck]")).toBe(null);
    // A dry recheck judged nothing: it says what it would ask, once - the engine's own summary
    // is the line's hover, not a second line.
    expect(text("[data-mrun=r-5] [data-mcheck]")).toBe("would recheck 27 note(s) in 2 domain(s): 3 question(s), 0 for a later run");
    expect(text("[data-mrun=r-5]").split("would recheck")).toHaveLength(2);
    expect(q("[data-mrun=r-5] [data-mcheck]").title).toBe("would recheck 27 note(s) in 2 domain(s): 3 question(s), 0 for a later run");
    // The redraw box stays a map's; a bad cap is said, a good one and the language ride along.
    q<HTMLInputElement>("#memSyncMapRedraw").checked = true;
    type("#memSyncMapLang", "zh");
    type("#memSyncInitMax", "x");
    await click("#memSyncRecheck");
    expect(text("#memSyncErr")).toBe("Max questions: a whole number from 1");
    expect(starts).toEqual([]);
    type("#memSyncInitMax", "5");
    await click("#memSyncRecheck");
    expect(starts.at(-1)).toEqual({ project: "p-1", recheck: true, dryRun: false, language: "zh", maxAsks: 5 });
    expect(text("#toast")).toContain("Queued recheck r-9");
  });

  it("a docs run says what it read and the cards it saved, and a running one names its phase", async () => {
    const docs = { sources: 1, files: 12, bytes: 90_000, added: 10, changed: 1, unchanged: 1, removed: 0, cut: 0, unread: 0, waiting: 11,
      questions: 3, asked: 3, failed: 1, more: 0, carded: 8, stale: 0, refused: 0, symbols: 14, dropped: 2 };
    syncAnswers({
      runs: [
        { id: "r-4", project: "p-1", trigger: "docs", dryRun: false, state: "running", queuedMs: NOW - 60_000, dueMs: NOW - 60_000, startedMs: NOW - 50_000,
          summary: "", scope: { phase: "docs", docs: { ...docs, asked: 1, failed: 0, carded: 4, symbols: 6 } } },
        { id: "r-3", project: "p-1", trigger: "docs", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 600_000,
          summary: "read 12 document(s) from 1 source(s): 10 added, 1 changed, 1 unchanged, 0 removed; carded 8", scope: { docs: { ...docs, relinked: 2, missing: ["docs/old"] } } },
        { id: "r-2", project: "p-1", trigger: "docs", dryRun: true, state: "ok", queuedMs: NOW - 990_000, dueMs: NOW - 990_000, finishedMs: NOW - 980_000,
          summary: "", scope: { docs: { ...docs, asked: 0, failed: 0, carded: 0, symbols: 0, dropped: 0 } } },
      ],
    });
    await mounted();
    await click("[data-msync]");
    expect(text("[data-mrun=r-4] .fl-title")).toBe("documents");
    expect(text("[data-mrun=r-4] .fl-summary")).toBe("carding the documents");
    expect(text("[data-mrun=r-3] [data-mdocs]")).toBe(
      "read 12 document(s) from 1 source(s): 10 new, 1 changed, 1 unchanged, 0 removed · 8 carded from 3/3 question(s), 14 code name(s) found in the code"
      + " · 2 card(s) re-linked to the code as it is now · 3 still wait for a card · 1 question(s) failed · not in the checkout: docs/old");
    // The engine's English is the line's hover, not a second line.
    expect(q("[data-mrun=r-3] [data-mdocs]").title).toBe("read 12 document(s) from 1 source(s): 10 added, 1 changed, 1 unchanged, 0 removed; carded 8");
    expect(text("[data-mrun=r-3]").split("12 document(s)")).toHaveLength(2);
    expect(text("[data-mrun=r-2] [data-mdocs]")).toBe(
      "read 12 document(s) from 1 source(s): 10 new, 1 changed, 1 unchanged, 0 removed · 11 would wait for a card: 3 question(s)");
  });

  it("a recheck's run says its outcome in the panel's language, never the engine's English beside it", async () => {
    const recheck = { domains: 48, notes: 500, questions: 73, asked: 73, failed: 0, more: 0, kept: 205, fixed: 60, dropped: 4, merged: 13, linked: 384, held: 158, refused: 2, left: 60 };
    const said = "rechecked 440 of 500 note(s) in 48 domain(s): 205 kept, 60 fixed, 4 dropped, 13 merged, 384 link(s) added; 158 still need review";
    syncAnswers({
      runs: [
        { id: "r-3", project: "p-1", trigger: "recheck", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 60_000,
          summary: said, scope: { language: "zh", recheck } },
        { id: "r-2", project: "p-1", trigger: "recheck", dryRun: false, state: "empty", queuedMs: NOW - 990_000, dueMs: NOW - 990_000, finishedMs: NOW - 980_000,
          summary: "no note waits for a recheck: a person wrote, confirmed or a recheck judged every one",
          scope: { recheck: { ...recheck, notes: 0, questions: 0, asked: 0, kept: 0, fixed: 0, dropped: 0, merged: 0, linked: 0, held: 0, refused: 0, left: 0 } } },
      ],
    });
    setLang("zh-CN");
    await loadLocale();
    try {
      await mounted();
      await click("[data-msync]");
      const row = text("[data-mrun=r-3]");
      expect(text("[data-mrun=r-3] [data-mcheck]")).toBe(
        "复核了 500 条笔记，问了 73/73 个问题：保留 205，修正 60，删除 4，合并 13，新增 384 条关联 · 158 条仍被代码校验拦下 · 2 处改动被拒绝");
      expect(row).not.toContain("rechecked");
      expect(q("[data-mrun=r-3] [data-mcheck]").title).toBe(said);
      expect(text("[data-mrun=r-2] [data-mcheck]")).toBe("没有待复核的笔记：每条都由人写下或确认过，或已被复核判过");
      expect(text("[data-mrun=r-2]")).not.toContain("no note waits");
    } finally {
      setLang("en");
      await loadLocale();
    }
  });

  it("Resolve the review inbox queues a review run without the lead or the redraw, and its run says what it confirmed, lifted and left open", async () => {
    const recheck = { domains: 3, notes: 40, questions: 8, asked: 8, failed: 0, more: 0, kept: 20, fixed: 6, dropped: 3, merged: 1, linked: 4, held: 10,
      refused: 0, left: 0, confirmed: 25, lifted: 4, rounds: 2, open: 1 };
    const none = { ...recheck, notes: 0, questions: 0, asked: 0, kept: 0, fixed: 0, dropped: 0, merged: 0, linked: 0, held: 0, confirmed: 0, lifted: 0,
      rounds: 0, open: 0 };
    const { starts } = syncAnswers({
      runs: [
        { id: "r-8", project: "p-1", trigger: "review", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 60_000,
          summary: "resolved 40 of 40 note(s) in 3 domain(s): 20 kept, 6 fixed, 3 dropped, 1 merged, 4 link(s) added; 25 confirmed, 4 contradiction(s) lifted"
            + " (2 more round(s) for the contradictions the answers raised); 1 contradiction(s) between the model's notes still open",
          scope: { recheck } },
        { id: "r-7", project: "p-1", trigger: "review", dryRun: true, state: "ok", queuedMs: NOW - 990_000, dueMs: NOW - 990_000, finishedMs: NOW - 980_000,
          summary: "would resolve 40 note(s) in 3 domain(s): 8 question(s), 0 for a later run", scope: { recheck: { ...none, notes: 40, questions: 8 } } },
        { id: "r-6", project: "p-1", trigger: "review", dryRun: false, state: "empty", queuedMs: NOW - 999_000, dueMs: NOW - 999_000, finishedMs: NOW - 998_000,
          summary: "the review inbox holds no note a model wrote: what waits there is a person's", scope: { recheck: none } },
        { id: "r-5", project: "p-1", trigger: "review", dryRun: false, state: "empty", queuedMs: NOW - 1_999_000, dueMs: NOW - 1_999_000, finishedMs: NOW - 1_998_000,
          summary: "no note a model wrote names a file that changed since it was last checked: nothing to review", scope: { changed: true, recheck: none } },
        { id: "r-4", project: "p-1", trigger: "review", dryRun: true, state: "ok", queuedMs: NOW - 2_999_000, dueMs: NOW - 2_999_000, finishedMs: NOW - 2_998_000,
          summary: "would review 5 note(s) whose code changed in 2 domain(s): 1 question(s), 0 for a later run",
          scope: { changed: true, recheck: { ...none, notes: 5, domains: 2, questions: 1 } } },
      ],
    });
    await mounted();
    await click("[data-msync]");
    expect(text("[data-mrun=r-8]")).toContain("review inbox");
    expect(text("[data-mrun=r-8] [data-mcheck]")).toBe(
      "rechecked 8/8 question(s) over 40 note(s): 20 kept, 6 fixed, 3 dropped, 1 merged, 4 link(s) added · 25 confirmed, 4 contradiction(s) lifted"
      + " · 2 more round(s) for the contradictions the answers raised · 1 contradiction(s) between the model's notes still open"
      + " · 10 still held by the code check");
    expect(text("[data-mrun=r-8]").split("25 confirmed")).toHaveLength(2);
    expect(text("[data-mrun=r-7] [data-mcheck]")).toBe("would resolve 40 note(s) of the review inbox in 3 domain(s): 8 question(s), 0 for a later run");
    expect(text("[data-mrun=r-6] [data-mcheck]")).toBe("No notes to review in this run.");
    // A review of the notes whose code changed - the daily one - says so in its row.
    expect(text("[data-mrun=r-5] .fl-title")).toBe("review · code changed");
    expect(text("[data-mrun=r-5] [data-mcheck]")).toBe("no note a model wrote names a file that changed since it was last checked: nothing to review");
    expect(text("[data-mrun=r-4] [data-mcheck]")).toBe("would review 5 note(s) whose code changed in 2 domain(s): 1 question(s), 0 for a later run");
    // The redraw is a map's and the lead a map or full run's: neither rides a review run.
    q<HTMLInputElement>("#memSyncMapRedraw").checked = true;
    await click("#memSyncReview");
    expect(starts.at(-1)).toEqual({ project: "p-1", review: true, dryRun: false });
    expect(text("#toast")).toContain("Queued review run r-9");
    await click("#memSyncReviewChanged");
    expect(starts.at(-1)).toEqual({ project: "p-1", review: true, dryRun: false, changed: true });
  });

  it("Stop tells a running run to stop, cancels a queued one, and the rows say the phase and the outcome", async () => {
    const knowledge = { files: 6, bytes: 9000, slices: 4, asked: 1, unchanged: 0, more: 0, failed: 0, proposed: 2, saved: 2, held: 0, duplicates: 0, refused: 0 };
    syncAnswers({
      runs: [
        { id: "r-5", project: "p-1", trigger: "full", dryRun: false, state: "running", queuedMs: NOW - 60_000, dueMs: NOW - 60_000, startedMs: NOW - 50_000,
          summary: "", scope: { asks: 2, filled: 2, knowledge, phase: "knowledge" } },
        { id: "r-6", project: "p-1", trigger: "manual", dryRun: false, state: "queued", queuedMs: NOW - 10_000, dueMs: NOW - 10_000, summary: "" },
        { id: "r-4", project: "p-1", trigger: "map", dryRun: false, state: "cancelled", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 800_000,
          summary: "cancelled while the map was drawn; nothing landed", scope: { asks: 3, filled: 1, phase: "areas" } },
      ],
    });
    const base = override!;
    const stopped: string[] = [];
    let stopping: string[] = [];
    override = (c) => {
      const hit = /^\/api\/knowledge\/sync\/runs\/([^/]+)\/cancel$/.exec(c.path);
      if (hit && c.method === "POST") {
        stopped.push(hit[1] + "?" + c.query.toString());
        const running = hit[1] === "r-5";
        if (running) stopping = ["r-5"];
        return [200, { run: { id: hit[1], state: running ? "running" : "cancelled" }, stopping: running }];
      }
      if (c.path === "/api/knowledge/sync" && c.method === "GET") {
        const got = base(c) as [number, Body];
        return [got[0], { ...got[1], stopping }];
      }
      return base(c);
    };
    await mounted();
    vi.useFakeTimers({ toFake: ["setTimeout"], shouldAdvanceTime: true });
    try {
      await click("[data-msync]");
      const row = (id: string): string => text(`#memSyncRuns [data-mrun="${id}"]`);
      expect(row("r-5")).toContain("reading the slices");
      expect(row("r-4")).toContain("cancelled");
      expect(row("r-4")).toContain("describing the areas");
      // Only a queued or running run has a Stop; an ended one has none.
      const stops = [...document.querySelectorAll<HTMLElement>("#memSyncRuns [data-mstop]")].map((b) => b.dataset.mstop);
      expect(stops).toEqual(["r-5", "r-6"]);

      await click('[data-mstop="r-5"]');
      expect(stopped).toEqual(["r-5?project=p-1"]);
      expect(text("#toast")).toContain("Run r-5 is stopping: the questions in flight are dropped");
      // Told to stop, it still runs until its next question: the row says stopping, with no second Stop.
      expect(row("r-5")).toContain("stopping…");
      expect(document.querySelector('[data-mstop="r-5"]')).toBe(null);

      await click('[data-mstop="r-6"]');
      expect(stopped.at(-1)).toBe("r-6?project=p-1");
      expect(text("#toast")).toContain("Run r-6 cancelled before it started");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a sync run lists the agent runs it started - a running one's at once, live first; a finished one's on opening - and a row opens its trace", async () => {
    syncAnswers({
      config: { agent: "distiller", transcripts: "claude-code", auto: true },
      agent: { name: "distiller", runtime: "builtin", model: "glm-5.3" },
      runs: [
        { id: "r-7", project: "p-1", trigger: "manual", dryRun: false, state: "running", queuedMs: NOW - 60_000, dueMs: NOW - 60_000, startedMs: NOW - 50_000,
          summary: "", runner: { runner: "distiller", model: "glm-5.3", attempts: 0 }, scope: { commits: [{ hash: "a1b2c3d", subject: "fix" }], phase: "ask" } },
        { id: "r-1", project: "p-1", trigger: "manual", dryRun: false, state: "ok", queuedMs: NOW - 900_000, dueMs: NOW - 900_000, finishedMs: NOW - 600_000, summary: "1 note, 1 update", ops: [] },
        { id: "r-0", project: "p-1", trigger: "manual", dryRun: false, state: "empty", queuedMs: NOW - 990_000, dueMs: NOW - 990_000, finishedMs: NOW - 980_000, summary: "nothing new since the last sync", ops: [] },
      ],
    });
    // The LLM history's rows filed under each sync run, as GET /api/llm/history/runs?source= answers.
    const agentRun = (id: number, more: Body): Body => ({
      id, at: "2026-10-09T12:07:00.000Z", atEnd: null, agent: "distiller", runtime: "builtin", provider: "glm", model: "glm-5.3",
      ok: null, ms: 0, turns: 3, kind: null, task: "memory.sync", subject: "p-1", source: "memory:r-7", parent: null, head: null,
      rounds: 1, tokensIn: 1200, tokensOut: 80, tokensCacheRead: 0, children: 0, tools: {}, ...more,
    });
    const live = agentRun(41, { live: { elapsedMs: 65_000, round: 2, step: "tool", tool: "read_file", stepMs: 3000, streamed: 0 } });
    const helper = agentRun(40, { ok: true, ms: 20_000, task: "memory.task", subject: "read a.java" });
    const earlier = agentRun(30, { ok: true, ms: 31_000, source: "memory:r-1", subject: "p-1 earlier" });
    const base = override!;
    override = (c) => {
      if (c.path === "/api/llm/history/runs" && c.query.get("source") === "memory:r-7") return [200, { runs: [live, helper] }];
      if (c.path === "/api/llm/history/runs" && c.query.get("source") === "memory:r-1") return [200, { runs: [earlier] }];
      return base(c);
    };
    const core = await import("../src/views/llm-core.js");
    await mounted();
    vi.useFakeTimers({ toFake: ["setTimeout"], shouldAdvanceTime: true });
    try {
      await click("[data-msync]");
      const row = (id: string): string => text(`#memSyncRuns [data-mrun="${id}"]`);
      // The running run opens on its own, says its phase and lists its agent runs, the live one first.
      expect(q('#memSyncRuns [data-mrun="r-7"]').classList.contains("open")).toBe(true);
      expect(q('#memSyncRuns [data-mrun="r-7"]').classList.contains("live")).toBe(true);
      expect(row("r-7")).toContain("asking the agent about the commits");
      expect(row("r-7")).toContain("Agent runs: 2, 1 running");
      expect(row("r-7")).toContain("Round 2 · running read_file · 3.0 s · 65 s in all");
      const lines = [...document.querySelectorAll<HTMLElement>('#memSyncRuns [data-mrun="r-7"] [data-magentrun]')].map((l) => l.dataset.magentrun);
      expect(lines).toEqual(["41", "40"]);
      expect(document.querySelector('#memSyncRuns [data-magentrun="41"] [data-lhstate="live"]')).toBeTruthy();
      expect(row("r-7")).toContain("single request · 1.2K / 80 · 20 s");
      // A finished run stays closed and its list waits for it to open; an empty run, which
      // asked nobody, reads none even opened.
      expect(q('#memSyncRuns [data-mrun="r-1"]').classList.contains("open")).toBe(false);
      expect(calls.some((c) => c.query.get("source") === "memory:r-1")).toBe(false);
      await click('#memSyncRuns [data-mrun="r-0"] .fl-sum');
      expect(calls.some((c) => c.query.get("source") === "memory:r-0")).toBe(false);
      await click('#memSyncRuns [data-mrun="r-1"] .fl-sum');
      expect(q('#memSyncRuns [data-mrun="r-1"]').classList.contains("open")).toBe(true);
      expect(calls.some((c) => c.query.get("source") === "memory:r-1")).toBe(true);
      expect(row("r-1")).toContain("Agent runs: 1");
      expect(row("r-1")).toContain("p-1 earlier");
      // Closed and opened again, it reads them no second time.
      await click('#memSyncRuns [data-mrun="r-1"] .fl-sum');
      await click('#memSyncRuns [data-mrun="r-1"] .fl-sum');
      expect(calls.filter((c) => c.query.get("source") === "memory:r-1").length).toBe(1);
      // A row opens its trace on the LLM history page.
      await click('[data-mllmrun="41"]');
      expect(location.hash).toBe("#llm-history");
      expect(q("#sheet").hidden).toBe(true);
      expect(core.takeWantedRun()).toBe(41);
    } finally {
      vi.useRealTimers();
      location.hash = "";
    }
  });

  it("a name the llm plugin lost still shows and says why; a stopped llm plugin is said; Agents goes to its page", async () => {
    syncAnswers({
      config: { agent: "gone", transcripts: "claude-code", auto: true },
      agent: { name: "gone", error: "no agent named gone" },
      llm: [503, { error: "plugin llm is not running" }],
    });
    await mounted();
    await click("[data-msync]");
    expect(text("#memSyncStatus")).toContain("The agent cannot run: no agent named gone");
    // The lost name stays picked in the multi-select, so an untouched save never drops it.
    expect(q<HTMLSelectElement>("#memSyncAgentMode").value).toBe("named");
    const held = q<HTMLInputElement>('#memSyncAgentNames [data-magent="gone"]');
    expect(held.checked).toBe(true);
    expect(text("#memSyncAgentNames")).toContain("gone (not defined)");
    expect(text("#memSyncBlockFold")).toContain("The LLM plugin did not answer: plugin llm is not running");

    await click("#memSyncManage");
    expect(location.hash).toBe("#llm-agents");
    expect(q("#sheet").hidden).toBe(true);
    location.hash = "";
  });
});

/* ------------------------------------------------------------------ auto-update */

describe("the Memory page - auto-update", () => {
  function change(sel: string, value: string, ev = "change"): void {
    const el = q<HTMLInputElement | HTMLSelectElement>(sel);
    el.value = value;
    el.dispatchEvent(new Event(ev, { bubbles: true }));
  }

  function check(sel: string, on: boolean): void {
    const el = q<HTMLInputElement>(sel);
    el.checked = on;
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }

  const AGENTS = [
    { name: "lead", runtime: "claude", provider: null, model: null, timeoutMs: 1_800_000, report: true, reports: "child", command: "claude" },
    { name: "cc", runtime: "claude", provider: null, model: null, timeoutMs: 1_800_000, report: true, reports: "child", command: "claude" },
  ];

  /** The review inbox: two notes a model wrote and one a person wrote whose code changed, and one
   *  only unconfirmed. */
  const INBOX = [
    summary("m-4", "Retry with the same key", { origin: "agent", actor: "codex", markers: ["anchor_changed"] }),
    summary("m-5", "Port 19999 is production", { origin: "agent", actor: "codex", markers: ["anchor_changed", "unconfirmed"], confirmed: false }),
    summary("m-6", "SQLite over redb", { markers: ["anchor_changed"] }),
    summary("m-7", "Agents run cargo fmt", { origin: "agent", actor: "codex", markers: ["unconfirmed"], confirmed: false }),
  ];

  /** A changed review that came to something (SPEC §memory.init The review run). */
  const REVIEWED = {
    id: "r-5", project: "p-1", trigger: "review", dryRun: false, state: "ok", queuedMs: NOW - 7_200_000, dueMs: NOW - 7_200_000,
    startedMs: NOW - 7_100_000, finishedMs: NOW - 7_000_000, summary: "4 questions",
    scope: { changed: true, recheck: { asked: 4, questions: 4, notes: 3, kept: 1, fixed: 1, dropped: 1, merged: 0, linked: 0, confirmed: 1, lifted: 0, failed: 0 } },
  };

  /** The routes the page and the sheet read: the project's settings (GET and PATCH, one stored
   *  copy), the sync overview and its queue, the review inbox, the llm plugin's agents and the
   *  plugin row's block. */
  function autoAnswers(o: { settings?: Body; config?: Body | null; chosen?: Body; workers?: Body[]; runs?: Body[]; inbox?: Body[] } = {}): { patches: Body[]; puts: Body[]; starts: Body[] } {
    const patches: Body[] = [];
    const puts: Body[] = [];
    const starts: Body[] = [];
    projectSettings = { ...noSettings(), ...(o.settings || {}) };
    const block = o.config === undefined ? { agent: "lead", transcripts: "claude-code", auto: true, minIntervalSec: 900 } : o.config;
    override = (c) => {
      if (c.path === "/api/knowledge/sync" && c.method === "GET") {
        return [200, {
          config: block, configError: null, agent: null, workers: o.workers ?? [{ name: "lead", runtime: "claude" }], lead: null,
          chosen: o.chosen ?? { agents: { names: ["lead"], from: "plugin" }, lead: { name: null, from: "none" } },
          settings: projectSettings, state: { head: null, lastFinishedMs: null, pendingCommits: 0, pendingBytes: 0 },
          runs: o.runs ?? [REVIEWED], stopping: [],
        }];
      }
      if (c.path === "/api/knowledge/sync" && c.method === "POST") {
        starts.push(c.body || {});
        return [202, { run: { id: "r-9", project: "p-1", state: "queued", queuedMs: NOW, dueMs: NOW }, merged: false }];
      }
      if (c.path === "/api/knowledge/review") return [200, { items: o.inbox ?? INBOX }];
      if (c.path === "/api/llm/status") return [200, { serving: true, providers: [], embedding: null, agents: AGENTS }];
      if (c.path === "/api/plugins/memory/config" && c.method === "GET") {
        return [200, { revision: 7, config: { enabled: true, privacy: "local", sync: block } }];
      }
      if (c.path === "/api/plugins/memory/config" && c.method === "PUT") {
        puts.push(c.body || {});
        return [200, {}];
      }
      if (/^\/api\/knowledge\/projects\/[^/]+\/settings$/.test(c.path) && c.method === "PATCH") {
        patches.push(c.body || {});
        projectSettings = { ...projectSettings, ...(c.body || {}) };
        return [200, { settings: projectSettings }];
      }
      return undefined;
    };
    return { patches, puts, starts };
  }

  it("the head says how the daily review stands and opens the sheet; the global scope has none", async () => {
    autoAnswers({ settings: { autoReview: "21:30" } });
    await mounted();
    expect(sent("GET", "/projects/p-1/settings")).toHaveLength(1);
    expect(text("#memAutoOpen")).toBe("Auto-update · daily 21:30");
    // It comes first, before the project's menu and New note.
    expect(q("#memAutoOpen").nextElementSibling?.hasAttribute("data-mpmenu")).toBe(true);
    await click("#memAutoOpen");
    expect(text("#sheet h2")).toBe("Auto-update");
    expect(text("#sheet")).toContain("swiss");

    await click("[data-mproject=global]");
    expect(document.querySelector("#memAutoOpen")).toBeNull();
    expect(sent("GET", "/projects/global/settings")).toHaveLength(0);
  });

  it("the head says off, and the bare word while the settings cannot be read", async () => {
    autoAnswers();
    await mounted();
    expect(text("#memAutoOpen")).toBe("Auto-update · off");
    view.unmount();
    override = (c) => (/\/settings$/.test(c.path) ? [500, { error: "memory_internal", message: "boom" }] : undefined);
    await mounted();
    expect(text("#memAutoOpen")).toBe("Auto-update");
  });

  it("the Review bar warns when changed notes wait with no daily review, and one click turns it on", async () => {
    autoAnswers();
    await mounted();
    await click("[data-mtab=review]");
    const bar = text("#memReviewAuto");
    expect(bar).toContain("no daily review");
    // Every pending note counts, regardless of origin or changed files.
    expect(bar).toContain("4 note(s) await review, including human notes");
    expect(text("#memReviewAuto button")).toBe("Turn on the daily review");
    await click(q("#memReviewAuto button"));
    expect(text("#sheet h2")).toBe("Auto-update");
  });

  it("the Review bar says the daily review's time when it is on, and nothing waits but its time", async () => {
    autoAnswers({ settings: { autoReview: "21:30" } });
    await mounted();
    await click("[data-mtab=review]");
    expect(text("#memReviewAuto")).toContain("Daily review at 21:30: all 4 pending note(s), including human notes, go to the workers then.");
    expect(document.querySelector("#memReviewAuto .tag")).toBeNull();
    expect(text("#memReviewAuto button")).toBe("Auto-update…");

    view.unmount();
    autoAnswers({ settings: { autoReview: "21:30" }, inbox: [] });
    await mounted();
    await click("[data-mtab=review]");
    expect(text("#memReviewAuto")).toContain("Daily review at 21:30.");
  });

  it("the sheet turns the daily review on: a time, runs a day and the workers in one PATCH", async () => {
    const { patches, puts } = autoAnswers();
    await mounted();
    await click("#memAutoOpen");
    const on = q<HTMLInputElement>("#memAutoReviewOn");
    expect(on.checked).toBe(false);
    // The time and the runs show only while the switch is on.
    expect(q("#memAutoReviewAt").closest("[hidden]")).not.toBeNull();
    check("#memAutoReviewOn", true);
    expect(q("#memAutoReviewAt").closest("[hidden]")).toBeNull();
    expect(q<HTMLInputElement>("#memAutoReviewRuns").placeholder).toBe("3");

    // A refused word is said in the sheet; nothing was sent for it.
    await click("#memAutoSave");
    expect(text("#memAutoErr")).toBe("Daily review: pick a time of day");
    type("#memAutoReviewAt", "21:30");
    type("#memAutoReviewRuns", "11");
    await click("#memAutoSave");
    expect(text("#memAutoErr")).toBe("Daily review runs: a whole number 1 to 10, or empty for 3");
    type("#memAutoReviewRuns", "5");
    change("#memAutoAgentsMode", "named");
    await click("#memAutoSave");
    expect(text("#memAutoErr")).toBe("Name at least one agent.");
    expect(patches).toHaveLength(0);

    await click('#memAutoAgentsNames [data-magent="lead"]');
    await click("#memAutoSave");
    expect(patches).toEqual([{ autoReview: "21:30", autoReviewRuns: 5, agents: "lead" }]);
    // The sync block was not touched: no PUT.
    expect(puts).toHaveLength(0);
    expect(text("#toast")).toBe("Auto-update saved");
    expect(q("#sheet").hidden).toBe(true);
    // The head says the new time at once.
    expect(text("#memAutoOpen")).toBe("Auto-update · daily 21:30");

    // Off clears the time and hands the runs back to the default.
    await click("#memAutoOpen");
    expect(q<HTMLInputElement>("#memAutoReviewRuns").value).toBe("5");
    check("#memAutoReviewOn", false);
    await click("#memAutoSave");
    expect(patches[1]).toEqual({ autoReview: null, autoReviewRuns: null, agents: "lead" });
    expect(text("#memAutoOpen")).toBe("Auto-update · off");
  });

  it("the sheet says who reviews, what waits, when the clock looks next and how the last review went", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    try {
      vi.setSystemTime(new Date(2026, 9, 5, 9, 0, 0));
      autoAnswers({ settings: { autoReview: "21:30" } });
      await mounted();
      await click("#memAutoOpen");
      const card = text("#memAutoStatus");
      expect(card).toContain("lead");
      expect(card).toContain("the sync block");
      expect(card).toContain("3 AI note(s) · 1 human note(s) — all included in the review");
      expect(card).toContain("today 21:30");
      expect(text('[data-mautorun="r-5"]')).toContain("1 kept, 1 fixed, 1 dropped, 0 merged");
      expect(text('[data-mautorun="r-5"]')).toContain("1 confirmed");
    } finally {
      vi.useRealTimers();
    }
  });

  it("the sheet with no review yet says none, and the next run off while the review is off", async () => {
    autoAnswers({ runs: [] });
    await mounted();
    await click("#memAutoOpen");
    expect(text("#memAutoStatus")).toContain("Next reviewoff");
    expect(text("#memAutoStatus")).toContain("Last reviewnone yet");
  });

  it("Review now queues the full inbox including unchanged and human notes", async () => {
    const { starts } = autoAnswers();
    await mounted();
    await click("#memAutoOpen");
    await click("#memAutoReviewNow");
    expect(starts).toEqual([{ project: "p-1", review: true }]);
    expect(text("#toast")).toBe("Queued review run r-9 - the worker picks it up");
  });

  it("Stop the review stands beside Review now while one runs, says stopping once told, and leaves with it", async () => {
    const running = { ...REVIEWED, id: "r-7", state: "running", startedMs: NOW - 60_000, finishedMs: null, summary: "" };
    autoAnswers({ runs: [running, REVIEWED] });
    const base = override;
    const cancels: string[] = [];
    let state: "running" | "stopping" | "ended" = "running";
    // Synchronous on purpose: a promise is an answer even when it resolves to none.
    override = (c) => {
      if (c.method === "POST" && c.path.startsWith("/api/knowledge/sync/runs/")) {
        cancels.push(c.path + "?" + c.query.toString());
        state = "stopping";
        return [200, { run: running, stopping: true }];
      }
      const answer = base ? base(c) : undefined;
      if (answer && !(answer instanceof Promise) && c.path === "/api/knowledge/sync" && c.method === "GET" && state !== "running") {
        const [status, body] = answer;
        const runs = state === "ended" ? [{ ...running, state: "cancelled", finishedMs: NOW }, REVIEWED] : [running, REVIEWED];
        return [status, { ...(body as Body), runs, stopping: state === "stopping" ? ["r-7"] : [] }];
      }
      return answer;
    };
    await mounted();
    await click("#memAutoOpen");
    expect(text("#memAutoStatus")).toContain("reviewing now");
    expect(q<HTMLButtonElement>("#memAutoReviewNow").disabled).toBe(true);
    expect(q("#memAutoStopReview").hidden).toBe(false);
    expect(text("#memAutoStopReview")).toBe("Stop the review");

    await click("#memAutoStopReview");
    expect(cancels).toHaveLength(1);
    expect(cancels[0]).toContain("/api/knowledge/sync/runs/r-7/cancel");
    expect(cancels[0]).toContain("p-1");
    expect(text("#toast")).toBe("Run r-7 is stopping: the questions in flight are dropped, the notes it saved stay");
    expect(text("#memAutoStopReview")).toBe("stopping…");
    expect(q<HTMLButtonElement>("#memAutoStopReview").disabled).toBe(true);

    // The run ends cancelled: the next read hides Stop and hands Review now back.
    state = "ended";
    await click("#memAutoStopReview");
    expect(cancels).toHaveLength(1);
    view.unmount();
    await mounted();
    await click("#memAutoOpen");
    expect(q("#memAutoStopReview").hidden).toBe(true);
    expect(q<HTMLButtonElement>("#memAutoReviewNow").disabled).toBe(false);
  });

  it("Stop the review is hidden while no review waits or runs", async () => {
    autoAnswers();
    await mounted();
    await click("#memAutoOpen");
    expect(q("#memAutoStopReview").hidden).toBe(true);
    expect(q<HTMLButtonElement>("#memAutoReviewNow").disabled).toBe(false);
  });

  it("auto sync is the shared block's: a change PUTs it back with every other field kept", async () => {
    const { patches, puts } = autoAnswers();
    await mounted();
    await click("#memAutoOpen");
    expect(q<HTMLInputElement>("#memAutoSyncOn").checked).toBe(true);
    expect(q<HTMLInputElement>("#memAutoSyncInterval").value).toBe("900");
    expect(q("#memAutoSyncUnnamed").hidden).toBe(true);
    expect(text("#memAutoHooks")).toContain("swiss memory sync hooks install");
    expect(text("#memAutoHooks")).toContain("swiss memory hook print claude-code");

    type("#memAutoSyncInterval", "ten");
    await click("#memAutoSave");
    expect(text("#memAutoErr")).toBe("Min interval (s): a whole number");
    expect(patches).toHaveLength(0);

    type("#memAutoSyncInterval", "");
    check("#memAutoSyncOn", false);
    await click("#memAutoSave");
    expect(patches).toHaveLength(1);
    expect(puts).toEqual([{
      revision: 7,
      config: { enabled: true, privacy: "local", sync: { agent: "lead", transcripts: "claude-code", auto: false } },
    }]);
    expect(text("#toast")).toBe("Auto-update saved");
  });

  it("auto sync with nobody named is warned of until the workers are picked; no block only says so", async () => {
    autoAnswers({ config: { transcripts: "claude-code", auto: true }, chosen: { agents: { names: ["cc"], from: "newest" }, lead: { name: null, from: "none" } } });
    await mounted();
    await click("#memAutoOpen");
    expect(q("#memAutoSyncUnnamed").hidden).toBe(false);
    change("#memAutoAgentsMode", "named");
    expect(q("#memAutoSyncUnnamed").hidden).toBe(true);
    change("#memAutoAgentsMode", "inherit");
    check("#memAutoSyncOn", false);
    expect(q("#memAutoSyncUnnamed").hidden).toBe(true);

    view.unmount();
    autoAnswers({ config: null });
    await mounted();
    await click("#memAutoOpen");
    expect(document.querySelector("#memAutoSyncOn")).toBeNull();
    expect(text("#memAutoSyncNone")).toContain("Sync is not set up yet");
  });

  it("nextReview judges the clock as the gateway does", async () => {
    const { nextReview } = await import("../src/views/memory-auto.js");
    const at = (d: number, hh: number, mm: number): number => new Date(2026, 9, d, hh, mm, 0).getTime();
    type Run = Parameters<typeof nextReview>[1][number];
    const run = (o: Body): Run => ({ id: "r", project: "p-1", trigger: "review", dryRun: false, state: "ok", queuedMs: at(5, 21, 31), dueMs: at(5, 21, 31), ...o }) as unknown as Run;
    // Before today's time: today.
    expect(nextReview("21:30", [], 3, at(5, 9, 0))).toEqual({ kind: "today", at: at(5, 21, 30) });
    // Past it with no review since (yesterday's and a dry run do not count): within the minute.
    expect(nextReview("21:30", [run({ queuedMs: at(4, 21, 31) }), run({ dryRun: true })], 3, at(5, 22, 0))).toEqual({ kind: "soon" });
    // One waits or runs: reviewing now.
    expect(nextReview("21:30", [run({ state: "running" })], 3, at(5, 22, 0))).toEqual({ kind: "running" });
    // The newest left questions and the day has runs left: another; at the cap, tomorrow.
    const more = run({ scope: { changed: true, recheck: { more: 40 } } });
    expect(nextReview("21:30", [more], 3, at(5, 22, 0))).toEqual({ kind: "more" });
    expect(nextReview("21:30", [more], 1, at(5, 22, 0))).toEqual({ kind: "tomorrow", at: at(6, 21, 30) });
    // Settled for the day: tomorrow.
    expect(nextReview("21:30", [run({})], 3, at(5, 22, 0))).toEqual({ kind: "tomorrow", at: at(6, 21, 30) });
  });
});

/* ------------------------------------------------------------------ the ask box */

describe("the Memory page - the ask box", () => {
  function change(sel: string, value: string, ev = "change"): void {
    const el = q<HTMLInputElement | HTMLSelectElement>(sel);
    el.value = value;
    el.dispatchEvent(new Event(ev, { bubbles: true }));
  }

  /** The llm plugin's agents, as GET /api/llm/status lists them. */
  const agents = [
    { name: "distiller", runtime: "builtin", provider: "ollama", model: "qwen3.5:4b", timeoutMs: 1_800_000, report: true },
    { name: "cc", runtime: "claude", provider: null, model: null, timeoutMs: 1_800_000, report: true },
  ];

  /** The ask routes: the llm plugin's agents and POST /ask, its bodies collected. */
  function askAnswers(mode: "tools" | "inline" = "tools", err = 0): Body[] {
    const asked: Body[] = [];
    override = (c) => {
      if (c.path === "/api/knowledge/ask" && c.method === "POST") {
        asked.push(c.body || {});
        return err
          ? [err, { error: "no llm agent to ask: add one on the LLM Agents page" }]
          : (c.body || {}).agent === "cc"
            ? [200, { answer: "Refunds reuse the charge's key.", agent: "claude:cc", model: null, mode: "inline" }]
            : [200, { answer: "Retries are keyed per cart (m-1).", agent: "builtin:distiller", model: "qwen3.5:4b", mode }];
      }
      if (c.path === "/api/llm/status") return [200, { serving: true, providers: [], embedding: null, agents }];
      return undefined;
    };
    return asked;
  }

  it("opens visibly, refuses an empty question, and shows the answer with its agent", async () => {
    const asked = askAnswers();
    await mounted();
    await click("#memAskOpen");
    expect(q<HTMLElement>("#sheet").hidden).toBe(false);
    expect(getComputedStyle(q<HTMLElement>("#sheet")).display).not.toBe("none");
    expect(text("#sheet h2")).toBe("Ask swiss's memory");
    // The agent picker offers the layers first, then the llm plugin's agents.
    expect([...q<HTMLSelectElement>("#memAskAgent").options].map((o) => o.value)).toEqual(["", "distiller", "cc"]);
    await click("#memAskRun");
    expect(text("#memAskErr")).toBe("The question says nothing.");
    expect(asked).toHaveLength(0);
    type("#memAskQ", "what did we decide about retries?");
    await click("#memAskRun");
    expect(asked).toEqual([{ project: "p-1", question: "what did we decide about retries?" }]);
    expect(text("#memAskText")).toContain("Retries are keyed per cart (m-1).");
    expect(text("#memAskMeta")).toContain("builtin:distiller");
    expect(text("#memAskMeta")).toContain("working the memory tools itself");
    // A named agent rides the body; the inline mode is said in its own words.
    change("#memAskAgent", "cc");
    type("#memAskQ", "and refunds?");
    await click("#memAskRun");
    expect(asked[1]).toEqual({ project: "p-1", question: "and refunds?", agent: "cc" });
    expect(text("#memAskText")).toContain("Refunds reuse the charge's key.");
    expect(text("#memAskMeta")).toContain("claude:cc");
    expect(text("#memAskMeta")).toContain("from the memory the gateway fetched");
  });

  it("an offline answer is said in the sheet, the sheet stays open to fix it", async () => {
    const asked = askAnswers("tools", 503);
    await mounted();
    await click("#memAskOpen");
    type("#memAskQ", "what did we decide about retries?");
    await click("#memAskRun");
    expect(asked).toHaveLength(1);
    expect(text("#memAskErr")).toContain("no llm agent to ask");
    expect(q<HTMLElement>("#sheet").hidden).toBe(false);
    location.hash = "";
  });
});

/* ------------------------------------------------------------------ previews */

describe("the Memory page - previews", () => {
  const ROOT = "C:/src/swiss";
  const STORE = "crates/swiss-memory/src/store.rs";
  const STORE_TEXT = "pub fn open() {\n    let db = sqlite();\n}\n";
  const SPEC_TEXT = "# SPEC\n\nintro\n\n## §memory.preview\n\nPreviews are read-only.\n";
  const PATCH = [
    "diff --git a/" + STORE + " b/" + STORE, "--- a/" + STORE, "+++ b/" + STORE,
    "@@ -1,2 +1,3 @@", " pub fn open() {", "+    let db = sqlite();", " }", "",
  ].join("\n");
  const SPEC_PATCH = ["diff --git a/docs/SPEC.md b/docs/SPEC.md", "new file mode 100644", "--- /dev/null", "+++ b/docs/SPEC.md", "@@ -0,0 +1 @@", "+# SPEC", ""].join("\n");
  const NEW = { hash: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", short: "a1b2c3d", author: "young1lin", dateMs: NOW - 600_000, subject: "memory: the store opens once" };
  const OLD = { hash: "f00dbabe00000000000000000000000000000000", short: "f00dbab", author: "young1lin", dateMs: NOW - 3 * 86_400_000, subject: "memory: SQLite over redb" };

  function previews(c: Call): Answer | undefined {
    const p = c.path.replace(/^\/api\/knowledge/, "");
    const path = c.query.get("path") || "";
    if (p === "/records/m-1" && c.method === "GET") {
      return [200, {
        record: {
          ...record("m-1", "SQLite over redb"),
          anchors: [
            { path: STORE, glob: false, hash: "ab12", state: "ok" },
            { path: "crates/swiss-memory/src/*.rs", glob: true, hash: "", state: "ok" },
            { path: "src/plugins/memory_preview.rs", glob: false, hash: "cd34", state: "changed" },
          ],
          refs: ["commit " + NEW.short, "docs/SPEC.md: §memory.preview", "https://example.com/adr", "ADR-031"],
        },
        revisions: REVISIONS,
      }];
    }
    if (p === "/preview/file") {
      const t = path === "docs/SPEC.md" ? SPEC_TEXT : STORE_TEXT;
      return [200, { root: ROOT, path, kind: "file", size: t.length, modifiedMs: NOW - 60_000, binary: false, truncated: false, lines: t.split("\n").length - 1, text: t }];
    }
    if (p === "/preview/files") {
      return [200, { root: ROOT, glob: c.query.get("glob"), files: ["crates/swiss-memory/src/lib.rs", STORE], truncated: false, git: true }];
    }
    if (p === "/preview/log") return [200, { root: ROOT, path, commits: [NEW, OLD], more: false, dirty: true, untracked: false }];
    if (p === "/preview/commit") {
      const only = c.query.get("path");
      return [200, {
        root: ROOT, ...NEW, body: "memory: the store opens once\n\nTwo processes, one file.", parents: [OLD.hash],
        files: [{ path: STORE, status: "M", additions: 1, deletions: 0 }, { path: "docs/SPEC.md", status: "A", additions: 1, deletions: 0 }],
        path: only, patch: only ? PATCH : PATCH + SPEC_PATCH, truncated: false,
      }];
    }
    if (p === "/preview/diff") return [200, { root: ROOT, path, patch: PATCH, truncated: false, untracked: false }];
    return undefined;
  }

  async function detail(): Promise<HTMLElement> {
    override = previews;
    // The served shell's #sheet starts hidden (the skeleton copies ids only): a preview opened
    // with no sheet showing takes it, one opened from a sheet stacks.
    q("#sheet").hidden = true;
    await mounted();
    return q("#memDetail article[data-mdoc=m-1]");
  }

  const stacked = (): HTMLElement | null => document.querySelector<HTMLElement>(".backdrop.stacked");
  const paths = (root: ParentNode): string[] => Array.from(root.querySelectorAll<HTMLElement>(".diff-file")).map((d) => d.dataset.dpath || "");

  it("an anchor opens the file as it is now, in a wide sheet, numbered", async () => {
    const doc = await detail();
    await click(q(`.ref[data-mpvfile="${STORE}"]`, doc));
    expect(sent("GET", "/preview/file").map((c) => Object.fromEntries(c.query))).toEqual([{ project: "p-1", path: STORE }]);
    const sh = q("#sheet .sheet.wide");
    expect(q(".sheet-head h2", sh).textContent).toBe("store.rs");
    expect(sh.querySelectorAll(".jv-line").length).toBe(3);
    expect(q(".jv-line:nth-child(2)", sh).textContent).toContain("let db = sqlite();");
    expect(sh.querySelectorAll("[data-mpvtab]").length).toBe(2);
    await click(q("[data-mpvclose]", sh));
    expect(q("#sheet").hidden).toBe(true);
  });

  it("a file reply that lands after its sheet closed never reopens it", async () => {
    let land: (a: Answer) => void = () => undefined;
    override = (c): Answer | Promise<Answer> | undefined =>
      c.path === "/api/knowledge/preview/file" ? new Promise<Answer>((r) => { land = r; }) : previews(c);
    q("#sheet").hidden = true;
    await mounted();
    const doc = q("#memDetail article[data-mdoc=m-1]");
    await click(q(`.ref[data-mpvfile="${STORE}"]`, doc));
    expect(q("#sheet").hidden).toBe(false); // the loading sheet
    await click("#sheet [data-mpvclose]");
    expect(q("#sheet").hidden).toBe(true);
    land([200, { root: ROOT, path: STORE, kind: "file", size: STORE_TEXT.length, modifiedMs: NOW - 60_000, binary: false, truncated: false, lines: STORE_TEXT.split("\n").length - 1, text: STORE_TEXT }]);
    await settle();
    expect(q("#sheet").hidden).toBe(true); // the late reply did not reopen the sheet
    expect(document.querySelector("#sheet .sheet")).toBeNull(); // and painted nothing into it
  });

  it("a file reply that lands over a REPLACED sheet never clobbers the new one", async () => {
    let land: (a: Answer) => void = () => undefined;
    override = (c): Answer | Promise<Answer> | undefined =>
      c.path === "/api/knowledge/preview/file" && c.query.get("path") === STORE ? new Promise<Answer>((r) => { land = r; }) : previews(c);
    q("#sheet").hidden = true;
    await mounted();
    const doc = q("#memDetail article[data-mdoc=m-1]");
    await click(q(`.ref[data-mpvfile="${STORE}"]`, doc));
    expect(q("#sheet").hidden).toBe(false); // store.rs, loading
    await click("#sheet [data-mpvclose]");
    // The user moved on: another preview (fast) took the sheet.
    await click(q(`.ref[data-mpvfile="docs/SPEC.md"]`, doc));
    expect(text("#sheet .sheet-head h2")).toBe("SPEC.md");
    land([200, { root: ROOT, path: STORE, kind: "file", size: STORE_TEXT.length, modifiedMs: NOW - 60_000, binary: false, truncated: false, lines: STORE_TEXT.split("\n").length - 1, text: STORE_TEXT }]);
    await settle();
    expect(text("#sheet .sheet-head h2")).toBe("SPEC.md"); // the stale preview painted nothing
    expect(document.querySelector("#sheet .md")).not.toBeNull(); // the new sheet's Markdown body is intact
  });
  it("History: the commits after the note are tagged, the uncommitted change one row away", async () => {
    const doc = await detail();
    await click(q(`.ref[data-mpvfile="${STORE}"]`, doc));
    await click("#sheet [data-mpvtab=history]");
    expect(sent("GET", "/preview/log").map((c) => c.query.get("path"))).toEqual([STORE]);
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#sheet [data-mpvcommit]"));
    expect(rows.map((r) => r.dataset.mpvcommit)).toEqual([NEW.hash, OLD.hash]);
    // The note was saved an hour ago: the commit ten minutes ago may have made it wrong.
    const tagged = (hash: string): string | null => {
      const r = q(`#sheet [data-mpvcommit="${hash}"]`).closest(".lrow");
      return r?.querySelector(".tag.warn")?.textContent ?? null;
    };
    expect(tagged(NEW.hash)).toBe("after the note");
    expect(tagged(OLD.hash)).toBeNull();

    await click(`#sheet [data-mpvdiff="${STORE}"]`);
    expect(sent("GET", "/preview/diff").map((c) => c.query.get("path"))).toEqual([STORE]);
    const over = stacked();
    expect(over).not.toBeNull();
    expect(q(".dl.add", over!).textContent).toBe("+    let db = sqlite();\n");
  });

  it("a commit from History stacks over the file, for that file; Show every file asks again; Escape walks back one layer", async () => {
    const doc = await detail();
    await click(q(`.ref[data-mpvfile="${STORE}"]`, doc));
    await click("#sheet [data-mpvtab=history]");
    await click(`#sheet [data-mpvcommit="${NEW.hash}"]`);
    expect(sent("GET", "/preview/commit").map((c) => Object.fromEntries(c.query))).toEqual([{ project: "p-1", rev: NEW.hash, path: STORE }]);
    let over = stacked()!;
    expect(q(".sheet-head h2", over).textContent).toBe(NEW.subject);
    expect(paths(over)).toEqual([STORE]);
    expect(over.querySelectorAll("[data-mpvjump]").length).toBe(2);

    await click(q("[data-mpvall]", over));
    expect(sent("GET", "/preview/commit").at(-1)?.query.has("path")).toBe(false);
    over = stacked()!;
    expect(paths(over)).toEqual([STORE, "docs/SPEC.md"]);
    expect(over.querySelector("[data-mpvall]")).toBeNull();

    q(".sheet-head h2", over).dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(stacked()).toBeNull();
    expect(q("#sheet").hidden).toBe(false);
  });

  it("a changed anchor opens on its History; a glob lists its files, one opens, Back returns", async () => {
    const doc = await detail();
    await click(q('.ref[data-mpvfile="src/plugins/memory_preview.rs"]', doc));
    expect(sent("GET", "/preview/file").length).toBe(0);
    expect(sent("GET", "/preview/log").map((c) => c.query.get("path"))).toEqual(["src/plugins/memory_preview.rs"]);
    await click("#sheet [data-mpvclose]");

    await click(q('.ref[data-mpvfile="crates/swiss-memory/src/*.rs"]', doc));
    expect(sent("GET", "/preview/files").map((c) => c.query.get("glob"))).toEqual(["crates/swiss-memory/src/*.rs"]);
    expect(document.querySelector("#sheet [data-mpvtab]")).toBeNull();
    expect(Array.from(document.querySelectorAll<HTMLElement>("#sheet [data-mpvopen]")).map((b) => b.dataset.mpvopen)).toEqual(["crates/swiss-memory/src/lib.rs", STORE]);
    await click(`#sheet [data-mpvopen="${STORE}"]`);
    expect(sent("GET", "/preview/file").map((c) => c.query.get("path"))).toEqual([STORE]);
    expect(text("#sheet .sheet-head h2")).toBe("store.rs");
    await click("#sheet [data-mpvback]");
    expect(sent("GET", "/preview/files").length).toBe(2);
    expect(text("#sheet .sheet-head h2")).toBe("crates/swiss-memory/src/*.rs");
    expect(document.querySelector("#sheet [data-mpvback]")).toBeNull();
  });

  it("refs: a commit opens the commit, `path: heading` the file at the heading, a URL a new tab, anything else stays text", async () => {
    const doc = await detail();
    await click(q(`.ref[data-mpvcommit="${NEW.short}"]`, doc));
    expect(sent("GET", "/preview/commit").map((c) => Object.fromEntries(c.query))).toEqual([{ project: "p-1", rev: NEW.short }]);
    expect(stacked()).toBeNull();
    expect(q("#sheet .sheet.wide .diff")).not.toBeNull();
    await click("#sheet [data-mpvclose]");

    await click(q('.ref[data-mpvfile="docs/SPEC.md"]', doc));
    expect(sent("GET", "/preview/file").map((c) => c.query.get("path"))).toEqual(["docs/SPEC.md"]);
    // A Markdown file is drawn; its source is one toggle away, the heading's line marked.
    expect(q("#sheet .md h2").textContent).toBe("§memory.preview");
    await click("#sheet [data-mpvsource]");
    expect(q("#sheet .jv-line.hit").textContent).toContain("## §memory.preview");

    const link = q<HTMLAnchorElement>('a[href="https://example.com/adr"]', doc);
    expect(link.target).toBe("_blank");
    expect(link.rel).toBe("noopener noreferrer");
    const plain = Array.from(doc.querySelectorAll("code")).find((c) => c.textContent === "ADR-031");
    expect(plain).toBeDefined();
    expect(plain!.closest("button")).toBeNull();
  });

  it("refTarget reads a ref as a place; findLine prefers a Markdown heading", async () => {
    const pv = await import("../src/views/memory-preview.js");
    expect(pv.refTarget("commit 9569b75")).toEqual({ commit: "9569b75" });
    expect(pv.refTarget("9569B75")).toEqual({ commit: "9569b75" });
    expect(pv.refTarget("1234567")).toBeNull();
    expect(pv.refTarget("https://example.com/a?b=1")).toEqual({ url: "https://example.com/a?b=1" });
    expect(pv.refTarget("docs/SPEC.md: §memory.preview")).toEqual({ path: "docs/SPEC.md", find: "§memory.preview" });
    expect(pv.refTarget("src/plugins/")).toEqual({ path: "src/plugins" });
    expect(pv.refTarget("README.md")).toEqual({ path: "README.md" });
    for (const t of ["ADR-031", "PR #12", "/etc/passwd", "commit abc", "two words.md"]) expect(pv.refTarget(t)).toBeNull();

    const md = "# A\nsee memory here\n## Memory\n";
    expect(pv.findLine(md, "memory", true)).toBe(3);
    expect(pv.findLine(md, "memory", false)).toBe(2);
    expect(pv.findLine(md, "nowhere", true)).toBeNull();
  });
});
