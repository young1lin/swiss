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

import { describe, it, expect, afterAll } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dbConn, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";
import type { DbPubsubLine, DbPubsubTab } from "../src/types/state.js";

/* SPEC §data.pubsub: the Pub/Sub page's pure parts (the NDJSON splitter, the log's ring, a typed
 * name's side) and its feed's lifecycle against a fake fetch - a page leaving the front aborts
 * its subscription, and the one that comes back says which stretch went unheard. Same micro-DOM
 * as db-toolbar.test.ts: h() needs a Node to test children against. */
class NodeStub {}

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { c.parentNode = n; n.children.push(c); return c; },
    append(...cs: Stub[]) { cs.forEach((c) => { if (typeof c === "object") n.children.push(c); }); },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() { const p = n.parentNode as Stub | undefined; if (p) p.children = p.children.filter((x: Stub) => x !== n); },
    contains: () => false, closest: () => null,
    setAttribute(k: string, v: string) { if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    focus() {}, blur() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
  };
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};
const SAVED = (["document", "window", "localStorage", "location", "matchMedia", "setInterval",
  "clearInterval", "fetch", "Node"] as const).map((k: string) => ({ k, d: Object.getOwnPropertyDescriptor(globalThis, k) }));

interface Call { path: string; method: string; body: string; signal?: AbortSignal }
const calls: Call[] = [];
/** The feed the next POST /pubsub/subscribe answers with: a body whose reader hands out what
 *  push() queued and rejects once the request's signal aborts, as fetch's own does. */
let feed: { push(s: string): void; end(): void } | null = null;

function feedResponse(signal: AbortSignal): unknown {
  const queue: string[] = [];
  let ended = false;
  let wake: (() => void) | null = null;
  const enc = new TextEncoder();
  const poke = (): void => { const w = wake; wake = null; if (w) w(); };
  signal.addEventListener("abort", poke);
  feed = {
    push(s: string): void { queue.push(s); poke(); },
    end(): void { ended = true; poke(); },
  };
  return {
    ok: true, status: 200, json: async () => ({}),
    body: {
      getReader: () => ({
        read: (): Promise<{ done: boolean; value?: Uint8Array }> => new Promise((resolve, reject) => {
          const step = (): void => {
            if (signal.aborted) { reject(new Error("aborted")); return; }
            const next = queue.shift();
            if (next !== undefined) { resolve({ done: false, value: enc.encode(next) }); return; }
            if (ended) { resolve({ done: true }); return; }
            wake = step;
          };
          step();
        }),
      }),
    },
  };
}

Object.assign(globalThis, {
  Node: NodeStub,
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible", activeElement: null,
    createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
    createDocumentFragment: () => el("#document-fragment"),
    createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
    getElementById: (id: string) => byId[id] || (byId[id] = el()),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener() {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {}, assign: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  setInterval: () => 0, clearInterval: () => {},
  fetch: async (path: string, opts: RequestInit = {}): Promise<unknown> => {
    calls.push({ path, method: opts.method || "GET", body: String(opts.body || ""), signal: opts.signal || undefined });
    if (path.endsWith("/pubsub/subscribe")) return feedResponse(opts.signal as AbortSignal);
    return { ok: true, status: 200, json: async () => ({ pattern: "*", channels: [{ name: "orders", subscribers: 2 }], total: 1, truncated: false, patterns: 0 }) };
  },
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const ps = await import(pathToFileURL(join(here, "data-pubsub.js")).href) as {
  DB_PUBSUB_LOG_MAX: number;
  dbPubsubSplit: (rest: string, chunk: string) => { events: { event: string }[]; rest: string };
  dbPubsubPush: (log: DbPubsubLine[], line: DbPubsubLine, max: number) => void;
  dbPubsubIsPattern: (name: string) => boolean;
  dbPubsubAddName: (t: Pick<DbPubsubTab, "watchChannels" | "watchPatterns">, raw: string) => string | null;
  dbPubsubTime: (at: number) => string;
  dbPubsubBytes: (n: number) => string;
  dbPubsubItems: (log: DbPubsubLine[]) => import("../src/ui/timeline.js").TimelineItem[];
  dbPubsubRemember: (recent: string[], name: string, max: number) => string[];
  dbPubsubGlob: (pattern: string, name: string) => boolean;
  dbPubsubOverlap: (t: Pick<DbPubsubTab, "watchChannels" | "watchPatterns">) => [string, string] | null;
  dbPubsubStart: () => void;
  dbPubsubStop: () => void;
};

afterAll(() => {
  ps.dbPubsubStop();
  for (const { k, d } of SAVED) {
    if (d) Object.defineProperty(globalThis, k, d);
    else delete (globalThis as unknown as Record<string, unknown>)[k];
  }
});

/** Let the fetch, the reads and the events after them settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 10; i++) await new Promise((r) => { setTimeout(r, 0); });
}

describe("the NDJSON splitter (SPEC §data.pubsub)", () => {
  it("parses whole lines and keeps the unfinished one for the next chunk", () => {
    const a = ps.dbPubsubSplit("", '{"event":"tick","at":1}\n{"event":"mess');
    expect(a.events).toEqual([{ event: "tick", at: 1 }]);
    expect(a.rest).toBe('{"event":"mess');
    const b = ps.dbPubsubSplit(a.rest, 'age","channel":"c","payload":"x y","size":3,"at":2}\n');
    expect(b.events).toEqual([{ event: "message", channel: "c", payload: "x y", size: 3, at: 2 }]);
    expect(b.rest).toBe("");
  });
  it("skips a blank or unparsable line instead of ending the feed", () => {
    const out = ps.dbPubsubSplit("", '\n  \nnot json\n{"no":"event"}\n{"event":"dropped","count":5}\n');
    expect(out.events).toEqual([{ event: "dropped", count: 5 }]);
  });
});

describe("the log's ring and the watch set (SPEC §data.pubsub)", () => {
  it("keeps the newest lines and lets the oldest fall off past the cap", () => {
    const log: DbPubsubLine[] = [];
    for (let i = 1; i <= 5; i++) ps.dbPubsubPush(log, { line: "note", seq: i, label: "n", text: String(i), at: i }, 3);
    expect(log.map((l) => l.seq)).toEqual([3, 4, 5]);
    expect(ps.DB_PUBSUB_LOG_MAX).toBe(500);
  });
  it("reads the log into event-list rows, newest first, identical messages folding", () => {
    const msg = (seq: number, payload: string): DbPubsubLine => {
      return { line: "message", seq, event: "message", channel: "orders", payload, size: payload.length, at: 1000 + seq };
    };
    const log: DbPubsubLine[] = [
      { line: "note", seq: 1, label: "Listening", text: "orders", at: 1000 },
      msg(2, "a"), msg(3, "b"), msg(4, "b"),
      { line: "note", seq: 5, label: "Dropped", text: "7 messages not shown", at: 1005, bad: true },
    ];
    const items = ps.dbPubsubItems(log);
    expect(items.map((i) => i.id)).toEqual(["5", "4", "3", "2", "1"]);
    expect(items[0].status && items[0].status.tone).toBe("warn");
    expect(items[4].status).toBeUndefined();
    expect([items[0].title, items[0].arg]).toEqual(["Dropped", "7 messages not shown"]);
    expect([items[1].title, items[1].arg]).toEqual(["orders", "b"]);
    expect(items[1].same).toBe(items[2].same);
    expect(items[2].same).not.toBe(items[3].same);
    expect(items[4].same, "a note never folds").toBeUndefined();
  });
  it("a name with a glob character is a pattern, anything else a channel", () => {
    expect(ps.dbPubsubIsPattern("news.*")).toBe(true);
    expect(ps.dbPubsubIsPattern("user:?")).toBe(true);
    expect(ps.dbPubsubIsPattern("[ab]x")).toBe(true);
    expect(ps.dbPubsubIsPattern("orders.created")).toBe(false);
    const t = { watchChannels: [] as string[], watchPatterns: [] as string[] };
    expect(ps.dbPubsubAddName(t, "  orders  ")).toBeNull();
    expect(ps.dbPubsubAddName(t, "news.*")).toBeNull();
    expect(t).toEqual({ watchChannels: ["orders"], watchPatterns: ["news.*"] });
    expect(ps.dbPubsubAddName(t, "orders")).toBe("dataPubsub.alreadyWatched");
    expect(ps.dbPubsubAddName(t, "   ")).toBe("dataPubsub.nameAChannelOrPattern");
    for (let i = 1; i < 32; i++) ps.dbPubsubAddName(t, "c" + i);
    expect(t.watchChannels.length).toBe(32);
    expect(ps.dbPubsubAddName(t, "one-too-many")).toBe("dataPubsub.tooManyNames");
  });
  it("remembers a watched name at the front, a repeat moving up, the oldest falling off", () => {
    const before = ["b", "a"];
    expect(ps.dbPubsubRemember(before, "c", 20)).toEqual(["c", "b", "a"]);
    expect(ps.dbPubsubRemember(before, "a", 20), "a repeat moves up, never twice").toEqual(["a", "b"]);
    expect(before, "the list given is not changed").toEqual(["b", "a"]);
    expect(ps.dbPubsubRemember(["c", "b", "a"], "news.*", 3)).toEqual(["news.*", "c", "b"]);
  });
  it("reads a pattern the way redis matches it, to say when a channel arrives twice", () => {
    const yes: [string, string][] = [
      ["*", "K_LINE_CHANNEL_ETCUSDT_1MIN"], ["K_LINE_*_1MIN", "K_LINE_CHANNEL_ETCUSDT_1MIN"], ["h?llo", "hello"],
      ["h[ae]llo", "hallo"], ["h[^e]llo", "hallo"], ["h[a-c]llo", "hbllo"], ["a\\*b", "a*b"], ["a.b", "a.b"],
      ["h[^]llo", "hxllo"],
    ];
    const no: [string, string][] = [
      ["K_LINE_*_1MIN", "K_LINE_CHANNEL_ETCUSDT_5MIN"], ["h?llo", "hllo"], ["h[ae]llo", "hillo"], ["h[^e]llo", "hello"],
      ["a\\*b", "axb"], ["a.b", "axb"], ["h[]llo", "hxllo"], ["news.*", "orders.created"],
    ];
    for (const [p, n] of yes) expect(ps.dbPubsubGlob(p, n), p + " ~ " + n).toBe(true);
    for (const [p, n] of no) expect(ps.dbPubsubGlob(p, n), p + " !~ " + n).toBe(false);
    expect(ps.dbPubsubOverlap({ watchChannels: ["orders", "K_LINE_X"], watchPatterns: ["news.*", "*"] })).toEqual(["orders", "*"]);
    expect(ps.dbPubsubOverlap({ watchChannels: ["orders"], watchPatterns: ["news.*"] })).toBeNull();
  });
  it("prints the clock to the millisecond and sizes in binary units", () => {
    expect(ps.dbPubsubTime(new Date(2026, 9, 22, 9, 5, 7, 42).getTime())).toBe("09:05:07.042");
    expect(ps.dbPubsubBytes(512)).toBe("512 B");
    expect(ps.dbPubsubBytes(16384)).toBe("16 KiB");
    expect(ps.dbPubsubBytes(40000)).toBe("39.1 KiB");
  });
});

describe("the feed follows the page in front (SPEC §data.pubsub)", () => {
  it("subscribes while the page is in front, aborts when it leaves, and notes the gap on return", async () => {
    unmountDbView();
    mountDbView();
    Object.assign(dbConn(), { conn: "cache" });
    const t = freshTab("pubsub");
    t.watchChannels = ["orders"];
    t.watchPatterns = ["news.*"];
    dbTabs().length = 0;
    dbTabs().push(t);

    ps.dbPubsubStart();
    await settle();
    const subs = calls.filter((c) => c.path.endsWith("/pubsub/subscribe"));
    expect(subs.length).toBe(1);
    expect(subs[0].path).toBe("/api/db/cache/pubsub/subscribe");
    expect(JSON.parse(subs[0].body)).toEqual({ channels: ["orders"], patterns: ["news.*"] });
    expect(calls.some((c) => c.path === "/api/db/cache/pubsub")).toBe(true);
    expect(t.live).toBe("connecting");

    feed!.push('{"event":"subscribed","channels":["orders"],"patterns":["news.*"]}\n');
    feed!.push('{"event":"message","channel":"news.eu","pattern":"news.*","payload":"hi","size":2,"at":5}\n');
    await settle();
    expect(t.live).toBe("live");
    expect(t.channels && t.channels.channels[0].name).toBe("orders");
    expect(t.log.map((l) => l.line)).toEqual(["note", "message"]);
    const msg = t.log[1];
    expect(msg.line === "message" && msg.pattern).toBe("news.*");

    // The page leaves the front: the request is aborted, so the server gives the subscription back.
    ps.dbPubsubStop();
    await settle();
    expect(subs[0].signal!.aborted).toBe(true);
    expect(t.live).toBe("off");
    expect(t.offAt).not.toBeNull();

    // Back in front: a new subscription, and the log says which stretch nobody heard.
    ps.dbPubsubStart();
    await settle();
    const again = calls.filter((c) => c.path.endsWith("/pubsub/subscribe"));
    expect(again.length).toBe(2);
    feed!.push('{"event":"subscribed","channels":["orders"],"patterns":["news.*"]}\n');
    await settle();
    expect(t.live).toBe("live");
    expect(t.offAt).toBeNull();
    const last = t.log[t.log.length - 1];
    expect(last.line === "note" && [last.label, last.bad, last.text.startsWith("from ")]).toEqual(["Not listening", true, true]);

    // A closed line ends the feed with its reason, counted dropped messages first; the body
    // ending after it is no cut-off of its own.
    feed!.push('{"event":"dropped","count":7}\n{"event":"closed","reason":"the server closed the connection"}\n');
    feed!.end();
    await settle();
    expect(t.dropped).toBe(7);
    expect(t.live).toBe("off");
    expect(t.liveNote).toBe("the server closed the connection");
    expect(t.log.filter((l) => l.line === "note").map((l) => l.line === "note" && l.label).slice(-2)).toEqual(["Dropped", "Closed"]);
    ps.dbPubsubStop();
  });

  it("a body that ends without a closed line says the feed was cut off", async () => {
    unmountDbView();
    mountDbView();
    Object.assign(dbConn(), { conn: "cache" });
    const t = freshTab("pubsub");
    t.watchChannels = ["orders"];
    dbTabs().length = 0;
    dbTabs().push(t);
    ps.dbPubsubStart();
    await settle();
    feed!.push('{"event":"subscribed","channels":["orders"],"patterns":[]}\n');
    await settle();
    feed!.end();
    await settle();
    expect(t.live).toBe("off");
    expect(t.liveNote).toBe("The feed was cut off - listening again in 5s");
    const last = t.log[t.log.length - 1];
    expect(last.line === "note" && last.label).toBe("Cut off");
    expect(t.offAt).not.toBeNull();
    ps.dbPubsubStop();
  });

  it("opens no feed while nothing is watched", async () => {
    unmountDbView();
    mountDbView();
    Object.assign(dbConn(), { conn: "cache" });
    dbTabs().length = 0;
    dbTabs().push(freshTab("pubsub"));
    const before = calls.filter((c) => c.path.endsWith("/pubsub/subscribe")).length;
    ps.dbPubsubStart();
    await settle();
    expect(calls.filter((c) => c.path.endsWith("/pubsub/subscribe")).length).toBe(before);
    ps.dbPubsubStop();
  });
});
