// @vitest-environment happy-dom

/* SPEC §mcp.calls: page 0 of the open logs tab re-reads on that MCP's CALLS
   notice. A page > 0 ignores it. Another MCP's key is not this subscription. */

import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, LinkId, Module, NoticeSchema, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { CallsId, McpSub } from "../src/bus/pb/swiss/bus/v1/mcp_pb.js";
import { setMcpDetail, mcpDetail } from "../src/mcp-state.js";
import { setCurrentView } from "../src/ui-state.js";

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

const callsUrls: string[] = [];

function notice(key: string, seq: number): Uint8Array {
  const body = toBinary(NoticeSchema, create(NoticeSchema, {}));
  return toBinary(FrameSchema, create(FrameSchema, {
    module: Module.MCP, sub: McpSub.CALLS, id: CallsId.CALLS_CHANGED,
    kind: Kind.EVENT, seq, key, origin: "page-a", body,
  }));
}

function fakeDetail(name: string) {
  const kd = { items: [], nextCursor: undefined, total: undefined, loaded: false, loading: false, error: null, cursors: [""], pageSize: 50 };
  return {
    name, tab: "tools", config: { type: "http", url: "http://x" }, source: "managed",
    editing: false, editType: null, editVals: null, oauthBusy: false,
    tools: { ...kd }, resources: { ...kd }, prompts: { ...kd },
    run: { tool: null, result: null, running: false, hist: null, histTool: null, histLoading: false, histOpen: false, histSelSeq: null, histFull: {}, histQ: "" },
    calls: null, stderr: "", callsOpen: {}, callsPage: 0, callsMore: false, callsFull: {}, callsGone: {}, callsAll: {},
    callsQ: "", callsPendingPage: null, callsError: "", callsErrStatus: "", callsRetryTarget: null,
    callsRetryDir: null, callsSwitch: null, callsRequest: 0, callsActive: 0,
  };
}

beforeAll(() => {
  const here = dirname(fileURLToPath(import.meta.url));
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  document.body.innerHTML = Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
  vi.stubGlobal("fetch", (url: string) => {
    const path = String(url);
    if (path.includes("/calls")) callsUrls.push(path);
    return Promise.resolve({
      ok: true,
      status: 200,
      json: async () => ({ calls: [], more: false, stderr: "" }),
    });
  });
});

describe("the open logs page re-reads on its MCP's CALLS notice (SPEC §mcp.calls)", () => {
  it("page 0 re-reads once per burst; page > 0 and another MCP do not", async () => {
    const { showTab, stopCallWatch } = await import("../src/detail.js");
    Sock.all.length = 0;
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
    setCurrentView("mcps");
    setMcpDetail(fakeDetail("alpha") as never);
    callsUrls.length = 0;
    vi.useFakeTimers();
    showTab("logs");
    await vi.advanceTimersByTimeAsync(0);
    expect(callsUrls.some((u) => u.includes("/api/mcps/alpha/calls"))).toBe(true);
    const opened = callsUrls.length;
    sock.receive(notice("alpha", 2));
    sock.receive(notice("alpha", 3));
    await vi.advanceTimersByTimeAsync(200);
    expect(callsUrls.length).toBe(opened + 1);
    sock.receive(notice("beta", 4));
    await vi.advanceTimersByTimeAsync(200);
    expect(callsUrls.length).toBe(opened + 1);
    const d = mcpDetail();
    if (d) d.callsPage = 1;
    sock.receive(notice("alpha", 5));
    await vi.advanceTimersByTimeAsync(200);
    expect(callsUrls.length).toBe(opened + 1);
    stopCallWatch();
    setMcpDetail(null);
    vi.useRealTimers();
  });
});
