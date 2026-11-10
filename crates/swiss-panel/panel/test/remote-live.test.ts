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

/* The shared coalesced re-read (SPEC §bus.topics "Notices") and a subscription held
 * only while the document is visible (SPEC §bus.topics "Demand"). The numbers the
 * SPEC states are held here so a burst can never cost more than one extra read. */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { create, fromBinary, toBinary } from "../src/bus/codec.js";
import { installBus, type BusSocket } from "../src/bus/bus.js";
import { busOnWhileVisible, coalescedRead } from "../src/bus/live.js";
import { FrameSchema, Kind, LinkId, Module, NoticeSchema, TopicId, WelcomeSchema } from "../src/bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, RemoteSeatId } from "../src/bus/pb/swiss/bus/v1/host_pb.js";

function hidden(value: boolean): void {
  vi.spyOn(document, "hidden", "get").mockReturnValue(value);
}

/** A read the test resolves by hand, so "in flight" is real and observable. */
function parkedRead(): { read: () => Promise<void>; done: () => void } {
  let finish: (() => void) | null = null;
  const read = (): Promise<void> => new Promise<void>((resolve) => { finish = resolve; });
  return { read, done: () => { finish?.(); finish = null; } };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("coalescedRead (SPEC §bus.topics \"Notices\" - re-reads coalesce)", () => {
  it("a burst settles into one read: 150 ms after the last notice", async () => {
    let reads = 0;
    const live = coalescedRead(async () => { reads += 1; });
    live.push();
    live.push();
    live.push();
    await vi.advanceTimersByTimeAsync(149);
    expect(reads).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(reads).toBe(1);
    live.stop();
  });

  it("never more than 1 s after the first: the cap fires while notices keep settling", async () => {
    let reads = 0;
    const live = coalescedRead(async () => { reads += 1; });
    live.push();
    // A notice every 100 ms keeps restarting the 150 ms settle timer; the 1 s cap
    // from the FIRST notice still fires, so the burst cannot postpone the read forever.
    for (let t = 0; t < 900; t += 100) {
      await vi.advanceTimersByTimeAsync(100);
      live.push();
      expect(reads).toBe(0);
    }
    await vi.advanceTimersByTimeAsync(100); // t=1000: the cap fires
    expect(reads).toBe(1);
    live.stop();
  });

  it("one read in flight, at most one queued behind it", async () => {
    let reads = 0;
    const parked = parkedRead();
    const live = coalescedRead(async () => { reads += 1; await parked.read(); });
    live.push();
    await vi.advanceTimersByTimeAsync(150); // the read starts, parked
    expect(reads, "the first notice starts one read").toBe(1);
    live.push();
    live.push();
    await vi.advanceTimersByTimeAsync(200); // queued behind the parked one
    expect(reads, "notices arriving during a read never start a second one").toBe(1);
    parked.done();
    await vi.advanceTimersByTimeAsync(1); // the queued read runs, parked again
    expect(reads, "exactly one queued read follows the finished one").toBe(2);
    parked.done();
    await vi.advanceTimersByTimeAsync(2000);
    expect(reads, "a burst of three notices costs no more than one extra read").toBe(2);
    live.stop();
  });

  it("a hidden page holds its re-read until it is visible again", async () => {
    let reads = 0;
    hidden(true);
    const live = coalescedRead(async () => { reads += 1; });
    live.push();
    await vi.advanceTimersByTimeAsync(2000);
    expect(reads).toBe(0);
    hidden(false);
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(0);
    expect(reads).toBe(1);
    live.stop();
  });

  it("stop ends the chain: no timer, no queued read, no listener left", async () => {
    // A pending settle timer must not survive stop(): however much time passes, no read.
    let reads = 0;
    const parked = parkedRead();
    const live = coalescedRead(async () => { reads += 1; await parked.read(); });
    live.push();
    live.stop();
    await vi.advanceTimersByTimeAsync(5000);
    expect(reads, "stop clears the settle and cap timers").toBe(0);
    parked.done();
    await vi.advanceTimersByTimeAsync(5000);
    expect(reads, "a stopped chain never starts a read later").toBe(0);

    // A read already in flight when a notice queues behind it and stop() follows must
    // not chain into another read when its promise resolves - the notice has to reach
    // fire() first, or the queue never holds anything and the stopped check goes untested.
    let during = 0;
    const inFlight = parkedRead();
    const stopped = coalescedRead(async () => { during += 1; await inFlight.read(); });
    stopped.push();
    await vi.advanceTimersByTimeAsync(150);
    expect(during, "the first read is in flight").toBe(1);
    stopped.push();
    await vi.advanceTimersByTimeAsync(150); // the notice reaches fire(): queued behind the parked read
    stopped.stop();
    inFlight.done();
    await vi.advanceTimersByTimeAsync(2000);
    expect(during, "no queued read is started after stop()").toBe(1);

    // The visibilitychange listener is unsubscribed, so a hidden page stopped while
    // held cannot be woken by a later visibilitychange either.
    let hiddenReads = 0;
    hidden(true);
    const removed = vi.spyOn(document, "removeEventListener");
    const held = coalescedRead(async () => { hiddenReads += 1; });
    held.push();
    held.stop();
    expect(removed.mock.calls.some((call) => call[0] === "visibilitychange")).toBe(true);
    hidden(false);
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(2000);
    expect(hiddenReads, "there is no held read left to wake").toBe(0);
  });
});

/* A bus the visibility test drives by hand: SUBSCRIBE and UNSUBSCRIBE are real frames. */
class FakeSocket implements BusSocket {
  static all: FakeSocket[] = [];
  sent: Uint8Array[] = [];
  binaryType: BinaryType = "blob";
  private listeners = new Map<string, Array<(ev: Event) => void>>();
  constructor() { FakeSocket.all.push(this); }
  addEventListener(type: string, listener: (ev: Event) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }
  removeEventListener(): void { /* the link outlives the assertion */ }
  send(data: Uint8Array): void { this.sent.push(data); }
  close(): void { /* left open: the test ends the page, not the socket */ }
  private emit(type: string, ev: Event): void {
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }
  open(): void { this.emit("open", new Event("open")); }
  receive(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    for (const fn of this.listeners.get("message") ?? []) fn(new MessageEvent("message", { data: copy.buffer }));
  }
  static latest(): FakeSocket {
    const sock = FakeSocket.all[FakeSocket.all.length - 1];
    if (!sock) throw new Error("the link did not create a socket");
    return sock;
  }
}

function welcome(): Uint8Array {
  return toBinary(FrameSchema, create(FrameSchema, {
    module: 0, sub: 1, id: LinkId.HELLO, kind: Kind.RESPONSE, seq: 1, replyTo: 1,
    body: toBinary(WelcomeSchema, create(WelcomeSchema, {
      protocol: 1, connId: "7", build: "stamp-a", heartbeatMs: 60000n, maxFrame: 65536, serverMs: 0n,
    })),
  }));
}

function openBus(): FakeSocket {
  FakeSocket.all.length = 0;
  installBus({
    socket: FakeSocket,
    random: () => 0.5,
    reload: () => {},
    info: () => Promise.resolve(),
    notice: () => {},
    url: "ws://127.0.0.1/api/bus",
  });
  const sock = FakeSocket.latest();
  sock.open();
  sock.receive(welcome());
  return sock;
}

/** TOPIC request ids, in the order the link sent them. HELLO and PING are not topics. */
function topicIds(sock: FakeSocket): number[] {
  const ids: number[] = [];
  for (const bytes of sock.sent) {
    const frame = fromBinary(FrameSchema, bytes);
    if (frame.module === 0 && frame.sub === 2) ids.push(frame.id);
  }
  return ids;
}

function setVisible(value: boolean): void {
  vi.spyOn(document, "visibilityState", "get").mockReturnValue(value ? "visible" : "hidden");
}

describe("busOnWhileVisible (SPEC §bus.topics \"Demand\")", () => {
  // SPEC §bus.topics: a sampled topic is held only while the document is visible.
  it("subscribes while visible, drops the topic on hide, and re-reads once on show", () => {
    setVisible(true);
    const sock = openBus();
    let shown = 0;
    const stop = busOnWhileVisible(
      { module: Module.HOST, sub: HostSub.REMOTE_SEAT, key: "" },
      RemoteSeatId.REMOTE_SEAT_CHANGED,
      NoticeSchema,
      () => {},
      () => { shown += 1; },
    );
    expect(topicIds(sock)).toEqual([TopicId.SUBSCRIBE]);
    expect(shown, "the first subscribe is the page's own read, not onVisible").toBe(0);

    setVisible(false);
    document.dispatchEvent(new Event("visibilitychange"));
    expect(topicIds(sock)).toEqual([TopicId.SUBSCRIBE, TopicId.UNSUBSCRIBE]);
    expect(shown).toBe(0);

    setVisible(true);
    document.dispatchEvent(new Event("visibilitychange"));
    expect(topicIds(sock)).toEqual([TopicId.SUBSCRIBE, TopicId.UNSUBSCRIBE, TopicId.SUBSCRIBE]);
    expect(shown, "show re-subscribes and then re-reads once").toBe(1);

    setVisible(false);
    document.dispatchEvent(new Event("visibilitychange"));
    const sent = topicIds(sock).length;
    stop();
    expect(topicIds(sock).length, "unmount while hidden sends nothing more").toBe(sent);
  });

  // SPEC §bus.topics "Resync": a held topic is re-read after every WELCOME; a hidden page
  // holds nothing and re-reads on show instead.
  it("a WELCOME while held re-reads once; while hidden it waits for the show", () => {
    setVisible(true);
    const sock = openBus();
    let shown = 0;
    const stop = busOnWhileVisible(
      { module: Module.HOST, sub: HostSub.REMOTE_SEAT, key: "" },
      RemoteSeatId.REMOTE_SEAT_CHANGED,
      NoticeSchema,
      () => {},
      () => { shown += 1; },
    );
    sock.receive(welcome());
    expect(shown, "the reconnect re-reads what the page shows").toBe(1);
    setVisible(false);
    document.dispatchEvent(new Event("visibilitychange"));
    sock.receive(welcome());
    expect(shown, "nothing held, nothing re-read").toBe(1);
    stop();
    setVisible(true);
    sock.receive(welcome());
    expect(shown, "an unmounted page is not re-read").toBe(1);
  });
});

describe("coalescedRead after a WELCOME (SPEC §bus.topics \"Resync\")", () => {
  // The link that was down may have missed any notice: every follower re-reads once.
  it("pushes one re-read per WELCOME unless the page owns its resync; stop drops it", async () => {
    setVisible(true);
    const sock = openBus();
    let reads = 0;
    let owned = 0;
    const live = coalescedRead(async () => { reads += 1; });
    const own = coalescedRead(async () => { owned += 1; }, { resync: false });
    sock.receive(welcome());
    await vi.advanceTimersByTimeAsync(149);
    expect(reads, "the resync is coalesced like a notice").toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(reads).toBe(1);
    expect(owned, "a page that owns its resync is not pushed").toBe(0);
    live.stop();
    sock.receive(welcome());
    await vi.advanceTimersByTimeAsync(2000);
    expect(reads, "a stopped coalescer is off the resync list").toBe(1);
    own.stop();
  });
});
