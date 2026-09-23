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

import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { lsMigrate, TOKEN_ID_KEY } from "../src/util.js";

/* fix-plan #17: the mcp_gateway_* localStorage keys moved to the swiss.* namespace. The
   move is a one-shot migration AT THE READ SITES (new key first, fall back to the old key,
   then delete it), so what needs pinning is the helper's CONTRACT plus the spelling of the
   new keys. The grid-config end-to-end case (derivation of the old key from the new) lives
   in admin-data-gridconfig.test.ts; this file owns the shared mechanism. */

const backing = new Map<string, string>();
const reads: string[] = [];
const writes: string[] = [];

beforeEach(() => {
  backing.clear(); reads.length = 0; writes.length = 0;
});
(globalThis as unknown as Record<string, unknown>).localStorage = {
  getItem(k: string) { reads.push(k); return backing.has(k) ? backing.get(k)! : null; },
  setItem(k: string, v: string) { writes.push(k); backing.set(k, v); },
  removeItem(k: string) { backing.delete(k); },
};

describe("fix-plan #17 — one-shot localStorage key migration", () => {
  it("a seeded old key moves to the new key on the first read, and the old key is deleted", () => {
    backing.set("mcp_gateway_db_sql_history", '["SELECT 1"]');
    const out = lsMigrate("swiss.dbSqlHistory", "mcp_gateway_db_sql_history");
    expect(out).toBe('["SELECT 1"]');
    expect(backing.get("swiss.dbSqlHistory")).toBe('["SELECT 1"]');
    expect(backing.has("mcp_gateway_db_sql_history")).toBe(false);
    expect(writes).toEqual(["swiss.dbSqlHistory"]); // nothing ever writes an old key back
  });

  it("a fresh install touches only the new key's slot", () => {
    const out = lsMigrate("swiss.dbFavorites", "mcp_gateway_db_favorites");
    expect(out).toBeNull();
    expect(writes).toEqual([]); // nothing moved because there was nothing to move
    expect(backing.size).toBe(0);
  });

  it("the new key wins when both eras are present", () => {
    backing.set("swiss.tokenId", "t-new");
    backing.set("mcp_gateway_token_id", "t-old");
    expect(lsMigrate("swiss.tokenId", "mcp_gateway_token_id")).toBe("t-new");
    expect(backing.has("mcp_gateway_token_id")).toBe(true); // untouched: the new key answered
  });

  it("a blocked store answers null without throwing", () => {
    (globalThis as unknown as Record<string, unknown>).localStorage = {
      getItem() { throw new Error("blocked"); },
      setItem() { throw new Error("blocked"); },
      removeItem() { throw new Error("blocked"); },
    };
    expect(lsMigrate("swiss.tokenId", "mcp_gateway_token_id")).toBeNull();
  });

  it("the new key names are the swiss.* namespace (the old prefix retires with the keys)", () => {
    expect(TOKEN_ID_KEY).toBe("swiss.tokenId");
    const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
    const dataView = readFileSync(join(here, "data-view.ts"), "utf8");
    const dataSql = readFileSync(join(here, "data-sql.ts"), "utf8");
    expect(dataView).toContain('"swiss.dbSqlHistory"');
    expect(dataSql).toContain('"swiss.dbFavorites"');
    // The only mcp_gateway_ strings left in the sources are migration arguments.
    const leftovers: string[] = [];
    for (const [name, src] of [["data-view.ts", dataView], ["data-sql.ts", dataSql]]) {
      for (const line of src.split("\n")) {
        const code = line.split("//")[0]; // trailing comments may name the old era
        if (code.includes("mcp_gateway_") && !code.includes("lsMigrate")) leftovers.push(name + ": " + line.trim());
      }
    }
    expect(leftovers, "old-key literals outside migration reads: " + leftovers.join(" | ")).toEqual([]);
  });
});
