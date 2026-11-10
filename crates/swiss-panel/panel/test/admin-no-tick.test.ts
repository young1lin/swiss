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

/* SPEC §panel: every page module exports no poll, and clock() reads nothing. */

import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
/* The entries the page registry actually loads: the shell's legacy and host pages,
   plus each plugin descriptor's view. */
const pages = [
  "mcps", "traffic", "tokens", "tunnels", "tunnel-forwards", "data", "jobs",
  "plugins", "secrets", "system", "process", "memory", "terminal",
  "remote", "remote-runs", "llm-providers", "llm-embedding", "llm-agents", "llm-history",
];
const fetches: string[] = [];

beforeAll(() => {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = [...new Set([...html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)].map((m) => m[1]))];
  document.body.innerHTML = ids.map((id) => '<div id="' + id + '"></div>').join("");
  const filter = document.getElementById("filter");
  if (filter) filter.outerHTML = '<input id="filter">';
  vi.stubGlobal("fetch", async (input: RequestInfo | URL) => {
    fetches.push(String(input));
    return { ok: true, status: 200, json: async () => ({}) };
  });
});

describe("no page is on a shell tick (SPEC §panel)", () => {
  it("every page module exports no poll", async () => {
    for (const id of pages) {
      const view = await import("../src/views/" + id + ".js") as { poll?: unknown };
      expect(view.poll, id).toBeUndefined();
    }
  });

  it("every clock() asks for nothing", async () => {
    const jobs = await import("../src/views/jobs.js");
    const remote = await import("../src/views/remote.js");
    const before = fetches.length;
    jobs.clock();
    remote.clock();
    expect(fetches.length).toBe(before);
  });
});
