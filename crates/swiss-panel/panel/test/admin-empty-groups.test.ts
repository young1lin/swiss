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

/* An empty list still paints its groups (docs/20: an empty group is a place - a drop target
   with a + - not an empty state). The Remote Targets page learned this on 2026-09-20; the
   other grouped pages kept swapping in a page-level empty state whenever they had no rows.
   Found live on 2026-09-27: a group made on an empty SSH Connections page ("defaul") showed
   up in the New sheet's Group select and nowhere else - no header to rename or delete it by.
   Tokens and Secrets carry the same case in their own suites; these are the pages whose
   render the other suites never reach (tunnels' and jobs' suites stub the DOM). */

import { describe, it, expect, beforeAll, beforeEach, afterAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* eslint-disable @typescript-eslint/no-explicit-any -- module surfaces asserted by name below */
let tunState: any;
let tunnels: any;
let jobState: any;
let jobs: any;
/* eslint-enable @typescript-eslint/no-explicit-any */

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;

/* The shell skeleton from the SERVED index.html, as the secrets and tokens suites build it:
 * modules in this graph wire shell controls at evaluation time. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

beforeAll(async () => {
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    // Nothing here should need the network; an unexpected call answers an empty object.
    fetch: (): Promise<Response> => Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve({}) } as Response),
  });
  afterAll(() => {
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  document.body.innerHTML = shellSkeleton();
  tunState = await import("../src/tunnel-state.js");
  tunnels = await import("../src/tunnels.js");
  jobState = await import("../src/job-state.js");
  jobs = await import("../src/jobs.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
});

function groupsOn(host: string): number {
  return $(host).children.length;
}

describe("an empty list still paints its groups", () => {
  it("SSH Connections: a group made before the first connection has its header", () => {
    tunState.setMountedTunScope("conns");
    tunState.setTunResponse({ connections: [], rules: [], connGroups: ["default", "defaul"], ruleGroups: ["default"], mcps: [] });
    tunnels.renderTunnels();
    expect(groupsOn("tunGroups")).toBe(2);
    expect($("tunGroups").textContent).toContain("defaul");
    expect($("pane").textContent).not.toContain("No SSH connections");
  });

  it("Port Forwards: the same, on the rules scope", () => {
    tunState.setMountedTunScope("rules");
    tunState.setTunResponse({ connections: [], rules: [], connGroups: ["default"], ruleGroups: ["default", "lab"], mcps: [] });
    tunnels.renderTunnels();
    expect(groupsOn("tunGroups")).toBe(2);
    expect($("tunGroups").textContent).toContain("lab");
    expect($("pane").textContent).not.toContain("No forwarding rules");
  });

  it("a bare page with no groups in the answer still paints the default group", () => {
    tunState.setMountedTunScope("conns");
    tunState.setTunResponse({ connections: [], rules: [], connGroups: [], ruleGroups: [], mcps: [] });
    tunnels.renderTunnels();
    expect(groupsOn("tunGroups")).toBe(1);
    expect($("tunGroups").textContent).toContain("default");
  });

  it("Jobs: a group made before the first job has its header", () => {
    jobState.setJobRows([]);
    jobState.setJobGroupNames(["default", "nightly"]);
    jobs.renderJobs();
    expect(groupsOn("jobGroups")).toBe(2);
    expect($("jobGroups").textContent).toContain("nightly");
    expect($("pane").textContent).not.toContain("No jobs");
  });

  it("Jobs: an answer without groups still paints the default group", () => {
    jobState.setJobRows([]);
    jobState.setJobGroupNames([]);
    jobs.renderJobs();
    expect(groupsOn("jobGroups")).toBe(1);
    expect($("jobGroups").textContent).toContain("default");
  });
});
