/*
 * Copyright 2026 The swiss authors
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

import type { GroupedRow } from "../src/types/dom.js";
import { describe, expect, it } from "vitest";
import {
  addTitle,
  deleteConfirmMsg,
  groupOf,
  lastGroupKey,
  resolveDefaultGroup,
  slice,
} from "../src/group-logic.js";

/** The pure half of groups.js (docs/20 G3): every rule here is one the panel once got wrong
 *  by having two copies of it - the delete confirm hard-coding 'default', the create title
 *  landing unnamed. Pinned here so the wording and the sink rule are data, not typing. */
describe("group logic", () => {
  const rows = [
    { name: "a", group: "Docs" },
    { name: "b", group: null },
    { name: "c", group: "Search" },
    { name: "d", group: "Gone" }, // a group deleted since the list was cut
  ];

  it("renders each row under a live group, the rest under the first slot", () => {
    const fn = groupOf(["default", "Docs", "Search"]);
    expect(rows.map(fn)).toEqual(["Docs", "default", "Search", "default"]);
  });

  it("falls back to 'default' when the list is somehow empty", () => {
    expect(groupOf([])({ name: "x", group: null } as GroupedRow)).toBe("default");
  });

  it("slices in stored group order, keeping empty groups in their slot", () => {
    const cut = slice(rows, ["default", "Docs", "Empty", "Search"], groupOf(["default", "Docs", "Empty", "Search"]));
    expect(cut.map((g) => [g.name, g.rows.map((r) => r.name)])).toEqual([
      ["default", ["b", "d"]],
      ["Docs", ["a"]],
      ["Empty", []],
      ["Search", ["c"]],
    ]);
  });

  it("names the sink group in the delete confirm - the first that remains, pluralising", () => {
    expect(deleteConfirmMsg("Docs", ["default", "Docs", "Search"], 1, "MCP")).toBe(
      "Delete group 'Docs'?\n\nIts 1 MCP moves to 'default'. Nothing is removed.",
    );
    expect(deleteConfirmMsg("default", ["default", "Docs"], 3, "row")).toBe(
      "Delete group 'default'?\n\nIts 3 rows move to 'Docs'. Nothing is removed.",
    );
  });

  it("builds create titles that name where the new thing goes", () => {
    // One sentence key since docs/38 L2: the old verb+noun+group concatenation could not
    // translate. The zh dictionary carries the other direction (see i18n-shell-zh.test.ts).
    expect(addTitle("MCP", "learn")).toBe("New MCP in learn");
  });

  it("resolves a fresh row's group: the last used while it lives, else the first slot", () => {
    expect(resolveDefaultGroup(["default", "Docs"], "Docs")).toBe("Docs");
    expect(resolveDefaultGroup(["default", "Docs"], "Deleted")).toBe("default");
    expect(resolveDefaultGroup(["default", "Docs"], null)).toBe("default");
    expect(resolveDefaultGroup([], null)).toBe("default");
  });

  it("keys the last-used choice per scope", () => {
    expect(lastGroupKey("conns")).toBe("swiss.groups.conns.last");
  });
});
