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

import { describe, expect, it } from "vitest";
import {
  MongoSyntaxError, canon, cellText, columnsOf, dateMs, idKey, mongoParse, mongoType, newObjectId, numberOf,
  parseDocument, sameValue, shellText, shellTokens, valueAt, withoutId,
} from "../src/mongo-ejson.js";

/* SPEC §data.mongo-panel: the panel speaks the mongo shell's syntax to the operator and canonical
 * Extended JSON to the wire. The property everything rests on: what is printed parses back to
 * the SAME BSON value, type included - an editor that turned a Double into an Int32 or an Int64
 * past 2^53 into a rounded number would corrupt data on a save that changed nothing. */

const doc = {
  _id: { $oid: "65f0c0ffee00000000000001" },
  name: "Ada",
  qty: { $numberInt: "5" },
  ratio: { $numberDouble: "1.0" },
  big: { $numberLong: "9007199254740993" },
  price: { $numberDecimal: "19.99" },
  at: { $date: { $numberLong: "1704067200000" } },
  id2: { $binary: { base64: "Ej5FZ+ibEtOkVkJmFBdAAA==", subType: "04" } },
  bin: { $binary: { base64: "AQID", subType: "00" } },
  re: { $regularExpression: { pattern: "^a.c$", options: "i" } },
  ts: { $timestamp: { t: 1700000000, i: 3 } },
  lo: { $minKey: 1 },
  hi: { $maxKey: 1 },
  tags: ["a", "b"],
  addr: { city: "London", zip: null, geo: { lat: { $numberDouble: "51.5" } } },
  "odd key": true,
  inf: { $numberDouble: "-Infinity" },
};

describe("printing in the shell's syntax", () => {
  it("spells every wrapper the way mongosh does", () => {
    const text = shellText(doc);
    for (const piece of [
      "_id: ObjectId(\"65f0c0ffee00000000000001\")",
      "qty: 5",
      "ratio: 1.0",
      "big: NumberLong(\"9007199254740993\")",
      "price: NumberDecimal(\"19.99\")",
      "at: ISODate(\"2024-01-01T00:00:00.000Z\")",
      "id2: UUID(\"123e4567-e89b-12d3-a456-426614174000\")",
      "bin: BinData(0, \"AQID\")",
      "re: /^a.c$/i",
      "ts: Timestamp({ t: 1700000000, i: 3 })",
      "lo: MinKey()",
      "\"odd key\": true",
      "inf: -Infinity",
      "tags: [\"a\", \"b\"]",
    ]) {
      expect(text, piece).toContain(piece);
    }
  });

  it("colours tokens with the code block's classes", () => {
    const toks = shellTokens({ a: { $oid: "65f0c0ffee00000000000001" }, n: 1 }, { oneLine: true });
    expect(toks.find((t) => t[1] === "a")?.[0]).toBe("jv-k");
    expect(toks.find((t) => t[1] === "ObjectId")?.[0]).toBe("jv-l");
    expect(toks.find((t) => t[1] === "1")?.[0]).toBe("jv-n");
  });

  it("opens a document that holds a document, and keeps a short flat one inline", () => {
    const text = shellText({ a: { b: { c: 1 } }, list: [1, 2, 3] });
    expect(text).toBe("{\n  a: {\n    b: { c: 1 }\n  },\n  list: [1, 2, 3]\n}");
    expect(shellText({ x: "y".repeat(100), z: 1 })).toContain("\n  z: 1\n");
  });

  it("prints a regex holding a slash with the constructor, which can carry it", () => {
    expect(shellText({ $regularExpression: { pattern: "a/b", options: "" } })).toBe("RegExp(\"a/b\", \"\")");
    expect(shellText({ $regularExpression: { pattern: "a\\/b", options: "m" } })).toBe("/a\\/b/m");
  });

  it("gives a date outside the JS range its raw milliseconds", () => {
    expect(shellText({ $date: { $numberLong: "-62135596800001000" } })).toBe("new Date(-62135596800001000)");
  });
});

describe("reading the shell's syntax", () => {
  it("reads unquoted keys, single quotes, comments and trailing commas", () => {
    expect(mongoParse("{ name: 'Ada', // who\n age: { $gt: 30 }, /* and */ ok: true, }")).toEqual({
      name: "Ada", age: { $gt: 30 }, ok: true,
    });
  });

  it("reads strict JSON too", () => {
    expect(mongoParse("{\"a\": [1, 2.5, null, \"x\"]}")).toEqual({ a: [1, { $numberDouble: "2.5" }, null, "x"] });
  });

  it("keeps integer width honest: Int32 plain, Int64 from its digits, a point makes a Double", () => {
    expect(mongoParse("5")).toBe(5);
    expect(mongoParse("2147483648")).toEqual({ $numberLong: "2147483648" });
    // Never through a JS number: 2^53 + 1 survives digit for digit.
    expect(mongoParse("9007199254740993")).toEqual({ $numberLong: "9007199254740993" });
    expect(mongoParse("1.0")).toEqual({ $numberDouble: "1.0" });
    expect(mongoParse("1e3")).toEqual({ $numberDouble: "1000" });
    expect(mongoParse("-0.10")).toEqual({ $numberDouble: "-0.10" });
    expect(mongoParse("NumberInt(7)")).toEqual({ $numberInt: "7" });
    expect(mongoParse("0x10")).toBe(16);
  });

  it("reads the shell's constructors into canonical wrappers", () => {
    expect(mongoParse("ObjectId('65F0C0FFEE00000000000001')")).toEqual({ $oid: "65f0c0ffee00000000000001" });
    expect(mongoParse("ISODate(\"2024-01-01T00:00:00Z\")")).toEqual({ $date: { $numberLong: "1704067200000" } });
    expect(mongoParse("new Date(\"2024-01-01\")")).toEqual({ $date: { $numberLong: "1704067200000" } });
    expect(mongoParse("NumberDecimal(\"0.1\")")).toEqual({ $numberDecimal: "0.1" });
    expect(mongoParse("UUID(\"123e4567-e89b-12d3-a456-426614174000\")")).toEqual({ $binary: { base64: "Ej5FZ+ibEtOkVkJmFBdAAA==", subType: "04" } });
    expect(mongoParse("BinData(0, 'AQID')")).toEqual({ $binary: { base64: "AQID", subType: "00" } });
    expect(mongoParse("Timestamp(5, 6)")).toEqual({ $timestamp: { t: 5, i: 6 } });
    expect(mongoParse("/ab+c/mi")).toEqual({ $regularExpression: { pattern: "ab+c", options: "im" } });
    expect(mongoParse("[MinKey, MaxKey()]")).toEqual([{ $minKey: 1 }, { $maxKey: 1 }]);
  });

  it("an empty ObjectId() mints a fresh id", () => {
    const v = mongoParse("ObjectId()") as { $oid: string };
    expect(v.$oid).toMatch(/^[0-9a-f]{24}$/);
    expect(newObjectId(0).slice(0, 8)).toBe("00000000");
  });

  it("says where a mistake is", () => {
    try {
      mongoParse("{\n  a: 1\n  b: 2 }");
      throw new Error("parsed");
    } catch (e) {
      expect(e).toBeInstanceOf(MongoSyntaxError);
      const err = e as MongoSyntaxError;
      expect(err.line).toBe(3);
      expect(err.col).toBe(3);
      expect(err.message).toContain("expected \",\" or \"}\"");
    }
    expect(() => mongoParse("{ a: Bogus(1) }")).toThrow(/unknown constructor/);
    expect(() => mongoParse("{ a: nope }")).toThrow(/quote it/);
    expect(() => mongoParse("ObjectId('xyz')")).toThrow(/24 hex/);
  });

  it("parseDocument reports instead of throwing, and treats empty as not sent", () => {
    expect(parseDocument("")).toEqual({ value: {}, error: null });
    expect(parseDocument("  ", null)).toEqual({ value: null, error: null });
    expect(parseDocument("[1]").error).toContain("not a document");
    expect(parseDocument("{ a: ").error).toMatch(/line 1, column 6/);
  });
});

describe("the round trip", () => {
  it("prints and re-reads every type to the same BSON value", () => {
    const back = mongoParse(shellText(doc));
    expect(sameValue(back, doc)).toBe(true);
    // ...and the one-line form says the same.
    expect(sameValue(mongoParse(shellText(doc, { oneLine: true })), doc)).toBe(true);
  });

  it("tells a changed type from an unchanged value", () => {
    expect(sameValue({ n: { $numberInt: "1" } }, { n: 1 })).toBe(true);
    expect(sameValue({ n: { $numberDouble: "1.0" } }, { n: 1 })).toBe(false);
    expect(sameValue({ at: { $date: "2024-01-01T00:00:00Z" } }, { at: { $date: { $numberLong: "1704067200000" } } })).toBe(true);
    // Field order is part of a BSON document.
    expect(sameValue({ a: 1, b: 2 }, { b: 2, a: 1 })).toBe(false);
  });
});

describe("reading values", () => {
  it("names types the way the schema view does", () => {
    expect(mongoType(doc._id)).toBe("ObjectId");
    expect(mongoType(doc.id2)).toBe("UUID");
    expect(mongoType(doc.bin)).toBe("Binary");
    expect(mongoType(doc.addr)).toBe("Document");
    expect(mongoType(3000000000)).toBe("Int64");
    expect(mongoType(1.5)).toBe("Double");
    expect(mongoType({ $code: "x", $scope: {} })).toBe("CodeWithScope");
  });

  it("reads numbers and dates out of any form", () => {
    expect(numberOf({ $numberLong: "42" })).toBe(42);
    expect(numberOf({ $numberDouble: "NaN" })).toBeNaN();
    expect(numberOf("x")).toBeNull();
    expect(dateMs({ $date: "2024-01-01T00:00:00Z" })).toBe(1704067200000);
  });

  it("cells, ids, columns and paths", () => {
    expect(cellText(doc.addr)).toBe("{3}");
    expect(cellText(doc.tags)).toBe("[2]");
    expect(cellText(doc._id)).toBe("ObjectId(\"65f0c0ffee00000000000001\")");
    expect(idKey({ _id: 1 })).toBe(idKey({ _id: { $numberInt: "1" } }));
    expect(columnsOf([{ b: 1, _id: 1 }, { c: 1 }])).toEqual(["_id", "b", "c"]);
    expect(valueAt(doc, "addr.geo.lat")).toEqual({ $numberDouble: "51.5" });
    expect(valueAt(doc, "_id.$oid")).toBeUndefined();
    expect(Object.keys(withoutId(doc))).not.toContain("_id");
    expect(canon(2.5)).toEqual({ $numberDouble: "2.5" });
  });
});
