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

import { beforeAll, describe, expect, it, vi } from "vitest";

// @vitest-environment happy-dom

/* SPEC §host.history (plan D6) — the deep stored-body search panel. What is pinned:
 * an INCOMPLETE page with zero hits is STILL SCANNING with a Continue, never the
 * No-matches empty state; a complete zero-hit scan says No matches; skipped modules
 * are named; each hit row offers its bounded body load. Pure markup, real tree. */
vi.mock("../src/util.js", () => ({
  esc: (s: string) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"),
  icon: () => "",
  iconNode: () => document.createElement("span"),
  state: {},
}));
vi.mock("../src/sidebar.js", () => ({ rowOf: () => null }));

let logs: typeof import("../src/logs.js");

beforeAll(async () => {
  logs = await import("../src/logs.js");
});

function stub(over: Record<string, unknown> = {}) {
  return {
    name: "redis", calls: [], callsPage: 0, callsMore: false,
    callsFull: {}, callsOpen: {}, stderr: "", callsQ: "",
    ...over,
  } as never;
}

function paint(d: unknown): HTMLElement {
  const host = document.createElement("div");
  host.append(...[logs.logsBodyNode(d as never)].flat(9) as HTMLElement[]);
  document.body.appendChild(host);
  return host;
}

describe("the deep stored-body search panel (SPEC §host.history, plan D6)", () => {
  it("an incomplete zero-hit page is STILL SCANNING with a Continue, never No matches", () => {
    const host = paint(stub({
      callsDeep: true, deepQ: "SECRET", deepHits: [], deepComplete: false,
      deepCursors: { mcp: "x" }, deepScanned: 64, deepSkipped: [], deepBodies: {},
    }));
    const text = host.textContent || "";
    expect(text).toContain("Scan incomplete");
    expect(text).toContain("Continue");
    expect(text).not.toContain("No matches in the stored bodies.");
  });

  it("a complete zero-hit scan says No matches; a stopped module is named", () => {
    const host = paint(stub({
      callsDeep: true, deepQ: "SECRET", deepHits: [], deepComplete: true,
      deepCursors: null, deepScanned: 3, deepSkipped: ["remote"], deepBodies: {},
    }));
    const text = host.textContent || "";
    expect(text).toContain("No matches in the stored bodies.");
    expect(text).toContain("Skipped (stopped modules): remote");
  });

  it("each hit row names its call and byte offset and offers a body load", () => {
    const host = paint(stub({
      callsDeep: true, deepQ: "SECRET", deepComplete: true,
      deepHits: [{ runId: 7, byteOfs: 65532 }], deepCursors: null,
      deepScanned: 1, deepSkipped: [], deepBodies: {},
    }));
    const text = host.textContent || "";
    expect(text).toContain("Call #7");
    expect(text).toContain("byte 65532");
    expect(host.querySelector("[data-deephit]")).toBeTruthy();
  });
});
