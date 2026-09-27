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

/* docs/48: /api/* sits behind the admin session. When a browser's sign-in is gone, every call
   answers 401; the panel then goes to its own address, which the server answers with the
   sign-in page - rather than a panel of empty lists and error toasts. */

import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/util.js";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function answering(status: number): void {
  vi.stubGlobal("fetch", () => Promise.resolve(new Response("{}", { status })));
}

describe("the panel and the session gate", () => {
  it("a 401 from /api sends the browser to the sign-in page", async () => {
    answering(401);
    const assign = vi.spyOn(location, "assign").mockImplementation(() => undefined);
    const r = await api("/api/mcps");
    expect(r.status).toBe(401);
    expect(assign).toHaveBeenCalledWith("/");
  });

  it("any other answer stays where it is", async () => {
    const assign = vi.spyOn(location, "assign").mockImplementation(() => undefined);
    for (const status of [200, 400, 403, 404, 500]) {
      answering(status);
      await api("/api/mcps");
    }
    expect(assign).not.toHaveBeenCalled();
  });
});
