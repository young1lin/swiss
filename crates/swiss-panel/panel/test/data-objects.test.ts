/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom
import { beforeEach, expect, it } from "vitest";
import { objectPrefs, favoriteObject, visitObject, objectPrefixes } from "../src/data-objects.js";
beforeEach(() => localStorage.clear());
it("isolates object addresses by connection and database and keeps same-name schemas distinct", () => {
  favoriteObject("one", "db", { name: "orders", schema: "public" });
  favoriteObject("one", "db", { name: "orders", schema: "archive" });
  expect(objectPrefs("one", "db").favorites).toHaveLength(2);
  expect(objectPrefs("two", "db").favorites).toEqual([]);
  expect(objectPrefs("one", "other").favorites).toEqual([]);
  favoriteObject("one", "db", { name: "orders", schema: "public" });
  expect(objectPrefs("one", "db").favorites).toEqual([{ name: "orders", schema: "archive" }]);
});
it("bounds history and favorites and groups very deep names at only one prefix", () => {
  for (let i = 0; i < 120; i++) { visitObject("one", "db", { name: String(i), schema: null }); favoriteObject("one", "db", { name: String(i), schema: null }); }
  expect(objectPrefs("one", "db").recent).toHaveLength(20);
  expect(objectPrefs("one", "db").recent[0].name).toBe("119");
  expect(objectPrefs("one", "db").favorites).toHaveLength(100);
  const groups = objectPrefixes([{ name: "shop_orders", schema: "a" }, { name: "shop_items", schema: "a" }, { name: "x_".repeat(1000), schema: "a" }]);
  expect(groups.map(g => [g.name, g.rows.length])).toEqual([["shop_", 2], ["x_", 1]]);
});

it("bounds the total number of persisted scopes without discarding favorites", () => {
  for (let i = 0; i < 64; i++) expect(favoriteObject("c" + i, null, { name: "orders", schema: null })).toBe(true);
  expect(favoriteObject("overflow", null, { name: "orders", schema: null })).toBe(false);
  expect(objectPrefs("c0", null).favorites).toHaveLength(1);
  expect(localStorage.length).toBe(64);
});
