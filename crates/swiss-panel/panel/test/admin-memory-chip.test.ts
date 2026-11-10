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

/* SPEC §host.memory: the chip subscribes while visible, does one GET when it
   subscribes, repaints from HOST.MEMORY readings, and unsubscribes on hide. */

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, LinkId, Module, TopicId, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, MemoryId, MemoryReadingSchema } from "../src/bus/pb/swiss/bus/v1/host_pb.js";
import type * as Polling from "../src/polling.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => { return '<div id="' + id + '"></div>'; }).join("");
}

class LiveSocket implements BusSocket {
  static all: LiveSocket[] = [];
  binaryType: BinaryType = "blob";
  sent: Uint8Array[] = [];
  private listeners = new Map<string, Array<(ev: Event) => void>>();
  constructor() { LiveSocket.all.push(this); }
  addEventListener(type: string, listener: (ev: Event) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }
  removeEventListener(): void { /* unused */ }
  send(data: Uint8Array): void { this.sent.push(new Uint8Array(data)); }
  close(): void { /* unused */ }
  private emit(type: string, ev: Event): void {
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }
  open(): void { this.emit("open", new Event("open")); }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
  }
}

let hidden = false;
let gets = 0;
let polling: typeof Polling;
let bus: LiveSocket;

function topicFrames(id: number): number {
  let n = 0;
  for (const bytes of bus.sent) {
    const frame = fromBinary(FrameSchema, bytes);
    if (frame.module === 0 && frame.sub === 2 && frame.id === id) n += 1;
  }
  return n;
}

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => (hidden ? "hidden" : "visible"),
  });
  globalThis.fetch = (async (url: string | URL | Request) => {
    if (String(url).includes("/api/memory")) {
      gets += 1;
      return new Response(JSON.stringify({
        gatewayMb: 1, heapUsedMb: 2, heapTotalMb: 2, externalMb: 0,
        processCount: 1, childrenPending: false, childrenMb: 0, at: "0",
      }), { status: 200 });
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;
  LiveSocket.all.length = 0;
  installBus({
    socket: LiveSocket,
    random: () => 0.5,
    reload: () => undefined,
    info: () => Promise.resolve(),
    notice: () => undefined,
    url: "ws://127.0.0.1/api/bus",
  });
  bus = LiveSocket.all[LiveSocket.all.length - 1];
  bus.open();
  bus.receive(toBinary(FrameSchema, create(FrameSchema, {
    module: 0, sub: 1, id: LinkId.HELLO, kind: Kind.RESPONSE, seq: 1, replyTo: 1,
    body: toBinary(WelcomeSchema, create(WelcomeSchema, {
      protocol: 1, connId: "7", build: "stamp-a", heartbeatMs: 60000n, maxFrame: 65536, serverMs: 0n,
    })),
  })));
  polling = await import("../src/polling.js");
});

describe("the memory chip", () => {
  it("subscribes while visible, reads once, repaints from a reading, and drops the topic on hide (SPEC §host.memory)", async () => {
    const stop = polling.watchMemoryChip();
    expect(topicFrames(TopicId.SUBSCRIBE)).toBeGreaterThan(0);
    expect(gets).toBe(0);
    await polling.loadMemory(true);
    expect(gets).toBe(1);
    expect(document.getElementById("memChip")?.textContent).toBe("1 MB");

    bus.receive(toBinary(FrameSchema, create(FrameSchema, {
      module: Module.HOST, sub: HostSub.MEMORY, id: MemoryId.MEMORY_READING, kind: Kind.EVENT,
      seq: 2, key: "", origin: "page-a",
      body: toBinary(MemoryReadingSchema, create(MemoryReadingSchema, {
        gatewayMb: 12.5, privateMb: 40, childrenMb: 3.5, processCount: 4, childrenPending: false, atMs: 1700000000000n,
      })),
    })));
    expect(gets).toBe(1);
    expect(document.getElementById("memChip")?.textContent).toBe("16 MB · 4 procs");
    expect(document.getElementById("memChip")?.getAttribute("title")).toContain("40");

    hidden = true;
    document.dispatchEvent(new Event("visibilitychange"));
    expect(topicFrames(TopicId.UNSUBSCRIBE)).toBeGreaterThan(0);

    hidden = false;
    document.dispatchEvent(new Event("visibilitychange"));
    await new Promise((r) => setTimeout(r, 0));
    expect(gets).toBe(2);
    stop();
  });
});
