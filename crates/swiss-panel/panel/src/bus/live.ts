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

/* One coalesced re-read, and a subscription held only while the document is visible
   (SPEC §bus.topics "Notices", "Demand").

   coalescedRead is the one coalescer. Remote Targets and Remote Runs use it; a later
   page imports it from here rather than growing a second one with different numbers.
   A notice says that something moved, never what, so the page re-reads through its
   owning route — but a burst must cost at most one more read, not one read a frame:

   - 150 ms after the last notice, and never more than 1 s after the first of a burst;
   - one read in flight, at most one queued behind it;
   - a hidden page holds its re-read until it is visible again;
   - every WELCOME pushes one re-read too (SPEC §bus.topics "Resync"): the link that was
     down may have missed any notice. A page whose resync reads more than its notices do
     (Remote clears its caches first) passes { resync: false } and pushes on its own.

   busOnWhileVisible is for a topic the gateway samples only while someone is looking
   (the memory reading, MCP health). It subscribes while the document is visible,
   unsubscribes on hide, and on show subscribes again and then calls onVisible, which
   is the page's re-read; a WELCOME while it is held calls onVisible too. Every other topic stays subscribed while hidden and lets the
   coalescer hold its read. */

import type { DescMessage } from "../vendor/protobuf/2.16.0/descriptors.js";
import type { MessageShape } from "../vendor/protobuf/2.16.0/types.js";
import { busOn, busOnResync, type BusTopic } from "./bus.js";

const SETTLE_MS = 150;
const CAP_MS = 1000;

export interface CoalescedRead {
  /** A notice arrived: schedule the re-read it asks for. */
  push(): void;
  /** The page went away: timers cleared, the queue dropped, the read chain ended. */
  stop(): void;
}

export interface CoalescedReadOptions {
  /** Push one re-read after every WELCOME. Default true. */
  resync?: boolean;
}

export function coalescedRead(read: () => Promise<unknown>, opts: CoalescedReadOptions = {}): CoalescedRead {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let capTimer: ReturnType<typeof setTimeout> | null = null;
  let inFlight = false;
  let queued = false;
  let held = false;
  let stopped = false;

  const fire = (): void => {
    // Whichever timer called, the other one is spent: cancel it, or a settled read
    // would fire again when its cap lands (caught by the coalescing tests).
    if (timer) clearTimeout(timer);
    if (capTimer) clearTimeout(capTimer);
    timer = null;
    capTimer = null;
    if (stopped) return;
    // A hidden page holds its re-read: the read would paint a page nobody sees.
    if (document.hidden) {
      held = true;
      return;
    }
    // One read in flight, at most one queued behind it: a burst costs one more read.
    if (inFlight) {
      queued = true;
      return;
    }
    inFlight = true;
    void Promise.resolve(read()).catch((): void => {}).then((): void => {
      inFlight = false;
      if (stopped) return;
      if (queued) {
        queued = false;
        fire();
      }
    });
  };

  const onVisible = (): void => {
    if (held && !document.hidden) {
      held = false;
      fire();
    }
  };
  document.addEventListener("visibilitychange", onVisible);

  const push = (): void => {
    if (stopped) return;
    // The settle restarts on every notice: 150 ms after the LAST one of the burst.
    if (timer) clearTimeout(timer);
    timer = setTimeout(fire, SETTLE_MS);
    // The cap arms once per burst: never more than 1 s after the FIRST notice.
    if (!capTimer) capTimer = setTimeout(fire, CAP_MS);
  };
  const offResync = opts.resync === false ? null : busOnResync(push);

  return {
    push,
    stop(): void {
      stopped = true;
      if (timer) clearTimeout(timer);
      if (capTimer) clearTimeout(capTimer);
      timer = null;
      capTimer = null;
      document.removeEventListener("visibilitychange", onVisible);
      if (offResync) offResync();
    },
  };
}

/** Subscribe while the document is visible. Hide sends UNSUBSCRIBE; show subscribes
 *  again and then calls `onVisible`. The returned function is unmount: if the topic
 *  was already dropped because the document is hidden, it sends nothing more. */
export function busOnWhileVisible<Desc extends DescMessage>(
  topic: BusTopic,
  id: number,
  body: Desc,
  fn: (msg: MessageShape<Desc>, origin: string) => void,
  onVisible: () => void,
): () => void {
  let off: (() => void) | null = null;
  let stopped = false;

  const hold = (): void => {
    if (stopped || off || document.visibilityState !== "visible") return;
    off = busOn(topic, id, body, fn);
  };

  const onChange = (): void => {
    if (stopped) return;
    if (document.visibilityState === "visible") {
      if (off) return;
      hold();
      onVisible();
    } else if (off) {
      off();
      off = null;
    }
  };

  document.addEventListener("visibilitychange", onChange);
  hold();
  // A WELCOME re-subscribes what is held (bus.ts); the page re-reads what it shows. A
  // hidden page holds nothing and re-reads on show instead.
  const offResync = busOnResync(() => { if (!stopped && off) onVisible(); });

  return () => {
    stopped = true;
    offResync();
    document.removeEventListener("visibilitychange", onChange);
    if (off) {
      off();
      off = null;
    }
  };
}
