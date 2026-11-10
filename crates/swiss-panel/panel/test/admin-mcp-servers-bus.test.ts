// @vitest-environment happy-dom

/* SPEC §mcp.registry: MCP.SERVERS is held only while the document is visible.
   Hide sends UNSUBSCRIBE. Servers no longer exports a tick poll. */

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, LinkId, Module, NoticeSchema, TopicId, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { McpSub, ServersId } from "../src/bus/pb/swiss/bus/v1/mcp_pb.js";
import { followMcpServers } from "../src/mcp-live.js";

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
  removeEventListener(): void { /* the link outlives the case */ }
  send(data: Uint8Array): void { this.sent.push(new Uint8Array(data)); }
  close(): void { /* never closed under test */ }
  private emit(type: string, ev: Event): void {
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }
  open(): void { this.emit("open", new Event("open")); }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
  }
}

let bus: LiveSocket;
let hidden = false;

function topicFrames(id: number): number {
  let n = 0;
  for (const bytes of bus.sent) {
    const frame = fromBinary(FrameSchema, bytes);
    if (frame.module === 0 && frame.sub === 2 && frame.id === id) n += 1;
  }
  return n;
}

beforeAll(() => {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => (hidden ? "hidden" : "visible"),
  });
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
});

afterEach(() => { vi.useRealTimers(); hidden = false; });

describe("MCP.SERVERS is held only while the page is visible (SPEC §mcp.registry)", () => {
  it("a hide sends UNSUBSCRIBE, and two notices are one read", async () => {
    vi.useFakeTimers();
    const reads: number[] = [];
    const stop = followMcpServers(async () => { reads.push(1); });
    expect(topicFrames(TopicId.SUBSCRIBE)).toBeGreaterThan(0);
    hidden = true;
    document.dispatchEvent(new Event("visibilitychange"));
    expect(topicFrames(TopicId.UNSUBSCRIBE)).toBeGreaterThan(0);
    hidden = false;
    document.dispatchEvent(new Event("visibilitychange"));
    const body = toBinary(NoticeSchema, create(NoticeSchema, {}));
    const frame = toBinary(FrameSchema, create(FrameSchema, {
      module: Module.MCP, sub: McpSub.SERVERS, id: ServersId.SERVERS_CHANGED,
      kind: Kind.EVENT, seq: 2, key: "", origin: "page-a", body,
    }));
    bus.receive(frame);
    bus.receive(frame);
    await vi.advanceTimersByTimeAsync(200);
    expect(reads).toEqual([1]);
    stop();
  });

  it("Servers left the tick; Traffic and Tunnels still export a poll", async () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
    const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
    document.body.innerHTML = Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
    const mcps = await import("../src/views/mcps.js");
    const traffic = await import("../src/views/traffic.js");
    const tunnels = await import("../src/views/tunnels.js");
    expect((mcps as { poll?: unknown }).poll).toBeUndefined();
    expect(typeof traffic.poll).toBe("function");
    expect(typeof tunnels.poll).toBe("function");
  });
});
