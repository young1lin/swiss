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

/* The Remote pages' coalesced re-read (SPEC §remote.panel "Live without a poll",
 * §bus.topics "Notices"): the numbers the SPEC states, held here so a burst can never
 * cost more than one extra read. */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { coalescedRead } from "../src/views/remote-live.js";

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
    const parked = parkedRead();
    const live = coalescedRead(parked.read);
    live.push();
    await vi.advanceTimersByTimeAsync(150); // the read starts, parked
    live.push();
    live.push();
    await vi.advanceTimersByTimeAsync(200); // queued behind the parked one
    parked.done();
    await vi.advanceTimersByTimeAsync(1); // the queued read runs, parked again
    parked.done();
    await vi.advanceTimersByTimeAsync(1);
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
    const parked = parkedRead();
    const live = coalescedRead(parked.read);
    live.push();
    live.stop();
    await vi.advanceTimersByTimeAsync(5000);
    parked.done();
    await vi.advanceTimersByTimeAsync(5000);
  });
});
