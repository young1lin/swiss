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

/* Remote Runs: the live tail this page holds for an OPEN running row is capped at the same
   256 KB the gateway itself keeps live (runs.rs MAX_LIVE_OUTPUT_BYTES). The regression this
   file owns: before the cap, live[runId].text grew without bound for as long as the row
   stayed open - a runaway remote child streaming megabytes grew the tab's memory hour over
   hour. The cap keeps the NEWEST bytes (what a human is actually watching), still advances
   the cursor (the next pull continues from the truth), and the finished record repaints
   the row from its durable tail when the run ends. */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view's public surface
   is asserted by name below, so a typed import would be circular. */
let view: any;

const here = dirname(fileURLToPath(import.meta.url));
const TAIL = 256 * 1024;

const chunks = [
  { output: "a".repeat(200000), nextCursor: 200000, terminal: false },
  { output: "b".repeat(200000), nextCursor: 400000, terminal: true },
  { output: "", nextCursor: 400000, terminal: true },
];
let outputCalls: string[] = [];

/* The shell skeleton, taken from the SERVED index.html rather than invented here (the same
 * rule as the other view suites): #pane must exist before the import. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

const row = {
  runId: 7, label: "build", state: "running", action: "remote.exec",
  input: { argv: ["make"] }, actor: "test", startedAt: Date.now(),
};

beforeEach(async () => {
  outputCalls = [];
  document.body.innerHTML = shellSkeleton();
  Object.assign(globalThis, {
    fetch: (url: string): Promise<Response> => {
      const u = String(url);
      let body: unknown = {};
      if (u.startsWith("/api/remote/targets")) body = { targets: [] };
      else if (u.startsWith("/api/remote/runs")) body = { runs: [], active: [row], nextBefore: null, usage: { bytes: 0, runs: 0 } };
      else if (u.startsWith("/api/runs/7/output")) {
        outputCalls.push(u);
        body = chunks[Math.min(outputCalls.length - 1, chunks.length - 1)];
      }
      return Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve(body) } as Response);
    },
  });
  vi.useFakeTimers();
  view = await import("../src/views/remote-runs.js");
});

afterEach(() => {
  view.unmount();
  vi.useRealTimers();
});

describe("Remote Runs / live tail", () => {
  it("caps an open live row at the gateway's live floor, keeping the newest bytes and still advancing", async () => {
    await view.mount();
    // Open the running row: the immediate pull brings the first 200 KB.
    /* panel-ui's library timeline addresses rows by [data-rrun] and toggles on the summary
       button (.tl-sum) - master's own data-rtog hook went with its hand-built rows. */
    document.querySelector('[data-rrun="7"] .tl-sum')!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await vi.advanceTimersByTimeAsync(0);
    const pre = document.querySelector('[data-rrun="7"] .tl-body pre')!;
    expect(pre.textContent).toBe("a".repeat(200000));

    // The interval pull brings 200 KB more: 400 KB total against a 256 KB floor. The held
    // tail must drop the OLDEST bytes, keep the newest, and say it was trimmed.
    await vi.advanceTimersByTimeAsync(1500);
    const text = pre.textContent ?? "";
    expect(text.endsWith("b".repeat(200000))).toBe(true);
    const marker = text.slice(0, text.indexOf("\n"));
    expect(marker.length).toBeGreaterThan(0);
    expect(marker.length).toBeLessThan(200);
    expect(text.slice(marker.length + 1)).toBe("a".repeat(TAIL - 200000) + "b".repeat(200000));

    // Capped or not, the cursor advanced with the truth: the next pull continues from 400000.
    await vi.advanceTimersByTimeAsync(1500);
    expect(outputCalls.length).toBeGreaterThanOrEqual(3);
    expect(outputCalls[2]).toContain("after=400000");
  });
});
