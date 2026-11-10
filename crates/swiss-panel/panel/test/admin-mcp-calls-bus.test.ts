// @vitest-environment happy-dom

/* SPEC §mcp.calls: page 0 of the open logs tab re-reads on that MCP's CALLS
   notice. A page > 0 ignores it. Another MCP's key is not this subscription. */

import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import { FrameSchema, Kind, LinkId, Module, NoticeSchema, TopicId, TopicsSchema, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { CallsId, McpSub } from "../src/bus/pb/swiss/bus/v1/mcp_pb.js";
import { setMcpDetail, mcpDetail, setSelectedMcp } from "../src/mcp-state.js";
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
/* How many frames the link had sent when each /calls read left: subscribe-then-read is an order. */
const sentAtCalls: number[] = [];
/* A test may answer a request itself (a parked read, the list a rename reloads). */
let answer: ((path: string, init?: RequestInit) => Promise<unknown> | undefined) | null = null;

function welcomeFrame(): Uint8Array {
  return toBinary(FrameSchema, create(FrameSchema, {
    module: 0, sub: 1, id: LinkId.HELLO, kind: Kind.RESPONSE, seq: 1, replyTo: 1,
    body: toBinary(WelcomeSchema, create(WelcomeSchema, {
      protocol: 1, connId: "7", build: "stamp-a", heartbeatMs: 60000n, maxFrame: 65536, serverMs: 0n,
    })),
  }));
}

function openLink(): Sock {
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
  sock.receive(welcomeFrame());
  return sock;
}

/** The index of the first SUBSCRIBE frame that names MCP.CALLS under `key`, or -1. */
function subscribeAt(sock: Sock, key: string): number {
  return sock.sent.findIndex((bytes) => {
    const frame = fromBinary(FrameSchema, bytes);
    if (frame.module !== 0 || frame.sub !== 2 || frame.id !== TopicId.SUBSCRIBE) return false;
    return fromBinary(TopicsSchema, frame.body).topics.some((t) => t.module === Module.MCP && t.sub === McpSub.CALLS && t.key === key);
  });
}

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
  vi.stubGlobal("fetch", (url: string, init?: RequestInit) => {
    const path = String(url);
    if (path.includes("/calls")) {
      callsUrls.push(path);
      sentAtCalls.push(Sock.all.length ? Sock.all[Sock.all.length - 1].sent.length : 0);
    }
    const own = answer ? answer(path, init) : undefined;
    if (own) return own;
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

  // SPEC §bus.topics "Subscribe, then read": a call committed while the first read is out
  // must still re-read, so the SUBSCRIBE leaves before the GET.
  it("opening Logs subscribes before it reads", async () => {
    const { showTab, stopCallWatch } = await import("../src/detail.js");
    const sock = openLink();
    setCurrentView("mcps");
    setMcpDetail(fakeDetail("alpha") as never);
    callsUrls.length = 0;
    sentAtCalls.length = 0;
    try {
      showTab("logs");
      await new Promise((r) => setTimeout(r, 0));
      const sub = subscribeAt(sock, "alpha");
      expect(sub, "the logs page subscribes to its MCP's CALLS").toBeGreaterThanOrEqual(0);
      expect(callsUrls.length).toBeGreaterThan(0);
      expect(sub, "the SUBSCRIBE frame left before the first /calls read").toBeLessThan(sentAtCalls[0]);
    } finally {
      stopCallWatch();
      setMcpDetail(null);
    }
  });

  // SPEC §mcp.calls: a notice while a foreground load hangs must not take its generation, or
  // the foreground failure lands as stale and is reported to nobody (the in-flight guard).
  it("a notice while a foreground load is in flight reads nothing, and the load's failure still shows", async () => {
    const { showTab, stopCallWatch } = await import("../src/detail.js");
    const sock = openLink();
    setCurrentView("mcps");
    setMcpDetail(fakeDetail("alpha") as never);
    callsUrls.length = 0;
    let fail: ((e: Error) => void) | null = null;
    answer = (path) => path.includes("/calls")
      ? new Promise((_, reject) => { fail = reject; })
      : undefined;
    vi.useFakeTimers();
    try {
      showTab("logs");
      await vi.advanceTimersByTimeAsync(0);
      expect(callsUrls.length, "the foreground load is out, parked").toBe(1);
      sock.receive(notice("alpha", 2));
      await vi.advanceTimersByTimeAsync(200);
      expect(callsUrls.length, "the notice's re-read stands aside for the foreground load").toBe(1);
      expect(fail).not.toBeNull();
      fail!(new TypeError("network down"));
      await vi.advanceTimersByTimeAsync(0);
      expect(mcpDetail()?.callsError).toBe("Could not load calls.");
    } finally {
      answer = null;
      vi.useRealTimers();
      stopCallWatch();
      setMcpDetail(null);
    }
  });

  // SPEC §mcp.calls: the subscription is keyed by name, so a rename re-keys the open Logs,
  // and the old key's coalescer goes with it.
  it("a rename re-keys the open Logs: the new name re-reads, the old key's coalescer stops", async () => {
    const { renameMcp, showTab, stopCallWatch } = await import("../src/detail.js");
    const sock = openLink();
    setCurrentView("mcps");
    setSelectedMcp("alpha");
    setMcpDetail(fakeDetail("alpha") as never);
    answer = (path, init) => {
      if (path.endsWith("/rename") && init?.method === "POST") {
        return Promise.resolve({ ok: true, status: 200, json: async () => ({ ok: true }) });
      }
      if (path === "/api/mcps") {
        return Promise.resolve({ ok: true, status: 200, json: async () => ({
          mcps: [{ name: "gamma", lifecycle: "started", state: "up", type: "http", source: "managed", group: "default", description: "" }],
          groups: ["default"],
        }) });
      }
      return undefined;
    };
    const removed = vi.spyOn(document, "removeEventListener");
    try {
      showTab("logs");
      await new Promise((r) => setTimeout(r, 0));
      removed.mockClear();
      renameMcp("alpha");
      (document.getElementById("g-name") as HTMLInputElement).value = "gamma";
      (document.getElementById("g-save") as HTMLButtonElement).click();
      for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
      expect(mcpDetail()?.name).toBe("gamma");
      expect(subscribeAt(sock, "gamma"), "the open Logs follows the new name").toBeGreaterThanOrEqual(0);
      expect(removed.mock.calls.some((c) => c[0] === "visibilitychange"), "the old key's coalescer stopped").toBe(true);
      callsUrls.length = 0;
      vi.useFakeTimers();
      sock.receive(notice("gamma", 2));
      await vi.advanceTimersByTimeAsync(200);
      expect(callsUrls.some((u) => u.includes("/api/mcps/gamma/calls")), "a call under the new name re-reads").toBe(true);
    } finally {
      removed.mockRestore();
      answer = null;
      vi.useRealTimers();
      stopCallWatch();
      setMcpDetail(null);
      setSelectedMcp(null);
    }
  });
});
