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
/* docs/46 P9: the Jobs view's "unavailable" note is the library's empty state (G5
   views/jobs.ts 2 -> 0). The walk instance serves jobs, so the state cannot be reached
   live; it is pinned here instead: a gateway with no plugin inventory and a failed probe
   gets the one empty shape - glyph, title, hint - in place of the loading placeholder. */
import { describe, expect, it, vi } from "vitest";

vi.mock("../src/page-registry.js", () => ({ pluginInventory: () => null }));
vi.mock("../src/jobs.js", () => ({ probeJobs: async () => false }));
vi.mock("../src/polling.js", () => ({ jobsChipText: () => "", loadJobs: async () => undefined }));
vi.mock("../src/job-state.js", () => ({ clearJobsView: () => undefined }));

describe("the Jobs view on a gateway without the jobs subsystem (docs/46 P9)", () => {
  it("paints the library's empty state with its glyph, title and hint", async () => {
    const pane = document.createElement("div");
    pane.id = "pane";
    pane.textContent = "Loading…";
    document.body.replaceChildren(pane);
    const view = await import("../src/views/jobs.js");
    await view.mount();
    const empty = pane.querySelector(".empty");
    expect(empty, "the pane holds the library's .empty").not.toBeNull();
    expect(empty?.querySelector(".empty-ic svg.ic use")?.getAttribute("href")).toBe("#i-clock");
    expect(empty?.querySelector("h2")?.textContent).toBe("Jobs unavailable");
    expect(empty?.querySelector("p.hint")?.textContent).toBe("This gateway does not serve the jobs subsystem.");
    expect(pane.textContent).not.toContain("Loading");
  });
});
