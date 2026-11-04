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

/* proto:check fails when a generated file is stale (SPEC §bus.codegen). The script's
   comparison is what `npm run proto:check` runs after regenerating. */

import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
const { protoDiff } = await import(new URL("../proto.mjs", import.meta.url).href) as {
  protoDiff: (leftDir: string, rightDir: string) => string[];
};

describe("proto:check (SPEC §bus.codegen)", () => {
  it("fails on a stale generated file", () => {
    const root = mkdtempSync(join(tmpdir(), "swiss-proto-diff-"));
    const left = join(root, "left");
    const right = join(root, "right");
    mkdirSync(left);
    mkdirSync(right);
    writeFileSync(join(left, "bus_pb.js"), "same\n");
    writeFileSync(join(right, "bus_pb.js"), "same\n");
    expect(protoDiff(left, right)).toEqual([]);
    writeFileSync(join(right, "bus_pb.js"), "stale\n");
    expect(protoDiff(left, right)).toEqual(["bus_pb.js"]);
    rmSync(root, { recursive: true, force: true });
  });
});
