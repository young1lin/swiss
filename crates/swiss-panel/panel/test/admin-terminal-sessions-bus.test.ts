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

/* SPEC §terminal.panel: the tabs and the count chip re-read the session list on
   TERMINAL.SESSIONS. The page exports no poll. A burst is one read. */

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type * as TerminalView from "../src/views/terminal.js";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, Module, NoticeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { SessionsId, TerminalSub } from "../src/bus/pb/swiss/bus/v1/terminal_pb.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => { return '<div id="' + id + '"></div>'; }).join("");
}

const TARGETS = {
  local: { enabled: false, shell: "", shells: [] },
  remote: { presence: "absent", reason: "no plugin currently provides interactive shells", targets: [] },
};

let sessions: Array<Record<string, unknown>> = [];
let sessionGets = 0;

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
  send(_data: Uint8Array): void { /* unused */ }
  close(): void { /* unused */ }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
  }
}

function notice(sock: Sock): void {
  const body = toBinary(NoticeSchema, create(NoticeSchema, {}));
  sock.receive(toBinary(FrameSchema, create(FrameSchema, {
    module: Module.TERMINAL, sub: TerminalSub.SESSIONS, id: SessionsId.SESSIONS_CHANGED,
    kind: Kind.EVENT, seq: 2, key: "", origin: "page-a", body,
  })));
}

let view: typeof TerminalView;

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  view = await import("../src/views/terminal.js");
});

afterEach(() => {
  view.unmount();
});

describe("the terminal session list", () => {
  it("a SESSIONS burst re-reads the list once and repaints the tabs and the chip (SPEC §terminal.panel)", async () => {
    sessions = [];
    sessionGets = 0;
    Sock.all.length = 0;
    globalThis.fetch = (async (url: string | URL | Request) => {
      const path = String(url);
      if (path.includes("/api/terminal/targets")) {
        return new Response(JSON.stringify(TARGETS), { status: 200 });
      }
      if (path.includes("/api/terminal/sessions")) {
        sessionGets += 1;
        return new Response(JSON.stringify(sessions), { status: 200 });
      }
      return new Response("{}", { status: 404 });
    }) as typeof fetch;
    installBus({
      socket: Sock,
      random: () => 0.5,
      reload: () => undefined,
      info: () => Promise.resolve(),
      notice: () => undefined,
      url: "ws://127.0.0.1/api/bus",
    });
    await view.mount();
    const before = sessionGets;
    expect(before).toBeGreaterThan(0);
    expect((view as { poll?: unknown }).poll).toBeUndefined();
    expect(document.getElementById("countChip")?.textContent).toBe("");
    sessions = [{
      id: "abc", target: "local", label: "Local shell", opened: "2026-11-10T00:00:00Z",
      bytesOut: 20, attached: true, recording: null,
    }];
    const sock = Sock.all[Sock.all.length - 1];
    notice(sock);
    notice(sock);
    await new Promise((r) => setTimeout(r, 250));
    expect(sessionGets - before).toBe(1);
    expect(document.getElementById("countChip")?.textContent).toBe("1 live");
    expect(document.getElementById("term-tabs")?.textContent).toContain("Local shell");
  });

  // SPEC §bus.topics "Subscribe, then read": a session that opens while mount's own listing
  // is out lands as a notice with no stage to patch yet. It is owed, and read once the
  // stage is painted, so the tabs never keep the stale first answer.
  it("a notice while mount's first listing is out is owed and read once the stage is up", async () => {
    document.body.innerHTML = shellSkeleton();
    sessions = [];
    sessionGets = 0;
    Sock.all.length = 0;
    let answerFirst: ((r: Response) => void) | null = null;
    globalThis.fetch = (async (url: string | URL | Request) => {
      const path = String(url);
      if (path.includes("/api/terminal/targets")) {
        return new Response(JSON.stringify(TARGETS), { status: 200 });
      }
      if (path.includes("/api/terminal/sessions")) {
        sessionGets += 1;
        if (sessionGets === 1) return new Promise<Response>((r) => { answerFirst = r; });
        return new Response(JSON.stringify(sessions), { status: 200 });
      }
      return new Response("{}", { status: 404 });
    }) as typeof fetch;
    installBus({
      socket: Sock,
      random: () => 0.5,
      reload: () => undefined,
      info: () => Promise.resolve(),
      notice: () => undefined,
      url: "ws://127.0.0.1/api/bus",
    });
    const mounted = view.mount();
    for (let i = 0; i < 10 && !answerFirst; i++) await new Promise((r) => setTimeout(r, 0));
    expect(answerFirst, "mount's listing is out, parked").not.toBeNull();
    sessions = [{
      id: "abc", target: "local", label: "Local shell", opened: "2026-11-10T00:00:00Z",
      bytesOut: 20, attached: true, recording: null,
    }];
    notice(Sock.all[Sock.all.length - 1]);
    await new Promise((r) => setTimeout(r, 250));
    expect(sessionGets, "no stage yet: the notice's read is owed, not sent").toBe(1);
    answerFirst!(new Response("[]", { status: 200 })); // the listing from before the open
    await mounted;
    for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0));
    expect(sessionGets, "the owed read went out after the paint").toBe(2);
    expect(document.getElementById("countChip")?.textContent).toBe("1 live");
    expect(document.getElementById("term-tabs")?.textContent).toContain("Local shell");
  });
});
