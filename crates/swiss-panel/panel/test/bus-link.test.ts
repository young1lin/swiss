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

/* The page's bus link, driven with real encoded frames through a fake socket
   (SPEC §bus.link, SPEC §bus.topics). No port is opened. */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import {
  busOn,
  busOnResync,
  busRequest,
  installBus,
  pageId,
  type BusSocket,
} from "../src/bus/bus.js";
import { BusFault } from "../src/bus/link.js";
import {
  ByeReason,
  ByeSchema,
  BuildSchema,
  FrameSchema,
  HelloSchema,
  Kind,
  LinkId,
  PongSchema,
  TopicId,
  TopicsSchema,
  WelcomeSchema,
  type Frame,
} from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { ConsoleListSchema, ConsolesId, ConsolesPageSchema, ConsoleSavedSchema } from "../src/bus/pb/swiss/bus/v1/data_pb.js";
import { mountBusLine } from "../src/bus/line.js";

class FakeSocket implements BusSocket {
  static all: FakeSocket[] = [];
  sent: Uint8Array[] = [];
  binaryType: BinaryType = "blob";
  private listeners = new Map<string, Array<(ev: Event) => void>>();
  url: string;
  constructor(url: string) {
    this.url = url;
    FakeSocket.all.push(this);
  }
  addEventListener(type: string, listener: (ev: Event) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }
  removeEventListener(type: string, listener: (ev: Event) => void): void {
    const list = this.listeners.get(type) ?? [];
    this.listeners.set(type, list.filter((fn) => fn !== listener));
  }
  send(data: Uint8Array): void { this.sent.push(data); }
  close(): void { this.emit("close", new Event("close")); }
  open(): void { this.emit("open", new Event("open")); }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    this.emit("message", new MessageEvent("message", { data: copy.buffer }));
  }
  private emit(type: string, ev: Event): void {
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }
}

let random = (): number => 0.5;
let reloads: string[] = [];
let infos = 0;

function latest(): FakeSocket {
  const sock = FakeSocket.all[FakeSocket.all.length - 1];
  if (!sock) throw new Error("no socket");
  return sock;
}

function frame(fields: {
  module?: number;
  sub?: number;
  id?: number;
  kind?: number;
  seq?: number;
  replyTo?: number;
  key?: string;
  origin?: string;
  body?: Uint8Array;
  fault?: { code: string; message: string; params: { [key: string]: string } };
}): Uint8Array {
  return toBinary(FrameSchema, create(FrameSchema, fields));
}

function welcome(build: string, heartbeatMs = 20_000, maxFrame = 4 * 1024 * 1024): Uint8Array {
  return frame({
    module: 0,
    sub: 1,
    id: LinkId.HELLO,
    kind: Kind.RESPONSE,
    seq: 1,
    replyTo: 1,
    body: toBinary(WelcomeSchema, create(WelcomeSchema, {
      protocol: 1,
      connId: "7",
      build,
      heartbeatMs: BigInt(heartbeatMs),
      maxFrame,
      serverMs: 0n,
    })),
  });
}

function decoded(bytes: Uint8Array): Frame {
  return fromBinary(FrameSchema, bytes);
}

function settle(p: Promise<unknown>): Promise<string> {
  return p.then(() => "ok", (err: unknown) => err instanceof BusFault ? err.code : "throw");
}

beforeEach(() => {
  vi.useFakeTimers();
  random = () => 0.5;
  reloads = [];
  infos = 0;
  FakeSocket.all.length = 0;
  document.body.innerHTML = '<div class="workbench"></div>';
  installBus({
    socket: FakeSocket,
    random: () => random(),
    reload: (build) => { reloads.push(build); },
    info: () => { infos += 1; return Promise.resolve(); },
    notice: mountBusLine,
    url: "ws://127.0.0.1/api/bus",
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("the bus link (SPEC §bus.link)", () => {
  it("handshakes with HELLO then uses the WELCOME heartbeat", async () => {
    const sock = latest();
    expect(sock.sent).toHaveLength(0);
    expect(document.querySelector(".note")).toBeNull();
    sock.open();
    expect(sock.sent).toHaveLength(1);
    const hello = decoded(sock.sent[0]);
    expect(hello.kind).toBe(Kind.REQUEST);
    expect(hello.id).toBe(LinkId.HELLO);
    expect(hello.seq).toBe(1);
    const body = fromBinary(HelloSchema, hello.body);
    expect(body.protocol).toBe(1);
    expect(body.pageId).toBe(pageId);
    sock.receive(welcome("stamp-a", 1_000));
    expect(reloads).toEqual([]);
    await vi.advanceTimersByTimeAsync(999);
    expect(sock.sent).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    const ping = decoded(sock.sent[1]);
    expect(ping.id).toBe(LinkId.PING);
    expect(ping.kind).toBe(Kind.REQUEST);
    expect(ping.seq).toBe(2);
  });

  it("answers out-of-order responses by reply_to, and a fault by its code", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a"));
    const route = { module: 6, sub: 1, id: ConsolesId.CONSOLES_LIST, schema: ConsolesPageSchema };
    const first = busRequest(route, "", create(ConsolesPageSchema, { conn: "a" }), ConsoleListSchema);
    const second = busRequest(route, "", create(ConsolesPageSchema, { conn: "b" }), ConsoleListSchema);
    const sent = sock.sent.slice(-2).map((bytes) => decoded(bytes));
    expect(sent[0].seq).not.toBe(sent[1].seq);
    sock.receive(frame({
      module: 6, sub: 1, id: ConsolesId.CONSOLES_LIST, kind: Kind.RESPONSE, seq: 4, replyTo: sent[1].seq,
      body: toBinary(ConsoleListSchema, create(ConsoleListSchema, { next: "b" })),
    }));
    await expect(second).resolves.toMatchObject({ next: "b" });
    let firstState = "pending";
    void first.then(() => { firstState = "ok"; }, () => { firstState = "fail"; });
    await Promise.resolve();
    expect(firstState).toBe("pending");
    sock.receive(frame({
      module: 6, sub: 1, id: ConsolesId.CONSOLES_PUT, kind: Kind.RESPONSE, seq: 5, replyTo: sent[0].seq,
      fault: { code: "console.changed", message: "conflict", params: { id: "c1" } },
    }));
    await expect(first).rejects.toMatchObject({ code: "console.changed", params: { id: "c1" } });
  });

  it("rejects a request with bus.timeout and bus.offline when the link drops", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a"));
    const route = { module: 6, sub: 1, id: ConsolesId.CONSOLES_LIST, schema: ConsolesPageSchema };
    const timed = settle(busRequest(route, "", create(ConsolesPageSchema, { conn: "a" }), ConsoleListSchema));
    await vi.advanceTimersByTimeAsync(14_999);
    expect(await Promise.race([timed, Promise.resolve("pending")])).toBe("pending");
    await vi.advanceTimersByTimeAsync(1);
    expect(await timed).toBe("bus.timeout");

    const dropped = settle(busRequest(route, "", create(ConsolesPageSchema, { conn: "a" }), ConsoleListSchema));
    sock.close();
    await Promise.resolve();
    expect(await dropped).toBe("bus.offline");
  });

  it("refuses a frame past the WELCOME max_frame with bus.tooLarge", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a", 20_000, 32));
    const before = sock.sent.length;
    const huge = "x".repeat(80);
    const result = await settle(busRequest(
      { module: 6, sub: 1, id: ConsolesId.CONSOLES_LIST, schema: ConsolesPageSchema },
      huge,
      create(ConsolesPageSchema, { conn: huge }),
      ConsoleListSchema,
    ));
    expect(result).toBe("bus.tooLarge");
    expect(sock.sent.length).toBe(before);
  });

  it("doubles the backoff from 0.5s to 30s and resets it only on WELCOME", async () => {
    const sock = latest();
    sock.open();
    sock.close();
    await vi.advanceTimersByTimeAsync(499);
    expect(FakeSocket.all).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(FakeSocket.all).toHaveLength(2);
    latest().open();
    latest().close();
    await vi.advanceTimersByTimeAsync(999);
    expect(FakeSocket.all).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(FakeSocket.all).toHaveLength(3);
    latest().open();
    latest().receive(welcome("stamp-a"));
    latest().close();
    await vi.advanceTimersByTimeAsync(499);
    expect(FakeSocket.all).toHaveLength(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(FakeSocket.all).toHaveLength(4);
  });

  it("jitters the backoff by 20 percent", async () => {
    random = () => 0;
    const sock = latest();
    sock.open();
    sock.close();
    await vi.advanceTimersByTimeAsync(399);
    expect(FakeSocket.all).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(FakeSocket.all).toHaveLength(2);
  });

  it("reconnects at once on online", async () => {
    const sock = latest();
    sock.open();
    sock.close();
    await vi.advanceTimersByTimeAsync(100);
    expect(FakeSocket.all).toHaveLength(1);
    window.dispatchEvent(new Event("online"));
    expect(FakeSocket.all).toHaveLength(2);
  });

  it("reconnects when PONG is overdue and at once on BYE SLOW", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a", 1_000));
    await vi.advanceTimersByTimeAsync(1_000);
    expect(decoded(sock.sent[sock.sent.length - 1]).id).toBe(LinkId.PING);
    await vi.advanceTimersByTimeAsync(9_999);
    expect(FakeSocket.all).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    await vi.advanceTimersByTimeAsync(500);
    expect(FakeSocket.all).toHaveLength(2);

    latest().open();
    latest().receive(welcome("stamp-a"));
    const before = FakeSocket.all.length;
    latest().receive(frame({
      module: 0, sub: 1, id: LinkId.BYE, kind: Kind.EVENT, seq: 2,
      body: toBinary(ByeSchema, create(ByeSchema, { reason: ByeReason.BYE_SLOW })),
    }));
    latest().close();
    await vi.advanceTimersByTimeAsync(0);
    expect(FakeSocket.all.length).toBe(before + 1);
  });

  it("an answered PING clears its deadline: the link stays and pings again a heartbeat later", async () => {
    const sock = latest();
    sock.open();
    const close = vi.spyOn(sock, "close");
    // SPEC §bus.link: the served heartbeat (20 s) outlasts the PONG deadline (10 s): a PONG
    // the link failed to recognize would close the socket before the next PING.
    sock.receive(welcome("stamp-a"));
    await vi.advanceTimersByTimeAsync(20_000);
    const ping = decoded(sock.sent[sock.sent.length - 1]);
    expect(ping.id).toBe(LinkId.PING);
    sock.receive(frame({
      module: 0, sub: 1, id: LinkId.PING, kind: Kind.RESPONSE, seq: 2, replyTo: ping.seq,
      body: toBinary(PongSchema, create(PongSchema, { sentMs: 1n, serverMs: 2n })),
    }));
    const answered = sock.sent.length;
    // Past the PONG deadline and past any reconnect backoff that a close would start.
    await vi.advanceTimersByTimeAsync(11_000);
    expect(FakeSocket.all, "the answered link is not dropped").toHaveLength(1);
    expect(close).not.toHaveBeenCalled();
    expect(sock.sent).toHaveLength(answered);
    await vi.advanceTimersByTimeAsync(9_000);
    expect(sock.sent, "the next PING goes a heartbeat after the PONG").toHaveLength(answered + 1);
    expect(decoded(sock.sent[answered]).id).toBe(LinkId.PING);
    expect(FakeSocket.all).toHaveLength(1);
  });

  it("one-second heartbeats keep the answered socket past the PONG deadline", async () => {
    const sock = latest();
    const close = vi.spyOn(sock, "close");
    sock.open();
    sock.receive(welcome("stamp-a", 1_000));
    const pong = () => {
      const ping = decoded(sock.sent[sock.sent.length - 1]);
      expect(ping).toMatchObject({ module: 0, sub: 1, kind: Kind.REQUEST, id: LinkId.PING });
      sock.receive(frame({
        module: 0, sub: 1, id: LinkId.PING, kind: Kind.RESPONSE, seq: ping.seq,
        replyTo: ping.seq, body: toBinary(PongSchema, create(PongSchema, { sentMs: 1n, serverMs: 2n })),
      }));
    };
    await vi.advanceTimersByTimeAsync(1_000);
    pong();
    const answered = sock.sent.length;
    await vi.advanceTimersByTimeAsync(999);
    expect(sock.sent).toHaveLength(answered);
    await vi.advanceTimersByTimeAsync(1);
    expect(sock.sent, "the next PING goes at the next heartbeat").toHaveLength(answered + 1);
    pong();
    // Answer later heartbeats too, past the first PONG's 10 s deadline (SPEC §bus.link).
    for (let i = 0; i < 10; i++) {
      await vi.advanceTimersByTimeAsync(1_000);
      pong();
    }
    expect(FakeSocket.all).toEqual([sock]);
    expect(close).not.toHaveBeenCalled();
  });

  it("reloads on BYE PROTOCOL and on a later WELCOME with another build", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a"));
    expect(reloads).toEqual([]);
    sock.receive(frame({
      module: 0, sub: 1, id: LinkId.BUILD, kind: Kind.EVENT, seq: 2,
      body: toBinary(BuildSchema, create(BuildSchema, { build: "stamp-b" })),
    }));
    expect(reloads).toEqual(["stamp-b"]);
    sock.receive(frame({
      module: 0, sub: 1, id: LinkId.BYE, kind: Kind.EVENT, seq: 3,
      body: toBinary(ByeSchema, create(ByeSchema, { reason: ByeReason.BYE_PROTOCOL })),
    }));
    sock.close();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(FakeSocket.all).toHaveLength(1);
    expect(reloads).toEqual(["stamp-b", "stamp-a"]);
  });

  it("reloads when a later WELCOME carries another build", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a"));
    sock.close();
    await vi.advanceTimersByTimeAsync(500);
    latest().open();
    latest().receive(welcome("stamp-b"));
    expect(reloads).toEqual(["stamp-b"]);
  });

  it("follows a failed connect with one GET /api/info", async () => {
    latest().close();
    expect(infos).toBe(1);
    await vi.advanceTimersByTimeAsync(500);
    latest().open();
    latest().receive(welcome("stamp-a"));
    latest().close();
    expect(infos).toBe(1);
  });

  it("draws the reconnecting line after 3s down and removes it on WELCOME", async () => {
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a"));
    expect(document.querySelector(".note")).toBeNull();
    sock.close();
    await vi.advanceTimersByTimeAsync(2_999);
    expect(document.querySelector(".note")).toBeNull();
    await vi.advanceTimersByTimeAsync(1);
    expect(document.querySelector(".note")?.textContent).toContain("Reconnecting");
    await vi.advanceTimersByTimeAsync(500);
    latest().open();
    latest().receive(welcome("stamp-a"));
    expect(document.querySelector(".note")).toBeNull();
  });
});

describe("topics and resync (SPEC §bus.topics)", () => {
  it("subscribes once, re-subscribes and re-reads after every WELCOME", () => {
    const reads: string[] = [];
    busOnResync(() => { reads.push("read"); });
    const topic = { module: 6, sub: 1, key: "alpha" };
    const stop = busOn(topic, ConsolesId.CONSOLES_CHANGED, ConsoleSavedSchema, () => {});
    const again = busOn(topic, ConsolesId.CONSOLES_CHANGED, ConsoleSavedSchema, () => {});
    const sock = latest();
    sock.open();
    expect(sock.sent).toHaveLength(1);
    sock.receive(welcome("stamp-a"));
    const subscribed = decoded(sock.sent[sock.sent.length - 1]);
    expect(subscribed.id).toBe(TopicId.SUBSCRIBE);
    const topics = fromBinary(TopicsSchema, subscribed.body);
    expect(topics.topics).toHaveLength(1);
    expect(topics.topics[0].key).toBe("alpha");
    expect(reads).toEqual(["read"]);
    stop();
    expect(decoded(sock.sent[sock.sent.length - 1]).id).toBe(TopicId.SUBSCRIBE);
    again();
    expect(decoded(sock.sent[sock.sent.length - 1]).id).toBe(TopicId.UNSUBSCRIBE);
  });

  it("re-reads after a second WELCOME when the topic is still held", () => {
    const reads: string[] = [];
    busOnResync(() => { reads.push("read"); });
    busOn({ module: 6, sub: 1, key: "alpha" }, ConsolesId.CONSOLES_CHANGED, ConsoleSavedSchema, () => {});
    const sock = latest();
    sock.open();
    sock.receive(welcome("stamp-a"));
    sock.close();
    vi.advanceTimersByTime(500);
    latest().open();
    latest().receive(welcome("stamp-a"));
    expect(reads).toEqual(["read", "read"]);
    expect(decoded(latest().sent[latest().sent.length - 1]).id).toBe(TopicId.SUBSCRIBE);
  });
});
