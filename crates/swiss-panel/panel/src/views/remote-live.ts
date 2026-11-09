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

/* The Remote pages' live re-read (SPEC §remote.panel "Live without a poll", §bus.topics
   "Notices"). A notice says that something moved, never what, so the page re-reads through
   its owning route - but a burst must cost at most one more read, not one read a frame:

   - 150 ms after the last notice, and never more than 1 s after the first of a burst;
   - one read in flight, at most one queued behind it;
   - a hidden page holds its re-read until it is visible again.

   The coalescing lives here so both Remote pages share one reading of it, and the view tests
   drive the timers through the same numbers the SPEC states. */

const SETTLE_MS = 150;
const CAP_MS = 1000;

export interface CoalescedRead {
  /** A notice arrived: schedule the re-read it asks for. */
  push(): void;
  /** The page went away: timers cleared, the queue dropped, the read chain ended. */
  stop(): void;
}

export function coalescedRead(read: () => Promise<unknown>): CoalescedRead {
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

  return {
    push(): void {
      if (stopped) return;
      // The settle restarts on every notice: 150 ms after the LAST one of the burst.
      if (timer) clearTimeout(timer);
      timer = setTimeout(fire, SETTLE_MS);
      // The cap arms once per burst: never more than 1 s after the FIRST notice.
      if (!capTimer) capTimer = setTimeout(fire, CAP_MS);
    },
    stop(): void {
      stopped = true;
      if (timer) clearTimeout(timer);
      if (capTimer) clearTimeout(capTimer);
      timer = null;
      capTimer = null;
      document.removeEventListener("visibilitychange", onVisible);
    },
  };
}
