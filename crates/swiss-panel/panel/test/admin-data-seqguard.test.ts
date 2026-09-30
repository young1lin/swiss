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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// util.js has no module-level DOM access, so it imports clean under Node.
const util = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "util.ts")).href
) as { dbReqGuard: () => { issue: () => number; accepts: (t: number) => boolean } };

/* SPEC §data: every db loader (dbLoadData, dbLoadRedisValue, dbLoadDetail,
   dbLoadTables, dbRunSql) fires a request with no record of which invocation it belongs to,
   so a slow answer could land after a newer one and overwrite the state the user is looking
   at (dbLoadData would even rewrite d.schema from the OLD table). The guard is the DDL
   sheet's S.seq pattern factored out: a request takes a token from issue(), and its response
   may only write state while accepts(token) holds. Pure, so the contract pins here. */
describe("dbReqGuard (response-race guard)", () => {
  it("accepts the newest token and refuses every superseded one", () => {
    const g = util.dbReqGuard();
    const first = g.issue();
    expect(g.accepts(first)).toBe(true);
    const second = g.issue();
    expect(g.accepts(first), "the first request was superseded").toBe(false);
    expect(g.accepts(second)).toBe(true);
    const third = g.issue();
    expect(g.accepts(first)).toBe(false);
    expect(g.accepts(second)).toBe(false);
    expect(g.accepts(third)).toBe(true);
  });

  it("a fresh guard accepts its first token", () => {
    const g = util.dbReqGuard();
    expect(g.accepts(g.issue())).toBe(true);
  });

  it("guards are independent — one loader's requests do not invalidate another's", () => {
    const a = util.dbReqGuard();
    const b = util.dbReqGuard();
    a.issue();
    const bToken = b.issue();
    a.issue(); // two more requests on the other loader
    expect(b.accepts(bToken), "loader b's only request is still the newest on b").toBe(true);
  });
});
