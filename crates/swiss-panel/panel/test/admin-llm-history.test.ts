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

/* The llm plugin fourth page (#llm-history, SPEC §llm.history): the calls and runs lists
   read from /api/llm/history/*. A canned host answers the reads with the wire shapes
   history.rs produces; the suites pin the list shapes, the kind filter and search as QUERY
   parameters (the server filters, never the browser), the body expand sheets, the run sheet
   as a trace - prompt outline, steps that open in place, answer, the runs it started - in
   one fetch with no per-turn round trips, the words a run is named by, the pager, and the
   honest 503 (plugin off). */

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type * as HistoryView from "../src/views/llm-history.js";

interface Call { method: string; path: string }
type Answer = [number, unknown];

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => { return '<div id="' + id + '"></div>'; }).join("");
}

/* --- the canned wire, field-for-field from swiss-llm history.rs --------------------------------- */

const CALL = {
  id: 7, at: "2026-10-09T12:00:00.000Z", kind: "chat", provider: "glm", model: "glm-5.3",
  agent: "distiller", ok: true, ms: 812, status: null,
  tokensIn: 10, tokensOut: 4, tokensCacheRead: 0, tokensCacheWrite: 0,
  requestId: "swiss-1", preview: '{\"request\":{\"prompt\":\"hi\"}}', chars: 90, hasBody: true,
};
const EMBED = {
  id: 8, at: "2026-10-09T12:01:00.000Z", kind: "embedding", provider: "ollama", model: "bge-m3",
  agent: null, ok: false, ms: 15, status: null,
  tokensIn: 0, tokensOut: 0, tokensCacheRead: 0, tokensCacheWrite: 0,
  requestId: null, preview: '{\"response\":{\"error\":\"no route\"}}', chars: 40, hasBody: true,
};
const TASK_CALL = {
  ...CALL, id: 11, at: "2026-10-09T12:02:00.000Z", agent: "recheck",
  task: "memory.recheck", subject: "orders (6 notes)",
};
const RUN = {
  id: 3, at: "2026-10-09T12:00:00.000Z", atEnd: "2026-10-09T12:00:20.000Z", agent: "distiller",
  runtime: "builtin", provider: "glm", model: "glm-5.3", ok: true, ms: 20310, turns: 5,
};
/* A run that says what it was for: the recheck task and its subject, the memory job that
   started it, three rounds over two tools, one child run. The child sits under its parent
   (parent 9), never on the page's own list - the wire nests it. */
const TASK_RUN = {
  id: 9, at: "2026-10-09T12:02:10.000Z", atEnd: "2026-10-09T12:03:11.000Z", agent: "recheck",
  runtime: "builtin", provider: "glm", model: "glm-5.3", ok: true, ms: 61000, turns: 8,
  task: "memory.recheck", subject: "orders (6 notes)", source: "memory:12", parent: null,
  kind: null, head: null, rounds: 3, tokensIn: 5200, tokensOut: 480, tokensCacheRead: 3000,
  children: 1, tools: { read: 2, delegate: 1 },
};
const CHILD_RUN = {
  id: 10, at: "2026-10-09T12:02:40.000Z", atEnd: "2026-10-09T12:02:55.000Z", agent: "worker",
  runtime: "builtin", provider: "glm", model: "glm-5.3", ok: true, ms: 15000, turns: 3,
  task: "memory.task", subject: "read a.java", source: "memory:12", parent: 9,
  kind: null, head: "read a.java", rounds: 1, tokensIn: 800, tokensOut: 90, tokensCacheRead: 0,
  children: 0, tools: {},
};
const DIALOGUE = {
  id: 3, at: RUN.at, atEnd: RUN.atEnd, agent: RUN.agent, runtime: RUN.runtime,
  provider: RUN.provider, model: RUN.model, ok: true, ms: RUN.ms,
  turns: [
    { seq: 1, at: RUN.at, role: "system", text: "be brief" },
    { seq: 2, at: RUN.at, role: "user", text: "read the notes and file a proposal" },
    { seq: 3, at: RUN.at, role: "thinking", text: "weighing the notes first" },
    { seq: 4, at: RUN.at, role: "final", text: '{\"type\":\"result\",\"result\":\"proposal filed\"}' },
  ],
  calls: [{
    ...CALL, turn: 1, prompt: "read the notes", answer: ["done"],
    stopReason: "tool_call: tool_use",
    toolCalls: [{ name: "read", arguments: '{\"path\":\"notes.md\"}', callId: "call-7" }],
  }],
};
const TASK_DIALOGUE = {
  ...TASK_RUN, turns: [
    { seq: 1, at: TASK_RUN.at, role: "user", text: "judge these notes against the code" },
    { seq: 2, at: TASK_RUN.atEnd, role: "final", text: "all six hold" },
  ], calls: [], children: [CHILD_RUN],
};
const CHILD_DIALOGUE = {
  ...CHILD_RUN, turns: [
    { seq: 1, at: CHILD_RUN.at, role: "user", text: "read a.java" },
    { seq: 2, at: CHILD_RUN.atEnd, role: "final", text: "it holds A" },
  ], calls: [], parentRun: TASK_RUN,
};
/* A run the conversation never closed (ok NULL, atEnd NULL) and one the reader closed
   without an outcome (ok NULL, atEnd set): the NULL-ok states the lead badges name. */
const OPEN_RUN = {
  id: 5, at: "2026-10-09T12:06:00.000Z", atEnd: null, agent: "distiller",
  runtime: "builtin", provider: "glm", model: "glm-5.3", ok: null, ms: 0, turns: 4,
};
const CLOSED_RUN = {
  id: 6, at: "2026-10-09T12:05:00.000Z", atEnd: "2026-10-09T12:05:30.000Z", agent: "distiller",
  runtime: "builtin", provider: "glm", model: "glm-5.3", ok: null, ms: 0, turns: 2,
};
const OPEN_DIALOGUE = {
  id: 5, at: OPEN_RUN.at, atEnd: null, agent: "distiller", runtime: "builtin",
  provider: "glm", model: "glm-5.3", ok: null, ms: 940,
  turns: [
    { seq: 1, at: OPEN_RUN.at, role: "system", text: "be brief" },
    { seq: 2, at: OPEN_RUN.at, role: "user", text: "read the notes" },
    { seq: 3, at: OPEN_RUN.at, role: "final", text: "Filed, still open." },
  ],
  calls: [],
};
const CLOSED_DIALOGUE = {
  id: 6, at: CLOSED_RUN.at, atEnd: CLOSED_RUN.atEnd, agent: "distiller", runtime: "builtin",
  provider: "glm", model: "glm-5.3", ok: null, ms: 310,
  turns: [
    { seq: 1, at: CLOSED_RUN.at, role: "user", text: "one ask" },
    { seq: 2, at: CLOSED_RUN.at, role: "final", text: "one answer, then closed" },
  ],
  calls: [],
};
/* A memory sync's agent run still working (SPEC §llm.history Live): no outcome yet, its
   live state on the row - round 2, a read_file in flight for 3 s, 65 s in all. Its
   dialogue holds the turns noted so far; once filed it reads whole with no live. */
const LIVE = { elapsedMs: 65_000, round: 2, step: "tool" as const, tool: "read_file" as string | null, stepMs: 3000, streamed: 0 };
const LIVE_FACTS = {
  id: 12, at: "2026-10-09T12:07:00.000Z", atEnd: null as string | null, agent: "distiller",
  runtime: "builtin", provider: "glm", model: "glm-5.3", ok: null as boolean | null, ms: 0,
  task: "memory.sync", subject: "shop", source: "memory:r-a1e84e8c", parent: null,
  kind: null, head: "summarize the commits", rounds: 1, tokensIn: 1200, tokensOut: 80, tokensCacheRead: 0,
};
const LIVE_RUN = { ...LIVE_FACTS, turns: 6, children: 0, tools: { read_file: 2 }, live: LIVE };
const LIVE_TURNS = [
  { seq: 1, at: LIVE_RUN.at, role: "user", text: "summarize the commits" },
  { seq: 2, at: LIVE_RUN.at, role: "thinking", text: "reading a first" },
  { seq: 3, at: LIVE_RUN.at, role: "assistant", text: 'tool call read_file [c1]: {"path":"a.rs"}' },
  { seq: 4, at: LIVE_RUN.at, role: "tool", text: "read_file [c1]: fn a() {}" },
  { seq: 5, at: LIVE_RUN.at, role: "thinking", text: "now b" },
  { seq: 6, at: LIVE_RUN.at, role: "assistant", text: 'tool call read_file [c2]: {"path":"b.rs"}' },
];
const LIVE_DIALOGUE = { ...LIVE_FACTS, turns: LIVE_TURNS, calls: [], children: [], live: LIVE };
const FILED_DIALOGUE = {
  ...LIVE_FACTS, ok: true, atEnd: "2026-10-09T12:08:10.000Z", ms: 70_000, children: [],
  turns: [
    ...LIVE_TURNS,
    { seq: 7, at: LIVE_RUN.at, role: "tool", text: "read_file [c2]: fn b() {}" },
    { seq: 8, at: LIVE_RUN.at, role: "final", text: "nothing to file" },
  ],
  calls: [],
};

let calls: Call[] = [];
let off = false;
let override: ((c: Call) => Answer | undefined) | null = null;

function answer(c: Call): Answer {
  const route = c.method + " " + c.path.split("?")[0];
  const q = new URLSearchParams(c.path.split("?")[1] || "");
  if (off) return [503, { error: "the llm history is not open" }];
  if (override) {
    const custom = override(c);
    if (custom) return custom;
  }
  if (route === "GET /api/llm/history/calls") {
    let list = [CALL, EMBED, TASK_CALL];
    if (q.get("kind") === "chat") list = [CALL];
    if (q.get("kind") === "embedding") list = [EMBED];
    if (q.get("q") === "glm") list = [CALL];
    // more: true by default so the pager's Older is live; the disabled-contract
    // test below overrides to the end of the list.
    return [200, { calls: list, page: Number(q.get("page") || 0), pageSize: 20, more: true }];
  }
  if (route === "GET /api/llm/history/calls/7/body") return [200, { id: 7, body: '{"request":{"prompt":"hi","turn":null},"response":{"texts":["a port is a loopback address"],"thinking":["weighing it first"],"cut":false}}', bodyGone: false }];
  if (route === "GET /api/llm/history/runs") return [200, { runs: [TASK_RUN, OPEN_RUN, CLOSED_RUN, RUN], page: Number(q.get("page") || 0), pageSize: 20, more: false }];
  if (route === "GET /api/llm/history/runs/9/dialogue") return [200, TASK_DIALOGUE];
  if (route === "GET /api/llm/history/runs/10/dialogue") return [200, CHILD_DIALOGUE];
  if (route === "GET /api/llm/history/runs/3/dialogue") return [200, DIALOGUE];
  if (route === "GET /api/llm/history/runs/5/dialogue") return [200, OPEN_DIALOGUE];
  if (route === "GET /api/llm/history/runs/6/dialogue") return [200, CLOSED_DIALOGUE];
  if (route === "GET /api/llm/history/turns/3/2/body") return [200, { runId: 3, seq: 2, body: "read the notes and file a proposal", bodyGone: false }];
  if (route === "GET /api/llm/history/turns/3/3/body") return [200, { runId: 3, seq: 3, body: "weighing the notes before I answer", bodyGone: false }];
  if (route === "GET /api/llm/history/turns/3/4/body") return [200, { runId: 3, seq: 4, body: '{"type":"result","result":"proposal filed"}', bodyGone: false }];
  return [404, { error: "no canned answer for " + route }];
}

/* --- the DOM side ------------------------------------------------------------------------------ */

let view: typeof HistoryView;

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, opts?: RequestInit): Promise<unknown> => {
      const call: Call = { method: (opts && opts.method) || "GET", path: String(url) };
      calls.push(call);
      const [status, json] = answer(call);
      return Promise.resolve({ status, ok: status >= 200 && status < 300, json: () => Promise.resolve(json) });
    },
  });
  document.body.innerHTML = shellSkeleton();
  view = await import("../src/views/llm-history.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  calls = [];
  off = false;
  override = null;
});

afterEach(() => { view.unmount(); });

const settle = async (): Promise<void> => {
  for (let i = 0; i < 8; i++) await new Promise((r) => { setTimeout(r, 0); });
};

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = document.querySelector<T>(sel);
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

/** Click a floating menu row by its label (popupMenu builds #menu of buttons). */
async function clickMenu(label: string): Promise<void> {
  const rows = Array.from(document.querySelectorAll<HTMLElement>("#menu button"));
  const row = rows.find((r) => { return r.textContent === label; });
  if (!row) throw new Error("no menu row " + label);
  await click(row);
}

const mount = async (): Promise<void> => { await view.mount(); await settle(); };

describe("llm history page", () => {
  it("a restored conversation's inspectors work without History being mounted", async () => {
    const restored = { ...DIALOGUE, calls: DIALOGUE.calls.map((c) => ({ ...c, kind: "chat" as const, runId: 3 })) };
    q("#pane").appendChild(view.conversationNode(restored));
    await click("#pane .fl-sum");
    await click("#pane [data-hcall='7']");
    expect(text("#sheet")).toContain("Call #7");
    await click("#sheet [data-hrun='3']");
    expect(text("#sheet")).toContain("read the notes and file a proposal");
  });

  it("a compacted conversation opens on one quiet line, its checkpoint folded under it, then carries on", async () => {
    const compacted = { ...OPEN_DIALOGUE, id: 9, source: "compact:5", turns: [
      { seq: 1, at: OPEN_RUN.at, role: "system", text: "be brief" },
      { seq: 2, at: OPEN_RUN.at, role: "compact", text: "Write the checkpoint." },
      { seq: 3, at: OPEN_RUN.at, role: "final", text: "## Requests and Intent\nread the notes" },
      { seq: 4, at: OPEN_RUN.at, role: "user", text: "and now?" },
      { seq: 5, at: OPEN_RUN.at, role: "final", text: "Now this." },
    ] };
    q("#pane").appendChild(view.conversationNode(compacted));
    const line = q("#pane [data-lhcompact='0']");
    expect(line.querySelector(".fl-facts")?.textContent).toBe("Conversation compacted");
    const checkpoint = line.querySelector<HTMLDetailsElement>("details");
    expect(checkpoint?.querySelector("summary")?.textContent).toContain("Checkpoint");
    expect(checkpoint?.open, "folded").toBe(false);
    expect(checkpoint?.textContent).toContain("Requests and Intent");
    // The instruction is the call's, never a turn of the reader's; the next ask reads as one.
    expect(text("#pane")).not.toContain("Write the checkpoint.");
    expect(Array.from(document.querySelectorAll("#pane .tc-turn.user")).map((t) => t.textContent)).toEqual([expect.stringContaining("and now?")]);
    expect(text("#pane")).toContain("Now this.");
  });

  it("reads as a conversation with the user's prompt on the right and the work on the left", async () => {
    await mount();
    await click("[data-hrun='3']");
    expect(q("#sheet .tc-turn.user").textContent).toContain("read the notes");
    expect(q("#sheet .tc-turn.assistant [data-lhsteps]")).toBeTruthy();
    expect(q("#sheet .tc-turn.assistant [data-lhanswer]")).toBeTruthy();
    expect(q<HTMLDetailsElement>("#sheet [data-lhprocess]").open).toBe(false);
  });

  it("a large prompt hides its outline behind one fold instead of dozens of headings", async () => {
    const prompt = "Review this project.\n" + Array.from({ length: 40 }, (_, i) => "## Context " + i + "\n" + "material ".repeat(80)).join("\n");
    override = (c) => c.path === "/api/llm/history/runs/3/dialogue"
      ? [200, { ...DIALOGUE, turns: DIALOGUE.turns.map((t) => t.role === "user" ? { ...t, text: prompt } : t) }]
      : undefined;
    await mount();
    await click("[data-hrun='3']");
    const user = q("#sheet .tc-turn.user");
    const folds = Array.from(user.querySelectorAll("details"));
    expect(folds.filter((f) => !f.parentElement?.closest("details")).length).toBe(1);
    expect(folds[0].open).toBe(false);
    expect(user.textContent).toContain("Review this project.");
    folds[0].open = true;
    expect(user.textContent).toContain("Context 39");
  });

  it("lists calls and runs from the two reads", async () => {
    await mount();
    expect(text("#pane")).toContain("glm / glm-5.3");
    expect(text("#pane")).toContain("distiller");
    expect(calls.some((c) => { return c.path === "/api/llm/history/calls?page=0&pageSize=20"; })).toBe(true);
    expect(calls.some((c) => { return c.path === "/api/llm/history/runs?page=0&pageSize=20"; })).toBe(true);
  });

  it("an agent-less call says a panel test, a failed one shows its tag", async () => {
    await mount();
    const failedRow = Array.from(document.querySelectorAll("[data-hcall]")).find((r) => { return r.textContent.includes("bge-m3"); });
    expect(failedRow).toBeTruthy();
    expect(failedRow!.textContent).toContain("ollama / bge-m3");
    expect(failedRow!.textContent).toContain("no route");
    expect(failedRow!.textContent).toContain("a panel test"); // the agent-less row says who it was
    const okRow = Array.from(document.querySelectorAll("[data-hcall]")).find((r) => { return r.textContent.includes("glm /"); });
    expect(okRow!.textContent).toContain("distiller");
    // The outcome is the panel's state dot: green answered, red failed, word in the tooltip.
    expect(okRow!.querySelector(".dot.up")).toBeTruthy();
    expect(failedRow!.querySelector(".dot.down")).toBeTruthy();
    expect((failedRow!.querySelector(".dot") as HTMLElement).title).toBe("failed");
  });

  it("a call row opens the sheet and expands the sealed body", async () => {
    await mount();
    await click("[data-hcall='7']");
    const sheetHost = q("#sheet");
    expect(sheetHost.hidden).toBe(false);
    expect(sheetHost.textContent).toContain("glm / glm-5.3");
    await settle();
    // The body opens as the detail viewer (request-log's page): the response side
    // first under a Request | Response switch, the thinking greyed above the answer
    // and still inside the JSON (SPEC llm.history, SPEC panel.ui).
    const responseBlock = sheetHost.querySelector('[data-lhbody="response"]');
    expect(responseBlock).toBeTruthy();
    expect(responseBlock!.textContent).toContain("a port is a loopback address");
    const thought = responseBlock!.querySelector(".jv-dim");
    expect(thought!.textContent).toBe("weighing it first");
    // The response block keeps the JSON whole - thinking rides inside it too: the
    // display separates reasoning, the record never edits it out.
    expect(responseBlock!.textContent).toContain('"thinking"');
    expect(responseBlock!.textContent).toContain("weighing it first");
    // The request side is one switch away, and lands when switched to.
    const tab = sheetHost.querySelector("[data-lhside='request']") as HTMLElement;
    expect(tab).toBeTruthy();
    expect(sheetHost.querySelector('[data-lhbody="request"]')).toBeNull();
    tab.click();
    expect(sheetHost.querySelector('[data-lhbody="request"]')!.textContent).toContain('"prompt": "hi"');
    expect(calls.some((c) => { return c.path === "/api/llm/history/calls/7/body"; })).toBe(true);
  });

  it("bodyBlocks: split, single, and verbatim by what the body holds", () => {
    const split = view.bodyBlocks('{"request":{"a":1},"response":{"texts":["x"]}}');
    expect(split.length).toBe(2);
    // Any other JSON is one themed block; non-JSON rides verbatim; empty stays empty.
    expect(view.bodyBlocks('{"type":"result","result":"done"}').length).toBe(1);
    expect(view.bodyBlocks("read the notes")[0] instanceof Node).toBe(true);
    expect(view.bodyBlocks("")[0] instanceof Node).toBe(true);
  });

  it("a run row opens the whole trace in one fetch - prompt, steps and answer", async () => {
    await mount();
    await click("[data-hrun='3']");
    const sheetHost = q("#sheet");
    expect(sheetHost.hidden).toBe(false);
    expect(sheetHost.textContent).toContain("distiller");
    await settle();
    // ONE fetch for the whole run - no per-turn body round trips, no separate calls list:
    // the dialogue route carries turns and round trips both.
    expect(calls.some((c) => { return c.path === "/api/llm/history/runs/3/dialogue"; })).toBe(true);
    expect(calls.some((c) => { return c.path === "/api/llm/history/runs/3/turns"; })).toBe(false);
    expect(calls.some((c) => { return c.path === "/api/llm/history/runs/3/calls"; })).toBe(false);
    expect(calls.some((c) => { return c.path.startsWith("/api/llm/history/turns/3/"); })).toBe(false);
    // The prompt reads in place; the system prompt folds under its own part, unopened.
    expect(sheetHost.textContent).toContain("read the notes and file a proposal");
    const sysFold = sheetHost.querySelector("details.fold");
    expect(sysFold).toBeTruthy();
    expect(sysFold!.querySelector("summary")!.textContent).toContain("System prompt");
    expect(sheetHost.textContent).toContain("be brief");
    // The steps are the flow: the model round is one line that opens in place - a round
    // that reasoned reads as dsh's Think line, the brain and the thought's first line.
    const steps = q("#sheet [data-lhsteps='0']");
    const sum = q("#sheet [data-lhsteps='0'] .fl-sum");
    expect(sum.querySelector(".fl-title")!.textContent).toBe("Think");
    expect(sum.querySelector(".fl-summary")!.textContent).toBe("weighing the notes first");
    expect(sum.querySelector(".fl-glyph use")?.getAttribute("href")).toBe("#i-brain");
    await click(sum);
    const round = steps.querySelector(".fl-step")!;
    expect(round.classList.contains("open")).toBe(true);
    // The opened round: the thinking dimmed, and the way to the round trip that carried it.
    expect(round.querySelector(".fl-body .tc-text.dim")!.textContent).toContain("weighing the notes first");
    await click(round.querySelector("[data-hcall='7']")!);
    await settle();
    expect(calls.some((c) => { return c.path === "/api/llm/history/calls/7/body"; })).toBe(true);
    expect(text(".backdrop.stacked")).toContain("Call #7");
    // Back to the run: the closing answer renders as the themed JSON block.
    await click(".backdrop.stacked .sheet-foot button:last-child");
    await settle();
    expect(text("#sheet")).toContain('"result": "proposal filed"');
  });

  it("a run's sheet lists its provider round trips with their asks; one opens its call sheet", async () => {
    await mount();
    await click("[data-hrun='3']");
    await settle();
    // The round trips ride the SAME dialogue fetch: their rows show the loop turn
    // they served and the prompt they asked, before any body opens.
    const sheetText = q("#sheet").textContent || "";
    expect(sheetText).toContain("Provider round trips in this conversation");
    expect(sheetText).toContain("read the notes");
    expect(sheetText).toContain("#1");
    const rowInRun = Array.from(document.querySelectorAll("#sheet [data-hcall]")).find((r) => {
      return r.getAttribute("data-hcall") === "7";
    });
    expect(rowInRun).toBeTruthy();
    // A round trip inside the run opens the same call sheet a page row does: the
    // sheet resolves it from the run's own fetch, not from the page's list.
    await click(rowInRun!);
    await settle();
    expect(calls.some((c) => { return c.path === "/api/llm/history/calls/7/body"; })).toBe(true);
    const reopened = q(".backdrop.stacked").textContent || "";
    expect(reopened).toContain("Call #7");
    // The detail viewer answers with the response side first; the ask is one
    // switch away (the request-log detail page's Request | Response).
    expect(reopened).toContain("a port is a loopback address");
    q("[data-lhside='request']").click();
    expect(q(".backdrop.stacked").textContent || "").toContain('"prompt": "hi"');
  });

  it.each(["Close", "Escape", "backdrop"])("dismissing a conversation's call via %s preserves its place and disclosures", async (how) => {
    await mount();
    await click("#pane [data-hrun='3']");
    const parent = q("#sheet > .sheet");
    const scroll = q("#sheet .sheet-body");
    const process = q<HTMLDetailsElement>("#sheet [data-lhprocess='0']");
    process.open = true;
    await click("#sheet [data-lhsteps='0'] .fl-sum");
    const round = q("#sheet .fl-step.open");
    const opener = q("#sheet [data-hcall='7']");
    opener.focus();
    scroll.scrollTop = 240;
    const reads = calls.filter((c) => c.path === "/api/llm/history/runs/3/dialogue").length;

    await click(opener);
    const layer = q(".backdrop.stacked");
    expect(layer.textContent).toContain("Call #7");
    expect(layer.querySelector('[data-lhbody="response"]')?.textContent).toContain("a port is a loopback address");
    expect(parent.isConnected).toBe(true);
    if (how === "Escape") {
      layer.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await settle();
    } else if (how === "backdrop") {
      await click(layer);
    } else {
      await click(q(".backdrop.stacked .sheet-foot button:last-child"));
    }

    expect(document.querySelector(".backdrop.stacked")).toBeNull();
    expect(q("#sheet").hidden).toBe(false);
    expect(q("#sheet > .sheet")).toBe(parent);
    expect(scroll.scrollTop).toBe(240);
    expect(process.open).toBe(true);
    expect(round.classList.contains("open")).toBe(true);
    expect(document.activeElement).toBe(opener);
    expect(calls.filter((c) => c.path === "/api/llm/history/runs/3/dialogue")).toHaveLength(reads);
    await click("#sheet .sheet-foot button:last-child");
    expect(q("#sheet").hidden).toBe(true);
  });

  it("the call's run link returns to the preserved conversation without another read", async () => {
    override = (c) => c.path === "/api/llm/history/runs/3/dialogue"
      ? [200, { ...DIALOGUE, calls: DIALOGUE.calls.map((call) => ({ ...call, runId: 3 })) }]
      : undefined;
    await mount();
    await click("#pane [data-hrun='3']");
    const parent = q("#sheet > .sheet");
    await click("#sheet [data-lhsteps='0'] .fl-sum");
    await click("#sheet [data-hcall='7']");
    await click(".backdrop.stacked [data-hrun='3']");
    expect(document.querySelector(".backdrop.stacked")).toBeNull();
    expect(q("#sheet > .sheet")).toBe(parent);
    expect(calls.filter((c) => c.path === "/api/llm/history/runs/3/dialogue")).toHaveLength(1);
  });

  it("a dismissed call's late body cannot overwrite the next inspector", async () => {
    const normalFetch = globalThis.fetch;
    let release = (): void => { throw new Error("no pending body read"); };
    let pending = true;
    globalThis.fetch = (url, opts): ReturnType<typeof fetch> => {
      if (String(url) === "/api/llm/history/calls/7/body" && pending) {
        pending = false;
        return new Promise((resolve) => {
          release = (): void => { resolve(new Response(JSON.stringify({ body: "stale body", bodyGone: false }))); };
        });
      }
      return normalFetch(url, opts);
    };
    try {
      await mount();
      await click("#pane [data-hrun='3']");
      await click("#sheet [data-hcall='7']");
      await click(".backdrop.stacked .sheet-foot button:last-child");
      await click("#sheet [data-hcall='7']");
      const current = q(".backdrop.stacked #lhCallBody");
      expect(current.textContent).toContain("a port is a loopback address");
      release();
      await settle();
      expect(q(".backdrop.stacked #lhCallBody")).toBe(current);
      expect(current.textContent).not.toContain("stale body");
    } finally {
      globalThis.fetch = normalFetch;
    }
  });

  it("the words: what a run is called by, how it ran, who started it, a tool's ask", () => {
    expect(view.taskLabel("memory.recheck")).toBe("Recheck held notes");
    expect(view.taskLabel("memory.unknown")).toBe("memory.unknown"); // a key the page does not know says itself
    expect(view.runWhat({ agent: "a", task: "memory.task", subject: "read a.java", kind: null, head: "" })).toBe("Delegated task · read a.java");
    expect(view.runWhat({ agent: "a", task: null, subject: null, kind: 1, head: null })).toBe("Conversation");
    expect(view.runWhat({ agent: "a", task: null, subject: null, kind: null, head: "line one\nline two" })).toBe("line one");
    expect(view.runWhat({ agent: "solo", task: null, subject: null, kind: null, head: "" })).toBe("solo");
    expect(view.runHow({ runtime: "builtin", rounds: 1, tools: {} })).toBe("single request");
    expect(view.runHow({ runtime: "claude", rounds: 0, tools: {} })).toBe("claude process");
    expect(view.runHow({ runtime: "builtin", rounds: 2, tools: {} })).toBe("2 requests, no tools");
    expect(view.runHow({ runtime: "builtin", rounds: 3, tools: { delegate: 1, read: 2 } })).toBe("tool loop · 3 rounds · read ×2 · delegate ×1");
    expect(view.sourceLabel("memory:12")).toBe("memory run 12");
    expect(view.sourceLabel("memory:r-a1e84e8c")).toBe("memory run r-a1e84e8c"); // a sync run's id is not a number
    expect(view.sourceLabel("cron:nightly")).toBe("cron:nightly");
    expect(view.sourceLabel("compact:41")).toBe("compacted from run 41");
    expect(view.sourceLabel(null)).toBe("");
    expect(view.argLine('{"command":"git log --oneline"}')).toBe("git log --oneline");
    expect(view.argLine('{"path":"src/a.java","from":4,"lines":30}')).toBe("src/a.java :4+30");
    expect(view.argLine('{"path":"src/a.java"}')).toBe("src/a.java");
    expect(view.argLine('{"pattern":"**/*.rs","path":"crates"}')).toBe("**/*.rs  crates");
    expect(view.argLine('{"pattern":"fn main","output_mode":"content"}')).toBe("fn main");
    expect(view.argLine("not json")).toBe("not json");
    // A step line says the call's own description when it gives one, else its arguments.
    expect(view.stepLine('{"command":"git log -1","description":"Show the  last\\ncommit"}')).toEqual({ summary: "Show the last commit", summaryMono: false });
    expect(view.stepLine('{"command":"git log -1","description":"  "}')).toEqual({ summary: "git log -1", summaryMono: true });
    expect(view.stepLine('{"path":"src/a.java"}')).toEqual({ summary: "src/a.java", summaryMono: true });
    expect(view.stepLine("not json")).toEqual({ summary: "not json", summaryMono: true });
  });

  it("outline: a prompt parts by its headings, code fences excepted", () => {
    const o = view.outline("opening words\n# Part one\nbody one\n```\n# not a heading\n```\n## Part two\nbody two");
    expect(o.lead).toBe("opening words");
    expect(o.parts.map((p) => { return p.head; })).toEqual(["Part one", "Part two"]);
    expect(o.parts[0].text).toContain("# not a heading");
    expect(o.parts[1].text).toBe("body two");
  });

  it("traceOf: the loop's notes become rounds - a model turn after a result opens the next", () => {
    const turns = [
      { seq: 1, at: "2026-10-09T12:00:00.000Z", role: "system", text: "be brief" },
      { seq: 2, at: "2026-10-09T12:00:01.000Z", role: "user", text: "check the notes" },
      { seq: 3, at: "2026-10-09T12:00:02.000Z", role: "thinking", text: "start with the file" },
      { seq: 4, at: "2026-10-09T12:00:03.000Z", role: "assistant", text: 'tool call read [c1]: {"path":"a.java"}' },
      { seq: 5, at: "2026-10-09T12:00:04.000Z", role: "tool", text: "read [c1]: class A {}" },
      { seq: 6, at: "2026-10-09T12:00:05.000Z", role: "assistant", text: "the note holds" },
      { seq: 7, at: "2026-10-09T12:00:06.000Z", role: "final", text: "kept" },
    ];
    const trace = view.traceOf(turns, []);
    expect(trace.system).toBe("be brief");
    expect(trace.asks.length).toBe(1);
    expect(trace.asks[0].prompt).toBe("check the notes");
    expect(trace.asks[0].answer).toBe("kept");
    expect(trace.asks[0].rounds.length).toBe(2); // the model spoke again after the result
    const first = trace.asks[0].rounds[0];
    expect(first.thinking).toEqual(["start with the file"]);
    expect(first.tools.length).toBe(1);
    expect(first.tools[0].name).toBe("read");
    expect(first.tools[0].result).toBe("class A {}");
    const second = trace.asks[0].rounds[1];
    expect(second.text).toBe("the note holds");
    expect(second.answered).toBe(true);
  });

  it("a run's row is named by its task and says how it ran; a call under it says the same task", async () => {
    await mount();
    const runRow = q("[data-hrun='9']");
    expect(runRow.textContent).toContain("Recheck held notes · orders (6 notes)");
    expect(runRow.textContent).toContain("tool loop · 3 rounds · read ×2 · delegate ×1");
    expect(runRow.textContent).toContain("5.2K / 480");
    const taskCall = Array.from(document.querySelectorAll("[data-hcall]")).find((r) => { return r.getAttribute("data-hcall") === "11"; });
    expect(taskCall!.textContent).toContain("recheck · Recheck held notes · orders (6 notes)");
    // The child never sits on the page's own list: the wire nests it under its parent.
    expect(document.querySelector("[data-hrun='10']")).toBe(null);
  });

  it("marks structured shell failures, PowerShell's on Windows too, while a successful job cancellation stays unflagged", async () => {
    await mount();
    override = (c) => c.path === "/api/llm/history/runs/3/dialogue" ? [200, {
      ...DIALOGUE, calls: [], turns: [
        { seq: 1, at: RUN.at, role: "user", text: "check commands" },
        { seq: 2, at: RUN.at, role: "assistant", text: 'tool call bash [c1]: {"command":"exit 7"}' },
        { seq: 3, at: RUN.at, role: "tool", text: 'bash [c1]: {"state":"failed","exitCode":7}' },
        { seq: 4, at: RUN.at, role: "assistant", text: 'tool call job_output [c2]: {"job_id":0}' },
        { seq: 5, at: RUN.at, role: "tool", text: 'job_output [c2]: {"state":"timeout"}' },
        { seq: 6, at: RUN.at, role: "assistant", text: 'tool call job_kill [c3]: {"job_id":1}' },
        { seq: 7, at: RUN.at, role: "tool", text: 'job_kill [c3]: {"state":"canceled"}' },
        { seq: 8, at: RUN.at, role: "assistant", text: 'tool call powershell [c4]: {"command":"exit 3"}' },
        { seq: 9, at: RUN.at, role: "tool", text: 'powershell [c4]: {"state":"failed","exitCode":3}' },
      ],
    }] : undefined;
    await click("[data-hrun='3']");
    await settle();
    expect(document.querySelectorAll("#sheet .fl-flag").length).toBe(3);
  });

  it("a run's sheet says who started it and lists the runs it started; each opens its own", async () => {
    await mount();
    await click("[data-hrun='9']");
    await settle();
    const sheetText = text("#sheet");
    expect(sheetText).toContain("Recheck held notes · orders (6 notes)");
    expect(sheetText).toContain("memory run 12"); // the source, in words
    expect(sheetText).toContain("started 1 runs");
    expect(sheetText).toContain("Runs it started (1)");
    const child = q("#sheet [data-hrun='10']");
    expect(child.textContent).toContain("Delegated task · read a.java");
    await click(child);
    await settle();
    expect(calls.some((c) => { return c.path === "/api/llm/history/runs/10/dialogue"; })).toBe(true);
    // The child's head names its parent as a link back to the lead.
    expect(text("#sheet")).toContain("run #9 · Recheck held notes · orders (6 notes)");
    expect(text("#sheet")).toContain("it holds A");
  });
  it("the kind filter and the search send QUERY parameters, never filter in the browser", async () => {
    await mount();
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/calls?") && c.path.includes("kind=chat")) {
        return [200, { calls: [CALL], page: 0, pageSize: 20, more: false }];
      }
      return undefined;
    };
    await click("#lhKind");
    await clickMenu("Chat");
    await settle();
    expect(calls.some((c) => { return c.path.includes("kind=chat"); })).toBe(true);
    // the needle goes URL-encoded on Enter
    const search = q<HTMLInputElement>("#lhSearch");
    search.value = "glm";
    search.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();
    expect(calls.some((c) => { return c.path.includes("q=glm"); })).toBe(true);
  });

  it("the pager asks for the next page from the server", async () => {
    await mount(); // the canned page answers more: true, so Older is live to click
    override = () => [200, { calls: [CALL], page: 1, pageSize: 20, more: false }];
    await click("[data-page='calls:1']");
    // the kind/needle filters persist across mounts (a deliberate carry), so the page is
    // what the assertion pins, by prefix
    expect(calls.some((c) => { return c.path.startsWith("/api/llm/history/calls?page=1&"); })).toBe(true);
  });

  it("Newer dies at the top, Older dies at the end (the wire disabled contract)", async () => {
    await mount();
    // Page 0 of a longer list (the canned more: true): Newer is dead, Older is live.
    expect(q<HTMLButtonElement>("[data-page='calls:-1']").disabled).toBe(true);
    expect(q<HTMLButtonElement>("[data-page='calls:1']").disabled).toBe(false);
    // Page 1 of a list with no more: both ends are stone dead.
    override = () => [200, { calls: [CALL], page: 1, pageSize: 20, more: false }];
    await click("[data-page='calls:1']");
    expect(q<HTMLButtonElement>("[data-page='calls:-1']").disabled).toBe(false);
    expect(q<HTMLButtonElement>("[data-page='calls:1']").disabled).toBe(true);
  });

  it("a stopped plugin is the honest 503 with a retry, never an invented empty list", async () => {
    off = true;
    await mount();
    expect(text("#pane")).toContain("The history did not load");
    off = false;
    await click("[data-lhretry]");
    expect(text("#pane")).toContain("glm / glm-5.3");
  });

  it("a poll patches the lists in place - the search box keeps its text and focus - and a failed poll keeps what is painted", async () => {
    await mount();
    const search = q<HTMLInputElement>("#lhSearch");
    search.value = "half-typed";
    search.focus();
    expect(document.activeElement).toBe(search);
    const fresh = Object.assign({}, CALL, { id: 99, preview: "a fresh round trip" });
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/calls")) return [200, { calls: [fresh], page: 0, pageSize: 20, more: false }];
      return undefined;
    };
    await view.poll();
    await settle();
    // the same input node, still focused, still holding the half-typed needle
    expect(q("#lhSearch")).toBe(search);
    expect(search.value).toBe("half-typed");
    expect(document.activeElement).toBe(search);
    // the list itself moved to the fresh answer
    expect(text("#lhCallsList")).toContain("a fresh round trip");
    // a failed poll never wipes the lists it cannot replace
    off = true;
    await view.poll();
    await settle();
    expect(text("#lhCallsList")).toContain("a fresh round trip");
  });

  it("open and closed runs wear their state on the lead where finished runs wear the dot", async () => {
    await mount();
    // ok NULL with no atEnd is a conversation still open; ok NULL with atEnd set is one
    // the reader closed. Both say their state - a NULL-ok row never renders a bare lead.
    expect(document.querySelector("[data-hrun='5'] [data-lhstate='open']")?.textContent).toBe("Open");
    expect(document.querySelector("[data-hrun='6'] [data-lhstate='closed']")?.textContent).toBe("Closed");
    // Legacy finished runs keep today's green/red dot and carry no state badge.
    expect(document.querySelector("[data-hrun='3'] [data-lhstate]")).toBe(null);
    expect(document.querySelector("[data-hrun='3'] .dot.up")).toBeTruthy();
  });

  it("the dialogue sheet's outcome line says Open and Closed instead of going blank", async () => {
    await mount();
    await click("[data-hrun='5']");
    await settle();
    expect(document.querySelector("#sheet [data-lhstate='open']")?.textContent).toBe("Open");
    expect(text("#sheet")).toContain("Filed, still open.");
    await click("[data-hrun='6']");
    await settle();
    expect(document.querySelector("#sheet [data-lhstate='closed']")?.textContent).toBe("Closed");
    // A legacy finished run keeps the wording it always had, with no state badge.
    await click("[data-hrun='3']");
    await settle();
    expect(document.querySelector("#sheet [data-lhstate]")).toBe(null);
    expect(text("#sheet")).toContain("proposal filed");
  });

  it("the live words: the round and the step, what streamed, a spawned runtime's wait", () => {
    const live = { elapsedMs: 65_000, round: 2, step: "tool" as const, tool: "read_file", stepMs: 3000, streamed: 0 };
    expect(view.liveText(live, "builtin")).toBe("Round 2 · running read_file · 3.0 s");
    expect(view.liveText({ ...live, round: 3, step: "model", tool: null, streamed: 1200 }, "builtin"))
      .toBe("Round 3 · waiting on the model · 3.0 s · 1.2K chars streamed");
    expect(view.liveText({ ...live, round: 0, step: "model", tool: null }, "builtin")).toBe("Waiting on the model · 3.0 s");
    expect(view.liveText({ ...live, round: 0, step: "model", tool: null }, "claude")).toBe("Waiting on claude · 3.0 s");
    expect(view.runBrief({ ...LIVE_RUN })).toBe("Round 2 · running read_file · 3.0 s · 65 s in all");
    expect(view.runBrief({ ...RUN, rounds: 1, tools: {}, tokensIn: 1200, tokensOut: 80 })).toBe("single request · 1.2K / 80 · 20 s");
  });

  it("a running run leads its list with the pulse and says what it does now", async () => {
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/runs?")) return [200, { runs: [LIVE_RUN, RUN], page: 0, pageSize: 20, more: false }];
      return undefined;
    };
    await mount();
    const row = q("[data-hrun='12']");
    expect(row.querySelector("[data-lhstate='live']")?.getAttribute("title")).toBe("Running");
    expect(row.textContent).toContain("Round 2 · running read_file");
    expect(row.textContent).toContain("65 s"); // its time so far, not the filed span
    expect(document.querySelector("[data-hrun='3'] [data-lhstate]")).toBe(null);
  });

  it("a live run a memory run asked carries Stop: it cancels the memory run by its id alone and says stopping", async () => {
    const other = { ...LIVE_RUN, id: 13, subject: "web" };
    const unasked = { ...LIVE_RUN, id: 14, task: null, subject: null, source: null };
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/runs?")) return [200, { runs: [LIVE_RUN, other, unasked, RUN], page: 0, pageSize: 20, more: false }];
      if (c.method === "POST" && c.path.startsWith("/api/knowledge/sync/runs/")) {
        return [200, { run: { id: "r-a1e84e8c", state: "running" }, stopping: true }];
      }
      return undefined;
    };
    await mount();
    // Only a live row a memory run asked has it; a filed run and a run of no job have none.
    expect(q("[data-hrun='12'] [data-hstop]").getAttribute("aria-label")).toBe("Stop");
    expect(q("[data-hrun='12'] [data-hstop] use").getAttribute("href")).toBe("#i-stop");
    // The tone is the red of a pressable Stop; ui.css greys it while the button is disabled.
    expect(q<HTMLButtonElement>("[data-hrun='12'] [data-hstop]").dataset.tone).toBe("stop");
    expect(q("[data-hrun='12'] [data-hstop]").getAttribute("title")).toContain("memory run r-a1e84e8c");
    expect(document.querySelector("[data-hrun='14'] [data-hstop]")).toBe(null);
    expect(document.querySelector("[data-hrun='3'] [data-hstop]")).toBe(null);

    await click("[data-hrun='12'] [data-hstop]");
    await settle();
    const posts = calls.filter((c) => c.method === "POST");
    expect(posts.map((c) => c.path)).toEqual(["/api/knowledge/sync/runs/r-a1e84e8c/cancel"]);
    // The click stopped there: no sheet opened for the row.
    expect(calls.some((c) => c.path === "/api/llm/history/runs/12/dialogue")).toBe(false);
    expect(text("#toast")).toBe("Run r-a1e84e8c is stopping: the questions in flight are dropped, the notes it saved stay");
    expect(q("[data-hrun='12'] [data-hstop]").getAttribute("aria-label")).toBe("stopping…");
    expect(q("[data-hrun='12'] [data-hstop] use").getAttribute("href")).toBe("#i-stop");

    // A repaint keeps it: another row of the same memory run says it is stopping too.
    view.unmount();
    await mount();
    const stopping = q<HTMLButtonElement>("[data-hrun='13'] [data-hstop]");
    expect(stopping.disabled).toBe(true);
    expect(stopping.getAttribute("aria-label")).toBe("stopping…");
    expect(stopping.querySelector("use")?.getAttribute("href")).toBe("#i-stop");
    expect(stopping.dataset.tone).toBe("stop");
  });

  it("disables every visible row of a memory run while its Stop is pending", async () => {
    const first = { ...LIVE_RUN, source: "memory:r-pending" };
    const second = { ...first, id: 13 };
    override = (c) => c.path.startsWith("/api/llm/history/runs?")
      ? [200, { runs: [first, second], page: 0, pageSize: 20, more: false }] : undefined;
    const original = globalThis.fetch;
    let finish!: (response: Response) => void;
    let posts = 0;
    const pending = new Promise<Response>((resolve) => { finish = resolve; });
    globalThis.fetch = ((url: string | URL | Request, opts?: RequestInit): Promise<Response> => {
      if (String(url).includes("/sync/runs/r-pending/cancel")) { posts++; return pending; }
      return original(url, opts);
    }) as typeof fetch;
    try {
      await mount();
      q<HTMLButtonElement>("[data-hrun='12'] [data-hstop]").focus();
      await click("[data-hrun='12'] [data-hstop]");
      q<HTMLButtonElement>("[data-hrun='12'] [data-hstop]").blur();
      const other = q<HTMLButtonElement>("[data-hrun='13'] [data-hstop]");
      expect(other.disabled).toBe(true);
      expect(other.getAttribute("aria-label")).toBe("stopping…");
      await click(other);
      expect(posts).toBe(1);
      await view.poll();
      finish({ ok: false, status: 400, json: () => Promise.resolve({ message: "refused" }) } as Response);
      await settle();
      expect(q<HTMLButtonElement>("[data-hrun='12'] [data-hstop]").disabled).toBe(false);
      expect(q<HTMLButtonElement>("[data-hrun='13'] [data-hstop]").disabled).toBe(false);
      expect(document.activeElement).toBe(q("[data-hrun='12'] [data-hstop]"));
    } finally {
      finish({ ok: false, status: 400, json: () => Promise.resolve({ message: "refused" }) } as Response);
      globalThis.fetch = original;
    }
  });

  it("a refused Stop says why and hands the button back", async () => {
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/runs?")) return [200, { runs: [{ ...LIVE_RUN, source: "memory:r-gone" }], page: 0, pageSize: 20, more: false }];
      if (c.method === "POST" && c.path.startsWith("/api/knowledge/sync/runs/")) {
        return [400, { error: "memory_invalid", message: "run r-gone already ended (ok); only a queued or running run cancels" }];
      }
      return undefined;
    };
    await mount();
    await click("[data-hrun='12'] [data-hstop]");
    await settle();
    expect(text("#toast")).toBe("run r-gone already ended (ok); only a queued or running run cancels");
    const button = q<HTMLButtonElement>("[data-hrun='12'] [data-hstop]");
    expect(button.disabled).toBe(false);
    expect(button.getAttribute("aria-label")).toBe("Stop");
    expect(button.title).toContain("memory run r-gone");
    expect(button.querySelector("use")?.getAttribute("href")).toBe("#i-stop");
  });

  it("a running run's sheet shows the step in flight and reads again until the run files, keeping what was opened", async () => {
    let filed = false;
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/runs?")) return [200, { runs: [LIVE_RUN], page: 0, pageSize: 20, more: false }];
      if (c.path === "/api/llm/history/runs/12/dialogue") return [200, filed ? FILED_DIALOGUE : LIVE_DIALOGUE];
      return undefined;
    };
    const ticks: Array<() => void> = [];
    const real = window.setTimeout;
    window.setTimeout = ((fn: () => void, ms?: number) => {
      if (ms === 3000) { ticks.push(fn); return 0; }
      return real(fn, ms);
    }) as typeof window.setTimeout;
    try {
      await mount();
      await click("[data-hrun='12']");
      await settle();
      expect(text("#sheet")).toContain("Running");
      expect(text("#sheet")).toContain("Round 2 · running read_file");
      // The tool call still waiting on its result shimmers, a pulse before its duration.
      const items = Array.from(document.querySelectorAll<HTMLElement>("#sheet [data-lhsteps='0'] .fl-step"));
      const last = items[items.length - 1];
      expect(last.textContent).toContain("b.rs");
      expect(last.classList.contains("live")).toBe(true);
      expect(last.textContent).not.toContain("no result");
      // The reader opens the first round; a read is due.
      await click(q("#sheet [data-lhsteps='0'] .fl-sum"));
      expect(items[0].classList.contains("open")).toBe(true);
      expect(ticks.length).toBe(1);
      // The run files: the next read paints it whole, the opened round still open, and no
      // further read is due.
      filed = true;
      ticks.shift()!();
      await settle();
      expect(calls.filter((c) => c.path === "/api/llm/history/runs/12/dialogue").length).toBe(2);
      expect(text("#sheet")).toContain("nothing to file");
      expect(text("#sheet")).not.toContain("Running");
      expect(document.querySelector("#sheet [data-lhsteps='0'] .fl-step")!.classList.contains("open")).toBe(true);
      expect(document.querySelector("#sheet .fl-step.live")).toBe(null);
      expect(ticks.length).toBe(0);
    } finally {
      window.setTimeout = real;
    }
  });

  it("another page's hand-off opens a run's sheet; a model round in flight closes its trace with its own line", async () => {
    const thinking = { ...LIVE_DIALOGUE, live: { elapsedMs: 9000, round: 3, step: "model", tool: null, stepMs: 4200, streamed: 1200 } };
    override = (c) => {
      if (c.path === "/api/llm/history/runs/12/dialogue") return [200, thinking];
      return undefined;
    };
    const real = window.setTimeout;
    window.setTimeout = ((fn: () => void, ms?: number) => (ms === 3000 ? 0 : real(fn, ms))) as typeof window.setTimeout;
    try {
      // Memory's sync sheet asks for run 12 and goes to this page.
      (await import("../src/views/llm-core.js")).showRun(12);
      await mount();
      await settle();
      expect(calls.some((c) => c.path === "/api/llm/history/runs/12/dialogue")).toBe(true);
      const items = Array.from(document.querySelectorAll<HTMLElement>("#sheet [data-lhsteps='0'] .fl-step"));
      const last = items[items.length - 1];
      expect(last.textContent).toContain("Round 3 · waiting on the model");
      expect(last.textContent).toContain("1.2K chars streamed");
      expect(last.classList.contains("live")).toBe(true);
      // The pending tool call of the earlier round is no longer the step in flight.
      expect(items[items.length - 2].classList.contains("live")).toBe(false);
      expect(location.hash).toBe("#llm-history");
    } finally {
      window.setTimeout = real;
      location.hash = "";
    }
  });

  it("empty stores are the two empty states", async () => {
    override = (c) => {
      if (c.path.startsWith("/api/llm/history/calls")) return [200, { calls: [], page: 0, pageSize: 20, more: false }];
      if (c.path.startsWith("/api/llm/history/runs")) return [200, { runs: [], page: 0, pageSize: 20, more: false }];
      return undefined;
    };
    await mount();
    expect(text("#pane")).toContain("No calls recorded yet");
    expect(text("#pane")).toContain("No conversations recorded yet");
  });
});
