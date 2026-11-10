// @vitest-environment happy-dom

/* SPEC §tunnels.panel: a TUNNELS.TABLE burst re-reads /api/tunnels and /api/mcps
   once. Neither tunnels page exports a tick poll. */

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, LinkId, Module, NoticeSchema, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { TunnelsSub, TunnelsTableId } from "../src/bus/pb/swiss/bus/v1/tunnels_pb.js";

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
const calls: string[] = [];

function notice(): Uint8Array {
  return toBinary(FrameSchema, create(FrameSchema, {
    module: Module.TUNNELS, sub: TunnelsSub.TABLE, id: TunnelsTableId.TUNNELS_TABLE_CHANGED,
    kind: Kind.EVENT, seq: 2, key: "", origin: "page-a",
    body: toBinary(NoticeSchema, create(NoticeSchema, {})),
  }));
}

beforeAll(() => {
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
  vi.stubGlobal("fetch", (url: string) => {
    calls.push(String(url));
    return Promise.resolve({
      ok: true,
      status: 200,
      json: () => Promise.resolve({ connections: [], rules: [], ruleGroups: [], connGroups: [], mcps: [], groups: [] }),
    });
  });
});

afterEach(() => { vi.useRealTimers(); calls.length = 0; });

describe("the tunnels pages left the tick (SPEC §tunnels.panel)", () => {
  it("neither page exports a poll, and one notice burst is one tunnels read and one mcp read", async () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
    const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
    document.body.innerHTML = Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
    const tunnels = await import("../src/views/tunnels.js");
    const forwards = await import("../src/views/tunnel-forwards.js");
    expect((tunnels as { poll?: unknown }).poll).toBeUndefined();
    expect((forwards as { poll?: unknown }).poll).toBeUndefined();

    vi.useFakeTimers();
    calls.length = 0;
    await tunnels.mount();
    const mounted = calls.slice();
    bus.receive(notice());
    bus.receive(notice());
    await vi.advanceTimersByTimeAsync(200);
    const again = calls.slice(mounted.length);
    expect(again.filter((url) => url === "/api/tunnels")).toEqual(["/api/tunnels"]);
    expect(again.filter((url) => url === "/api/mcps")).toEqual(["/api/mcps"]);
    tunnels.unmount();
  });
});
