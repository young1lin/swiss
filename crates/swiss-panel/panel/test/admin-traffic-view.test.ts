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

import { beforeEach, describe, expect, it, vi } from "vitest";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, LinkId, TopicId, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";

/* The Traffic page's context-bar chip counts the MCPs (mcpChipText over mcpRows). Found on the
   SPEC §panel.pages, found on a walk: a cold load straight onto #traffic read "0 MCPs · 0 up" and kept reading it,
   because nothing on that page ever loaded the MCP list - the Servers page's poll is what fills
   it, and that page was never visited. The view now loads the list it counts, on mount and on
   every poll. */

const calls: string[] = [];
vi.mock("../src/traffic.js", () => ({
  clearTrafficView: () => { calls.push("clear"); },
  loadTraffic: async () => { calls.push("loadTraffic"); },
  resetTrafficSig: () => { calls.push("resetSig"); },
  trafficReload: () => { calls.push("trafficReload"); },
}));
vi.mock("../src/polling.js", () => ({
  loadList: async () => { calls.push("loadList"); },
  mcpChipText: () => "chip",
}));

let view: typeof import("../src/views/traffic.js");

beforeEach(async () => {
  calls.length = 0;
  view = await import("../src/views/traffic.js");
});

describe("the Traffic view counts MCPs it loaded itself (SPEC §panel.pages)", () => {
  it("mount loads the MCP list before the first paint, so the chip is right on a cold load", async () => {
    await view.mount();
    expect(calls).toEqual(["loadList", "trafficReload"]);
  });

  it("every poll refreshes the MCP list with the activity, so the chip follows state changes", async () => {
    await view.poll();
    expect(calls).toContain("loadList");
    expect(calls).toContain("loadTraffic");
  });

  it("mount holds MCP.SERVERS, and hide sends UNSUBSCRIBE (SPEC §mcp.registry)", async () => {
    class Sock implements BusSocket {
      static all: Sock[] = [];
      binaryType: BinaryType = "blob";
      sent: Uint8Array[] = [];
      private listeners = new Map<string, Array<(ev: Event) => void>>();
      constructor() { Sock.all.push(this); }
      addEventListener(type: string, listener: (ev: Event) => void): void {
        const list = this.listeners.get(type) ?? [];
        list.push(listener);
        this.listeners.set(type, list);
      }
      removeEventListener(): void { /* unused */ }
      send(data: Uint8Array): void { this.sent.push(new Uint8Array(data)); }
      close(): void { /* unused */ }
      open(): void {
        for (const fn of this.listeners.get("open") ?? []) fn(new Event("open"));
      }
      receive(bytes: Uint8Array): void {
        const copy = new Uint8Array(bytes);
        for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
      }
    }
    let hidden = false;
    Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => (hidden ? "hidden" : "visible"),
    });
    installBus({
      socket: Sock,
      random: () => 0.5,
      reload: () => undefined,
      info: () => Promise.resolve(),
      notice: () => undefined,
      url: "ws://127.0.0.1/api/bus",
    });
    const sock = Sock.all[Sock.all.length - 1];
    sock.open();
    sock.receive(toBinary(FrameSchema, create(FrameSchema, {
      module: 0, sub: 1, id: LinkId.HELLO, kind: Kind.RESPONSE, seq: 1, replyTo: 1,
      body: toBinary(WelcomeSchema, create(WelcomeSchema, {
        protocol: 1, connId: "7", build: "stamp-a", heartbeatMs: 60000n, maxFrame: 65536, serverMs: 0n,
      })),
    })));
    await view.mount();
    const subscribed = sock.sent.some((bytes) => {
      const frame = fromBinary(FrameSchema, bytes);
      return frame.module === 0 && frame.sub === 2 && frame.id === TopicId.SUBSCRIBE;
    });
    expect(subscribed).toBe(true);
    hidden = true;
    document.dispatchEvent(new Event("visibilitychange"));
    const unsubscribed = sock.sent.some((bytes) => {
      const frame = fromBinary(FrameSchema, bytes);
      return frame.module === 0 && frame.sub === 2 && frame.id === TopicId.UNSUBSCRIBE;
    });
    expect(unsubscribed).toBe(true);
    hidden = false;
    view.unmount();
  });
});
