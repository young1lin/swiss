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

import { beforeEach, describe, expect, it, vi } from "vitest";

/* The Traffic page's context-bar chip counts the MCPs (mcpChipText over mcpRows). Found on the
   SPEC §panel.pages, found on a walk: a cold load straight onto #traffic read "0 MCPs · 0 up" and kept reading it,
   because nothing on that page ever loaded the MCP list - the Servers page's poll is what fills
   it, and that page was never visited. The view now loads the list it counts, on mount and on
   every poll. */

const calls: string[] = [];
vi.mock("../src/traffic.js", () => ({
  clearTrafficView: () => { calls.push("clear"); },
  loadTraffic: async () => { calls.push("loadTraffic"); },
  resetTrafficSig: () => { calls.push("resetSig"); },
  trafficReload: () => { calls.push("trafficReload"); },
}));
vi.mock("../src/polling.js", () => ({
  loadList: async () => { calls.push("loadList"); },
  mcpChipText: () => "chip",
}));

let view: typeof import("../src/views/traffic.js");

beforeEach(async () => {
  calls.length = 0;
  view = await import("../src/views/traffic.js");
});

describe("the Traffic view counts MCPs it loaded itself (SPEC §panel.pages)", () => {
  it("mount loads the MCP list before the first paint, so the chip is right on a cold load", async () => {
    await view.mount();
    expect(calls).toEqual(["loadList", "trafficReload"]);
  });

  it("every poll refreshes the MCP list with the activity, so the chip follows state changes", async () => {
    await view.poll();
    expect(calls).toContain("loadList");
    expect(calls).toContain("loadTraffic");
  });
});
