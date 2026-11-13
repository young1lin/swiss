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

/* The Lane page (SPEC §lane.panel): the capacity and caps, the running and waiting rows with
 * their Release, the recent outcomes, a re-read on each LANE.QUEUE notice and no timer.
 * Driven through the real fetch surface and a fake bus socket. */

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, Module, NoticeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { LaneQueueId, LaneSub } from "../src/bus/pb/swiss/bus/v1/lane_pb.js";
import type * as LaneView from "../src/views/lane.js";

const here = dirname(fileURLToPath(import.meta.url));
let view: typeof LaneView;
let calls: { method: string; path: string }[] = [];
let replies: Record<string, [number, unknown]> = {};

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => { return m[1]; });
  return Array.from(new Set(ids)).map((id) => { return '<div id="' + id + '"></div>'; }).join("");
}

const NOW = Date.now();
const LANE = {
  capacity: 1,
  env: { CARGO_BUILD_JOBS: "2", VITEST_MAX_WORKERS: "4" },
  revision: 9,
  running: [{ id: 4, cmd: "cargo test -j 2", cwd: "~/dev/swiss", by: "codex", pid: 4242, enteredAt: NOW - 120_000, startedAt: NOW - 90_000 }],
  waiting: [{ id: 5, cmd: "npx vitest run", cwd: "~/dev/swiss/panel", by: "claude", enteredAt: NOW - 30_000 }],
  recent: [
    { id: 3, cmd: "cargo build", cwd: "~/dev/swiss", by: "cli", enteredAt: NOW - 600_000, startedAt: NOW - 590_000, endedAt: NOW - 300_000, outcome: "exit", exit: 0 },
    { id: 2, cmd: "cargo clippy", cwd: "~/dev/swiss", by: "cli", enteredAt: NOW - 900_000, startedAt: NOW - 890_000, endedAt: NOW - 800_000, outcome: "exit", exit: 101 },
    { id: 1, cmd: "npm test", cwd: "~/dev/x", by: "cursor", enteredAt: NOW - 990_000, startedAt: NOW - 980_000, endedAt: NOW - 970_000, outcome: "gone" },
  ],
};
const IDLE = { capacity: 2, env: {}, revision: 1, running: [], waiting: [], recent: [] };

const text = (sel: string): string => document.querySelector(sel)?.textContent ?? "";
const settle = async (): Promise<void> => {
  for (let i = 0; i < 8; i++) await new Promise((r) => { setTimeout(r, 0); });
};

class Sock implements BusSocket {
  static all: Sock[] = [];
  binaryType: BinaryType = "blob";
  private listeners = new Map<string, Array<(ev: Event) => void>>();
  constructor() { Sock.all.push(this); }
  addEventListener(type: string, listener: (ev: Event) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }
  removeEventListener(): void { /* unused */ }
  send(): void { /* unused */ }
  close(): void { /* unused */ }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
  }
}

function armBus(): Sock {
  Sock.all.length = 0;
  installBus({ socket: Sock, random: () => 0.5, reload: () => undefined, info: () => Promise.resolve(), notice: () => undefined, url: "ws://127.0.0.1/api/bus" });
  return Sock.all[Sock.all.length - 1];
}

async function queueNotice(sock: Sock): Promise<void> {
  vi.useFakeTimers();
  try {
    sock.receive(toBinary(FrameSchema, create(FrameSchema, {
      module: Module.LANE, sub: LaneSub.QUEUE, id: LaneQueueId.LANE_QUEUE_CHANGED,
      kind: Kind.EVENT, seq: 2, key: "", origin: "", body: toBinary(NoticeSchema, create(NoticeSchema, {})),
    })));
    await vi.advanceTimersByTimeAsync(200);
  } finally {
    vi.useRealTimers();
  }
}

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, opts?: RequestInit): Promise<unknown> => {
      const call = { method: (opts && opts.method) || "GET", path: String(url) };
      calls.push(call);
      const [status, json] = replies[(call.method === "GET" ? "" : call.method + " ") + call.path] ?? [500, { error: "no stub for " + call.path }];
      return Promise.resolve({ status, ok: status >= 200 && status < 300, json: () => Promise.resolve(json) });
    },
    confirm: () => { return true; },
  });
  document.body.innerHTML = shellSkeleton();
  view = await import("../src/views/lane.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  calls = [];
  replies = { "/api/lane": [200, LANE] };
});

afterEach(() => { view.unmount(); });

describe("the Lane page (SPEC §lane.panel)", () => {
  it("shows the slots and caps, who runs, who waits, and how each recent command ended", async () => {
    await view.mount();
    expect(text("#laneHead")).toBe("1 slot · caps CARGO_BUILD_JOBS=2  VITEST_MAX_WORKERS=4");
    const running = document.querySelector<HTMLElement>("[data-lanerow='4']");
    expect(running?.textContent).toContain("cargo test -j 2");
    expect(running?.textContent).toContain("codex");
    expect(running?.textContent).toContain("PID 4242");
    expect(running?.querySelector("[data-lanerelease='4']")?.textContent).toBe("Release");
    expect(text("#pane")).toContain("Waiting");
    expect(document.querySelector("[data-lanerow='5']")?.textContent).toContain("npx vitest run");
    const outcome = (id: number): Element | null => document.querySelector("[data-lanedone='" + id + "'] .tag");
    expect(outcome(3)?.textContent).toBe("exit 0");
    expect(outcome(3)?.className).not.toContain("bad");
    expect(outcome(2)?.textContent).toBe("exit 101");
    expect(outcome(2)?.className).toContain("bad");
    expect(outcome(1)?.textContent).toBe("gone");
    expect(text("#countChip")).toBe("1 running · 1 waiting");
    expect((view as { poll?: unknown }).poll, "no timer: the bus drives the re-reads").toBeUndefined();
  });

  it("an idle lane says a command starts at once and hides the empty waiting list", async () => {
    replies["/api/lane"] = [200, IDLE];
    await view.mount();
    expect(text("#laneHead")).toBe("2 slots · no caps");
    expect(text("#pane")).toContain("Idle: a command run through the lane starts at once.");
    expect(text("#pane")).toContain("Nothing has finished yet.");
    expect(document.querySelectorAll("[data-lanerow]").length).toBe(0);
    expect(text("#pane")).not.toContain("In arrival order");
    expect(text("#pane")).toContain("swiss lane run -- cargo test -j 2");
  });

  it("Release posts for that id and reads the lane again", async () => {
    replies["POST /api/lane/4/release"] = [200, { ok: true }];
    await view.mount();
    calls = [];
    document.querySelector<HTMLElement>("[data-lanerelease='4']")!.click();
    await settle();
    expect(calls).toEqual([{ method: "POST", path: "/api/lane/4/release" }, { method: "GET", path: "/api/lane" }]);
  });

  it("a command that already left answers lane.unknown, shown as a note rather than an error", async () => {
    replies["POST /api/lane/4/release"] = [404, { error: "the lane holds no command 4", code: "lane.unknown" }];
    await view.mount();
    document.querySelector<HTMLElement>("[data-lanerelease='4']")!.click();
    await settle();
    expect(text("#toast")).toBe("That command already left the lane.");
    expect(document.getElementById("toast")?.className).not.toContain("err");
  });

  it("a LANE.QUEUE notice re-reads the lane and repaints it", async () => {
    const sock = armBus();
    await view.mount();
    replies["/api/lane"] = [200, IDLE];
    calls = [];
    await queueNotice(sock);
    await settle();
    expect(calls).toEqual([{ method: "GET", path: "/api/lane" }]);
    expect(text("#pane")).toContain("Idle: a command run through the lane starts at once.");
    expect(text("#countChip")).toBe("0 running · 0 waiting");
  });

  it("clock rewords the ages from what it holds, with no request", async () => {
    await view.mount();
    const age = document.querySelector<HTMLElement>("[data-lanerow='4'] [data-laneat]")!;
    age.textContent = "stale";
    calls = [];
    view.clock();
    expect(age.textContent).toMatch(/^running /);
    expect(calls).toEqual([]);
  });
});
